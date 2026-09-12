#Requires -Version 5.1
<#
.SYNOPSIS
  CSDN Cookie 刷新（free-renew 日常维护；登录态存活时约 30 秒，需重新扫码则更久）
.DESCRIPTION
  原理：用本机 Chrome/Edge 开一个"专用 profile + 远程调试端口"的窗口。
  - 首次运行：弹出浏览器 → 扫码登录 → 脚本自动检测到登录态并导出
  - 之后运行：专用 profile 里登录态通常还活着 → 直接重新导出，零操作
  - Cookie 落盘到 %TEMP%\csdn_cookies_oneline.txt
  - 若当前目录是本仓库的 git clone 且装了 gh CLI：询问后直传 Secret CSDN_COOKIES

  CDP 实现与 refresh-zhihu-cookie.ps1 保持同一套（Storage.getCookies + 按 id 匹配响应
  + 每次运行唯一类型名 + 单次调用 15s 超时）。早期版本用的 Network.getAllCookies 已被
  Chrome 129 移除（实测调用后连响应都不返回），且不检查响应里的 error，表现为"登录成功了
  却一直等不到登录态"的假死——已同步修复。

.EXAMPLE
  .\refresh-csdn-cookie.ps1
  .\refresh-csdn-cookie.ps1 -SelfTest   # 仅体检：编译 CDP 客户端 + 查依赖/路径，不启动浏览器、不动登录态
#>
[CmdletBinding()]
param([switch]$SelfTest, [switch]$NoSecretPush)
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# ── 配置 ──────────────────────────────────────────────────────
$Port      = 9222
$Profile   = Join-Path $env:LOCALAPPDATA "free-renew\csdn-profile"
$OutFile   = Join-Path $env:TEMP "csdn_cookies_oneline.txt"

# ── 找浏览器 ──────────────────────────────────────────────────
$browser = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
    "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { Write-Host "ERR: 找不到 Chrome/Edge"; exit 1 }
Write-Host "[1/4] 浏览器: $browser"

# ── CDP WebSocket 客户端 ──────────────────────────────────────
# cookie 读取走原生 .NET WebSocket（System.Net.WebSockets），不用第三方依赖。
# 这里有两个"踩过才知道"的坑，两个都在这份实现里堵住了：
#   1. 必须**按 id 精确匹配**响应，跳过事件消息。早先的实现"收满一条消息就返回"，
#      一旦有 CDP 事件先到，拿到的就不是响应 → 解析不出 cookies → 一直空转到超时。
#   2. 类型名必须**每次运行唯一**：同一 PowerShell 会话里重跑脚本时，上次编译的
#      同名类型仍在，Add-Type 会报"类型已存在"，而 $ErrorActionPreference=Stop
#      会当场终止整个脚本。
$TypeName = "CdpClient_" + [Guid]::NewGuid().ToString("N")
$wsClientCode = @'
using System;
using System.Net.WebSockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

