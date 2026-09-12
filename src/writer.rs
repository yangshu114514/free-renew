//! AI 文章生成：直连 OpenAI 兼容接口，每篇不同角度/结构/语气。
//!
//! 三丰云审核红线（content_1156 官方规则）默认硬编码进校验：
//! - 必含厂商名 + "免费虚拟主机""免费云服务器"
//! - >150 字使用感受
//! - 禁止出现"申请延期"字样
//!
//! 全部可通过 config.toml [ai] 节覆盖/追加。

use anyhow::{bail, Context, Result};
use rand::seq::SliceRandom;
use serde_json::json;

use crate::config::LlmConfig;

/// 默认禁词。只封真正的"求延期/白嫖"信号词。
/// 注意：曾把"续期/续费"也封了——但实测用户自己那篇被三丰云接受、从没删过的
/// 知乎文章标题就是《…每7天手动续期…》，"续期"恰恰是最真实的用法词，封它反而
/// 逼模型绕着说人话不了、写出更假的同义替换。故解禁续期/续费，仅保留真正的红线。
pub const DEFAULT_FORBIDDEN: &[&str] = &["申请延期", "白嫖", "薅羊毛"];

/// 默认生成角度池（2026-09-12 重写：对齐实测能过审、不被删的真人文章 DNA——
/// 长期真实使用视角 + 具体项目 + 诚实短板，杜绝营销测评腔）
pub const DEFAULT_ANGLES: &[&str] = &[
    "长期使用者复盘：跑了几个月，续期节奏与稳定性体感，哪些省心哪些要忍",
    "拿它跟自己以前用过的免费/廉价云对比，逐项说优劣，最后给适用边界",
    "一次具体部署记录：栈选型、踩的坑、资源占用数字、怎么扛住日常访问",
    "学生/个人开发者视角：为什么拿它练手 Linux 和运维，能干什么不能干什么",
    "环境折腾日志：装环境/换系统/配反代的过程与真实耗时、工单响应体验",
    "从个人实际需求（博客/网盘/内网穿透/私有仓库）出发写选型与取舍",
    "控制台与生态体验：文档、Docker/语言环境、社区问答、隐藏限制",
    "成本与风险权衡：免费换来的限制是什么，为什么仍愿意每周花几十秒维系",
];

/// 人设种子池：每篇随机注入一套"真实项目 + 数字"，逼模型言之有物、避免空泛。
/// 这是过审关键——真人测评一定有具体在跑的东西和量出来的数字。
const PERSONA_SEEDS: &[(&str, &str)] = &[
    ("FastAPI + PostgreSQL 后端、Nuxt3 SSR 前端、Redis 缓存、Gitea 私有仓库", "CPU 偶尔冲到 75~80%，内存常驻 600MB 左右，5M 带宽扛日常访问绰绰有余"),
    ("一个 Typecho 博客 + 反代 + 自动备份脚本，外加 frp 内网穿透", "负载常年 0.2~0.5，内存占用不到一半，磁盘每周涨几百 MB"),
    ("Django 小站 + PostgreSQL + Nginx + 定时任务爬虫", "跑三个服务后内存吃到七成，CPU 峰值到过九成但没卡死过"),
    ("自建 Meilisearch 搜索 + Node 接口 + Obsidian 笔记同步中转", "冷启动慢一点，稳态内存 500MB 上下，API 响应几十毫秒"),
    ("一个 Rust axum 服务 + SQLite + Caddy 自动 HTTPS", "编译时 CPU 会打满几分钟，平时内存占用很低，几百 MB 就够"),
    ("Python 数据分析 notebook + JupyterHub + 定时拉数脚本", "跑大任务时内存吃紧要加 swap，日常挂小任务很稳"),
];

/// 默认目标字数池（2026-09-12 起加长：实测 418 字偏短，审核观感单薄）
pub const DEFAULT_LENGTHS: &[usize] = &[500, 650, 800, 600];

pub struct Article {
    pub title: String,
    pub body_markdown: String,
    pub word_count: usize,
}

fn system_prompt() -> String {
    "你在知乎/博客园以真名写长期技术使用笔记，读者是同类开发者，编辑会人工核是不是 AI 水文。\
     文风：第一人称、口语、有具体项目和数字、敢写缺点和自己踩的坑，句子长短错落、允许偶尔不完整。\
     铁律：\n\
     - 严禁营销/AI 腔词：赋能、一站式、轻松搞定、海量、强大功能、稳定可靠性价比高式排比、\
     总而言之、综上所述、首先其次最后、随着…的发展、不难发现、值得一提的是。\n\
     - 严禁完美对仗的小标题和每段等长的工整结构——真实笔记是松散、详略不均的。\n\
     - 至少写一个具体缺点或麻烦之处（免费服务的真实限制），不许全程夸。\n\
     - 不要免责声明、不要‘以上为个人观点’、不要求赞求关注。\n\
     写成一气呵成的真人记录，不像模板。"
        .to_string()
}

