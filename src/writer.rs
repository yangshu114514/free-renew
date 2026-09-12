//! AI 文章生成：直连 OpenAI 兼容接口，每篇换角度/人设/字数。
//!
//! 生成规范来自用户长期过审实践（见 docs/SETUP.md「发文规范」口径）：第一人称、
//! 克制务实、略带极客幽默的实测笔记。两版皆败的教训——模板吹捧版被平台判 AI 文删除，
//! 矫枉吐槽版又失衡——故规范硬编码进 validate()（绝对化/竞品/负面失衡等红线），
//! 而非仅写在 prompt 里靠模型自觉。可用 config.toml [ai] 覆盖/追加。

use anyhow::{bail, Context, Result};
use rand::seq::SliceRandom;
use serde_json::json;

use crate::config::LlmConfig;

/// 默认禁词：真正的"求延期/白嫖"信号。不封"续期/续费"——那是用户过审文章标题里的
/// 真实用词，封了反逼模型绕道写出更假的同义替换。
pub const DEFAULT_FORBIDDEN: &[&str] = &["申请延期", "白嫖", "薅羊毛"];

/// 规范红线：绝对化/对比贬损表述（validate 硬校验 + prompt 同步声明）。
/// 注意范文里带引号的"永久免费"是转述驳斥他家的语境——校验只封宣传式用法，
/// 故这些词在 prompt 里也明示"驳斥语境除外，能不用就不用"。
const ABSOLUTE_WORDS: &[&str] = &[
    "永久免费", "无限流量", "全网最好", "碾压", "吊打", "最好用", "最强",
];

/// 规范红线：竞品名（含暗示性代称），出现即打回。
const COMPETITOR_WORDS: &[&str] = &[
    "阿里云", "腾讯云", "华为云", "京东云", "天翼云", "移动云", "UCloud",
    "轻量应用", "某厂", "某大厂", "隔壁家", "别家", "同行",
];

