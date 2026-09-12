#Requires -Version 5.1
<#
.SYNOPSIS
  free-renew 交互式安装向导：仓库 → 云账号 → LLM → 发文平台(CSDN/知乎)Cookie → 通知 → 定时与首跑。

.DESCRIPTION
  全程问答式，本地登录 Git + GitHub CLI 即可完成。所有配置以加密 Secrets / 仓库 Variables
  存入你自己的私有副本，代码不接触凭据。

.PARAMETER DryRun
  演练模式：只走一遍问答与决策、打印将要执行的动作，绝不 fork、不写 Secret、不改 cron、
  不触发 workflow。用于安全验证脚本逻辑（也便于自动化测试）。

.PARAMETER Answers
  配合 -DryRun 使用：按问答顺序预置答案数组，非交互地跑通整条向导（测试用途）。

.EXAMPLE
  .\install.ps1
  .\install.ps1 -DryRun -Answers @("","","","","2","myurl","u","p","9","y")
#>
[CmdletBinding()]
param(
    [switch]$DryRun,
    [string[]]$Answers = @()
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$UPSTREAM = "yangshu114514/free-renew"
$script:AnsIdx = 0

function Step($n, $msg) { Write-Host "`n========== [$n/6] $msg ==========" -ForegroundColor Cyan }
function Warn($msg)     { Write-Host "  [!] $msg" -ForegroundColor Yellow }
function Ok($msg)       { Write-Host "  [ok] $msg" -ForegroundColor Green }
function Die($msg)      { Write-Host "  [x] $msg" -ForegroundColor Red; exit 1 }

# Ask：真交互读一行；DryRun 时按 Answers 顺序给答案（用完即空串=走默认），不阻塞。
function Ask($prompt, $default = "") {
    if ($DryRun) {
        if ($script:AnsIdx -lt $Answers.Count) { $a = $Answers[$script:AnsIdx] } else { $a = "" }
        $script:AnsIdx++
        Write-Host "  [dry-answer] $prompt  =>  '$a'" -ForegroundColor DarkGray
        return $a
    }
    return Read-Host $prompt
}

# Guard：执行副作用；DryRun 时只打印不执行。
function Guard($desc, [scriptblock]$cmd) {
    if ($DryRun) { Write-Host "  [dry-run] 跳过：$desc" -ForegroundColor DarkCyan; return }
    Write-Host "  执行：$desc" -ForegroundColor DarkGray
    & $cmd
}

if ($DryRun) { Write-Host "`n*** DRY-RUN 演练模式：不会写入任何改动 ***`n" -ForegroundColor Magenta }

Write-Host @"
========================================
  free-renew 安装向导
  阿贝云/三丰云 免费服务器自动续期
========================================
"@ -ForegroundColor Cyan

# ── 前置检查 ─────────────────────────────────────────────────
Step 0 "前置检查"
foreach ($cmd in @("git", "gh")) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        Die "缺少 $cmd。安装后重跑本脚本。（gh: winget install GitHub.cli）"
    }
}
if ($DryRun) {
    Write-Host "  [dry-run] 跳过 gh auth 校验" -ForegroundColor DarkCyan
} else {
    gh auth status 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Die "GitHub CLI 未登录。先运行: gh auth login，然后重跑本脚本。" }
}
Ok "git / gh 就绪"

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