fn user_prompt(
    angle: &str,
    length: usize,
    vendor: &str,
    required: &[String],
    forbidden: &[String],
    persona: (&str, &str),
) -> String {
    let domain = if vendor == "三丰云" { "sanfengyun" } else { "abeiyun" };
    let kw_list = required.join("”、“");
    let forbidden_list = forbidden.join("、");
    let (stack, numbers) = persona;
    format!(
        "以【{angle}】写一篇关于 {vendor} 免费云服务器的真实长期使用笔记。\n\n\
         你在这台机器上实际跑着：{stack}。\
         观测到的资源情况：{numbers}。把这些具体细节自然写进去（可改写措辞，但要有真东西）。\n\
         长度：正文 {length} 字左右。\n\
         必须自然包含关键词：“{kw_list}”，\
         以及官网地址 https://www.{domain}.com（至少一次，融进句子，别单列一行）。\n\
         禁止出现这些词：{forbidden_list}。\n\
         结构松散些、详略不均，穿插真实细节（某次工单多久回、哪步折腾了很久、半夜会不会担心掉线）。\n\
         直接输出 markdown 正文，第一行是 “# 标题”。不要任何解释。"
    )
}

/// 检测"AI 腔"信号词/句式。命中不直接判死（LLM 偶尔误触），但累计过多则重试，
/// 因为平台机审/编辑正是靠这类模板痕迹判定"非真人"——这是本次被删的真病根。
const AI_TELLS: &[&str] = &[
    "总而言之", "综上所述", "首先，", "其次，", "最后，", "值得一提的是",
    "不难发现", "赋能", "一站式", "轻松搞定", "海量",
    "无论是", "为你提供", "值得信赖", "性价比极高",
];

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
    // AI 腔：命中 >=3 处判为"太像机器文"，回炉
    let tells = AI_TELLS.iter().filter(|t| text.contains(**t)).count();
    if tells >= 3 {
        problems.push(format!("AI 模板腔过重（命中 {tells} 处套话），改写成更松散真实的第一人称"));
    }
    // 小标题过密 = 模板结构信号；正文里 "## " 出现 >=4 次视为工整过头
    if text.matches("\n## ").count() >= 4 {
        problems.push("小标题过多，结构太模板化，减少或去掉部分小标题".into());
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
    let mut retry_feedback: Vec<String> = vec![]; // 历轮问题清单（不带旧正文，避免与新人设事实冲突）
    // 人设种子打乱后按轮取用：每轮（含重试）换一套具体项目+数字，
    // 重试才是真的换内容，而不是拿同一批事实逼模型换个说法
    let mut personas: Vec<&(&str, &str)> = PERSONA_SEEDS.iter().collect();
    personas.shuffle(&mut rng);

    for (attempt, _ignored) in (0..llm.max_retries).enumerate() {
        let angle = llm
            .angles
            .choose(&mut rng)
            .map(String::as_str)
            .unwrap_or("写一次通用的使用体验");
        let length = llm.lengths.choose(&mut rng).copied().unwrap_or(400);
        let persona = personas[attempt % personas.len()];

        let user = user_prompt(angle, length, vendor, &required, &llm.forbidden_words, *persona);
        let mut messages = vec![
            json!({"role": "system", "content": system_prompt()}),
            json!({"role": "user", "content": user}),
        ];
        // 上一版被判为 AI 腔/不合规——只追加"写作约束"，不塞旧正文（旧人设会串味）
        if !retry_feedback.is_empty() {
            let all = retry_feedback.join("；");
            messages.push(json!({"role": "user", "content": format!("上一版被判定不合格，问题：{all}。这次务必规避：结构更松散、详略不均、至少一个真实缺点，不要套话，重写一篇。")}));
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
            bail!("LLM HTTP {status}: {}", crate::http::truncate_chars(&body, 300));
        }

        let text = serde_json::from_str::<serde_json::Value>(&body)
            .context("LLM 响应 JSON 解析失败")?
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .map(str::to_string)
            .context("LLM 响应缺少 content")?;

        let mut problems = validate(&text, vendor, &required, &llm.forbidden_words);
        if !text.starts_with('#') {
            // 无标题的文章审核通过率极低，等同不合规，重试
            problems.push("第一行不是 # 标题".into());
        }
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

        // 累积问题清单喂回下一轮（只记问题，不记旧正文，避免人设串味）
        retry_feedback.push(problems.join("；"));
        retry_feedback.dedup();
        if retry_feedback.len() > 3 {
            retry_feedback.remove(0);
        }
    }

    bail!("生成文章 {} 次仍不合规，放弃本次（宁缺毋滥，不做垃圾提交）", llm.max_retries)
}
