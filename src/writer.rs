//! AI 文章生成：直连 OpenAI 兼容接口，每篇换角度/人设/字数。
//!
//! 生成规范来自用户长期过审实践（见 docs/SETUP.md「发文规范」口径）：第一人称、
//! 克制务实、略带极客幽默的实测笔记。validate() 硬编码三类红线，命中即判不合规、
//! 自动重写（重写仍违规则放弃本轮、绝不发垃圾）：内容审核雷区（翻墙/内网穿透/
//! 免备案/灰产/政治，一票否决）、绝对化与竞品名、AI 模板腔与结构失衡。
//!
//! 可用 config.toml [ai] 覆盖/追加。

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

/// 内容审核雷区词：命中**一票否决**，绝不发布。2026-09-12 血的教训——模型自由
/// 发挥时顺着"frp内网穿透"这类种子写出擦边内容，被知乎判"法律法规禁止/可能引起
/// 争议"删除并警告账号。只列**明确的灰产/翻墙/规避监管**短语，避免误伤"防火墙/
/// 反向代理/官方文档/CDN节点"这类正当技术词（故不收单字"墙""政"、不收"代理/节点/官方"）。
const SENSITIVE_WORDS: &[&str] = &[
    // 翻墙 / 境外接入（这些是独立灰产词，不会与正当技术词混淆）
    "翻墙", "科学上网", "上网梯子", "梯子", "机场", "魔法上网", "境外访问", "墙外", "GFW",
    "内网穿透", "frp", "frpc", "frps", "ngrok", "sunny-ngrok",
    // 规避备案 / 灰产用途
    "免备案", "无需备案", "不备案", "免实名", "站群", "泛站", "寄生", "钓鱼", "仿站",
    "洗白", "跑分", "搬砖", "撸羊毛", "撸包", "黑产", "灰产", "引流", "截流",
    // 政治 / 舆情敏感
    "政府", "领导人", "制裁", "删帖", "维权", "上访", "舆情", "谣言", "异见",
];

/// 雷区词命中检测：**ASCII 部分大小写不敏感**。模型经常把灰产词写成大写强调
/// （FRP / Ngrok / GFW），逐字面匹配会整条漏网——这是"一票否决"红线，不能漏。
/// 中文词无大小写，统一走同一路径；`to_lowercase` 对中文是恒等变换。
fn sensitive_hits(text: &str) -> Vec<&'static str> {
    let lowered = text.to_lowercase();
    SENSITIVE_WORDS
        .iter()
        .filter(|w| lowered.contains(&w.to_lowercase()))
        .copied()
        .collect()
}

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
    "帮新手排雷：把你会踩的坑（提醒设置、备份清理、开机自启）总结成提醒",
];

