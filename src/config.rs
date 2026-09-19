//! 运行时配置：文件配置(config.toml) + 环境变量合并。
//!
//! 优先级：环境变量 > config.toml > 内置默认值。
//! 此模块是全程序唯一的配置出口，其余模块只看这里的类型。
//!
//! ## 多账号
//!
//! 同一个厂商可以配任意多个账号（两台三丰云 + 三台阿贝云都行）：
//!
//! - 文件：见 `file_config.rs` 头部（`[[clouds.sanfengyun]]` 数组表）。
//! - 环境变量（GitHub Actions Secrets 场景）：第 1 个账号用无后缀变量
//!   （`SANFENGYUN_USERNAME`，**旧部署零迁移**），第 N 个用 `_N` 后缀：
//!
//!   ```text
//!   SANFENGYUN_USERNAME / SANFENGYUN_PASSWORD            # 第 1 台
//!   SANFENGYUN_USERNAME_2 / SANFENGYUN_PASSWORD_2        # 第 2 台
//!   SANFENGYUN_LABEL_2  = "备用机"                        # 可选，通知里显示「三丰云(备用机)」
//!   SANFENGYUN_ENABLED_2 = false                         # 可选，临时停用
//!   ```
//!
//!   显式写 `_1` 也认（等价于无后缀）。每个账号装配出一个稳定的
//!   `CloudAccount::id`（`sanfengyun-1`、`abeiyun-3`…），事件 step 前缀与
//!   debug 产物文件名全部用它——同厂商多账号时这些位置再用厂商名就会互相覆盖。

use std::collections::BTreeMap;

use crate::file_config::{
    default_openclaw_model, default_temperature, CloudAccountConfig, FileConfig,
    OpenClawNotifyConfig,
};

/// 云厂商元数据（源自 2020 FreeServer 逆向 + 2026-09 实测存活）。
///
/// 这是"厂商是什么"的唯一真相：名称、端点、官网域名都从这一处取。
/// writer 生成/校验文章时也只认这里的字段，不再按中文字符串 if/else。
#[derive(Debug, Clone)]
pub struct CloudProfile {
    pub key: &'static str,
    pub name: &'static str,
    pub login_url: &'static str,
    pub renew_url: &'static str,
    /// https 被境外 WAF 拦时降级 http（阿贝云 80 实测通）
    pub allow_http_fallback: bool,
}

impl CloudProfile {
    /// 官网域名（`sanfengyun.com`），从 API 端点派生。
    ///
    /// 不另存一份字段：域名有两处定义就一定会漂移，而文章里的官网链接必须与
    /// 本次续期的厂商严格一致（挂错域名 = 人工审核直接判"文章与申请不符"）。
    pub fn site_domain(&self) -> String {
        let host = self
            .login_url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or(self.login_url);
        host.strip_prefix("api.").unwrap_or(host).to_string()
    }
}

pub static CLOUDS: &[CloudProfile] = &[
    CloudProfile {
        key: "sanfengyun",
        name: "三丰云",
        login_url: "https://api.sanfengyun.com/www/login.php",
        renew_url: "https://api.sanfengyun.com/www/renew.php",
        // 2026-09-11 Actions 实测：Azure→sanfengyun HTTPS 会整段抖掉（连接层失败），
        // 与阿贝云同款降级策略，提高境外存活率
        allow_http_fallback: true,
    },
    CloudProfile {
        key: "abeiyun",
        name: "阿贝云",
        login_url: "https://api.abeiyun.com/www/login.php",
        renew_url: "https://api.abeiyun.com/www/renew.php",
        allow_http_fallback: true,
    },
];

/// 除 `key` 之外的其它厂商元数据——跨厂商污染检查用。
/// 从 `CLOUDS` 派生：将来加第三家厂商时，前两家的"别家名单"自动包含它，
/// 不用再去 writer 里改一处写死两家的 match。
pub fn other_vendors(key: &str) -> impl Iterator<Item = &'static CloudProfile> + use<'_> {
    CLOUDS.iter().filter(move |p| p.key != key)
}

/// 本程序会从环境变量读取的**非账号**配置项。
///
/// 这份清单的唯一用途是被下面的测试拿去比对 `.github/workflows/renew.yml`：
/// GitHub Actions 不支持通配符 Secrets，**没在 job 的 `env:` 段里声明的变量，
/// 程序读到的就是空**——配了 Secret 也等于没配，而且全程没有任何报错。
///
/// 这个坑真的踩过：`install.ps1` 选"通用 Webhook"会写 `NOTIFY_WEBHOOK_URL`，
/// 但 renew.yml 从来没透传它，选了 webhook 的用户一条通知都发不出去。
/// 以后再加环境变量，这里补一行，测试会替你盯着工作流。
pub const ENV_KEYS: &[&str] = &[
    // LLM
    "LLM_BASE_URL",
    "LLM_API_KEY",
    "LLM_MODEL",
    "LLM_TIMEOUT",
    // 发文平台
    "PLATFORM_PROVIDER",
    "PLATFORM_FALLBACK",
    "CSDN_COOKIES",
    "ZHIHU_COOKIES",
    "ZHIHU_TOPICS",
    // 通知
    "NOTIFY_OPENCLAW_URL",
    "NOTIFY_OPENCLAW_USER",
    "NOTIFY_OPENCLAW_PASSWORD",
    "NOTIFY_OPENCLAW_MODEL",
    "NOTIFY_PUSHPLUS_TOKEN",
    "NOTIFY_WEBHOOK_URL",
    "NOTIFY_TAG",
    // 限额
    "HTTP_TIMEOUT",
    "ARTICLE_READY_TIMEOUT",
    "ARTICLE_VISIBLE_TIMEOUT",
    // 运行环境（job 级 env，非 Secret）
    "CHROME_PATH",
    "FREE_RENEW_LOG_DIR",
    "FREE_RENEW_DEBUG_DIR",
];

