#Requires -Version 5.1
<#
.SYNOPSIS
  知乎 Cookie 采集器 v2：free-renew 走知乎发文路线时一次性登录 + 取 Cookie。

.DESCRIPTION
  知乎登录态命脉 z_c0 是 httpOnly，F12 / document.cookie 拿不到，必须经 CDP 读取。
  v1 用了已在新版 Chrome 废弃的 Network.getAllCookies 且不检查错误，导致“登录后一直等不到
  z_c0”的假死。v2 改用 Storage.getCookies(flatten)，按 id 精确匹配 CDP 响应，并把每轮
  抓到的 cookie 名单打出来，便于诊断。

  流程：启动带远程调试端口的专用 Chrome → 你登录 → 检测到 z_c0 → 导航写文页补齐
  _xsrf/d_c0/q_c1 → 导出单行 → 可选直传 gh Secret ZHIHU_COOKIES。

.EXAMPLE
  .\scripts\refresh-zhihu-cookie.ps1
  .\scripts\refresh-zhihu-cookie.ps1 -SelfTest   # 仅体检：编译 CDP 客户端 + 查依赖/路径，不启动浏览器、不动登录态
#>
[CmdletBinding()]
param([switch]$SelfTest)
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Port      = 9223
$Profile   = Join-Path $env:LOCALAPPDATA "free-renew\zhihu-profile"
$OutFile   = Join-Path $env:TEMP "zhihu_cookies_oneline.txt"
$LoginUrl  = "https://www.zhihu.com/signin"
$SettleUrl = "https://zhuanlan.zhihu.com/write"

$browser = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
    "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { Write-Host "ERR: 找不到 Chrome 或 Edge，请先安装"; exit 1 }
Write-Host "[1/5] 浏览器: $browser"

# ── CDP WebSocket 客户端：发命令后按 id 精确匹配响应，跳过事件消息 ──────
# 类型名每次运行唯一（同一 PowerShell 会话里旧版残留类名会和本脚本撞车，
# 且旧版没有按 id 匹配的逻辑，复用它等于把 bug 带回来）。用占位符替换。
$TypeName = "CdpClient_" + [Guid]::NewGuid().ToString("N")
$wsClientCode = @'
using System;
using System.Net.WebSockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

public static class __TYPE__ {
    public static async Task<string> CallAsync(ClientWebSocket ws, int id, string method, string paramsJson) {
        var req = "{\"id\":" + id + ",\"method\":\"" + method + "\"";
        if (paramsJson != null) req += ",\"params\":" + paramsJson;
        req += "}";
        var bytes = Encoding.UTF8.GetBytes(req);
        await ws.SendAsync(new ArraySegment<byte>(bytes), WebSocketMessageType.Text, true, CancellationToken.None);
        string needle1 = "\"id\":" + id + ",";
        string needle2 = "\"id\":" + id + "}";
        var buf = new byte[16 * 1024 * 1024];
        int guard = 0;
        while (true) {
            if (++guard > 500) throw new Exception("等待响应超时(事件过多)");
            var ms = new System.IO.MemoryStream();
            while (true) {
                var r = await ws.ReceiveAsync(new ArraySegment<byte>(buf), CancellationToken.None);
                if (r.MessageType == WebSocketMessageType.Close) throw new Exception("连接被关闭");
                ms.Write(buf, 0, r.Count);
                if (r.EndOfMessage) break;
            }
            var text = Encoding.UTF8.GetString(ms.ToArray());
            if (text.Contains(needle1) || text.Contains(needle2)) return text;
            // 否则是事件消息，继续读下一条
        }
    }
}
'@
$wsClientCode = $wsClientCode -replace "__TYPE__", $TypeName
Add-Type -TypeDefinition $wsClientCode -Language CSharp

