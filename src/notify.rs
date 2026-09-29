//! 通知：失败/成功投递到用户。绝不 panic——通知是尽力而为，不能反过来弄死主流程。
//!
//! 三后端，按序主 + 兜底（第一个送达即停，避免重复告警）：
//! 1. `openclaw`（推荐）：POST 网关 chatCompletions → agent → 微信消息工具。
//!    fire-and-forget 语义：agent 在服务端异步执行，客户端超时/5xx 都不影响送达
//!    （实测网关侧 100s 超时后 agent 仍完成投递）。
//!    ⚠️ 网关走 Cloudflare 橙云时，GitHub runner（数据中心 IP）的请求会被 CF 边缘
//!    managed challenge 拦成 403（请求根本到不了源站）——此时本会静默，由下面兜底。
//! 2. `pushplus`：POST pushplus.plus 公网 API 推微信（不经过 CF，GHA 可达；
//!    免费层每日 200 条，失败告警频率远低于此）。
//! 3. `webhook`：通用 JSON POST（{"tag","title","detail"}）。
//!
//! 投递指令模板让 agent「立即用微信消息工具发送」，与心跳的"自主判断是否打扰"
//! 哲学不同——告警/回执是明确指令，不需要它判断值不值得。

use std::time::Duration;

use base64::Engine;
use serde_json::json;

use crate::config::{NotifyConfig, OpenClawNotify};
use crate::http::truncate_chars;

/// openclaw 后端超时：2026-09-29 起走 /tools/invoke 工具直调（不经 LLM agent），
/// 同步秒级返回 deliveryStatus，30s 足够覆盖微信通道发送。
const OPENCLAW_TIMEOUT_SECS: u64 = 30;
/// free-renew 通知的微信绑定目标：裸 "<id>@im.wechat" 是唯一合法格式
/// （加 user: 前缀 ret=-3、残缺 ID 也 ret=-3——2026-09-09 实测）。
/// 与 sf-monitor Worker 告警用同一绑定 ID（该 ID 亦存于 agent 记忆）；改绑定时两处同步。
const OPENCLAW_WECHAT_TARGET: &str = "REDACTED_WECHAT_TARGET";
/// pushplus 后端超时（同步等应答，code=200 才算送达）。
const PUSHPLUS_TIMEOUT_SECS: u64 = 15;
/// webhook 后端超时。
const WEBHOOK_TIMEOUT_SECS: u64 = 15;
/// 进日志的通知正文预览长度（防 Actions 日志爆量）。
const LOG_PREVIEW_CHARS: usize = 500;
/// 发给 agent 的正文上限（指令本身也要占 token）。
const OPENCLAW_DETAIL_CHARS: usize = 1200;
/// 发给通用 webhook 的正文上限。
const WEBHOOK_DETAIL_CHARS: usize = 2000;
/// 发给 pushplus 的正文上限（免费层消息长度有限，截断保标题完整）。
const PUSHPLUS_DETAIL_CHARS: usize = 2000;
/// pushplus 官方 API（公网直发，不经过 CF 边缘——GHA 出口 IP 不会被 challenge）。
const PUSHPLUS_API: &str = "https://www.pushplus.plus/send/";

