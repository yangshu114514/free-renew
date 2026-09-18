#Requires -Version 5.1
<#
.SYNOPSIS
  free-renew 交互式安装向导：仓库 → 云账号 → LLM → 发文平台(CSDN/知乎)Cookie → 通知 → 定时与首跑。

.DESCRIPTION
  全程问答式，本地登录 Git + GitHub CLI 即可完成。所有配置以加密 Secrets / 仓库 Variables
  存入你自己的私有副本，代码不接触凭据。

  同一家云厂商可配任意多台（例如 2 台三丰云 + 3 台阿贝云）：第 1 台用无后缀的
  SANFENGYUN_USERNAME/PASSWORD（与旧版安装完全一致），第 2 台起用 _2、_3……
  重跑本向导并少配几台时，脚本会自动清掉多出来的编号 Secrets。

.PARAMETER DryRun
  演练模式：只走一遍问答与决策、打印将要执行的动作，绝不 fork、不写 Secret、不改 cron、
  不触发 workflow。用于安全验证脚本逻辑（也便于自动化测试）。

.PARAMETER Answers
  配合 -DryRun 使用：按问答顺序预置答案数组，非交互地跑通整条向导（测试用途）。

.EXAMPLE
  .\install.ps1
  # 演练：三丰云配 2 台、阿贝云不配、走 CSDN、不配通知（答案按问答顺序给出）
  .\install.ps1 -DryRun -Answers @("13800000000","pw1","主力","13900000000","pw2","","","","https://api.example.com/v1","sk-demo","gpt-4o-mini","N","1","0","9","Y")
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
Step 2 "云厂商账号（同一家可配多台——2 台三丰云 + 3 台阿贝云都行；两家都不配就没有可续期的对象）"
Write-Host @"
  逐台输入即可，回车结束该厂商。凭据只写入你仓库的加密 Secrets。
  第 1 台用 SANFENGYUN_USERNAME / SANFENGYUN_PASSWORD（与旧版安装完全一致），
  第 2 台起用 SANFENGYUN_USERNAME_2 / _3 ……，程序按序号逐台续期。
  想临时停用某一台：给对应编号加 SANFENGYUN_ENABLED_2=false 变量即可，不必删凭据。
"@ -ForegroundColor Gray

# 逐台采集某个厂商的账号；回车 = 结束该厂商。
function Read-CloudAccounts($label) {
    $list = @()
    while ($true) {
        $n = $list.Count + 1
        $hint = if ($n -eq 1) { "（回车跳过该厂商）" } else { "（回车结束）" }
        $u = "$(Ask "$label 第 $n 台 手机号 $hint")".Trim()
        if ($u -eq "") { break }
        $p = "$(Ask "$label 第 $n 台 密码")"
        if ($p -eq "") { Warn "$label 第 $n 台密码为空 → 该台跳过，本厂商采集结束"; break }
        $lbl = "$(Ask "$label 第 $n 台 备注名（可选，如 主力/备用；只用于通知与排障，回车跳过）")".Trim()
        $list += [pscustomobject]@{ User = $u; Pass = $p; Label = $lbl }
    }
    return ,$list
}

# 写入某厂商的全部账号。第 1 台用无后缀变量（旧部署零迁移），第 2 台起用 _N。
function Set-CloudAccounts($label, $keyPrefix, $accounts) {
    if ($accounts.Count -eq 0) { Warn "$label 未配置 → 跳过此厂商"; return }
    for ($i = 0; $i -lt $accounts.Count; $i++) {
        $suffix = if ($i -eq 0) { "" } else { "_" + ($i + 1) }
        Set-GhSecret "${keyPrefix}_USERNAME$suffix" $accounts[$i].User
        Set-GhSecret "${keyPrefix}_PASSWORD$suffix" $accounts[$i].Pass
        if ($accounts[$i].Label -ne "") { Set-GhVar "${keyPrefix}_LABEL$suffix" $accounts[$i].Label }
    }
    Ok "$label 已配置 $($accounts.Count) 台"
}

