//! free-renew：阿贝云/三丰云 免费服务器自动续期
//!
//! 流程（每天被 Actions cron 拉起，幂等）：
//! login API → check_free_delay → [未到期/审核中] 退出 / [到期]
//! → LLM 写文章 → CSDN 发布 → 截图 → multipart 提交 free_delay_add → 失败通知
//!
//! 配置优先级：环境变量 > config.toml > 内置默认（见 config.rs）。
//! 日志：终端单行 + JSONL 落盘（logs/ 或 FREE_RENEW_LOG_DIR），见 logging.rs。

mod cloud;
mod config;
mod csdn;
mod file_config;
mod http;
mod logging;
mod notify;
mod screenshot;
mod writer;

use anyhow::Result;
use cloud::RenewState;
use config::AppConfig;
use serde_json::json;

/// 单个云账号的完整续期流程。返回 Ok(true)=无事可做或成功，Ok(false)=需人工介入。
fn process_account(cfg: &AppConfig, run: &logging::RunContext, profile_key: &str) -> Result<bool> {
    let step = |name: &str| format!("{profile_key}.{name}");

    let account = match cfg.accounts.iter().find(|a| a.profile.key == profile_key) {
        Some(a) => a.clone(),
        None => {
            tracing::info!("[config] {profile_key} 未配置，跳过");
            run.event("account.skip", "skipped", json!({"key": profile_key, "reason": "not_configured"}));
            return Ok(true);
        }
    };
    let vendor = account.profile.name;
    let masked_user = logging::mask_id(&account.username);
    run.event("account.start", "ok", json!({
        "vendor": vendor, "username": masked_user,
        "login_url": account.profile.login_url,
        "http_fallback": account.profile.allow_http_fallback,
    }));

    let mut client = cloud::CloudClient::new(account.clone(), cfg.http_timeout);

    // 1. 登录 + 查状态
    let (state, extra) = match client.login_and_check() {
        Ok(v) => {
            run.event(step("login_and_check").as_str(), "ok",
                json!({"vendor": vendor, "username": masked_user}));
            v
        }
        Err(e) => {
            let detail = e.to_string();
            tracing::error!("{vendor} 登录/查状态失败: {detail}");
            run.event(step("login_and_check").as_str(), "failed", json!({
                "vendor": vendor, "error": detail,
            }));
            notify::send(&cfg.notify, &format!("{vendor} 登录/查状态失败"), &detail);
            return Ok(false);
        }
    };
    tracing::info!("{vendor} 状态: {state:?} ({extra})");
    run.event(step("check_status").as_str(), "ok", json!({
        "vendor": vendor, "state": format!("{state:?}"), "extra": extra,
    }));

    match state {
        RenewState::CanRenew => {
            run.event(step("decision").as_str(), "will_renew", json!({"vendor": vendor}));
        }
        RenewState::UnderReview => {
            tracing::info!("{vendor} 已提交待人工审核，本轮无事可做");
            run.event(step("decision").as_str(), "skip_under_review", json!({"vendor": vendor}));
            return Ok(true);
        }
        RenewState::Waiting => {
            tracing::info!("{vendor} 未到期（{extra}），本轮无事可做");
            run.event(step("decision").as_str(), "skip_waiting", json!({"vendor": vendor, "next_time": extra}));
            return Ok(true);
        }
        RenewState::Unknown => {
            tracing::warn!("{vendor} 状态未识别（{extra}），保守起见不执行续期，请人工确认");
            run.event(step("decision").as_str(), "skip_unknown", json!({"vendor": vendor, "raw": extra}));
            return Ok(true);
        }
    }

    // 2. 生成文章
    let Some(llm) = &cfg.llm else {
        notify::send(&cfg.notify, &format!("{vendor} 已到续期日但 LLM 未配置"), "检查 config.toml [ai] 或 LLM_* 环境变量");
        run.event(step("llm").as_str(), "skipped", json!({"reason": "llm_not_configured"}));
        return Ok(false);
    };
    tracing::info!("生成文章中: vendor={vendor} model={} 角度池={}篇 字数池={:?}",
        llm.model, llm.angles.len(), llm.lengths);
    run.event(step("llm.start").as_str(), "ok", json!({
        "vendor": vendor, "model": llm.model,
        "angles_count": llm.angles.len(), "max_retries": llm.max_retries,
    }));

    let article = match writer::generate_article(llm, vendor) {
        Ok(a) => {
            run.event(step("llm.done").as_str(), "ok", json!({
                "vendor": vendor, "title": a.title, "word_count": a.word_count,
                "body_preview": a.body_markdown.chars().take(300).collect::<String>(),
            }));
            a
        }
        Err(e) => {
            let detail = e.to_string();
            run.event(step("llm.done").as_str(), "failed", json!({"vendor": vendor, "error": detail}));
            notify::send(&cfg.notify, &format!("{vendor} 文章生成失败"), &detail);
            return Ok(false);
        }
    };
    tracing::info!("文章生成完毕: {} ({} 字)", article.title, article.word_count);

    // 3. 发布到发文平台
    run.event(step("publish.start").as_str(), "ok", json!({
        "vendor": vendor, "provider": cfg.platform_provider, "title": article.title,
    }));
    let url = match publish_article(cfg, vendor, &article) {
        Ok(u) => {
            run.event(step("publish.done").as_str(), "ok",
                json!({"vendor": vendor, "provider": cfg.platform_provider, "url": u}));
            tracing::info!("已发布: {u}");
            u
        }
        Err(e) => {
            let detail = e.to_string();
            run.event(step("publish.done").as_str(), "failed", json!({"vendor": vendor, "error": detail}));
            notify::send(&cfg.notify, &format!("{vendor} 发文失败"), &detail);
            return Ok(false);
        }
    };

    // 4. 等文章可访问 + 截图
    run.event(step("article_wait").as_str(), "ok", json!({"url": url, "timeout": cfg.article_ready_timeout}));
    if let Err(e) = screenshot::wait_article_ready(&url, cfg.article_ready_timeout) {
        run.event(step("article_wait").as_str(), "failed", json!({"url": url, "error": e.to_string()}));
        notify::send(&cfg.notify, &format!("{vendor} 文章页未就绪"), &e.to_string());
        return Ok(false);
    }

    let debug_dir = std::path::PathBuf::from(
        std::env::var("FREE_RENEW_DEBUG_DIR").unwrap_or_else(|_| "/tmp/freerenew-debug".into()),
    );
    let pic = match screenshot::capture(&url, &debug_dir) {
        Ok(p) => {
            let meta = std::fs::metadata(&p).ok();
            run.event(step("screenshot").as_str(), "ok", json!({
                "url": url,
                "path": p.display().to_string(),
                "bytes": meta.as_ref().map(|m| m.len()),
            }));
            p
        }
        Err(e) => {
            run.event(step("screenshot").as_str(), "failed", json!({"url": url, "error": e.to_string()}));
            notify::send(&cfg.notify, &format!("{vendor} 截图失败"), &e.to_string());
            return Ok(false);
        }
    };

    // 5. 提交
    run.event(step("submit").as_str(), "ok", json!({"vendor": vendor, "url": url}));
    let result = client.submit_renewal(&url, &pic);
    let _ = std::fs::remove_file(&pic);
    match result {
        Ok(r) if r.ok => {
            run.event(step("submit.done").as_str(), "submitted", json!({
                "vendor": vendor, "url": url, "raw": r.raw,
            }));
            tracing::info!("{vendor} 续期提交成功");
            Ok(true)
        }
        Ok(r) => {
            run.event(step("submit.done").as_str(), "rejected", json!({
                "vendor": vendor, "url": url, "raw": r.raw,
            }));
            notify::send(&cfg.notify, &format!("{vendor} 续期提交被拒"), &r.raw);
            Ok(false)
        }
        Err(e) => {
            run.event(step("submit.done").as_str(), "failed", json!({
                "vendor": vendor, "url": url, "error": e.to_string(),
            }));
            notify::send(&cfg.notify, &format!("{vendor} 提交异常"), &e.to_string());
            Ok(false)
        }
    }
}

