//! free-renew：阿贝云/三丰云 免费服务器自动续期
//!
//! SPDX-License-Identifier: Apache-2.0
//! 协议层致谢 BookerLiu/FreeServer (Apache-2.0)，详见 NOTICE
//!
//! 流程（每天被 Actions cron 拉起，幂等）：
//! 逐账号 login API → check_free_delay → [未到期/审核中] 下一个 / [到期]
//! → LLM 写文章 → 发布到内容平台(CSDN/知乎) → 截图 → multipart 提交续期 → 失败通知
//!
//! 多账号：`cfg.accounts` 里有多少个账号就跑多少轮，账号之间**串行**执行
//! （并发会同时向同一内容平台发文、同时登录同一厂商，风控与限流风险明显上升，
//! 日志也会交错难读）。每个账号有稳定的 `id`，事件与产物文件名都按它区分。
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

use std::path::{Path, PathBuf};

use anyhow::Result;
use cloud::RenewState;
use config::{AppConfig, CloudAccount};
use serde_json::json;

/// 单个账号的续期结果。
///
/// 改造前这里是一个 `bool`，"续期成功"与"未到期所以什么都没做"共用一个 `true`，
/// 调用方根本无法区分——统计和告警只能靠猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountOutcome {
    /// 本轮真的续期成功（提交已被厂商受理）
    Renewed,
    /// 本轮无事可做（未到期/审核中/状态未识别），不算失败
    Skipped,
    /// 出问题了，需要人工介入
    NeedHuman,
}

/// 阶段返回值：`Continue` 带着值进入下一阶段，`Done` 直接结束本账号。
enum Step<T> {
    Continue(T),
    Done(AccountOutcome),
}

/// 阶段推进语法糖：`Continue` 取值继续，`Done` 直接把结果交回 main。
macro_rules! next_or_return {
    ($step:expr) => {
        match $step {
            Step::Continue(v) => v,
            Step::Done(outcome) => return Ok(outcome),
        }
    };
}

/// 事件 step 前缀用**账号 ID**而不是厂商 key：两个三丰云账号时，用厂商 key 会让
/// 两台的每一步事件混成同一条流水，事后对账分不清哪条属于哪台。
fn step_name(account: &CloudAccount, step: &str) -> String {
    format!("{}.{step}", account.id)
}

/// 日志里的账号前缀 `[sanfengyun-2 三丰云#2]`：日志行常被单独 grep 出来看，
/// 光凭厂商名分不清同厂商的多台。
fn who(account: &CloudAccount) -> String {
    format!("[{} {}]", account.id, account.label)
}

/// 阶段失败的统一出口：写事件 + 发通知 + 标记需人工介入。
///
/// 这段样板在改造前被复制了 5 遍（main 里 5 处 + probe 里 1 处）；想给所有失败
/// 路径加"补一次重试"或改通知格式，就得改 6 个地方，漏一处就是某条失败路径静默无声。
fn fail<T>(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
    step: &str,
    what: &str,
    err: &anyhow::Error,
) -> Step<T> {
    // {e:#} 打印完整错误链：失败必须看到根因（超时/连接重置/HTTP 码），
    // 只 to_string() 会退化成一句"失败了"，下次还是查不出为什么
    let detail = format!("{err:#}");
    tracing::error!("{} {what}失败: {detail}", who(account));
    run.event(
        &step_name(account, step),
        "failed",
        json!({
            "account": account.id, "vendor": account.vendor(), "label": account.label,
            "error": detail,
        }),
    );
    notify::send(
        &cfg.notify,
        &format!("{} {what}失败", account.label),
        &detail,
    );
    Step::Done(AccountOutcome::NeedHuman)
}

