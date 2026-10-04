//! dev.to（Forem）发文渠道。
//!
//! 来源：现成 SDK [`socialsbase/devto-api`](https://github.com/socialsbase/devto-api)（MIT）。
//! 本文件是它的**精简内联版**，对照关系：
//!
//! | 原项目 | 这里 |
//! | --- | --- |
//! | `src/config.rs` / `ClientExt::forem()`：base URL + `api-key` 头 | [`DevtoClient::new`] |
//! | `openapi/api_v1.json` `POST /api/articles`（operationId `createArticle`） | [`DevtoClient::publish`] |
//! | `build.rs` 生成代码 `create_article()`：`Accept: application/json`，201/401/422 | 同左，改 blocking |
//! | 生成类型 `types::Article` / `types::ArticleArticle` | [`CreateArticle`] / [`ArticleBody`] |
//!
//! 砍掉的部分：Progenitor 代码生成层（生成物 8142 行 / 287 KB）、async + Tokio runtime，
//! 以及本渠道用不到的 comments / billboards / pages / organizations 等 60+ 端点。
//! 本项目现有 HTTP 栈是 blocking reqwest（见 `csdn.rs`），沿用它就不必引入第二个 runtime。
//!
//! **凭据**：API key 只经 [`DevtoClient::new`] 的入参流动，本模块不落任何常量；
//! 配置层从 `DEVTO_API_KEY` 环境变量读，`Debug` 输出只给长度不给内容。
//!
//! **许可以及版权（MIT，派生自上游；本仓库整体为 Apache-2.0，二者兼容，详见 NOTICE）**
//! ```
//! Copyright (c) 2025 socialsbase
//! ```

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::http::truncate_chars;

/// dev.to API 基址（原项目常量 `DEVTO_API_URL`）。
pub const API_BASE: &str = "https://dev.to/api";

/// 原项目 `ClientInfo::api_version()` 的取值，作为 `api-version` 请求头发送。
const API_VERSION: &str = "1.0.0";

/// `POST /api/articles` 请求体，对应原项目 `types::Article`。
#[derive(Debug, Serialize)]
struct CreateArticle<'a> {
    article: ArticleBody<'a>,
}

/// 对应原项目 `types::ArticleArticle`，只保留续期链路用得上的字段。
///
/// 原 schema 把 `tags` 声明成 `string`，但 dev.to 实际收 JSON 数组
/// （2026-10-02 真账号 201 实测），这里按实测走。
#[derive(Debug, Serialize)]
struct ArticleBody<'a> {
    title: &'a str,
    body_markdown: &'a str,
    published: bool,
    tags: &'a [String],
}

/// 201 响应：原项目反序列化整个 `types::Article`，这里只取续期要用的字段。
#[derive(Debug, Deserialize)]
struct CreatedArticle {
    id: u64,
    url: String,
}

pub struct DevtoClient {
    api_key: String,
    http: reqwest::blocking::Client,
}

impl DevtoClient {
    /// 凭据只在这里进、不留痕：key 为空直接判失败，不让后续请求带空头出网。
    pub fn new(api_key: &str) -> Result<Self> {
        Ok(Self {
            api_key: api_key.trim().to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .context("构建 dev.to HTTP 客户端失败")?,
        })
    }

