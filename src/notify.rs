//! 通知：失败/成功投递到用户。绝不 panic——通知是尽力而为，不能反过来弄死主流程。
//!
//! 双后端：
//! 1. `openclaw`（推荐）：POST 网关 chatCompletions → agent → 微信消息工具。
//!    fire-and-forget 语义：agent 在服务端异步执行，客户端超时/5xx 都不影响送达
//!    （实测网关侧 100s 超时后 agent 仍完成投递）。
//! 2. `webhook`：通用 JSON POST（{"tag","title","detail"}）。
//!
//! 两者的关系是**主 + 兜底**：openclaw 明确没接单（鉴权失败/连不上/缺配置）时
//! 才走 webhook。原来只要 openclaw 是 Some 就直接 return，webhook 永远不被尝试，
//! 也不留一行日志——同时配了两套的用户并不知道自己只有一套在工作。
//!
//! 投递指令模板让 agent「立即用微信消息工具发送」，与心跳的"自主判断是否打扰"
//! 哲学不同——告警/回执是明确指令，不需要它判断值不值得。

use std::time::Duration;

use base64::Engine;
use serde_json::json;

use crate::config::{NotifyConfig, OpenClawNotify};
use crate::http::truncate_chars;

/// openclaw 后端超时：够发出请求即可，agent 在服务端异步跑完。
const OPENCLAW_TIMEOUT_SECS: u64 = 30;
/// webhook 后端超时。
const WEBHOOK_TIMEOUT_SECS: u64 = 15;
/// 进日志的通知正文预览长度（防 Actions 日志爆量）。
const LOG_PREVIEW_CHARS: usize = 500;
/// 发给 agent 的正文上限（指令本身也要占 token）。
const OPENCLAW_DETAIL_CHARS: usize = 1200;
/// 发给通用 webhook 的正文上限。
const WEBHOOK_DETAIL_CHARS: usize = 2000;

/// 发送通知。任何错误只记日志，绝不向上传播。
pub fn send(cfg: &NotifyConfig, title: &str, detail: &str) {
    tracing::warn!(
        "[notify] {title} | {}",
        truncate_chars(detail, LOG_PREVIEW_CHARS)
    );

    let mut delivered = false;
    if let Some(oc) = &cfg.openclaw {
        delivered = send_openclaw(oc, title, detail);
    }
    if delivered {
        return;
    }
    if cfg.webhook_url.is_empty() {
        if cfg.openclaw.is_some() {
            tracing::error!("[notify] openclaw 未送达且没有 webhook 兜底——这条通知发不出去");
        }
        return;
    }
    if cfg.openclaw.is_some() {
        tracing::warn!("[notify] 改用 webhook 兜底投递");
    }
    send_webhook(cfg, title, detail);
}

/// 通知用的 HTTP 客户端。构建失败只记日志并返回 None（通知是尽力而为）。
fn http_client(timeout_secs: u64) -> Option<reqwest::blocking::Client> {
    match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::error!("通知客户端构建失败 {e}（忽略，不影响主流程）");
            None
        }
    }
}

