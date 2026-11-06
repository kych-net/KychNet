use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub admin_origin: String,
    pub rp_id: String,
    pub rp_origin: String,
    pub content_root: PathBuf,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        Ok(Self {
            bind: env_or("BIND", "127.0.0.1:8080"),
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL 未设置")?,
            admin_origin: env_or("ADMIN_ORIGIN", "https://admin.kych.net"),
            rp_id: env_or("WEBAUTHN_RP_ID", "kych.net"),
            rp_origin: env_or("WEBAUTHN_ORIGIN", "https://admin.kych.net"),
            content_root: PathBuf::from(env_or("CONTENT_ROOT", "content")),
        })
    }
}
