//! 截图：headless chromium 对已发布文章页截 1920x1080 全页图。
//!
//! 关键认知（2026-09-11 实测）：CSDN 对非浏览器流量随机返回 521 + JS 挑战页
//! （裸 HTTP GET 大概率 521，Chrome 执行挑战 JS 后会种 acw cookie 放行）。
//! 因此"页面就绪"判断必须在 Chrome 内做，裸 HTTP 检查只能参考不能定生死。
//!
//! `capture` 按阶段拆开（启动/反检测/WAF 预解/注入 Cookie/等可见/截图存档），
//! 每一步都能单独读、单独调，不再是 200 行揉在一起。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use headless_chrome::protocol::cdp::Network;
use headless_chrome::{Browser, Tab};

/// WAF 预解用的裸请求超时（秒）。
const WAF_PROBE_TIMEOUT_SECS: u64 = 30;
/// 等文章公开可见的上限（秒）。总时长受厂商"发布后 1 小时内提交"约束。
const DEFAULT_VISIBLE_SECS: u64 = 720;
/// 可见性轮询间隔（秒）。
const DEFAULT_POLL_SECS: u64 = 30;
/// 导航完成后的 JS 渲染余量（秒）。
const RENDER_GRACE_SECS: u64 = 3;
/// 标题校验取前多少个字符（标题可能被平台截断/加后缀）。
const TITLE_KEY_CHARS: usize = 12;
/// 裸 HTTP 就绪探测的单请求超时（秒）。
const READY_PROBE_TIMEOUT_SECS: u64 = 20;
/// 裸 HTTP 就绪探测的轮询间隔（秒）。
const READY_POLL_SECS: u64 = 10;

/// 等文章可见的等待预算。这些值受厂商"发布后 1 小时内提交"的约束互相牵制，
/// 散成字面量时调一个就顾此失彼，集中到一处并由环境变量统一注入。
struct WaitBudget {
    visible_secs: u64,
    poll_secs: u64,
}

