//! 诊断子命令（`--test-*`）：不跑真实续期流程，单独验证某一段链路，供人工排障/验收。
//!
//! 这些入口从 main() 抽出来，保持 main 只负责"装配配置 → 逐账号续期"。
//! 命中任一探测子命令即执行并返回 Ok(true)，main 据此提前返回。
//!
//! - `--test-notify`        通知链路端到端（正式格式，不碰云厂商）
//! - `--test-screenshot <url> [title]`  截图链路（WAF 挑战 + Cookie 注入 + 标题渲染）
//! - `--test-write [vendor]`  只生成样文并打印（不发文、不碰知乎/厂商），验内容质量
//! - `--test-zhihu [vendor]`  知乎发文链路探路：建草稿+写正文+挂话题，**不发布**，返回编辑链接

use anyhow::{bail, Result};
use serde_json::json;

use crate::cloud;
use crate::config::AppConfig;
use crate::logging::RunContext;
use crate::{login_cookie, notify, screenshot, writer, zhihu};

/// vendor 位置参数：取 flag 后第一个非 `--` 参数，缺省"三丰云"。
fn vendor_arg(flag: &str) -> String {
    std::env::args()
        .position(|a| a == flag)
        .and_then(|i| std::env::args().nth(i + 1))
        .filter(|s| !s.starts_with("--"))
        .unwrap_or_else(|| "三丰云".to_string())
}

fn debug_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("FREE_RENEW_DEBUG_DIR").unwrap_or_else(|_| "/tmp/freerenew-debug".into()),
    )
}

fn test_notify(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    if cfg.notify.openclaw.is_none() && cfg.notify.webhook_url.is_empty() {
        run.event("test_notify", "failed", json!({"reason": "no_notify_backend"}));
        anyhow::bail!(
            "通知链路未配置。二选一：环境变量 NOTIFY_OPENCLAW_URL/USER/PASSWORD 三件套（或 NOTIFY_WEBHOOK_URL），\
             或 config.toml 的 [notify.openclaw] / [notify].webhook_url"
        );
    }
    let has_oc = cfg.notify.openclaw.is_some();
    let backend_label = if has_oc { "openclaw（网关 agent → 微信）" } else { "webhook" };
    let chain = if has_oc { "本工具 → 网关 → agent → 微信" } else { "本工具 → webhook" };
    let detail = format!(
        "通知后端: {}\n本轮为人工触发测试，非真实续期。你看到这条消息说明: {} 全链路可用。",
        backend_label, chain
    );
    notify::send(&cfg.notify, "free-renew 通知链路自检", &detail);
    run.event("test_notify", "ok", json!({ "backend": if has_oc { "openclaw" } else { "webhook" } }));
    println!("通知已投递（fire-and-forget），到你的 {} 查收。", if has_oc { "微信" } else { "webhook 接收端" });
    Ok(())
}

fn test_screenshot(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let pos = std::env::args().position(|a| a == "--test-screenshot").unwrap();
    let url = std::env::args()
        .nth(pos + 1)
        .ok_or_else(|| anyhow::anyhow!("--test-screenshot 需要一个文章 URL 参数"))?;
    let title = std::env::args().nth(pos + 2).unwrap_or_default();
    let pic = screenshot::capture(&url, &title, &debug_dir(), login_cookie(cfg))?;
    let meta = std::fs::metadata(&pic)?;
    let _ = run; // 截图探测无需落 JSONL 事件
    println!("截图成功: {} ({} bytes)", pic.display(), meta.len());
    Ok(())
}

fn test_write(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let Some(llm) = &cfg.llm else {
        anyhow::bail!("--test-write 需要 LLM（LLM_BASE_URL/LLM_API_KEY/LLM_MODEL）");
    };
    let vendor = vendor_arg("--test-write");
    let article = writer::generate_article(llm, &vendor)?;
    println!(
        "===== 样文（{}，{} 字）=====\n# {}\n\n{}",
        vendor, article.word_count, article.title, article.body_markdown
    );
    // 全文写成 artifact 文件，供下载后干净查看（不受 Actions 日志行前缀污染）。
    let dbg = debug_dir();
    let _ = std::fs::create_dir_all(&dbg);
    let _ = std::fs::write(
        dbg.join("article-sample.md"),
        format!("# {}\n\n{}", article.title, article.body_markdown),
    );
    run.event("test_write", "ok", json!({"vendor": vendor, "title": article.title, "word_count": article.word_count}));
    Ok(())
}

