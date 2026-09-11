//! CSDN 登录 Cookie 采集器：有头浏览器人工扫码登录，自动检测登录态并导出 Cookie。
//!
//! 本地运行（别进 CI）：
//!   cargo run --release --bin csdn_cookie_export
//!   → 弹出 Chrome 打开 csdn.net → 你扫码/手动登录
//!   → 程序自动检测到 UserToken Cookie → 等登录态稳定 → 导出退出，无需回车
//!
//! 产物：
//!   csdn_cookies.txt  Netscape 全文格式
//!   终端打印单行版 k=v; k=v...（整段复制进 GitHub Secret: CSDN_COOKIES）

use std::time::Duration;

use anyhow::{bail, Context, Result};

const LOGIN_CHECK_INTERVAL: u64 = 3;
const SETTLE_SECONDS: u64 = 10;

fn main() -> Result<()> {
    println!("== CSDN Cookie 采集器 ==");
    println!("即将打开浏览器，请在页面里完成登录（建议手机扫码）……");

    let mut opts = headless_chrome::LaunchOptions {
        headless: false, // 有头，人工扫码
        sandbox: false,
        window_size: Some((1366, 900)),
        args: vec![std::ffi::OsStr::new("--lang=zh-CN")],
        ..Default::default()
    };
    if let Ok(p) = std::env::var("CHROME_PATH") {
        if !p.is_empty() {
            opts.path = Some(std::path::PathBuf::from(p));
        }
    }
    let browser = headless_chrome::Browser::new(opts).context(
        "启动 Chrome 失败：装 Chrome，或设环境变量 CHROME_PATH 指向 msedge.exe 也可",
    )?;

    let tab = browser.new_tab().context("开标签页失败")?;
    tab.navigate_to("https://www.csdn.net/")
        .context("打不开 csdn.net")?;
    tab.wait_until_navigated().context("csdn.net 加载超时")?;

    // 轮询登录态：UserToken / UserName 出现即视为已登录
    let mut logged = false;
    for round in 1.. {
        std::thread::sleep(Duration::from_secs(LOGIN_CHECK_INTERVAL));
        let cookies = match tab.get_cookies() {
            Ok(c) => c,
            Err(e) => bail!("浏览器窗口已关闭或失联（{e}），采集终止"),
        };
        if cookies.iter().any(|c| c.name == "UserToken" || c.name == "UserName") {
            println!("[采集器] 第 {round} 轮检测到登录态！");
            logged = true;
            break;
        }
        if round == 1 {
            println!("[采集器] 等待登录中……（登录完成后自动导出，无需按回车）");
        }
    }
    if !logged {
        unreachable!("循环内必然 break 或 bail");
    }

    // 等登录态稳定（重定向、后续 Set-Cookie 落地）
    println!("[采集器] 等待 {SETTLE_SECONDS} 秒让 Cookie 稳定……");
    std::thread::sleep(Duration::from_secs(SETTLE_SECONDS));
    tab.navigate_to("https://www.csdn.net/").ok();
    tab.wait_until_navigated().ok();
    std::thread::sleep(Duration::from_secs(3));

    let cookies = tab.get_cookies().context("最终 Cookie 抓取失败")?;
    if cookies.is_empty() {
        bail!("Cookie 为空，异常退出");
    }

    let mut lines: Vec<String> = vec![
        "# Netscape HTTP Cookie File".to_string(),
        "# 由 csdn_cookie_export 生成".to_string(),
    ];
    let mut kv_pairs: Vec<String> = vec![];
    for c in &cookies {
        // Netscape 行：domain \t flag \t path \t secure \t expiry \t name \t value
        let expiry = format!("{}", c.expires as i64);
        lines.push(format!(
            "{}\tTRUE\t{}\t{}\t{}\t{}\t{}",
            c.domain, c.path, c.secure, expiry, c.name, c.value
        ));
        kv_pairs.push(format!("{}={}", c.name, c.value));
    }

    // 固定绝对路径落盘（相对路径会被宿主 shell 清理上下文时吞掉）
    let out_dir = std::env::var("CSDN_COOKIE_OUT_DIR")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().to_string());
    std::fs::create_dir_all(&out_dir)?;
    let out = std::path::Path::new(&out_dir).join("csdn_cookies.txt");
    std::fs::write(&out, lines.join("\n") + "\n").context("写入 csdn_cookies.txt 失败")?;

    // 单行版单独落盘（塞 GitHub Secret CSDN_COOKIES 用的就是它）
    let one = std::path::Path::new(&out_dir).join("csdn_cookies_oneline.txt");
    std::fs::write(&one, kv_pairs.join("; ")).context("写入单行版失败")?;

    println!("\n[采集器] ✅ 共导出 {} 条 Cookie → {}", cookies.len(), out.display());
    println!("[采集器] 单行 Secret 版 → {}", one.display());
    println!("[采集器] 关键 Cookie 检查：");
    for key in ["UserName", "UserToken", "UN", "p_uid"] {
        match cookies.iter().find(|c| c.name == key) {
            Some(c) => println!("  ✅ {key} = {}…", c.value.chars().take(10).collect::<String>()),
            None => println!("  ❌ {key} 缺失（可能影响发文接口）"),
        }
    }
    println!("[采集器] 完成，浏览器即将退出。");
    Ok(())
}
