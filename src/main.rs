//! free-renew：阿贝云/三丰云 免费服务器自动续期
//!
//! SPDX-License-Identifier: Apache-2.0
//! 协议层致谢 BookerLiu/FreeServer (Apache-2.0)，详见 NOTICE
//!
//! 流程（每天被 Actions cron 拉起，幂等）：
//! login API → check_free_delay → [未到期/审核中] 退出 / [到期]
//! → LLM 写文章 → 发布到内容平台(CSDN/知乎) → 截图 → multipart 提交续期 → 失败通知
//!
//! 配置优先级：环境变量 > config.toml > 内置默认（见 config.rs）。
//! 日志：终端单行 + JSONL 落盘（logs/ 或 FREE_RENEW_LOG_DIR），见 logging.rs。

// 本 crate 不含 unsafe 代码；禁用以防未来无意引入 UB（严格性闸门之一）。
#![forbid(unsafe_code)]

mod cloud;
mod config;
mod csdn;
mod file_config;
mod http;
mod logging;
mod markdown;
mod notify;
mod probe;
mod screenshot;
mod waf;
mod writer;
mod zhihu;

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
    let (state, extra, raw) = match client.login_and_check() {
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
        "vendor": vendor, "state": format!("{state:?}"), "extra": extra, "raw": raw,
    }));

    match state {
        RenewState::CanRenew => {
            run.event(step("decision").as_str(), "will_renew", json!({"vendor": vendor}));
            // 交叉核对：状态接口的 delay_state 是参考字段，最近一次延期记录
            // （真历史接口）一并落日志，供事后核对审核结论
            if let Ok(hist) = client.review_history() {
                let latest = hist.pointer("/msg/content/0");
                if latest.is_some() {
                    run.event(step("history").as_str(), "ok", json!({
                        "vendor": vendor, "latest_record": latest,
                    }));
                }
            }
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
            // 状态看不懂 = 潜在风险。只写 Actions 日志用户永远看不到——发通知让人工介入
            notify::send(
                &cfg.notify,
                &format!("{vendor} 状态未识别，跳过本轮"),
                &format!("原始响应：{extra}\n程序未识别该状态组合，保守跳过。请人工核对控制台，确认到期时间。"),
            );
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

    // 全文落盘到 debug 目录（随 debug-dump artifact 上传）：日志里只留 300 字预览，
    // 一旦发文被平台删除/判违规，能第一时间从 artifact 看到**到底哪句惹的祸**
    // （2026-09-12 知乎删稿事件就是因为当时没有全文副本）。
    {
        let dbg = logging::debug_dir();
        let _ = std::fs::create_dir_all(&dbg);
        let _ = std::fs::write(
            dbg.join(format!("article-{vendor}.md")),
            format!("# {}\n\n{}", article.title, article.body_markdown),
        );
    }

    // 3. 发布到发文平台（主平台失败且有兜底时自动换平台）
    run.event(step("publish.start").as_str(), "ok", json!({
        "vendor": vendor, "provider": cfg.platform_provider,
        "fallback": cfg.platform_fallback, "title": article.title,
    }));
    let pub_out = match publish_article(cfg, run, vendor, &article) {
        Ok(o) => {
            run.event(step("publish.done").as_str(), "ok",
                json!({"vendor": vendor, "provider": o.platform, "fell_back": o.fell_back, "url": o.url}));
            tracing::info!("已发布({}): {}", o.platform, o.url);
            o
        }
        Err(e) => {
            let detail = format!("{e:#}");
            run.event(step("publish.done").as_str(), "failed", json!({"vendor": vendor, "error": detail}));
            notify::send(&cfg.notify, &format!("{vendor} 发文失败"), &detail);
            return Ok(false);
        }
    };
    let url = pub_out.url;

    // 4. 就绪检查（非致命：裸 HTTP 会被 CSDN WAF 521 挑战，仅作参考）
    //    真正的门禁在 screenshot::capture 内（Chrome 过挑战 + 标题验证）。
    if let Err(e) = screenshot::wait_article_ready(&url, cfg.article_ready_timeout) {
        tracing::warn!("{vendor} 裸 HTTP 就绪检查未通过（WAF 挑战，Chrome 可过），继续截图: {e}");
    }

    let debug_dir = logging::debug_dir();
    let pic = match screenshot::capture(&url, &article.title, &debug_dir, cookie_for_url(cfg, &url), profile_key) {
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
            // {e:#} 打印完整错误链，截图失败要看得到根因（导航超时/无字形/文件写入）
            let detail = format!("{e:#}");
            run.event(step("screenshot").as_str(), "failed", json!({"url": url, "error": detail}));
            notify::send(&cfg.notify, &format!("{vendor} 截图失败"), &detail);
            return Ok(false);
        }
    };

    // 5. 提交
    run.event(step("submit").as_str(), "ok", json!({"vendor": vendor, "url": url}));
    // 截图留档进 debug 目录（提交原件照旧使用后删除）：
    // 厂商审核若拒，run 的 debug-dump artifact 里必须有原图可对照排查。
    // 文件名带厂商 key：两家同轮提交时留档不互相覆盖。
    let archive = debug_dir.join(format!("postpone_submitted-{profile_key}.png"));
    let _ = std::fs::copy(&pic, &archive);
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
            // {e:#} 打印完整错误链：提交失败必须看到根因（超时/连接重置/HTTP 码），
            // 只 to_string() 会退化成一句"续期提交失败"，下次还是查不出为什么
            let detail = format!("{e:#}");
            run.event(step("submit.done").as_str(), "failed", json!({
                "vendor": vendor, "url": url, "error": detail,
            }));
            notify::send(&cfg.notify, &format!("{vendor} 提交异常"), &detail);
            Ok(false)
        }
    }
}

