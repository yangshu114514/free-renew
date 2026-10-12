//! 通用常量与小工具（User-Agent、字符安全截断、响应读取）。

use anyhow::Context;

pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";

/// 带浏览器 User-Agent 的 blocking 客户端——发外部请求的统一出口。
///
/// UA 必须在**客户端层**设死，不能只靠各处逐请求 `.header("User-Agent", …)`：
/// dev.to 渠道自 2026-10-02 上线起一次都没发成功过，2026-10-12 查到根因就是
/// [`crate::devto::DevtoClient`] 建客户端时没设 UA，而 reqwest 默认**不发**
/// `User-Agent` 头，dev.to 前置的 Fastly 对无 UA 的 API 请求直接回 403（空体）。
/// 本机对照实测（同一 URL、同一伪造 key，只改 UA 这一个变量）：
///
/// | 请求 | 响应 |
/// | --- | --- |
/// | 不带 `User-Agent` 头 | **403**，响应体为空 |
/// | 带 `reqwest/0.12` / 浏览器 / `curl` 任一 UA | 401 `{"error":"unauthorized"}` |
///
/// 401 与 403 的差别正是"**凭据不对**"与"**被边缘层拦**"的分界。当时把 403 读成
/// "key 已被吊销"，于是十天里每轮续期都静默兜底回 CSDN——故障现象（文章被 CSDN 删）
/// 和故障原因（key 失效）看起来都自洽，唯独没有一个是真的。
///
/// 新增平台客户端请用它，别自己 `Client::builder()`——漏一个 UA 不会有任何编译期
/// 或测试期的提示，只会在生产里变成"某渠道从来没成功过"。
pub fn browser_client(timeout_secs: u64) -> anyhow::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .user_agent(BROWSER_UA)
        .build()
        .context("构建 HTTP 客户端失败")
}

/// 字符安全截断：按字符取前 `max` 个。
/// 字节切片 `&s[..n]` 在中文等多字节字符中间切开会 panic，一律用这个。
pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// 凭据的 Debug 呈现：只给长度，不给内容。
///
/// `redact("")` 返回 `"<空>"`（与"有值但被隐藏"区分开，排障时能看出是没配置
/// 还是配置了）。原先这个函数在 config.rs、devto.rs 的 Debug impl 里各写一份
/// 同义实现，统一到通用层。
pub fn redact(secret: &str) -> String {
    if secret.trim().is_empty() {
        "<空>".to_string()
    } else {
        format!("<已隐藏 {} 字符>", secret.chars().count())
    }
}

/// 统一的响应读取：取状态码 + 读响应体。
///
/// 此前"status → text().context(...) → if !success → bail(truncate)"这段样板在
/// 4 个发文平台客户端里各抄一份（csdn/devto/cnblogs/writer，共约 49 行），
/// zhihu.rs 另有模块内私有版——五份手写，连"先状态后体"的纪律都各写各的。
/// 收敛到这里后，调用方只留各自**业务**的失败判定（哪个码该 bail、bail 文案是什么）。
///
/// 返回 `(状态码, 响应体)`。**不做**任何成功/失败判定：那一步是各平台的业务规则
/// （401/403 对 devto 是鉴权错、对知乎是 Cookie 失效、对 csdn 可能是风控），
/// 硬塞进公共层只会逼出参数化的 if 树。
pub fn read(resp: reqwest::blocking::Response) -> anyhow::Result<(u16, String)> {
    let status = resp.status().as_u16();
    let body = resp.text().context("读取响应体失败")?;
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_char_boundary() {
        let s = "登录失败，密码错误!响应体继续很长很长";
        assert_eq!(truncate_chars(s, 5).chars().count(), 5);
        assert_eq!(truncate_chars("ab", 10), "ab");
        assert_eq!(truncate_chars("", 10), "");
    }

    /// 回归：客户端必须**真的把 User-Agent 发出去**。
    ///
    /// 这个测试存在的唯一理由是 `browser_client` 文档里那个 403：漏设 UA 这件事
    /// 在源码里看不出来（`.user_agent()` 少一行什么都不报），在单元测试外也看不出来
    /// （只有边缘层会拒绝），dev.to 渠道因此静默失效了十天。
    ///
    /// 做法：起一个本地监听当"跳板"，让真客户端打过去，把它收到的请求头原样交回来
    /// 断言。零外网依赖，所以 CI 与本机行为一致。
    #[test]
    fn browser_client_actually_sends_user_agent() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地监听");
        let addr = listener.local_addr().expect("取本地监听地址");
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("接受连接");
            // 请求头可能分多次到达，读到空行（\r\n\r\n）才算收全
            let mut head = Vec::new();
            let mut buf = [0u8; 512];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let _ = sock.write_all(
                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = sock.flush();
            String::from_utf8_lossy(&head).into_owned()
        });

        let client = browser_client(10).expect("构建客户端");
        // 204 不是 reqwest 眼里的错误，这里只关心"请求发出去且被收到"
        let _ = client.get(format!("http://{addr}/probe")).send();
        let head = server.join().expect("收集请求头");

        assert!(head.contains("HTTP/1.1"), "跳板没收到完整请求行: {head:?}");
        let ua = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("user-agent:"))
            .unwrap_or_else(|| {
                panic!("请求里没有 User-Agent 头（dev.to 会对这种请求回 403）:\n{head}")
            });
        assert!(ua.contains("Mozilla/"), "UA 不是浏览器串: {ua}");
    }
}