/// `renew.yml` 里预置的账号槽位数（第 1 台无后缀 + `_2`..`_N`）。
///
/// `install.ps1` 的 `MinCloudSlots` 和 renew.yml 的账号 env 块都必须与它一致：
/// 加了台数却没在 env 段声明，那台就会被静默跳过。
/// 生产代码用不到这个数字（程序读不到 yml），它纯粹是给下面那条测试当标尺的。
#[cfg(test)]
const PRESET_ACCOUNT_SLOTS: usize = 6;

/// 一个已装配好的云账号（凭据 + 生效端点 + 稳定标识）。
#[derive(Clone)]
pub struct CloudAccount {
    pub profile: &'static CloudProfile,
    /// 稳定唯一标识：`{厂商key}-{序号}`。事件 step 前缀、debug 产物文件名都用它。
    pub id: String,
    /// 展示名：默认厂商名，配了 label 则是「三丰云(主力)」。
    pub label: String,
    pub username: String,
    pub password: String,
    /// 运行时端点（默认取 profile，可被 {KEY}_LOGIN_URL{,_N} 覆盖）
    pub login_url: String,
    pub renew_url: String,
}

impl CloudAccount {
    /// 厂商中文名（文章生成用）。
    pub fn vendor(&self) -> &'static str {
        self.profile.name
    }
}

/// 手写 Debug：`CloudAccount` 带着密码，而 `{:?}` 会一路进 JSONL 日志工件。
/// derive 出来的 Debug 是一颗随时会响的雷——将来任何一行 `tracing::debug!("{acc:?}")`
/// 都会把凭据写进上传的 artifact。这里直接让凭据不可打印。
impl std::fmt::Debug for CloudAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudAccount")
            .field("id", &self.id)
            .field("vendor", &self.profile.key)
            .field("username", &crate::logging::mask_id(&self.username))
            .field("password", &redact(&self.password))
            .field("login_url", &self.login_url)
            .field("renew_url", &self.renew_url)
            .finish()
    }
}

/// 凭据的 Debug 呈现：只给长度，不给内容。
fn redact(secret: &str) -> String {
    if secret.trim().is_empty() {
        "<空>".to_string()
    } else {
        format!("<已隐藏 {} 字符>", secret.chars().count())
    }
}

#[derive(Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 角度池（空则用内置默认）
    pub angles: Vec<String>,
    pub lengths: Vec<usize>,
    pub forbidden_words: Vec<String>,
    pub required_keywords: Vec<String>,
    pub max_retries: u32,
    /// 单次生成请求超时（秒）。LLM 出文比厂商接口慢得多，与 http_timeout 不同源。
    pub timeout_secs: u64,
    /// 采样温度
    pub temperature: f32,
    /// 是否发 `enable_thinking=false`（ModelScope Qwen3 系需要，其它供应商无此字段）
    pub disable_thinking: bool,
}

impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &redact(&self.api_key))
            .field("model", &self.model)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

#[derive(Clone)]
pub struct CsdnConfig {
    pub cookie: String,
    pub creation_statement: u8,
    pub tags: Vec<String>,
    pub categories: Vec<String>,
    pub app_secret: String,
    pub x_ca_key: String,
}

impl CsdnConfig {
    /// 平台是否可用：段落存在 ≠ Cookie 有值，所有判定统一走这里。
    pub fn ready(&self) -> bool {
        !self.cookie.trim().is_empty()
    }
}

impl std::fmt::Debug for CsdnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CsdnConfig")
            .field("cookie", &redact(&self.cookie))
            .field("creation_statement", &self.creation_statement)
            .field("tags", &self.tags)
            .finish()
    }
}

#[derive(Clone)]
pub struct ZhihuConfig {
    /// 单行 Cookie，含 z_c0/_xsrf/d_c0/q_c1
    pub cookie: String,
    /// 发文必挂话题（不挂通常发不出去）
    pub topics: Vec<String>,
    /// 是否开启目录
    pub toc: bool,
}

impl ZhihuConfig {
    /// 平台是否可用：与 `CsdnConfig::ready` 同一口径。
    pub fn ready(&self) -> bool {
        !self.cookie.trim().is_empty()
    }
}

impl std::fmt::Debug for ZhihuConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZhihuConfig")
            .field("cookie", &redact(&self.cookie))
            .field("topics", &self.topics)
            .field("toc", &self.toc)
            .finish()
    }
}

#[derive(Clone)]
pub struct NotifyConfig {
    pub webhook_url: String,
    pub tag: String,
    /// OpenClaw 后端（Some 时优先，失败会再试 pushplus/webhook 兜底）
    pub openclaw: Option<OpenClawNotify>,
    /// PushPlus 后端（免费层每日 200 条；GHA 直发公网 API 可达，不经过 CF）
    pub pushplus_token: String,
}

/// 手写 Debug：token 会随 `{cfg:?}` 进 JSONL 日志工件（与 CloudAccount 同一个坑），必须遮掉。
impl std::fmt::Debug for NotifyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyConfig")
            .field("webhook_url", &self.webhook_url)
            .field("tag", &self.tag)
            .field("openclaw", &self.openclaw)
            .field("pushplus_token", &redact(&self.pushplus_token))
            .finish()
    }
}

#[derive(Clone)]
pub struct OpenClawNotify {
    pub url: String,
    pub basic_user: String,
    pub basic_password: String,
    pub model: String,
}

impl std::fmt::Debug for OpenClawNotify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenClawNotify")
            .field("url", &self.url)
            .field("basic_user", &self.basic_user)
            .field("basic_password", &redact(&self.basic_password))
            .field("model", &self.model)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub accounts: Vec<CloudAccount>,
    pub llm: Option<LlmConfig>,
    pub platform_provider: String,
    /// 主平台发文失败时自动切换的备胎（Some 且 != provider 才生效）
    pub platform_fallback: Option<String>,
    pub csdn: Option<CsdnConfig>,
    pub zhihu: Option<ZhihuConfig>,
    pub notify: NotifyConfig,
    pub article_ready_timeout: u64,
    pub http_timeout: u64,
}

