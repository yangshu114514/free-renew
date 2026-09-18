//! 诊断子命令（`--test-*`）：不跑真实续期流程，单独验证某一段链路，供人工排障/验收。
//!
//! 这些入口从 main() 抽出来，保持 main 只负责"装配配置 → 逐账号续期"。
//! 命中任一探测子命令即执行并返回 Ok(true)，main 据此提前返回。
//!
//! - `--test-notify`        通知链路端到端（正式格式，不碰云厂商）
//! - `--test-screenshot <url> [title]`  截图链路（WAF 挑战 + Cookie 注入 + 标题渲染）
//! - `--test-write [厂商或账号ID]`  只生成样文并打印（不发文、不碰知乎/厂商），验内容质量
//! - `--test-zhihu [厂商或账号ID]`  知乎发文链路探路：建草稿+写正文+挂话题，**不发布**，返回编辑链接
//! - `--test-platforms`     已配置的发文平台逐个试链路：全部停在发布前
//!   （知乎=草稿编辑链接，CSDN=草稿预览），零公开贴文
//! - `--submit-existing <账号ID> <url> [title]`  复用已发布文章只重试截图+提交
//!
//! 同传多个子命令时，分派优先级必须与 renew.yml 的 if/elif 顺序一致
//! （submit-existing > test-write > test-zhihu > test-platforms > test-screenshot >
//! test-notify），否则"手动触发以为跑 A、实际跑了 B"。改任何一边都要同步另一边。

use anyhow::{bail, Result};
use serde_json::json;

use crate::cloud;
use crate::config::{AppConfig, CloudProfile, CLOUDS};
use crate::logging::{self, RunContext};
use crate::{cookie_for_url, notify, publish_on, screenshot, writer};

/// 位置参数解析：按"紧跟 flag 之后、且不以 `--` 开头"的连续段消费。
///
/// 原实现是"每个子命令各自 `position(flag)` + `nth(i+1)` 重新遍历整个 argv"，
/// 既重复遍历，又让 `--submit-existing --test-notify <url>` 这类误输入把下一个
/// flag 当成账号名吞掉，错误信息离根因很远。
struct ProbeArgs {
    argv: Vec<String>,
}

impl ProbeArgs {
    fn from_env() -> Self {
        Self {
            argv: std::env::args().collect(),
        }
    }

    fn has(&self, flag: &str) -> bool {
        self.argv.iter().any(|a| a == flag)
    }

    fn positionals(&self, flag: &str) -> Vec<String> {
        let Some(i) = self.argv.iter().position(|a| a == flag) else {
            return vec![];
        };
        self.argv[i + 1..]
            .iter()
            .take_while(|a| !a.starts_with("--"))
            .cloned()
            .collect()
    }
}

/// 探针的"厂商/账号"目标：给了参数就按账号 ID/厂商名/label 找；没给则
/// 只有一个账号时自动用它，多账号时明确要求指定（绝不静默挑第一个）。
fn target_profile(
    cfg: &AppConfig,
    needle: Option<&str>,
    flag: &str,
) -> Result<&'static CloudProfile> {
    let Some(n) = needle.filter(|s| !s.trim().is_empty()) else {
        return match cfg.accounts.as_slice() {
            // 没配账号也要能验内容生成：退回内置厂商表的第一个，够做 prompt 上下文
            [] => Ok(&CLOUDS[0]),
            [only] => Ok(only.profile),
            many => bail!(
                "{flag} 未指定厂商/账号，而当前配置了 {} 个账号（{}）——请显式指定。\
                 用法：{flag} <账号ID>",
                many.len(),
                cfg.account_hint()
            ),
        };
    };
    cfg.resolve_profile(n)
}

fn test_notify(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    if cfg.notify.openclaw.is_none() && cfg.notify.webhook_url.is_empty() {
        run.event(
            "test_notify",
            "failed",
            json!({"reason": "no_notify_backend"}),
        );
        bail!(
            "通知链路未配置。二选一：环境变量 NOTIFY_OPENCLAW_URL/USER/PASSWORD 三件套（或 NOTIFY_WEBHOOK_URL），\
             或 config.toml 的 [notify.openclaw] / [notify].webhook_url"
        );
    }
    let has_oc = cfg.notify.openclaw.is_some();
    let backend_label = if has_oc {
        "openclaw（网关 agent → 微信）"
    } else {
        "webhook"
    };
    let chain = if has_oc {
        "本工具 → 网关 → agent → 微信"
    } else {
        "本工具 → webhook"
    };
    let detail = format!(
        "通知后端: {}\n本轮为人工触发测试，非真实续期。你看到这条消息说明: {} 全链路可用。",
        backend_label, chain
    );
    notify::send(&cfg.notify, "free-renew 通知链路自检", &detail);
    run.event(
        "test_notify",
        "ok",
        json!({ "backend": if has_oc { "openclaw" } else { "webhook" } }),
    );
    println!(
        "通知已投递（fire-and-forget），到你的 {} 查收。",
        if has_oc {
            "微信"
        } else {
            "webhook 接收端"
        }
    );
    Ok(())
}

