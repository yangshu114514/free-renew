//! 云厂商 API 客户端：登录 → 查延期状态 → 提交续期。
//!
//! 协议双重来源：
//! 1. 2020 年 FreeServer 项目逆向（致谢 BookerLiu/FreeServer, Apache-2.0）
//! 2. 2026-09 真账号实测：登录/状态/记录三接口全通，响应形状两家不同（见 parse_state）
//!
//! 错误码实测：所有 cmd 未登录时一律 50140 尚未登录。
//! 阿贝云 HTTPS 对境外 IP 挂 WAF，HTTP 80 实测通——自动降级。

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_LANGUAGE, ORIGIN, REFERER, USER_AGENT};
use serde_json::Value;

use crate::config::CloudAccount;

/// 外层重试退避（秒）。Actions(Azure) → 中国 IDC 的线路抖动率很高，一次失败不能
/// 定生死；总尝试 = 5 轮 × 候选端点数（https + 可选 http 降级）。
const RETRY_BACKOFFS: &[u64] = &[0, 5, 15, 30, 60];

/// 连接握手单独设上限：三丰云境外不可达时表现为 connect 挂起，
/// 若无此上限会白等满 timeout，快速失败才能把时间留给多轮重试。
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// 确定性业务拒绝：请求本身就不对，换端点或再等 4 轮退避都不会变好。
/// 识别出来直接收工，省下的 5/15/30/60 秒是多账号场景里实打实的时限预算。
/// 403/429 不在其列——那多半是 WAF/限流，等一等或换 http 端点确实可能恢复。
fn is_deterministic_reject(code: u16) -> bool {
    matches!(code, 400 | 404 | 405 | 406 | 410 | 422)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewState {
    /// 已到审核期，可以提交
    CanRenew,
    /// 未到期，extra 里是 next_time
    Waiting,
    /// 已提交，人工审核中
    UnderReview,
    /// 未识别的原始状态
    Unknown,
}

pub struct SubmitResult {
    pub ok: bool,
    pub raw: String,
}

/// api.xxx.com → https://www.xxx.com/（Referer/Origin 用）。
/// 解析不出主机就返回 None——原来硬编码兜底成 www.sanfengyun.com，会让阿贝云的
/// 请求带上三丰云的 Referer，而日志里完全看不出来。
fn site_origin(url: &str) -> Option<String> {
    let host = url.split("://").nth(1)?.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    let site = host.strip_prefix("api.").unwrap_or(host);
    Some(format!("https://www.{site}"))
}

pub struct CloudClient {
    account: CloudAccount,
    http: Client,
    logged_in: bool,
}

impl CloudClient {
    pub fn new(account: CloudAccount, timeout_secs: u64) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(5))
            // 关键：登录后的会话 Cookie 必须跨请求保持
            .cookie_store(true)
            .build()
            .context("构建厂商 API HTTP 客户端失败")?;
        Ok(Self {
            account,
            http,
            logged_in: false,
        })
    }

    /// https 失败自动降级 http（阿贝云境外 WAF 特供）
    fn candidate_urls(&self, url: &str) -> Vec<String> {
        let mut urls = vec![url.to_string()];
        if self.account.profile.allow_http_fallback {
            if let Some(stripped) = url.strip_prefix("https://") {
                urls.push(format!("http://{stripped}"));
            }
        }
        urls
    }

    /// 对每个候选端点现构请求体（multipart 的 Form 不可 Clone，不能 build 一次再 clone）。
    /// 外层带重试：Actions(Azure) → 中国 IDC 的线路抖动率很高，一次失败不能定生死。
    /// 总尝试 = RETRY_BACKOFFS.len() 轮 × 候选端点数；配合 connect_timeout，
    /// 死主机每端点约 10s 快速失败，路由短暂恢复即可续上。
    /// timeout_secs：小请求（登录/查状态）30s 足够；带截图的提交 POST 需更宽。
    fn post_with<F>(&self, url: &str, timeout_secs: u64, build: F) -> Result<String>
    where
        F: Fn(&str) -> reqwest::blocking::RequestBuilder,
    {
        let origin = site_origin(url);
        // 每轮失败都留档：只保留最后一次的话，"连接被拒 / 超时 / 500"的区别
        // 在排障时全丢了——本项目的错误处理一贯以完整错误链为准。
        let mut errors: Vec<String> = Vec::new();
        let mut last_err: Option<anyhow::Error> = None;

        for (round, backoff) in RETRY_BACKOFFS.iter().enumerate() {
            if *backoff > 0 {
                tracing::warn!("请求重试 第{}轮（{backoff}s 后）: {url}", round + 1);
                std::thread::sleep(Duration::from_secs(*backoff));
            }
            let mut deterministic_reject = false;
            for u in self.candidate_urls(url) {
                let mut req = build(&u)
                    .header(USER_AGENT, crate::http::BROWSER_UA)
                    .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
                    .timeout(Duration::from_secs(timeout_secs));
                if let Some(o) = &origin {
                    req = req
                        .header(REFERER, format!("{o}/"))
                        .header(ORIGIN, o.clone());
                }
                match req.send() {
                    Ok(r) if r.status().as_u16() == 200 => match r.text() {
                        Ok(text) => return Ok(decode_text(&text)),
                        Err(e) => {
                            // 读响应体失败也必须回到重试循环：原来这里一个 `?` 直接结束
                            // 整轮，后续 4 轮退避与 http 降级候选全部作废——网络抖一次
                            // 就废掉整个账号的续期机会。
                            errors.push(format!("{u} 读取响应体失败: {e}"));
                            last_err = Some(anyhow!(e).context(format!("读取响应体失败: {u}")));
                        }
                    },
                    Ok(r) => {
                        let code = r.status().as_u16();
                        errors.push(format!("{u} HTTP {code}"));
                        last_err = Some(anyhow!("HTTP {code}: {u}"));
                        deterministic_reject |= is_deterministic_reject(code);
                    }
                    Err(e) => {
                        errors.push(format!("{u} 请求失败: {e}"));
                        last_err = Some(anyhow!(e).context(format!("请求失败: {u}")));
                    }
                }
            }
            if deterministic_reject {
                tracing::warn!("厂商接口给出确定性拒绝（请求本身不对），停止重试以节省时间预算");
                break;
            }
        }

        let detail = if errors.is_empty() {
            "无可用端点".to_string()
        } else {
            errors.join(" ｜ ")
        };
        Err(last_err
            .unwrap_or_else(|| anyhow!("所有端点均失败: {url}"))
            .context(format!(
                "{url} 全部尝试失败（{} 次）: {detail}",
                errors.len()
            )))
    }

    /// 登录并立刻查询免费服务器延期状态。返回 (状态, next_time 或说明, 原始响应摘录)。
    pub fn login_and_check(&mut self) -> Result<(RenewState, String, String)> {
        let login_url = self.account.login_url.clone();
        let form = [
            ("cmd", "login"),
            ("id_mobile", self.account.username.as_str()),
            ("password", self.account.password.as_str()),
        ];
        let resp_body = self
            .post_with(&login_url, 30, |u| self.http.post(u).form(&form))
            .context("登录请求失败")?;

        if !resp_body.contains("登录成功") && !resp_body.contains("登陆成功") {
            // 响应体可能是整页 WAF HTML，截断防日志爆量
            anyhow::bail!("登录失败: {}", crate::http::truncate_chars(&resp_body, 300));
        }
        self.logged_in = true;
        tracing::debug!("登录成功");

        self.check_status()
    }

    pub fn check_status(&self) -> Result<(RenewState, String, String)> {
        if !self.logged_in {
            anyhow::bail!("请先 login_and_check()");
        }
        let url = self.account.renew_url.clone();
        let body = self
            .post_with(&url, 30, |u| {
                self.http
                    .post(u)
                    .form(&[("cmd", "check_free_delay"), ("ptype", "vps")])
            })
            .context("状态查询失败")?;

        // 解析失败要单独报出来：原来 unwrap_or(Value::Null) 让"响应不是 JSON"
        // 和"JSON 结构变了"在下游退化成同一句"状态接口异常"，分不清是哪种。
        let data: Value = serde_json::from_str(&body).map_err(|e| {
            anyhow!(
                "状态接口响应非 JSON（{e}）: {}",
                crate::http::truncate_chars(&body, 300)
            )
        })?;
        let inner = data.get("msg").cloned().unwrap_or(Value::Null);
        if !inner.is_object() {
            anyhow::bail!("状态接口异常: {}", crate::http::truncate_chars(&body, 300));
        }
        let (state, extra) = parse_state(self.account.profile.key, &inner);
        // 原始响应摘录一并返回：厂商 API 字段形状常有漂移，
        // 日志里留原文，状态判定有疑问时不用再抓包猜
        let raw = crate::http::truncate_chars(&body, 500);
        Ok((state, extra, raw))
    }

    /// 提交续期：文章 URL + 截图文件。
    /// 与官方控制台一致：multipart/form-data，boundary 由 http 库自动生成。
    pub fn submit_renewal(
        &self,
        article_url: &str,
        screenshot: &std::path::Path,
    ) -> Result<SubmitResult> {
        if !self.logged_in {
            anyhow::bail!("请先 login_and_check()");
        }
        let img_bytes = std::fs::read(screenshot).context("读取截图文件失败")?;
        let url = self.account.renew_url.clone();

        let body = self
            .post_with(&url, 90, |u| {
                // 每次迭代现构 Form（Part 不可 Clone）
                let part = multipart_part(&img_bytes);
                let form = reqwest::blocking::multipart::Form::new()
                    .text("cmd", "free_delay_add")
                    .text("ptype", "vps")
                    .text("url", article_url.to_string())
                    .part("yanqi_img", part);
                self.http.post(u).multipart(form)
            })
            .context("续期提交失败")?;

        Ok(SubmitResult {
            ok: body.contains("提交成功"),
            raw: body.chars().take(200).collect(),
        })
    }

    /// 延期记录列表（真·历史记录接口：上一轮提交的审核结论在这里）。
    /// 与状态接口交叉核对——状态查询接口的 delay_state 只是参考。
    pub fn review_history(&self) -> Result<Value> {
        let url = self.account.renew_url.clone();
        let body = self
            .post_with(&url, 30, |u| {
                self.http.post(u).form(&[
                    ("cmd", "free_delay_list"),
                    ("ptype", "vps"),
                    ("count", "20"),
                    ("page", "1"),
                ])
            })
            .context("延期记录查询失败")?;
        serde_json::from_str(&body).context("延期记录 JSON 解析失败")
    }
}

