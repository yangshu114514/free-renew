//! 截图：headless chromium 对已发布文章页截 1920x1080 全页图。
//!
//! 关键认知（2026-09-11 实测）：CSDN 对非浏览器流量随机返回 521 + JS 挑战页
//! （裸 HTTP GET 大概率 521，Chrome 执行挑战 JS 后会种 acw cookie 放行）。
//! 因此"页面就绪"判断必须在 Chrome 内做，裸 HTTP 检查只能参考不能定生死。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const CHALLENGE_SIGNATURES: &[&str] = &["iw(", "acw_sc", "window.onload=setTimeout"];

fn looks_like_challenge(html: &str) -> bool {
    // 挑战页特征：极短 + 混淆 JS；正常文章页 100KB+ 且含标题
    if html.len() > 20_000 {
        return false;
    }
    CHALLENGE_SIGNATURES.iter().any(|s| html.contains(s))
}

/// 对文章页截图（挑战感知：先过 WAF 挑战再截），返回截图路径。
/// `title` 用于验证渲染的是真文章页而非挑战页。
pub fn capture(url: &str, title: &str, debug_dir: &Path) -> Result<PathBuf> {
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

    // 注入 CSDN 登录 Cookie（CSDN_COOKIES 环境变量，单行 k=v; k=v）：
    // 实测矩阵（2026-09-11）：无 Cookie 的 headless Chrome 从数据中心 IP 访问
    // 文章页 = 403 bot-score 硬拒；带登录 Cookie = 降级为 521 JS 挑战，而
    // Chrome 原生执行挑战 JS 种 acw cookie 后放行 → 截图可行
    if let Ok(ck) = std::env::var("CSDN_COOKIES") {
        let ck = ck.trim();
        if !ck.is_empty() {
            let mut headers = std::collections::HashMap::new();
            headers.insert("Cookie", ck);
            tab.set_extra_http_headers(headers)
                .context("注入 Cookie 头失败")?;
            tracing::info!("已注入 CSDN 登录 Cookie（len={}）", ck.len());
        }
    }

    let title_key = title.chars().take(12).collect::<String>();
    let mut rendered = false;

    // 挑战感知循环：最多 4 次导航。挑战 JS 在首次加载时执行并种 cookie，
    // 之后的导航就是真页面。
    for attempt in 1..=4 {
        tab.navigate_to(url).with_context(|| format!("导航失败(第{attempt}次)"))?;
        tab.wait_until_navigated().context("页面加载超时")?;
        // 挑战 JS 执行 + 真页面渲染余量
        std::thread::sleep(std::time::Duration::from_secs(3));

        let html = tab.get_content().unwrap_or_default();
        if looks_like_challenge(&html) {
            tracing::warn!("第 {attempt} 次导航命中 WAF 挑战页，等 cookie 生效后重导航");
            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }
        if !html.contains(&title_key) {
            tracing::warn!("第 {attempt} 次导航：页面不含文章标题关键词（len={}），重试", html.len());
            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }
        rendered = true;
        break;
    }
    if !rendered {
        // 截一张现场图用于诊断（可能是挑战页/404），但明确报错，不提交垃圾截图
        let _ = tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        );
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