# 清理上一次残留的多余账号 Secret。
# 上次装 3 台、这次只配 2 台时，第 3 台的 Secret 仍会被程序读到并继续续期——
# "删掉一台"这个动作不清理编号变量，在程序侧等于根本没做。
function Remove-StaleCloudSecrets($keyPrefix, $keepCount) {
    if ($DryRun) { return }
    $secretNames = @(gh secret list @repoArg 2>$null | ForEach-Object { ($_ -split '\s+')[0] })
    foreach ($n in $secretNames) {
        if ($n -match "^${keyPrefix}_(USERNAME|PASSWORD|ENABLED)_(\d+)$" -and [int]$Matches[2] -gt $keepCount) {
            gh secret delete $n @repoArg 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) { Ok "清理残留 Secret $n（账号数已减少，不再使用）" }
        }
    }
    $varNames = @(gh variable list @repoArg 2>$null | ForEach-Object { ($_ -split '\s+')[0] })
    foreach ($n in $varNames) {
        if ($n -match "^${keyPrefix}_(LABEL|ENABLED)_(\d+)$" -and [int]$Matches[2] -gt $keepCount) {
            gh variable delete $n @repoArg 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) { Ok "清理残留 Variable $n（账号数已减少，不再使用）" }
        }
    }
}

$sfAccounts = Read-CloudAccounts "三丰云"
$abAccounts = Read-CloudAccounts "阿贝云"
Set-CloudAccounts "三丰云" "SANFENGYUN" $sfAccounts
Set-CloudAccounts "阿贝云" "ABEIYUN" $abAccounts
Remove-StaleCloudSecrets "SANFENGYUN" $sfAccounts.Count
Remove-StaleCloudSecrets "ABEIYUN" $abAccounts.Count
if ($sfAccounts.Count -eq 0 -and $abAccounts.Count -eq 0 -and -not $DryRun) {
    Die "两家云账号都没配置——本工具没有可续期的对象"
}
Ok "云账号完成（三丰云 $($sfAccounts.Count) 台、阿贝云 $($abAccounts.Count) 台）"

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
续期需把体验文章发布到一个第三方内容平台，厂商人工审核该文章 URL。支持：
  1 = CSDN   （需已开通博客的 CSDN 号；Cookie 寿命数月，较省心）
  2 = 知乎   （需发帖正常、有重量的知乎号）
        提醒：知乎在 GitHub Actions 机房 IP 上自动发帖有触发风控/影响账号的实质风险，
        代码已加内容安全红线并'弹验证码即停不重试'，但机房 IP 的画像风险无法消除。
  3 = 两家都连（主/备自选：CSDN 主或知乎主，主平台链路出问题当轮自动切另一家并发通知）
"@ -ForegroundColor Gray
$platform = Ask "选择发文平台 (1/2/3，默认 1)" "1"
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

if ($platform -eq "2" -or $platform -eq "3") {
    $ack = Ask "确认已了解知乎机房 IP 风控风险并继续? (y/N)" "N"
    if ($DryRun) { $ack = "y" }   # 演练不因此中断，只为走通分支
    if ($ack -notmatch "^[yY]") { Die "已取消。如改用 CSDN，请重跑本向导并选 1" }
}

