//! AI 文章生成：直连 OpenAI 兼容接口，每篇换角度/人设/字数。
//!
//! 生成规范来自用户长期过审实践（见 docs/SETUP.md「发文规范」口径）：第一人称、
//! 克制务实、略带极客幽默的实测笔记。validate() 硬编码三类红线，命中即判不合规、
//! 自动重写（重写仍违规则放弃本轮、绝不发垃圾）：内容审核雷区（翻墙/内网穿透/
//! 免备案/灰产/政治，一票否决）、绝对化与竞品名、AI 模板腔与结构失衡。
//!
//! 厂商相关的一切（名字、官网域名、别家名单）都从 `config::CloudProfile` 取，
//! 本模块**不认识**任何厂商中文字符串——新增第三家厂商不用改这里一行代码。
//!
//! 可用 config.toml [ai] 覆盖/追加。

use std::time::Duration;

use anyhow::{bail, Context, Result};
use rand::seq::SliceRandom;
use serde_json::json;

use crate::config::{other_vendors, CloudProfile, LlmConfig};

/// 生成请求超时默认值（秒）。一次出 1500 字比厂商接口慢得多，故与 http_timeout
/// 不同源；用 `LLM_TIMEOUT` 环境变量可覆盖。
pub const LLM_REQUEST_TIMEOUT_SECS: u64 = 120;

/// 默认禁词：真正的"求延期/白嫖"信号。不封"续期/续费"——那是用户过审文章标题里的
/// 真实用词，封了反逼模型绕道写出更假的同义替换。
const DEFAULT_FORBIDDEN: &[&str] = &["申请延期", "白嫖", "薅羊毛"];

/// 默认必含关键词（厂商名之外的通用词）。空配置时由 config 层兜这里，
/// 生成端不再单独兜一份（两处兜底语义必然分裂）。
const DEFAULT_KEYWORDS: &[&str] = &["免费云服务器", "免费虚拟主机"];

/// 默认必含关键词表（config 层兜底用）。
pub fn default_keywords() -> Vec<String> {
    DEFAULT_KEYWORDS.iter().map(|s| s.to_string()).collect()
}

/// 规范红线：绝对化/对比贬损表述（validate 硬校验 + prompt 同步声明）。
/// 注意范文里带引号的"永久免费"是转述驳斥他家的语境——校验只封宣传式用法，
/// 故这些词在 prompt 里也明示"驳斥语境除外，能不用就不用"。
const ABSOLUTE_WORDS: &[&str] = &[
    "永久免费",
    "无限流量",
    "全网最好",
    "碾压",
    "吊打",
    "最好用",
    "最强",
];

/// 规范红线：竞品名（含暗示性代称），出现即打回。
const COMPETITOR_WORDS: &[&str] = &[
    "阿里云",
    "腾讯云",
    "华为云",
    "京东云",
    "天翼云",
    "移动云",
    "UCloud",
    "轻量应用",
    "某厂",
    "某大厂",
    "隔壁家",
    "别家",
    "同行",
];

/// 负面渲泄词：堆叠 = "措辞太坏"方向跑偏，同样打回。
const GRIPE_WORDS: &[&str] = &[
    "无奈",
    "恶心",
    "坑爹",
    "坑人",
    "离谱",
    "受不了",
    "气死",
    "垃圾",
];

/// 内容审核雷区词：命中**一票否决**，绝不发布。2026-09-12 血的教训——模型自由
/// 发挥时顺着"frp内网穿透"这类种子写出擦边内容，被知乎判"法律法规禁止/可能引起
/// 争议"删除并警告账号。只列**明确的灰产/翻墙/规避监管**短语，避免误伤"防火墙/
/// 反向代理/官方文档/CDN节点"这类正当技术词（故不收单字"墙""政"、不收"代理/节点/官方"）。
const SENSITIVE_WORDS: &[&str] = &[
    // 翻墙 / 境外接入（这些是独立灰产词，不会与正当技术词混淆）
    "翻墙",
    "科学上网",
    "上网梯子",
    "梯子",
    "机场",
    "魔法上网",
    "境外访问",
    "墙外",
    "GFW",
    "内网穿透",
    "frp",
    "frpc",
    "frps",
    "ngrok",
    "sunny-ngrok",
    // 规避备案 / 灰产用途
    "免备案",
    "无需备案",
    "不备案",
    "免实名",
    "站群",
    "泛站",
    "寄生",
    "钓鱼",
    "仿站",
    "洗白",
    "跑分",
    "搬砖",
    "撸羊毛",
    "撸包",
    "黑产",
    "灰产",
    "引流",
    "截流",
    // 政治 / 舆情敏感
    "政府",
    "领导人",
    "制裁",
    "删帖",
    "维权",
    "上访",
    "舆情",
    "谣言",
    "异见",
];

/// 词表匹配统一入口：**ASCII 部分大小写不敏感**。
///
/// 模型经常把灰产词写成大写强调（FRP / Ngrok / GFW），逐字面匹配会整条漏网——
/// 这是"一票否决"红线，不能漏。此前只有敏感词做了 to_lowercase，禁词/绝对化/竞品
/// 各用一套逐字面 contains：同一个文件两套语义，改一处必漏另一处，故统一到这里。
/// 调用方传入的 `lowered` 是全文小写化结果（中文无大小写，to_lowercase 恒等）。
fn contains_ci(lowered: &str, needle: &str) -> bool {
    if needle.is_ascii() {
        lowered.contains(&needle.to_ascii_lowercase())
    } else {
        lowered.contains(needle)
    }
}

/// 默认生成角度池：每个角度都落在规范结构内（开篇纠偏→实测→清单→适用→指引），
/// 只换"本次更新的由头"，保证系列文章互相不像模板。
const DEFAULT_ANGLES: &[&str] = &[
    "开篇纠偏：上一版说得太绝对/太夸张，这次用更准的事实修正它",
    "更新近况：新增了一段负载观察和一次小故障自救，回应评论区常见问题",
    "换季/节点复盘：距上次记录又过了段时间，续期节奏和稳定性有没有变化",
    "被朋友问值不值：把'为什么还愿意每周花30秒续期'再拆细讲一遍",
    "一次具体运维记录：备份/迁移/环境重装折腾了一遍，把过程和数据写出来",
    "工单体验更新：这次又找了一次客服，把响应速度和问题解决写实",
    "从'差点弃坑'写起：某个限制真实烦到过你，为什么最后还是留着它",
    "帮新手排雷：把你会踩的坑（提醒设置、备份清理、开机自启）总结成提醒",
];