fn multipart_part(bytes: &[u8]) -> reqwest::blocking::multipart::Part {
    reqwest::blocking::multipart::Part::bytes(bytes.to_vec())
        .file_name("postpone.png")
        .mime_str("image/png")
        .expect("png mime 固定合法")
}

/// 取字段并统一成字符串：两家厂商数字/字符串形状不一，都吃。
fn field_str(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// 三丰云状态判定（2026-09 真账号实测）：
///   {"msg":{"delay_state":"审核中"},"response":"200"}   // 提交后待审：中文状态字
///   审核期外预期：delay_enable:"0"/"1"(字符串) + next_time
fn parse_sanfengyun(enable: Option<&str>, state_raw: &str) -> RenewState {
    match (enable, state_raw) {
        (Some("1"), _) => RenewState::CanRenew,
        (Some("0"), _) => RenewState::Waiting,
        // 无 enable 字段时只能看状态字
        (_, "审核通过") => RenewState::CanRenew,
        (_, "1") => RenewState::CanRenew,
        (_, "0") => RenewState::Waiting,
        _ => RenewState::Unknown,
    }
}

/// 阿贝云状态判定（2026-09 真账号实测）：
///   {"msg":{"delay_enable":0,"next_time":"2026-09-10 23:49:12"},"check":"e","response":"200"}
///   // delay_enable 是 JSON 数字；未到续期日时仅此字段 + next_time
fn parse_abeiyun(enable: Option<&str>, state_raw: &str) -> RenewState {
    match enable {
        Some("1") => RenewState::CanRenew,
        Some("0") => RenewState::Waiting,
        _ => match state_raw {
            "1" => RenewState::CanRenew,
            "0" => RenewState::Waiting,
            "审核通过" => RenewState::CanRenew,
            _ => RenewState::Unknown,
        },
    }
}

/// 状态解析——两家厂商形状不同，明确分开，别混为一谈。
///
/// 解析优先级。权衡原则：漏续期的代价是服务器回收+数据丢失，
/// 远大于一次多余提交被拒（被拒只产生一条通知）。
/// 1. "审核中" → 上一轮提交仍在人工审核，绝不重复提交
/// 2. delay_enable=1 → 续期窗口已开，续（历史状态字不影响）
/// 3. delay_enable=0 → 未到期
/// 4. 无 enable 字段的 "审核通过" → 上轮延期了结、新窗口可能已开。
///    实测 2026-09-12：控制台显示已到期但 API 仅返回该状态字，
///    视为可续；若窗口实际未开，多余提交会被厂商拒绝并通知，无害。
/// 5. 其余 → Unknown，保守跳过
fn parse_state(vendor_key: &str, inner: &Value) -> (RenewState, String) {
    let enable = field_str(inner, "delay_enable");
    let state_raw = field_str(inner, "delay_state").unwrap_or_default();
    let next_time = field_str(inner, "next_time").unwrap_or_default();

    if state_raw.contains("审核") && !state_raw.contains("通过") {
        return (RenewState::UnderReview, state_raw);
    }

    let state = match vendor_key {
        "sanfengyun" => parse_sanfengyun(enable.as_deref(), &state_raw),
        "abeiyun" => parse_abeiyun(enable.as_deref(), &state_raw),
        _ => RenewState::Unknown,
    };

    let extra = if state == RenewState::Unknown {
        // 未识别时把能拿到的字段都带上，人工排查不用再抓包
        format!("enable={enable:?} state={state_raw:?} next_time={next_time:?}")
    } else {
        next_time
    };
    (state, extra)
}

/// 响应可能是 unicode escape 形态，尽力还原成可读文本。
fn decode_text(text: &str) -> String {
    if text.contains("\\u") {
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            return v.to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn site_origin_derives_from_api_host() {
        assert_eq!(
            site_origin("https://api.sanfengyun.com/www/login.php").as_deref(),
            Some("https://www.sanfengyun.com")
        );
        assert_eq!(
            site_origin("https://api.abeiyun.com/www/renew.php").as_deref(),
            Some("https://www.abeiyun.com")
        );
        // 解析不出主机时不猜一个厂商名（旧实现会兜成三丰云，给阿贝云带错 Referer）
        assert_eq!(site_origin("not-a-url"), None);
    }

    #[test]
    fn state_parsing_matches_real_samples() {
        // 三丰云：审核中（中文状态字）绝不重复提交
        let (s, e) = parse_state("sanfengyun", &json!({"delay_state": "审核中"}));
        assert_eq!(s, RenewState::UnderReview);
        assert_eq!(e, "审核中");

        // 三丰云：delay_enable 字符串
        let (s, e) = parse_state(
            "sanfengyun",
            &json!({"delay_enable": "1", "next_time": "2026-10-01"}),
        );
        assert_eq!(s, RenewState::CanRenew);
        assert_eq!(e, "2026-10-01");
        let (s, _) = parse_state("sanfengyun", &json!({"delay_enable": "0"}));
        assert_eq!(s, RenewState::Waiting);

        // 阿贝云：delay_enable 是 JSON 数字
        let (s, _) = parse_state(
            "abeiyun",
            &json!({"delay_enable": 1, "next_time": "2026-09-10 23:49:12"}),
        );
        assert_eq!(s, RenewState::CanRenew);
        let (s, e) = parse_state(
            "abeiyun",
            &json!({"delay_enable": 0, "next_time": "2026-09-10"}),
        );
        assert_eq!(s, RenewState::Waiting);
        assert_eq!(e, "2026-09-10");

        // 未识别：原始字段全带上，人工排查不用抓包
        let (s, e) = parse_state("abeiyun", &json!({"delay_state": "天知道"}));
        assert_eq!(s, RenewState::Unknown);
        assert!(e.contains("天知道"), "{e}");
    }

    #[test]
    fn unknown_vendor_never_claims_can_renew() {
        // 新厂商未接状态解析前，宁可 Unknown（保守跳过并通知）也不能误判成可续
        let (s, _) = parse_state(
            "thirdvendor",
            &json!({"delay_enable": "1", "delay_state": "1"}),
        );
        assert_eq!(s, RenewState::Unknown);
    }

    #[test]
    fn deterministic_rejects_are_recognized() {
        assert!(is_deterministic_reject(400));
        assert!(is_deterministic_reject(404));
        // WAF/限流类必须继续重试与降级
        assert!(!is_deterministic_reject(403));
        assert!(!is_deterministic_reject(429));
        assert!(!is_deterministic_reject(500));
        assert!(!is_deterministic_reject(200));
    }
}
