#Requires -Version 5.1
<#
.SYNOPSIS
  free-renew 交互式安装向导：Fork → Secrets → CSDN Cookie → 定时 → 首跑
  全程问答式，本地登录 Git + GitHub CLI 即可完成。

.EXAMPLE
  .\install.ps1
#>
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$UPSTREAM = "yangshu114514/free-renew"

function Step($n, $msg) { Write-Host "`n========== [$n/6] $msg ==========" -ForegroundColor Cyan }
function Warn($msg)     { Write-Host "  ⚠ $msg" -ForegroundColor Yellow }
function Ok($msg)       { Write-Host "  ✓ $msg" -ForegroundColor Green }
function Die($msg)      { Write-Host "  ✗ $msg" -ForegroundColor Red; exit 1 }

Write-Host @"
╔════════════════════════════════════════════╗
║   free-renew 安装向导                       ║
║   阿贝云/三丰云 免费服务器自动续期           ║
╚════════════════════════════════════════════╝
"@ -ForegroundColor Cyan

# ── 前置检查 ─────────────────────────────────────────────────
Step 0 "前置检查"
foreach ($cmd in @("git", "gh")) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        Die "缺少 $cmd。安装后重跑本脚本。（gh: winget install GitHub.cli）"
    }
}
gh auth status 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Die "GitHub CLI 未登录。先运行: gh auth login，然后重跑本脚本。" }
Ok "git / gh 就绪且已登录"

# ── [1/6] 仓库：复刻或使用现有 clone ────────────────────────
Step 1 "仓库准备"
$inRepo = $false; $repo = $null
try {
    if ((git rev-parse --is-inside-work-tree 2>$null) -eq "true") { $inRepo = $true }
} catch {}
if ($inRepo) {
    $u = git config --get remote.origin.url 2>$null
    if ($u -match "github\.com[:/](.+?)(\.git)?$") { $repo = $Matches[1] }
}
if ($inRepo -and $repo -and $repo -ne $UPSTREAM) {
    Ok "当前目录已是你的仓库: $repo"
} else {
    Write-Host @"
本向导会基于 $UPSTREAM 创建**你自己的私有副本**（Secrets 存在你名下）。

副本可见性说明（GitHub Actions 规则）:
  • Public  → Actions 完全免费无限量（推荐）
  • Private → 每月 2000 免费分钟（本工具每天约用 2 分钟，也绰绰有余）
  注意: Actions 仅可用于与仓库代码相关的自动化，禁止当通用算力薅（GitHub AUP）
"@ -ForegroundColor Gray
    $ans = Read-Host "是否现在为你 fork 并 clone? (Y/n)"
    if ($ans -eq "" -or $ans -match "^[yY]") {
        gh repo fork $UPSTREAM --clone 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { Die "fork 失败，请检查网络/权限" }
        Set-Location free-renew
    } else {
        $name = Read-Host "或输入你已有的 仓库名 (owner/repo，回车=新建私有 $env:USERNAME/free-renew)"
        if ($name -eq "") { $name = "$env:USERNAME/free-renew" }
        gh repo create $name --private --clone 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { Die "建仓失败" }
        Set-Location ($name.Split("/")[1])
    }
    $repo = git config --get remote.origin.url 2>$null
    if ($repo -match "github\.com[:/](.+?)(\.git)?$") { $repo = $Matches[1] }
    Ok "仓库就绪: $repo"
}
$repoArg = @("--repo", $repo)
function Set-GhSecret($name, $value) {
    $value | gh secret set $name --body - @repoArg
    if ($LASTEXITCODE -eq 0) { Ok "Secret $name 已写入" } else { Die "Secret $name 写入失败" }
}

# ── [2/6] 云账号 ─────────────────────────────────────────────
Step 2 "云厂商账号（免费服务器的控制台账密，仅存入你仓库的加密 Secrets）"
$sfUser = Read-Host "三丰云 手机号"
$sfPass = Read-Host "三丰云 密码"
Set-GhSecret "SANFENGYUN_USERNAME" $sfUser
Set-GhSecret "SANFENGYUN_PASSWORD" $sfPass
$abUser = Read-Host "阿贝云 手机号"
$abPass = Read-Host "阿贝云 密码"
Set-GhSecret "ABEIYUN_USERNAME" $abUser
Set-GhSecret "ABEIYUN_PASSWORD" $abPass
Ok "两台云账号完成"