/// 默认角度池（config 层兜底用）。
pub fn default_angles() -> Vec<String> {
    DEFAULT_ANGLES.iter().map(|s| s.to_string()).collect()
}

/// 默认禁词表（config 层兜底用）。
pub fn default_forbidden() -> Vec<String> {
    DEFAULT_FORBIDDEN.iter().map(|s| s.to_string()).collect()
}

/// 人设种子池：每篇随机注入一套"真实项目 + 数字"，逼模型言之有物、避免空泛。
/// 这是过审关键——真人测评一定有具体在跑的东西和量出来的数字。
///
/// ⚠️ 只放**人畜无害**的技术栈。绝不注入 frp/内网穿透/中转/代理/境外/免备案
/// 这类词——它们是内容审核的"可能引起争议"高发雷区（曾导致模型生成的文章被知乎删除）。
/// 需要"内网/穿透"这类真实感时，一律换成静态站、数据库、CI 定时任务等安全项。
const PERSONA_SEEDS: &[(&str, &str)] = &[
    (
        "FastAPI + PostgreSQL 后端、Nuxt3 SSR 前端、Redis 缓存",
        "CPU 偶尔冲到 75~80%，内存常驻 600MB 左右，5M 带宽扛日常访问绰绰有余",
    ),
    (
        "一个 Typecho 博客 + Nginx + 自动备份脚本",
        "负载常年 0.2~0.5，内存占用不到一半，磁盘每周涨几百 MB",
    ),
    (
        "Django 小站 + PostgreSQL + Nginx + 定时任务",
        "跑三个服务后内存吃到七成，CPU 峰值到过九成但没卡死过",
    ),
    (
        "自建搜索服务 + Node 接口 + 笔记同步",
        "冷启动慢一点，稳态内存 500MB 上下，接口响应几十毫秒",
    ),
    (
        "一个 Rust axum 服务 + SQLite + Caddy 自动 HTTPS",
        "编译时 CPU 会打满几分钟，平时内存占用很低，几百 MB 就够",
    ),
    (
        "Python 数据分析 notebook + JupyterHub + 定时拉数脚本",
        "跑大任务时内存吃紧要加 swap，日常挂小任务很稳",
    ),
];

/// 默认目标字数池。规范要求 1200–1800 字（不含标题）；4 个档保证系列文章
/// 长度不雷同，全部落在区间内。
pub const DEFAULT_LENGTHS: &[usize] = &[1250, 1450, 1600, 1750];

/// 正文下限 = 字数池最小档的 6/10。
/// 门禁与"规范下限 1200"曾各写一个数（提示语说 1200、代码只卡 800），配
/// `lengths = [3000]` 也照样放行；现在从配置的字数池派生，两边不可能再漂移。
const MIN_LENGTH_NUM: usize = 6;
const MIN_LENGTH_DEN: usize = 10;

/// "为什么还愿意用"清单的硬性最少条数（规范要求 8-12 条，留出余量只卡 6）。
const MIN_BULLETS: usize = 6;

/// AI 腔命中阈值：达到即回炉。
const MAX_AI_TELLS: usize = 3;

/// 小标题数量上限（规范本身要求 5-6 个模块标题，故只卡 >=7）。
const MAX_HEADINGS: usize = 6;

/// 喂回下一轮的问题清单最多保留几条。
const MAX_RETRY_FEEDBACK: usize = 3;

/// 范文里代表厂商名的占位符。
const VENDOR_PLACEHOLDER: &str = "{VENDOR}";

/// 风格锚（用户提供的范文，few-shot 用）：语气校准的唯一真相——
/// 正面克制不绝对化、负面具体不抱怨、清单收束、短句。
///
/// 厂商名写成占位符：范文原本通篇是"阿贝云"，给三丰云生成时注入的范文里满是别家
/// 名字，模型极易照抄——这正是"串厂商必被拒"那条校验要事后补救的根因。模板化后
/// 防线从"事后拦"前移到"不产生"。
const STYLE_EXAMPLE: &str = r##"# {VENDOR}免费服务器实测：每7天手动续期，稳如老狗

之前写的“八个月零事故”确实有点夸张了——实际上我是每7天登录控制台手动点一次续期，断断续续用下来的。但正因为这种“主动维系”的使用方式，反而让我对它的稳定性有了更真实的体感：不是平台自动兜底，而是你愿意为它花30秒点一下按钮，它就真的给你稳稳跑着。

## 真实使用节奏：7天一续，从未掉链子

从第一次开通到现在，累计续期三十多次，中间没有一次因为忘记续期被回收（设了手机日历提醒），也没有任何一次续期失败、审核卡住或提示“资源不足”。每次点完续期，有效期立刻刷新，IP不变、数据不丢、服务不中断，连SSH会话都不用重连。这种“可预期的确定性”，比那些号称“随时可能改规则”的平台踏实太多。

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

如果你能接受每7天花30秒点一下续期，并且用途是学生课设、Linux学习、博客/简历托管、API调试、个人网盘、笔记同步、跑点小脚本这类轻量场景——那{VENDOR}的免费云服务器就是目前最稳妥的选择之一。它不承诺“永久”，但兑现了“只要你愿意续，我就一直给”。