if ($platform -eq "2") {
    # 单平台：显式关兜底——仓库里可能残留另一家的旧 Cookie Secret，
    # "自动互备"会让重装/换选的用户被陈Cookie拖累出诡异行为
    Set-GhVar "PLATFORM_PROVIDER" "zhihu"
    Set-GhVar "PLATFORM_FALLBACK" "none"
    $ck = Get-PlatformCookie "知乎" "scripts\refresh-zhihu-cookie.ps1" (Join-Path $env:TEMP "zhihu_cookies_oneline.txt")
    Set-GhSecret "ZHIHU_COOKIES" $ck
    $topics = Ask "知乎发文话题（空格分隔，回车用默认 '免费云服务器 虚拟主机'）"
    if ("$topics".Trim() -ne "") { Set-GhVar "ZHIHU_TOPICS" "$topics".Trim() }
    $chosenPlatform = "知乎"
    $chosenRefresh  = "scripts/refresh-zhihu-cookie.ps1"
    Ok "发文平台 = 知乎（Cookie 已入 Secret ZHIHU_COOKIES）"
} elseif ($platform -eq "3") {
    # 双平台：主备方向由用户自选。谁都可能哪天挂掉（Cookie 过期/风控/验证码），
    # 主平台挂了自动切另一家发出并通知——但"自动切"≠"不用修"。
    $prim = Ask "主发文平台选哪家? (1=CSDN 主、知乎备 / 2=知乎 主、CSDN 备，默认 2)" "2"
    if ($prim -eq "") { $prim = "2" }
    if ($prim -eq "1") {
        Set-GhVar "PLATFORM_PROVIDER" "csdn"
        Set-GhVar "PLATFORM_FALLBACK" "zhihu"
        $ckc = Get-PlatformCookie "CSDN" "scripts\refresh-csdn-cookie.ps1" (Join-Path $env:TEMP "csdn_cookies_oneline.txt")
        Set-GhSecret "CSDN_COOKIES" $ckc
        $ck = Get-PlatformCookie "知乎" "scripts\refresh-zhihu-cookie.ps1" (Join-Path $env:TEMP "zhihu_cookies_oneline.txt")
        Set-GhSecret "ZHIHU_COOKIES" $ck
        $chosenPlatform = "CSDN 为主 + 知乎兜底"
        Ok "发文平台 = CSDN 优先，链路故障自动切知乎（两家 Cookie 均已入 Secrets）"
    } else {
        Set-GhVar "PLATFORM_PROVIDER" "zhihu"
        Set-GhVar "PLATFORM_FALLBACK" "csdn"
        $ck = Get-PlatformCookie "知乎" "scripts\refresh-zhihu-cookie.ps1" (Join-Path $env:TEMP "zhihu_cookies_oneline.txt")
        Set-GhSecret "ZHIHU_COOKIES" $ck
        $ckc = Get-PlatformCookie "CSDN" "scripts\refresh-csdn-cookie.ps1" (Join-Path $env:TEMP "csdn_cookies_oneline.txt")
        Set-GhSecret "CSDN_COOKIES" $ckc
        $chosenPlatform = "知乎为主 + CSDN 兜底"
        Ok "发文平台 = 知乎优先，链路故障自动切 CSDN（两家 Cookie 均已入 Secrets）"
    }
    $topics = Ask "知乎发文话题（空格分隔，回车用默认 '免费云服务器 虚拟主机'）"
    if ("$topics".Trim() -ne "") { Set-GhVar "ZHIHU_TOPICS" "$topics".Trim() }
    $chosenRefresh = "知乎: scripts/refresh-zhihu-cookie.ps1  CSDN: scripts/refresh-csdn-cookie.ps1"
} else {
    Set-GhVar "PLATFORM_PROVIDER" "csdn"
    Set-GhVar "PLATFORM_FALLBACK" "none"
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
Write-Host @"
  GitHub 的定时任务在高负载时会延迟或跳过，而"整点"正是最拥堵的时刻
  （实测：写成整点时，一天只触发了 6 次而期望 24 次）。程序本身幂等，
  未到期几秒退出、不刷通知，所以默认每小时查一次最稳，且刻意避开整点。
"@ -ForegroundColor Gray
$freq = Ask "检查频率: 1=每小时(推荐)  2=每天一次 (默认 1)" "1"
if ($freq -eq "2") {
    $t = Ask "每天几点(北京时间,0-23)? (默认 9)" "9"
    if ($t -eq "") { $t = "9" }
    if ($t -notmatch "^\d{1,2}$" -or [int]$t -gt 23) { Die "小时格式不对" }
    $utcH = ([int]$t - 8 + 24) % 24
    # 分钟取 23 而非 30/0：同样是躲开整点拥堵
    $cron = "23 $utcH * * *"
    $scheduleDesc = "每天北京时间 ${t}:23"
    $scheduleNote = "每天 ${t}:23 北京时间"
} else {
    $cron = "7 * * * *"
    $scheduleDesc = "每小时（UTC 的第 7 分钟）"
    $scheduleNote = "每小时（UTC 第 7 分钟，避开整点拥堵）"
}
Write-Host "  cron = $cron (UTC) → $scheduleDesc"

# ── 同步 renew.yml 的云账号 env 块 ───────────────────────────
# 为什么必须做：GitHub Actions 不支持通配符 Secrets——**没在 env 段里声明的编号
# 变量，程序根本读不到**。向导往仓库写了 SANFENGYUN_USERNAME_7，而 renew.yml 里
# 没有对应行，第 7 台就会静默不续期（日志里连一条都没有）。这正是"改了程序没改
# 脚本"最容易踩的坑，所以由向导自己保证两边一致。
#
# 至少生成 6 个槽位：日后手动加台时直接改对应 Secret 名即可，不必再动 yml。
$script:MinCloudSlots = 6

function New-CloudEnvBlock($sfCount, $abCount) {
    $slots = [Math]::Max($script:MinCloudSlots, [Math]::Max($sfCount, $abCount))
    $lines = @()
    $lines += '      # >>> CLOUD_ACCOUNTS_BEGIN (由 install.ps1 自动维护，也可手动编辑) >>>'
    foreach ($entry in @(@("SANFENGYUN", $sfCount), @("ABEIYUN", $abCount))) {
        $key = $entry[0]
        for ($i = 1; $i -le $slots; $i++) {
            $suffix = ''
            if ($i -gt 1) { $suffix = '_' + $i }
            # 单引号拼串：`${{ ... }}` 在双引号里会被 PowerShell 当成变量解析
            $lines += '      ' + $key + '_USERNAME' + $suffix + ': ${{ secrets.' + $key + '_USERNAME' + $suffix + ' }}'
            $lines += '      ' + $key + '_PASSWORD' + $suffix + ': ${{ secrets.' + $key + '_PASSWORD' + $suffix + ' }}'
            $lines += '      ' + $key + '_LABEL' + $suffix + ': ${{ vars.' + $key + '_LABEL' + $suffix + ' }}'
            $lines += '      ' + $key + '_ENABLED' + $suffix + ': ${{ vars.' + $key + '_ENABLED' + $suffix + ' }}'
            # 端点覆盖：厂商 WAF 拉黑 Actions 出口 IP 时指向自建中继（默认留空 = 用官方端点）
            $lines += '      ' + $key + '_LOGIN_URL' + $suffix + ': ${{ vars.' + $key + '_LOGIN_URL' + $suffix + ' }}'
            $lines += '      ' + $key + '_RENEW_URL' + $suffix + ': ${{ vars.' + $key + '_RENEW_URL' + $suffix + ' }}'
        }
    }
    $lines += '      # <<< CLOUD_ACCOUNTS_END <<<'
    return ($lines -join "`n")
}

# 用标记定位替换（不做正则，块内容里全是 ${{ }} 之类的正则元字符）。
# 找不到标记返回 $null，由调用方警告——那是旧版 renew.yml，不能瞎猜结构。
function Update-CloudEnvBlock($yaml, $sfCount, $abCount) {
    $begin = '# >>> CLOUD_ACCOUNTS_BEGIN'
    $end = '# <<< CLOUD_ACCOUNTS_END <<<'
    $i = $yaml.IndexOf($begin)
    $j = $yaml.IndexOf($end)
    if ($i -lt 0 -or $j -le $i) { return $null }
    # 必须从**行首**开始替换：标记前的 6 个缩进空格属于这一行，若不纳入替换范围，
    # 新块自带的缩进就会叠加上去——重跑一次向导 BEGIN 行多 6 个空格，
    # 跑几次 YAML 缩进就废了。这是幂等性的前提。
    $lineStart = $yaml.LastIndexOf("`n", [Math]::Max(0, $i - 1)) + 1
    # 行尾也必须跟随原文件：生成器默认用 LF，而 Windows 上 clone 出来的 yml 是 CRLF。
    # 混用会让"内容其实没变"的比较判定为有变——每重跑一次向导就 commit 一次空改动，
    # 提交历史被垃圾填满，真正的变更淹没在里面。
    $eol = "`n"
    if ($yaml.Contains("`r`n")) { $eol = "`r`n" }
    $block = (New-CloudEnvBlock $sfCount $abCount)
    $block = ($block -replace "`r`n", "`n") -replace "`n", $eol
    return $yaml.Substring(0, $lineStart) + $block + $yaml.Substring($j + $end.Length)
}

Write-Host ""
Write-Host "──────── 部署摘要 ────────" -ForegroundColor Cyan
function Mask($s) { if ($s.Length -ge 5) { $s.Substring(0,3) + "****" + $s.Substring($s.Length-2) } else { "****" } }
function MaskAccounts($accounts) {
    if ($accounts.Count -eq 0) { return "(跳过)" }
    return (($accounts | ForEach-Object {
        if ($_.Label -ne "") { "$(Mask $_.User)[$($_.Label)]" } else { Mask $_.User }
    }) -join "、")
}
Write-Host "仓库:     $repo"
Write-Host "三丰云:   $(MaskAccounts $sfAccounts)   共 $($sfAccounts.Count) 台"
Write-Host "阿贝云:   $(MaskAccounts $abAccounts)   共 $($abAccounts.Count) 台"
Write-Host "合计:     $($sfAccounts.Count + $abAccounts.Count) 台服务器将逐台检查续期"
Write-Host "LLM:      $llmModel @ $llmBase"
Write-Host "发文平台: $chosenPlatform（Cookie 已入 Secrets）"
Write-Host "通知:     $notifyStatus"
Write-Host "定时:     $scheduleNote"
Write-Host ""
Write-Host "注意：上面除 cron 与账号 env 块外的配置【已实际写入】仓库 Secrets/Variables；" -ForegroundColor DarkGray
Write-Host "      这里的确认只控制后续三个动作——改 renew.yml、启用定时、触发首跑。" -ForegroundColor DarkGray
$ans = Ask "继续完成部署? (Y/n)" "Y"
if ($ans -match "^[nN]") { Die "已停止。注意：此前写入的 Secrets/Variables 仍在仓库，可用 uninstall.ps1 -Execute 清理。" }

# 改 cron + 同步账号 env 块（任一有变化就一起 commit/push）。
# 用 WriteAllText 避免 WinPS5.1 的 UTF8 BOM 污染 yaml。
$wfPath = Join-Path (Get-Location) ".github\workflows\renew.yml"
if (-not (Test-Path $wfPath)) {
    Warn "找不到 .github\workflows\renew.yml（请在完整仓库目录内运行向导）"
    Warn "账号 env 块无法自动同步——请手动确认 env 段里每台服务器都有对应的 _N 行，否则多出来的台数程序读不到"
} else {
    $yaml = Get-Content $wfPath -Raw
    $newYaml = $yaml
    $wfChanges = @()

    if ($newYaml -match '- cron: "([^"]+)"' -and $Matches[1] -ne $cron) {
        $newYaml = $newYaml -replace '- cron: "[^"]*"', ('- cron: "' + $cron + '"')
        $wfChanges += "cron → $cron"
    }

    $withBlock = Update-CloudEnvBlock $newYaml $sfAccounts.Count $abAccounts.Count
    if ($null -eq $withBlock) {
        Warn "renew.yml 里没有 CLOUD_ACCOUNTS_BEGIN/END 标记——无法自动同步账号 env 块"
        Warn "这是旧版 renew.yml：请手动确认 env 段覆盖了你配置的每一台（缺行 = 那台静默不续期）"
    } elseif ($withBlock -ne $newYaml) {
        $newYaml = $withBlock
        $wfChanges += "账号 env 块 → 三丰云 $($sfAccounts.Count) 台 / 阿贝云 $($abAccounts.Count) 台"
    }

    if ($wfChanges.Count -eq 0) {
        Ok "renew.yml 无需改动（cron 与账号 env 块都已是最新）"
    } else {
        Guard ("改 renew.yml 并 commit/push：" + ($wfChanges -join "；")) {
            [System.IO.File]::WriteAllText($wfPath, $newYaml, (New-Object System.Text.UTF8Encoding($false)))
            git add .github/workflows/renew.yml
            git commit -m ("chore: 同步 renew.yml（" + ($wfChanges -join "；") + "）") | Out-Null
            git push 2>&1 | Out-Null
        }
        Ok ("renew.yml 已更新：" + ($wfChanges -join "；"))
    }
}

# 首跑
Write-Host ""
# fork 仓库的 scheduled workflow 被 GitHub 默认禁用，需先启用一次（用 gh api 查 is_fork，url 串匹配不可靠）。
# enable 端点的 workflow id 只认【文件名】renew.yml——实测工作流显示名
# "free-server-renewal"（及其 .yml 变体）一律 404，此前这一句从未成功启用过。
if ($repo -and $repo -ne $UPSTREAM) {
    Guard "确保 fork 的定时任务已启用" {
        try {
            $isFork = ((gh api "repos/$repo" --jq '.fork' 2>$null) -eq "true")
            if ($isFork) {
                Write-Host "  检测到 fork 仓库：启用其定时任务..." -ForegroundColor Yellow
                gh api -X PUT "repos/$repo/actions/workflows/renew.yml/enable" 2>&1 | Out-Null
                if ($LASTEXITCODE -ne 0) { Warn "自动启用失败——请到 Actions 页点 Enable workflow" }
            }
        } catch { Warn "查询 fork 状态失败，稍后到 Actions 页手动确认已启用" }
    }
}
Guard "触发首次运行" {
    gh workflow run renew.yml @repoArg 2>&1 | Out-Null
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
  - 增删服务器 → 重跑本向导（多配几台就继续加，少配几台会自动清理残留的编号 Secrets）
  - 临时停用某一台 → 仓库 Settings→Variables 里给对应编号加 XXX_ENABLED_N=false（如 ABEIYUN_ENABLED_2）
  - 密码轮换 → 仓库 Settings→Secrets 直接改对应编号的项
  - 一切正常时它按定时计划逐台检查，没到期几秒退出
到期临近或异常时，若已配通知，你会收到提醒（每台各一条，标题里带厂商名与备注名）。
"@ -ForegroundColor Green
