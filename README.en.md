# free-renew

English | [简体中文](README.md)

Auto-renewal for the "permanently free cloud servers" of Abeiyun (阿贝云) and Sanfengyun (三丰云). Daily automated checks; when a renewal is due: article generation → publishing → screenshot → submission, fully automated; on success or failure you get an alert through your configured notification backend (optional — without one, results are visible on the Actions page only).

> ⚠️ **Disclaimer**: for learning and personal technical research only. Whether vendor terms permit such automation is your call; all consequences (account suspension, instance reclamation, data loss) are borne by the user. Full terms at the end of this page.

## Install

Requires [Git](https://git-scm.com/) + [GitHub CLI](https://cli.github.com/) (`gh auth login`):

```powershell
# Windows PowerShell — one-liner wizard
irm https://raw.githubusercontent.com/yangshu114514/free-renew/main/install.ps1 | iex
```

The wizard runs 6 steps: repo → cloud account passwords (**any number of servers per vendor**) → LLM API (optional test) → **choose publish platform (CSDN/Zhihu) and capture its cookie** → notification method → check schedule (default: hourly, offset off the top of the hour) + confirm. About 5 minutes. Preview safely first with `.\install.ps1 -DryRun` (writes nothing).

Linux/macOS or manual route: clone the repo and follow [docs/SETUP.md](docs/SETUP.md).

## Multiple servers: as many as you like

Each vendor supports any number of accounts (e.g. **2 Sanfengyun + 3 Abeiyun**); the program renews them one by one:

- **At install time**: the wizard asks for each server of a vendor in turn (blank line ends that vendor).
- **Adding/removing later**:
  - **Add** → re-run the wizard, or add a `SANFENGYUN_USERNAME_2` / `SANFENGYUN_PASSWORD_2` Secret pair (the 1st server keeps the unsuffixed names, so **existing deployments need no migration**), then add the matching `_N` lines to the `env:` block of `.github/workflows/renew.yml` (pre-wired up to `_6`).
  - **Remove** → re-running the wizard **auto-deletes** the now-unused numbered Secrets; if you delete them by hand, don't leave them behind — the program would keep reading them.
  - **Temporarily disable one** → set Variable `SANFENGYUN_ENABLED_2=false` (credentials stay).
  - **Give one a name** → set Variable `SANFENGYUN_LABEL_2=backup`; notifications then read "三丰云(backup)".
- **One article per server**: the article is the evidence behind *that* machine's renewal request; sharing one URL across several accounts looks like repeated applications to a human reviewer. Cost: 5 servers = up to 5 LLM generations + 5 publishes + 5 screenshots per run.
- **Serial, never concurrent**: concurrent runs would log into the same vendor and post to the same content platform simultaneously, raising risk-control and rate-limit exposure. Worst case ≈25 min per server; the job timeout defaults to 180 min. For more servers, set Variable `RUN_TIMEOUT_MINUTES = servers × 25 + 30`.
- **Scheduled runs queue instead of overlapping** (`concurrency`), so two runs never touch the same accounts at once.

> ⚠️ **Publishing quota is a real constraint**: a new CSDN account allows about 2 posts/day. With 3+ servers due on the same day, CSDN may not be enough — `PLATFORM_FALLBACK` automatically switches to the other platform, or stagger the renewal windows.

## Publish platform: CSDN / Zhihu (both supported, with automatic fallback)

Renewal articles must go to a third-party content platform for the vendor's human review. Pick one — or both — at install; switchable later:

- **CSDN** (default): needs a CSDN account with blog enabled; cookie captured automatically by `scripts/refresh-csdn-cookie.ps1`. Simplest.
- **Zhihu**: needs an account that can post normally; cookie captured via CDP by `scripts/refresh-zhihu-cookie.ps1` (the `z_c0` auth cookie is httpOnly). ⚠️ Auto-posting to Zhihu from a datacenter IP risks triggering their risk control; the code stops-and-does-not-retry on captcha/403, but the IP-profile risk can't be removed by code. If the account matters, use CSDN.
- **Both connected (you pick primary/backup)**: whichever platform is primary, if its pipeline fails (expired cookie / captcha / risk-control reject) the article is **automatically published via the other platform** in the same run, plus a notification to fix the primary. With only one platform's cookie configured, the other is never attempted. After setup, the `test_platforms` dispatch input sends one *draft* on each platform (everything stops before going public; no quota used) as a health check.

## Daily use: one command

When the publish cookie expires (weeks to months; you'll be notified if a notify backend is configured), re-run the matching script:

```powershell
.\scripts\refresh-csdn-cookie.ps1     # if using CSDN
.\scripts\refresh-zhihu-cookie.ps1    # if using Zhihu
```

The dedicated browser profile usually keeps you logged in — the script re-exports the cookie automatically and optionally pushes it to GitHub Secrets. 30 seconds.

## Content safety & recovery

- **Content red-line**: the generator hard-rejects review landmine terms (VPN/circumvention, intranet tunneling, no-ICP-filing, gray-industry, politics, …). A hit triggers a rewrite; repeated hits **abort the run rather than publish** — better to skip a renewal than post borderline content that could strike your platform account.
- **Publish → screenshot waits for the article to go public**: Zhihu/CSDN newly-published posts are briefly invisible; the screenshot step polls until it's live, so a login-wall page is never submitted as a "screenshot".
- **`--submit-existing`**: if publishing succeeded but only the "upload screenshot to vendor" POST died on network flakiness, reuse the already-published article to retry screenshot+submit **without re-posting** — no extra articles spamming your account.
- Diagnostic entry points (`--test-write` / `--test-zhihu` / `--test-platforms` two-platform draft health-check / screenshot test / the recovery above) are documented in [docs/SETUP.md](docs/SETUP.md). → "手动触发与故障恢复".

> The installer supports a `-DryRun` preview: `.\install.ps1 -DryRun` prints what it would do without forking, writing secrets, or triggering the workflow.

## Documentation

| Doc | Contents |
|---|---|
| [docs/SETUP.md](docs/SETUP.md) | full install guide, secrets reference, notification wiring, ops troubleshooting table |
| [docs/protocol/](docs/protocol/) | technical details: Sanfengyun/Abeiyun `cmd=` protocol, CSDN signing, Zhihu publish API (verified samples) |
| [NOTICE](NOTICE) | third-party attribution |

Architecture in one sentence: **GitHub Actions runs this repo's Rust binary on a schedule** — logs into each configured vendor account in turn, checks the renewal window (exits in seconds when nothing is due); when due, an LLM writes a unique-angle experience article (machine-validated against banned words / required keywords / AI-tone heuristics / content-safety red-lines), publishes it to the chosen content platform (CSDN or Zhihu), screenshots the page, and submits to the vendor's review queue. Scheduled workflows get disabled by GitHub after 60 days of repo inactivity — a self-contained keepalive job in `renew.yml` prevents that.

## Acknowledgments

The protocol layer stands on **[BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer)** (Apache-2.0) — its author reverse-engineered the vendors' API in 2020; this project reuses the endpoints and command names, re-verified in 2026 (no source code copied; see [NOTICE](NOTICE) for the attribution statement). FreeServer stopped maintenance in 2021 because template articles no longer passed human review — this project's core improvement is LLM-generated unique articles. Thanks to Demo-Liu.


Other direct credits (full list in NOTICE):

- **[rust-headless-chrome](https://github.com/rust-headless-chrome/rust-headless-chrome)** (MIT) - CDP client; the screenshot anti-detection capability (webdriver/chrome/plugins/permissions/webgl bypass) comes from its built-in enable_stealth_mode().
- ~~[gautamkrishnar/keepalive-workflow](https://github.com/marketplace/actions/keepalive-workflow)~~ (MIT) - formerly used to prevent GitHub's 60-day auto-disable of scheduled workflows; the action was blocked by GitHub for ToS reasons in 2025-04, so this project now ships a self-contained keepalive job in `renew.yml`. Kept here as a historical note.
- CSDN x-ca signing constants: public constants embedded in CSDN's own frontend JS, as documented in community articles.
- Zhihu publish flow: modeled on community HTTP implementations such as [zimya/zhihu_obsidian](https://github.com/zimya/zhihu_obsidian) (0BSD) — the create-draft → PATCH-content → attach-topic → publish endpoints, which need no x-zse-96 signing. Independent Rust implementation, no code copied (see NOTICE).
- Sanfengyun official help documents (content_1009 / content_1156) - source of the review red lines.
- NodeLoc / CSDN community threads - vendor review failure-mode intel.

## License

[Apache-2.0](LICENSE) · © 2026 yangshu114514 · third-party attribution in [NOTICE](NOTICE)

**Disclaimer (full text)**:

1. This project is for learning and personal technical research only. Users are responsible for complying with the Terms of Service of Abeiyun, Sanfengyun, CSDN, Zhihu, and all third-party platforms involved.
2. Automating the vendors' promotion-style renewal terms may not be endorsed by them. **Any consequence of using this project (account suspension, server reclamation, data loss) is borne by the user.**
3. Article content is LLM-generated. Ensure compliance with platform content policies and truthfully declare AI-assisted generation (enabled by default). Do not mass-produce spam or abuse.
4. No user credentials are stored, uploaded, or collected by this project; all configuration lives in your local files or private repo Secrets.
5. Free-server stability is decided by the vendors; NO warranty of data safety. **Always keep off-site backups.**
6. Provided "as is" under Apache-2.0. Continued use constitutes agreement with all of the above.