impl WaitBudget {
    fn from_env() -> Self {
        let visible_secs = std::env::var("ARTICLE_VISIBLE_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_VISIBLE_SECS);
        Self {
            visible_secs,
            poll_secs: DEFAULT_POLL_SECS,
        }
    }
}

/// 组装 Cookie 头：登录 Cookie（config 传入）+ WAF 挑战解出的 acw Cookie（如有）。
/// 按 name 去重且不留尾随分隔符：同名 Cookie 重复可能让部分 WAF 判定异常。
fn build_cookie_header(acw: Option<&str>, login: Option<&str>) -> HashMap<String, String> {
    let mut merged = String::new();
    let mut seen: Vec<String> = Vec::new();
    for part in [acw, login]
        .into_iter()
        .flatten()
        .flat_map(|s| s.split(';'))
    {
        let kv = part.trim();
        if kv.is_empty() {
            continue;
        }
        let name = kv.split('=').next().unwrap_or("").trim();
        if name.is_empty() || seen.iter().any(|s| s == name) {
            continue;
        }
        seen.push(name.to_string());
        if !merged.is_empty() {
            merged.push_str("; ");
        }
        merged.push_str(kv);
    }
    let mut headers = HashMap::new();
    if !merged.is_empty() {
        headers.insert("Cookie".into(), merged);
    }
    headers
}

/// 把单行 "k1=v1; k2=v2" 登录 Cookie 串解析成 (name, value) 对，供种进 cookie jar。
/// 空段 / 无名键跳过。值保留原样（不剥引号：知乎 z_c0 等 base64 值本就不带引号，
/// 强行剥反而破坏少数合法含引号的值）。
fn parse_cookie_pairs(cookie: &str) -> Vec<(String, String)> {
    cookie
        .split(';')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// 启动 headless chromium（Actions 容器里以 root 运行，必须 --no-sandbox）。
fn launch_browser() -> Result<Browser> {
    let mut opts = headless_chrome::LaunchOptions {
        headless: true,
        sandbox: false,
        window_size: Some((1920, 1080)),
        args: vec![
            std::ffi::OsStr::new("--no-sandbox"),
            std::ffi::OsStr::new("--disable-dev-shm-usage"),
            std::ffi::OsStr::new("--disable-gpu"),
        ],
        ..Default::default()
    };
    // runner 上 chrome 路径用环境变量指定（ubuntu-latest: /usr/bin/google-chrome）
    if let Ok(p) = std::env::var("CHROME_PATH") {
        if !p.is_empty() {
            opts.path = Some(PathBuf::from(p));
        }
    }
    Browser::new(opts).context("启动 headless chromium 失败（检查 chrome/chromium 是否安装）")
}

/// 反检测：启用 crate 内置 stealth 五件套（webdriver/chrome/plugins/permissions/webgl）
/// 并覆盖 UA（去掉 HeadlessChrome 标记）。bot-score 按浏览器指纹打分，
/// 不伪装 = 数据中心 IP + headless 指纹 → 直接 403。
///
/// 注意：本地网络可能经梯子，探测结果不可作准；Actions 环境才是准数。
fn apply_stealth(browser: &Browser, tab: &Tab) -> Result<()> {
    tab.enable_stealth_mode()
        .context("注入 stealth 反检测脚本失败")?;
    // UA 以浏览器**自报**的真实值为准，只剥掉 HeadlessChrome 标记。
    // 硬编码 UA（曾经的 Chrome/129 + Win32）与 Chrome 自发的 Sec-CH-UA-* 客户端
    // 提示必然对不上：版本随 runner 升级漂移、平台永远是 Linux。指纹自相矛盾
    // 比"承认自己是数据中心 headless"更可疑。查询失败才退回常量兜底。
    let (ua, platform) = match browser.get_version() {
        Ok(v) if !v.user_agent.is_empty() => (
            v.user_agent.replace("HeadlessChrome", "Chrome"),
            "Linux x86_64",
        ),
        _ => (crate::http::BROWSER_UA.to_string(), "Win32"),
    };
    tab.set_user_agent(&ua, Some("zh-CN,zh;q=0.9"), Some(platform))
        .context("设置 UA 覆盖失败")?;
    tracing::info!("已启用 stealth 模式 + UA 覆盖: {ua}");
    Ok(())
}

/// WAF 预解：裸请求拿挑战 → Node 沙箱求解 → 返回 acw Cookie 供注入 Chrome。
/// 解不出来不算失败（Chrome 自己也可能过），返回 None 继续。
fn solve_waf_if_challenged(url: &str) -> Option<String> {
    let probe = reqwest::blocking::Client::new()
        .get(url)
        .header("User-Agent", crate::http::BROWSER_UA)
        .timeout(Duration::from_secs(WAF_PROBE_TIMEOUT_SECS))
        .send();
    let body = probe.ok()?.text().ok()?;
    if crate::waf::is_challenge(&body) {
        tracing::info!("命中 WAF 521 挑战，Node 沙箱求解中...");
        match crate::waf::solve(&body) {
            Ok(acw_cookie) => {
                tracing::info!("挑战求解成功: {} 字节 Cookie", acw_cookie.len());
                return Some(acw_cookie);
            }
            Err(e) => tracing::warn!("挑战求解失败（继续尝试直连）: {e}"),
        }
    } else if crate::waf::is_hard_block(&body) {
        tracing::warn!("裸请求即被 bot-score 硬拒——尝试注入 Cookie 后由 Chrome 重试");
    }
    None
}

/// 注入出站 HTTP 的 Cookie 头（登录 Cookie + acw 挑战 Cookie）。
fn inject_http_cookie_header(tab: &Tab, acw: Option<&str>, login: Option<&str>) -> Result<()> {
    let headers = build_cookie_header(acw, login);
    if headers.is_empty() {
        return Ok(());
    }
    let hdr_ref: HashMap<&str, &str> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    tab.set_extra_http_headers(hdr_ref)
        .context("注入 Cookie 头失败")?;
    // 分别报 acw 与登录 Cookie 的长度：合并后取 "第一个值" 会让排障时误判
    // "登录 Cookie 没注入"（acw 通常排在前面，长度自然对不上）
    tracing::info!(
        "已注入 Cookie 头（acw={} 字节，登录 Cookie={} 字节）",
        acw.map(str::len).unwrap_or(0),
        login.map(|c| c.trim().len()).unwrap_or(0),
    );
    Ok(())
}

/// 登录 Cookie 种进浏览器 cookie jar（document.cookie 可读）。
///
/// `set_extra_http_headers` 只给出站 HTTP 请求加一个 `Cookie` 头，但知乎/CSDN
/// 前端 JS 是用 document.cookie 读登录态判断的——extra HTTP 头**不进**
/// document.cookie，前端仍把访问者当未登录，整页弹扫码登录框
/// （2026-09 实测知乎专栏页复现：截图里满屏"扫码登录"二维码弹窗）。
/// 这里先预导航让 tab.get_url() 落到文章域（set_cookies 内部据此定位 host），
/// 再把登录 Cookie 逐条种成 host cookie；之后渲染循环每次 navigate 都带真实 jar。
///
/// 全流程不致命：种 jar 失败仍有 HTTP 头兜底，只是版面可能带登录墙。
fn seed_login_cookies(tab: &Tab, url: &str, login_cookie: Option<&str>) {
    let Some(login) = login_cookie else {
        return;
    };
    let pairs = parse_cookie_pairs(login);
    if pairs.is_empty() {
        tracing::warn!(
            "登录 Cookie 解析不出有效键值对，未种 jar（若文章在知乎/CSDN 可能仍弹登录墙）"
        );
        return;
    }
    // 预导航：只为让 set_cookies 能据 tab.get_url() 定位 host
    if let Err(e) = tab
        .navigate_to(url)
        .and_then(|_| tab.wait_until_navigated())
    {
        tracing::warn!("种 cookie 前预导航失败（仍尝试种入）: {e:#}");
    }
    // CDP 生成的 CookieParam 只有 name/value 必填（String）、其余 Option
    // 且带 #[serde(default)]，但**没有** derive Default。用最小 JSON 反序列化
    // 最稳（不手写 13 个字段、不赌 Default 是否存在）。
    let params: Vec<Network::CookieParam> = pairs
        .iter()
        .map(|(n, v)| serde_json::json!({ "name": n, "value": v }))
        .map(serde_json::from_value::<Network::CookieParam>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| {
            tracing::warn!("构造 cookie 参数失败，跳过种 jar: {e:#}");
            vec![]
        });
    if params.is_empty() {
        return;
    }
    match tab.set_cookies(params) {
        Ok(()) => tracing::info!(
            "已种 {} 条登录 Cookie 进浏览器 jar（document.cookie 可读）",
            pairs.len()
        ),
        Err(e) => tracing::warn!("种 Cookie jar 失败，仅靠 HTTP 头注入兜底: {e:#}"),
    }
}

/// 轮询等文章"真正公开可见"。
///
/// 知乎/CSDN 刚发布的文章都有审核/放行延迟，期间对访问者（哪怕作者会话）返回
/// 登录墙/首页壳——早前的"内容不存在"拒单就是这个。这里限时反复导航，直到页面
/// 出现标题或超时。
///
/// 返回 `Ok(true)`=已可见；`Ok(false)`=限时内始终不可见（调用方 dump 现场后报错）。
/// 导航失败**不**直接返回错误：原来一次瞬时导航抖动就放弃整轮，与"限时反复重试"
/// 的本意矛盾，而且那句"第 N 次"永远打印 1。
fn wait_until_visible(tab: &Tab, url: &str, title: &str, budget: &WaitBudget) -> Result<bool> {
    // title 为空 = 跳过标题匹配（只防挑战页）；非空 = 验证渲染的是真文章页
    let title_key: String = title.chars().take(TITLE_KEY_CHARS).collect();
    let started = Instant::now();
    let mut attempt = 0u32;
    let mut dom_failures = 0u32;

    loop {
        attempt += 1;
        match tab
            .navigate_to(url)
            .and_then(|_| tab.wait_until_navigated())
        {
            Ok(_) => {
                std::thread::sleep(Duration::from_secs(RENDER_GRACE_SECS)); // JS 渲染余量
                match tab.get_content() {
                    // 连续子串找不到时 html_shows_title 会剥标签再找（知乎/CSDN 常把
                    // 标题拆进多个节点）
                    Ok(html)
                        if !crate::waf::is_challenge(&html)
                            && (title_key.is_empty() || html_shows_title(&html, &title_key)) =>
                    {
                        return Ok(true)
                    }
                    Ok(_) => {}
                    Err(e) => {
                        // 取 DOM 失败曾被 `unwrap_or_default()` 当成"空页面"：既不会命中
                        // 挑战检测，也不含标题，于是循环一路空转到 720s 上限，最后报
                        // "文章仍未公开可见"——把浏览器通信故障这个真实根因彻底掩盖。
                        dom_failures += 1;
                        tracing::warn!(
                            "第 {attempt} 次读取页面 DOM 失败（累计 {dom_failures} 次）: {e}"
                        );
                    }
                }
            }
            Err(e) => tracing::warn!("第 {attempt} 次导航失败（继续重试）: {e:#}"),
        }

        let waited = started.elapsed().as_secs();
        if waited >= budget.visible_secs {
            tracing::warn!(
                "等待 {waited}s（{attempt} 次导航，其中 {dom_failures} 次 DOM 读取失败）后文章仍未公开可见，放弃"
            );
            return Ok(false);
        }
        tracing::info!(
            "文章尚未公开可见（疑似平台审核中），已等 {waited}s，{}s 后重试（第 {attempt} 次）",
            budget.poll_secs
        );
        std::thread::sleep(Duration::from_secs(budget.poll_secs));
    }
}

/// 截 1920x1080 全页图并落盘。
fn shoot(tab: &Tab, out: &Path) -> Result<PathBuf> {
    let png_bytes = tab
        .capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true, // captureBeyondViewport = 全页
        )
        .context("截图失败")?;
    std::fs::write(out, png_bytes).context("保存截图失败")?;
    Ok(out.to_path_buf())
}

/// DOM 存档供诊断（失败只警告：存档是辅助手段，不能反过来打断主流程）。
fn archive_html(tab: &Tab, debug_dir: &Path, tag: &str) {
    match tab.get_content() {
        Ok(html) => {
            if let Err(e) = std::fs::write(debug_dir.join(format!("page-{tag}.html")), html) {
                tracing::warn!("页面 HTML 存档失败: {e}");
            }
        }
        Err(e) => tracing::warn!("读取页面 HTML 失败（无法存档）: {e}"),
    }
}

/// 可见性判定失败时，截一张现场图 + 存 DOM 用于诊断（可能是挑战页/404），
/// 但明确报错，不提交垃圾截图。
fn dump_failure(tab: &Tab, debug_dir: &Path, tag: &str) {
    match tab.capture_screenshot(
        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
        None,
        None,
        true,
    ) {
        Ok(png) => {
            if let Err(e) = std::fs::write(debug_dir.join(format!("failed-{tag}.png")), png) {
                tracing::warn!("失败现场截图落盘失败: {e}");
            }
        }
        Err(e) => tracing::warn!("失败现场截图捕获失败: {e}"),
    }
    archive_html(tab, debug_dir, tag);
}

/// 对文章页截图（挑战感知：先过 WAF 挑战再截），返回截图路径。
/// `title` 用于验证渲染的是真文章页而非挑战页。
/// `login_cookie` 来自 config（config.rs 是唯一配置出口），无则 None。
/// `tag` 进产物文件名（postpone-{tag}.png / failed-{tag}.png / page-{tag}.html）：
/// 多台服务器同轮都到期时，后一台不再覆盖前一台的现场，被拒对账才拿得对图。
/// 正式流程传的是账号 ID（同厂商多账号时用厂商名会互相覆盖）。
pub fn capture(
    url: &str,
    title: &str,
    debug_dir: &Path,
    login_cookie: Option<&str>,
    tag: &str,
) -> Result<PathBuf> {
    let out = debug_dir.join(format!("postpone-{tag}.png"));
    std::fs::create_dir_all(debug_dir).context("创建截图目录失败")?;

    let browser = launch_browser()?;
    let tab = browser.new_tab().context("打开新标签页失败")?;
    apply_stealth(&browser, &tab)?;

    let acw = solve_waf_if_challenged(url);
    inject_http_cookie_header(&tab, acw.as_deref(), login_cookie)?;
    seed_login_cookies(&tab, url, login_cookie);

    if !wait_until_visible(&tab, url, title, &WaitBudget::from_env())? {
        dump_failure(&tab, debug_dir, tag);
        bail!("多次导航后文章仍未公开可见（平台审核未完成或不可见），拒绝提交垃圾截图");
    }

    let path = shoot(&tab, &out)?;
    archive_html(&tab, debug_dir, tag);
    Ok(path)
}

/// 页面是否显示了文章标题。先按原始 HTML 连续子串找（快路径）；找不到再剥掉
/// 标签与空白后找——知乎/CSDN 常把标题拆进多个节点，连续子串会误判"不含标题"。
fn html_shows_title(html: &str, title_key: &str) -> bool {
    if html.contains(title_key) {
        return true;
    }
    let flat: String = strip_tags(html)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let needle: String = title_key.chars().filter(|c| !c.is_whitespace()).collect();
    !needle.is_empty() && flat.contains(&needle)
}

/// 去掉 `<...>` 标签（含属性里的尖括号已转义，够用）。
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// 裸 HTTP 就绪探测（参考性检查，非门禁）。
/// CSDN 对非浏览器流量随机 521 挑战，此检查只能证明"可达"，不能证明"未挑战"。
/// 真正的渲染验证在 `capture` 内（Chrome 过挑战 + 标题匹配）。
pub fn wait_article_ready(url: &str, timeout_secs: u64) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut last_status;
    loop {
        match client
            .get(url)
            .timeout(Duration::from_secs(READY_PROBE_TIMEOUT_SECS))
            .send()
        {
            Ok(resp) => {
                last_status = resp.status().to_string();
                if resp.status().as_u16() == 200 {
                    if let Ok(body) = resp.text() {
                        // 200 且内容量大 → 大概率真页面（挑战页只有 ~2KB）。
                        // 阈值与 waf::is_challenge 共用同一个常量，避免两处漂移。
                        if body.len() > crate::waf::CHALLENGE_MAX_BYTES {
                            return Ok(());
                        }
                        last_status = format!("200-but-small({}B)", body.len());
                    }
                }
            }
            Err(e) => last_status = format!("err:{e}"),
        }
        if Instant::now() > deadline {
            bail!("裸 HTTP 就绪检查超时（最后状态: {last_status}）——WAF 挑战期，交由 Chrome 截图流程处理");
        }
        std::thread::sleep(Duration::from_secs(READY_POLL_SECS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_title_even_when_split_by_tags() {
        let key = "三丰云免费云服务器实测";
        // 连续子串直接命中
        assert!(html_shows_title("<h1>三丰云免费云服务器实测：稳</h1>", key));
        // 被标签/空白切碎也能命中
        let split = "<h1><span>三丰云</span>\n<b>免费云服务器</b>实测</h1>";
        assert!(html_shows_title(split, key));
        // 真不含
        assert!(!html_shows_title("<div>无关内容</div>", key));
    }

    #[test]
    fn cookie_header_merges_dedupes_and_trims_separator() {
        // 只有登录 Cookie 时不得留尾随 "; "
        let h = build_cookie_header(None, Some("z_c0=x; _xsrf=y"));
        assert_eq!(h.get("Cookie").unwrap(), "z_c0=x; _xsrf=y");
        // 合并时登录 Cookie 在内、acw 在前
        let h = build_cookie_header(Some("acw_sc__v2=1"), Some("z_c0=x"));
        assert_eq!(h.get("Cookie").unwrap(), "acw_sc__v2=1; z_c0=x");
        // 同名 Cookie 只保留先出现的那个（重复会让部分 WAF 判定异常）
        let h = build_cookie_header(Some("acw_sc__v2=old"), Some("acw_sc__v2=new; z_c0=x"));
        assert_eq!(h.get("Cookie").unwrap(), "acw_sc__v2=old; z_c0=x");
        // 两个都空 → 不产生 Cookie 头
        assert!(build_cookie_header(None, Some("   ")).is_empty());
    }

    #[test]
    fn parses_login_cookie_pairs() {
        let pairs = parse_cookie_pairs("a=1; b=2 ; ; =bad; c=");
        assert_eq!(
            pairs,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
                ("c".to_string(), String::new()),
            ]
        );
    }
}
