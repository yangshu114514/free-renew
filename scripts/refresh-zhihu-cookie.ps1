#Requires -Version 5.1
<#
.SYNOPSIS
  知乎 Cookie 采集器：free-renew 走知乎发文路线时的一次性登录 + 取 Cookie。

.DESCRIPTION
  原理同 refresh-csdn-cookie.ps1，但关键差异：知乎登录态 cookie `z_c0` 是 httpOnly，
  浏览器里 F12 / document.cookie 都拿不到它。本脚本启动一个带“远程调试端口”的
  专用 Chrome，通过 CDP 协议 Network.getAllCookies 读取——这是唯一能拿到 httpOnly
  z_c0 又不用手点网络面板的办法。

  流程：
  - 首次运行：弹浏览器 → 你扫码/手机号登录知乎 → 脚本自动检测到 z_c0 → 导出
  - 之后运行：专用 profile 里登录态通常还活着 → 直接重新导出
  - 额外导航到 zhuanlan 写文页，确保发文所需的 _xsrf / d_c0 / q_c1 全部落齐
  - 产物落盘：  %TEMP%\zhihu_cookies_oneline.txt
  - 若当前在 free-renew git 仓库内且装了 gh CLI：询问后直传 Secret ZHIHU_COOKIES

  取到的 Cookie 只含知乎会话，不会外泄；你也可以贴完后自行去知乎“退出登录/改密码”
  让这串失效。

.EXAMPLE
  .\scripts\refresh-zhihu-cookie.ps1
#>
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# ── 配置 ──────────────────────────────────────────────────────────────
$Port    = 9223                       # 端口与 csdn(9222) 错开，可同时/交替使用
$Profile = Join-Path $env:LOCALAPPDATA "free-renew\zhihu-profile"
$OutFile = Join-Path $env:TEMP "zhihu_cookies_oneline.txt"
$LoginUrl = "https://www.zhihu.com/signin"
$SettleUrl = "https://zhuanlan.zhihu.com/write"   # 导航此处以补齐 _xsrf/d_c0/q_c1

# ── 找浏览器 ───────────────────────────────────────────────────────────
$browser = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
    "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { Write-Host "ERR: 找不到 Chrome 或 Edge，请先安装"; exit 1 }
Write-Host "[1/5] 浏览器: $browser"

# ── CDP WebSocket 客户端（内联 C#，同 csdn 版） ────────────────────────
$wsClientCode = @'
using System;
using System.Net.WebSockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

public static class CdpClient {
    public static async Task<string> CallAsync(ClientWebSocket ws, int id, string method, string paramsJson) {
        var msg = "{\"id\":" + id + ",\"method\":\"" + method + "\"";
        if (paramsJson != null) msg += ",\"params\":" + paramsJson;
        msg += "}";
        var bytes = Encoding.UTF8.GetBytes(msg);
        await ws.SendAsync(new ArraySegment<byte>(bytes), WebSocketMessageType.Text, true, CancellationToken.None);
        var buf = new byte[8 * 1024 * 1024];
        var ms = new System.IO.MemoryStream();
        while (true) {
            var r = await ws.ReceiveAsync(new ArraySegment<byte>(buf), CancellationToken.None);
            if (r.MessageType == WebSocketMessageType.Close) throw new Exception("closed");
            ms.Write(buf, 0, r.Count);
            if (r.EndOfMessage) break;
        }
        return Encoding.UTF8.GetString(ms.ToArray());
    }
}
'@
Add-Type -TypeDefinition $wsClientCode -Language CSharp

function Invoke-CdpJson {
    param($Ws, [int]$Id, [string]$Method, [string]$ParamsJson = $null)
    $raw = [CdpClient]::CallAsync($Ws, $Id, $Method, $ParamsJson).GetAwaiter().GetResult()
    return ($raw | ConvertFrom-Json)
}

# ── 启动 / 连接浏览器 ──────────────────────────────────────────────────
Write-Host "[2/5] 启动独立 profile 浏览器（不影响你日常的 Chrome）"
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