/// 兜底平台判定（纯函数便于测试）。explicit 来自 PLATFORM_FALLBACK / 文件段：
/// - "none"（或大小写等价）→ 关闭兜底；与主平台同名 → 等价于关闭
/// - 其它值 → 原样采用（配置错名会在发文时报"未知发文平台"，不静默）
/// - 未设置 → 自动：主平台之外、另一家 Cookie 已就绪就兜底（两家都连即互为备份）
fn resolve_fallback(
    primary: &str,
    explicit: Option<String>,
    csdn_ready: bool,
    zhihu_ready: bool,
) -> Option<String> {
    if let Some(e) = explicit {
        let e = e.to_ascii_lowercase();
        if e == "none" || e == primary {
            return None;
        }
        return Some(e);
    }
    match primary {
        "zhihu" if csdn_ready => Some("csdn".into()),
        "csdn" if zhihu_ready => Some("zhihu".into()),
        _ => None,
    }
}

/// 装配用环境快照（已 trim、已丢弃空值、键统一大写）。
///
/// 抽成"先快照再查询"而非散落的 `std::env::var`，一是让"编号后缀"这类纯逻辑
/// 可单测，二是键统一大写后，Windows 上写小写变量名不会莫名失效。
type EnvMap = BTreeMap<String, String>;

fn env_snapshot() -> EnvMap {
    // 所有凭据/URL 一律 trim：Secret 注入渠道（管道/网页粘贴）常混入尾部换行或
    // 空白，LLM 供应商实测会对带 \n 的 Bearer 报"未提供令牌"
    std::env::vars_os()
        .filter_map(|(k, v)| {
            let k = k.to_str()?.to_ascii_uppercase();
            let v = v.to_str()?.trim().to_string();
            (!v.is_empty()).then_some((k, v))
        })
        .collect()
}

/// 一个厂商允许的最大账号数：纯防御性上限，防止手滑写出
/// `SANFENGYUN_USERNAME_999999` 这种变量把循环拖爆。
const MAX_ACCOUNTS_PER_VENDOR: usize = 32;

/// 账号凭据在环境变量里的字段名后缀。
const ACCOUNT_FIELDS: &[&str] = &[
    "USERNAME",
    "PASSWORD",
    "LABEL",
    "ENABLED",
    "LOGIN_URL",
    "RENEW_URL",
];

/// 取某个账号槽位上的字段值。第 1 个账号用无后缀变量（兼容既有部署），
/// 也接受显式 `_1`；第 N 个用 `_N`。
fn env_slot(env: &EnvMap, key_upper: &str, slot: usize, field: &str) -> Option<String> {
    let base = format!("{key_upper}_{field}");
    if let Some(v) = env.get(&format!("{base}_{slot}")) {
        return Some(v.clone());
    }
    // 无后缀只对第 1 个账号有意义
    if slot == 1 {
        return env.get(&base).cloned();
    }
    None
}

/// 环境变量里出现过的最大账号序号（无后缀算 1，一个都没出现算 0）。
fn max_env_slot(env: &EnvMap, key_upper: &str) -> usize {
    let mut max = 0;
    for name in env.keys() {
        for field in ACCOUNT_FIELDS {
            let prefix = format!("{key_upper}_{field}");
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            // "" → 无后缀（第 1 个）；"_2" → 第 2 个；"_EXTRA" 这类不是编号，忽略
            let slot = if rest.is_empty() {
                1
            } else if let Some(n) = rest.strip_prefix('_') {
                match n.parse::<usize>() {
                    Ok(v) if v >= 1 => v,
                    _ => continue,
                }
            } else {
                continue;
            };
            max = max.max(slot);
        }
    }
    max
}

