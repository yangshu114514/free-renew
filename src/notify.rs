//! 通知：失败/成功投递到用户。绝不 panic——通知是尽力而为，不能反过来弄死主流程。
//!
//! 双后端：
//! 1. `openclaw`（推荐）：POST 网关 chatCompletions → agent → 微信消息工具。
//!    fire-and-forget 语义：agent 在服务端异步执行，客户端超时/524 都不影响送达
//!    （实测 CF 100s 超时后 agent 仍完成投递）。
//! 2. `webhook`：通用 JSON POST（{"tag","title","detail"}），备用。
//!
//! 投递指令模板让 agent「立即用微信消息工具发送」，与心跳的"自主判断是否打扰"
//! 哲学不同——告警/回执是明确指令，不需要它判断值不值得。

use base64::Engine;
use serde_json::json;

use crate::config::NotifyConfig;

/// openclaw 后端超时：够发出请求即可，agent 在服务端异步跑完
const OPENCLAW_TIMEOUT_SECS: u64 = 30;

/// 发送通知。任何错误只记日志，绝不向上传播。
pub fn send(cfg: &NotifyConfig, title: &str, detail: &str) {
    tracing::warn!("[notify] {title} | {}", detail.chars().take(500).collect::<String>());

    if let Some(oc) = &cfg.openclaw {
        send_openclaw(cfg, oc, title, detail);
        return;
    }
    send_webhook(cfg, title, detail);
}

fn send_openclaw(cfg: &NotifyConfig, oc: &crate::config::OpenClawNotify, title: &str, detail: &str) {
    // 指令化消息体：明确要求用微信消息工具投递，避免 agent 自作判断吞掉告警
    let instruction = format!(
        "自动化系统通知（tag={},请立刻用微信消息工具把以下内容原样发送到我的微信,发送后只回复:已发送）:\n【{}】\n{}",
        cfg.tag,
        title,
        detail.chars().take(1500).collect::<String>()
    );
    let payload = json!({
        "model": oc.model,
        "messages": [{"role": "user", "content": instruction}],
        "max_tokens": 300,
    });

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(OPENCLAW_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("通知客户端构建失败 {e}（忽略，不影响主流程）");
            return;
        }
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
            // 2xx 或网关侧超时(4xx/5xx 但 agent 可能已接单)都算"已投出"，只记日志不区分
            tracing::info!("[notify] openclaw 后端 HTTP {status}（agent 异步执行，送达以微信为准）");
        }
        Err(e) => tracing::error!("[notify] openclaw 投递失败 {e}（忽略；若为超时，agent 可能仍在执行）"),
    }
}

fn send_webhook(cfg: &NotifyConfig, title: &str, detail: &str) {
    if cfg.webhook_url.is_empty() {
        return;
    }
    let payload = json!({
        "tag": cfg.tag,
        "title": title,
        "detail": detail.chars().take(2000).collect::<String>(),
    });
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("通知客户端构建失败 {e}（忽略，不影响主流程）");
            return;
        }
    };
    match client.post(&cfg.webhook_url).json(&payload).send() {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            tracing::error!("通知投递失败 HTTP {}（忽略，不影响主流程）", resp.status())
        }
        Err(e) => tracing::error!("通知投递失败 {e}（忽略，不影响主流程）"),
    }
}
