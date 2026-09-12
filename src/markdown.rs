//! 极简 markdown → HTML，供两个发文平台共用。
//!
//! 此前 csdn.rs / zhihu.rs 各有一份近乎重复的转换器（且 csdn 那份把闭合标签写成了
//! `</pre></code>`）。统一到此，仅保留一处真实差异：知乎的文章标题由 API 单独传，正文
//! 里不要再出现 <h1>，故 `keep_h1=false`；CSDN 保留 <h1>。
//!
//! 只覆盖续期文章用到的子集：#~#### 标题、``` 代码围栏、普通段落。表格/链接/列表不处理
//! （`- ` 行会成普通段落，知乎/CSDN 均能正常渲染为文字，非阻塞）。

/// HTML 转义（最小集：& < >）。LLM 输出不可信，进 HTML/属性前必须过。
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// markdown → HTML。`keep_h1=false` 时丢弃 `# ` 一级标题（知乎标题单独传）。
pub fn to_html(md: &str, keep_h1: bool) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in md.lines() {
        if let Some(lang) = line.strip_prefix("```") {
            if in_code {
                out.push_str("</code></pre>\n");
            } else {
                out.push_str(&format!("<pre><code class=\"language-{}\">", escape(lang)));
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push_str(&format!("{}\n", escape(line)));
            continue;
        }
        if let Some(h) = line.strip_prefix("#### ") {
            out.push_str(&format!("<h4>{}</h4>\n", escape(h)));
        } else if let Some(h) = line.strip_prefix("### ") {
            out.push_str(&format!("<h3>{}</h3>\n", escape(h)));
        } else if let Some(h) = line.strip_prefix("## ") {
            out.push_str(&format!("<h2>{}</h2>\n", escape(h)));
        } else if let Some(h) = line.strip_prefix("# ") {
            if keep_h1 {
                out.push_str(&format!("<h1>{}</h1>\n", escape(h)));
            }
        } else if line.trim().is_empty() {
            continue;
        } else {
            out.push_str(&format!("<p>{}</p>\n", escape(line)));
        }
    }
    if in_code {
        out.push_str("</code></pre>\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_dangerous_chars() {
        assert_eq!(escape("a<b>&c"), "a&lt;b&gt;&amp;c");
    }

    #[test]
    fn csdn_keeps_h1_and_code_lang() {
        let h = to_html("# T\n## H\n正文\n```rust\nfn a(){}\n```\n尾行", true);
        assert!(h.contains("<h1>T</h1>"));
        assert!(h.contains("<h2>H</h2>"));
        assert!(h.contains("language-rust"));
        assert!(h.contains("</code></pre>")); // 闭合顺序正确
        assert!(h.contains("<p>尾行</p>"));
    }

    #[test]
    fn zhihu_drops_h1_keeps_h2() {
        let h = to_html("# T\n## H\n正文段落", false);
        assert!(!h.contains("<h1"));
        assert!(h.contains("<h2>H</h2>"));
        assert!(h.contains("<p>正文段落</p>"));
    }

    #[test]
    fn unclosed_fence_is_closed() {
        let h = to_html("```\ncode\n", true);
        assert!(h.ends_with("</code></pre>\n"));
    }
}