fn test_zhihu(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let Some(zh) = &cfg.zhihu else {
        anyhow::bail!("--test-zhihu 需要知乎 Cookie：设 ZHIHU_COOKIES 环境变量或 config.toml [platform.zhihu]");
    };
    let vendor = vendor_arg("--test-zhihu");
    let (title, html) = match &cfg.llm {
        Some(llm) => {
            let a = writer::generate_article(llm, &vendor)?;
            println!("生成文章: {} ({} 字)", a.title, a.word_count);
            (a.title, crate::markdown::to_html(&a.body_markdown, false))
        }
        None => {
            tracing::warn!("未配置 LLM，用固定样例正文探路（仅验证接口，不验证内容质量）");
            (
                format!("{vendor} 连通性测试草稿"),
                "<p>这是一条来自 free-renew 的接口探路草稿，非正式文章，可删。</p>".to_string(),
            )
        }
    };
    let client = zhihu::ZhihuClient::new(zh)?;
    let edit = client.publish(&title, &html, &zh.topics, zh.toc, false)?;
    run.event("test_zhihu", "ok", json!({"vendor": vendor, "draft_edit": edit}));
    println!(
        "知乎草稿探路成功（未发布）。打开这个链接在浏览器里看排版/话题/内容：\n  {edit}\n\
         满意后删掉该草稿，再把 PLATFORM_PROVIDER 设为 zhihu 走正式发布。"
    );
    Ok(())
}

/// `--submit-existing <vendor> <url> [title]`：复用一篇**已发布**的文章，只做
/// 登录厂商 → 截图该 URL → 提交续期，**绝不重新生成/重新发布**。
/// 用途：当知乎/CSDN 已成功发文、却卡在"上传截图到厂商"这一步的网络抖动时，
/// 用现成文章反复重试提交，避免每试一次就往你内容平台多灌一篇、多赌一次风控。
fn submit_existing(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let pos = args.iter().position(|a| a == "--submit-existing").unwrap();
    let vendor = args.get(pos + 1).cloned().unwrap_or_else(|| "三丰云".into());
    let url = args
        .get(pos + 2)
        .cloned()
        .filter(|s| !s.starts_with("--"))
        .ok_or_else(|| anyhow::anyhow!("--submit-existing 需要文章 URL"))?;
    let title = args.get(pos + 3).cloned().unwrap_or_default();

    let account = cfg
        .accounts
        .iter()
        .find(|a| a.profile.name == vendor || a.profile.key == vendor)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("未配置厂商账号 {vendor}（*_USERNAME/PASSWORD）"))?;

    let mut client = cloud::CloudClient::new(account, cfg.http_timeout);
    // 登录只为建立会话；状态不拦提交（冗余提交比漏提交安全）。
    let (state, ..) = client.login_and_check()?;
    tracing::warn!("submit-existing：当前状态 {state:?}（忽略，直接尝试提交现成文章）");

    let dbg = debug_dir();
    let pic = screenshot::capture(&url, &title, &dbg, login_cookie(cfg))?;
    let meta = std::fs::metadata(&pic).ok();
    tracing::info!("截图就绪 {} 字节，提交中…", meta.as_ref().map(|m| m.len()).unwrap_or(0));
    let result = client.submit_renewal(&url, &pic);
    match result {
        Ok(r) if r.ok => {
            run.event("submit_existing", "submitted", json!({"vendor": vendor, "url": url, "raw": r.raw}));
            notify::send(&cfg.notify, &format!("{vendor} 续期已提交(复用文章)"), &format!("文章: {url}\n现成文章重试提交成功，等待厂商人工审核。"));
            println!("✅ 提交成功：{url}");
            Ok(())
        }
        Ok(r) => {
            run.event("submit_existing", "rejected", json!({"vendor": vendor, "raw": r.raw}));
            bail!("提交被厂商拒绝：{}", r.raw)
        }
        Err(e) => {
            let d = format!("{e:#}");
            run.event("submit_existing", "failed", json!({"vendor": vendor, "error": &d}));
            bail!("提交失败（网络/接口）：{d}")
        }
    }
}

/// 命中任一 `--test-*` 子命令 → 执行并返回 true（main 提前退出）；否则 false。
pub fn run_if_probe(cfg: &AppConfig, run: &RunContext) -> Result<bool> {
    let has = |f: &str| std::env::args().any(|a| a == f);
    if has("--test-notify") { test_notify(cfg, run)?; return Ok(true); }
    if has("--test-screenshot") { test_screenshot(cfg, run)?; return Ok(true); }
    if has("--test-write") { test_write(cfg, run)?; return Ok(true); }
    if has("--test-zhihu") { test_zhihu(cfg, run)?; return Ok(true); }
    if has("--submit-existing") { submit_existing(cfg, run)?; return Ok(true); }
    Ok(false)
}
