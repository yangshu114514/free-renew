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

/// api.xxx.com → https://www.xxx.com/（Referer/Origin 用）
fn site_origin(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("www.sanfengyun.com");
    let site = host.strip_prefix("api.").unwrap_or(host);
    format!("https://www.{site}")
}

pub struct CloudClient {
    account: CloudAccount,
    http: Client,
    logged_in: bool,
}

impl CloudClient {
    pub fn new(account: CloudAccount, timeout_secs: u64) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .redirect(reqwest::redirect::Policy::limited(5))
            // 关键：登录后的会话 Cookie 必须跨请求保持
            .cookie_store(true)
            .build()
            .expect("reqwest client 构建，静态参数不会失败");
        Self {
            account,
            http,
            logged_in: false,
        }
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
    /// 总尝试 = 2 轮 × 候选端点数，轮间隔 5s/15s。
    fn post_with<F>(&self, url: &str, build: F) -> Result<String>
    where
        F: Fn(&str) -> reqwest::blocking::RequestBuilder,
    {
        let origin = site_origin(url);
        let mut last_err: Option<anyhow::Error> = None;
        let backoffs = [0u64, 5, 15];
        for (round, backoff) in backoffs.iter().enumerate() {
            if *backoff > 0 {
                tracing::warn!("请求重试 第{}轮（{backoff}s 后）: {url}", round + 1);
                std::thread::sleep(Duration::from_secs(*backoff));
            }
            for u in self.candidate_urls(url) {
                let resp = build(&u)
                    .header(USER_AGENT, crate::http::BROWSER_UA)
                    .header(REFERER, format!("{origin}/"))
                    .header(ORIGIN, origin.clone())
                    .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
                    .timeout(Duration::from_secs(30))
                    .send();
                match resp {
                    Ok(r) if r.status().as_u16() == 200 => {
                        let text = r.text().context("读取响应体失败")?;
                        return Ok(decode_text(&text));
                    }
                    Ok(r) => {
                        last_err = Some(anyhow!("HTTP {}: {u}", r.status()).context("非 200 响应"));
                    }
                    Err(e) => {
                        last_err = Some(anyhow!(e).context(format!("请求失败: {u}")));
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("所有端点均失败: {url}")))
    }

    /// 登录并立刻查询免费服务器延期状态。返回 (状态, next_time 或说明)。
    pub fn login_and_check(&mut self) -> Result<(RenewState, String)> {
        let login_url = self.account.login_url.clone();
        let form = [
            ("cmd", "login"),
            ("id_mobile", self.account.username.as_str()),
            ("password", self.account.password.as_str()),
        ];
        let resp_body = self
            .post_with(&login_url, |u| self.http.post(u).form(&form))
            .context("登录请求失败")?;

        if !resp_body.contains("登录成功") && !resp_body.contains("登陆成功") {
            anyhow::bail!("登录失败: {resp_body}");
        }
        self.logged_in = true;
        tracing::debug!("登录成功");

        self.check_status()
    }

    pub fn check_status(&self) -> Result<(RenewState, String)> {
        if !self.logged_in {
            anyhow::bail!("请先 login_and_check()");
        }
        let url = self.account.renew_url.clone();
        let body = self
            .post_with(&url, |u| {
                self.http.post(u).form(&[("cmd", "check_free_delay"), ("ptype", "vps")])
            })
            .context("状态查询失败")?;

        let data: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let inner = data.get("msg").cloned().unwrap_or(Value::Null);
        if !inner.is_object() {
            anyhow::bail!("状态接口异常: {body}");
        }
        let (state, extra) = parse_state(self.account.profile.key, &inner);
        Ok((state, extra))
    }

    /// 提交续期：文章 URL + 截图文件。
    /// 与官方控制台一致：multipart/form-data，boundary 由 http 库自动生成。
    pub fn submit_renewal(&self, article_url: &str, screenshot: &std::path::Path) -> Result<SubmitResult> {
        if !self.logged_in {
            anyhow::bail!("请先 login_and_check()");
        }
        let img_bytes = std::fs::read(screenshot).context("读取截图文件失败")?;
        let url = self.account.renew_url.clone();

        let body = self
            .post_with(&url, |u| {
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

    /// 延期记录列表（审核状态查询，只读）。日常流程不调用，供手动诊断。
    #[allow(dead_code)]
    pub fn review_history(&self) -> Result<Value> {
        let url = self.account.renew_url.clone();
        let body = self
            .post_with(&url, |u| {
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

/// 状态解析——两家厂商形状不同，明确分开，别混为一谈。
///
/// 三丰云（2026-09 真账号实测）：
///   {"msg":{"delay_state":"审核中"},"response":"200"}   // 提交后待审：中文状态字
///   审核期外预期：delay_enable:"0"/"1"(字符串) + next_time
///
/// 阿贝云（2026-09 真账号实测）：
///   {"msg":{"delay_enable":0,"next_time":"2026-09-10 23:49:12"},"check":"e","response":"200"}
///   // delay_enable 是 JSON 数字；未到续期日时仅此字段 + next_time
fn parse_state(vendor_key: &str, inner: &Value) -> (RenewState, String) {
    let enable = field_str(inner, "delay_enable");
    let state_raw = field_str(inner, "delay_state").unwrap_or_default();
    let next_time = field_str(inner, "next_time").unwrap_or_default();

    let state = match vendor_key {
        "sanfengyun" => match (enable.as_deref(), state_raw.as_str()) {
            (Some("1"), _) => RenewState::CanRenew,
            (Some("0"), _) => RenewState::Waiting,
            (_, "1") => RenewState::CanRenew,
            (_, "0") => RenewState::Waiting,
            // 中文状态字（"审核中"等）只在这家出现
            (_, s) if s.contains("审核") => RenewState::UnderReview,
            _ => RenewState::Unknown,
        },
        "abeiyun" => match enable.as_deref() {
            // 实测：未到期时只有 delay_enable:0 + next_time
            Some("1") => RenewState::CanRenew,
            Some("0") => RenewState::Waiting,
            _ => match state_raw.as_str() {
                "1" => RenewState::CanRenew,
                "0" => RenewState::Waiting,
                s if s.contains("审核") => RenewState::UnderReview,
                _ => RenewState::Unknown,
            },
        },
        _ => RenewState::Unknown,
    };

    let extra = if state == RenewState::UnderReview { state_raw } else { next_time };
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

