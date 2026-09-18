//! 知乎发文：移植 zimya/zhihu_obsidian 的纯 HTTP 链路（大佬造好的轮子，不逆向 x-zse-96）。
//!
//! 实测这些专栏写接口只靠 Cookie + x-xsrftoken + x-requested-with:fetch 即可，
//! 无需前端签名。四步：
//!   1. POST zhuanlan.zhihu.com/api/articles/drafts           → 拿 draft id
//!   2. PATCH zhuanlan.zhihu.com/api/articles/{id}/draft      → 写标题+正文(HTML)
//!   3. GET  autocomplete/topics → POST /api/articles/{id}/topics → 挂话题(不挂通常发不出)
//!   4. POST www.zhihu.com/api/v4/content/publish            → 发布，URL=/p/{id}
//!
//! 每一步各自一个函数：四步的成功判定与失败语义都不同（建草稿要 id、写正文要 2xx、
//! 挂话题允许部分失败、发布要 message=success），揉在一个百行函数里时，正是"某一步
//! 漏了状态码校验"这类不一致的温床。
//!
//! 注意：从数据中心 IP 发请求本身有知乎风控风险（异地/异常环境画像）。本模块
//! 命中 403/验证码即**明确报错、不重试猛戳**——反复撞风控才会真把账号搞封。

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

use crate::config::ZhihuConfig;

const ZHUANLAN: &str = "https://zhuanlan.zhihu.com";
/// 知乎单篇文章可挂的话题上限。
const MAX_TOPICS: usize = 3;
/// 写正文接口的 delta_time（协议魔数，实测可用的固定值）。
const DELTA_TIME_SECS: u64 = 5;
/// 话题补全接口的固定查询参数。
const TOPIC_AUTOCOMPLETE_QUERY: &str = "max_matches=5&use_similar=0&topic_filter=1";

pub struct ZhihuClient {
    http: Client,
    cookie: String,
    xsrf: String,
}

