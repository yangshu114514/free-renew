//! 知乎发文：移植 zimya/zhihu_obsidian 的纯 HTTP 链路（大佬造好的轮子，不逆向 x-zse-96）。
//!
//! 实测这些专栏写接口只靠 Cookie + x-xsrftoken + x-requested-with:fetch 即可，
//! 无需前端签名。四步：
//!   1. POST zhuanlan.zhihu.com/api/articles/drafts           → 拿 draft id
//!   2. PATCH zhuanlan.zhihu.com/api/articles/{id}/draft      → 写标题+正文(HTML)
//!   3. GET  autocomplete/topics → POST /api/articles/{id}/topics → 挂话题(不挂通常发不出)
//!   4. POST www.zhihu.com/api/v4/content/publish            → 发布，URL=/p/{id}
//!
//! 注意：从数据中心 IP 发请求本身有知乎风控风险（异地/异常环境画像）。本模块
//! 命中 403/验证码即**明确报错、不重试猛戳**——反复撞风控才会真把账号搞封。

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

use crate::config::ZhihuConfig;

const ZHUANLAN: &str = "https://zhuanlan.zhihu.com";

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

/// 极简 percent-decode（够用：_xsrf 可能含编码字符）
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

    /// 发布知乎专栏文章，返回公开 URL。`html` 为正文 HTML。
    /// `publish_final=false` 时只建草稿+写正文+挂话题，停在发布前，返回草稿编辑链接
    /// （用于低风控代价的连通性探路：验证鉴权与接口形状，不真公开贴文）。
    pub fn publish(&self, title: &str, html: &str, topics: &[String], toc: bool, publish_final: bool) -> Result<String> {
        // 1. 建草稿
        let resp = self
            .req(reqwest::Method::POST, &format!("{ZHUANLAN}/api/articles/drafts"))
            .header("Content-Type", "application/json")
            .json(&json!({"title": title, "delta_time": 0, "can_reward": false}))
            .send()
            .context("知乎建草稿请求失败")?;
        let (status, body) = read(resp)?;
        check_block(status, &body)?;
        let id = serde_json::from_str::<Value>(&body)
            .context("知乎建草稿响应非 JSON")?
            .get("id")
            .and_then(Value::as_str)
            .context("建草稿未返回 id")?
            .to_string();
        tracing::info!("知乎草稿已建: id={id}");

        // 2. 写正文
        let resp = self
            .req(
                reqwest::Method::PATCH,
                &format!("{ZHUANLAN}/api/articles/{id}/draft"),
            )
            .header("Content-Type", "application/json")
            .json(&json!({
                "title": title,
                "content": html,
                "table_of_contents": toc,
                "delta_time": 5,
                "can_reward": false,
            }))
            .send()
            .context("知乎写正文请求失败")?;
        let (status, body) = read(resp)?;
        check_block(status, &body)?;
        if !(200..300).contains(&status) {
            bail!("知乎写正文失败 HTTP {status}: {}", crate::http::truncate_chars(&body, 200));
        }

        // 3. 挂话题（不挂通常发不出去）。逐个候选挂上，最多 3 个（知乎上限），
        //    按解析到的真实话题名去重，失败不致命但每个都留日志。
        {
            let mut attached: Vec<String> = vec![];
            for topic in topics.iter().take(3) {
                match self.attach_topic(&id, topic, &attached) {
                    Ok(Some(name)) => attached.push(name),
                    Ok(None) => tracing::warn!("话题“{topic}”无安全匹配或已重复，跳过"),
                    Err(e) => tracing::warn!("挂话题“{topic}”失败（继续尝试发布）: {e}"),
                }
            }
            if attached.is_empty() {
                bail!("所有候选话题都没挂上，知乎发布通常会被拒——请调 ZHIHU_TOPICS/[platform.zhihu].topics");
            }
        }

        // 3.5 探路模式：停在发布前，返回草稿编辑链接
        if !publish_final {
            let edit = format!("{ZHUANLAN}/p/{id}/edit");
            tracing::info!("知乎草稿探路完成（未发布）: {edit}");
            return Ok(edit);
        }

        // 4. 发布
        let trace = format!("{},{}", now_millis(), uuid::Uuid::new_v4());
        let biz = json!({
            "column": null,
            "commentPermission": "anyone",
            "table_of_contents_enabled": toc,
            "commercial_report_info": {"commercial_types": []},
            "canReward": false,
        })
        .to_string();
        let resp = self
            .req(
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
            }))
            .send()
            .context("知乎发布请求失败")?;
        let (status, body) = read(resp)?;
        check_block(status, &body)?;
        let v: Value = serde_json::from_str(&body).context("知乎发布响应非 JSON")?;
        let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
        if status != 200 || msg != "success" {
            bail!(
                "知乎发布未成功 HTTP {status} message={msg:?}: {}",
                crate::http::truncate_chars(&body, 240)
            );
        }
        let url = format!("{ZHUANLAN}/p/{id}");
        tracing::info!("知乎发布成功: {url}");
        Ok(url)
    }

    /// 挂话题。成功返回 Some(知乎侧真实话题名)；无安全匹配返回 None；网络/HTTP 错误 Err。
    /// 已挂在 `attached` 里的话题名会被跳过（返回 None）。
    fn attach_topic(&self, id: &str, topic: &str, attached: &[String]) -> Result<Option<String>> {
        let q = urlencode(topic);
        let resp = self
            .req(
                reqwest::Method::GET,
                &format!(
                    "{ZHUANLAN}/api/autocomplete/topics?token={q}&max_matches=5&use_similar=0&topic_filter=1"
                ),
            )
            .send()
            .context("知乎话题补全请求失败")?;
        let (status, body) = read(resp)?;
        check_block(status, &body)?;
        let arr: Value = serde_json::from_str(&body).context("话题补全响应非 JSON")?;
        let candidates = arr.as_array().cloned().unwrap_or_default();
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
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("{ZHUANLAN}/api/articles/{id}/topics"),
            )
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .context("知乎绑定话题请求失败")?;
        let (status, body) = read(resp)?;
        check_block(status, &body)?;
        if !(200..300).contains(&status) {
            bail!("绑定话题 HTTP {status}: {}", crate::http::truncate_chars(&body, 160));
        }
        tracing::info!("话题“{topic}”→ 知乎话题“{name}”");
        Ok(Some(name))
    }
}

