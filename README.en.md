# free-renew

English | [简体中文](README.md)

Auto-renewal tool for the "permanently free cloud servers" of Abeiyun (阿贝云) and Sanfengyun (三丰云). Written in Rust, ships as a single ~9MB binary, runs on GitHub Actions — zero load on your own servers.

> **Why**: these vendors require users to post promotional articles on third-party platforms and submit screenshots for human review every few days. Miss a deadline and the instance is reclaimed with all data. This project dismantles that time bomb: daily automated status checks, and when a renewal is due — article generation → publishing → screenshot → submission, fully automated, with WeChat notifications on success or failure.

## Acknowledgments

This project stands on the shoulders of:

- **[BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer)** (Apache-2.0) — reverse-engineered the vendors' `cmd=` form protocol in 2020 (`login.php`, `renew.php`, the `check_free_delay` / `free_delay_list` / `free_delay_add` command family, the `yanqi_img` multipart field). This project's protocol layer follows that documentation, re-verified against live services in 2026 ([docs/protocol/](docs/protocol/)). No FreeServer source code is included; this is an independent reimplementation.
- **Core improvement over FreeServer**: the original stopped maintenance in 2021 because template articles no longer passed human review. This project generates a **unique article per run** via LLM (random angle pool + machine-validated word lists), and moved the runtime to GitHub Actions (the original required a resident server — using a free server to keep a free server alive was always a bit circular).
- CSDN `x-ca` signing algorithm: from CSDN's own frontend JS, as documented in community articles ([Tencent Cloud Community](https://cloud.tencent.com/developer/article/2420128)). `x-ca-key` and `appSecret` are public constants embedded in CSDN's web frontend.
- Sanfengyun review rules: from their official help docs (content_1009 / content_1156).

FreeServer was archived in July 2025 (read-only). Per Apache-2.0 requirements, its license and attribution are preserved in the [NOTICE](NOTICE) file.

## How it works

```
GitHub Actions cron (daily 09:30 CST, idempotent)
  → vendor API login, check free-server renewal status
  ├── not due / under review → exit (< 1 min)
  └── due ↓
      → LLM writes an experience article (random angle/length, machine-checked compliance, ≤3 rewrites)
      → publish via CSDN Markdown editor protocol (HMAC-SHA256 signed)
      → wait for page readiness → headless Chrome screenshot
      → multipart-submit article URL + screenshot to vendor renewal API
      → success / failure → OpenClaw gateway → WeChat notification
```

Key design decisions:

- **Fresh login every run, zero cookie persistence** — both vendors have short-lived sessions; persistence only adds an "expired cookie" failure mode.
- **Fire-and-forget notifications** via OpenClaw gateway's `chatCompletions` endpoint; the agent executes server-side, immune to Cloudflare's 100s timeout (verified: HTTP 524 did not prevent WeChat delivery).
- **Dual notify backends**: OpenClaw (preferred, → WeChat) and generic JSON webhook (fallback). Notification failure never blocks the renewal flow.
- **Better skip than spam**: if the LLM article fails validation 3 times, the round is abandoned. A 5-day window + daily retries makes one wasted round cheap; a blacklisted account is not.

## Verified protocols (2026-09)

Full details in [docs/protocol/](docs/protocol/) — request/response samples, field-by-field signing algorithm. Summary:

