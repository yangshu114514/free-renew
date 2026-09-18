//! 文件配置模型（config.toml）。
//!
//! 配置加载优先级：环境变量 > config.toml > 内置默认值。
//! 环境变量层保证 GitHub Actions Secrets 场景零文件依赖；
//! 文件层保证本地开发与自托管场景一键配置。
//!
//! 敏感项（密码/cookie/api key）既可写在文件也可走 env：
//!   [clouds.sanfengyun] password = "..."          # 文件方式
//!   SANFENGYUN_PASSWORD=...                       # env 方式（覆盖文件）
//!
//! ## 多账号
//!
//! 同一厂商可配任意多个账号（例如两台三丰云 + 三台阿贝云）。两种写法等价，
//! 旧配置一行都不用改：
//!
//! ```toml
//! # 数组表：该厂商的第 1、2…个账号（新写法，账户数不限）
//! [[clouds.sanfengyun]]
//! username = "13800000000"
//! password = "pw-1"
//! label    = "主力"          # 可选，通知/日志里显示为「三丰云(主力)」
//!
//! [[clouds.sanfengyun]]
//! username = "13900000000"
//! password = "pw-2"
//! label    = "备用"
//!
//! # 单表：等价于"该厂商只有一个账号"（旧写法，继续支持）
//! [clouds.abeiyun]
//! username = "13800000000"
//! password = "pw"
//! ```
//!
//! 厂商 key（`sanfengyun`/`abeiyun`/未来的第三家）**不写死在结构体里**：
//! `clouds` 是一张「厂商 key → 账号列表」的表，新增厂商只需在
//! `config::CLOUDS` 加一条元数据，本文件与 config.rs 的装配循环都不必改。

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// config.toml 的完整 schema。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    #[serde(default)]
    pub clouds: CloudsSection,
    #[serde(default)]
    pub platform: PlatformSection,
    #[serde(default)]
    pub ai: AiSection,
    #[serde(default)]
    pub notify: NotifySection,
    #[serde(default)]
    pub limits: LimitsSection,
}

/// `[clouds]` 段：厂商 key → 该厂商的账号（一个或一组）。
pub type CloudsSection = BTreeMap<String, OneOrMany<CloudAccountConfig>>;

/// TOML 的单表与数组表共用一个 Rust 类型。
///
/// `[clouds.x]`（表）与 `[[clouds.x]]`（数组表）在 TOML 里是两种形状，
/// 但用户意图相同——前者是"只有一台"，后者是"有多台"。untagged 让 serde
/// 先试单表再试数组，于是旧配置的 `[clouds.sanfengyun]` 不需要任何迁移。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(Box<T>),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    /// 摊平成列表：单表 → 单元素列表。
    pub fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(one) => vec![*one],
            Self::Many(many) => many,
        }
    }
}