/// "0/false/no/off" 视为关（大小写不敏感），其余为开。
fn parse_bool(v: &str) -> bool {
    !matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// 装配单个厂商的账号列表：文件给顺序与骨架，环境变量按槽位覆盖/补充。
///
/// 合并规则（这就是"env > config.toml"在账号维度上的落地）：
/// - 第 i 个槽位两边都有 → 环境变量逐字段覆盖文件
/// - 只有文件有 → 用文件；只有环境变量有 → 用环境变量（账号总数取两者较大者）
fn build_accounts(
    profile: &'static CloudProfile,
    file_accs: &[CloudAccountConfig],
    env: &EnvMap,
) -> Vec<CloudAccount> {
    let key_upper = profile.key.to_ascii_uppercase();
    let slots = file_accs
        .len()
        .max(max_env_slot(env, &key_upper))
        .min(MAX_ACCOUNTS_PER_VENDOR);
    let mut out = Vec::with_capacity(slots);

    for idx in 0..slots {
        let slot = idx + 1;
        let file_acc = file_accs.get(idx);
        let non_empty = |s: &str| -> Option<String> {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };

        let username = env_slot(env, &key_upper, slot, "USERNAME")
            .or_else(|| file_acc.and_then(|a| non_empty(&a.username)));
        let password = env_slot(env, &key_upper, slot, "PASSWORD")
            .or_else(|| file_acc.and_then(|a| non_empty(&a.password)));

        // enabled 也可被环境变量覆盖：模块头声明的"env > 文件"对每个字段都成立
        let enabled = match env_slot(env, &key_upper, slot, "ENABLED") {
            Some(v) => parse_bool(&v),
            None => file_acc.map(|a| a.enabled).unwrap_or(true),
        };
        if !enabled {
            tracing::info!(
                "[config] {}(第 {slot} 台) 被 enabled=false 禁用",
                profile.name
            );
            continue;
        }

        let (Some(username), Some(password)) = (username, password) else {
            // 文件名里有槽位但凭据不全：说清是第几台、缺什么，别只留一句"跳过"
            tracing::info!(
                "[config] {}(第 {slot} 台) 凭据不完整（需 {key_upper}_USERNAME{sep}{slot} 与 \
                 {key_upper}_PASSWORD{sep}{slot}），跳过",
                profile.name,
                sep = if slot == 1 { " 或 " } else { "_" },
            );
            continue;
        };

        // 展示名：配了 label 就用「三丰云(备用机)」；一台都没多时就是「三丰云」；
        // 多台且没配 label 时附上序号「三丰云#2」——否则通知里两条一模一样的
        // 「三丰云 发文失败」，你根本看不出是哪台出了事。
        let label = env_slot(env, &key_upper, slot, "LABEL")
            .or_else(|| file_acc.and_then(|a| a.label.clone()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| format!("{}({s})", profile.name))
            .unwrap_or_else(|| {
                if slots > 1 {
                    format!("{}#{slot}", profile.name)
                } else {
                    profile.name.to_string()
                }
            });

        // 端点 URL 支持环境变量覆盖：厂商 WAF 拉黑 Actions 出口 IP 时，
        // 可指向自建中继（如服务器反代）而无需改代码
        let login_url = env_slot(env, &key_upper, slot, "LOGIN_URL")
            .unwrap_or_else(|| profile.login_url.to_string());
        let renew_url = env_slot(env, &key_upper, slot, "RENEW_URL")
            .unwrap_or_else(|| profile.renew_url.to_string());

        out.push(CloudAccount {
            profile,
            id: format!("{}-{slot}", profile.key),
            label,
            username,
            password,
            login_url,
            renew_url,
        });
    }
    out
}

/// 遍历全部厂商，装配所有已配置的账号。
fn load_accounts(file: Option<&FileConfig>, env: &EnvMap) -> Vec<CloudAccount> {
    let empty = BTreeMap::new();
    let clouds = file.map(|f| &f.clouds).unwrap_or(&empty);
    let mut accounts = Vec::new();
    for profile in CLOUDS {
        // 文件里的厂商段：数组表给多台，单表给一台（见 file_config::OneOrMany）。
        // 认不出的厂商 key 明确报警——静默忽略会让"配置了却没生效"极难排查。
        let file_accs: Vec<CloudAccountConfig> = match clouds.get(profile.key) {
            Some(v) => v.clone().into_vec(),
            None => vec![],
        };
        accounts.extend(build_accounts(profile, &file_accs, env));
    }
    for key in clouds.keys() {
        if !CLOUDS.iter().any(|p| p.key == key) {
            tracing::error!(
                "[config] [clouds.{key}] 不是已知厂商（支持：{}），该段被忽略",
                CLOUDS.iter().map(|p| p.key).collect::<Vec<_>>().join("、")
            );
        }
    }
    accounts
}

/// 空切片用默认池填充（angles/lengths/forbidden_words 三处同款兜底，只写一遍）。
fn or_default<T: Clone>(v: &[T], d: &[T]) -> Vec<T> {
    if v.is_empty() {
        d.to_vec()
    } else {
        v.to_vec()
    }
}

/// 解析数值型环境变量。解析失败要出声：`ARTICLE_READY_TIMEOUT=60s` 这种手滑
/// 若静默回落默认值，用户会以为配置已经生效。
fn env_u64(env: &EnvMap, key: &str, default: u64) -> u64 {
    match env.get(key) {
        None => default,
        Some(v) => match v.parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!("[config] {key}={v:?} 不是合法非负整数，回落到默认值 {default}");
                default
            }
        },
    }
}

fn load_llm(file: Option<&FileConfig>, env: &EnvMap) -> Option<LlmConfig> {
    let ai = file.map(|f| f.ai.clone()).unwrap_or_default();
    let base_url = env.get("LLM_BASE_URL").cloned().or(ai.base_url.clone())?;
    let api_key = env.get("LLM_API_KEY").cloned().or(ai.api_key.clone())?;
    let model = env.get("LLM_MODEL").cloned().or(ai.model.clone())?;

    if ai.max_retries == 0 {
        tracing::warn!("[config] ai.max_retries=0 会让生成一次都不跑，已按 1 处理");
    }
    Some(LlmConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key,
        model,
        angles: or_default(&ai.angles, &crate::writer::default_angles()),
        lengths: or_default(&ai.lengths, crate::writer::DEFAULT_LENGTHS),
        forbidden_words: or_default(&ai.forbidden_words, &crate::writer::default_forbidden()),
        required_keywords: or_default(&ai.required_keywords, &crate::writer::default_keywords()),
        max_retries: ai.max_retries.max(1),
        timeout_secs: env_u64(env, "LLM_TIMEOUT", crate::writer::LLM_REQUEST_TIMEOUT_SECS),
        temperature: ai.temperature.unwrap_or_else(default_temperature),
        disable_thinking: ai.disable_thinking,
    })
}

fn load_csdn(file: Option<&FileConfig>, env: &EnvMap) -> Option<CsdnConfig> {
    let section = file.and_then(|f| f.platform.csdn.clone());
    let cookie_env = env.get("CSDN_COOKIES").cloned();
    // 既没有文件段又没有 Cookie 环境变量 = 用户没打算用 CSDN
    if section.is_none() && cookie_env.is_none() {
        return None;
    }
    // 只有环境变量时用默认骨架（默认值统一来自 file_config，不再在 env 分支抄一份）
    let section = section.unwrap_or_default();
    Some(CsdnConfig {
        cookie: cookie_env.unwrap_or(section.cookie),
        creation_statement: section.creation_statement,
        tags: section.tags,
        categories: section.categories,
        app_secret: section
            .app_secret
            .unwrap_or_else(|| crate::csdn::DEFAULT_APP_SECRET.into()),
        x_ca_key: section
            .x_ca_key
            .unwrap_or_else(|| crate::csdn::DEFAULT_X_CA_KEY.into()),
    })
}

