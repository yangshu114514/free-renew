//! 博客园发文：走平台官方的 MetaWeblog API（XML-RPC over HTTP）。
//!
//! 为什么选它而不是模拟浏览器：MetaWeblog 是博客园**对外承诺的发布接口**
//! （Windows Live Writer / MWeb 这类写作软件都靠它），不是逆向出来的 DOM
//! 选择器——平台改版不会让它失效，真要下线也会先公告。认证只要「登录用户名
//! 与 MetaWeblog 访问令牌」两样：没有签名、没有 Cookie、不需要浏览器，
//! 也就没有"网站一改版全废"这类脆弱面。
//!
//! （上一段刻意不用 `+` / `-` / `*` 开头的行：那些会被 Markdown 解析成列表项，
//! 而列表续行的缩进要求会让 `cargo clippy -D warnings` 的 doc lint 直接报错。）
//!
//! 协议（XML-RPC，单次 POST 到 `https://rpc.cnblogs.com/metaweblog/<博客子域名>`）：
//!   `metaWeblog.newPost(blogid, username, password, struct, publish)`
//! struct 里放 title / description(HTML 正文) / mt_keywords(标签) / categories。
//! 返回文章 id（字符串），公开地址 = `https://www.cnblogs.com/<博客子域名>/p/<id>.html`
//!
//! 开关在博客园「账户中心 → 博客设置 → 其他设置 → 允许 MetaWeblog 博客客户端访问」，
//! 打开后才能拿到「MetaWeblog 访问令牌」；未开启时接口返回 fault（见 parse_fault）。

use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;

use crate::config::CnblogsConfig;

/// XML-RPC 端点前缀（后面拼博客子域名）。
const RPC_BASE: &str = "https://rpc.cnblogs.com/metaweblog";
/// 公开文章地址前缀。
const POST_BASE: &str = "https://www.cnblogs.com";

pub struct CnblogsClient {
    http: Client,
    endpoint: String,
    /// XML-RPC 的 username 参数（登录用户名）
    username: String,
    /// MetaWeblog 访问令牌（**不是**登录密码）
    token: String,
    /// 博客子域名，用于拼公开 URL。通常与登录用户名相同，但允许不同。
    blog_user: String,
    tags: Vec<String>,
    categories: Vec<String>,
}