/// 截图注入的登录 Cookie 按**文章实际所在域**选，而不是"配置的主平台"：
/// 兜底切换后文章在 CSDN，若仍按主平台拿知乎 Cookie 注入，等于把 z_c0 泄漏到
/// csdn.net 的请求头里（跨域凭据泄露），且版面/折叠行为也不对。
/// 唯一来源是 config（config.rs 已合并 env/文件）。截公开文章页本无需登录态，
/// 注入只为版面一致 + 规避"未登录访客"折叠/挑战。
pub(crate) fn cookie_for_url<'a>(cfg: &'a AppConfig, url: &str) -> Option<&'a str> {
    let opt = if url.contains("zhihu.") {
        cfg.zhihu.as_ref().map(|z| z.cookie.as_str())
    } else if url.contains("csdn.") {
        cfg.csdn.as_ref().map(|c| c.cookie.as_str())
    } else {
        None
    };
    opt.filter(|c| !c.trim().is_empty())
}

/// 发布产物：URL + 实际落在哪个平台（截图 Cookie 选择、通知、排障都要看真实值）
#[derive(Debug)]
pub(crate) struct Published {
    pub url: String,
    pub platform: String,
    /// 主平台失败、由兜底平台发出
    pub fell_back: bool,
    /// 主平台的失败原因（fell_back 时供事件/通知展示）
    pub primary_reason: Option<String>,
}