# 找一个 page 目标连上 CDP
function Get-PageTarget {
    $targets = Invoke-RestMethod "http://127.0.0.1:$Port/json/list" -TimeoutSec 5
    $targets | Where-Object { $_.type -eq "page" } | Select-Object -First 1
}

Write-Host "[3/5] 连接 CDP，轮询登录态——请在弹出的浏览器里扫码/登录知乎（检测到 z_c0 自动继续）"
$page = Get-PageTarget
if (-not $page) { Write-Host "ERR: 未找到可调试页面"; exit 1 }
$ws = [System.Net.WebSockets.ClientWebSocket]::new()
$ws.ConnectAsync([Uri]$page.webSocketDebuggerUrl, [Threading.CancellationToken]::None).Wait()
$script:cid = 0

function Get-ZhihuCookies {
    $script:cid++
    $r = Invoke-CdpJson $ws $script:cid "Network.getAllCookies"
    @($r.result.cookies | Where-Object { $_.domain -match "zhihu\.com" })
}

$cookies = $null
$deadline = (Get-Date).AddMinutes(10)
while ($true) {
    $cookies = Get-ZhihuCookies
    # z_c0 是知乎登录态命脉；出现即视为已登录
    if ($cookies | Where-Object { $_.name -eq "z_c0" -and $_.value }) { break }
    if ((Get-Date) -gt $deadline) { Write-Host "ERR: 10 分钟内未检测到 z_c0，终止"; $ws.Dispose(); exit 1 }
    Start-Sleep -Seconds 3
}
Write-Host "  登录态确认，等 5s 让 Cookie 稳定..."
Start-Sleep -Seconds 5

# 导航到写文页，补齐 _xsrf / d_c0 / q_c1（发文接口要用）
$script:cid++
Invoke-CdpJson $ws $script:cid "Page.navigate" "{`"url`":`"$SettleUrl`"}" | Out-Null
Start-Sleep -Seconds 6
$cookies = Get-ZhihuCookies
$ws.Dispose()

# ── 导出 ──────────────────────────────────────────────────────────────
Write-Host "[4/5] 导出单行 Cookie"
# 同名 cookie 可能跨子域重复，按名去重（保留值较长/后出现者），避免 header 里重复项
$dedup = @{}
foreach ($c in $cookies) { $dedup[$c.name] = $c.value }
$singleLine = ($dedup.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join "; "
Set-Content -Path $OutFile -Value $singleLine -Encoding UTF8

Write-Host ""
Write-Host "OK: 导出 $($dedup.Count) 个字段 → $OutFile"
foreach ($k in @("z_c0","_xsrf","d_c0","q_c1")) {
    if ($dedup.ContainsKey($k)) {
        $v = $dedup[$k]; if ($v.Length -gt 12) { $v = $v.Substring(0,12) + "…" }
        Write-Host "   ✅ $k = $v"
    } else {
        Write-Host "   ❌ $k 缺失（_xsrf/z_c0 缺则知乎发文会失败）"
    }
}

# z_c0 有效期提示（知乎 z_c0 通常数周到数月）
$zc0 = $cookies | Where-Object { $_.name -eq "z_c0" } | Select-Object -First 1
if ($zc0 -and $zc0.expires -and $zc0.expires -gt 0) {
    try {
        $exp = [DateTimeOffset]::FromUnixTimeSeconds([long]$zc0.expires).LocalDateTime
        Write-Host "   z_c0 到期: $exp（过期后收到 401 类通知时重跑本脚本）"
    } catch {}
}

# ── 可选直传 Secret ────────────────────────────────────────────────────
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
        $singleLine | gh secret set ZHIHU_COOKIES --body - --repo $repoSlug
        if ($LASTEXITCODE -eq 0) { Write-Host "   ✅ Secret ZHIHU_COOKIES 已更新，下次 Actions 运行即生效" }
        else { Write-Host "   gh secret set 失败，请手动复制 $OutFile 内容去 Settings→Secrets 添加" }
    }
} else {
    Write-Host "   (不在 git 仓库内或未装 gh：手动打开 $OutFile 复制整行 → 仓库 Settings→Secrets→ZHIHU_COOKIES)"
}
Write-Host "完成。浏览器窗口可手动关闭。"