fn env_zhihu_topics(env: &EnvMap) -> Option<Vec<String>> {
    env.get("ZHIHU_TOPICS")
        .map(|s| {
            s.split_whitespace()
                .map(String::from)
                .collect::<Vec<String>>()
        })
        .filter(|v| !v.is_empty())
}

fn load_zhihu(file: Option<&FileConfig>, env: &EnvMap) -> Option<ZhihuConfig> {
    let section = file.and_then(|f| f.platform.zhihu.clone());
    let cookie_env = env.get("ZHIHU_COOKIES").cloned();
    if section.is_none() && cookie_env.is_none() {
        return None;
    }
    let section = section.unwrap_or_default();
    Some(ZhihuConfig {
        cookie: cookie_env.unwrap_or(section.cookie),
        topics: env_zhihu_topics(env).unwrap_or(section.topics),
        toc: section.toc,
    })
}

/// 发文平台整段（主平台、兜底、两家 Cookie 配置）——它们互相依赖，
/// 拆开加载会让"兜底要不要开"的判定散到别处。
struct PlatformBundle {
    provider: String,
    fallback: Option<String>,
    csdn: Option<CsdnConfig>,
    zhihu: Option<ZhihuConfig>,
}

fn load_platform(file: Option<&FileConfig>, env: &EnvMap) -> PlatformBundle {
    // 小写归一：仓库 Variables 是网页表单手填的，"Zhihu"/"CSDN" 这类大小写手滑
    // 不该让整轮续期以"未知发文平台"作废
    let provider = env
        .get("PLATFORM_PROVIDER")
        .cloned()
        .or_else(|| file.and_then(|f| f.platform.provider.clone()))
        .unwrap_or_else(|| "csdn".to_string())
        .to_ascii_lowercase();
    let csdn = load_csdn(file, env);
    let zhihu = load_zhihu(file, env);
    // 兜底：显式 PLATFORM_FALLBACK / 文件段优先，未设则"两家都连即自动互备"
    let fallback = resolve_fallback(
        &provider,
        env.get("PLATFORM_FALLBACK")
            .cloned()
            .or_else(|| file.and_then(|f| f.platform.fallback.clone())),
        csdn.as_ref().map(CsdnConfig::ready).unwrap_or(false),
        zhihu.as_ref().map(ZhihuConfig::ready).unwrap_or(false),
    );
    PlatformBundle {
        provider,
        fallback,
        csdn,
        zhihu,
    }
}

fn load_openclaw(section: Option<&OpenClawNotifyConfig>, env: &EnvMap) -> Option<OpenClawNotify> {
    let url_env = env.get("NOTIFY_OPENCLAW_URL").cloned();
    let user_env = env.get("NOTIFY_OPENCLAW_USER").cloned();
    let pass_env = env.get("NOTIFY_OPENCLAW_PASSWORD").cloned();
    let model_env = env.get("NOTIFY_OPENCLAW_MODEL").cloned();

    // 文件段在 → 就以文件为骨架，环境变量逐字段覆盖
    if let Some(o) = section {
        return Some(OpenClawNotify {
            url: url_env.unwrap_or_else(|| o.url.clone()),
            basic_user: user_env.unwrap_or_else(|| o.basic_user.clone()),
            basic_password: pass_env.unwrap_or_else(|| o.basic_password.clone()),
            model: model_env.unwrap_or_else(|| o.model.clone()),
        });
    }

    // 纯环境变量路径（GitHub Actions 无 config.toml）：三个 Secrets 必须齐全。
    // 只配一半时必须出声——通知是失败唯一的可见通道，静默禁用等于出事无人知。
    let missing: Vec<&str> = [
        (url_env.is_none(), "NOTIFY_OPENCLAW_URL"),
        (user_env.is_none(), "NOTIFY_OPENCLAW_USER"),
        (pass_env.is_none(), "NOTIFY_OPENCLAW_PASSWORD"),
    ]
    .into_iter()
    .filter_map(|(absent, name)| absent.then_some(name))
    .collect();
    if missing.len() == 3 {
        return None; // 一个都没配 = 用户本来就没配通知，正常
    }
    if !missing.is_empty() {
        tracing::error!(
            "[config] OpenClaw 通知只配了一半，缺 {}——通知无法送达，补齐这三个 Secrets 后自动生效",
            missing.join("、")
        );
        return None;
    }
    Some(OpenClawNotify {
        url: url_env?,
        basic_user: user_env?,
        basic_password: pass_env?,
        model: model_env.unwrap_or_else(default_openclaw_model),
    })
}

fn load_notify(file: Option<&FileConfig>, env: &EnvMap) -> NotifyConfig {
    let section = file.map(|f| f.notify.clone()).unwrap_or_default();
    NotifyConfig {
        webhook_url: env
            .get("NOTIFY_WEBHOOK_URL")
            .cloned()
            .unwrap_or(section.webhook_url),
        // tag 也支持环境变量覆盖：Actions 纯 Secrets 部署没有 config.toml，
        // 只给 webhook_url 留 env 通道会让这里的优先级约定自相矛盾
        tag: env.get("NOTIFY_TAG").cloned().unwrap_or(section.tag),
        openclaw: load_openclaw(section.openclaw.as_ref(), env),
        pushplus_token: env
            .get("NOTIFY_PUSHPLUS_TOKEN")
            .cloned()
            .unwrap_or(section.pushplus_token),
    }
}