fn test_screenshot(cfg: &AppConfig) -> Result<()> {
    let args = ProbeArgs::from_env();
    let pos = args.positionals("--test-screenshot");
    let url = pos
        .first()
        .ok_or_else(|| anyhow::anyhow!("--test-screenshot 需要一个文章 URL 参数"))?;
    let title = pos.get(1).cloned().unwrap_or_default();
    let pic = screenshot::capture(
        url,
        &title,
        &logging::debug_dir(),
        cookie_for_url(cfg, url),
        "probe",
    )?;
    let meta = std::fs::metadata(&pic)?;
    println!("截图成功: {} ({} bytes)", pic.display(), meta.len());
    Ok(())
}

fn test_write(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let Some(llm) = &cfg.llm else {
        bail!("--test-write 需要 LLM（LLM_BASE_URL/LLM_API_KEY/LLM_MODEL）");
    };
    let args = ProbeArgs::from_env();
    let pos = args.positionals("--test-write");
    let profile = target_profile(cfg, pos.first().map(String::as_str), "--test-write")?;
    let article = writer::generate_article(llm, profile)?;
    println!(
        "===== 样文（{}，{} 字）=====\n# {}\n\n{}",
        profile.name, article.word_count, article.title, article.body_markdown
    );
    // 全文写成 artifact 文件，供下载后干净查看（不受 Actions 日志行前缀污染）。
    let dbg = logging::debug_dir();
    if let Err(e) = std::fs::create_dir_all(&dbg) {
        tracing::warn!("创建 debug 目录失败（样文不会有留档）: {e}");
    } else if let Err(e) = std::fs::write(
        dbg.join("article-sample.md"),
        format!("# {}\n\n{}", article.title, article.body_markdown),
    ) {
        tracing::warn!("样文落盘失败: {e}");
    }
    run.event(
        "test_write",
        "ok",
        json!({"vendor": profile.key, "title": article.title, "word_count": article.word_count}),
    );
    Ok(())
}

fn test_zhihu(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let ready = cfg.zhihu.as_ref().map(|z| z.ready()).unwrap_or(false);
    if !ready {
        bail!("--test-zhihu 需要知乎 Cookie：设 ZHIHU_COOKIES 环境变量或 config.toml [platform.zhihu]");
    }
    let args = ProbeArgs::from_env();
    let pos = args.positionals("--test-zhihu");
    let profile = target_profile(cfg, pos.first().map(String::as_str), "--test-zhihu")?;
    let article = match &cfg.llm {
        Some(llm) => {
            let a = writer::generate_article(llm, profile)?;
            println!("生成文章: {} ({} 字)", a.title, a.word_count);
            a
        }
        None => {
            tracing::warn!("未配置 LLM，用固定样例正文探路（仅验证接口，不验证内容质量）");
            writer::Article {
                title: format!("{} 连通性测试草稿（可删）", profile.name),
                body_markdown: "这是一条来自 free-renew 的接口探路草稿，非正式文章，可删。"
                    .to_string(),
                word_count: 30,
            }
        }
    };
    let edit = publish_on(cfg, "zhihu", probe_who(cfg), &article, false)?;
    run.event(
        "test_zhihu",
        "ok",
        json!({"vendor": profile.key, "draft_edit": edit}),
    );
    println!(
        "知乎草稿探路成功（未发布）。打开这个链接在浏览器里看排版/话题/内容：\n  {edit}\n\
         满意后删掉该草稿，再把 PLATFORM_PROVIDER 设为 zhihu 走正式发布。"
    );
    Ok(())
}

/// 探针没有具体账号时的错误上下文占位（`publish_on` 的 who 只用于报错文案）。
fn probe_who(cfg: &AppConfig) -> &str {
    cfg.accounts
        .first()
        .map(|a| a.label.as_str())
        .unwrap_or("平台体检")
}

/// 发文平台链路体检：把**已配置**的平台各发一篇草稿（停在公开门槛前一步），
/// 一次看清"知乎挂没挂、CSDN 兜底还活着吗"。零公开贴文、零厂商请求。
fn test_platforms(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let article = writer::Article {
        title: "free-renew 发文平台链路测试草稿（未发布，可删）".to_string(),
        body_markdown: "# free-renew 发文平台链路测试草稿\n\n这是安装/巡检时自动生成的链路测试内容，不会发布。\n以草稿形态存在，验证后请直接删除。\n".to_string(),
        word_count: 60,
    };
    let who = probe_who(cfg);
    let mut tried = Vec::new();
    if cfg.zhihu.as_ref().map(|z| z.ready()).unwrap_or(false) {
        tried.push(("zhihu", publish_on(cfg, "zhihu", who, &article, false)));
    }
    if cfg.csdn.as_ref().map(|c| c.ready()).unwrap_or(false) {
        tried.push(("csdn", publish_on(cfg, "csdn", who, &article, false)));
    }
    if tried.is_empty() {
        bail!("两个发文平台的 Cookie 都没配置，没有可体检的链路（跑 install.ps1 或采集脚本）");
    }
    let mut ok_any = false;
    for (p, r) in tried {
        match r {
            Ok(link) => {
                ok_any = true;
                println!("✅ {p} 链路可用（草稿停在发布前）: {link}");
                run.event(
                    "test_platforms",
                    "ok",
                    json!({"platform": p, "draft": link}),
                );
            }
            Err(e) => {
                println!("❌ {p} 链路失败: {e:#}");
                run.event(
                    "test_platforms",
                    "failed",
                    json!({"platform": p, "error": format!("{e:#}")}),
                );
            }
        }
    }
    println!(
        "体检结论：主={} 兜底={}｜去平台侧把测试草稿删除。",
        cfg.platform_provider,
        cfg.platform_fallback.as_deref().unwrap_or("无")
    );
    if ok_any {
        Ok(())
    } else {
        bail!("所有已配置平台链路均失败——对照上面原因逐个修 Cookie/风控")
    }
}

