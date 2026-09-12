#Requires -Version 5.1
<#
.SYNOPSIS
  free-renew 卸载：清理写入 GitHub 的 Secrets / Variables 与本地浏览器 Cookie profile。

.DESCRIPTION
  默认安全：**不加 -Execute 时等价 DryRun，只列出将删除的东西，不动任何数据。**
  真正删除需显式 -Execute 并二次确认。绝不删除仓库/fork 本体（那是你自己的资产，
  要删去 GitHub 网页手动删）。

  清理范围：
  - GitHub Secrets：SANFENGYUN_*、ABEIYUN_*、LLM_*、CSDN_COOKIES、ZHIHU_COOKIES、NOTIFY_*
  - GitHub Variables：PLATFORM_PROVIDER、ZHIHU_TOPICS
  - 本地：专用 cookie profile 目录 + %TEMP% 里的 *_cookies_oneline.txt

  保留（不碰）：本仓库代码、你的 GitHub Actions 定时任务记录、厂商/内容平台的真实账号。

.PARAMETER Repo
  owner/repo。省略则从当前 git 目录的 origin 自动推断。

.PARAMETER DryRun
  只列出，不删除（默认行为；即使不加此开关，没 -Execute 也不会删）。

.PARAMETER Execute
  真正执行删除（会二次确认）。仍建议配 -Yes 供非交互。

.PARAMETER Yes
  跳过交互确认（配合 -Execute）。
#>
[CmdletBinding()]
param(
    [string]$Repo,
    [switch]$DryRun,
    [switch]$Execute,
    [switch]$Yes
)

$ErrorActionPreference = "Stop"

$SECRETS = @(
    "SANFENGYUN_USERNAME","SANFENGYUN_PASSWORD",
    "ABEIYUN_USERNAME","ABEIYUN_PASSWORD",
    "LLM_BASE_URL","LLM_API_KEY","LLM_MODEL",
    "CSDN_COOKIES","ZHIHU_COOKIES",
    "NOTIFY_OPENCLAW_URL","NOTIFY_OPENCLAW_USER","NOTIFY_OPENCLAW_PASSWORD",
    "NOTIFY_WEBHOOK_URL"
)
$VARS = @("PLATFORM_PROVIDER","ZHIHU_TOPICS")

# 只有显式 -Execute 才真删；否则一律演练。
$doDelete = $Execute.IsPresent -and -not $DryRun.IsPresent

function Step($m){ Write-Host "`n== $m ==" -ForegroundColor Cyan }
function Ok($m){ Write-Host "  [ok] $m" -ForegroundColor Green }
function Info($m){ Write-Host "  $m" }

# ── 前置：gh 可用 + 解析仓库 ──────────────────────────────────
Step "前置检查"
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { Write-Host "  [x] 需要 gh CLI"; exit 1 }
if (-not $Repo) {
    try {
        if ((git rev-parse --is-inside-work-tree 2>$null) -ne "true") { throw }
        $u = git config --get remote.origin.url 2>$null
        if ($u -match "github\.com[:/](.+?)(\.git)?/?$") { $Repo = $Matches[1] }
    } catch {}
}
if (-not $Repo) { Write-Host "  [x] 未能推断仓库，请用 -Repo owner/repo 指定"; exit 1 }
$repoArg = @("--repo", $Repo)
if ($doDelete) { Write-Host "  模式：真正删除（repo=$Repo）" -ForegroundColor Yellow }
else { Write-Host "  模式：DRY-RUN 演练，仅列出不删除（repo=$Repo）" -ForegroundColor Magenta }

# ── 二次确认 ──────────────────────────────────────────────────
if ($doDelete -and -not $Yes) {
    Write-Host ""
    Info "即将从 $Repo 删除下面这些 Secrets/Variables，并清理本地 cookie profile。仓库本体不动。"
    $a = Read-Host "确认继续? 输入 yes 执行"
    if ($a -ne "yes") { Write-Host "  已取消。"; exit 0 }
}

function Remove-GhSecret($name) {
    if (-not $doDelete) { Info "[dry] gh secret delete $name"; return }
    gh secret delete $name @repoArg 2>$null
    if ($LASTEXITCODE -eq 0) { Ok "删除 Secret $name" } else { Info "  （Secret $name 不存在或已删）" }
}
function Remove-GhVar($name) {
    if (-not $doDelete) { Info "[dry] gh variable delete $name"; return }
    gh variable delete $name @repoArg 2>$null
    if ($LASTEXITCODE -eq 0) { Ok "删除 Variable $name" } else { Info "  （Variable $name 不存在或已删）" }
}

# ── GitHub Secrets ────────────────────────────────────────────
Step "GitHub Secrets"
foreach ($s in $SECRETS) { Remove-GhSecret $s }

# ── GitHub Variables ──────────────────────────────────────────
Step "GitHub Variables"
foreach ($v in $VARS) { Remove-GhVar $v }

# ── 本地 cookie profile / 文件 ────────────────────────────────
Step "本地浏览器 profile 与 Cookie 文件"
$paths = @(
    (Join-Path $env:LOCALAPPDATA "free-renew\csdn-profile"),
    (Join-Path $env:LOCALAPPDATA "free-renew\zhihu-profile"),
    (Join-Path $env:TEMP "csdn_cookies_oneline.txt"),
    (Join-Path $env:TEMP "zhihu_cookies_oneline.txt")
)
foreach ($p in $paths) {
    if (-not (Test-Path $p)) { Info "  跳过（不存在）: $p"; continue }
    if (-not $doDelete) { Info "[dry] 删除本地: $p"; continue }
    Remove-Item $p -Recurse -Force -ErrorAction SilentlyContinue
    if (-not (Test-Path $p)) { Ok "删除本地 $p" } else { Info "  （删除失败/占用: $p）" }
}

# ── 收尾 ──────────────────────────────────────────────────────
Step "完成"
if ($doDelete) {
    Ok "已清理 Secrets/Variables 与本地 cookie profile。"
    Info "未删除：本仓库/fork 本体、Actions 运行历史、厂商与内容平台的真实账号（需各自平台手动注销）。"
    Info "停用定时任务：GitHub Actions 页 → 该 workflow → 'Disable workflow'（本脚本不自动停）。"
} else {
    Info "DRY-RUN 结束：上面 [dry] 行即真实执行会删除的条目。要真删：加 -Execute。"
}