| Stage | Endpoint | Verdict |
|---|---|---|
| Sanfengyun login/status/submit | `api.sanfengyun.com/www/{login,renew}.php` | ✅ real-account verified. Plain cookie + form; no token/signature/CSRF (confirmed from the vendor console SPA's axios config) |
| Abeiyun login/status/submit | `api.abeiyun.com/www/{login,renew}.php` | ✅ real-account verified. **Non-CN IPs must fall back to HTTP 80** (HTTPS WAF-blocked; DNS has a fake AAAA record `fc00::6`) |
| CSDN publishing | `bizapi.csdn.net/blog-console-api/v3/mdeditor/saveArticle` | ✅ real article published. HMAC-SHA256; the string-to-sign has a **double newline between Accept and Content-Type** (the widely-circulated Java version omits one `\n` and is wrong) |

The two vendors return **different response shapes** (Sanfengyun `delay_state` is a Chinese status word like "审核中"; Abeiyun `delay_enable` is a JSON number). Parsers are implemented per-vendor with verified samples locked in comments (`src/cloud.rs::parse_state`) — do not merge them.

## LLM article rules

Generation and validation are separate gates:

- **Prompt constraints**: tech-blogger persona; explicitly banned: marketing tone, AI-flavored parallelism, "firstly/secondly" templates, disclaimer endings; random angle from an 8-item pool; random length (300/380/420/500 chars); temperature 1.0; required keywords (vendor name, "免费云服务器", "免费虚拟主机", official domain); forbidden words (申请延期/续费/白嫖/薅羊毛/…); 1-2 image placeholder notes.
- **Post-generation machine validation** (retry with violation feedback, ≤3 rounds): vendor name present / each required keyword / official link / zero forbidden words / ≥150 chars.
- All pools and word lists are configurable via `config.toml [ai]` — **rewrite the angle pool in your own style**.
- Publishing side: CSDN `creation_statement` defaults to `1` (AI-assisted declaration, shown on the article page). Honest by default.

## Quick start

```bash
cargo build --release
cp config.example.toml config.toml   # fill in accounts / CSDN cookie / LLM config
./target/release/free-renew          # or --config /path/to.toml
./target/release/free-renew --test-notify   # notification self-check only
```

CSDN cookie harvesting (local Chrome, scan QR, auto-export on login detection):

```bash
cargo run --release --bin csdn_cookie_export
```

### GitHub Actions (recommended)

Fork / push to your **private** repo, set repo Secrets (env-var equivalents of config.toml — see table in the Chinese README), then `workflow_dispatch` once to verify. JSONL run logs upload as `run-logs` artifacts (30-day retention).

Configuration precedence: **environment variables > config.toml > built-in defaults**.

## Project layout

```
src/
├── main.rs           # orchestration (idempotent daily flow)
├── config.rs         # runtime config (file + env merge)
├── file_config.rs    # config.toml schema
├── cloud.rs          # vendor API (per-vendor response parsing — do not merge)
├── csdn.rs           # CSDN publishing (HMAC signing + saveArticle + md→html)
├── writer.rs         # LLM article generation + compliance validation loop
├── screenshot.rs     # headless Chrome screenshot + readiness polling
├── notify.rs         # notifications (openclaw / webhook backends)
├── logging.rs        # JSONL run logs (masked credentials, per-step timing)
└── bin/
    └── csdn_cookie_export.rs  # CSDN cookie harvester (interactive QR login)
docs/protocol/        # full protocol docs (verified samples + signing algorithm)
```

Stack: Rust (reqwest blocking / headless_chrome / hmac+sha2+base64 / toml / tracing). Strictly serial, no tokio.

## Known limitations

- **50220**: reported by the community on `renew.php` (suspected locked-account-only); never reproduced here. JSONL logs will capture it if it ever appears.
- **CSDN cookie lifetime**: months. Expiry → publish failure → WeChat notification → 1-minute re-scan. The only periodic manual task in the whole system.
- **WeChat notification requires a reachable OpenClaw gateway**: if it's down, notifications are silently dropped (logged), but renewals continue.
- **Zhihu route**: not implemented (no API; `x-zse-96` signing is complex). Manual Zhihu publishing is a proven 100%-pass fallback.
- Vendor names appear in this README for technical description only; no affiliation or endorsement.

## Disclaimer

See the complete bilingual disclaimer in [README.md](README.md) (the Chinese version is authoritative). Summary: for learning and personal research only; you are responsible for complying with all vendors' terms; any consequences (account suspension, data loss) are borne by the user; always keep off-site backups of important data.

## License

Apache-2.0. Copyright notice in [LICENSE](LICENSE); third-party attribution in [NOTICE](NOTICE).