/// 单个云账号的凭据。username/password 允许缺省（由编号环境变量提供），
/// 这样文件里可以只写 `label`，凭据全走 Secrets。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CloudAccountConfig {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// false = 本轮跳过该账号（不改用户名密码即可临时停用一台）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 通知/日志里的人类可读名字（可选）。默认用厂商名，配了就显示为「三丰云(主力)」。
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlatformSection {
    /// 发文平台：csdn | zhihu
    pub provider: Option<String>,
    /// 主平台发文失败时的兜底：csdn | zhihu | none(关)。留空=自动：
    /// 两家 Cookie 都配了就互为备份（知乎为主失败切 CSDN）
    pub fallback: Option<String>,
    pub csdn: Option<CsdnPlatformConfig>,
    pub zhihu: Option<ZhihuPlatformConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ZhihuPlatformConfig {
    /// 单行 "k=v; k=v" 形态 Cookie，须含 z_c0、_xsrf、d_c0、q_c1（DevTools 手动复制）
    pub cookie: String,
    /// 发文必挂话题（不挂通常发不出去）。逐个精确/前缀匹配，含其它云品牌名的候选自动排除。
    #[serde(default = "default_zhihu_topics")]
    pub topics: Vec<String>,
    /// 是否开启目录（table_of_contents）
    #[serde(default)]
    pub toc: bool,
}

pub fn default_zhihu_topics() -> Vec<String> {
    vec!["免费云服务器".into(), "虚拟主机".into()]
}

/// 纯环境变量场景（GitHub Actions 无 config.toml）用的默认骨架。
/// 放在这里而不是在 config.rs 里再写一份字段与默认值，是为了让"默认值"只有一个来源。
impl Default for ZhihuPlatformConfig {
    fn default() -> Self {
        Self {
            cookie: String::new(),
            topics: default_zhihu_topics(),
            toc: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CsdnPlatformConfig {
    /// 单行 k=v; k=v 形态的完整 Cookie（采集器产出）
    pub cookie: String,
    /// CSDN 创作声明：0=无 1=AI辅助 2=整合 3=个人观点。默认 1（诚实声明）。
    #[serde(default = "default_creation_statement")]
    pub creation_statement: u8,
    #[serde(default = "default_tags")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    /// 可覆盖协议常量（CSDN 换 secret 时无需重编译）
    pub app_secret: Option<String>,
    pub x_ca_key: Option<String>,
}

/// 纯环境变量场景的默认骨架，理由同 `ZhihuPlatformConfig`。
impl Default for CsdnPlatformConfig {
    fn default() -> Self {
        Self {
            cookie: String::new(),
            creation_statement: default_creation_statement(),
            tags: default_tags(),
            categories: vec![],
            app_secret: None,
            x_ca_key: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AiSection {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    /// 生成角度池（每篇随机抽一个，保证文章不重复）
    #[serde(default)]
    pub angles: Vec<String>,
    /// 目标字数池
    #[serde(default)]
    pub lengths: Vec<usize>,
    /// 禁词表（三丰云审核红线等）
    #[serde(default)]
    pub forbidden_words: Vec<String>,
    /// 必含关键词
    #[serde(default)]
    pub required_keywords: Vec<String>,
    /// 生成重试次数（至少 1，0 会让生成流程一次都不跑）
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// 采样温度（留空用内置 1.0）
    pub temperature: Option<f32>,
    /// 是否发送 `enable_thinking=false`（ModelScope Qwen3 系需要；其它供应商可关）
    #[serde(default = "default_true")]
    pub disable_thinking: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NotifySection {
    #[serde(default)]
    pub webhook_url: String,
    #[serde(default = "default_notify_tag")]
    pub tag: String,
    /// OpenClaw 通知后端：配置后走网关 chatCompletions → agent → 微信
    #[serde(default)]
    pub openclaw: Option<OpenClawNotifyConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenClawNotifyConfig {
    /// 网关 chatCompletions 公网地址（如 https://<你的域名或IP:端口>/v1/chat/completions）
    pub url: String,
    pub basic_user: String,
    pub basic_password: String,
    /// 固定 "openclaw"（网关按 agentId 路由）
    #[serde(default = "default_openclaw_model")]
    pub model: String,
}

pub fn default_openclaw_model() -> String {
    "openclaw".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsSection {
    /// 等 Pages/平台文章可访问的超时（秒）
    #[serde(default = "default_article_wait")]
    pub article_ready_timeout: u64,
    /// 单请求超时（秒）
    #[serde(default = "default_http_timeout")]
    pub http_timeout: u64,
}

impl Default for LimitsSection {
    fn default() -> Self {
        Self {
            article_ready_timeout: default_article_wait(),
            http_timeout: default_http_timeout(),
        }
    }
}

/// 默认值只有这一处定义：config.rs 的纯环境变量分支也调这里，不再各抄一份。
pub fn default_true() -> bool {
    true
}
pub fn default_creation_statement() -> u8 {
    1
}
pub fn default_tags() -> Vec<String> {
    vec!["云服务器".into()]
}
pub fn default_max_retries() -> u32 {
    3
}
pub fn default_notify_tag() -> String {
    "renewal".into()
}
pub fn default_temperature() -> f32 {
    1.0
}
fn default_article_wait() -> u64 {
    // 裸 HTTP 就绪检查必被 CSDN WAF 521，300s 纯属空等；真正门禁在 Chrome 截图。
    // 压到 60s：给刚发的文章一点索引时间即可，省下的预算留给网络重试
    60
}
fn default_http_timeout() -> u64 {
    30
}

impl FileConfig {
    /// 从 TOML 文件解析。
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读配置文件 {} 失败: {e}", path.display()))?;
        let cfg: FileConfig = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("配置文件 {} 解析失败: {e}", path.display()))?;
        Ok(cfg)
    }

    /// 查找配置文件：显式路径 > ./config.toml。
    ///
    /// 返回 `Result<Option<Self>>`：`Ok(None)`=没有配置文件（纯 env 模式，合法），
    /// `Err`=文件存在但读不了/解析不了（硬错误，调用方据此明确报错而不是 panic）。
    pub fn find(explicit: Option<&Path>) -> anyhow::Result<Option<Self>> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => {
                let local = Path::new("config.toml");
                if !local.exists() {
                    return Ok(None);
                }
                local.to_path_buf()
            }
        };
        Self::load(&path).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧写法（单表）必须继续可解析——线上已有配置一行不改也要能跑。
    #[test]
    fn legacy_single_table_still_parses() {
        let cfg: FileConfig = toml::from_str(
            r#"
[clouds.sanfengyun]
username = "13800000000"
password = "pw"
enabled = true

[clouds.abeiyun]
username = "13800000001"
password = "pw2"
"#,
        )
        .expect("旧格式必须可解析");
        let sf = cfg
            .clouds
            .get("sanfengyun")
            .expect("三丰云段")
            .clone()
            .into_vec();
        assert_eq!(sf.len(), 1);
        assert_eq!(sf[0].username, "13800000000");
        assert!(sf[0].enabled, "enabled 缺省为 true");
        assert!(sf[0].label.is_none());
        assert_eq!(
            cfg.clouds.get("abeiyun").unwrap().clone().into_vec().len(),
            1
        );
    }

    /// 新写法：同一厂商多个账号。
    #[test]
    fn multi_account_array_parses_in_order() {
        let cfg: FileConfig = toml::from_str(
            r#"
[[clouds.sanfengyun]]
username = "13800000000"
password = "pw1"
label = "主力"

[[clouds.sanfengyun]]
username = "13900000000"
password = "pw2"
label = "备用"

[[clouds.sanfengyun]]
username = "13700000000"
password = "pw3"
enabled = false
"#,
        )
        .expect("数组表必须可解析");
        let sf = cfg
            .clouds
            .get("sanfengyun")
            .expect("三丰云段")
            .clone()
            .into_vec();
        assert_eq!(sf.len(), 3, "三个账号一个都不能丢");
        assert_eq!(sf[0].label.as_deref(), Some("主力"));
        assert_eq!(sf[1].username, "13900000000");
        assert!(!sf[2].enabled, "enabled=false 要读到");
    }

    /// 只写 label、凭据走编号环境变量的写法（Actions 场景）。
    #[test]
    fn credentialless_slot_allowed() {
        let cfg: FileConfig = toml::from_str(
            r#"
[[clouds.abeiyun]]
label = "备用机"
"#,
        )
        .expect("只写 label 必须可解析");
        let ab = cfg.clouds.get("abeiyun").unwrap().clone().into_vec();
        assert_eq!(ab.len(), 1);
        assert!(ab[0].username.is_empty());
        assert!(ab[0].enabled);
    }
}