/// 候选话题名是否可用及优先级（数字小=优先）。None=直接排除。
fn topic_score(want: &str, name: &str) -> Option<usize> {
    const BRANDS: &[&str] = &["阿里", "腾讯", "华为", "京东", "天翼", "移动", "亚马逊", "AWS", "百度", "电信", "联通"];
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
        bail!("知乎要求人工验证（captcha）：自动发布已停止，需人工在浏览器完成验证。响应: {}",
            crate::http::truncate_chars(body, 200));
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
        assert_eq!(topic_score("云服务器", "腾讯云服务器"), None, "品牌名必须排除");
        assert_eq!(topic_score("云服务器", "华为云服务器"), None);
        assert_eq!(topic_score("云服务器", "某大厂服务器"), None);
        assert_eq!(topic_score("免费云服务器", "免费云服务器"), Some(0), "完全相等最优");
        assert_eq!(topic_score("免费云服务器", "免费云服务器推荐"), Some(1), "前缀匹配次优");
        assert_eq!(topic_score("免费云服务器", "三丰云服务器"), None, "不含目标词则排除");
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
    fn check_block_detects_captcha() {
        assert!(check_block(200, "{\"err\":\"请通过验证码\"}").is_err());
        assert!(check_block(403, "forbidden").is_err());
        assert!(check_block(200, "{\"captcha\":{\"token\":\"x\"}}").is_err());
        assert!(check_block(200, "ok").is_ok());
        // 正常响应里的 verified 字段不得被当成验证挑战（假阳性会废掉整轮发文）
        assert!(check_block(200, "{\"author\":{\"is_verified\":true}}").is_ok());
    }
}