/// 阶段①：登录厂商并查延期状态。返回可复用的客户端与状态。
fn stage_login(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
) -> Step<(cloud::CloudClient, RenewState, String)> {
    let vendor = account.vendor();
    run.event(
        &step_name(account, "account.start"),
        "ok",
        json!({
            "vendor": vendor, "account": account.id, "label": account.label,
            "username": logging::mask_id(&account.username),
            "login_url": account.login_url,
            "http_fallback": account.profile.allow_http_fallback,
        }),
    );

    let mut client = match cloud::CloudClient::new(account.clone(), cfg.http_timeout) {
        Ok(c) => c,
        Err(e) => return fail(cfg, run, account, "client", "初始化 HTTP 客户端", &e),
    };

    match client.login_and_check() {
        Ok((state, extra, _raw)) => {
            run.event(
                &step_name(account, "login_and_check"),
                "ok",
                json!({"vendor": vendor, "account": account.id}),
            );
            Step::Continue((client, state, extra))
        }
        Err(e) => fail(cfg, run, account, "login_and_check", "登录/查状态", &e),
    }
}

/// 阶段②：状态决策——到期才继续，其余情况本轮到此为止。
fn stage_decide(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
    client: &cloud::CloudClient,
    state: RenewState,
    extra: &str,
) -> Step<()> {
    let vendor = account.vendor();
    tracing::info!("{} {vendor} 状态: {state:?} ({extra})", who(account));
    run.event(
        &step_name(account, "check_status"),
        "ok",
        json!({
            "vendor": vendor, "account": account.id,
            "state": format!("{state:?}"), "extra": extra,
        }),
    );

    match state {
        RenewState::CanRenew => {
            run.event(
                &step_name(account, "decision"),
                "will_renew",
                json!({"vendor": vendor, "account": account.id}),
            );
            // 交叉核对：状态接口的 delay_state 只是参考字段，真历史接口里才有上一轮的
            // 审核结论，一并落日志供事后核对。拉不到不致命，但必须留痕——原来
            // `if let Ok(..)` 把错误整个吞了，等要查审核结论时才发现记录从来没落过。
            match client.review_history() {
                Ok(hist) => {
                    if let Some(latest) = hist.pointer("/msg/content/0") {
                        run.event(
                            &step_name(account, "history"),
                            "ok",
                            json!({
                                "vendor": vendor, "account": account.id,
                                "latest_record": latest,
                            }),
                        );
                    }
                }
                Err(e) => {
                    let detail = format!("{e:#}");
                    tracing::warn!(
                        "{vendor} 延期记录查询失败（不影响续期，仅少了核对信息）: {detail}"
                    );
                    run.event(
                        &step_name(account, "history"),
                        "failed",
                        json!({"vendor": vendor, "account": account.id, "error": detail}),
                    );
                }
            }
            Step::Continue(())
        }
        RenewState::UnderReview => {
            tracing::info!("{} {vendor} 已提交待人工审核，本轮无事可做", who(account));
            run.event(
                &step_name(account, "decision"),
                "skip_under_review",
                json!({"vendor": vendor, "account": account.id}),
            );
            Step::Done(AccountOutcome::Skipped)
        }
        RenewState::Waiting => {
            tracing::info!("{} {vendor} 未到期（{extra}），本轮无事可做", who(account));
            run.event(
                &step_name(account, "decision"),
                "skip_waiting",
                json!({"vendor": vendor, "account": account.id, "next_time": extra}),
            );
            Step::Done(AccountOutcome::Skipped)
        }
        RenewState::Unknown => {
            tracing::warn!(
                "{} {vendor} 状态未识别（{extra}），保守起见不执行续期，请人工确认",
                who(account)
            );
            run.event(
                &step_name(account, "decision"),
                "skip_unknown",
                json!({"vendor": vendor, "account": account.id, "raw": extra}),
            );
            // 状态看不懂 = 潜在风险。只写 Actions 日志用户永远看不到——发通知让人工介入
            notify::send(
                &cfg.notify,
                &format!("{} 状态未识别，跳过本轮", account.label),
                &format!(
                    "原始响应：{extra}\n程序未识别该状态组合，保守跳过。请人工核对控制台，确认到期时间。"
                ),
            );
            Step::Done(AccountOutcome::Skipped)
        }
    }
}

