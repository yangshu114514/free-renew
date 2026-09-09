//! Playwright 截图 → 无浏览器依赖的截图方案：headless chromium（系统安装）。
//! 对已发布文章页截 1920x1080 全页图。
//! 老项目用 PhantomJS + 固定裁剪坐标（top 270 left 520），现在全页截图更真实。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 对文章页截图，返回截图路径。
/// `debug_dir` 同时存页面 HTML（失败诊断用）。
pub fn capture(url: &str, debug_dir: &Path) -> Result<PathBuf> {
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
    // 防御：文章 URL 必须来自我们发文平台的域。CSDN 响应若被污染成任意 URL，
    // 不允许 headless 浏览器去导航（SSRF 面收敛）。
    if !url.starts_with("https://blog.csdn.net/") {
        anyhow::bail!("拒绝截图非预期域的文章 URL: {url}");
    }
    tab.navigate_to(url).context("导航失败")?;
    tab.wait_until_navigated().context("页面加载超时")?;
    // 字体/图片渲染余量
    std::thread::sleep(std::time::Duration::from_secs(2));

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

/// 轮询文章 URL 可访问（CSDN 发布后立即可访问，简单确认 + 渲染余量）。
pub fn wait_article_ready(url: &str, timeout_secs: u64) -> Result<()> {
    let client = reqwest::blocking::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match client
            .get(url)
            .timeout(std::time::Duration::from_secs(20))
            .send()
        {
            Ok(resp) if resp.status().as_u16() == 200 => {
                if let Ok(body) = resp.text() {
                    if body.len() > 500 {
                        return Ok(());
                    }
                }
            }
            _ => tracing::info!("文章页未就绪，10s 后重试: {url}"),
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("等待文章可访问超时: {url}");
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}