/// 通过配置的发文平台发布文章，返回文章 URL。
fn publish_article(cfg: &AppConfig, vendor: &str, article: &writer::Article) -> Result<String> {
    match cfg.platform_provider.as_str() {
        "csdn" => {
            let Some(csdn_cfg) = &cfg.csdn else {
                anyhow::bail!("发文平台为 csdn 但未配置 Cookie（config.toml [platform.csdn] 或 CSDN_COOKIES 环境变量）");
            };
            let client = csdn::CsdnClient::new(&csdn_cfg.cookie, &csdn_cfg.app_secret, &csdn_cfg.x_ca_key);
            let tags: Vec<String> = csdn_cfg.tags.clone();
            client.publish_sync(
                &article.title,
                &article.body_markdown,
                &tags,
                &csdn_cfg.categories,
                csdn_cfg.creation_statement,
                true, // publish；草稿模式留给人工确认场景
            )
        }
        other => anyhow::bail!("未知发文平台: {other}（当前支持: csdn）"),
    }
    .map_err(|e| {
        // 平台名进错误上下文，通知里能看出是哪家发文失败
        anyhow::anyhow!("[{vendor}] {e}")
    })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let run = logging::RunContext::init()?;
    tracing::info!("=== free-renew 开始 run_id={} ===", run.run_id);
    run.event("run.start", "ok", json!({
        "version": env!("CARGO_PKG_VERSION"),
        "args": std::env::args().collect::<Vec<_>>(),
    }));

    // --config 参数：显式指定配置文件路径
    let config_path = std::env::args()
        .position(|a| a == "--config")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(std::path::PathBuf::from);

    let cfg = AppConfig::load(config_path.as_deref());
    if cfg.accounts.is_empty() {
        run.event("run.config", "failed", json!({"reason": "no_accounts"}));
        anyhow::bail!("未配置任何云账号（config.toml [clouds.*] 或 *_USERNAME/PASSWORD 环境变量）");
    }
    if cfg.llm.is_none() {
        tracing::warn!("LLM 未配置：到期时将无法生成文章（仅查询状态可用）");
    }
    run.event("run.config", "ok", json!({
        "accounts": cfg.accounts.iter().map(|a| a.profile.key).collect::<Vec<_>>(),
        "llm_model": cfg.llm.as_ref().map(|l| l.model.clone()),
        "provider": cfg.platform_provider.clone(),
        "csdn_ready": cfg.csdn.is_some(),
        "notify_webhook": !cfg.notify.webhook_url.is_empty(),
    }));

    let mut failures = 0;
    for profile in config::CLOUDS {
        if !process_account(&cfg, &run, profile.key).unwrap_or(false) {
            failures += 1;
        }
    }

    let summary = json!({
        "elapsed_secs": run.elapsed_secs(),
        "failures": failures,
    });
    if failures > 0 {
        run.event("run.end", "failed", summary);
        anyhow::bail!("{failures} 个账号需要人工介入");
    }
    run.event("run.end", "ok", summary);
    tracing::info!("=== free-renew 结束，耗时 {} 秒 ===", run.elapsed_secs());
    Ok(())
}
