//! 极简 markdown → HTML，供两个发文平台共用。
//!
//! 此前 csdn.rs / zhihu.rs 各有一份近乎重复的转换器（且 csdn 那份把闭合标签写成了
//! `</pre></code>`）。统一到此，仅保留一处真实差异：知乎的文章标题由 API 单独传，正文
//! 里不要再出现 <h1>，故 `keep_h1=false`；CSDN 保留 <h1>。
//!
//! 只覆盖续期文章用到的子集：#~#### 标题、``` 代码围栏、普通段落。表格/链接/列表不处理
//! （`- ` 行会成普通段落，知乎/CSDN 均能正常渲染为文字，非阻塞）。

/// HTML 转义（& < > "）。LLM 输出不可信，进 HTML/属性前必须过。
/// `"` 必须转：本函数同时用于 `<code class="language-{...}">` 属性上下文，
/// 只转 &<> 时一个引号就能从属性里越狱。
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 代码围栏语言标识 → 安全 class 片段：只取首个 token，且仅保留 [A-Za-z0-9_+-]。
/// 围栏行后面常跟说明文字（```js 用于高亮），且内容出自 LLM——属性值必须消毒。
fn safe_lang_token(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
        .take(32)
        .collect()
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
                let cls = safe_lang_token(lang);
                if cls.is_empty() {
                    out.push_str("<pre><code>");
                } else {
                    out.push_str(&format!("<pre><code class=\"language-{cls}\">"));
                }
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

    #[test]
    fn fence_lang_cannot_escape_class_attribute() {
        // LLM 围栏行 ```js " onx="1 —— 引号/空格不得原样进属性，否则 XSS 越狱
        let h = to_html("```js \" onx=\"1\ncode\n```", true);
        assert!(h.starts_with("<pre><code class=\"language-js\">"), "got: {h}");
        assert!(!h.contains("onx"));
        // 非字母数字符号被过滤（rust>"x → rustx）
        let h2 = to_html("```rust>\"x\ncode\n```", true);
        assert!(h2.starts_with("<pre><code class=\"language-rustx\">"), "got: {h2}");
        // 纯符号语言标识 → 消毒后为空 → 不带 class
        assert!(to_html("```!!!\ncode\n```", true).starts_with("<pre><code>"));
    }

    #[test]
    fn escape_covers_quotes() {
        assert_eq!(escape("a\"b"), "a&quot;b");
    }
}