public static class __TYPE__ {
    public static async Task<string> CallAsync(ClientWebSocket ws, int id, string method, string paramsJson, int timeoutMs) {
        var msg = "{\"id\":" + id + ",\"method\":\"" + method + "\"";
        if (paramsJson != null) msg += ",\"params\":" + paramsJson;
        msg += "}";
        var bytes = Encoding.UTF8.GetBytes(msg);
        await ws.SendAsync(new ArraySegment<byte>(bytes), WebSocketMessageType.Text, true, CancellationToken.None);
        string needle1 = "\"id\":" + id + ",";
        string needle2 = "\"id\":" + id + "}";
        var buf = new byte[16 * 1024 * 1024];
        int guard = 0;
        while (true) {
            if (++guard > 500) throw new Exception("等待响应超时(事件过多)");
            var ms = new System.IO.MemoryStream();
            while (true) {
                // 读必须有超时：Chrome 对**已移除**的 CDP 命令不回任何响应
                // （实测 Network.getAllCookies 就是这样），裸 ReceiveAsync 会永远
                // 阻塞，脚本表现成"卡死"，连错误信息都打不出来。
                var recv = ws.ReceiveAsync(new ArraySegment<byte>(buf), CancellationToken.None);
                if (await Task.WhenAny(recv, Task.Delay(timeoutMs)) != recv) {
                    try { ws.Abort(); } catch { }
                    throw new Exception("等待 CDP 响应超时(" + timeoutMs + "ms)，方法: " + method);
                }
                var r = recv.Result;
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

# CDP 单次调用超时：正常命令是毫秒级，15s 足以区分"慢"与"这个命令根本没人应答"
$CdpTimeoutMs = 15000

function Invoke-Cdp {
    param($Ws, [ref]$CidRef, [string]$Method, [string]$ParamsJson = $null)
    $CidRef.Value++
    $cdpType = $TypeName -as [type]
    try {
        $raw = $cdpType::CallAsync($Ws, $CidRef.Value, $Method, $ParamsJson, $CdpTimeoutMs).GetAwaiter().GetResult()
    } catch {
        # .GetResult() 抛出来的是 MethodInvocationException→AggregateException 套娃，
        # 原样上屏只有一句"一个或多个错误"。剥到最内层，让"浏览器窗口被关了"这类
        # 根因直接可读（实测：人工关掉弹窗后满屏堆栈，没人看得出发生了什么）。
        $ex = $_.Exception
        while ($ex.InnerException) { $ex = $ex.InnerException }
        throw "CDP ${Method} 失败（浏览器被关闭/失联？）: $($ex.Message)"
    }
    $obj = $raw | ConvertFrom-Json
    # CDP 的错误是**响应体里的 error 字段**，不是 HTTP 异常；不查它就会把
    # "method not found" 之类当成"没有 cookie"，然后一直等到超时。
    if ($obj.error) { throw "CDP ${Method} 返回错误: $($obj.error.code) $($obj.error.message)" }
    return $obj
}

# 读 CSDN 域下的 cookie。
# 用 Storage.getCookies：当前稳定接口，含 httpOnly，返回全部上下文。
# **不做 Network.getAllCookies 回退**——该方法已被 Chrome 129 移除，实测调用后
# 浏览器连响应都不返回（白等一次超时），回退救不了任何浏览器，只会把
# Storage.getCookies 的真实错误信息盖掉。
function Get-CsdnCookies($conn, [ref]$cidRef) {
    $r = Invoke-Cdp -Ws $conn -CidRef $cidRef -Method "Storage.getCookies" -ParamsJson '{"flatten":true}'
    return @($r.result.cookies | Where-Object { $_.domain -match "csdn\.net" })
}

# ── 自体检：只验证依赖/编译，绝不启动浏览器、绝不动登录态 ───────────
if ($SelfTest) {
    Write-Host "[selftest] C# CDP 客户端编译通过（唯一类名 $TypeName）"
    Write-Host "[selftest] 浏览器: $browser"
    $dir = Split-Path $OutFile -Parent
    Write-Host ("[selftest] 输出目录可写: {0} ({1})" -f (Test-Path $dir), $dir)
    $probe = Join-Path $dir (".fr_selftest_" + [guid]::NewGuid().ToString("N"))
    try { Set-Content -Path $probe -Value "x" -ErrorAction Stop; Remove-Item $probe -Force; Write-Host "[selftest] 实际写入测试: OK" }
    catch { Write-Host "[selftest] 实际写入测试: 失败 ($_)" }
    if (Get-Command gh -ErrorAction SilentlyContinue) { Write-Host "[selftest] gh CLI: 已装（可直传 Secret）" }
    else { Write-Host "[selftest] gh CLI: 未装（需手动贴 Secret）" }
    Write-Host "[selftest] OK — 未启动浏览器、未改动任何登录态/Secret"
    exit 0
}

# ── 启动/复用浏览器 ───────────────────────────────────────────
Write-Host "[2/4] 启动浏览器(专用 profile)"
$devtoolsOk = $false
try { Invoke-RestMethod "http://127.0.0.1:$Port/json/version" -TimeoutSec 2 | Out-Null; $devtoolsOk = $true } catch {}
if (-not $devtoolsOk) {
    Start-Process $browser -ArgumentList @(
        "--remote-debugging-port=$Port",
        "--user-data-dir=`"$Profile`"",
        "--no-first-run", "--no-default-browser-check",
        "https://www.csdn.net/"
    )
    Start-Sleep -Seconds 4
} else {
    Write-Host "  复用已有调试实例"
}

Write-Host "[3/4] 连接 CDP,检测登录态(若弹出登录页请扫码,脚本自动继续)..."
$targets = Invoke-RestMethod "http://127.0.0.1:$Port/json/list" -TimeoutSec 5
$page = $targets | Where-Object { $_.type -eq "page" } | Select-Object -First 1
if (-not $page) { Write-Host "ERR: 未找到可调试页面（Chrome 是否被关了？）"; exit 1 }
$ws = [System.Net.WebSockets.ClientWebSocket]::new()
$ws.ConnectAsync([Uri]$page.webSocketDebuggerUrl, [Threading.CancellationToken]::None).Wait()
$cid = 0

$cookies = @()
$deadline = (Get-Date).AddMinutes(8)
$round = 0
while ($true) {
    $round++
    # 读不到就抛错终止：CDP 通道已坏时继续空等 8 分钟没有意义
    $cookies = Get-CsdnCookies $ws ([ref]$cid)
    if ($cookies | Where-Object { $_.name -in @("UserToken","UserName") }) { break }
    # 每轮都静默会让人以为脚本卡死；这里周期性把"已看到什么"打出来
    if ($round -le 3 -or ($round % 5 -eq 0)) {
        $names = ($cookies | ForEach-Object { $_.name } | Sort-Object -Unique) -join ","
        if ($names) { Write-Host "  还在等登录态… 已见 $($cookies.Count) 条 CSDN cookie: $names" }
        else { Write-Host "  还没读到任何 CSDN cookie——请确认你在【刚弹出的那个浏览器窗口】里登录" }
    }
    if ((Get-Date) -gt $deadline) { Write-Host "ERR: 8 分钟未检测到登录态"; $ws.Dispose(); exit 1 }
    Start-Sleep -Seconds 3
}
Write-Host "  登录态确认,稳定等待 5s..."
Start-Sleep -Seconds 5
Invoke-Cdp -Ws $ws -CidRef ([ref]$cid) -Method "Page.navigate" '{"url":"https://www.csdn.net/"}' | Out-Null
Start-Sleep -Seconds 4
$cookies = Get-CsdnCookies $ws ([ref]$cid)

# ── 导出 ──────────────────────────────────────────────────────
Write-Host "[4/4] 导出..."
$singleLine = ($cookies | ForEach-Object { "$($_.name)=$($_.value)" }) -join "; "
# 必须写**无 BOM** UTF-8：Windows PowerShell 5.1 的 `Set-Content -Encoding UTF8`
# 会在文件头塞 BOM，而这行内容是直接塞进 Secret CSDN_COOKIES 的——BOM 会跟着
# 进 Cookie 请求头，服务端按脏字符处理，表现为"Cookie 明明是对的却登录不上"。
[System.IO.File]::WriteAllText($OutFile, $singleLine, (New-Object System.Text.UTF8Encoding($false)))
$ws.Dispose()

Write-Host ""
Write-Host "OK: $($cookies.Count) 条 Cookie → $OutFile"
$ut = @($cookies | Where-Object { $_.name -eq "UserToken" -and $_.expires -gt 0 }) | Select-Object -First 1
# expires>0 才打印：session 级 cookie 的 expires 是 -1，FromUnixTimeSeconds(-1)
# 会显示"有效期至 1970-01-01"，看着像马上过期实则没有过期概念（注入测试实测暴露）
if ($ut) {
    try {
        $exp = [DateTimeOffset]::FromUnixTimeSeconds([long]$ut.expires).LocalDateTime
        Write-Host "   UserToken 有效期至: $exp"
    } catch {}
}

# ── 可选直传 Secret ───────────────────────────────────────────
# gh secret set --repo 支持 owner/repo 简名；实测也接受完整 https URL（已验证）。
# 但 git@ 形态 URL 不一定，这里统一把任意 remote url 规范化为 owner/repo。
$hasGh = Get-Command gh -ErrorAction SilentlyContinue
$inRepo = $false; $repoSlug = $null
try {
    if ((git rev-parse --is-inside-work-tree 2>$null) -eq "true") {
        $u = git config --get remote.origin.url 2>$null
        if ($u -match "github\.com[:/](.+?)(\.git)?/?$") { $repoSlug = $Matches[1]; $inRepo = $true }
    }
} catch {}
if ($NoSecretPush) {
    Write-Host "(调用方要求不直传 Secret：Cookie 已存 $OutFile，交由上层统一写入)"
} elseif ($hasGh -and $inRepo) {
    $ans = Read-Host "检测到 gh CLI + git 仓库，直接更新 Secret CSDN_COOKIES? (y/n)"
    if ($ans -eq "y") {
        $singleLine | gh secret set CSDN_COOKIES --repo $repoSlug   # 走 stdin，免疫 PS5.1 参数引用坑
        if ($LASTEXITCODE -eq 0) { Write-Host "OK: Secret CSDN_COOKIES 已更新，下轮运行即生效" }
    }
} else {
    Write-Host "(不在 git 仓库或无 gh CLI: 手动把 $OutFile 内容填入 Secret)"
}
Write-Host "完成。浏览器窗口可手动关闭。"
