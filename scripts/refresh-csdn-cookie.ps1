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

# ── HTTP CDP 辅助(只用 /json 端点 + 命令走 WebSocket) ────────
# 说明: cookie 读取若走 WS 会引入不稳定的接收循环; 这里用
# 原生 .NET WebSocket 客户端(System.Net.WebSockets), 每次调用
# 收满一个完整消息即返回。
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

# ── 自体检：只验证依赖/编译，绝不启动浏览器、绝不动登录态 ───────────
if ($SelfTest) {
    Write-Host "[selftest] C# CDP 客户端编译通过"
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
$ws = [System.Net.WebSockets.ClientWebSocket]::new()
$ws.ConnectAsync([Uri]$page.webSocketDebuggerUrl, [Threading.CancellationToken]::None).Wait()
$script:cid = 0

$cookies = $null
$deadline = (Get-Date).AddMinutes(8)
while ($true) {
    $script:cid++
    $r = Invoke-CdpJson $ws $script:cid "Network.getAllCookies"
    $cookies = @($r.result.cookies | Where-Object { $_.domain -match "csdn\.net" })
    if ($cookies | Where-Object { $_.name -in @("UserToken","UserName") }) { break }
    if ((Get-Date) -gt $deadline) { Write-Host "ERR: 8 分钟未检测到登录态"; $ws.Dispose(); exit 1 }
    Start-Sleep -Seconds 3
}
Write-Host "  登录态确认,稳定等待 5s..."
Start-Sleep -Seconds 5
$script:cid++
Invoke-CdpJson $ws $script:cid "Page.navigate" '{"url":"https://www.csdn.net/"}' | Out-Null
Start-Sleep -Seconds 4
$script:cid++
$r = Invoke-CdpJson $ws $script:cid "Network.getAllCookies"
$cookies = @($r.result.cookies | Where-Object { $_.domain -match "csdn\.net" })

# ── 导出 ──────────────────────────────────────────────────────
Write-Host "[4/4] 导出..."
$singleLine = ($cookies | ForEach-Object { "$($_.name)=$($_.value)" }) -join "; "
Set-Content -Path $OutFile -Value $singleLine -Encoding UTF8
$ws.Dispose()

Write-Host ""
Write-Host "OK: $($cookies.Count) 条 Cookie → $OutFile"
$ut = $cookies | Where-Object { $_.name -eq "UserToken" }
if ($ut -and $ut.expires) {
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