/// 往指定平台发一篇。`final_publish=false` = 草稿模式（知乎停在发布前/CSDN 存草稿），
/// 链路测试用——"发到公开门槛之前一步拦截"。
pub(crate) fn publish_on(
    cfg: &AppConfig,
    platform: &str,
    vendor: &str,
    article: &writer::Article,
    final_publish: bool,
) -> Result<String> {
    match platform {
        "csdn" => {
            let Some(csdn_cfg) = &cfg.csdn else {
                anyhow::bail!("发文平台为 csdn 但未配置 Cookie（config.toml [platform.csdn] 或 CSDN_COOKIES 环境变量）");
            };
            // 空 Cookie 要在这里拦：否则一路走到 HTTP 4xx 才炸，报错毫无指向性
            if csdn_cfg.cookie.trim().is_empty() {
                anyhow::bail!("发文平台为 csdn 但 Cookie 为空——运行采集脚本或重跑 install.ps1 写入 CSDN_COOKIES");
            }
            let client = csdn::CsdnClient::new(&csdn_cfg.cookie, &csdn_cfg.app_secret, &csdn_cfg.x_ca_key);
            let tags: Vec<String> = csdn_cfg.tags.clone();
            client.publish_sync(
                &article.title,
                &article.body_markdown,
                &tags,
                &csdn_cfg.categories,
                csdn_cfg.creation_statement,
                final_publish,
            )
        }
        "zhihu" => {
            let Some(zh) = &cfg.zhihu else {
                anyhow::bail!("发文平台为 zhihu 但未配置 Cookie（config.toml [platform.zhihu] 或 ZHIHU_COOKIES 环境变量）");
            };
            // 同上：配置段落存在≠Cookie 有值，空值提前拦、报可执行结论
            if zh.cookie.trim().is_empty() {
                anyhow::bail!("发文平台为 zhihu 但 Cookie 为空——运行 scripts/refresh-zhihu-cookie.ps1 或重跑 install.ps1 写入 ZHIHU_COOKIES");
            }
            let client = zhihu::ZhihuClient::new(zh)?;
            client.publish(
                &article.title,
                &crate::markdown::to_html(&article.body_markdown, false),
                &zh.topics,
                zh.toc,
                final_publish,
            )
        }
        other => anyhow::bail!("未知发文平台: {other}（当前支持: csdn, zhihu）"),
    }
    .map_err(|e| {
        // 平台名进错误上下文，通知里能看出是哪家发文失败
        anyhow::anyhow!("[{platform}|{vendor}] {e}")
    })
}

/// 主/备编排（对发布动作泛型化，纯逻辑可单测）：主平台成功即返回；失败且配了
/// 兜底则换平台重试一次；两家都失败时错误里保留两个根因。
fn choose_and_publish<F>(primary: &str, fallback: Option<&str>, mut publish: F) -> Result<Published>
where
    F: FnMut(&str) -> Result<String>,
{
    match publish(primary) {
        Ok(url) => Ok(Published { url, platform: primary.to_string(), fell_back: false, primary_reason: None }),
        Err(e1) => {
            let reason = format!("{e1:#}");
            let Some(fb) = fallback.filter(|x| *x != primary) else {
                anyhow::bail!("{reason}");
            };
            tracing::warn!("主平台 {primary} 发文失败，自动切换兜底 {fb}: {reason}");
            match publish(fb) {
                Ok(url) => Ok(Published {
                    url,
                    platform: fb.to_string(),
                    fell_back: true,
                    primary_reason: Some(reason),
                }),
                Err(e2) => anyhow::bail!("主({primary})与兜底({fb})均发文失败｜主因: {reason}｜兜底因: {e2:#}"),
            }
        }
    }
}