/// 文章全文落盘到 debug 目录（随 debug-dump artifact 上传）。
///
/// 日志里只留 300 字预览，一旦发文被平台删除/判违规，能第一时间从 artifact 看到
/// **到底哪句惹的祸**（2026-09-12 知乎删稿事件就是因为当时没有全文副本）。
/// 文件名带账号 ID：同厂商多账号时用厂商名会互相覆盖。
/// 落盘失败必须出声——这份副本是排障时唯一的全文证据，静默失败等于从来没有过。
fn dump_article(account: &CloudAccount, article: &writer::Article) {
    let dbg = logging::debug_dir();
    if let Err(e) = std::fs::create_dir_all(&dbg) {
        tracing::warn!("创建 debug 目录失败（文章全文不会有留档）: {e}");
        return;
    }
    let path = dbg.join(format!("article-{}.md", account.id));
    let content = format!("# {}\n\n{}", article.title, article.body_markdown);
    if let Err(e) = std::fs::write(&path, content) {
        tracing::warn!("文章全文落盘失败 {}: {e}", path.display());
    }
}

/// 阶段③：生成文章。
fn stage_article(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
) -> Step<writer::Article> {
    let vendor = account.vendor();
    let Some(llm) = &cfg.llm else {
        notify::send(
            &cfg.notify,
            &format!("{} 已到续期日但 LLM 未配置", account.label),
            "检查 config.toml [ai] 或 LLM_* 环境变量",
        );
        run.event(
            &step_name(account, "llm"),
            "skipped",
            json!({"account": account.id, "reason": "llm_not_configured"}),
        );
        return Step::Done(AccountOutcome::NeedHuman);
    };
    tracing::info!(
        "生成文章中: account={} vendor={vendor} model={} 角度池={}篇 字数池={:?}",
        account.id,
        llm.model,
        llm.angles.len(),
        llm.lengths
    );
    run.event(
        &step_name(account, "llm.start"),
        "ok",
        json!({
            "vendor": vendor, "account": account.id, "model": llm.model,
            "angles_count": llm.angles.len(), "max_retries": llm.max_retries,
        }),
    );

    // 每台机器各自一篇文章：提交给厂商的是"这台机器的续期申请所依据的文章"，
    // 多个账号共用一篇会让审核看到同一个 URL 反复申请。
    let article = match writer::generate_article(llm, account.profile) {
        Ok(a) => {
            run.event(
                &step_name(account, "llm.done"),
                "ok",
                json!({
                    "vendor": vendor, "account": account.id, "title": a.title,
                    "word_count": a.word_count,
                    "body_preview": a.body_markdown.chars().take(300).collect::<String>(),
                }),
            );
            a
        }
        Err(e) => return fail(cfg, run, account, "llm.done", "文章生成", &e),
    };
    tracing::info!(
        "{} 文章生成完毕: {} ({} 字)",
        who(account),
        article.title,
        article.word_count
    );
    dump_article(account, &article);
    Step::Continue(article)
}

/// 阶段④：发布到发文平台（主平台失败且有兜底时自动换平台）。
fn stage_publish(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
    article: &writer::Article,
) -> Step<Published> {
    let vendor = account.vendor();
    run.event(
        &step_name(account, "publish.start"),
        "ok",
        json!({
            "vendor": vendor, "account": account.id, "provider": cfg.platform_provider,
            "fallback": cfg.platform_fallback, "title": article.title,
        }),
    );
    match publish_article(cfg, run, account, article) {
        Ok(o) => {
            run.event(
                &step_name(account, "publish.done"),
                "ok",
                json!({
                    "vendor": vendor, "account": account.id, "provider": o.platform,
                    "fell_back": o.fell_back, "url": o.url,
                }),
            );
            tracing::info!("已发布({}): {}", o.platform, o.url);
            Step::Continue(o)
        }
        Err(e) => fail(cfg, run, account, "publish.done", "发文", &e),
    }
}