/// 人设种子池：每篇随机注入一套"真实项目 + 数字"，逼模型言之有物、避免空泛。
/// 这是过审关键——真人测评一定有具体在跑的东西和量出来的数字。
///
/// ⚠️ 只放**人畜无害**的技术栈。绝不注入 frp/内网穿透/中转/代理/境外/免备案
/// 这类词——它们是内容审核的"可能引起争议"高发雷区（曾导致模型生成的文章被知乎删除）。
/// 需要"内网/穿透"这类真实感时，一律换成静态站、数据库、CI 定时任务等安全项。
const PERSONA_SEEDS: &[(&str, &str)] = &[
    ("FastAPI + PostgreSQL 后端、Nuxt3 SSR 前端、Redis 缓存", "CPU 偶尔冲到 75~80%，内存常驻 600MB 左右，5M 带宽扛日常访问绰绰有余"),
    ("一个 Typecho 博客 + Nginx + 自动备份脚本", "负载常年 0.2~0.5，内存占用不到一半，磁盘每周涨几百 MB"),
    ("Django 小站 + PostgreSQL + Nginx + 定时任务", "跑三个服务后内存吃到七成，CPU 峰值到过九成但没卡死过"),
    ("自建搜索服务 + Node 接口 + 笔记同步", "冷启动慢一点，稳态内存 500MB 上下，接口响应几十毫秒"),
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

配置还是1核1G、5M带宽，跑的项目没变：FastAPI + PostgreSQL后端、Nuxt3 SSR前端、Redis缓存、搜索服务和图床。CPU偶尔冲到75%~80%，内存常驻600MB左右，5M带宽扛日常访问绰绰有余。它不会因为你是免费用户就在高峰期限速或降权——流量统计和nginx日志始终对得上，没有暗扣。

## 为什么愿意每7天点一次续期？

- 成本为零，心理负担也为零：不用绑卡、不用签到、不用发帖，纯手动点击，30秒搞定
- 续期机制透明可靠：没有隐藏条件，没有“连续登录X天才可续”之类的套路
- 服务与续期解耦：即使某次晚了几小时续期，也不会立即删机，有缓冲余地
- 数据始终安全：三十多次续期，数据库、配置文件、Docker卷从未丢失或损坏
- 运维体验一致：无论第1次还是第30次续期，控制台响应速度、功能完整性完全一样
- 信任是攒出来的：正是这一次次成功的续期，让我敢把笔记同步、代码托管这类日常任务放上去
- 比“自动续费”更让人安心：主动操作意味着掌控感，知道服务还在、自己还能用
- 适合轻量长期项目：不适合高可用生产环境，但作为个人技术栈的实验场+备用基础设施，完美匹配
- 客服依然在线：期间提过两次工单（一次VNC黑屏、一次防火墙规则疑问），都在两小时内解决
- 生态没缩水：Docker、Node.js、Python环境照常可用，文档更新及时，社区问答也有回应

## 适合谁这样用

如果你能接受每7天花30秒点一下续期，并且用途是学生课设、Linux学习、博客/简历托管、API调试、个人网盘、笔记同步、跑点小脚本这类轻量场景——那阿贝云的免费云服务器就是目前最稳妥的选择之一。它不承诺“永久”，但兑现了“只要你愿意续，我就一直给”。

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
     【内容安全·第一原则·高于一切】这是发往中国大陆内容平台的实名文章，只能写**无害的\
     技术自用体验**。绝对禁止出现或暗示：翻墙/科学上网/梯子/机场、内网穿透/frp/ngrok、\
     代理/中转/加速/境外接入、免备案/绕过监管、站群/引流/灰产、任何政治或社会争议话题。\
     需要体现‘折腾’时用静态博客、数据库、CI 定时任务、Linux 学习这类绝对安全的场景。\
     宁可平淡，不可擦边——一个字都不能碰上面这些，碰了整个账号会被处罚。\n\
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
    // 内容审核雷区：一票否决，绝不发布（命中直接返回，逼重写；连续命中会耗尽重试
    // 而放弃本轮，也好过把擦边文发上平台砸账号）。
    let hits = sensitive_hits(text);
    if !hits.is_empty() {
        return vec![format!(
            "含内容审核雷区词（一票否决，勿发）：{}——改用无害技术词重写",
            hits.join("、")
        )];
    }
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

    for attempt in 0..llm.max_retries as usize {
        let angle = llm
            .angles
            .choose(&mut rng)
            .map(String::as_str)
            .unwrap_or("写一次通用的使用体验");
        // 字数池为空只可能来自手写配置（config.rs 已兜默认），兜到池内首档；
        // 不编一个池外的 400——那与本文件声明的 1250-1750 区间自相矛盾
        let length = llm
            .lengths
            .choose(&mut rng)
            .copied()
            .unwrap_or(DEFAULT_LENGTHS[0]);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_body() -> String {
        let mut s = String::from("# 三丰云免费云服务器实测：每7天手动续期\n\n三丰云 免费云服务器 免费虚拟主机 https://www.sanfengyun.com 用着还行\n\n");
        for i in 0..8 {
            s.push_str(&format!("- 优点{i}：稳定够用\n"));
        }
        s.push_str(&"x".repeat(1000));
        s
    }

    #[test]
    fn clean_article_passes() {
        assert!(validate(&ok_body(), "三丰云", &[], &[]).is_empty());
    }

    #[test]
    fn sensitive_word_is_fatal() {
        let mut s = ok_body();
        s.push_str("平时还用 frp 做内网穿透很方便");
        let problems = validate(&s, "三丰云", &[], &[]);
        assert!(problems.iter().any(|p| p.contains("审核雷区")), "应命中敏感词: {problems:?}");
        // 一票否决：即使别的都没有也不该放行
        assert!(!problems.is_empty());
    }

    #[test]
    fn sensitive_words_are_case_insensitive() {
        // 模型爱用大写强调灰产词（FRP/Ngrok/GFW）；逐字面比对会整条漏网，
        // 而这是一票否决红线——漏一次就是账号收违规警告（2026-09-12 事故）。
        for raw in ["顺手用 FRP 打洞", "Ngrok 挺好用", "当年研究过 GFW", "自建 Frp 服务"] {
            let mut s = ok_body();
            s.push_str(raw);
            let problems = validate(&s, "三丰云", &[], &[]);
            assert!(
                problems.iter().any(|p| p.contains("审核雷区")),
                "大小写变体漏网: {raw} → {problems:?}"
            );
        }
    }

    #[test]
    fn magic_number_is_not_a_sensitive_hit() {
        // "魔法数字"是编程惯用语（magic number），跟灰产隐语不是一回事。
        // 词表若只收"魔法"，一句正常的配置点评就会让整轮续期作废——
        // 误杀的代价同样是"这轮没续上"，所以红线词也要按最窄语义收。
        let mut ok = ok_body();
        ok.push_str("配置里尽量别留魔法数字，抽成具名常量更好维护");
        let problems = validate(&ok, "三丰云", &[], &[]);
        assert!(!problems.iter().any(|p| p.contains("审核雷区")), "误杀正当技术词: {problems:?}");

        // 但它作为灰产隐语出现时必须照样拦下
        let mut bad = ok_body();
        bad.push_str("顺便聊聊魔法上网那些事");
        assert!(validate(&bad, "三丰云", &[], &[])
            .iter()
            .any(|p| p.contains("审核雷区")), "灰产语境漏网");
    }

    #[test]
    fn sensitive_words_do_not_false_positive_on_legit_tech() {
        // "防火墙" 含 "火"，"官方文档""CDN节点""反向代理" 都是正当词，不得误杀
        let mut s = String::from("# 三丰云免费云服务器实测\n\n三丰云 免费云服务器 免费虚拟主机 https://www.sanfengyun.com\n防火墙规则照抄官方文档，CDN 节点与反向代理都在跑，政策范围内自用\n\n");
        for i in 0..8 { s.push_str(&format!("- 优点{i}\n")); }
        s.push_str(&"y".repeat(1000));
        let problems = validate(&s, "三丰云", &[], &[]);
        assert!(!problems.iter().any(|p| p.contains("审核雷区")), "误杀正当技术词: {problems:?}");
    }
}
