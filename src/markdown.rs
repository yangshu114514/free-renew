//! 极简 markdown → HTML，供两个发文平台共用。
//!
//! 此前 csdn.rs / zhihu.rs 各有一份近乎重复的转换器（且 csdn 那份把闭合标签写成了
//! `</pre></code>`）。统一到此，仅保留一处真实差异：知乎的文章标题由 API 单独传，正文
//! 里不要再出现 <h1>，故 `keep_h1=false`；CSDN 保留 <h1>。
//!
//! 覆盖续期文章用到的子集：`#`~`######` 标题、``` 代码围栏、`- ` 无序列表、普通段落。
//! 表格/链接/行内强调不处理（原样作为文字输出，知乎/CSDN 均能接受，非阻塞）。

/// 代码围栏语言标识的最大长度（属性值消毒时截断）。
const MAX_LANG_TOKEN_CHARS: usize = 32;

/// 支持的标题层级上限（`#` ~ `######`）。
const MAX_HEADING_LEVEL: usize = 6;

/// HTML 转义（& < > "）。LLM 输出不可信，进 HTML/属性前必须过。
/// `"` 必须转：本函数同时用于 `<code class="language-{...}">` 属性上下文，
/// 只转 &<> 时一个引号就能从属性里越狱。
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 代码围栏语言标识 → 安全 class 片段：只取首个 token，且仅保留 [A-Za-z0-9_+-]。
/// 入参是围栏后的**整行**（常跟说明文字，如 ```js 用于高亮），且内容出自 LLM——
/// 属性值必须消毒。形参旧名叫 `lang`，读代码的人会以为它只是语言名。
fn safe_lang_token(fence_rest: &str) -> String {
    fence_rest
        .split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
        .take(MAX_LANG_TOKEN_CHARS)
        .collect()
}

/// 识别标题行 → (层级, 标题文本)。用查表式实现代替逐级 strip_prefix 的 if-else 链：
/// 原来只认 1~4 级，加一层就得往控制流里塞一个分支。
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > MAX_HEADING_LEVEL {
        return None;
    }
    // markdown 要求 `#` 后必须有空格：`#标题` 不是标题（保持原行为）
    line.get(hashes..)?.strip_prefix(' ').map(|t| (hashes, t))
}

/// 关闭当前列表（如果有）。
fn close_list(out: &mut String, in_list: &mut bool) {
    if *in_list {
        out.push_str("</ul>\n");
        *in_list = false;
    }
}

/// markdown → HTML。`keep_h1=false` 时丢弃 `# ` 一级标题（知乎标题单独传）。
pub fn to_html(md: &str, keep_h1: bool) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut in_list = false;

    for line in md.lines() {
        if let Some(fence_rest) = line.strip_prefix("```") {
            close_list(&mut out, &mut in_list);
            if in_code {
                out.push_str("</code></pre>\n");
            } else {
                let cls = safe_lang_token(fence_rest);
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
        if let Some((level, text)) = heading(line) {
            close_list(&mut out, &mut in_list);
            if level != 1 || keep_h1 {
                out.push_str(&format!("<h{level}>{}</h{level}>\n", escape(text)));
            }
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            // 连续的 `- ` 行合并成一个 <ul>。原来每行都被包成 `<p>- xxx</p>`，
            // 发到平台渲染出来就是纯文本破折号——而 writer 把"8-12 条清单"当作
            // 过审硬指标，"清单感"其实从没送达过。
            if !in_list {
                out.push_str("<ul>\n");
                in_list = true;
            }
            out.push_str(&format!("<li>{}</li>\n", escape(item)));
            continue;
        }
        close_list(&mut out, &mut in_list);
        if line.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("<p>{}</p>\n", escape(line)));
    }

    close_list(&mut out, &mut in_list);
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
        assert!(
            h.starts_with("<pre><code class=\"language-js\">"),
            "got: {h}"
        );
        assert!(!h.contains("onx"));
        // 非字母数字符号被过滤（rust>"x → rustx）
        let h2 = to_html("```rust>\"x\ncode\n```", true);
        assert!(
            h2.starts_with("<pre><code class=\"language-rustx\">"),
            "got: {h2}"
        );
        // 纯符号语言标识 → 消毒后为空 → 不带 class
        assert!(to_html("```!!!\ncode\n```", true).starts_with("<pre><code>"));
    }

    #[test]
    fn escape_covers_quotes() {
        assert_eq!(escape("a\"b"), "a&quot;b");
    }

    #[test]
    fn bullet_lists_render_as_real_lists() {
        // 清单是过审硬指标之一：必须渲染成 <ul><li>，不能塌成 `<p>- xxx</p>` 的纯文本
        let h = to_html("前言\n\n- 第一条\n- 第二条\n\n结尾", true);
        assert!(h.contains("<ul>\n"), "{h}");
        assert!(h.contains("<li>第一条</li>"), "{h}");
        assert!(h.contains("<li>第二条</li>"), "{h}");
        assert!(h.contains("</ul>"), "{h}");
        // 列表外仍走段落
        assert!(h.contains("<p>前言</p>") && h.contains("<p>结尾</p>"));
        // 不得留下 <p>- 开头 的残留
        assert!(!h.contains("<p>- "), "{h}");
        // ul 必须闭合，且不能在 <p> 内部
        assert_eq!(h.matches("<ul>").count(), h.matches("</ul>").count());
    }

    #[test]
    fn list_is_closed_before_following_block() {
        let h = to_html("- 项\n```\ncode\n```", true);
        let ul_end = h.find("</ul>").expect("列表要闭合");
        let pre = h.find("<pre>").expect("代码块要出现");
        assert!(ul_end < pre, "列表必须在代码块前闭合: {h}");
    }

    #[test]
    fn headings_cover_all_six_levels() {
        let h = to_html(
            "# a\n## b\n### c\n#### d\n##### e\n###### f\n####### g",
            true,
        );
        for lvl in 1..=6 {
            assert!(h.contains(&format!("<h{lvl}>")), "缺 h{lvl}: {h}");
        }
        // 7 个 # 不是标题（超出 markdown 层级），落到段落
        assert!(h.contains("<p>####### g</p>"), "{h}");
    }
}
