//! AI 文章生成：直连 OpenAI 兼容接口，每篇不同角度/结构/语气。
//!
//! 三丰云审核红线（content_1156 官方规则）默认硬编码进校验：
//! - 必含厂商名 + "免费虚拟主机""免费云服务器"
//! - >150 字使用感受
//! - 禁止出现"申请延期"字样
//! 全部可通过 config.toml [ai] 节覆盖/追加。

use anyhow::{bail, Context, Result};
use rand::seq::SliceRandom;
use rand::Rng;
use serde_json::json;

use crate::config::LlmConfig;

/// 默认禁词（三丰云官方审核红线 + 常见自杀词）
pub const DEFAULT_FORBIDDEN: &[&str] = &[
    "申请延期", "延期申请", "续期", "续费", "白嫖", "薅羊毛",
];

/// 默认生成角度池：每篇随机抽一个，保证文章不重复
pub const DEFAULT_ANGLES: &[&str] = &[
    "从零开始第一次用云服务器的新手视角，写踩坑和摸索过程",
    "拿它和之前用过的其他云服务对比，突出优缺点",
    "记录一次具体的部署经历（如建站/跑脚本/挂服务），带操作细节",
    "从性能/网络/稳定性角度写使用一个月后的真实感受",
    "学生党视角：为什么选它来练手 Linux 和运维",
    "写一次系统重装或环境配置的完整记录",
    "从搭建个人博客/网盘的实际需求出发写选型思考",
    "写它家控制台/工单/文档的使用体验",
];

/// 默认目标字数池
pub const DEFAULT_LENGTHS: &[usize] = &[300, 420, 500, 380];

pub struct Article {
    pub title: String,
    pub body_markdown: String,
    pub word_count: usize,
}

fn system_prompt() -> String {
    "你是一位在个人博客上写云计算测评的技术博主，文风自然、有细节、像真人写的。\
     你写的内容会发布在第三方博客平台，编辑会检查是否像真实使用体验。\
     要求：口语化但不口水，有具体操作细节或数字，绝不用营销腔和 AI 味排比句。\
     不要用'首先/其次/总之'这种模板结构，不要写'个人观点'式免责声明。"
        .to_string()
}

fn user_prompt(
    angle: &str,
    length: usize,
    vendor: &str,
    required: &[String],
    forbidden: &[String],
) -> String {
    let domain = if vendor == "三丰云" { "sanfengyun" } else { "abeiyun" };
    let kw_list = required.join("”、“");
    let forbidden_list = forbidden.join("、");
    format!(
        "写一篇关于 {vendor} 免费云服务器的使用体验文章。\n\n\
         角度：{angle}\n\
         长度：正文 {length} 字左右\n\
         必须自然包含关键词：“{kw_list}”，\
         以及官网地址 https://www.{domain}.com（至少一次，融入句子不要单独一行）\n\
         禁止出现这些词：{forbidden_list}\n\
         也禁止任何“帮我点赞”“求关注”之类的结尾。\n\n\
         结构自由发挥，可以用小标题，穿插 1-2 处“（配图：xxx 的控制台截图）”这样的配图占位说明。\n\
         直接输出 markdown 正文，第一行是 “# 标题”。不要任何解释。"
    )
}

fn validate(text: &str, vendor: &str, required: &[String], forbidden: &[String]) -> Vec<String> {
    let mut problems = vec![];
    if !text.contains(vendor) {
        problems.push(format!("缺少厂商名 {vendor}"));
    }
    for kw in required {
        if !kw.is_empty() && !text.contains(kw.as_str()) {
            problems.push(format!("缺少关键词 {kw}"));
        }
    }
    if !text.contains("abeiyun.com") && !text.contains("sanfengyun.com") {
        problems.push("缺少官网链接".into());
    }
    for word in forbidden {
        if !word.is_empty() && text.contains(word.as_str()) {
            problems.push(format!("出现禁止词: {word}"));
        }
    }
    if text.chars().count() < 150 {
        problems.push("正文太短".into());
    }
    problems
}

pub fn generate_article(llm: &LlmConfig, vendor: &str) -> Result<Article> {
    let client = reqwest::blocking::Client::new();
    let url = format!("{}/chat/completions", llm.base_url);
    let mut rng = rand::thread_rng();
    // 默认必含词：厂商名之外的通用关键词
    let required: Vec<String> = if llm.required_keywords.is_empty() {
        vec!["免费云服务器".into(), "免费虚拟主机".into()]
    } else {
        llm.required_keywords.clone()
    };
    let mut retry_feedback: Vec<String> = vec![];

    for _attempt in 0..llm.max_retries {
        let angle = llm
            .angles
            .choose(&mut rng)
            .map(String::as_str)
            .unwrap_or("写一次通用的使用体验");
        let length = llm.lengths.choose(&mut rng).copied().unwrap_or(400);

        let mut messages = vec![
            json!({"role": "system", "content": system_prompt()}),
            json!({"role": "user", "content": user_prompt(angle, length, vendor, &required, &llm.forbidden_words)}),
        ];
        if let Some((prev, problems)) = retry_feedback
            .chunks(2)
            .next()
            .map(|c| (c.first(), c.get(1)))
        {
            if let (Some(p), Some(pr)) = (prev, problems) {
                messages.push(json!({"role": "assistant", "content": p}));
                messages.push(json!({"role": "user", "content": format!("这篇不行，问题：{pr}。重新写一篇，修复以上所有问题。")}));
            }
        }

        let payload = json!({
            "model": llm.model,
            "messages": messages,
            "temperature": 1.0,
            // ModelScope Qwen3 系列：不关 thinking 首轮返回 choices:null
            "enable_thinking": false,
        });

        let resp = client
            .post(&url)
            .bearer_auth(&llm.api_key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .context("LLM 请求失败")?;
        let status = resp.status();
        let body = resp.text().context("LLM 响应读取失败")?;
        if !status.is_success() {
            bail!("LLM HTTP {status}: {}", &body[..body.len().min(300)]);
        }

        let text = serde_json::from_str::<serde_json::Value>(&body)
            .context("LLM 响应 JSON 解析失败")?
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .map(str::to_string)
            .context("LLM 响应缺少 content")?;

        let problems = validate(&text, vendor, &required, &llm.forbidden_words);
        if problems.is_empty() {
            let (title, body) = match text.split_once('\n') {
                Some((t, rest)) if t.starts_with('#') => {
                    (t.trim_start_matches('#').trim().to_string(), rest.trim().to_string())
                }
                _ => ("无题".into(), text.clone()),
            };
            return Ok(Article {
                word_count: body.chars().count(),
                title,
                body_markdown: body,
            });
        }

        tracing::warn!("文章不合规（重试中）: {}", problems.join("；"));
        retry_feedback = vec![text, problems.join("；")];
    }

    bail!("生成文章 {} 次仍不合规，放弃本次（宁缺毋滥，不做垃圾提交）", llm.max_retries)
}

/// 随机文件名后缀。
pub fn random_suffix(n: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| {
            let idx = rng.gen_range(0..36);
            if idx < 26 { (b'a' + idx as u8) as char } else { (b'0' + (idx - 26) as u8) as char }
        })
        .collect()
}