/// 阶段⑤：等文章可见并截图。
fn stage_capture(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
    article: &writer::Article,
    url: &str,
) -> Step<PathBuf> {
    // 就绪检查（非致命：裸 HTTP 会被 CSDN WAF 521 挑战，仅作参考）
    // 真正的门禁在 screenshot::capture 内（Chrome 过挑战 + 标题验证）。
    if let Err(e) = screenshot::wait_article_ready(url, cfg.article_ready_timeout) {
        tracing::warn!(
            "{} 裸 HTTP 就绪检查未通过（WAF 挑战，Chrome 可过），继续截图: {e}",
            account.label
        );
    }

    let debug_dir = logging::debug_dir();
    match screenshot::capture(
        url,
        &article.title,
        &debug_dir,
        cookie_for_url(cfg, url),
        &account.id,
    ) {
        Ok(p) => {
            let meta = std::fs::metadata(&p).ok();
            run.event(
                &step_name(account, "screenshot"),
                "ok",
                json!({
                    "account": account.id, "url": url,
                    "path": p.display().to_string(),
                    "bytes": meta.as_ref().map(|m| m.len()),
                }),
            );
            Step::Continue(p)
        }
        Err(e) => fail(cfg, run, account, "screenshot", "截图", &e),
    }
}

/// 阶段⑥：提交续期（终段，直接给出本账号的结果）。
fn stage_submit(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
    client: &cloud::CloudClient,
    url: &str,
    pic: &Path,
) -> AccountOutcome {
    let vendor = account.vendor();
    run.event(
        &step_name(account, "submit"),
        "ok",
        json!({"vendor": vendor, "account": account.id, "url": url}),
    );

    // 截图留档进 debug 目录（提交原件照旧使用后删除）：
    // 厂商审核若拒，run 的 debug-dump artifact 里必须有原图可对照排查。
    // 归档失败就**不能**删原图，否则现场彻底丢失且无人知晓。
    let archive = logging::debug_dir().join(format!("postpone_submitted-{}.png", account.id));
    let archived = match std::fs::copy(pic, &archive) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("截图归档失败 {}: {e}（保留原图不删）", archive.display());
            false
        }
    };

    let result = client.submit_renewal(url, pic);
    if archived {
        let _ = std::fs::remove_file(pic);
    }

    match result {
        Ok(r) if r.ok => {
            run.event(
                &step_name(account, "submit.done"),
                "submitted",
                json!({"vendor": vendor, "account": account.id, "url": url, "raw": r.raw}),
            );
            tracing::info!("{} {vendor} 续期提交成功", who(account));
            // 成功也通知：不加通知的话，唯一能确认"它活着"的方式是它一直失败
            notify::send(
                &cfg.notify,
                &format!("{} 续期已提交", account.label),
                &format!("文章: {url}\n等待厂商人工审核，审核结果见下轮运行日志。"),
            );
            AccountOutcome::Renewed
        }
        Ok(r) => {
            run.event(
                &step_name(account, "submit.done"),
                "rejected",
                json!({"vendor": vendor, "account": account.id, "url": url, "raw": r.raw}),
            );
            notify::send(
                &cfg.notify,
                &format!("{} 续期提交被拒", account.label),
                &r.raw,
            );
            AccountOutcome::NeedHuman
        }
        Err(e) => {
            let detail = format!("{e:#}");
            run.event(
                &step_name(account, "submit.done"),
                "failed",
                json!({"vendor": vendor, "account": account.id, "url": url, "error": detail}),
            );
            notify::send(&cfg.notify, &format!("{} 提交异常", account.label), &detail);
            AccountOutcome::NeedHuman
        }
    }
}

