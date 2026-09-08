//! 日志体系：结构化、详细、可回溯。
//!
//! 设计：
//! - 终端（Actions 日志）：tracing 单行格式
//! - 本地/自托管：同时落盘 JSON Lines（`logs/renew-YYYY-MM-DD.log`），带完整字段，
//!   方便事后分析（每次运行、每个账号、每一步的耗时/结果/原始响应摘录）
//! - RUST_LOG 过滤（默认 info；诊断用 RUST_LOG=free_renew=debug,reqwest=warn）
//!
//! 脱敏原则：用户名打码到前 3 后 2，密码/Cookie/API key 永不进日志。

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;

/// 全局运行上下文：run_id + 开始时间，每条日志都带。
pub struct RunContext {
    pub run_id: String,
    pub started: Instant,
    json_sink: Option<Mutex<std::fs::File>>,
}

impl RunContext {
    pub fn init() -> anyhow::Result<Self> {
        let now = chrono_like_timestamp();
        let run_id = format!("run-{}", now.replace([':', ' '], "-"));

        // 文件日志目录：FREE_RENEW_LOG_DIR > ./logs
        let dir = std::env::var("FREE_RENEW_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("logs"));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{run_id}.log.jsonl"));
        let file = std::fs::File::create(&path)?;

        tracing::info!(target: "log", "JSON 日志文件: {}", path.display());

        Ok(Self {
            run_id,
            started: Instant::now(),
            json_sink: Some(Mutex::new(file)),
        })
    }

    /// 写一条结构化 JSON 事件。
    pub fn event(&self, step: &str, status: &str, detail: serde_json::Value) {
        let Some(sink) = &self.json_sink else { return };
        let line = json!({
            "ts": chrono_like_timestamp(),
            "run_id": self.run_id,
            "elapsed_ms": self.started.elapsed().as_millis() as u64,
            "step": step,
            "status": status,
            "detail": detail,
        });
        if let Ok(mut f) = sink.lock() {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}

/// 手机号/用户名脱敏：保留前 3 后 2，中间全部替换为 *（个数与原长度一致）。
pub fn mask_id(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 5 {
        "*".repeat(chars.len())
    } else {
        format!(
            "{}{}{}",
            chars[..3].iter().collect::<String>(),
            "*".repeat(chars.len() - 5),
            chars[chars.len() - 2..].iter().collect::<String>()
        )
    }
}

/// 无 chrono 依赖的时间戳（UTC）。
fn chrono_like_timestamp() -> String {
    // std 不提供格式化时间；用 unix 秒 + 简易 UTC 换算（精度到秒，够日志用）
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil_from_days 算法（Howard Hinnant），无外部依赖
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_id_shapes() {
        // 11 位手机号形状（假号码）：前3 + 6个* + 后2（总长不变）
        assert_eq!(mask_id("13812345678"), "138******78");
        assert_eq!(mask_id("abc"), "***");
        assert_eq!(mask_id("abcd"), "****");
        assert_eq!(mask_id("abcdef"), "abc*ef");
    }

    #[test]
    fn civil_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // 2024-01-01
    }
}