/// `--submit-existing <账号ID> <url> [title]`：复用一篇**已发布**的文章，只做
/// 登录厂商 → 截图该 URL → 提交续期，**绝不重新生成/重新发布**。
/// 用途：当知乎/CSDN 已成功发文、却卡在"上传截图到厂商"这一步的网络抖动时，
/// 用现成文章反复重试提交，避免每试一次就往你内容平台多灌一篇、多赌一次风控。
///
/// 账号参数支持 ID / 厂商名 / label；同厂商配了多个账号时必须用 ID
/// （按厂商名会歧义，程序会明确报错而不是替你挑一台）。
fn submit_existing(cfg: &AppConfig, run: &RunContext) -> Result<()> {
    let args = ProbeArgs::from_env();
    let pos = args.positionals("--submit-existing");
    let account_arg = pos.first().ok_or_else(|| {
        anyhow::anyhow!(
            "--submit-existing 需要账号参数（账号 ID，或用厂商名/label 且它唯一）。{}",
            cfg.account_hint()
        )
    })?;
    let url = pos
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("--submit-existing 需要文章 URL"))?;
    let title = pos.get(2).cloned().unwrap_or_default();
    if title.trim().is_empty() {
        // 空 title = 截图的标题校验被跳过：登录墙/首页壳也能过 → 把垃圾图提交给
        // 厂商换一句"内容不存在"。复用文章重试时务必把标题前 12 字传进来。
        tracing::warn!("未提供文章标题：截图阶段无法校验页面是不是真文章，强烈建议传 title 参数");
    }

    let account = cfg.find_account(account_arg)?;
    let mut client = cloud::CloudClient::new(account.clone(), cfg.http_timeout)?;
    // 登录只为建立会话；状态不拦提交（冗余提交比漏提交安全）。
    let (state, ..) = client.login_and_check()?;
    tracing::warn!("submit-existing：当前状态 {state:?}（忽略，直接尝试提交现成文章）");

    let dbg = logging::debug_dir();
    let pic = screenshot::capture(url, &title, &dbg, cookie_for_url(cfg, url), &account.id)?;
    let meta = std::fs::metadata(&pic).ok();
    tracing::info!(
        "截图就绪 {} 字节，提交中…",
        meta.as_ref().map(|m| m.len()).unwrap_or(0)
    );
    let result = client.submit_renewal(url, &pic);
    match result {
        Ok(r) if r.ok => {
            run.event(
                "submit_existing",
                "submitted",
                json!({"account": account.id, "vendor": account.vendor(), "url": url, "raw": r.raw}),
            );
            notify::send(
                &cfg.notify,
                &format!("{} 续期已提交(复用文章)", account.label),
                &format!("文章: {url}\n现成文章重试提交成功，等待厂商人工审核。"),
            );
            println!("✅ 提交成功：{url}");
            Ok(())
        }
        Ok(r) => {
            run.event(
                "submit_existing",
                "rejected",
                json!({"account": account.id, "raw": r.raw}),
            );
            bail!("提交被厂商拒绝：{}", r.raw)
        }
        Err(e) => {
            let d = format!("{e:#}");
            run.event(
                "submit_existing",
                "failed",
                json!({"account": account.id, "error": &d}),
            );
            bail!("提交失败（网络/接口）：{d}")
        }
    }
}

/// 命中任一 `--test-*` 子命令 → 执行并返回 true（main 提前退出）；否则 false。
/// 判定顺序 = renew.yml if/elif 优先级（submit>write>zhihu>platforms>screenshot>notify）。
pub fn run_if_probe(cfg: &AppConfig, run: &RunContext) -> Result<bool> {
    let args = ProbeArgs::from_env();
    if args.has("--submit-existing") {
        submit_existing(cfg, run)?;
        return Ok(true);
    }
    if args.has("--test-write") {
        test_write(cfg, run)?;
        return Ok(true);
    }
    if args.has("--test-zhihu") {
        test_zhihu(cfg, run)?;
        return Ok(true);
    }
    if args.has("--test-platforms") {
        test_platforms(cfg, run)?;
        return Ok(true);
    }
    if args.has("--test-screenshot") {
        test_screenshot(cfg)?;
        return Ok(true);
    }
    if args.has("--test-notify") {
        test_notify(cfg, run)?;
        return Ok(true);
    }
    Ok(false)
}
