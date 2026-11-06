use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub site_url: String,
    pub admin_origin: String,
    pub rp_id: String,
    pub rp_origin: String,
    pub content_root: PathBuf,
    /// 是否信任 `X-Forwarded-For`。只有当服务确实只经由可信 CDN/反代进来时才开。
    pub trust_proxy_headers: bool,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 只认显式的真值，没设置一律按 false：限流用的客户端 IP 一旦信错来源，
/// 攻击者改个请求头就能绕过所有限制，所以默认必须是不信任。
fn env_flag(key: &str) -> bool {
    matches!(
        std::env::var(key).map(|value| value.to_ascii_lowercase()).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        Ok(Self {
            bind: env_or("BIND", "127.0.0.1:8080"),
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL 未设置")?,
            site_url: env_or("SITE_URL", "https://www.kych.net"),
            admin_origin: env_or("ADMIN_ORIGIN", "https://admin.kych.net"),
            rp_id: env_or("WEBAUTHN_RP_ID", "kych.net"),
            rp_origin: env_or("WEBAUTHN_ORIGIN", "https://admin.kych.net"),
            content_root: PathBuf::from(env_or("CONTENT_ROOT", "content")),
            trust_proxy_headers: env_flag("TRUST_PROXY_HEADERS"),
        })
    }
}
