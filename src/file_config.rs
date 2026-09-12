//! 文件配置模型（config.toml）。
//!
//! 配置加载优先级：环境变量 > config.toml > 内置默认值。
//! 环境变量层保证 GitHub Actions Secrets 场景零文件依赖；
//! 文件层保证本地开发与自托管场景一键配置。
//!
//! 敏感项（密码/cookie/api key）既可写在文件也可走 env：
//!   [clouds.sanfengyun] password = "..."          # 文件方式
//!   SANFENGYUN_PASSWORD=...                       # env 方式（覆盖文件）

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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CloudsSection {
    pub sanfengyun: Option<CloudAccountConfig>,
    pub abeiyun: Option<CloudAccountConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudAccountConfig {
    pub username: String,
    pub password: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for CloudAccountConfig {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            enabled: default_true(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlatformSection {
    /// 发文平台：csdn | zhihu
    pub provider: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct CsdnPlatformConfig {
    /// 单行 k=v; k=v 形态的完整 Cookie（采集器产出）
    pub cookie: String,
    #[serde(default = "default_creation_statement")]
    /// CSDN 创作声明：0=无 1=AI辅助 2=整合 3=个人观点。默认 1（诚实声明）。
    pub creation_statement: u8,
    #[serde(default = "default_tags")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    /// 可覆盖协议常量（CSDN 换 secret 时无需重编译）
    pub app_secret: Option<String>,
    pub x_ca_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
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
    /// 生成重试次数
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, Deserialize)]
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

fn default_openclaw_model() -> String {
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

impl Default for NotifySection {
    fn default() -> Self {
        Self {
            webhook_url: String::new(),
            tag: default_notify_tag(),
            openclaw: None,
        }
    }
}

impl Default for LimitsSection {
    fn default() -> Self {
        Self {
            article_ready_timeout: default_article_wait(),
            http_timeout: default_http_timeout(),
        }
    }
}

impl Default for AiSection {
    fn default() -> Self {
        Self {
            base_url: None,
            api_key: None,
            model: None,
            angles: vec![],
            lengths: vec![],
            forbidden_words: vec![],
            required_keywords: vec![],
            max_retries: default_max_retries(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_creation_statement() -> u8 {
    1
}
fn default_tags() -> Vec<String> {
    vec!["云服务器".into()]
}
fn default_max_retries() -> u32 {
    3
}
fn default_notify_tag() -> String {
    "renewal".into()
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

    /// 查找配置文件：显式路径 > ./config.toml。找不到返回 None（纯 env 模式合法）。
    pub fn find(explicit: Option<&Path>) -> Option<anyhow::Result<Self>> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => {
                let local = Path::new("config.toml");
                if !local.exists() {
                    return None;
                }
                local.to_path_buf()
            }
        };
        Some(Self::load(&path))
    }
}