    /// 发一篇。`publish=false` = 存草稿（链路测试用，停在公开门槛之前一步）。
    /// 返回文章公开 URL。
    pub fn publish(
        &self,
        title: &str,
        body_markdown: &str,
        tags: &[String],
        publish: bool,
    ) -> Result<String> {
        if self.api_key.is_empty() {
            bail!(
                "dev.to API key 为空——请设 DEVTO_API_KEY（GitHub Actions 用 Secret；\
                 不要写进代码、脚本或 config.toml）"
            );
        }

        let payload = CreateArticle {
            article: ArticleBody {
                title,
                body_markdown,
                published: publish,
                tags,
            },
        };

        let resp = self
            .http
            .post(format!("{API_BASE}/api/articles"))
            .header("api-key", &self.api_key)
            .header("api-version", API_VERSION)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .context("dev.to 发文请求失败")?;

        let (status, body) = crate::http::read(resp)?;

        if !(200..300).contains(&status) {
            // 实测：伪造/失效的 key 返回 **403**（不是 401），两种都归到"凭据问题"
            // 给同一条可执行结论，别让人对着一个裸状态码猜。
            if matches!(status, 401 | 403) {
                bail!(
                    "dev.to 拒绝发文（HTTP {status}）：API key 无效或已吊销，\
                     请到 dev.to/settings/extensions 重新签发并更新 DEVTO_API_KEY"
                );
            }
            bail!("dev.to HTTP {status}: {}", truncate_chars(&body, 300));
        }

        let created: CreatedArticle =
            serde_json::from_str(&body).context("dev.to 响应 JSON 解析失败")?;
        if created.url.trim().is_empty() {
            bail!("dev.to 响应缺少 url（id={}）", created.id);
        }
        Ok(created.url)
    }
}

/// 手写 Debug：key 会随 `{client:?}` 之类的调试输出走，必须遮掉。
impl std::fmt::Debug for DevtoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevtoClient")
            .field("api_key", &crate::http::redact(&self.api_key))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁定 2026-10-02 真账号验证过的请求体形状（`{"article":{...}}` 包裹 + tags 为数组）。
    /// 若此测试挂了而接口报 422，说明 dev.to 改了 requestBody 契约。
    #[test]
    fn create_article_payload_shape_is_verified() {
        let tags = vec!["cloud".to_string(), "vps".to_string()];
        let payload = CreateArticle {
            article: ArticleBody {
                title: "标题",
                body_markdown: "# 正文",
                published: true,
                tags: &tags,
            },
        };
        let v = serde_json::to_value(&payload).expect("序列化请求体");
        assert_eq!(v["article"]["title"], "标题");
        assert_eq!(v["article"]["body_markdown"], "# 正文");
        assert_eq!(v["article"]["published"], true);
        assert_eq!(v["article"]["tags"][0], "cloud");
        // 规范里写的是 string，实测收数组——这条断言是防止有人"照规范修正"回去
        assert!(v["article"]["tags"].is_array());
    }

    #[test]
    fn created_article_parses_201_response() {
        // 形状照 201 响应（type_of/id/title/url），但**值全部占位**：
        // 测试数据里不该出现真实用户名或真实文章 id——它们是可关联到具体
        // 个人与已发布内容的标识符，而本仓库是公开的。
        let body = r#"{"type_of":"article","id":123456,"title":"t",
            "url":"https://dev.to/example-user/sample-article-1a2b"}"#;
        let a: CreatedArticle = serde_json::from_str(body).expect("解析 201 响应");
        assert_eq!(a.id, 123456);
        assert!(a.url.starts_with("https://dev.to/"));
        assert!(
            !a.url.trim_end_matches('/').ends_with("123456"),
            "url 与 id 无强绑定"
        );
    }

    /// key 为空时必须**在出网之前**失败，且提示里要指出该设哪个环境变量。
    #[test]
    fn empty_api_key_fails_before_any_request() {
        let c = DevtoClient::new("   ").expect("客户端构建");
        let e = c.publish("t", "b", &[], true).unwrap_err();
        assert!(e.to_string().contains("DEVTO_API_KEY"), "{e}");
    }

    /// Debug 不能把 key 原文带出去。
    #[test]
    fn debug_never_leaks_api_key() {
        let c = DevtoClient::new("super-secret-key").expect("客户端构建");
        let s = format!("{c:?}");
        assert!(!s.contains("super-secret-key"), "{s}");
        assert!(s.contains("已隐藏"), "{s}");
    }
}
