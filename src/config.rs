//! 运行时配置：文件配置(config.toml) + 环境变量合并。
//!
//! 优先级：环境变量 > config.toml > 内置默认值。
//! 此模块是全程序唯一的配置出口，其余模块只看这里的类型。

use crate::file_config::FileConfig;

/// 云厂商 API 端点与协议参数（源自 2020 FreeServer 逆向 + 2026-09 实测存活）。
#[derive(Debug, Clone)]
pub struct CloudProfile {
    pub key: &'static str,
    pub name: &'static str,
    pub login_url: &'static str,
    pub renew_url: &'static str,
    /// https 被境外 WAF 拦时降级 http（阿贝云 80 实测通）
    pub allow_http_fallback: bool,
}

pub static CLOUDS: &[CloudProfile] = &[
    CloudProfile {
        key: "sanfengyun",
        name: "三丰云",
        login_url: "https://api.sanfengyun.com/www/login.php",
        renew_url: "https://api.sanfengyun.com/www/renew.php",
        allow_http_fallback: false,
    },
    CloudProfile {
        key: "abeiyun",
        name: "阿贝云",
        login_url: "https://api.abeiyun.com/www/login.php",
        renew_url: "https://api.abeiyun.com/www/renew.php",
        allow_http_fallback: true,
    },
];

pub fn cloud_profile(key: &str) -> Option<&'static CloudProfile> {
    CLOUDS.iter().find(|p| p.key == key)
}

#[derive(Debug, Clone)]
pub struct CloudAccount {
    pub profile: &'static CloudProfile,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub struct CsdnConfig {
    pub cookie: String,
    pub creation_statement: u8,
    pub tags: Vec<String>,
    pub categories: Vec<String>,
    pub app_secret: String,
    pub x_ca_key: String,
}

#[derive(Debug, Clone)]
pub struct NotifyConfig {
    pub webhook_url: String,
    pub tag: String,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub accounts: Vec<CloudAccount>,
    pub llm: Option<LlmConfig>,
    pub platform_provider: String,
    pub csdn: Option<CsdnConfig>,
    pub notify: NotifyConfig,
    pub article_ready_timeout: u64,
    pub http_timeout: u64,
}

impl AppConfig {
    /// 合并文件配置与环境变量。`config_path` 为 --config 显式传入。
    pub fn load(config_path: Option<&std::path::Path>) -> Self {
        let file: Option<FileConfig> = match FileConfig::find(config_path) {
            Some(Ok(c)) => Some(c),
            Some(Err(e)) => {
                // 配置文件存在但坏 = 硬错误，宁可不开机也不带病运行
                panic!("{e}");
            }
            None => None,
        };

        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());

        // ---- 云账号：env 优先，文件兜底；enabled=false 跳过 ----
        let mut accounts = Vec::new();
        for profile in CLOUDS {
            let key_upper = profile.key.to_uppercase();
            let file_acc = file.as_ref().and_then(|f| {
                let opt = match profile.key {
                    "sanfengyun" => f.clouds.sanfengyun.as_ref(),
                    "abeiyun" => f.clouds.abeiyun.as_ref(),
                    _ => None,
                };
                opt.cloned()
            });
            let username = env(&format!("{key_upper}_USERNAME"))
                .or_else(|| file_acc.as_ref().map(|a| a.username.clone()));
            let password = env(&format!("{key_upper}_PASSWORD"))
                .or_else(|| file_acc.as_ref().map(|a| a.password.clone()));
            let enabled = file_acc.as_ref().map(|a| a.enabled).unwrap_or(true);
            if !enabled {
                tracing::info!("[config] {} 被 enabled=false 禁用", profile.name);
                continue;
            }
            if let (Some(u), Some(p)) = (username, password) {
                accounts.push(CloudAccount {
                    profile,
                    username: u,
                    password: p,
                });
            } else {
                tracing::info!("[config] {} 未配置凭据，跳过", profile.name);
            }
        }

        // ---- LLM ----
        let ai_file = file.as_ref().map(|f| f.ai.clone()).unwrap_or_default();
        let llm = match (
            env("LLM_BASE_URL").or(ai_file.base_url.clone()),
            env("LLM_API_KEY").or(ai_file.api_key.clone()),
            env("LLM_MODEL").or(ai_file.model.clone()),
        ) {
            (Some(base), Some(key), Some(model)) => Some(LlmConfig {
                base_url: base.trim_end_matches('/').to_string(),
                api_key: key,
                model,
                angles: if ai_file.angles.is_empty() {
                    crate::writer::DEFAULT_ANGLES.iter().map(|s| s.to_string()).collect()
                } else {
                    ai_file.angles.clone()
                },
                lengths: if ai_file.lengths.is_empty() {
                    crate::writer::DEFAULT_LENGTHS.to_vec()
                } else {
                    ai_file.lengths.clone()
                },
                forbidden_words: if ai_file.forbidden_words.is_empty() {
                    crate::writer::DEFAULT_FORBIDDEN.iter().map(|s| s.to_string()).collect()
                } else {
                    ai_file.forbidden_words.clone()
                },
                required_keywords: ai_file.required_keywords.clone(),
                max_retries: ai_file.max_retries,
            }),
            _ => None,
        };

        // ---- 发文平台 ----
        let provider = env("PLATFORM_PROVIDER")
            .or(file.as_ref().and_then(|f| f.platform.provider.clone()))
            .unwrap_or_else(|| "csdn".to_string());
        let csdn = file
            .as_ref()
            .and_then(|f| f.platform.csdn.clone())
            .map(|c| CsdnConfig {
                cookie: env("CSDN_COOKIES").unwrap_or(c.cookie),
                creation_statement: c.creation_statement,
                tags: c.tags,
                categories: c.categories,
                app_secret: c.app_secret.unwrap_or_else(|| crate::csdn::DEFAULT_APP_SECRET.into()),
                x_ca_key: c.x_ca_key.unwrap_or_else(|| crate::csdn::DEFAULT_X_CA_KEY.into()),
            })
            .or_else(|| {
                env("CSDN_COOKIES").map(|cookie| CsdnConfig {
                    cookie,
                    creation_statement: 1,
                    tags: vec!["云服务器".into()],
                    categories: vec![],
                    app_secret: crate::csdn::DEFAULT_APP_SECRET.into(),
                    x_ca_key: crate::csdn::DEFAULT_X_CA_KEY.into(),
                })
            });

        // ---- 通知 ----
        let notify_file = file.as_ref().map(|f| f.notify.clone()).unwrap_or_default();
        let notify = NotifyConfig {
            webhook_url: env("NOTIFY_WEBHOOK_URL").unwrap_or(notify_file.webhook_url),
            tag: notify_file.tag,
        };

        // ---- 限额 ----
        let limits = file.as_ref().map(|f| f.limits.clone()).unwrap_or_default();
        let article_ready_timeout = env("ARTICLE_READY_TIMEOUT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(limits.article_ready_timeout);
        let http_timeout = env("HTTP_TIMEOUT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(limits.http_timeout);

        Self {
            accounts,
            llm,
            platform_provider: provider,
            csdn,
            notify,
            article_ready_timeout,
            http_timeout,
        }
    }
}