/// 单个云账号的完整续期流程：六个阶段串起来，编排本身只有六行。
fn process_account(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
) -> Result<AccountOutcome> {
    let (client, state, extra) = next_or_return!(stage_login(cfg, run, account));
    next_or_return!(stage_decide(cfg, run, account, &client, state, &extra));
    let article = next_or_return!(stage_article(cfg, run, account));
    let published = next_or_return!(stage_publish(cfg, run, account, &article));
    let pic = next_or_return!(stage_capture(cfg, run, account, &article, &published.url));
    Ok(stage_submit(
        cfg,
        run,
        account,
        &client,
        &published.url,
        &pic,
    ))
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
/// `who` 只用于错误上下文（账号展示名），让通知里看得出是哪台机器的发文失败。
pub(crate) fn publish_on(
    cfg: &AppConfig,
    platform: &str,
    who: &str,
    article: &writer::Article,
    final_publish: bool,
) -> Result<String> {
    // 分派单独一个函数，错误统一在这里加"平台 + 账号"前缀。
    // 直接在 match 里 bail! 会绕过这层 map_err（bail 是 return），
    // "未知平台/Cookie 为空"这类最早的失败反而没有任何上下文。
    dispatch_publish(cfg, platform, article, final_publish)
        .map_err(|e| anyhow::anyhow!("[{platform}|{who}] {e}"))
}

fn dispatch_publish(
    cfg: &AppConfig,
    platform: &str,
    article: &writer::Article,
    final_publish: bool,
) -> Result<String> {
    match platform {
        "csdn" => {
            let csdn_cfg = require_configured(
                cfg.csdn.as_ref(),
                "csdn",
                "Cookie",
                "config.toml [platform.csdn] 或 CSDN_COOKIES 环境变量",
            )?;
            if !csdn_cfg.ready() {
                anyhow::bail!("发文平台为 csdn 但 Cookie 为空——运行采集脚本或重跑 install.ps1 写入 CSDN_COOKIES");
            }
            let client =
                csdn::CsdnClient::new(&csdn_cfg.cookie, &csdn_cfg.app_secret, &csdn_cfg.x_ca_key)?;
            client.publish_sync(
                &article.title,
                &article.body_markdown,
                &csdn_cfg.tags,
                &csdn_cfg.categories,
                csdn_cfg.creation_statement,
                final_publish,
            )
        }
        "zhihu" => {
            let zh = require_configured(
                cfg.zhihu.as_ref(),
                "zhihu",
                "Cookie",
                "config.toml [platform.zhihu] 或 ZHIHU_COOKIES 环境变量",
            )?;
            if !zh.ready() {
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
}

/// "配置段存在且 Cookie 有值"的统一检查：两家平台原本各抄一份同构代码，
/// 再加一家平台就要再抄第三份，而提示文案里的脚本路径会各自漂移。
fn require_configured<'a, T>(
    section: Option<&'a T>,
    platform: &str,
    field: &str,
    hint: &str,
) -> Result<&'a T> {
    section.ok_or_else(|| anyhow::anyhow!("发文平台为 {platform} 但未配置 {field}（{hint}）"))
}

/// 主/备编排（对发布动作泛型化，纯逻辑可单测）：主平台成功即返回；失败且配了
/// 兜底则换平台重试一次；两家都失败时错误里保留两个根因。
fn choose_and_publish<F>(primary: &str, fallback: Option<&str>, mut publish: F) -> Result<Published>
where
    F: FnMut(&str) -> Result<String>,
{
    match publish(primary) {
        Ok(url) => Ok(Published {
            url,
            platform: primary.to_string(),
            fell_back: false,
            primary_reason: None,
        }),
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
                Err(e2) => anyhow::bail!(
                    "主({primary})与兜底({fb})均发文失败｜主因: {reason}｜兜底因: {e2:#}"
                ),
            }
        }
    }
}