/// 发送通知。任何错误只记日志，绝不向上传播。
///
/// 顺序：openclaw → pushplus → webhook。第一个"已送达"的即停——
/// 兜底通道全都能发就重复刷屏，比漏发更烦人。
pub fn send(cfg: &NotifyConfig, title: &str, detail: &str) {
    tracing::warn!(
        "[notify] {title} | {}",
        truncate_chars(detail, LOG_PREVIEW_CHARS)
    );

    let mut delivered = false;
    // openclaw 后端可被 NOTIFY_OPENCLAW_ENABLED=false 停用（Secrets 控制，默认开）：
    // 网关链路长期 403 时停用，避免每次告警都白白消耗一次 pushplus 兜底名额。
    let openclaw_enabled = std::env::var("NOTIFY_OPENCLAW_ENABLED")
        .ok()
        .map(|v| !v.trim().is_empty() && v.trim().to_ascii_lowercase() != "false")
        .unwrap_or(true);
    if let Some(oc) = &cfg.openclaw {
        if !openclaw_enabled {
            tracing::warn!("[notify] openclaw 后端已停用（NOTIFY_OPENCLAW_ENABLED=false），直接走 pushplus 兜底");
        } else {
            delivered = send_openclaw(oc, title, detail);
        }
    }
    if !delivered && !cfg.pushplus_token.trim().is_empty() {
        if openclaw_enabled && cfg.openclaw.is_some() {
            tracing::warn!("[notify] 改用 pushplus 兜底投递");
        }
        delivered = send_pushplus(&cfg.pushplus_token, title, detail);
    }
    if delivered {
        return;
    }
    if cfg.webhook_url.is_empty() {
        if cfg.openclaw.is_some() || !cfg.pushplus_token.trim().is_empty() {
            tracing::error!(
                "[notify] openclaw/pushplus 未送达且没有 webhook 兜底——这条通知发不出去"
            );
        }
        return;
    }
    if cfg.openclaw.is_some() || !cfg.pushplus_token.trim().is_empty() {
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

    // 2026-09-29 改为 POST /tools/invoke 直调 message 工具（网关文档
    // docs.openclaw.ai/gateway/tools-invoke-http-api）：
    //   * 旧路径 /v1/chat/completions 让 agent 自己决定调不调工具——实测弱模型
    //     经常只回文本指导不执行，且 agent 全程 60-90s、客户端一断就中止；
    //   * 工具直调不经 LLM：秒回、必达、不烧 token，响应体带 deliveryStatus=sent。
    //   * 鉴权走现有隧道链（GHA ssh -L → nginx basic → 网关 trusted-proxy 身份头）。
    let text = format!(
        "【{title}】\n{detail}",
        title = title,
        detail = truncate_chars(detail, OPENCLAW_DETAIL_CHARS),
    );
    let payload = json!({
        "tool": "message",
        "action": "send",
        "args": {
            "channel": "openclaw-weixin",
            "target": OPENCLAW_WECHAT_TARGET,
            "text": text,
        },
        "sessionKey": "main",
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
            let body = resp.text().unwrap_or_default();
            if status.is_success() && body.contains("\"ok\":true") {
                tracing::info!("[notify] openclaw 工具直调送达（/tools/invoke deliveryStatus=sent）");
                true
            } else {
                // 工具直调是同步语义：401/403=鉴权或策略拒、400=参数错、404=工具未放行、
                // 5xx=执行错——没有"agent 可能已接单"的模糊地带，一律未送达走兜底。
                tracing::error!(
                    "[notify] openclaw /tools/invoke 未送达 HTTP {status}: {}（转 pushplus 兜底）",
                    body.chars().take(200).collect::<String>()
                );
                false
            }
        }
        Err(e) => {
            // 同步语义：没拿到响应就没有送达凭证，交兜底通道而不是假装成功
            tracing::error!("[notify] openclaw 工具直调失败 {e}（转 pushplus 兜底）");
            false
        }
    }
}

/// PushPlus 后端：同步等应答，HTTP 2xx 且响应 `code == 200` 才算送达。
/// 返回 true = 已交给 pushplus（不再走 webhook，防重复告警）。
fn send_pushplus(token: &str, title: &str, detail: &str) -> bool {
    let content = truncate_chars(detail, PUSHPLUS_DETAIL_CHARS)
        // pushplus 微信模板默认 html：\n 不换行，必须转 <br/>（与 site-watchdog 同样的手法）
        .replace('\n', "<br/>");
    let payload = json!({
        "token": token,
        "title": truncate_chars(title, 50),
        "content": content,
        "template": "html",
    });
    let Some(client) = http_client(PUSHPLUS_TIMEOUT_SECS) else {
        return false;
    };
    match client.post(PUSHPLUS_API).json(&payload).send() {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                tracing::error!(
                    "[notify] pushplus HTTP {status}——通知未送达（token 错误或网络问题）"
                );
                return false;
            }
            // 官方应答 {"code":200,"message":"success"}；code=900 = 账号使用受限（超每日限额）
            match resp.json::<serde_json::Value>() {
                Ok(body) => {
                    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
                    if code == 200 {
                        tracing::info!("[notify] pushplus 已接单（code=200，送达以微信为准）");
                        true
                    } else {
                        let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                        tracing::error!(
                            "[notify] pushplus 拒绝 code={code} ({message})——\
                             900=超免费层每日 200 条限额或账号受限，通知未送达"
                        );
                        false
                    }
                }
                Err(e) => {
                    tracing::error!("[notify] pushplus 应答解析失败 {e}——通知未送达");
                    false
                }
            }
        }
        Err(e) => {
            tracing::error!("[notify] pushplus 投递失败 {e}（忽略，不影响主流程）");
            false
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