想试的自己搜“{VENDOR}”官网，实名后创建实例，记得设个每周提醒。新手建议先从虚拟主机上手，再过渡到云服务器，体验更平滑。
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
     口语（‘稳如老狗’‘绰绰有余’‘还要啥自行车’），不低俗不黑话；不滥用感叹号。\
     结构：开篇用一句纠偏/更新说明起手；实测段带配置、时长、负载、故障自救；\
     涉及虚拟主机则简述 FTP/数据库/维护公告；中段用8-12条短语清单答‘为什么还愿意\
     用’（每条以动词或名词短语开头，非完整句）；‘适合谁’列举具体场景；结尾给注册\
     路径和新手建议。禁止营销词（震惊/必看/赶紧冲）、禁止求关注、禁止免责声明、\
     禁止编号列表、禁止对仗工整的小标题。"
        .to_string()
}

/// 把范文按当前厂商渲染（唯一的厂商相关替换点）。
fn style_example_for(profile: &CloudProfile) -> String {
    STYLE_EXAMPLE.replace(VENDOR_PLACEHOLDER, profile.name)
}

fn user_prompt(
    angle: &str,
    length: usize,
    profile: &CloudProfile,
    required: &[String],
    forbidden: &[String],
    persona: (&str, &str),
) -> String {
    let vendor = profile.name;
    let (stack, numbers) = persona;
    // 关键词为空时整句跳过：原来的 join 会生成一对空引号 “” 塞进提示词。
    //
    // 收尾必须是句号换行——此前这里以「，以及」结尾，是为了接后面那句
    // 「以及官网链接 <域名>」。2026-10-03 撤掉链接要求后连接词就悬空了，
    // 提示词变成「必须自然包含关键词：“免费虚拟主机”，以及正文里不要放任何官网链接…」，
    // 两件事被粘成一句，模型的注意力被后半句带走，连挂三轮漏词。
    // 现在把关键词单独成句并写明"硬性要求"，与 validate 的判罚口径对齐。
    let kw_clause = if required.is_empty() {
        String::new()
    } else {
        format!(
            "正文必须原样出现这些词（硬性要求，少一个整篇作废）：{}。\n",
            required
                .iter()
                .map(|k| format!("“{k}”"))
                .collect::<Vec<_>>()
                .join("、")
        )
    };
    let forbidden_clause = if forbidden.is_empty() {
        String::new()
    } else {
        format!("禁止出现这些词：{}。\n", forbidden.join("、"))
    };
    // ⚠️ 这里**不再要求**文章带官网链接，且明确要求模型别自发补外链。
    // 2026-10-03 决策（覆盖此前"必须含官网域名"的硬要求）：
    //   · 人工审核并不要求文章出现官网链接，不带照样过；
    //   · 正文里的外链会被平台判成推广/引流，**反而抬高封号风险**。
    // 所以"要求加链接"与"校验必须含域名"两头都撤——继续要求加等于在赌一条
    // 纯风险项，而它正是 2026-10-03 三轮生成全废的直接原因。
    format!(
        "以【{angle}】为由头，写一篇 {vendor} 免费云服务器长期使用实测，正文 {length} 字左右（不含标题）。\n\n\
         你在这台机器上实际跑着：{stack}。观测到的资源情况：{numbers}。\
         把这些事实自然织进文章（可改写措辞、可补同类细节，但数字必须与这些观测一致）。\n\
         {kw_clause}正文里不要放任何官网链接或推广性外链——需要引导读者就写“自己搜官网”，\
         不要贴 URL（贴外链会被平台判为推广，有封号风险）。\n\
         {forbidden_clause}\
         涉及续期时，明确写清周期和操作方式；不得暗示它是生产级高可用方案，\
         必须强调轻量/实验/备用属性。\n\
         标题格式：# {vendor}免费[产品类型]实测：[核心差异点]+[时间/行为锚点]。\n\n\
         下面这篇是你自己的历史文章，**严格对齐它的语气、密度和结构**\
         （注意：厂商名、项目栈、数字以本次要求为准，不要照抄）：\n\n\
         {style}\n\n\
         直接输出新文章 markdown，第一行是 # 标题，不要任何解释。",
        style = style_example_for(profile),
    )
}

/// 检测"AI 腔"信号词/句式。命中不直接判死（LLM 偶尔误触），但累计过多则重试，
/// 因为平台机审/编辑正是靠这类模板痕迹判定"非真人"——这是本次被删的真病根。
const AI_TELLS: &[&str] = &[
    "总而言之",
    "综上所述",
    "首先，",
    "其次，",
    "最后，",
    "值得一提的是",
    "不难发现",
    "赋能",
    "一站式",
    "轻松搞定",
    "海量",
    "无论是",
    "为你提供",
    "值得信赖",
    "性价比极高",
];

/// 内容审核雷区：一票否决，绝不发布（命中直接返回，逼重写；连续命中会耗尽重试
/// 而放弃本轮，也好过把擦边文发上平台砸账号）。
fn check_sensitive(lowered: &str) -> Vec<String> {
    let hits: Vec<&str> = SENSITIVE_WORDS
        .iter()
        .filter(|w| contains_ci(lowered, w))
        .copied()
        .collect();
    if hits.is_empty() {
        return vec![];
    }
    vec![format!(
        "含内容审核雷区词（一票否决，勿发）：{}——改用无害技术词重写",
        hits.join("、")
    )]
}

/// 厂商一致性：必须提本家厂商名，且**不能**出现任何别家厂商名或官网域名。
/// 串厂商是"必被拒"级错误——人工审核会把它看成替别家申请的。
fn check_vendor(text: &str, profile: &CloudProfile, lowered: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if !text.contains(profile.name) {
        problems.push(format!("缺少厂商名 {}", profile.name));
    }
    let want = profile.site_domain();
    for other in other_vendors(profile.key) {
        if text.contains(other.name) {
            problems.push(format!(
                "出现另一厂商名「{}」——本次是 {} 的续期文，串厂商必被拒",
                other.name, profile.name
            ));
        }
        // 别家也可能以域名形态出现（范文/模型记忆里的官网链接），一并拦
        let d = other.site_domain();
        if lowered.contains(&d) {
            problems.push(format!("出现别家官网链接 {d}（本家要求 {want}）"));
        }
    }
    problems
}