impl AppConfig {
    /// 合并文件配置与环境变量。`config_path` 为 --config 显式传入。
    pub fn load(config_path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        // 配置文件存在但坏 = 硬错误：宁可不开机也不带病运行。
        // 走 Result 而非 panic——进程崩溃拿不到错误链，也落不了一条结构化事件。
        let file = FileConfig::find(config_path)?;
        let env = env_snapshot();
        let platform = load_platform(file.as_ref(), &env);
        let limits = file.as_ref().map(|f| f.limits.clone()).unwrap_or_default();

        Ok(Self {
            accounts: load_accounts(file.as_ref(), &env),
            llm: load_llm(file.as_ref(), &env),
            platform_provider: platform.provider,
            platform_fallback: platform.fallback,
            csdn: platform.csdn,
            zhihu: platform.zhihu,
            notify: load_notify(file.as_ref(), &env),
            article_ready_timeout: env_u64(
                &env,
                "ARTICLE_READY_TIMEOUT",
                limits.article_ready_timeout,
            ),
            http_timeout: env_u64(&env, "HTTP_TIMEOUT", limits.http_timeout),
        })
    }

    /// 按账号 ID、厂商名或展示名找账号。
    ///
    /// 多账号场景下"按厂商名"必然歧义：命中多个时**明确报错并列出可用 ID**，
    /// 绝不静默取第一个——那会把文章和截图提交到错误的账号上。
    pub fn find_account(&self, needle: &str) -> anyhow::Result<&CloudAccount> {
        let needle = needle.trim();
        if let Some(acc) = self.accounts.iter().find(|a| a.id == needle) {
            return Ok(acc);
        }
        let hits: Vec<&CloudAccount> = self
            .accounts
            .iter()
            .filter(|a| a.profile.name == needle || a.profile.key == needle || a.label == needle)
            .collect();
        match hits.len() {
            1 => Ok(hits[0]),
            0 => anyhow::bail!("未找到账号 {needle:?}。{}", self.account_hint()),
            n => anyhow::bail!(
                "{needle:?} 匹配到 {n} 个账号，无法确定是哪一个——请改用账号 ID。{}",
                self.account_hint()
            ),
        }
    }

    /// 找不到/歧义时给用户的可用清单。
    pub fn account_hint(&self) -> String {
        if self.accounts.is_empty() {
            return "当前没有任何已配置账号：检查 config.toml [clouds.*] 或 SANFENGYUN_/ABEIYUN_ 环境变量"
                .to_string();
        }
        format!(
            "可用账号：{}",
            self.accounts
                .iter()
                .map(|a| format!("{}={}", a.id, a.label))
                .collect::<Vec<_>>()
                .join("、")
        )
    }