# 已经是用户自己的副本（非上游）→ 直接用它配置。
if ($inRepo -and $repo -and $repo -ne $UPSTREAM) {
    Ok "当前目录已是你的仓库: $repo"
} elseif ($DryRun) {
    # 演练且当前是上游/非个人副本：不 fork，给个假仓库名走后面的流程。
    $repo = "dryrun/free-renew"
    Warn "DRY-RUN：不 fork，使用占位仓库 $repo"
} else {
    Write-Host @"
本向导会基于 $UPSTREAM 创建**你自己的私有副本**（Secrets 存在你名下）。

副本可见性（GitHub Actions 规则）:
  - Private（推荐）→ 每月 2000 免费分钟（本工具每天约用几分钟，绰绰有余）
  - Public → Actions 分钟数充足，但 Actions 日志可能含厂商返回的账号信息（见安全清单）
  注意: Actions 只能用于与仓库代码相关的自动化，不得用作通用算力（GitHub AUP）
"@ -ForegroundColor Gray
    $ans = Ask "是否现在为你 fork 并 clone? (Y/n)" "Y"
    if ($ans -eq "" -or $ans -match "^[yY]") {
        Guard "gh repo fork $UPSTREAM --clone" {
            gh repo fork $UPSTREAM --clone 2>&1 | Out-Host
            if ($LASTEXITCODE -ne 0) { Die "fork 失败，请检查网络/权限" }
            Set-Location free-renew
        }
    } else {
        $name = Ask "或输入你已有的仓库名 (owner/repo，回车=新建私有 $env:USERNAME/free-renew)"
        if ($name -eq "") { $name = "$env:USERNAME/free-renew" }
        Guard "gh repo create $name --private --clone" {
            gh repo create $name --private --clone 2>&1 | Out-Host
            if ($LASTEXITCODE -ne 0) { Die "建仓失败" }
            Set-Location ($name.Split("/")[1])
        }
    }
    $repo = git config --get remote.origin.url 2>$null
    if ($repo -match "github\.com[:/](.+?)(\.git)?$") { $repo = $Matches[1] }
    Ok "仓库就绪: $repo"
}
$repoArg = @("--repo", $repo)

# 传值一律走 stdin 管道（**不给 --body**）：彻底免疫 Windows PowerShell 5.1 对
# native 命令参数的引用改写（值含空格/引号/%/| 都不怕）。坑点：`--body -` 会把值存成
# 字面量 "-"（gh 只在完全不给 --body 时才读 stdin）。stdin 尾随换行无害：Rust config.rs
# 对每个 env 值都 .trim()。
function Set-GhSecret($name, $value) {
    if ($DryRun) { Guard "写 Secret $name" { }; return }
    $value | gh secret set $name @repoArg
    if ($LASTEXITCODE -eq 0) { Ok "Secret $name 已写入" } else { Die "Secret $name 写入失败" }
}
function Set-GhVar($name, $value) {
    if ($DryRun) { Guard "写 Variable $name = $value" { }; return }
    $value | gh variable set $name @repoArg
    if ($LASTEXITCODE -eq 0) { Ok "Variable $name = $value" } else { Die "Variable $name 写入失败" }
}

# ── [2/6] 云账号 ─────────────────────────────────────────────
Step 2 "云厂商账号（免费服务器的控制台账密，仅存入你仓库的加密 Secrets）"
$sfUser = Ask "三丰云 手机号" "13800000000"
$sfPass = Ask "三丰云 密码" "demo-pass"
Set-GhSecret "SANFENGYUN_USERNAME" $sfUser
Set-GhSecret "SANFENGYUN_PASSWORD" $sfPass
$abUser = Ask "阿贝云 手机号" "13800000000"
$abPass = Ask "阿贝云 密码" "demo-pass"
Set-GhSecret "ABEIYUN_USERNAME" $abUser
Set-GhSecret "ABEIYUN_PASSWORD" $abPass
Ok "两台云账号完成"