function Invoke-Cdp {
    param($Ws, [ref]$CidRef, [string]$Method, [string]$ParamsJson = $null)
    $CidRef.Value++
    $cdpType = $TypeName -as [type]
    $raw = $cdpType::CallAsync($Ws, $CidRef.Value, $Method, $ParamsJson).GetAwaiter().GetResult()
    $obj = $raw | ConvertFrom-Json
    if ($obj.error) { throw "CDP ${Method} 返回错误: $($obj.error.code) $($obj.error.message)" }
    return $obj
}

# ── 自体检：只验证依赖/编译，绝不启动浏览器、绝不动登录态 ───────────
if ($SelfTest) {
    Write-Host "[selftest] C# CDP 客户端编译通过（唯一类名 $TypeName）"
    Write-Host "[selftest] 浏览器: $browser"
    $dir = Split-Path $OutFile -Parent
    $probe = Join-Path $dir (".fr_selftest_" + [guid]::NewGuid().ToString("N"))
    try { Set-Content -Path $probe -Value "x" -ErrorAction Stop; Remove-Item $probe -Force; Write-Host "[selftest] 输出目录可写: OK ($dir)" }
    catch { Write-Host "[selftest] 输出目录写入失败: $_" }
    if (Get-Command gh -ErrorAction SilentlyContinue) { Write-Host "[selftest] gh CLI: 已装（可直传 Secret）" }
    else { Write-Host "[selftest] gh CLI: 未装（需手动贴 Secret）" }
    Write-Host "[selftest] OK — 未启动浏览器、未改动任何登录态/Secret"
    exit 0
}