# ── [3/6] LLM ────────────────────────────────────────────────
Step 3 "LLM 配置（写文章用，任何 OpenAI 兼容接口）"
$llmBase  = Read-Host "API 基础地址 (如 https://api.example.com/v1)"
$llmKey   = Read-Host "API Key"
$llmModel = Read-Host "模型名 (如 deepseek-chat / gpt-4o-mini / agnes-2.5-flash)"
$ans = Read-Host "发一条测试消息验证连通性? 会消耗约 20 token (默认 N)"
if ($ans -match "^[yY]") {
    try {
        $r = Invoke-RestMethod -Method Post -Uri "$($llmBase.TrimEnd('/'))/chat/completions" `
            -Headers @{ Authorization = "Bearer $llmKey" } -ContentType "application/json" `
            -Body (@{ model = $llmModel; messages = @(@{ role = "user"; content = "回复:ok" }); max_tokens = 10 } | ConvertTo-Json -Depth 5) `
            -TimeoutSec 60
        Ok "LLM 连通: " + $r.choices[0].message.content
    } catch {
        Warn "LLM 测试失败: $($_.Exception.Message)"
        $go = Read-Host "仍要继续使用此配置? (y/N)"
        if ($go -notmatch "^[yY]") { Die "请修正 LLM 配置后重跑" }
    }
} else { Write-Host "  跳过测试" }
Set-GhSecret "LLM_BASE_URL"  $llmBase
Set-GhSecret "LLM_API_KEY"   $llmKey
Set-GhSecret "LLM_MODEL"     $llmModel
Ok "LLM 配置完成"

# ── [4/6] CSDN Cookie ────────────────────────────────────────
Step 4 "CSDN Cookie（发文平台登录态）"
$cookieFile = Join-Path $env:TEMP "csdn_cookies_oneline.txt"
# 先删旧文件：防止刷新脚本失败时把上一次运行的残留 Cookie 当新 Cookie 写入
Remove-Item $cookieFile -Force -ErrorAction SilentlyContinue
$scriptPath = Join-Path (Get-Location) "scripts\refresh-csdn-cookie.ps1"
if (Test-Path $scriptPath) {
    Write-Host "即将弹出浏览器: 扫码登录 CSDN（专用 profile，登录态会保留供日后刷新）"
    & $scriptPath
} elseif (Test-Path (Join-Path (Get-Location) "scripts/refresh-csdn-cookie.ps1")) {
    & (Join-Path (Get-Location) "scripts/refresh-csdn-cookie.ps1")
} else {
    Die "找不到 scripts/refresh-csdn-cookie.ps1（请在完整仓库目录内运行本向导）"
}
if (-not (Test-Path $cookieFile)) {
    Die "Cookie 文件未产出。请重跑本向导或手动执行 scripts/refresh-csdn-cookie.ps1"
}
$cookieAge = ((Get-Date) - (Get-Item $cookieFile).LastWriteTime).TotalMinutes
if ($cookieAge -gt 2) {
    Die "Cookie 文件是 $([math]::Round($cookieAge)) 分钟前的残留，疑似本次刷新失败。请重跑。"
}
Set-GhSecret "CSDN_COOKIES" (Get-Content $cookieFile -Raw).Trim()
Ok "CSDN Cookie 完成（寿命数月，过期时微信会提醒你重跑刷新脚本）"

# ── [5/6] 通知 ───────────────────────────────────────────────
Step 5 "通知配置（出事时微信/其他渠道喊你；强烈建议配）"
Write-Host @"
通知后端选择:
  1 = OpenClaw 网关 → 微信（你已有一台跑 OpenClaw 的服务器时选这个）
  2 = 通用 Webhook（Server酱/企业微信机器人/Bark…）
  0 = 暂不配置（失败将无提醒，仅 Actions 页可见——不推荐）
"@ -ForegroundColor Gray
$backend = Read-Host "选择 (默认 1)"
if ($backend -eq "") { $backend = "1" }
if ($backend -eq "1") {
    Write-Host "需要: 一台跑 OpenClaw 的服务器 + 公网可达的 /v1/chat/completions 端点 + basic auth bot 账号。"
    Write-Host "接线步骤已写在: docs/SETUP.md 的「OpenClaw 网关通知」一节（也可按 Ctrl+点击打开 GitHub 上此文件）"
    Start-Process "https://github.com/$UPSTREAM/blob/main/docs/SETUP.md" 2>$null
    $ocUrl  = Read-Host "chatCompletions 完整 URL (如 https://你的域名/v1/chat/completions)"
    $ocUser = Read-Host "basic auth 用户名"
    $ocPass = Read-Host "basic auth 密码"
    Set-GhSecret "NOTIFY_OPENCLAW_URL"      $ocUrl
    Set-GhSecret "NOTIFY_OPENCLAW_USER"     $ocUser
    Set-GhSecret "NOTIFY_OPENCLAW_PASSWORD" $ocPass
    $ans = Read-Host "发一条测试通知验证链路? agent 会真发微信给你 (默认 N)"
    if ($ans -match "^[yY]") {
        try {
            $auth = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("${ocUser}:${ocPass}"))
            $r = Invoke-RestMethod -Method Post -Uri $ocUrl -Headers @{ Authorization = "Basic $auth" } `
                -ContentType "application/json" -TimeoutSec 90 `
                -Body (@{ model = "openclaw"; messages = @(@{ role = "user"; content = "自动化安装测试:请用微信消息工具发送【free-renew 安装成功】然后只回复:已发送" }) } | ConvertTo-Json -Depth 5)
            Ok "请求已投出（agent 异步执行，微信以实际收到为准；CF 超时也不影响送达）"
        } catch { Warn "请求异常 $($_.Exception.Message)——若为超时，agent 可能仍在执行，稍后查微信" }
    }
} elseif ($backend -eq "2") {
    $wh = Read-Host "Webhook URL"
    if ($wh) { Set-GhSecret "NOTIFY_WEBHOOK_URL" $wh }
} else {
    Warn "已跳过通知。出事时仅 Actions 页可见红叉——建议日后补配 NOTIFY_* Secrets"
}

# ── [6/6] 定时 + 部署确认 ────────────────────────────────────
Step 6 "定时计划与部署"
$t = Read-Host "每天几点(北京时间,0-23)自动检查并续期? (默认 9)"
if ($t -eq "") { $t = "9" }
if ($t -notmatch "^\d{1,2}$" -or [int]$t -gt 23) { Die "小时格式不对" }
$utcH = ([int]$t - 8 + 24) % 24
$cron = "30 $utcH * * *"
Write-Host "  cron = `"$cron`" (UTC) = 北京时间 $t:30"

Write-Host ""
Write-Host "──────── 部署确认 ────────" -ForegroundColor Cyan
Write-Host "仓库:     $repo"
Write-Host "三丰云:   $($sfUser.Substring(0,3))****$($sfUser.Substring($sfUser.Length-2))"
Write-Host "阿贝云:   $($abUser.Substring(0,3))****$($abUser.Substring($abUser.Length-2))"
Write-Host "LLM:      $llmModel @ $($llmBase)"
Write-Host "CSDN:     Cookie 已入 Secrets"
Write-Host "通知:     $(if ($backend -eq '1') { 'OpenClaw→微信' } elseif ($backend -eq '2') { 'Webhook' } else { '未配置' })"
Write-Host "定时:     每天 $t:30 北京时间"
Write-Host ""
$ans = Read-Host "确认部署? (Y/n)"
if ($ans -match "^[nN]") { Die "已取消。Secrets 已写入的条目可到仓库 Settings→Secrets 手动清理。" }

# 改 cron（只有和默认不同才需要提交）
$wfPath = Join-Path (Get-Location) ".github\workflows\renew.yml"
if (Test-Path $wfPath) {
    $yaml = Get-Content $wfPath -Raw
    if ($yaml -match '- cron: "([^"]+)"' -and $Matches[1] -ne $cron) {
        ($yaml -replace '- cron: "[^"]*"', "- cron: `"$cron`"") | Set-Content $wfPath -Encoding UTF8 -NoNewline
        git add .github/workflows/renew.yml
        git -c user.name="setup" -c user.email="setup@users.noreply.github.com" commit -m "chore: schedule = $t:30 CST" | Out-Null
        git push 2>&1 | Out-Null
        Ok "定时已改为每天 $t:30 北京时间（GitHub cron 实际触发可能延迟数分钟，属正常）"
    }
}

# 首跑
Write-Host ""
# 官方规则：fork 出来的仓库 scheduled workflows 默认禁用，必须先 Enable 一次。
# 判定：remote url 指向用户名下且 ≠ 上游（gh repo fork 后 remote 无 "fork" 字样，
# 不能靠字符串匹配，直接用 gh api 查 is_fork 最可靠）。
if ($repo -and $repo -ne $UPSTREAM) {
    $isFork = $false
    try {
        $info = gh api "repos/$repo" --jq '.fork' 2>$null
        $isFork = ($info -eq "true")
    } catch {}
    if ($isFork) {
        Write-Host "  检测到 fork 仓库：GitHub 默认禁用 fork 的定时任务，需要启用一次..." -ForegroundColor Yellow
        gh api -X PUT "repos/$repo/actions/workflows/free-server-renewal.yml/enable" 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { Ok "定时任务已启用" } else {
            Warn "自动启用失败——请到仓库 Actions 页选中 free-server-renewal 点 Enable workflow"
        }
    }
}
gh workflow run free-server-renewal @repoArg 2>&1 | Out-Null
if ($LASTEXITCODE -eq 0) {
    Ok "首次运行已触发: https://github.com/$repo/actions"
} else {
    Warn "触发失败，请到 Actions 页面手动 Run workflow"
}
Start-Process "https://github.com/$repo/actions" 2>$null

Write-Host ""
Write-Host "🎉 部署完成！" -ForegroundColor Green
Write-Host @"
后续你唯一可能要做的事:
  • CSDN Cookie 过期(数月后) → 微信收到提醒 → 重跑 scripts/refresh-csdn-cookie.ps1
  • 密码轮换 → 仓库 Settings→Secrets 直接改
  • 一切正常时它只是每天定时默默看一眼，没到期 4 秒退出
到期日临近时你会收到微信通知。
"@ -ForegroundColor Green
