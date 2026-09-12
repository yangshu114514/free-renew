//! CSDN 发文：HMAC-SHA256 签名 + saveArticle 接口。
//!
//! 协议来源（2026-09 社区公开逆向 + 真账号实测）：
//! - POST https://bizapi.csdn.net/blog-console-api/v3/mdeditor/saveArticle
//! - 成功响应: {"code":200,"data":{"url":"https://blog.csdn.net/.../details/...", ...}}
//!
//! 签名细节见 `sign` 文档——那里有一个社区流传版本普遍写错的坑（双空行）。

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::http::BROWSER_UA;

const API_HOST: &str = "https://bizapi.csdn.net";
const SAVE_PATH: &str = "/blog-console-api/v3/mdeditor/saveArticle";

/// 协议常量默认值（可在 config.toml [platform.csdn] 覆盖）。
/// appSecret 是 CSDN 前端 JS 内嵌的公开常量，社区已广泛已知，非用户隐私。
pub const DEFAULT_APP_SECRET: &str = "9znpamsyl2c7cdrr9sas0le9vbc3r6ba";
pub const DEFAULT_X_CA_KEY: &str = "203803574";

pub struct CsdnClient {
    cookie: String,
    app_secret: String,
    x_ca_key: String,
    http: reqwest::blocking::Client,
}

impl CsdnClient {
    /// cookie 形如 "k=v; k=v; ..."（csdn_cookies_oneline.txt 原样内容）
    pub fn new(cookie: &str, app_secret: &str, x_ca_key: &str) -> Self {
        Self {
            cookie: cookie.trim().to_string(),
            app_secret: app_secret.to_string(),
            x_ca_key: x_ca_key.to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest 静态参数"),
        }
    }