# ── 启动 / 连接浏览器 ──────────────────────────────────────────────────
Write-Host "[2/5] 启动独立 profile 浏览器（不影响你日常 Chrome）"
$devtoolsOk = $false
try { Invoke-RestMethod "http://127.0.0.1:$Port/json/version" -TimeoutSec 2 | Out-Null; $devtoolsOk = $true } catch {}
if (-not $devtoolsOk) {
    Start-Process $browser -ArgumentList @(
        "--remote-debugging-port=$Port",
        "--user-data-dir=`"$Profile`"",
        "--no-first-run", "--no-default-browser-check",
        "$LoginUrl"
    )
    Start-Sleep -Seconds 4
} else {
    Write-Host "  复用已运行实例"
}

function Get-PageWs {
    $targets = Invoke-RestMethod "http://127.0.0.1:$Port/json/list" -TimeoutSec 5
    $page = $targets | Where-Object { $_.type -eq "page" } | Select-Object -First 1
    if (-not $page) { return $null }
    $w = [System.Net.WebSockets.ClientWebSocket]::new()
    $w.ConnectAsync([Uri]$page.webSocketDebuggerUrl, [Threading.CancellationToken]::None).Wait()
    return $w
}

Write-Host "[3/5] 连接 CDP，轮询登录态——请在弹出的浏览器窗口里登录知乎"
$cid = 0
$ws = Get-PageWs
if (-not $ws) { Write-Host "ERR: 未找到可调试页面（Chrome 是否被关了？）"; exit 1 }

function Get-ZhihuCookies($conn, [ref]$cidRef) {
    # 首选 Storage.getCookies(官方、含 httpOnly、返回全上下文)；失败回退 Network.getAllCookies
    $r = $null
    try {
        $r = Invoke-Cdp -Ws $conn -CidRef $cidRef -Method "Storage.getCookies" -ParamsJson '{"flatten":true}'
    } catch {
        $r = Invoke-Cdp -Ws $conn -CidRef $cidRef -Method "Network.getAllCookies"
    }
    return @($r.result.cookies | Where-Object { $_.domain -match "zhihu\.com" })
}

$cookies = @()
$deadline = (Get-Date).AddMinutes(10)
$round = 0
while ($true) {
    $round++
    try {
        $cookies = Get-ZhihuCookies $ws ([ref]$cid)
    } catch {
        Write-Host "  读取 cookie 出错($_)，尝试重连页面…"
        try { $ws.Dispose() } catch {}
        Start-Sleep -Seconds 2
        $ws = Get-PageWs
        if (-not $ws) { Write-Host "ERR: 页面失联且无法重连"; exit 1 }
        continue
    }
    $zc0 = $cookies | Where-Object { $_.name -eq "z_c0" -and $_.value }
    if ($zc0) { break }
    $names = ($cookies | ForEach-Object { $_.name } | Sort-Object -Unique) -join ","
    if ($round -le 3 -or ($round % 3 -eq 0)) {
        if ($names) {
            Write-Host "  还在等 z_c0… 已见 $($cookies.Count) 条知乎 cookie: $names"
        } else {
            Write-Host "  还没读到任何知乎 cookie——请确认你登录的是【刚弹出的那个 Chrome 窗口】，且已完成登录（右上角出现头像）"
        }
    }
    if ((Get-Date) -gt $deadline) { Write-Host "ERR: 10 分钟内未检测到 z_c0，终止"; try{$ws.Dispose()}catch{}; exit 1 }
    Start-Sleep -Seconds 4
}
Write-Host "  ✅ 检测到 z_c0！等 5s 让 Cookie 稳定..."
Start-Sleep -Seconds 5

# 导航写文页，补齐 _xsrf / d_c0 / q_c1
Invoke-Cdp -Ws $ws -CidRef ([ref]$cid) -Method "Page.navigate" -ParamsJson "{`"url`":`"$SettleUrl`"}" | Out-Null
Start-Sleep -Seconds 6
$cookies = Get-ZhihuCookies $ws ([ref]$cid)
try { $ws.Dispose() } catch {}

# ── 导出 ──────────────────────────────────────────────────────────────
Write-Host "[4/5] 导出单行 Cookie"
$dedup = [ordered]@{}
foreach ($c in $cookies) { $dedup[$c.name] = $c.value }
$singleLine = ($dedup.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join "; "
[System.IO.File]::WriteAllText($OutFile, $singleLine, (New-Object System.Text.UTF8Encoding($false)))

Write-Host ""
Write-Host "OK: 导出 $($dedup.Count) 个字段 → $OutFile"
foreach ($k in @("z_c0","_xsrf","d_c0","q_c1")) {
    if ($dedup.Contains($k)) {
        $v = $dedup[$k]; if ($v.Length -gt 12) { $v = $v.Substring(0,12) + "…" }
        Write-Host "   ✅ $k = $v"
    } else {
        Write-Host "   ❌ $k 缺失（z_c0/_xsrf 缺则知乎发文会失败）"
    }
}

Write-Host "[5/5] 尝试直传 GitHub Secret ZHIHU_COOKIES"
$hasGh = Get-Command gh -ErrorAction SilentlyContinue
$repoSlug = $null
try {
    if ((git rev-parse --is-inside-work-tree 2>$null) -eq "true") {
        $u = git config --get remote.origin.url 2>$null
        if ($u -match "github\.com[:/](.+?)(\.git)?/?$") { $repoSlug = $Matches[1] }
    }
} catch {}
if ($hasGh -and $repoSlug) {
    $ans = Read-Host "   检测到 gh CLI + 仓库 $repoSlug，直接更新 Secret ZHIHU_COOKIES? (y/n)"
    if ($ans -eq "y") {
        gh secret set ZHIHU_COOKIES --body $singleLine --repo $repoSlug
        if ($LASTEXITCODE -eq 0) { Write-Host "   ✅ Secret ZHIHU_COOKIES 已更新，下次 Actions 运行即生效" }
        else { Write-Host "   gh secret set 失败，请手动复制 $OutFile 内容去 Settings→Secrets 添加" }
    }
} else {
    Write-Host "   (不在 git 仓库内或未装 gh：打开 $OutFile 复制整行 → 仓库 Settings→Secrets→ZHIHU_COOKIES)"
}
Write-Host "完成。浏览器窗口可手动关闭。"
