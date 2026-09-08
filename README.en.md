# free-renew

English | [简体中文](README.md)

Auto-renewal tool for the "permanently free cloud servers" of Abeiyun (阿贝云) and Sanfengyun (三丰云). Written in Rust, ships as a single binary, runs on GitHub Actions — zero load on your own servers.

> Credits to [BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer) (Apache-2.0), which reverse-engineered the vendors' `cmd=` API protocol back in 2020. The original project stopped maintenance in 2021 because "template articles no longer pass human review". This project's core improvement: **LLM-generated, unique-per-run experience articles**.

## How it works

```
GitHub Actions cron (daily, idempotent)
  → log into vendor API, check free-server renewal status
  ├── not due / under review → exit (< 1 min)
  └── due for renewal ↓
      → LLM writes an experience article (random angle/length, compliance-checked, auto-rewritten if rejected)
      → publish to platform (CSDN supported, via its Markdown editor protocol)
      → headless Chrome screenshots the article page
      → multipart-submit article URL + screenshot to the vendor's renewal API
      → any failure → webhook notification (OpenClaw/WeChat compatible)
```

Key design: **fresh login every run, zero cookie persistence** — naturally immune to the vendors' short-lived sessions.

## Verified protocols (2026-09)

| Stage | Status |
|---|---|
| Sanfengyun `login.php` / `renew.php` (check/list/add) | ✅ verified with real account |
| Abeiyun `login.php` / `renew.php` (HTTP fallback needed for non-CN IPs) | ✅ verified |
| CSDN `mdeditor/saveArticle` (HMAC signature) | ✅ real article published |

The two vendors return different response shapes (numeric vs. Chinese status words); parsers are implemented per-vendor — see the verified sample comments in `src/cloud.rs::parse_state`. Protocol details in `docs/protocol/`.

## Quick start

```bash
cargo build --release
cp config.example.toml config.toml   # fill in accounts / CSDN cookie / LLM config
./target/release/free-renew
```

CSDN cookie harvesting (local Chrome, scan QR once, auto-export):

```bash
cargo run --release --bin csdn_cookie_export
```

### GitHub Actions (recommended)

Push to your **private** repo, set repo Secrets (env-var equivalents of config.toml fields — see table in the Chinese README), then trigger `workflow_dispatch` once to verify.

## Configuration

See [config.example.toml](config.example.toml) (comments in Chinese). Highlights:

- `enabled = false` temporarily disables an account/platform
- The `[ai]` section lets you customize angle pools, word-count pools, forbidden/required word lists — **rewrite the angle pool in your own style** for more personalized articles
- `creation_statement` defaults to `1` (CSDN "AI-assisted" declaration, honest & compliant)
- Precedence: environment variables > config.toml > built-in defaults

## Disclaimer

See the complete bilingual disclaimer in [README.md](README.md) (Chinese version is authoritative). Summary: for learning and personal research only; you are responsible for complying with all vendors' terms; any consequences (account suspension, data loss) are borne by the user; always keep off-site backups of important data.

## License

Apache-2.0 (inherited from [BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer))
