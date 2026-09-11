//! free-renew：阿贝云/三丰云 免费服务器自动续期
//!
//! SPDX-License-Identifier: Apache-2.0
//! 协议层致谢 BookerLiu/FreeServer (Apache-2.0)，详见 NOTICE
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
mod waf;
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
            let detail = format!("{e:#}");
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
            let detail = format!("{e:#}");
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
            let detail = format!("{e:#}");
            run.event(step("publish.done").as_str(), "failed", json!({"vendor": vendor, "error": detail}));
            notify::send(&cfg.notify, &format!("{vendor} 发文失败"), &detail);
            return Ok(false);
        }
    };

    // 4. 就绪检查（非致命：裸 HTTP 会被 CSDN WAF 521 挑战，仅作参考）
    //    真正的门禁在 screenshot::capture 内（Chrome 过挑战 + 标题验证）。
    if let Err(e) = screenshot::wait_article_ready(&url, cfg.article_ready_timeout) {
        tracing::warn!("{vendor} 裸 HTTP 就绪检查未通过（WAF 挑战，Chrome 可过），继续截图: {e}");
    }

    let debug_dir = std::path::PathBuf::from(
        std::env::var("FREE_RENEW_DEBUG_DIR").unwrap_or_else(|_| "/tmp/freerenew-debug".into()),
    );
    let pic = match screenshot::capture(&url, &article.title, &debug_dir) {
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
            // 成功也通知：不加通知的话，唯一能确认"它活着"的方式是它一直失败
            notify::send(&cfg.notify, &format!("{vendor} 续期已提交"), &format!("文章: {url}\n等待厂商人工审核，审核结果见下轮运行日志。"));
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

    // --test-notify 子命令：端到端验证通知链路（正式格式）。
    // 在云账号检查之前执行：本子命令只需要通知配置，不碰云厂商 API。
    if std::env::args().any(|a| a == "--test-notify") {
        if cfg.notify.openclaw.is_none() && cfg.notify.webhook_url.is_empty() {
            run.event("test_notify", "failed", json!({"reason": "no_notify_backend"}));
            anyhow::bail!("通知链路未配置（config.toml [notify.openclaw] 或 [notify].webhook_url）");
        }
        let title = "free-renew 通知链路自检";
        let backend_label = if cfg.notify.openclaw.is_some() {
            "openclaw（网关 agent → 微信）"
        } else {
            "webhook"
        };
        let chain = if cfg.notify.openclaw.is_some() {
            "本工具 → 网关 → agent → 微信"
        } else {
            "本工具 → webhook"
        };
        let detail = format!(
            "通知后端: {}\n本轮为人工触发测试，非真实续期。你看到这条消息说明: {} 全链路可用。",
            backend_label, chain
        );
        notify::send(&cfg.notify, title, &detail);
        run.event("test_notify", "ok", json!({
            "backend": if cfg.notify.openclaw.is_some() { "openclaw" } else { "webhook" },
        }));
        println!(
            "通知已投递（fire-and-forget），到你的 {} 查收。",
            if cfg.notify.openclaw.is_some() { "微信" } else { "webhook 接收端" }
        );
        return Ok(());
    }

    // --test-screenshot <url>：单独验证截图链路（WAF 挑战 + Cookie 注入 + 标题渲染）
    if let Some(pos) = std::env::args().position(|a| a == "--test-screenshot") {
        let url = std::env::args()
            .nth(pos + 1)
            .ok_or_else(|| anyhow::anyhow!("--test-screenshot 需要一个文章 URL 参数"))?;
        let debug_dir = std::path::PathBuf::from(
            std::env::var("FREE_RENEW_DEBUG_DIR").unwrap_or_else(|_| "/tmp/freerenew-debug".into()),
        );
        let pic = screenshot::capture(&url, "阿贝云", &debug_dir)?;
        let meta = std::fs::metadata(&pic)?;
        println!("截图成功: {} ({} bytes)", pic.display(), meta.len());
        return Ok(());
    }

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
        "notify_backend": if cfg.notify.openclaw.is_some() { "openclaw" }
            else if !cfg.notify.webhook_url.is_empty() { "webhook" }
            else { "none" },
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