/// 文章**不得**出现本家官网链接（2026-10-03 决策，与 prompt 中"不要贴 URL"配套）。
///
/// 此前这里是**反过来的**：强制正文必须含本家域名，缺了直接判不合格——那正是
/// 2026-10-03 连废三轮、整轮续期放弃的直接原因。而这条要求本身两头都不成立：
/// · 人工审核并不要求文章带官网链接，不带照样过；
/// · 正文里的外链会被平台判成推广/引流，**反而抬高封号风险**。
/// 所以从"必须含"翻转成"出现即拦"：宁可少一个链接，也不要多一次风控。
///
/// 别家域名由 [`check_vendor`] 另行拦截（防串厂商），此处只管本家。
/// 历史上的 `.com.com` 死链检测已随本条一并撤除——它含在本家域名里，现在会被
/// 这一条直接拦下，无需再单独判重复后缀。
fn check_links(lowered: &str, profile: &CloudProfile) -> Vec<String> {
    let own = profile.site_domain();
    if lowered.contains(&own) {
        return vec![format!(
            "正文出现官网链接 {own}——已决定文章一律不带官网链接\
             （人工审核不要求，而贴外链会被平台判为推广、抬高封号风险）"
        )];
    }
    vec![]
}

fn check_keywords(text: &str, required: &[String]) -> Vec<String> {
    required
        .iter()
        .filter(|kw| !kw.is_empty() && !text.contains(kw.as_str()))
        .map(|kw| format!("缺少关键词 {kw}"))
        .collect()
}

fn check_words(lowered: &str, forbidden: &[String]) -> Vec<String> {
    forbidden
        .iter()
        .filter(|w| !w.is_empty() && contains_ci(lowered, w))
        .map(|w| format!("出现禁止词: {w}"))
        .collect()
}

/// 语气红线：绝对化表述、竞品/暗示代称、负面渲泄堆叠。
fn check_tone(lowered: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for word in ABSOLUTE_WORDS {
        if contains_ci(lowered, word) {
            problems.push(format!("绝对化表述（红线）: {word}"));
        }
    }
    for word in COMPETITOR_WORDS {
        if contains_ci(lowered, word) {
            problems.push(format!("提及竞品/暗示代称（红线）: {word}"));
        }
    }
    let gripes = GRIPE_WORDS
        .iter()
        .filter(|g| contains_ci(lowered, g))
        .count();
    if gripes >= 2 {
        problems.push(format!(
            "抱怨词 {gripes} 处，负面渲染过度——负面要具体克制，一句带过解决方式"
        ));
    }
    problems
}

/// 正文长度下限（由字数池派生，见 MIN_LENGTH_NUM 注释）。
fn min_body_chars(lengths: &[usize]) -> usize {
    let floor = lengths
        .iter()
        .copied()
        .filter(|n| *n > 0)
        .min()
        .unwrap_or(DEFAULT_LENGTHS[0]);
    floor * MIN_LENGTH_NUM / MIN_LENGTH_DEN
}

/// 结构红线：字数、清单条数、AI 腔、小标题密度。
fn check_shape(text: &str, lowered: &str, lengths: &[usize]) -> Vec<String> {
    let mut problems = Vec::new();
    let chars = text.chars().count();
    let floor = min_body_chars(lengths);
    if chars < floor {
        problems.push(format!(
            "正文太短（{chars} 字）：字数池最小档 {floor_target} 字，至少需 {floor} 字",
            floor_target = lengths
                .iter()
                .copied()
                .filter(|n| *n > 0)
                .min()
                .unwrap_or(0),
        ));
    }
    // “为什么愿意继续用”清单：至少 6 条 markdown 列表项
    let bullets = text
        .lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .count();
    if bullets < MIN_BULLETS {
        problems.push(format!(
            "清单式总结只有 {bullets} 条，规范要求 8-12 条（- 开头的列表项）"
        ));
    }
    let tells = AI_TELLS.iter().filter(|t| contains_ci(lowered, t)).count();
    if tells >= MAX_AI_TELLS {
        problems.push(format!(
            "AI 模板腔过重（命中 {tells} 处套话），改写成更松散真实的第一人称"
        ));
    }
    if text.matches("\n## ").count() > MAX_HEADINGS {
        problems.push(format!("小标题过多（>{MAX_HEADINGS}），结构太模板化"));
    }
    problems
}

/// 生成前的机器校验：每条规则各自独立，命中即判不合规、自动重写。
/// 拆成流水线而不是一条长 if 链，是为了"加一条规则"不再需要通读 90 行。
fn validate(
    text: &str,
    profile: &CloudProfile,
    required: &[String],
    forbidden: &[String],
    lengths: &[usize],
) -> Vec<String> {
    // 全文小写化一次，供所有大小写不敏感的词表匹配复用
    let lowered = text.to_lowercase();

    let fatal = check_sensitive(&lowered);
    if !fatal.is_empty() {
        return fatal;
    }

    let mut problems = Vec::new();
    problems.extend(check_vendor(text, profile, &lowered));
    problems.extend(check_keywords(text, required));
    problems.extend(check_links(&lowered, profile));
    problems.extend(check_words(&lowered, forbidden));
    problems.extend(check_tone(&lowered));
    problems.extend(check_shape(text, &lowered, lengths));
    problems
}

/// 拆标题与正文。`None` = 第一行不是合法标题。
///
/// 原来拆不出来时回落到标题"无题"并照常发布——一篇没标题的文章审核通过率极低，
/// 等同于不合规，应该重试而不是硬发。另：只剥一层 `#`，`trim_start_matches('#')`
/// 会把 `### 标题` 的前导 # 全吃掉，层级信息就丢了。
fn split_title_body(text: &str) -> Option<(String, String)> {
    let (first, rest) = text.split_once('\n')?;
    let title = first.strip_prefix('#')?.trim();
    if title.is_empty() {
        return None;
    }
    Some((title.to_string(), rest.trim().to_string()))
}

