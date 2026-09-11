//! 通知：失败/成功投递到用户。绝不 panic——通知是尽力而为，不能反过来弄死主流程。
//!
//! 双后端：
//! 1. `openclaw`（推荐）：POST 网关 chatCompletions → agent → 微信消息工具。
//!    fire-and-forget 语义：agent 在服务端异步执行，客户端超时/5xx 都不影响送达
//!    （实测网关侧 100s 超时后 agent 仍完成投递）。
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
        detail = detail.chars().take(1200).collect::<String>(),
    );
    let _ = cfg.tag;
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
            if status.is_success() {
                tracing::info!("[notify] openclaw 后端 HTTP {status}（agent 异步执行，送达以微信为准）");
            } else if status.as_u16() == 401 {
                // 401 = 网关在 basic auth 层就拒了，agent 没接单，这条告警没送达。
                // 别用"异步执行"话术掩盖——明确报错，用户必须修 Secrets 凭据
                tracing::error!(
                    "[notify] openclaw 网关 401：NOTIFY_OPENCLAW_USER/PASSWORD 与网关 basic auth 不符，\
                     通知未送达（去仓库 Settings→Secrets 核对，网关侧查 htpasswd）"
                );
            } else {
                // 4xx/5xx 超时类：agent 可能已接单（fire-and-forget），只记日志
                tracing::warn!("[notify] openclaw 后端 HTTP {status}（agent 可能已接单，送达以微信为准）");
            }
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
