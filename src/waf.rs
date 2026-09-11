//! CSDN WAF 521 挑战求解器。
//!
//! 挑战机制（2026-09-11 实测）：对非浏览器客户端，CSDN 返回 521 + 混淆 JS
//! （`oo` 十六进制数组 + 位运算变换）。脚本执行后向 document.cookie 种入
//! `acw_sc__v2=...`，随后 location.reload()。携带该 Cookie 的后续请求放行。
//!
//! 求解方式：把挑战脚本丢进 Node.js 沙箱（Actions 预装 Node 20），用
//! document/location 桩捕获 Cookie。纯本地执行，脚本内容来自厂商服务器，
//! 不外发任何数据。本地运行需 Node；无 Node 时该降级路径不可用（报错清晰）。

use anyhow::{bail, Context, Result};

/// 判断 HTML 是否为 521 挑战页（而非真页面/403 硬拒页）
pub fn is_challenge(html: &str) -> bool {
    // 挑战页特征：体积小 + 混淆数组 + onload 定时器模式
    html.len() < 5000
        && (html.contains("window.onload=setTimeout") || html.contains("acw_sc__v2"))
}

/// 403 bot-score 硬拒页特征（带 captcha 脚本引用）
pub fn is_hard_block(html: &str) -> bool {
    html.contains("bot-score") || (html.contains("403 Forbidden") && html.contains("WAF"))
}

const NODE_SOLVER: &str = r#"
const fs = require('fs');
const html = fs.readFileSync(process.argv[2], 'utf8');
const scripts = [...html.matchAll(/<script[^>]*>([\s\S]*?)<\/script>/g)].map(m => m[1]);
const jar = {};
const document = {};
Object.defineProperty(document, 'cookie', {
  set(v) { const kv = String(v).split(';')[0]; const i = kv.indexOf('='); if (i > 0) jar[kv.slice(0, i).trim()] = kv.slice(i + 1); },
  get() { return Object.entries(jar).map(([k, v]) => k + '=' + v).join('; '); }
});
const location = { reload() {}, href: '' };
const navigator = { userAgent: 'Mozilla/5.0' };
const window = globalThis;
window.document = document;
window.location = location;
window.navigator = navigator;
window.setTimeout = (fn) => { if (typeof fn === 'string') eval(fn); else if (typeof fn === 'function') fn(); };
let executed = 0;
for (const code of scripts) {
  if (!code.trim()) continue;
  try { eval(code); executed++; } catch (e) { /* 挑战脚本可能依赖执行顺序，忽略单段失败 */ }
}
// 某些变体把 Cookie 种进 window.onload 字符串，已被 setTimeout 桩立即执行
console.log(JSON.stringify(jar));
console.error('scripts executed: ' + executed);
"#;

/// 求解挑战页，返回 "name=value; ..." 形态的 Cookie 串。
pub fn solve(challenge_html: &str) -> Result<String> {
    let script = extract_script(challenge_html).context("挑战页中未找到脚本")?;
    let tmp = std::env::temp_dir();
    let challenge_path = tmp.join("freerenew_challenge.html");
    let solver_path = tmp.join("freerenew_solver.js");
    std::fs::write(&challenge_path, challenge_html).context("写挑战页临时文件失败")?;
    std::fs::write(&solver_path, NODE_SOLVER).context("写求解器临时文件失败")?;

    let out = std::process::Command::new("node")
        .arg(&solver_path)
        .arg(&challenge_path)
        .output()
        .context("运行 node 求解器失败（Actions 预装 Node；本地需安装）")?;

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!("node 求解器退出码 {:?}: {}", out.status.code(), stderr.trim());
    }
    tracing::debug!("挑战求解器: {}", stderr.trim());

    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).context("求解器输出 JSON 解析失败")?;
    let obj = v.as_object().context("求解器输出非对象")?;
    let mut pairs = vec![];
    for (k, val) in obj {
        if let Some(s) = val.as_str() {
            pairs.push(format!("{k}={s}"));
        }
    }
    if pairs.is_empty() {
        bail!("挑战求解未产出 Cookie（脚本可能未执行到种 Cookie 分支）");
    }
    Ok(pairs.join("; "))
}

fn extract_script(html: &str) -> Option<String> {
    // 挑战脚本 = 含混淆数组特征的最大 script 块
    let mut best: Option<&str> = None;
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let open_end = after.find('>')?;
        let close = after[open_end..].find("</script>")? + open_end;
        let body = &after[open_end + 1..close];
        if body.contains("oo=") || body.contains("document.cookie") || body.len() > 500 {
            best = Some(body);
        }
        rest = &after[close..];
    }
    best.map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_challenge_and_hard_block() {
        let challenge = r#"<html><body><script>window.onload=setTimeout("iw(225)", 200);function iw(x){var oo=[0x82,0x8a];document.cookie="acw_sc__v2=test123";}</script></body></html>"#;
        assert!(is_challenge(challenge));
        assert!(!is_hard_block(challenge));

        let hard = r#"<html><head><title>403 Forbidden</title><script src="/cdn_cgi_bs_bot/static/bot-score-v1.js"></script></head><body><center><h1>403 Forbidden</h1></center><hr><center>WAF</center></body></html>"#;
        assert!(is_hard_block(hard));
        assert!(!is_challenge(hard));
    }

    #[test]
    fn extracts_largest_script() {
        let html = r#"<html><script src="x.js"></script><body><script>var oo=[1,2];document.cookie="a=b";</script></body></html>"#;
        let s = extract_script(html).unwrap();
        assert!(s.contains("var oo"));
    }
}