    /// 解析"厂商/账号"参数：先按已配置账号匹配（账号 ID > 厂商名 > label），
    /// 匹配不上再退回厂商元数据——诊断子命令要能在"还没配账号"时也验内容生成链路，
    /// 故这里有意吞掉 `find_account` 的歧义错误继续往下找。
    pub fn resolve_profile(&self, needle: &str) -> anyhow::Result<&'static CloudProfile> {
        if let Ok(acc) = self.find_account(needle) {
            return Ok(acc.profile);
        }
        let n = needle.trim();
        CLOUDS
            .iter()
            .find(|p| p.key == n || p.name == n)
            .ok_or_else(|| anyhow::anyhow!("未找到厂商或账号 {needle:?}。{}", self.account_hint()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_resolution_matrix() {
        // 自动：知乎为主 + CSDN 就绪 → 兜底 csdn；CSDN 未配置 → 无兜底
        assert_eq!(
            resolve_fallback("zhihu", None, true, true).as_deref(),
            Some("csdn")
        );
        assert_eq!(resolve_fallback("zhihu", None, false, true), None);
        assert_eq!(
            resolve_fallback("csdn", None, true, true).as_deref(),
            Some("zhihu")
        );
        assert_eq!(resolve_fallback("csdn", None, true, false), None);
        // 显式覆盖：none 关兜底；与主同名视为关；其它值原样采用（错名在发文时明确报错）
        assert_eq!(
            resolve_fallback("zhihu", Some("none".into()), true, true),
            None
        );
        assert_eq!(
            resolve_fallback("zhihu", Some("NONE".into()), true, true),
            None
        );
        assert_eq!(
            resolve_fallback("zhihu", Some("zhihu".into()), true, true),
            None
        );
        assert_eq!(
            resolve_fallback("zhihu", Some("CSDN".into()), false, true).as_deref(),
            Some("csdn")
        );
    }

    fn env_of(pairs: &[(&str, &str)]) -> EnvMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_ascii_uppercase(), (*v).to_string()))
            .collect()
    }

    fn sanfengyun() -> &'static CloudProfile {
        CLOUDS.iter().find(|p| p.key == "sanfengyun").unwrap()
    }

    #[test]
    fn site_domain_derives_from_api_host() {
        assert_eq!(sanfengyun().site_domain(), "sanfengyun.com");
        assert_eq!(
            CLOUDS
                .iter()
                .find(|p| p.key == "abeiyun")
                .unwrap()
                .site_domain(),
            "abeiyun.com"
        );
    }

    #[test]
    fn other_vendors_excludes_self() {
        let names = |key| other_vendors(key).map(|p| p.name).collect::<Vec<_>>();
        assert_eq!(names("sanfengyun"), vec!["阿贝云"]);
        assert_eq!(names("abeiyun"), vec!["三丰云"]);
    }

    #[test]
    fn single_legacy_env_gives_one_account_with_default_id() {
        // 旧部署：无后缀变量 → 一个账号，id 为 {key}-1，行为与改造前一致
        let env = env_of(&[
            ("SANFENGYUN_USERNAME", "13800000000"),
            ("SANFENGYUN_PASSWORD", "pw"),
        ]);
        let accs = build_accounts(sanfengyun(), &[], &env);
        assert_eq!(accs.len(), 1);
        assert_eq!(accs[0].id, "sanfengyun-1");
        // 单台时不加序号：老用户的日志/通知文案保持不变
        assert_eq!(accs[0].label, "三丰云");
        assert_eq!(accs[0].login_url, sanfengyun().login_url);
    }

    /// 多台都不配 label 时，展示名必须能互相区分——否则通知里两条
    /// 「三丰云 发文失败」让人无从下手。
    #[test]
    fn multiple_accounts_without_label_get_numbered_names() {
        let env = env_of(&[
            ("SANFENGYUN_USERNAME", "u1"),
            ("SANFENGYUN_PASSWORD", "p1"),
            ("SANFENGYUN_USERNAME_2", "u2"),
            ("SANFENGYUN_PASSWORD_2", "p2"),
        ]);
        let accs = build_accounts(sanfengyun(), &[], &env);
        let labels: Vec<&str> = accs.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, vec!["三丰云#1", "三丰云#2"]);
    }

    #[test]
    fn numbered_env_gives_all_accounts_in_order() {
        let env = env_of(&[
            ("SANFENGYUN_USERNAME", "u1"),
            ("SANFENGYUN_PASSWORD", "p1"),
            ("SANFENGYUN_USERNAME_2", "u2"),
            ("SANFENGYUN_PASSWORD_2", "p2"),
            ("SANFENGYUN_LABEL_2", "备用机"),
            ("SANFENGYUN_USERNAME_3", "u3"),
            ("SANFENGYUN_PASSWORD_3", "p3"),
        ]);
        let accs = build_accounts(sanfengyun(), &[], &env);
        let ids: Vec<&str> = accs.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["sanfengyun-1", "sanfengyun-2", "sanfengyun-3"]);
        // 多台且没配 label 时自动带序号，否则通知里两条一模一样的「三丰云」分不清谁是谁
        assert_eq!(accs[0].label, "三丰云#1");
        assert_eq!(accs[1].label, "三丰云(备用机)");
        assert_eq!(accs[2].label, "三丰云#3");
        assert_eq!(accs[2].username, "u3");
        // 每台账号必须拿到自己的密码，不能串号
        assert_eq!(accs[2].password, "p3");
    }

    #[test]
    fn explicit_slot_one_is_equivalent_to_unsuffixed() {
        let env = env_of(&[
            ("SANFENGYUN_USERNAME_1", "u1"),
            ("SANFENGYUN_PASSWORD_1", "p1"),
        ]);
        let accs = build_accounts(sanfengyun(), &[], &env);
        assert_eq!(accs.len(), 1);
        assert_eq!(accs[0].username, "u1");
    }

    #[test]
    fn half_configured_slot_is_skipped_not_half_used() {
        // 第二台只填了用户名：跳过它，但第一台照常工作（不能整段作废）
        let env = env_of(&[
            ("SANFENGYUN_USERNAME", "u1"),
            ("SANFENGYUN_PASSWORD", "p1"),
            ("SANFENGYUN_USERNAME_2", "u2"),
        ]);
        let accs = build_accounts(sanfengyun(), &[], &env);
        assert_eq!(accs.len(), 1);
        assert_eq!(accs[0].id, "sanfengyun-1");
    }

    #[test]
    fn file_accounts_merge_with_env_overrides() {
        // 两台在文件里；环境变量只覆盖第 1 台的密码，第 2 台保持文件值
        let file_accs = vec![
            CloudAccountConfig {
                username: "file-u1".into(),
                password: "file-p1".into(),
                enabled: true,
                label: None,
            },
            CloudAccountConfig {
                username: "file-u2".into(),
                password: "file-p2".into(),
                enabled: true,
                label: Some("二号".into()),
            },
        ];
        let env = env_of(&[("SANFENGYUN_PASSWORD", "env-p1")]);
        let accs = build_accounts(sanfengyun(), &file_accs, &env);
        assert_eq!(accs.len(), 2, "文件里的两台都要保留");
        assert_eq!(accs[0].username, "file-u1");
        assert_eq!(accs[0].password, "env-p1", "环境变量必须覆盖文件");
        assert_eq!(accs[1].password, "file-p2", "未被覆盖的槽位保持文件值");
        assert_eq!(accs[1].label, "三丰云(二号)");
    }

    #[test]
    fn disabled_flag_skips_slot_but_keeps_others() {
        let file_accs = vec![
            CloudAccountConfig {
                username: "u1".into(),
                password: "p1".into(),
                enabled: true,
                label: None,
            },
            CloudAccountConfig {
                username: "u2".into(),
                password: "p2".into(),
                enabled: false,
                label: None,
            },
        ];
        let accs = build_accounts(sanfengyun(), &file_accs, &env_of(&[]));
        assert_eq!(accs.len(), 1);
        assert_eq!(accs[0].username, "u1");
    }

    #[test]
    fn max_env_slot_ignores_non_numeric_suffixes() {
        let env = env_of(&[
            ("SANFENGYUN_USERNAME", "u1"),
            ("SANFENGYUN_USERNAME_EXTRA", "x"),
            ("ABEIYUN_USERNAME_4", "u4"),
        ]);
        assert_eq!(max_env_slot(&env, "SANFENGYUN"), 1);
        assert_eq!(max_env_slot(&env, "ABEIYUN"), 4);
    }

    /// 用户举的例子：2 台三丰云 + 3 台阿贝云，从 TOML 一路装配到账号列表。
    #[test]
    fn file_config_with_many_accounts_yields_unique_ids() {
        let file: FileConfig = toml::from_str(
            r#"
[[clouds.sanfengyun]]
username = "sf1"
password = "p1"
label = "主力"

[[clouds.sanfengyun]]
username = "sf2"
password = "p2"

[[clouds.abeiyun]]
username = "ab1"
password = "p3"

[[clouds.abeiyun]]
username = "ab2"
password = "p4"

[[clouds.abeiyun]]
username = "ab3"
password = "p5"
"#,
        )
        .expect("多账号配置必须可解析");

        let accounts = load_accounts(Some(&file), &env_of(&[]));
        let ids: Vec<&str> = accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "sanfengyun-1",
                "sanfengyun-2",
                "abeiyun-1",
                "abeiyun-2",
                "abeiyun-3"
            ],
            "厂商顺序 + 厂商内序号，一个都不能少"
        );

        // ID 要当事件前缀与 debug 产物文件名，必须全局唯一——同名就会互相覆盖
        let mut uniq = ids.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), ids.len(), "账号 ID 撞名: {ids:?}");

        // 每台的凭据与展示名都要对得上自己的槽位（不能串号）
        assert_eq!(accounts[0].username, "sf1");
        assert_eq!(accounts[0].label, "三丰云(主力)");
        assert_eq!(accounts[1].password, "p2");
        assert_eq!(accounts[4].username, "ab3");
        assert_eq!(accounts[4].label, "阿贝云#3");
        // 端点默认取各自厂商的 profile
        assert!(accounts[0].renew_url.contains("sanfengyun"));
        assert!(accounts[2].renew_url.contains("abeiyun"));
    }

    /// 模板文件本身必须永远可解析：用户是照着 config.example.toml 改的，
    /// 它语法错或者结构过时，等于所有人都装不上。
    #[test]
    fn example_config_template_stays_parseable() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
        let file = FileConfig::find(Some(&path))
            .expect("读示例配置不应报错")
            .expect("config.example.toml 必须存在");
        let accounts = load_accounts(Some(&file), &env_of(&[]));
        let ids: Vec<&str> = accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "sanfengyun-1",
                "sanfengyun-2",
                "abeiyun-1",
                "abeiyun-2",
                "abeiyun-3"
            ],
            "示例模板里写的 2 台三丰云 + 3 台阿贝云必须一模一样地装配出来"
        );
        // 示例里的 label 也要生效（它是给人看的文档，也是可运行的配置）
        assert_eq!(accounts[0].label, "三丰云(主力)");
        assert_eq!(accounts[1].label, "三丰云(备用)");
        assert_eq!(accounts[2].label, "阿贝云(主力)");
    }

    /// 程序能读到的环境变量，工作流必须全都透传。
    ///
    /// GitHub Actions 不支持通配符 Secrets：**没在 renew.yml 的 `env:` 段声明的
    /// 变量，程序读到的就是空**——配了 Secret 也等于没配，而且没有任何报错。
    /// 这个坑真的踩过（`NOTIFY_WEBHOOK_URL` 写了 Secret 却没透传，选 webhook
    /// 通知的用户一条都收不到）。这条测试把"靠人记住"变成"改漏了就红"。
    #[test]
    fn every_env_var_the_program_reads_is_wired_in_the_workflow() {
        let yaml = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/renew.yml"),
        )
        .expect("读 .github/workflows/renew.yml");

        let mut missing: Vec<String> = Vec::new();
        for key in ENV_KEYS {
            if !yaml.contains(&format!("{key}:")) {
                missing.push((*key).to_string());
            }
        }
        // 账号变量：每个厂商 × 每个预置槽位 × 每个字段。
        // install.ps1 生成的账号 env 块与这份展开必须逐字一致。
        for profile in CLOUDS {
            let upper = profile.key.to_ascii_uppercase();
            for slot in 1..=PRESET_ACCOUNT_SLOTS {
                for field in ACCOUNT_FIELDS {
                    let name = if slot == 1 {
                        format!("{upper}_{field}")
                    } else {
                        format!("{upper}_{field}_{slot}")
                    };
                    if !yaml.contains(&format!("{name}:")) {
                        missing.push(name);
                    }
                }
            }
        }

        assert!(
            missing.is_empty(),
            "renew.yml 的 env 段没透传这些变量，程序会读到空值（等于没配）：{missing:?}"
        );
    }

    #[test]
    fn account_lookup_rejects_ambiguous_vendor_name() {
        // 两台三丰云：按厂商名找必须报错，不能默默挑第一台
        let env = env_of(&[
            ("SANFENGYUN_USERNAME", "u1"),
            ("SANFENGYUN_PASSWORD", "p1"),
            ("SANFENGYUN_USERNAME_2", "u2"),
            ("SANFENGYUN_PASSWORD_2", "p2"),
        ]);
        let cfg = AppConfig {
            accounts: build_accounts(sanfengyun(), &[], &env),
            llm: None,
            platform_provider: "csdn".into(),
            platform_fallback: None,
            csdn: None,
            zhihu: None,
            notify: NotifyConfig {
                webhook_url: String::new(),
                tag: "renewal".into(),
                openclaw: None,
                pushplus_token: String::new(),
            },
            article_ready_timeout: 1,
            http_timeout: 1,
        };
        assert!(cfg.find_account("三丰云").is_err(), "歧义必须报错");
        assert_eq!(cfg.find_account("sanfengyun-2").unwrap().username, "u2");
        let msg = format!("{:#}", cfg.find_account("三丰云").unwrap_err());
        assert!(msg.contains("sanfengyun-1"), "错误里要列出可用 ID：{msg}");
    }

    #[test]
    fn debug_never_prints_secrets() {
        let env = env_of(&[
            ("SANFENGYUN_USERNAME", "13812345678"),
            ("SANFENGYUN_PASSWORD", "sup3r-s3cret"),
        ]);
        let acc = &build_accounts(sanfengyun(), &[], &env)[0];
        let dumped = format!("{acc:?}");
        assert!(
            !dumped.contains("sup3r-s3cret"),
            "密码泄漏进了 Debug: {dumped}"
        );
        assert!(!dumped.contains("13812345678"), "手机号未打码: {dumped}");
        assert!(dumped.contains("sanfengyun-1"));
    }
}