/// XML 文本节点转义。
///
/// 正文是 HTML，`&` 与 `<` 不转义的话整个请求体就不是合法 XML，博客园会直接
/// 回 fault 而错误信息还看不出是转义问题。
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// 组装 `metaWeblog.newPost` 的 XML-RPC 请求体。
///
/// blogid 传空串：博客园按端点里的博客子域名定位博客，不接受别的值。
fn new_post_request(
    title: &str,
    html: &str,
    username: &str,
    token: &str,
    tags: &[String],
    categories: &[String],
    publish_final: bool,
) -> String {
    // 标签用逗号连接（MetaWeblog 的 mt_keywords 约定）
    let keywords = tags.join(",");
    let cats = if categories.is_empty() {
        String::new()
    } else {
        let items: String = categories
            .iter()
            .map(|c| format!("<value><string>{}</string></value>", xml_escape(c)))
            .collect();
        format!(
            "<member><name>categories</name><value><array><data>{items}</data></array></value></member>"
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<methodCall>
<methodName>metaWeblog.newPost</methodName>
<params>
<param><value><string></string></value></param>
<param><value><string>{username}</string></value></param>
<param><value><string>{token}</string></value></param>
<param><value><struct>
<member><name>title</name><value><string>{title}</string></value></member>
<member><name>description</name><value><string>{body}</string></value></member>
<member><name>mt_keywords</name><value><string>{keywords}</string></value></member>
{cats}
</struct></value></param>
<param><value><boolean>{publish}</boolean></value></param>
</params>
</methodCall>"#,
        username = xml_escape(username),
        token = xml_escape(token),
        title = xml_escape(title),
        body = xml_escape(html),
        keywords = xml_escape(&keywords),
        cats = cats,
        publish = if publish_final { 1 } else { 0 },
    )
}

/// 从 fault 响应里抠出可读的错误文本（`<fault>` 内是 struct，含 faultString）。
///
/// 必须先判 fault 再取 id：否则"未开启 MetaWeblog"这类错误文本会被当成文章 id
/// 拼进 URL，报出一个看似成功的假链接。
fn parse_fault(body: &str) -> Option<String> {
    if !body.contains("<fault>") {
        return None;
    }
    let marker = "<name>faultString</name>";
    if let Some(i) = body.find(marker) {
        let rest = &body[i + marker.len()..];
        if let (Some(s), Some(e)) = (rest.find("<string>"), rest.find("</string>")) {
            if e > s {
                return Some(rest[s + "<string>".len()..e].trim().to_string());
            }
        }
    }
    Some(crate::http::truncate_chars(body, 200))
}

/// 取响应里第一个 `<string>…</string>` 的内容（XML-RPC 的返回值就是纯字符串 id）。
///
/// 不为这一处形状固定的响应拖一个 XML 库进来；但要先判 fault（见 parse_fault）。
fn parse_new_post_response(body: &str) -> Result<String> {
    if let Some(msg) = parse_fault(body) {
        bail!("博客园返回 fault: {msg}");
    }
    let start = body
        .find("<string>")
        .context("博客园响应缺少 <string> 返回值")?
        + "<string>".len();
    let rest = &body[start..];
    let end = rest
        .find("</string>")
        .context("博客园响应 <string> 未闭合")?;
    let id = rest[..end].trim();
    if id.is_empty() {
        bail!("博客园返回了空的文章 id");
    }
    Ok(id.to_string())
}

impl CnblogsClient {
    pub fn new(cfg: &CnblogsConfig) -> Result<Self> {
        let username = cfg.username.trim().to_string();
        let token = cfg.token.trim().to_string();
        if username.is_empty() {
            bail!("博客园用户名未配置（CNBLOGS_USERNAME）");
        }
        if token.is_empty() {
            bail!(
                "博客园 MetaWeblog 访问令牌未配置（CNBLOGS_TOKEN）——在 账户中心→博客设置→\
                 其他设置 打开「允许 MetaWeblog 博客客户端访问」后获取令牌"
            );
        }
        // 博客子域名默认与登录用户名一致；两者不一致时用 CNBLOGS_BLOG_USER 覆盖
        let blog_user = if cfg.blog_user.trim().is_empty() {
            username.clone()
        } else {
            cfg.blog_user.trim().to_string()
        };
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("reqwest 构建失败")?;
        Ok(Self {
            http,
            endpoint: format!("{RPC_BASE}/{blog_user}"),
            username,
            token,
            blog_user,
            tags: cfg.tags.clone(),
            categories: cfg.categories.clone(),
        })
    }

    /// 发布文章（`publish_final=false` 时存为草稿）。返回文章公开 URL。
    pub fn publish(&self, title: &str, html: &str, publish_final: bool) -> Result<String> {
        if title.trim().is_empty() {
            bail!("博客园发文标题为空");
        }
        let body = new_post_request(
            title,
            html,
            &self.username,
            &self.token,
            &self.tags,
            &self.categories,
            publish_final,
        );
        let resp = self
            .http
            .post(&self.endpoint)
            .header("Content-Type", "text/xml; charset=UTF-8")
            .header("User-Agent", crate::http::BROWSER_UA)
            .body(body)
            .send()
            .with_context(|| format!("博客园 XML-RPC 请求失败（端点 {}）", self.endpoint))?;
        // 先看 HTTP 状态再解析 XML：反过来的话，网关返回的 HTML 错误页会以
        // "响应缺少 <string>" 上报，把 4xx/5xx 的真相盖掉（zhihu.rs 同款纪律；
        // http::read 就是这条"先状态后体"纪律的统一实现）
        let (status, text) = crate::http::read(resp)?;
        if !(200..300).contains(&status) {
            bail!(
                "博客园 HTTP {status}: {}",
                crate::http::truncate_chars(&text, 200)
            );
        }
        let id = parse_new_post_response(&text)?;
        Ok(format!("{POST_BASE}/{}/p/{id}.html", self.blog_user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_body() {
        let req = new_post_request("标题", "<p>a & b</p>", "u", "t", &[], &[], true);
        assert!(
            req.contains("&lt;p&gt;a &amp; b&lt;/p&gt;"),
            "正文 HTML 必须整体转义，否则请求体不是合法 XML"
        );
        assert!(!req.contains("<p>a & b</p>"));
    }

    #[test]
    fn request_carries_required_members() {
        let req = new_post_request(
            "标题",
            "<p>x</p>",
            "user1",
            "tok",
            &["云服务器".into()],
            &["技术".into()],
            true,
        );
        assert!(req.contains("<methodName>metaWeblog.newPost</methodName>"));
        for member in ["title", "description", "mt_keywords", "categories"] {
            assert!(
                req.contains(&format!("<name>{member}</name>")),
                "缺 member: {member}"
            );
        }
        assert!(req.contains("<boolean>1</boolean>"), "正式发布应为 true");
        // blogid 必须是空串（博客园按端点定位博客）
        assert!(req.contains("<param><value><string></string></value></param>"));
    }

    #[test]
    fn draft_flag_is_zero() {
        let req = new_post_request("t", "b", "u", "p", &[], &[], false);
        assert!(req.contains("<boolean>0</boolean>"));
    }

    #[test]
    fn parses_article_id() {
        let ok = r#"<?xml version="1.0"?><methodResponse><params><param><value><string>12345678</string></value></param></params></methodResponse>"#;
        assert_eq!(parse_new_post_response(ok).unwrap(), "12345678");
    }

    #[test]
    fn fault_is_not_mistaken_for_id() {
        let fault = r#"<?xml version="1.0"?><methodResponse><fault><value><struct><member><name>faultString</name><value><string>未开启MetaWeblog访问</string></value></member></struct></value></fault></methodResponse>"#;
        let err = parse_new_post_response(fault).unwrap_err().to_string();
        assert!(
            err.contains("未开启MetaWeblog访问"),
            "错误信息应透出 fault 原文: {err}"
        );
    }

    #[test]
    fn empty_id_is_rejected() {
        let body = r#"<?xml version="1.0"?><methodResponse><params><param><value><string></string></value></param></params></methodResponse>"#;
        assert!(parse_new_post_response(body).is_err());
    }
}