/// 返回 true = 已交给网关（fire-and-forget，实际送达以微信为准）；
/// false = 明确没接单（缺配置/鉴权失败/连不上），值得让 webhook 再试一次。
fn send_openclaw(oc: &OpenClawNotify, title: &str, detail: &str) -> bool {
    // 空配置会白发一次注定失败的请求，而且排障时分不清是"没配"还是"被网关拒"
    let missing: Vec<&str> = [
        (oc.url.trim().is_empty(), "NOTIFY_OPENCLAW_URL"),
        (oc.basic_user.trim().is_empty(), "NOTIFY_OPENCLAW_USER"),
        (
            oc.basic_password.trim().is_empty(),
            "NOTIFY_OPENCLAW_PASSWORD",
        ),
    ]
    .into_iter()
    .filter_map(|(absent, name)| absent.then_some(name))
    .collect();
    if !missing.is_empty() {
        tracing::error!("[notify] OpenClaw 后端缺 {}，本次跳过", missing.join("、"));
        return false;
    }

    // 指令化消息体（2026-09-09 实测调优）：
    //
    // message 工具 send 动作的 target 规则（微信通道实测）：
    //   * 不给 target                 → "Action send requires a target"
    //   * 给备注名/普通文本 ID         → "Unknown target"
    //   * 给 "@im.wechat"（残缺）      → sendMessage ret=-3 invalid arguments
    //   * 给 "user:<id>@im.wechat"    → ret=-3 invalid arguments（不能加前缀！）
    //   * 给裸 "<id>@im.wechat"       → ✅ 必成功
    //
    // target 是微信用户绑定 ID（每人固定）。本指令不硬编码任何具体 target，
    // 而是让 agent 用其记忆中「上次成功发送 free-renew 消息的 target」——
    // 首次部署前先给 agent 手动发一条测试消息，让该 target 进入记忆即可。
    let instruction = format!(
        "自动化告警,最高优先级。用 message 工具 send 动作(channel=openclaw-weixin, target=你记忆中上次成功发送 free-renew 消息的完整裸 @im.wechat ID,严禁加 user: 前缀)给微信发消息,内容:\n【{title}】\n{detail}\n发送成功只回复:已发送。若 ret=-3 则去掉前缀重试,若 Unknown target 则用记忆中上次成功的 target 重试。",
        title = title,
        detail = truncate_chars(detail, OPENCLAW_DETAIL_CHARS),
    );
    let payload = json!({
        "model": oc.model,
        "messages": [{"role": "user", "content": instruction}],
        "max_tokens": 300,
    });

    let Some(client) = http_client(OPENCLAW_TIMEOUT_SECS) else {
        return false;
    };
    let auth = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", oc.basic_user, oc.basic_password));

    let result = client
        .post(&oc.url)
        .header("Authorization", format!("Basic {auth}"))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send();

    match result {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                tracing::info!(
                    "[notify] openclaw 后端 HTTP {status}（agent 异步执行，送达以微信为准）"
                );
                true
            } else if matches!(status.as_u16(), 401 | 403) {
                // 401/403 = 网关在鉴权层就拒了，agent 没接单，这条告警**没送达**。
                // 别用"异步执行"话术掩盖——明确报错，用户必须修 Secrets 凭据
                tracing::error!(
                    "[notify] openclaw 网关 {status}：NOTIFY_OPENCLAW_USER/PASSWORD 与网关 basic auth 不符\
                     （或网关拒绝该来源），通知未送达（去仓库 Settings→Secrets 核对，网关侧查 htpasswd/防火墙）"
                );
                false
            } else {
                // 4xx/5xx 超时类：agent 可能已接单（fire-and-forget），只记日志
                tracing::warn!(
                    "[notify] openclaw 后端 HTTP {status}（agent 可能已接单，送达以微信为准）"
                );
                true
            }
        }
        Err(e) => {
            if e.is_timeout() {
                // 超时按"可能已接单"处理（实测网关侧执行完仍会送达）。
                // 再走 webhook 会给用户发第二条重复告警，比漏发更烦人。
                tracing::error!(
                    "[notify] openclaw 投递超时 {e}（agent 可能仍在执行，不再走 webhook 以免重复告警）"
                );
                true
            } else {
                tracing::error!("[notify] openclaw 投递失败 {e}");
                false
            }
        }
    }
}

fn send_webhook(cfg: &NotifyConfig, title: &str, detail: &str) {
    // 注意：空 URL 的早退在 send() 里已做，这里不再重复判定
    let payload = json!({
        "tag": cfg.tag,
        "title": title,
        "detail": truncate_chars(detail, WEBHOOK_DETAIL_CHARS),
    });
    let Some(client) = http_client(WEBHOOK_TIMEOUT_SECS) else {
        return;
    };
    match client.post(&cfg.webhook_url).json(&payload).send() {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            tracing::error!("通知投递失败 HTTP {}（忽略，不影响主流程）", resp.status())
        }
        Err(e) => tracing::error!("通知投递失败 {e}（忽略，不影响主流程）"),
    }
}
