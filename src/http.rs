//! 阻塞式 HTTP 客户端封装（reqwest::blocking）。
//!
//! 说明：整个程序流程是严格串行的（登录→发文→截图→提交，每步依赖上一步），
//! async 没有收益，直接用 blocking API 让错误处理和测试都简单一截。

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub fn client(timeout_secs: u64) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::limited(5))
        .cookie_store(true)
        .build()
        .context("构建 HTTP 客户端失败")
}

pub fn get_json(url: &str, timeout_secs: u64) -> Result<serde_json::Value> {
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .with_context(|| format!("GET {url} 失败"))?;
    let status = resp.status();
    let body = resp.text().context("读取响应失败")?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", &body[..body.len().min(300)]);
    }
    serde_json::from_str(&body).with_context(|| format!("JSON 解析失败: {url}"))
}

pub fn err_ctx<T>(r: std::result::Result<T, reqwest::Error>, what: &str) -> Result<T> {
    r.map_err(|e| anyhow!("{what}: {e}"))
}