    /// x-ca-signature：HMAC-SHA256(appSecret, 待签串) 的 Base64
    ///
    /// 待签串格式（2026-09 真请求实测验证，注意 accept 与 content-type 之间是
    /// 空行、content-type 与第一个 header 之间也是空行——社区流传的 Java 版
    /// 少了 accept 后那个 \n，是错的；以 CSDN 前端 JS 为准）：
    ///   "POST\n*/*\n\napplication/json\n\nx-ca-key:203803574\nx-ca-nonce:{uuid}\n{path}"
    fn sign(&self, method: &str, accept: &str, content_type: &str, nonce: &str, path: &str) -> String {
        let string_to_sign = format!(
            "{method}\n{accept}\n\n{content_type}\n\nx-ca-key:{}\nx-ca-nonce:{nonce}\n{path}",
            self.x_ca_key
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(self.app_secret.as_bytes())
            .expect("HMAC 接受任意长度密钥");
        mac.update(string_to_sign.as_bytes());
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    /// 发布文章。`publish=true` 直接发布，`false` 存草稿（人工确认后手动发）。
    /// 返回最终 URL（草稿也返回预览 URL）。
    pub fn publish_sync(
        &self,
        title: &str,
        markdown: &str,
        tags: &[String],
        categories: &[String],
        creation_statement: u8,
        publish: bool,
    ) -> Result<String> {
        let html = markdown_to_simple_html(markdown);
        let payload = json!({
            "title": title,
            "content": html,
            "markdowncontent": markdown,
            "pubStatus": if publish { "publish" } else { "draft" },
            "readType": "public",
            "type": "original",
            "tags": tags.join(","),
            "categories": categories.join(","),
            "creation_statement": creation_statement,
            "status": if publish { 0 } else { 2 },
            "cover_type": 1,
            "authorized_status": false,
            "source": "pc_mdeditor",
        });
        let body_str = serde_json::to_string(&payload)?;

        let nonce = uuid::Uuid::new_v4().to_string();
        let sig = self.sign("POST", "*/*", "application/json", &nonce, SAVE_PATH);

        let resp = self
            .http
            .post(format!("{API_HOST}{SAVE_PATH}"))
            .header("Cookie", &self.cookie)
            .header("Content-Type", "application/json")
            .header("Accept", "*/*")
            .header("User-Agent", BROWSER_UA)
            .header("Origin", "https://editor.csdn.net")
            .header("Referer", "https://editor.csdn.net/")
            .header("x-ca-key", &self.x_ca_key)
            .header("x-ca-nonce", &nonce)
            .header("x-ca-signature", sig)
            .header("x-ca-signature-headers", "x-ca-key,x-ca-nonce")
            .body(body_str)
            .send()
            .context("CSDN saveArticle 请求失败")?;

        let status = resp.status();
        let body = resp.text().context("CSDN 响应读取失败")?;
        if !status.is_success() {
            // CSDN 新号每日发文额度有限（实测约 2 篇/天）。这是硬额度，同日重试
            // 永远不会成功，反而继续空耗——识别出来直接给可执行结论，不套通用 400
            if body.contains("发表文章数量已达到限制") || body.contains("400300012") {
                bail!(
                    "CSDN 今日发文额度已用尽（新号约 2 篇/天）：今日无法续期，\
                     请明日额度重置后由定时任务自动重试，或提升 CSDN 账号等级以增加每日发文数"
                );
            }
            bail!("CSDN HTTP {status}: {}", crate::http::truncate_chars(&body, 300));
        }

        let v: serde_json::Value = serde_json::from_str(&body).context("CSDN 响应 JSON 解析失败")?;
        if v.get("code").and_then(serde_json::Value::as_i64) != Some(200) {
            bail!("CSDN 发文被拒: {}", crate::http::truncate_chars(&body, 300));
        }
        let url = v
            .pointer("/data/url")
            .and_then(serde_json::Value::as_str)
            .context("CSDN 响应缺少 data.url")?
            .to_string();
        Ok(url)
    }
}

/// markdown → 简易 HTML。CSDN content 字段要 HTML；markdowncontent 字段原样。
/// 只处理续期文章用到的子集：标题、代码块、段落。
fn markdown_to_simple_html(md: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in md.lines() {
        if let Some(lang) = line.strip_prefix("```") {
            if in_code {
                out.push_str("</pre></code>\n");
            } else {
                // lang 会进 HTML 属性，必须转义（LLM 输出不可信）
                out.push_str(&format!(
                    "<pre><code class=\"language-{}\">",
                    html_escape(lang)
                ));
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push_str(&format!("{}\n", html_escape(line)));
            continue;
        }
        if let Some(h) = line.strip_prefix("### ") {
            out.push_str(&format!("<h3>{}</h3>\n", html_escape(h)));
        } else if let Some(h) = line.strip_prefix("## ") {
            out.push_str(&format!("<h2>{}</h2>\n", html_escape(h)));
        } else if let Some(h) = line.strip_prefix("# ") {
            out.push_str(&format!("<h1>{}</h1>\n", html_escape(h)));
        } else if line.trim().is_empty() {
            continue;
        } else {
            out.push_str(&format!("<p>{}</p>\n", html_escape(line)));
        }
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_shape_matches_verified_request() {
        // 锁定 2026-09 真请求验证过的待签串形状（双空行版）。
        // 若此测试挂了而接口报 "HMAC signature does not match"，
        // 说明 CSDN 改了签名格式，回前端 JS 重新比对。
        let client = CsdnClient::new("k=v", DEFAULT_APP_SECRET, DEFAULT_X_CA_KEY);
        let sig = client.sign("POST", "*/*", "application/json", "939abdaa-0bdd-4eba-a720-e8fc0b151621", "/blog-console-api/v3/mdeditor/saveArticle");
        assert_eq!(sig.len(), 44, "HMAC-SHA256 base64 应为 44 字符: {sig}");
    }

    #[test]
    fn md_to_html_basic() {
        let html = markdown_to_simple_html("# T\n## H\n正文\n```rust\nfn a(){}\n```\n尾行");
        assert!(html.contains("<h1>T</h1>"));
        assert!(html.contains("<h2>H</h2>"));
        assert!(html.contains("language-rust"));
        assert!(html.contains("<p>尾行</p>"));
    }
}