/// 从单行 cookie 里取某项的值（URL 解码 _xsrf 用）。
fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    cookie.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// 极简 percent-decode（够用：_xsrf 可能含编码字符）。
///
/// 用 `s.get()` 而不是 `&s[i+1..i+3]` 字节切片：输入里若出现 `%` 后紧跟多字节
/// UTF-8（如 `%中`），字节切片会切在字符边界中间直接 panic。
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(hex) = s.get(i + 1..i + 3) {
                if let Ok(n) = u8::from_str_radix(hex, 16) {
                    out.push(n);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 统一的状态码校验：**先看 HTTP 状态，再解析 JSON**。
/// 反过来的话，风控返回的 HTML 错误体会以"响应非 JSON"上报，把 4xx/5xx 的真相盖掉。
fn ensure_2xx(status: u16, body: &str, what: &str) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    bail!(
        "知乎{what} HTTP {status}: {}",
        crate::http::truncate_chars(body, 200)
    )
}

impl ZhihuClient {
    pub fn new(cfg: &ZhihuConfig) -> Result<Self> {
        let raw = cookie_value(&cfg.cookie, "_xsrf").context(
            "知乎 Cookie 缺 _xsrf（发文必需）。登录 zhihu.com→F12→Application→Cookies 复制含 _xsrf/z_c0 的完整串",
        )?;
        let xsrf = pct_decode(&raw);
        if !cfg.cookie.contains("z_c0") {
            bail!("知乎 Cookie 缺 z_c0（登录态命脉），请重新复制完整 Cookie");
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .context("reqwest 构建失败")?;
        Ok(Self {
            http,
            cookie: cfg.cookie.clone(),
            xsrf,
        })
    }

    /// 统一带知乎写接口所需头部。
    fn req(&self, method: reqwest::Method, url: &str) -> reqwest::blocking::RequestBuilder {
        self.http
            .request(method, url)
            .header("Cookie", &self.cookie)
            .header("User-Agent", crate::http::BROWSER_UA)
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .header("x-requested-with", "fetch")
            .header("x-xsrftoken", &self.xsrf)
            .header("Origin", ZHUANLAN)
            .header("Referer", format!("{ZHUANLAN}/write"))
    }

    /// 发一个写请求并做统一校验：风控识别 → 状态码 → 响应体交给调用方解析。
    /// 四步共用同一条校验链，不会再出现"某一步漏了状态码检查"。
    fn send_checked(&self, rb: reqwest::blocking::RequestBuilder, what: &str) -> Result<String> {
        let resp = rb.send().with_context(|| format!("知乎{what}请求失败"))?;
        let (status, body) = read(resp)?;
        check_block(status, &body)?;
        ensure_2xx(status, &body, what)?;
        Ok(body)
    }

    /// 步骤 1：建草稿，返回 draft id。
    fn create_draft(&self, title: &str) -> Result<String> {
        let body = self.send_checked(
            self.req(
                reqwest::Method::POST,
                &format!("{ZHUANLAN}/api/articles/drafts"),
            )
            .header("Content-Type", "application/json")
            .json(&json!({"title": title, "delta_time": 0, "can_reward": false})),
            "建草稿",
        )?;
        let id = serde_json::from_str::<Value>(&body)
            .context("知乎建草稿响应非 JSON")?
            .get("id")
            .and_then(Value::as_str)
            .context("建草稿未返回 id")?
            .to_string();
        tracing::info!("知乎草稿已建: id={id}");
        Ok(id)
    }

    /// 步骤 2：写标题与正文（HTML）。
    fn write_body(&self, id: &str, title: &str, html: &str, toc: bool) -> Result<()> {
        self.send_checked(
            self.req(
                reqwest::Method::PATCH,
                &format!("{ZHUANLAN}/api/articles/{id}/draft"),
            )
            .header("Content-Type", "application/json")
            .json(&json!({
                "title": title,
                "content": html,
                "table_of_contents": toc,
                "delta_time": DELTA_TIME_SECS,
                "can_reward": false,
            })),
            "写正文",
        )?;
        Ok(())
    }

    /// 步骤 3：挂话题（不挂通常发不出去）。逐个候选挂上，最多 3 个（知乎上限），
    /// 按解析到的真实话题名去重，失败不致命但每个都留日志；全部挂不上则报错。
    fn attach_topics(&self, id: &str, topics: &[String]) -> Result<()> {
        let mut attached: Vec<String> = vec![];
        for topic in topics.iter().take(MAX_TOPICS) {
            match self.attach_topic(id, topic, &attached) {
                Ok(Some(name)) => attached.push(name),
                Ok(None) => tracing::warn!("话题“{topic}”无安全匹配或已重复，跳过"),
                Err(e) => tracing::warn!("挂话题“{topic}”失败（继续尝试发布）: {e}"),
            }
        }
        if attached.is_empty() {
            bail!("所有候选话题都没挂上，知乎发布通常会被拒——请调 ZHIHU_TOPICS/[platform.zhihu].topics");
        }
        Ok(())
    }

    /// 步骤 4：正式发布，返回公开 URL。
    fn publish_public(&self, id: &str, toc: bool) -> Result<String> {
        let trace = format!("{},{}", now_millis(), uuid::Uuid::new_v4());
        let biz = json!({
            "column": null,
            "commentPermission": "anyone",
            "table_of_contents_enabled": toc,
            "commercial_report_info": {"commercial_types": []},
            "canReward": false,
        })
        .to_string();
        let body = self.send_checked(
            self.req(
                reqwest::Method::POST,
                "https://www.zhihu.com/api/v4/content/publish",
            )
            .header("Content-Type", "application/json")
            .json(&json!({
                "action": "article",
                "data": {
                    "publish": {"traceId": trace},
                    "extra_info": {"publisher": "pc", "pc_business_params": biz},
                    "draft": {"disabled": 1, "id": id, "isPublished": false},
                    "commentsPermission": {"comment_permission": "anyone"},
                }
            })),
            "发布",
        )?;
        let v: Value = serde_json::from_str(&body).context("知乎发布响应非 JSON")?;
        let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
        // 宽松匹配：知乎历史上返回过带空白/大小写变体的成功字，严格相等会把一次
        // 成功的发布误报成失败，下一轮又发一篇（等于往账号上多灌一篇重复内容）
        if !msg.trim().eq_ignore_ascii_case("success") {
            bail!(
                "知乎发布未成功 message={msg:?}: {}",
                crate::http::truncate_chars(&body, 240)
            );
        }
        let url = format!("{ZHUANLAN}/p/{id}");
        tracing::info!("知乎发布成功: {url}");
        Ok(url)
    }

    /// 发布知乎专栏文章，返回公开 URL。`html` 为正文 HTML。
    /// `publish_final=false` 时只建草稿+写正文+挂话题，停在发布前，返回草稿编辑链接
    /// （用于低风控代价的连通性探路：验证鉴权与接口形状，不真公开贴文）。
    pub fn publish(
        &self,
        title: &str,
        html: &str,
        topics: &[String],
        toc: bool,
        publish_final: bool,
    ) -> Result<String> {
        let id = self.create_draft(title)?;
        self.write_body(&id, title, html, toc)?;
        self.attach_topics(&id, topics)?;

        if !publish_final {
            let edit = format!("{ZHUANLAN}/p/{id}/edit");
            tracing::info!("知乎草稿探路完成（未发布）: {edit}");
            return Ok(edit);
        }
        self.publish_public(&id, toc)
    }

    /// 挂话题。成功返回 Some(知乎侧真实话题名)；无安全匹配返回 None；网络/HTTP 错误 Err。
    /// 已挂在 `attached` 里的话题名会被跳过（返回 None）。
    fn attach_topic(&self, id: &str, topic: &str, attached: &[String]) -> Result<Option<String>> {
        let q = urlencode(topic);
        let body = self.send_checked(
            self.req(
                reqwest::Method::GET,
                &format!("{ZHUANLAN}/api/autocomplete/topics?token={q}&{TOPIC_AUTOCOMPLETE_QUERY}"),
            ),
            "话题补全",
        )?;
        let arr: Value = serde_json::from_str(&body).context("话题补全响应非 JSON")?;
        let candidates = match arr.as_array() {
            Some(a) => a.clone(),
            None => {
                // 形状变了要留痕：静默当空数组的话，上层只会说"无安全匹配，跳过"，
                // 接口改版时没有任何线索
                tracing::warn!(
                    "话题补全响应不是数组（接口可能改版）: {}",
                    crate::http::truncate_chars(&body, 160)
                );
                return Ok(None);
            }
        };
        let chosen = candidates
            .iter()
            .filter_map(|c| {
                let name = c.get("name").and_then(Value::as_str)?;
                let score = topic_score(topic, name)?;
                Some((score, c.clone(), name.to_string()))
            })
            .min_by_key(|(score, _, name)| (*score, name.chars().count()))
            .filter(|(_, _, name)| !attached.contains(name));
        let (_, payload, name) = match chosen {
            Some(x) => x,
            None => return Ok(None),
        };
        self.send_checked(
            self.req(
                reqwest::Method::POST,
                &format!("{ZHUANLAN}/api/articles/{id}/topics"),
            )
            .header("Content-Type", "application/json")
            .json(&payload),
            "绑定话题",
        )?;
        tracing::info!("话题“{topic}”→ 知乎话题“{name}”");
        Ok(Some(name))
    }
}

/// 候选话题名是否可用及优先级（数字小=优先）。None=直接排除。
fn topic_score(want: &str, name: &str) -> Option<usize> {
    const BRANDS: &[&str] = &[
        "阿里",
        "腾讯",
        "华为",
        "京东",
        "天翼",
        "移动",
        "亚马逊",
        "AWS",
        "百度",
        "电信",
        "联通",
    ];
    if BRANDS.iter().any(|b| name.contains(b)) {
        return None;
    }
    if name == want {
        Some(0)
    } else if name.starts_with(want) || want.starts_with(name) {
        Some(1)
    } else if name.contains(want) {
        Some(2)
    } else {
        None
    }
}

fn read(resp: reqwest::blocking::Response) -> Result<(u16, String)> {
    let status = resp.status().as_u16();
    let body = resp.text().context("读取响应体失败")?;
    Ok((status, body))
}

/// 风控/验证码识别：命中即硬错误（绝不重试猛戳）。
///
/// 只认**确定**的验证标识。曾用裸子串 `"verify"`，但知乎正常响应里本就带
/// `is_verified` / `verify_status` 一类字段——一个合法字段就能把整轮发文打断。
/// 假阳性比漏报更贵：漏报还有 401/403 与鉴权失败兜底，假阳性直接让当轮续期作废。
fn check_block(status: u16, body: &str) -> Result<()> {
    if status == 403 || status == 401 {
        bail!(
            "知乎鉴权/风控 HTTP {status}：登录 Cookie 可能失效或触发风控。请重新复制最新 Cookie，\
             并留意账号是否要求验证。已停止，不重试以免加重风控。响应: {}",
            crate::http::truncate_chars(body, 200)
        );
    }
    if body.contains("验证码") || body.contains("captcha") || body.contains("unhuman") {
        bail!(
            "知乎要求人工验证（captcha）：自动发布已停止，需人工在浏览器完成验证。响应: {}",
            crate::http::truncate_chars(body, 200)
        );
    }
    Ok(())
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 极简 query 编码（话题名可能含中文/空格）
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_score_prefers_exact_and_rejects_brands() {
        // 实测候选：腾讯云服务器/华为云服务器/三丰云服务器/免费云服务器
        assert_eq!(
            topic_score("云服务器", "腾讯云服务器"),
            None,
            "品牌名必须排除"
        );
        assert_eq!(topic_score("云服务器", "华为云服务器"), None);
        assert_eq!(topic_score("云服务器", "某大厂服务器"), None);
        assert_eq!(
            topic_score("免费云服务器", "免费云服务器"),
            Some(0),
            "完全相等最优"
        );
        assert_eq!(
            topic_score("免费云服务器", "免费云服务器推荐"),
            Some(1),
            "前缀匹配次优"
        );
        assert_eq!(
            topic_score("免费云服务器", "三丰云服务器"),
            None,
            "不含目标词则排除"
        );
        assert_eq!(topic_score("虚拟主机", "虚拟主机"), Some(0));
        assert_eq!(topic_score("虚拟主机", "不相关内容"), None);
    }

    #[test]
    fn cookie_value_and_xsrf() {
        let c = "d_c0=abc; _xsrf%3Dabc%3Ddef; z_c0=\"2|1:0\"; _xsrf=ttt%2Bxxx";
        assert_eq!(cookie_value(c, "z_c0").as_deref(), Some("\"2|1:0\""));
        assert_eq!(cookie_value(c, "_xsrf").as_deref(), Some("ttt%2Bxxx"));
        assert_eq!(pct_decode("ttt%2Bxxx"), "ttt+xxx");
        assert_eq!(cookie_value(c, "nope"), None);
    }

    #[test]
    fn pct_decode_survives_multibyte_input() {
        // `%` 后紧跟多字节 UTF-8：字节切片版会 panic，get() 版原样保留
        assert_eq!(pct_decode("%中"), "%中");
        assert_eq!(pct_decode("a%2"), "a%2");
        assert_eq!(pct_decode("%zz"), "%zz");
        assert_eq!(pct_decode("%41中"), "A中");
    }

    #[test]
    fn ensure_2xx_rejects_error_status() {
        assert!(ensure_2xx(200, "{}", "建草稿").is_ok());
        assert!(ensure_2xx(201, "{}", "建草稿").is_ok());
        let e = format!(
            "{:#}",
            ensure_2xx(503, "<html>busy</html>", "建草稿").unwrap_err()
        );
        assert!(e.contains("503") && e.contains("建草稿"), "{e}");
    }

    #[test]
    fn check_block_detects_captcha() {
        assert!(check_block(200, "{\"err\":\"请通过验证码\"}").is_err());
        assert!(check_block(403, "forbidden").is_err());
        assert!(check_block(200, "{\"captcha\":{\"token\":\"x\"}}").is_err());
        assert!(check_block(200, "ok").is_ok());
        // 正常响应里的 verified 字段不得被当成验证挑战（假阳性会废掉整轮发文）
        assert!(check_block(200, "{\"author\":{\"is_verified\":true}}").is_ok());
    }
}