# ── [3/6] LLM ────────────────────────────────────────────────
Step 3 "LLM 配置（写文章用，任何 OpenAI 兼容接口）"
$llmBase  = Ask "API 基础地址 (如 https://api.example.com/v1)" "https://api.example.com/v1"
$llmKey   = Ask "API Key" "sk-demo"
$llmModel = Ask "模型名 (如 deepseek-chat / gpt-4o-mini)" "gpt-4o-mini"
$ans = Ask "发一条测试消息验证连通性? 会消耗约 20 token (默认 N)" "N"
if ($ans -match "^[yY]" -and -not $DryRun) {
    try {
        $r = Invoke-RestMethod -Method Post -Uri "$($llmBase.TrimEnd('/'))/chat/completions" `
            -Headers @{ Authorization = "Bearer $llmKey" } -ContentType "application/json" `
            -Body (@{ model = $llmModel; messages = @(@{ role = "user"; content = "回复:ok" }); max_tokens = 10 } | ConvertTo-Json -Depth 5) `
            -TimeoutSec 60
        Ok ("LLM 连通: " + $r.choices[0].message.content)
    } catch {
        Warn "LLM 测试失败: $($_.Exception.Message)"
        $go = Ask "仍要继续使用此配置? (y/N)" "N"
        if ($go -notmatch "^[yY]") { Die "请修正 LLM 配置后重跑" }
    }
} else { Write-Host "  跳过测试" }
Set-GhSecret "LLM_BASE_URL"  $llmBase
Set-GhSecret "LLM_API_KEY"   $llmKey
Set-GhSecret "LLM_MODEL"     $llmModel
Ok "LLM 配置完成"

# ── [4/6] 发文平台 ───────────────────────────────────────────
Step 4 "选择发文平台并采集其登录 Cookie"
Write-Host @"
续期需把体验文章发布到一个第三方内容平台，厂商人工审核该文章 URL。支持两种：
  1 = CSDN   （默认。需已开通博客的 CSDN 号；Cookie 用脚本自动采集，较省心）
  2 = 知乎   （需发帖正常、有重量的知乎号）
        提醒：知乎在 GitHub Actions 机房 IP 上自动发帖有触发风控/影响账号的实质风险，
        代码已加内容安全红线并'弹验证码即停不重试'，但机房 IP 的画像风险无法消除。号重要请选 1。
"@ -ForegroundColor Gray
$platform = Ask "选择发文平台 (默认 1=CSDN)" "1"
if ($platform -eq "") { $platform = "1" }

# 采集 Cookie：真机弹浏览器 + 校验新鲜度；DRY-RUN 返回占位、不弹窗。
function Get-PlatformCookie($label, $refreshRel, $cookieFile) {
    if ($DryRun) { Warn "DRY-RUN：不弹浏览器，$label Cookie 用占位值"; return "$label=dryrun-cookie" }
    Remove-Item $cookieFile -Force -ErrorAction SilentlyContinue
    $refresh = Join-Path (Get-Location) $refreshRel
    if (-not (Test-Path $refresh)) { Die "找不到 $refreshRel（请在完整仓库目录内运行本向导）" }
    Write-Host "即将弹出专用浏览器：请在其中登录 $label（登录态保留在独立 profile，供日后刷新）"
    & $refresh -NoSecretPush   # 让本向导成为 Cookie Secret 的唯一写入者，避免重复设+重复问
    if (-not (Test-Path $cookieFile)) { Die "Cookie 文件未产出，请重跑本向导或手动执行 $refreshRel" }
    $age = ((Get-Date) - (Get-Item $cookieFile).LastWriteTime).TotalMinutes
    if ($age -gt 2) { Die "Cookie 文件是 $([math]::Round($age)) 分钟前的残留，疑似本次刷新失败，请重跑" }
    (Get-Content $cookieFile -Raw).Trim()
}

if ($platform -eq "2") {
    $ack = Ask "确认已了解知乎机房 IP 风控风险并继续? (y/N)" "N"
    if ($DryRun) { $ack = "y" }   # 演练不因此中断，只为走通分支
    if ($ack -notmatch "^[yY]") { Die "已取消。如改用 CSDN，请重跑本向导并选 1" }
    Set-GhVar "PLATFORM_PROVIDER" "zhihu"
    $ck = Get-PlatformCookie "知乎" "scripts\refresh-zhihu-cookie.ps1" (Join-Path $env:TEMP "zhihu_cookies_oneline.txt")
    Set-GhSecret "ZHIHU_COOKIES" $ck
    $topics = Ask "知乎发文话题（空格分隔，回车用默认 '免费云服务器 虚拟主机'）"
    if ($topics.Trim() -ne "") { Set-GhVar "ZHIHU_TOPICS" $topics.Trim() }
    $chosenPlatform = "知乎"
    $chosenRefresh  = "scripts/refresh-zhihu-cookie.ps1"
    Ok "发文平台 = 知乎（Cookie 已入 Secret ZHIHU_COOKIES）"
} else {
    Set-GhVar "PLATFORM_PROVIDER" "csdn"
    $ck = Get-PlatformCookie "CSDN" "scripts\refresh-csdn-cookie.ps1" (Join-Path $env:TEMP "csdn_cookies_oneline.txt")
    Set-GhSecret "CSDN_COOKIES" $ck
    $chosenPlatform = "CSDN"
    $chosenRefresh  = "scripts/refresh-csdn-cookie.ps1"
    Ok "发文平台 = CSDN（Cookie 已入 Secret CSDN_COOKIES；寿命数月，过期会告警提醒）"
}

# ── [5/6] 通知 ───────────────────────────────────────────────
$notifyStatus = "未配置"
Step 5 "通知配置（出事时通过所选渠道提醒你；强烈建议配）"
Write-Host @"
通知后端选择:
  1 = OpenClaw 网关 → 微信（你已有一台跑 OpenClaw 的服务器时选这个）
  2 = 通用 Webhook（Server酱/企业微信机器人/Bark…）
  0 = 暂不配置（失败将无提醒，仅 Actions 页可见——不推荐）
"@ -ForegroundColor Gray
$backend = Ask "选择 (默认 1)" "1"
if ($backend -eq "") { $backend = "1" }
if ($backend -eq "1") {
    Write-Host "需要: 一台跑 OpenClaw 的服务器 + 公网可达的 /v1/chat/completions 端点 + basic auth bot 账号。"
    Write-Host "三个值将以 Secrets 形式存入你的仓库，Actions 运行时作为环境变量生效，无需 config.toml。"
    Write-Host "接线步骤见: docs/SETUP.md 的「OpenClaw 网关通知」一节"
    Guard "打开 SETUP.md 通知章节" { Start-Process "https://github.com/$UPSTREAM/blob/main/docs/SETUP.md" 2>$null }
    $ocUrl  = Ask "chatCompletions 完整 URL (如 https://你的域名或IP:端口/v1/chat/completions)" "https://example.com/v1/chat/completions"
    $ocUser = Ask "basic auth 用户名" "botuser"
    $ocPass = Ask "basic auth 密码" "botpass"
    if ([string]::IsNullOrWhiteSpace($ocUrl) -or [string]::IsNullOrWhiteSpace($ocUser) -or [string]::IsNullOrWhiteSpace($ocPass)) {
        Warn "URL/用户名/密码存在空值——空配置不会生效，本次已跳过通知写入。可重跑向导或手动 gh secret set"
    } else {
        Set-GhSecret "NOTIFY_OPENCLAW_URL"      $ocUrl
        Set-GhSecret "NOTIFY_OPENCLAW_USER"     $ocUser
        Set-GhSecret "NOTIFY_OPENCLAW_PASSWORD" $ocPass
        $notifyStatus = "OpenClaw→微信（Secrets 已写入）"
        $ans = Ask "发一条测试通知验证链路? agent 会真发微信给你 (默认 N)" "N"
        if ($ans -match "^[yY]" -and -not $DryRun) {
            try {
                $auth = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("${ocUser}:${ocPass}"))
                Invoke-RestMethod -Method Post -Uri $ocUrl -Headers @{ Authorization = "Basic $auth" } `
                    -ContentType "application/json" -TimeoutSec 90 `
                    -Body (@{ model = "openclaw"; messages = @(@{ role = "user"; content = "自动化安装测试:请用微信消息工具发送【free-renew 安装成功】然后只回复:已发送" }) } | ConvertTo-Json -Depth 5) | Out-Null
                Ok "请求已投出（agent 异步执行，微信以实际收到为准；网关超时也不影响送达）"
            } catch { Warn "请求异常 $($_.Exception.Message)——若为超时，agent 可能仍在执行，稍后查微信" }
        }
    }
} elseif ($backend -eq "2") {
    $wh = Ask "Webhook URL" "https://webhook.example/notify"
    if (-not [string]::IsNullOrWhiteSpace($wh)) {
        Set-GhSecret "NOTIFY_WEBHOOK_URL" $wh
        $notifyStatus = "Webhook（Secret 已写入）"
    } else {
        Warn "Webhook URL 为空，本次未配置。日后可手动设置 Secret NOTIFY_WEBHOOK_URL"
    }
} else {
    Warn "已跳过通知。出事时仅 Actions 页可见红叉——建议日后补配 NOTIFY_* Secrets"
}

# ── [6/6] 定时 + 部署确认 ────────────────────────────────────
Step 6 "定时计划与部署"
$t = Ask "每天几点(北京时间,0-23)自动检查并续期? (默认 9)" "9"
if ($t -eq "") { $t = "9" }
if ($t -notmatch "^\d{1,2}$" -or [int]$t -gt 23) { Die "小时格式不对" }
$utcH = ([int]$t - 8 + 24) % 24
$cron = "30 $utcH * * *"
Write-Host "  cron = `"$cron`" (UTC) = 北京时间 ${t}:30"

Write-Host ""
Write-Host "──────── 部署确认（以下均为拟写入项）────────" -ForegroundColor Cyan
function Mask($s) { if ($s.Length -ge 5) { $s.Substring(0,3) + "****" + $s.Substring($s.Length-2) } else { "****" } }
Write-Host "仓库:     $repo"
Write-Host "三丰云:   $(Mask $sfUser)"
Write-Host "阿贝云:   $(Mask $abUser)"
Write-Host "LLM:      $llmModel @ $llmBase"
Write-Host "发文平台: $chosenPlatform（Cookie 拟入 Secrets）"
Write-Host "通知:     $notifyStatus"
Write-Host "定时:     每天 ${t}:30 北京时间"
Write-Host ""
$ans = Ask "确认部署? (Y/n)" "Y"
if ($ans -match "^[nN]") { Die "已取消。Secrets 已写入的条目可到仓库 Settings→Secrets 手动清理。" }

# 改 cron（与默认不同才需要提交）。用 WriteAllText 避免 WinPS5.1 的 UTF8 BOM 污染 yaml。
$wfPath = Join-Path (Get-Location) ".github\workflows\renew.yml"
if (Test-Path $wfPath) {
    $yaml = Get-Content $wfPath -Raw
    if ($yaml -match '- cron: "([^"]+)"' -and $Matches[1] -ne $cron) {
        $newYaml = $yaml -replace '- cron: "[^"]*"', "- cron: `"$cron`""
        Guard "改 cron 并 commit/push renew.yml" {
            [System.IO.File]::WriteAllText($wfPath, $newYaml, (New-Object System.Text.UTF8Encoding($false)))
            git add .github/workflows/renew.yml
            git commit -m "chore: schedule = ${t}:30 CST" | Out-Null
            git push 2>&1 | Out-Null
        }
        Ok "定时拟改为每天 ${t}:30 北京时间（GitHub cron 实际触发可能延迟数分钟，属正常）"
    } else {
        Ok "定时保持现有 cron（${t}:30 北京时间无需改动）"
    }
}

# 首跑
Write-Host ""
# fork 仓库的 scheduled workflow 被 GitHub 默认禁用，需先启用一次（用 gh api 查 is_fork，url 串匹配不可靠）。
if ($repo -and $repo -ne $UPSTREAM) {
    Guard "确保 fork 的定时任务已启用" {
        try {
            $isFork = ((gh api "repos/$repo" --jq '.fork' 2>$null) -eq "true")
            if ($isFork) {
                Write-Host "  检测到 fork 仓库：启用其定时任务..." -ForegroundColor Yellow
                gh api -X PUT "repos/$repo/actions/workflows/free-server-renewal.yml/enable" 2>&1 | Out-Null
                if ($LASTEXITCODE -ne 0) { Warn "自动启用失败——请到 Actions 页点 Enable workflow" }
            }
        } catch { Warn "查询 fork 状态失败，稍后到 Actions 页手动确认已启用" }
    }
}
Guard "触发首次运行" {
    gh workflow run free-server-renewal @repoArg 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Warn "触发失败，请到 Actions 页面手动 Run workflow" }
}
Guard "打开 Actions 页" { Start-Process "https://github.com/$repo/actions" 2>$null }

Write-Host ""
if ($DryRun) {
    Write-Host "DRY-RUN 结束：以上是拟执行动作，未做任何改动。" -ForegroundColor Magenta
} else {
    Write-Host "部署完成！" -ForegroundColor Green
}
Write-Host @"
后续你唯一可能要做的事:
  - 发文平台 Cookie 过期($chosenPlatform；数月一次) → 收到通知(需已配通知) → 重跑 $chosenRefresh
  - 密码轮换 → 仓库 Settings→Secrets 直接改
  - 一切正常时它每天定时看一眼，没到期几秒退出
到期临近或异常时，若已配通知，你会收到提醒。
"@ -ForegroundColor Green
