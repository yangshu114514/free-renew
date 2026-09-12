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

The wizard runs 6 steps: repo → cloud account passwords → LLM API (optional test) → **choose publish platform (CSDN/Zhihu) and capture its cookie** → notification method → daily schedule (default 09:30 CST) + confirm. About 5 minutes. Preview safely first with `.\install.ps1 -DryRun` (writes nothing).

Linux/macOS or manual route: clone the repo and follow [docs/SETUP.md](docs/SETUP.md).

## Publish platform: CSDN or Zhihu

Renewal articles must go to a third-party content platform for the vendor's human review. Pick one at install; switchable later:

- **CSDN** (default): needs a CSDN account with blog enabled; cookie captured automatically by `scripts/refresh-csdn-cookie.ps1`. Simplest.
- **Zhihu**: needs an account that can post normally; cookie captured via CDP by `scripts/refresh-zhihu-cookie.ps1` (the `z_c0` auth cookie is httpOnly). ⚠️ Auto-posting to Zhihu from a datacenter IP risks triggering their risk control; the code stops-and-does-not-retry on captcha/403, but the IP-profile risk can't be removed by code. If the account matters, use CSDN.

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
- Diagnostic entry points (`--test-write` / `--test-zhihu` / screenshot test / the recovery above) are documented in [docs/SETUP.md](docs/SETUP.md) → "手动触发与故障恢复".

> The installer supports a `-DryRun` preview: `.\install.ps1 -DryRun` prints what it would do without forking, writing secrets, or triggering the workflow.

## Documentation

| Doc | Contents |
|---|---|
| [docs/SETUP.md](docs/SETUP.md) | full install guide, secrets reference, notification wiring, ops troubleshooting table |
| [docs/protocol/](docs/protocol/) | technical details: Sanfengyun/Abeiyun `cmd=` protocol, CSDN signing, Zhihu publish API (verified samples) |
| [NOTICE](NOTICE) | third-party attribution |

Architecture in one sentence: **GitHub Actions runs this repo's Rust binary daily** — vendor API login + status check (exits in seconds when not due); when due, an LLM writes a unique-angle experience article (machine-validated against banned words / required keywords / AI-tone heuristics), publishes it to the chosen content platform (CSDN or Zhihu), screenshots the page, and submits to the vendor's review queue. Scheduled workflows get disabled by GitHub after 60 days of repo inactivity — [keepalive-workflow](https://github.com/marketplace/actions/keepalive-workflow) is built in to prevent that.

## Acknowledgments

The protocol layer stands on **[BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer)** (Apache-2.0) — its author reverse-engineered the vendors' API in 2020; this project reuses the endpoints and command names, re-verified in 2026 (no source code copied; see [NOTICE](NOTICE) for the attribution statement). FreeServer stopped maintenance in 2021 because template articles no longer passed human review — this project's core improvement is LLM-generated unique articles. Thanks to Demo-Liu.


Other direct credits (full list in NOTICE):

- **[rust-headless-chrome](https://github.com/rust-headless-chrome/rust-headless-chrome)** (MIT) - CDP client; the screenshot anti-detection capability (webdriver/chrome/plugins/permissions/webgl bypass) comes from its built-in enable_stealth_mode().
- **[gautamkrishnar/keepalive-workflow](https://github.com/marketplace/actions/keepalive-workflow)** (MIT) - prevents GitHub's 60-day auto-disable of scheduled workflows.
- CSDN x-ca signing constants: public constants embedded in CSDN's own frontend JS, as documented in community articles.
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