/// 默认生成角度池：每个角度都落在规范结构内（开篇纠偏→实测→清单→适用→指引），
/// 只换"本次更新的由头"，保证系列文章互相不像模板。
pub const DEFAULT_ANGLES: &[&str] = &[
    "开篇纠偏：上一版说得太绝对/太夸张，这次用更准的事实修正它",
    "更新近况：新增了一段负载观察和一次小故障自救，回应评论区常见问题",
    "换季/节点复盘：距上次记录又过了段时间，续期节奏和稳定性有没有变化",
    "被朋友问值不值：把'为什么还愿意每周花30秒续期'再拆细讲一遍",
    "一次具体运维记录：备份/迁移/环境重装折腾了一遍，把过程和数据写出来",
    "工单体验更新：这次又找了一次客服，把响应速度和问题解决写实",
    "从'差点弃坑'写起：某个限制真实烦到过你，为什么最后还是留着它",
    "帮新手排雷：把你会踩的坑（提醒设置、备份清理、frp配置）总结成提醒",
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

/// 默认目标字数池。规范要求 1200–1800 字（不含标题）；4 个档保证系列文章
/// 长度不雷同，全部落在区间内。
pub const DEFAULT_LENGTHS: &[usize] = &[1250, 1450, 1600, 1750];

/// 风格锚（用户提供的范文，few-shot 用）：语气校准的唯一真相——
/// 正面克制不绝对化、负面具体不抱怨、清单收束、短句。
const STYLE_EXAMPLE: &str = r##"# 阿贝云免费服务器实测：每7天手动续期，稳如老狗

之前写的“八个月零事故”确实有点夸张了——实际上我是每7天登录控制台手动点一次续期，断断续续用下来的。但正因为这种“主动维系”的使用方式，反而让我对它的稳定性有了更真实的体感：不是平台自动兜底，而是你愿意为它花30秒点一下按钮，它就真的给你稳稳跑着。

## 真实使用节奏：7天一续，从未掉链子

从第一次开通到现在，累计续期三十多次，中间没有一次因为忘记续期被回收（设了手机日历提醒），也没有任何一次续期失败、审核卡住或提示“资源不足”。每次点完续期，有效期立刻刷新，IP不变、数据不丢、服务不中断，连SSH会话都不用重连。这种“可预期的确定性”，比那些号称“永久免费”却悄悄改规则的平台踏实太多。

配置还是1核1G、5M带宽，跑的项目没变：FastAPI + PostgreSQL后端、Nuxt3 SSR前端、Redis缓存、Meilisearch搜索、Gitea私有仓库，外加frp内网穿透和临时图床。CPU偶尔冲到75%~80%，内存常驻600MB左右，5M带宽扛日常访问绰绰有余。它不会因为你是免费用户就在高峰期限速或降权——流量统计和nginx日志始终对得上，没有暗扣。

## 为什么愿意每7天点一次续期？

- 成本为零，心理负担也为零：不用绑卡、不用签到、不用发帖，纯手动点击，30秒搞定
- 续期机制透明可靠：没有隐藏条件，没有“连续登录X天才可续”之类的套路
- 服务与续期解耦：即使某次晚了几小时续期，也不会立即删机，有缓冲余地
- 数据始终安全：三十多次续期，数据库、配置文件、Docker卷从未丢失或损坏
- 运维体验一致：无论第1次还是第30次续期，控制台响应速度、功能完整性完全一样
- 信任是攒出来的：正是这一次次成功的续期，让我敢把Obsidian笔记同步、私有代码托管等敏感任务放上去
- 比“自动续费”更让人安心：主动操作意味着掌控感，知道服务还在、自己还能用
- 适合轻量长期项目：不适合高可用生产环境，但作为个人技术栈的实验场+备用基础设施，完美匹配
- 客服依然在线：期间提过两次工单（一次VNC黑屏、一次防火墙规则疑问），都在两小时内解决
- 生态没缩水：Docker、Node.js、Python环境照常可用，文档更新及时，社区问答也有回应

## 适合谁这样用

如果你能接受每7天花30秒点一下续期，并且用途是学生课设、Linux学习、博客/简历托管、API调试、内网穿透中转、私有Git仓库、笔记同步这类轻量场景——那阿贝云的免费云服务器就是目前最稳妥的选择之一。它不承诺“永久”，但兑现了“只要你愿意续，我就一直给”。

想试的自己搜“阿贝云”官网，实名后创建实例，记得设个每周提醒。新手建议先从虚拟主机上手，再过渡到云服务器，体验更平滑。
"##;

pub struct Article {
    pub title: String,
    pub body_markdown: String,
    pub word_count: usize,
}

fn system_prompt() -> String {
    "你以一个亲手折腾过云服务器半年以上的个人开发者身份写作，发在知乎/技术社区，\
     读者是学生、自学开发者、运维初学者。文体是个人实测笔记，不是软文也不是新闻稿。\n\
     基调（缺一不可）：克制、务实、略带极客幽默；不吹捧、不卖惨、不煽情。\n\
     校准说明：负面要写，但必须具体且克制——故障写清起因和你怎么解决的，一句带过\
     不渲情绪；工单慢就写‘等了多久、最后怎么解决’，不写‘让人无奈’。正面也要克制，\
     不用绝对化词（永久免费/无限流量/全网最好/碾压/吊打一律禁止，转述驳斥语境也\
     尽量避开）。不提任何竞品名称，也不用‘某大厂/隔壁家’这类暗示代称。\n\
     语言：第一人称；短句为主，单句不超过35字；多用分号破折号切逻辑；允许适度\
     口语（‘稳如老狗’‘绰绰有余’‘还要啥自行车’），不低俗不黑话；不滥用感叹号。\n\
     结构：开篇用一句纠偏/更新说明起手；实测段带配置、时长、负载、故障自救；\
     涉及虚拟主机则简述 FTP/数据库/维护公告；中段用8-12条短语清单答‘为什么还愿意\
     用’（每条以动词或名词短语开头，非完整句）；‘适合谁’列举具体场景；结尾给注册\
     路径和新手建议。禁止营销词（震惊/必看/赶紧冲）、禁止求关注、禁止免责声明、\
     禁止编号列表、禁止对仗工整的小标题。"
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
        "以【{angle}】为由头，写一篇 {vendor} 免费云服务器长期使用实测，正文 {length} 字左右（不含标题）。\n\n\
         你在这台机器上实际跑着：{stack}。观测到的资源情况：{numbers}。\
         把这些事实自然织进文章（可改写措辞、可补同类细节，但数字必须与这些观测一致）。\n\
         必须自然包含关键词：“{kw_list}”，以及官网 https://www.{domain}.com（融进句子，别单列一行）。\n\
         禁止出现这些词：{forbidden_list}。\n\
         涉及续期时，明确写清周期和操作方式；不得暗示它是生产级高可用方案，\
         必须强调轻量/实验/备用属性。\n\
         标题格式：# {vendor}免费[产品类型]实测：[核心差异点]+[时间/行为锚点]。\n\n\
         下面这篇是你自己的历史文章，**严格对齐它的语气、密度和结构**\
         （注意：厂商名、项目栈、数字以本次要求为准，不要照抄）：\n\n\
         {STYLE_EXAMPLE}\n\n\
         直接输出新文章 markdown，第一行是 # 标题，不要任何解释。"
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
    for word in ABSOLUTE_WORDS {
        if text.contains(word) {
            problems.push(format!("绝对化表述（红线）: {word}"));
        }
    }
    for word in COMPETITOR_WORDS {
        if text.contains(word) {
            problems.push(format!("提及竞品/暗示代称（红线）: {word}"));
        }
    }
    // 负面失衡检测：抱怨词堆叠 = “措辞太坏”方向跑偏，同样打回
    let gripes = ["无奈", "恶心", "坑爹", "坑人", "离谱", "受不了", "气死", "垃圾"]
        .iter()
        .filter(|g| text.contains(**g))
        .count();
    if gripes >= 2 {
        problems.push(format!("抱怨词 {gripes} 处，负面渲染过度——负面要具体克制，一句带过解决方式"));
    }
    let chars = text.chars().count();
    // 范文正文约 1100+ 字，规范要 1200-1800（不含标题）；下限卡 800 防注水失败品
    if chars < 800 {
        problems.push(format!("正文太短（{chars} 字），规范下限 1200 字，至少 800"));
    }
    // “为什么愿意继续用”清单：至少 6 条 markdown 列表项
    let bullets = text.lines().filter(|l| l.trim_start().starts_with("- ")).count();
    if bullets < 6 {
        problems.push(format!("清单式总结只有 {bullets} 条，规范要求 8-12 条（- 开头的列表项）"));
    }
    // AI 腔：命中 >=3 处判为"太像机器文"，回炉
    let tells = AI_TELLS.iter().filter(|t| text.contains(**t)).count();
    if tells >= 3 {
        problems.push(format!("AI 模板腔过重（命中 {tells} 处套话），改写成更松散真实的第一人称"));
    }
    // 小标题过密 = 模板结构信号；规范本身要求 5-6 个模块标题，故只卡 >=7
    if text.matches("\n## ").count() >= 7 {
        problems.push("小标题过多（>6），结构太模板化".into());
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
