//! 通用常量与小工具（User-Agent、字符安全截断）。

pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 字符安全截断：按字符取前 `max` 个。
/// 字节切片 `&s[..n]` 在中文等多字节字符中间切开会 panic，一律用这个。
pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
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