/// 续期流程的正式发文：主/备编排 + 切换事件与通知（主平台健康度要让人知道）。
fn publish_article(
    cfg: &AppConfig,
    run: &logging::RunContext,
    account: &CloudAccount,
    article: &writer::Article,
) -> Result<Published> {
    let out = choose_and_publish(
        &cfg.platform_provider,
        cfg.platform_fallback.as_deref(),
        |p| publish_on(cfg, p, &account.label, article, true),
    )?;
    if out.fell_back {
        run.event(
            "publish.fallback",
            "switched",
            json!({
                "account": account.id, "vendor": account.vendor(), "label": account.label,
                "from": cfg.platform_provider, "to": out.platform, "reason": out.primary_reason,
            }),
        );
        notify::send(
            &cfg.notify,
            &format!("{} 发文已切换兜底平台 {}", account.label, out.platform),
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

/// 命令行参数。集中解析一次：原来 `--config` 用 position + nth 各遍历一遍 argv，
/// 且 `--config` 位于末尾或缺值时静默当成"没指定"，用户以为换了配置文件实际没有。
struct Cli {
    config_path: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<Cli> {
    let mut config_path = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--config" {
            let v = args
                .get(i + 1)
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| {
                    anyhow::anyhow!("--config 需要一个配置文件路径（用法：--config ./config.toml）")
                })?;
            config_path = Some(PathBuf::from(v));
            i += 2;
        } else {
            // 其余 flag 由 probe::run_if_probe 分派
            i += 1;
        }
    }
    Ok(Cli { config_path })
}

/// 进日志的命令行参数：凭据类参数迟早会加（--password/--cookie），而 args 会原样
/// 进 JSONL 并被 debug-dump artifact 上传，提前把口子堵上。
fn loggable_args(args: &[String]) -> Vec<String> {
    const SECRET_FLAGS: &[&str] = &["--password", "--cookie", "--token", "--api-key", "--secret"];
    let mut out = Vec::new();
    let mut hide_next = false;
    for a in args {
        if hide_next {
            hide_next = false;
            out.push("<已隐藏>".to_string());
            continue;
        }
        if SECRET_FLAGS.contains(&a.as_str()) {
            hide_next = true;
        }
        out.push(a.clone());
    }
    out
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let run = logging::RunContext::init()?;
    let args: Vec<String> = std::env::args().collect();
    let cli = parse_args(&args)?;

    tracing::info!("=== free-renew 开始 run_id={} ===", run.run_id);
    run.event(
        "run.start",
        "ok",
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "args": loggable_args(&args),
        }),
    );

    // 配置文件存在但坏 = 硬错误：走 Result 而不是 panic，这样事件日志里
    // 能留下完整错误链（进程崩溃的话只剩一段栈回溯，artifact 里什么线索都没有）
    let cfg = match AppConfig::load(cli.config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            run.event("run.config", "failed", json!({"error": format!("{e:#}")}));
            return Err(e);
        }
    };

    // 诊断子命令（--test-notify / --test-screenshot / --test-write / --test-zhihu）：
    // 命中则执行并在此提前返回，不进入真实续期流程。逻辑见 probe.rs。
    if probe::run_if_probe(&cfg, &run)? {
        return Ok(());
    }

    if cfg.accounts.is_empty() {
        run.event("run.config", "failed", json!({"reason": "no_accounts"}));
        anyhow::bail!(
            "未配置任何云账号（config.toml [clouds.*] 或 SANFENGYUN_/ABEIYUN_ 环境变量）"
        );
    }
    if cfg.llm.is_none() {
        tracing::warn!("LLM 未配置：到期时将无法生成文章（仅查询状态可用）");
    }
    run.event(
        "run.config",
        "ok",
        json!({
            "accounts": cfg.accounts.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            "account_count": cfg.accounts.len(),
            "llm_model": cfg.llm.as_ref().map(|l| l.model.clone()),
            "provider": cfg.platform_provider.clone(),
            "csdn_ready": cfg.csdn.as_ref().map(|c| c.ready()).unwrap_or(false),
            "zhihu_ready": cfg.zhihu.as_ref().map(|z| z.ready()).unwrap_or(false),
            "notify_backend": if cfg.notify.openclaw.is_some() { "openclaw" }
                else if !cfg.notify.webhook_url.is_empty() { "webhook" }
                else { "none" },
            // 本次真正传进来的可选项环境变量名（只有名字，没有值）。
            // 排障"我明明配了 X 却没生效"最快的一眼：变量没出现在这里，
            // 就是它在工作流里没被透传（Actions 最常见的原因是 env 段少一行）。
            "env_present": config::ENV_KEYS
                .iter()
                .copied()
                .filter(|k| std::env::var(k).map(|v| !v.trim().is_empty()).unwrap_or(false))
                .collect::<Vec<_>>(),
        }),
    );

    // 逐账号串行续期。process_account 内部已把失败写进事件与通知，这里只负责统计。
    // 曾经的 unwrap_or(false) 把 Err 静默降级成"未成功"——日志里连一行根因都不留。
    let mut renewed = 0usize;
    let mut skipped = 0usize;
    let mut failures = 0usize;
    for account in &cfg.accounts {
        tracing::info!("--- 账号 {} ({}) ---", account.id, account.label);
        match process_account(&cfg, &run, account) {
            Ok(AccountOutcome::Renewed) => renewed += 1,
            Ok(AccountOutcome::Skipped) => skipped += 1,
            Ok(AccountOutcome::NeedHuman) => failures += 1,
            Err(e) => {
                let detail = format!("{e:#}");
                tracing::error!("{} 续期流程异常终止: {detail}", account.label);
                run.event(
                    "run.account_error",
                    "failed",
                    json!({
                        "account": account.id, "vendor": account.vendor(), "error": detail,
                    }),
                );
                failures += 1;
            }
        }
    }

    let summary = json!({
        "elapsed_secs": run.elapsed_secs(),
        "accounts": cfg.accounts.len(),
        "renewed": renewed,
        "skipped": skipped,
        "failures": failures,
    });
    if failures > 0 {
        run.event("run.end", "failed", summary);
        anyhow::bail!("{failures} 个账号需要人工介入");
    }
    run.event("run.end", "ok", summary);
    tracing::info!(
        "=== free-renew 结束，耗时 {} 秒（续期 {renewed}／跳过 {skipped}）===",
        run.elapsed_secs()
    );
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
            zhihu: Some(ZhihuConfig {
                cookie: "z_c0=x".into(),
                topics: vec![],
                toc: false,
            }),
            notify: NotifyConfig {
                webhook_url: String::new(),
                tag: String::new(),
                openclaw: None,
            },
            article_ready_timeout: 1,
            http_timeout: 1,
        }
    }

    #[test]
    fn cookie_follows_article_domain_not_primary() {
        // 主平台 zhihu、文章却兜底落在 CSDN 时：截图必须拿 CSDN cookie，
        // 绝不能把知乎 z_c0 带进 csdn 域（跨域凭据泄露）
        let c = cfg_both();
        assert_eq!(
            cookie_for_url(&c, "https://blog.csdn.net/x/details/1"),
            Some("u=1")
        );
        assert_eq!(
            cookie_for_url(&c, "https://zhuanlan.zhihu.com/p/1"),
            Some("z_c0=x")
        );
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

    #[test]
    fn publish_on_reports_platform_and_account_in_error() {
        // 错误里必须同时有平台名与账号名，否则多账号多平台时看不出是谁失败了
        let mut cfg = cfg_both();
        cfg.csdn = Some(CsdnConfig {
            cookie: "   ".into(), // 段落存在但 Cookie 为空
            creation_statement: 1,
            tags: vec![],
            categories: vec![],
            app_secret: "s".into(),
            x_ca_key: "k".into(),
        });
        let article = writer::Article {
            title: "t".into(),
            body_markdown: "b".into(),
            word_count: 1,
        };
        let e = publish_on(&cfg, "csdn", "三丰云(主力)", &article, true).unwrap_err();
        let d = format!("{e:#}");
        assert!(d.contains("csdn") && d.contains("三丰云(主力)"), "{d}");
    }

    #[test]
    fn parse_args_requires_value_for_config() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_args(&args(&["free-renew"]))
            .unwrap()
            .config_path
            .is_none());
        assert_eq!(
            parse_args(&args(&["free-renew", "--config", "a.toml"]))
                .unwrap()
                .config_path
                .unwrap()
                .to_str()
                .unwrap(),
            "a.toml"
        );
        // 缺值必须明确报错，而不是静默用默认配置
        assert!(parse_args(&args(&["free-renew", "--config"])).is_err());
        assert!(parse_args(&args(&["free-renew", "--config", "--test-notify"])).is_err());
    }

    #[test]
    fn loggable_args_hides_secret_values() {
        let args: Vec<String> = ["free-renew", "--password", "hunter2", "--test-notify"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let logged = loggable_args(&args);
        assert!(!logged.iter().any(|a| a == "hunter2"), "{logged:?}");
        assert!(logged.iter().any(|a| a == "<已隐藏>"));
    }
}