/// 把上一轮的具体问题翻译成"这次到底该怎么改"。
///
/// 此前这里是一句写死的写作风格建议——不管实际问题是什么都原样附上
/// （"结构更松散、详略不均、至少一个真实缺点，不要套话"）。可校验清单里
/// **大多数条目跟写作风格无关**：缺关键词、串厂商、正文带链接、标题不合法……
/// 这些拿到的处方完全不对症，模型既不知道要补哪个词、也不知道要删哪个链接，
/// 只能原样再赌一轮，而重试次数只有 3。
///
/// 2026-10-03 实测就是这么废掉整轮续期的：三轮全挂在"缺少关键词 免费虚拟主机"，
/// 每轮收到的却都是"结构更松散、不要套话"。问题清单一直是对的，错的是这句处方。
///
/// 返回去重后的处方列表；识别不了的残余问题给一条兜底，绝不返回空。
fn retry_guidance(problems: &[String]) -> Vec<String> {
    fn add(tips: &mut Vec<String>, tip: &str) {
        if !tips.iter().any(|t| t == tip) {
            tips.push(tip.to_string());
        }
    }
    let mut tips: Vec<String> = Vec::new();
    for p in problems {
        if p.contains("缺少关键词") {
            add(
                &mut tips,
                "上一版漏了必须出现的词，这次务必把这些词**原样**写进正文\
                 （不要换近义词、不要只写一半、不要只在标题里带）",
            );
        } else if p.contains("官网链接") {
            add(
                &mut tips,
                "正文里**一个网址都不要有**：不写 http/https、不写任何域名，\
                 厂商只用中文名提到",
            );
        } else if p.contains("缺少厂商名") {
            add(&mut tips, "正文必须至少一次出现本厂商的中文全名");
        } else if p.contains("禁止词") || p.contains("敏感词") {
            add(
                &mut tips,
                "逐字避开上面列出的禁止词/敏感词，换一个不触线的说法",
            );
        } else if p.contains("（红线）") {
            add(
                &mut tips,
                "不要用绝对化表述（最/第一/永久/无限…），也不要提任何竞品或暗示代称",
            );
        } else if p.contains("正文太短") {
            add(
                &mut tips,
                "把正文写到规定字数下限以上，靠具体细节和过程扩写，不要复读灌水",
            );
        } else if p.contains("清单式总结") {
            add(&mut tips, "补足 8-12 条 `- ` 开头的清单项");
        } else if p.contains("小标题过多") {
            add(&mut tips, "减少 `## ` 小标题，最多 4 个，其余用自然段承接");
        } else if p.contains("AI 模板腔") || p.contains("抱怨词") {
            add(
                &mut tips,
                "结构更松散、详略不均、至少一个真实缺点，不要套话",
            );
        } else if p.contains("第一行不是合法") {
            add(
                &mut tips,
                "正文第一行必须是 `# 标题` 形式的 markdown 一级标题",
            );
        }
    }
    if tips.is_empty() {
        add(&mut tips, "逐条规避上面列出的问题");
    }
    tips
}

/// 单轮尝试的结果。
enum AttemptOutcome {
    /// 合格，直接采用
    Accepted(Article),
    /// 不合格：问题清单 + 原文（原文供放弃时落日志）
    Rejected(Vec<String>, String),
}

/// 一次重试会话里**不变**的部分：客户端、端点、必含关键词。
///
/// 提成结构体而不是让 `attempt_once` 收 8 个参数——clippy 默认 7 个就报
/// too_many_arguments，而且这些值本来在所有轮次里就不变，作为一组传递
/// 语义也更清楚（"这一轮的上下文"）。
struct AttemptCtx<'a> {
    client: &'a reqwest::blocking::Client,
    url: &'a str,
    required: &'a [String],
}

/// 单轮：选角度/字数 → 拼 prompt → 调 LLM → 机器校验。
///
/// 从 [`generate_article`] 提出来的"试一次"——原函数 149 行里混着五件事
/// （构客户端、洗人设池、循环编排、调 API、放弃诊断），现在这层只负责
/// "一次完整尝试"，重试编排留在主循环，两边都能独立读。
fn attempt_once(
    ctx: &AttemptCtx<'_>,
    llm: &LlmConfig,
    profile: &CloudProfile,
    persona: (&str, &str),
    retry_feedback: &[String],
    rng: &mut rand::rngs::ThreadRng,
) -> Result<AttemptOutcome> {
    let angle = llm
        .angles
        .choose(rng)
        .map(String::as_str)
        .unwrap_or("写一次通用的使用体验");
    // 字数池为空只可能来自手写配置（config.rs 已兜默认），兜到池内首档；
    // 不编一个池外的 400——那与本文件声明的区间自相矛盾
    let length = llm
        .lengths
        .choose(rng)
        .copied()
        .unwrap_or(DEFAULT_LENGTHS[0]);

    let user = user_prompt(
        angle,
        length,
        profile,
        ctx.required,
        &llm.forbidden_words,
        persona,
    );
    let mut messages = vec![
        json!({"role": "system", "content": system_prompt()}),
        json!({"role": "user", "content": user}),
    ];
    // 上一版被判不合格——只追加"本轮要改什么"，不塞旧正文（旧人设会串味）。
    // 处方由 retry_guidance 按实际问题逐条生成，不再是一句写死的文风建议：
    // 缺关键词/串厂商/带链接这些非风格问题，收到"不要套话"是纯噪音。
    if !retry_feedback.is_empty() {
        let all = retry_feedback.join("；");
        let tips = retry_guidance(retry_feedback);
        messages.push(json!({
            "role": "user",
            "content": format!(
                "上一版被判定不合格，问题：{all}。重写一篇，这次务必逐条做到：{}。",
                tips.join("；")
            )
        }));
    }

    let mut payload = json!({
        "model": llm.model,
        "messages": messages,
        "temperature": llm.temperature,
    });
    if llm.disable_thinking {
        // ModelScope Qwen3 系列：不关 thinking 首轮返回 choices:null。
        // 该字段非 OpenAI 标准，其它供应商不认——用 ai.disable_thinking=false 关掉。
        payload["enable_thinking"] = json!(false);
    }

    let resp = ctx
        .client
        .post(ctx.url)
        .bearer_auth(&llm.api_key)
        .json(&payload)
        .send()
        .context("LLM 请求失败")?;
    let (status, body) = crate::http::read(resp)?;
    if !(200..300).contains(&status) {
        bail!(
            "LLM HTTP {status}: {}",
            crate::http::truncate_chars(&body, 300)
        );
    }

    let text = serde_json::from_str::<serde_json::Value>(&body)
        .context("LLM 响应 JSON 解析失败")?
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .map(str::to_string)
        .context("LLM 响应缺少 content")?;

    let mut problems = validate(
        &text,
        profile,
        ctx.required,
        &llm.forbidden_words,
        &llm.lengths,
    );
    if problems.is_empty() {
        if let Some((title, body_markdown)) = split_title_body(&text) {
            return Ok(AttemptOutcome::Accepted(Article {
                word_count: body_markdown.chars().count(),
                title,
                body_markdown,
            }));
        }
        // 无标题的文章审核通过率极低，等同不合规，重试
        problems.push("第一行不是合法的 # 标题".into());
    }
    Ok(AttemptOutcome::Rejected(problems, text))
}

