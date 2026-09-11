//! 截图：headless chromium 对已发布文章页截 1920x1080 全页图。
//!
//! 关键认知（2026-09-11 实测）：CSDN 对非浏览器流量随机返回 521 + JS 挑战页
//! （裸 HTTP GET 大概率 521，Chrome 执行挑战 JS 后会种 acw cookie 放行）。
//! 因此"页面就绪"判断必须在 Chrome 内做，裸 HTTP 检查只能参考不能定生死。

use std::path::{Path, PathBuf};
use std::collections::HashMap;

use anyhow::{bail, Context, Result};

/// 组装 Cookie 头：登录 Cookie（config 传入）+ WAF 挑战解出的 acw Cookie（如有）
fn build_cookie_header(acw: Option<&str>, login: Option<&str>) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    let login = login.map(str::trim).unwrap_or_default();
    let mut merged = String::new();
    if let Some(a) = acw {
        merged.push_str(a);
        merged.push_str("; ");
    }
    merged.push_str(login);
    if !merged.trim().is_empty() {
        headers.insert("Cookie".into(), merged);
    }
    headers
}

/// 对文章页截图（挑战感知：先过 WAF 挑战再截），返回截图路径。
/// `title` 用于验证渲染的是真文章页而非挑战页。
/// `login_cookie` 来自 config（config.rs 是唯一配置出口），无则 None。
pub fn capture(url: &str, title: &str, debug_dir: &Path, login_cookie: Option<&str>) -> Result<PathBuf> {
    let out = debug_dir.join("postpone.png");
    std::fs::create_dir_all(debug_dir).context("创建截图目录失败")?;

    let mut opts = headless_chrome::LaunchOptions {
        headless: true,
        sandbox: false,
        window_size: Some((1920, 1080)),
        // Actions 容器里 root 用户必须带 --no-sandbox
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
            opts.path = Some(std::path::PathBuf::from(p));
        }
    }

    let browser = headless_chrome::Browser::new(opts)
        .context("启动 headless chromium 失败（检查 chrome/chromium 是否安装）")?;
    let tab = browser.new_tab().context("打开新标签页失败")?;

    // ── 反检测：crate 内置 stealth 五件套（webdriver/chrome/plugins/permissions/webgl）
    //    + UA 覆盖（去掉 HeadlessChrome 标记）。bot-score 按浏览器指纹打分，
    //    不伪装 = 数据中心 IP + headless 指纹 → 直接 403。
    //    注意：本地网络可能经梯子，探测结果不可作准；Actions 环境才是准数。
    tab.enable_stealth_mode()
        .context("注入 stealth 反检测脚本失败")?;
    tab.set_user_agent(crate::http::BROWSER_UA, Some("zh-CN,zh;q=0.9"), Some("Win32"))
        .context("设置 UA 覆盖失败")?;
    tracing::info!("已启用 stealth 模式 + UA 覆盖");

    // ── WAF 预解：裸请求拿挑战 → Node 沙箱求解 → acw Cookie 注入 Chrome ──
    let mut acw: Option<String> = None;
    let probe = reqwest::blocking::Client::new()
        .get(url)
        .header("User-Agent", crate::http::BROWSER_UA)
        .timeout(std::time::Duration::from_secs(30))
        .send();
    if let Ok(resp) = probe {
        if let Ok(body) = resp.text() {
            if crate::waf::is_challenge(&body) {
                tracing::info!("命中 WAF 521 挑战，Node 沙箱求解中...");
                match crate::waf::solve(&body) {
                    Ok(acw_cookie) => {
                        tracing::info!("挑战求解成功: {} 字节 Cookie", acw_cookie.len());
                        acw = Some(acw_cookie);
                    }
                    Err(e) => tracing::warn!("挑战求解失败（继续尝试直连）: {e}"),
                }
            } else if crate::waf::is_hard_block(&body) {
                tracing::warn!("裸请求即被 bot-score 硬拒——尝试注入 Cookie 后由 Chrome 重试");
            }
        }
    }

    // Cookie 头合并：登录 Cookie + acw（挑战解出的）
    let headers = build_cookie_header(acw.as_deref(), login_cookie);
    if !headers.is_empty() {
        let hdr_ref: HashMap<&str, &str> =
            headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        tab.set_extra_http_headers(hdr_ref)
            .context("注入 Cookie 头失败")?;
        tracing::info!("已注入 Cookie 头（len={}）", headers.values().next().map(|v| v.len()).unwrap_or(0));
    }

    // title 为空 = 跳过标题匹配（只防挑战页）；非空 = 验证渲染的是真文章页
    let title_key = if title.is_empty() {
        String::new()
    } else {
        title.chars().take(12).collect::<String>()
    };
    let mut rendered = false;

    // 挑战感知循环：最多 4 次导航。挑战 JS 在首次加载时执行并种 cookie，
    // 之后的导航就是真页面。
    for attempt in 1..=4 {
        tab.navigate_to(url).with_context(|| format!("导航失败(第{attempt}次)"))?;
        tab.wait_until_navigated().context("页面加载超时")?;
        // 挑战 JS 执行 + 真页面渲染余量
        std::thread::sleep(std::time::Duration::from_secs(3));

        let html = tab.get_content().unwrap_or_default();
        if crate::waf::is_challenge(&html) {
            tracing::warn!("第 {attempt} 次导航命中 WAF 挑战页，等 cookie 生效后重导航");
            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }
        if !title_key.is_empty() && !html.contains(&title_key) {
            tracing::warn!("第 {attempt} 次导航：页面不含文章标题关键词（len={}），重试", html.len());
            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }
        rendered = true;
        break;
    }
    if !rendered {
        // 截一张现场图落盘用于诊断（可能是挑战页/404），但明确报错，不提交垃圾截图
        if let Ok(png) = tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        ) {
            let _ = std::fs::write(debug_dir.join("failed.png"), png);
        }
        if let Ok(html) = tab.get_content() {
            let _ = std::fs::write(debug_dir.join("page.html"), html);
        }
        bail!("4 次导航后仍未渲染出真文章页（WAF 挑战或文章不可见），拒绝提交垃圾截图");
    }

    let png_bytes = tab
        .capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true, // captureBeyondViewport = 全页
        )
        .context("截图失败")?;
    std::fs::write(&out, png_bytes).context("保存截图失败")?;

    // HTML 存档供诊断
    if let Ok(html) = tab.get_content() {
        let _ = std::fs::write(debug_dir.join("page.html"), html);
    }

    Ok(out)
}

/// 裸 HTTP 就绪探测（参考性检查，非门禁）。
/// CSDN 对非浏览器流量随机 521 挑战，此检查只能证明"可达"，不能证明"未挑战"。
/// 真正的渲染验证在 `capture` 内（Chrome 过挑战 + 标题匹配）。
pub fn wait_article_ready(url: &str, timeout_secs: u64) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut last_status;
    loop {
        match client.get(url).timeout(std::time::Duration::from_secs(20)).send() {
            Ok(resp) => {
                last_status = resp.status().to_string();
                if resp.status().as_u16() == 200 {
                    if let Ok(body) = resp.text() {
                        // 200 且内容量大 → 大概率真页面（挑战页只有 ~2KB）
                        if body.len() > 20_000 {
                            return Ok(());
                        }
                        last_status = format!("200-but-small({}B)", body.len());
                    }
                }
            }
            Err(e) => last_status = format!("err:{e}"),
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("裸 HTTP 就绪检查超时（最后状态: {last_status}）——WAF 挑战期，交由 Chrome 截图流程处理");
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}
