//! 通用常量与小工具（User-Agent、字符安全截断、响应读取）。

use anyhow::Context;

pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";

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
}