pub fn generate_article(llm: &LlmConfig, profile: &CloudProfile) -> Result<Article> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(llm.timeout_secs))
        .build()
        .context("LLM HTTP 客户端构建失败")?;
    let url = format!("{}/chat/completions", llm.base_url);
    let mut rng = rand::thread_rng();
    let required = llm.required_keywords.clone();
    let mut retry_feedback: Vec<String> = vec![]; // 历轮问题清单（不带旧正文，避免与新人设事实冲突）
                                                  // 人设种子打乱后按轮取用：每轮（含重试）换一套具体项目+数字，
                                                  // 重试才是真的换内容，而不是拿同一批事实逼模型换个说法。
                                                  // 重试次数超过池大小时会从头复用——那说明重试空间已经用尽，此时"换事实"的
                                                  // 边际收益极小，不值得为此无限扩池。
    let mut personas: Vec<&(&str, &str)> = PERSONA_SEEDS.iter().collect();
    personas.shuffle(&mut rng);

    let mut last_problems: Vec<String> = vec![];
    // 最后一轮被判废的原文。放弃时连它一起打进日志——此前失败只留一句问题清单，
    // "缺少关键词 X"到底长什么样、是漏写还是换了近义词，事后完全无从判断
    // （2026-10-03 为此又白烧了一次 runner）。
    let mut last_text = String::new();
    let ctx = AttemptCtx {
        client: &client,
        url: &url,
        required: &required,
    };
    for attempt in 0..llm.max_retries as usize {
        let persona = personas[attempt % personas.len()];
        match attempt_once(&ctx, llm, profile, *persona, &retry_feedback, &mut rng)? {
            AttemptOutcome::Accepted(article) => return Ok(article),
            AttemptOutcome::Rejected(problems, text) => {
                // 累积问题清单喂回下一轮（只记问题，不记旧正文，避免人设串味）。
                // 去重必须用 contains：dedup() 只合并**相邻**重复，而新问题恒追加在末尾，
                // 原写法实际是恒不生效的空操作。
                last_problems = problems.clone();
                last_text = text;
                let feedback = problems.join("；");
                if !retry_feedback.contains(&feedback) {
                    retry_feedback.push(feedback);
                }
                if retry_feedback.len() > MAX_RETRY_FEEDBACK {
                    retry_feedback.remove(0);
                }
            }
        }
    }

    // 放弃前把上一轮的具体问题打出来——"不合规"三个字进日志毫无排障价值，
    // 用户面对一整屏 ERROR 却不知道是哪条校验规则挡了路（2026-09-20 阿贝云事故的直接教训）
    if !last_problems.is_empty() {
        tracing::warn!(
            "文章生成放弃时最后一轮校验问题: {}",
            last_problems.join("；")
        );
    }
    if !last_text.is_empty() {
        // 原文照打：排障时要看的是"模型到底写了什么"，不是我们以为它写了什么
        tracing::warn!("文章生成放弃时最后一轮原文如下（供排障）:\n{last_text}");
    }
    bail!(
        "生成文章 {} 次仍不合规，放弃本次（宁缺毋滥，不做垃圾提交）",
        llm.max_retries
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CLOUDS;

    fn profile(key: &str) -> &'static CloudProfile {
        CLOUDS.iter().find(|p| p.key == key).expect("厂商必须存在")
    }

    fn ok_body() -> String {
        // 正文不带任何官网链接——2026-10-03 起"干净"的定义就是无链接
        // （带外链会被平台判为推广、抬高封号风险，见 check_links）
        let mut s = String::from("# 三丰云免费云服务器实测：每7天手动续期\n\n三丰云 免费云服务器 免费虚拟主机 用着还行，想试的自己搜官网\n\n");
        for i in 0..8 {
            s.push_str(&format!("- 优点{i}：稳定够用\n"));
        }
        s.push_str(&"x".repeat(1000));
        s
    }

    #[test]
    fn style_example_does_not_trip_red_lines() {
        // 范文是"严格对齐"的校准源：它自己若含红线词（曾实锤：号称“永久免费”的
        // 转述句），模型原样借用就被 validate 打回，下一轮又被要求对齐范文——
        // 死循环空耗重试。范文对自家校验器必须干净（官网链接项按任务厂商而定，不查）。
        for key in ["sanfengyun", "abeiyun"] {
            let p = profile(key);
            let rendered = style_example_for(p);
            // 模板化后范文里**只能**有本家厂商名，绝不能残留另一家
            for other in other_vendors(p.key) {
                assert!(
                    !rendered.contains(other.name),
                    "{key} 的范文里残留了别家厂商名 {}",
                    other.name
                );
            }
            let problems = validate(&rendered, p, &[], &[], DEFAULT_LENGTHS);
            let red = problems
                .iter()
                .filter(|p| p.contains("雷区") || p.contains("绝对化") || p.contains("竞品"))
                .collect::<Vec<_>>();
            assert!(
                red.is_empty(),
                "{key} 的 few-shot 范文踩了自己的红线: {red:?}"
            );
        }
    }

    #[test]
    fn cross_vendor_article_is_rejected() {
        // 给阿贝云续期，模型却把范文里的三丰云元素带进来 → 必须打回
        let mut s = String::from("# 阿贝云免费云服务器实测：三丰云用户也说好\n\n阿贝云 免费云服务器 免费虚拟主机 https://www.sanfengyun.com 不错\n\n");
        for i in 0..8 {
            s.push_str(&format!("- 优点{i}：稳定\n"));
        }
        s.push_str(&"z".repeat(1000));
        let p = profile("abeiyun");
        let problems = validate(&s, p, &[], &[], DEFAULT_LENGTHS);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("串厂商") || p.contains("另一厂商名")),
            "漏拦另一厂商名: {problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("官网链接")),
            "漏拦错误域名: {problems:?}"
        );
        // 换成本家名字后必须放行（域名也一并去掉：现在任何官网链接都不许出现，
        // 否则这里会被 check_links 拦成"本家文章被误杀"的假失败）
        let fixed = s
            .replace("三丰云用户也说好", "半年观察")
            .replace("sanfengyun", "abeiyun")
            .replace("https://www.abeiyun.com", "自己搜官网");
        assert!(
            validate(&fixed, p, &[], &[], DEFAULT_LENGTHS).is_empty(),
            "本家文章被误杀"
        );
    }

    #[test]
    fn clean_article_passes() {
        assert!(validate(&ok_body(), profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS).is_empty());
    }

    #[test]
    fn sensitive_word_is_fatal() {
        let mut s = ok_body();
        s.push_str("平时还用 frp 做内网穿透很方便");
        let problems = validate(&s, profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS);
        assert!(
            problems.iter().any(|p| p.contains("审核雷区")),
            "应命中敏感词: {problems:?}"
        );
        // 一票否决：即使别的都没有也不该放行
        assert!(!problems.is_empty());
    }

    #[test]
    fn sensitive_words_are_case_insensitive() {
        // 模型爱用大写强调灰产词（FRP/Ngrok/GFW）；逐字面比对会整条漏网，
        // 而这是一票否决红线——漏一次就是账号收违规警告（2026-09-12 事故）。
        for raw in [
            "顺手用 FRP 打洞",
            "Ngrok 挺好用",
            "当年研究过 GFW",
            "自建 Frp 服务",
        ] {
            let mut s = ok_body();
            s.push_str(raw);
            let problems = validate(&s, profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS);
            assert!(
                problems.iter().any(|p| p.contains("审核雷区")),
                "大小写变体漏网: {raw} → {problems:?}"
            );
        }
    }

    #[test]
    fn forbidden_and_absolute_words_are_case_insensitive_too() {
        // 同一文件里两套匹配语义是本项目修过的坑：禁词/绝对化/竞品也必须大小写不敏感
        let mut s = ok_body();
        s.push_str("配置里写了 UCloud 作为对比");
        let problems = validate(
            &s,
            profile("sanfengyun"),
            &[String::new()],
            &[],
            DEFAULT_LENGTHS,
        );
        assert!(
            problems.iter().any(|p| p.contains("竞品")),
            "竞品词大小写变体漏网: {problems:?}"
        );
        // 空字符串禁词不得误伤（配置里常有空行）
        assert!(!problems.iter().any(|p| p.contains("禁止词: ")));
    }

    #[test]
    fn magic_number_is_not_a_sensitive_hit() {
        // "魔法数字"是编程惯用语（magic number），跟灰产隐语不是一回事。
        // 词表若只收"魔法"，一句正常的配置点评就会让整轮续期作废——
        // 误杀的代价同样是"这轮没续上"，所以红线词也要按最窄语义收。
        let mut ok = ok_body();
        ok.push_str("配置里尽量别留魔法数字，抽成具名常量更好维护");
        let problems = validate(&ok, profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS);
        assert!(
            !problems.iter().any(|p| p.contains("审核雷区")),
            "误杀正当技术词: {problems:?}"
        );

        // 但它作为灰产隐语出现时必须照样拦下
        let mut bad = ok_body();
        bad.push_str("顺便聊聊魔法上网那些事");
        assert!(
            validate(&bad, profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS)
                .iter()
                .any(|p| p.contains("审核雷区")),
            "灰产语境漏网"
        );
    }

    #[test]
    fn sensitive_words_do_not_false_positive_on_legit_tech() {
        // "防火墙" 含 "火"，"官方文档""CDN节点""反向代理" 都是正当词，不得误杀
        let mut s = String::from("# 三丰云免费云服务器实测\n\n三丰云 免费云服务器 免费虚拟主机 自己搜官网\n防火墙规则照抄官方文档，CDN 节点与反向代理都在跑，政策范围内自用\n\n");
        for i in 0..8 {
            s.push_str(&format!("- 优点{i}\n"));
        }
        s.push_str(&"y".repeat(1000));
        let problems = validate(&s, profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS);
        assert!(
            !problems.iter().any(|p| p.contains("审核雷区")),
            "误杀正当技术词: {problems:?}"
        );
    }

    #[test]
    fn length_floor_follows_configured_pool() {
        // 门禁必须跟着配置走：lengths 提到 3000 时，1000 字的正文要判太短，
        // 而原来的硬编码 800 会让它蒙混过关
        let s = ok_body();
        let chars = s.chars().count();
        assert!(chars > 800, "样例本身要超过旧门禁，测试才有意义");
        let problems = validate(&s, profile("sanfengyun"), &[], &[], &[3000]);
        assert!(
            problems.iter().any(|p| p.contains("正文太短")),
            "字数门禁没跟着字数池走: {problems:?}"
        );
    }

    #[test]
    fn untitled_output_is_rejected() {
        // 拆不出标题时不能回落成"无题"硬发
        assert!(split_title_body("正文直接开始，没有标题").is_none());
        assert!(split_title_body("# \n正文").is_none());
        let (t, b) = split_title_body("### 三级标题\n正文").unwrap();
        // 只剥一层 #：层级标记不能被吃光
        assert_eq!(t, "## 三级标题");
        assert_eq!(b, "正文");
    }

    #[test]
    fn prompt_omits_clauses_for_empty_lists() {
        let p = profile("sanfengyun");
        let prompt = user_prompt("角度", 1500, p, &[], &[], ("栈", "数字"));
        assert!(!prompt.contains("“”"), "空关键词不得生成空引号: {prompt}");
        assert!(!prompt.contains("禁止出现这些词"));
        // 2026-10-03 起提示词**不再**要求带官网链接（贴外链会被判推广、抬高封号风险）
        assert!(
            !prompt.contains(&format!("https://www.{}", p.site_domain())),
            "提示词不得要求模型贴官网链接: {prompt}"
        );
        assert!(
            prompt.contains("不要放任何官网链接"),
            "提示词应明确禁止外链: {prompt}"
        );
        assert!(prompt.contains("三丰云"));
        assert!(!prompt.contains("阿贝云"), "提示词里不得出现别家厂商");
    }

    #[test]
    fn prompt_never_contains_any_official_link() {
        // 覆盖所有云服务商：提示词一律不给、也不许模型自发补官网链接。
        // 人工审核不要求链接，而外链会被平台判为推广、抬高封号风险。
        for key in ["sanfengyun", "abeiyun"] {
            let p = profile(key);
            let prompt = user_prompt("角度", 1500, p, &[], &[], ("栈", "数字"));
            let domain = p.site_domain();
            assert!(
                !prompt.contains(&domain),
                "{key} 提示词里出现了官网域名 {domain}"
            );
            assert!(
                !prompt.contains("https://www."),
                "{key} 提示词里出现了 https://www. 开头的链接"
            );
        }
    }

    #[test]
    fn keyword_clause_is_a_standalone_sentence() {
        // 2026-10-03 事故：撤掉链接要求时把连接词「，以及」留在了关键词句尾，
        // 提示词变成"必须原样出现这些词：…，以及正文里不要放任何官网链接…"，
        // 两件事被粘成一句，模型的注意力被后半句带走，连挂三轮漏词、整轮续期放弃。
        // 这里钉住"关键词句自己收尾，且与下一条要求之间没有悬空连接词"。
        let p = profile("sanfengyun");
        let required = vec!["免费云服务器".to_string(), "免费虚拟主机".to_string()];
        let prompt = user_prompt("角度", 1500, p, &required, &[], ("栈", "数字"));
        assert!(
            prompt.contains("“免费云服务器”、“免费虚拟主机”。"),
            "关键词必须逐字列出并以句号收尾: {prompt}"
        );
        assert!(
            !prompt.contains("，以及正文"),
            "关键词句与链接句之间不得残留悬空连接词: {prompt}"
        );
        assert!(
            prompt.contains("”。\n正文里不要放任何官网链接"),
            "关键词句未独立成句、直接粘上了下一条要求: {prompt}"
        );
    }

    #[test]
    fn retry_guidance_targets_the_actual_problem() {
        // 这是 2026-10-03 整轮续期报废的直接病灶：三轮问题都是
        // "缺少关键词 免费虚拟主机"，而每轮喂回去的却都是写死的文风建议
        // （"结构更松散、不要套话"）。处方不对症，模型自然一轮都改不对。
        let g = retry_guidance(&["缺少关键词 免费虚拟主机".to_string()]);
        assert!(
            g.iter().any(|t| t.contains("原样")),
            "缺关键词必须给出「把词写进去」的处方，实际: {g:?}"
        );
        assert!(
            !g.iter().any(|t| t.contains("结构更松散")),
            "缺关键词不该收到文风处方，实际: {g:?}"
        );

        // 带链接 → 处方必须是"删链接"，不是谈文风
        let g = retry_guidance(&[
            "正文出现官网链接 sanfengyun.com——已决定文章一律不带官网链接".to_string(),
        ]);
        assert!(g.iter().any(|t| t.contains("网址")), "实际: {g:?}");
        assert!(!g.iter().any(|t| t.contains("结构更松散")), "实际: {g:?}");

        // 文风类问题仍要拿到文风处方（不能为了修上面那条把原来的能力砍掉）
        let g = retry_guidance(&["AI 模板腔过重（命中 4 处套话）".to_string()]);
        assert!(g.iter().any(|t| t.contains("结构更松散")), "实际: {g:?}");

        // 同类问题只出一条处方，避免 3 条重试清单里塞满重复指令
        let g = retry_guidance(&[
            "缺少关键词 免费虚拟主机".to_string(),
            "缺少关键词 免费云服务器".to_string(),
        ]);
        assert_eq!(g.len(), 1, "同类问题只该出一条处方，实际: {g:?}");

        // 将来新增的、识别不了的校验项必须有兜底，绝不返回空处方
        let g = retry_guidance(&["某种将来才会新增的问题".to_string()]);
        assert!(!g.is_empty(), "残余问题必须有兜底处方");
    }

    #[test]
    fn official_link_in_body_is_rejected() {
        // 出现本家官网域名即拦——这是"文章不带链接"的机器保证
        let mut s = ok_body();
        s.push_str("官网 https://www.sanfengyun.com 文档更新及时");
        let problems = validate(&s, profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS);
        assert!(
            problems.iter().any(|p| p.contains("官网链接")),
            "漏拦本家官网链接: {problems:?}"
        );

        // .com.com 死链同样含本家域名，必须一并拦下（不再单列"重复后缀"分支）
        let mut dup = ok_body();
        dup.push_str("官网 https://www.sanfengyun.com.com 文档更新及时");
        let problems = validate(&dup, profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS);
        assert!(
            problems.iter().any(|p| p.contains("官网链接")),
            "漏拦 .com.com 死链: {problems:?}"
        );

        // 不含任何官网链接的正文必须放行
        let problems = validate(&ok_body(), profile("sanfengyun"), &[], &[], DEFAULT_LENGTHS);
        assert!(
            !problems.iter().any(|p| p.contains("官网链接")),
            "误杀无链接的正文: {problems:?}"
        );
    }
}