/// 续期流程的正式发文：主/备编排 + 切换事件与通知（主平台健康度要让人知道）。
fn publish_article(
    cfg: &AppConfig,
    run: &logging::RunContext,
    vendor: &str,
    article: &writer::Article,
) -> Result<Published> {
    let out = choose_and_publish(
        &cfg.platform_provider,
        cfg.platform_fallback.as_deref(),
        |p| publish_on(cfg, p, vendor, article, true),
    )?;
    if out.fell_back {
        run.event("publish.fallback", "switched", json!({
            "vendor": vendor,
            "from": cfg.platform_provider,
            "to": out.platform,
            "reason": out.primary_reason,
        }));
        notify::send(
            &cfg.notify,
            &format!("{vendor} 发文已切换兜底平台 {}", out.platform),
            &format!(
                "主平台 {} 失败：{}\n文章已改由 {} 发出；主平台需要人工排查（Cookie 过期/风控）。",
                cfg.platform_provider,
                out.primary_reason.as_deref().unwrap_or("未知"),
                out.platform
            ),
        );
    }
    Ok(out)
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

    // 诊断子命令（--test-notify / --test-screenshot / --test-write / --test-zhihu）：
    // 命中则执行并在此提前返回，不进入真实续期流程。逻辑见 probe.rs。
    if probe::run_if_probe(&cfg, &run)? {
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
        "zhihu_ready": cfg.zhihu.is_some(),
        "notify_backend": if cfg.notify.openclaw.is_some() { "openclaw" }
            else if !cfg.notify.webhook_url.is_empty() { "webhook" }
            else { "none" },
    }));

    // process_account 内部已把失败写进事件与通知；这里只负责统计。
    // 曾经的 unwrap_or(false) 把 Err 静默降级成"未成功"——日志里连一行根因都不留。
    let mut failures = 0;
    for profile in config::CLOUDS {
        match process_account(&cfg, &run, profile.key) {
            Ok(true) => {}
            Ok(false) => failures += 1,
            Err(e) => {
                let detail = format!("{e:#}");
                tracing::error!("{} 续期流程异常终止: {detail}", profile.name);
                run.event("run.account_error", "failed", json!({
                    "profile": profile.key, "error": detail,
                }));
                failures += 1;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CsdnConfig, NotifyConfig, ZhihuConfig};

    fn cfg_both() -> AppConfig {
        AppConfig {
            accounts: vec![],
            llm: None,
            platform_provider: "zhihu".into(),
            platform_fallback: Some("csdn".into()),
            csdn: Some(CsdnConfig {
                cookie: "u=1".into(),
                creation_statement: 1,
                tags: vec![],
                categories: vec![],
                app_secret: "s".into(),
                x_ca_key: "k".into(),
            }),
            zhihu: Some(ZhihuConfig { cookie: "z_c0=x".into(), topics: vec![], toc: false }),
            notify: NotifyConfig { webhook_url: String::new(), tag: String::new(), openclaw: None },
            article_ready_timeout: 1,
            http_timeout: 1,
        }
    }

    #[test]
    fn cookie_follows_article_domain_not_primary() {
        // 主平台 zhihu、文章却兜底落在 CSDN 时：截图必须拿 CSDN cookie，
        // 绝不能把知乎 z_c0 带进 csdn 域（跨域凭据泄露）
        let c = cfg_both();
        assert_eq!(cookie_for_url(&c, "https://blog.csdn.net/x/details/1"), Some("u=1"));
        assert_eq!(cookie_for_url(&c, "https://zhuanlan.zhihu.com/p/1"), Some("z_c0=x"));
        assert_eq!(cookie_for_url(&c, "https://example.com/a"), None);
    }

    #[test]
    fn publish_fallback_matrix() {
        // 主成功 → 根本不叫兜底
        let mut calls: Vec<String> = vec![];
        let out = choose_and_publish("zhihu", Some("csdn"), |p| {
            calls.push(p.to_string());
            Ok("zh/1".into())
        })
        .unwrap();
        assert!(!out.fell_back && out.platform == "zhihu");
        assert_eq!(calls, vec!["zhihu".to_string()]);

        // 主失败 → 兜底顶上，主因留档
        let out = choose_and_publish("zhihu", Some("csdn"), |p| match p {
            "zhihu" => Err(anyhow::anyhow!("auth 过期")),
            _ => Ok("cs/2".into()),
        })
        .unwrap();
        assert!(out.fell_back && out.platform == "csdn" && out.url == "cs/2");
        assert!(out.primary_reason.as_deref().unwrap().contains("auth 过期"));

        // 无兜底 → 原错上抛（不吞）
        let e = choose_and_publish("zhihu", None, |p| match p {
            "zhihu" => Err(anyhow::anyhow!("boom")),
            _ => Ok("x".into()),
        })
        .unwrap_err();
        assert!(format!("{e:#}").contains("boom"));

        // 双失败 → 两个根因都要在错误里（只留一个就没法定位了）
        let e = choose_and_publish("zhihu", Some("csdn"), |p| match p {
            "zhihu" => Err(anyhow::anyhow!("zfail")),
            _ => Err(anyhow::anyhow!("cfail")),
        })
        .unwrap_err();
        let d = format!("{e:#}");
        assert!(d.contains("zfail") && d.contains("cfail"), "{d}");
    }
}

