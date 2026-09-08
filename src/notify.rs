//! 通知：失败/异常时投递到 OpenClaw webhook（微信通道），没配就只打日志。
//!
//! OpenClaw 侧约定：POST JSON {"tag","title","detail"}，由网关侧一个轻量 skill
//! 转发到微信。链路故障时绝不 panic——通知是尽力而为，不能反过来弄死主流程。

use crate::config::NotifyConfig;

/// 发送通知。任何错误只记日志，绝不向上传播。
pub fn send(cfg: &NotifyConfig, title: &str, detail: &str) {
    tracing::warn!("[notify] {title} | {}", detail.chars().take(500).collect::<String>());
    if cfg.webhook_url.is_empty() {
        return;
    }
    let payload = serde_json::json!({
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
