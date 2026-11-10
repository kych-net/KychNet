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
    /// 站点产物输出目录，`build` 子命令与管理端触发的重建共用这个路径。
    /// serve 进程需要知道它，才能把重建请求落在对的地方。
    pub build_out_dir: PathBuf,
    /// 是否信任 `X-Forwarded-For`。只有当服务确实只经由可信 CDN/反代进来时才开。
    pub trust_proxy_headers: bool,
    /// Git LFS 二进制对象的落盘根目录。
    pub lfs_root: PathBuf,
    /// Git over SSH 服务监听地址（如 `0.0.0.0:2222`）；未设置则不启动 SSH 服务。
    pub ssh_bind: Option<String>,
    /// SSH 主机私钥文件路径，首次启动自动生成。
    pub ssh_host_key: PathBuf,
    /// Git HTTP 服务的对外根地址。SSH 侧没有 Host 头可用，
    /// 拼 LFS 的 batch 端点时需要显式配置；换域名或自托管时覆盖。
    pub git_http_base: String,
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
            // 内容站、管理端、API 是三个独立子域：内容站走 CDN 缓存的静态产物，
            // 管理端是 EdgeOne Pages 上的独立静态站，API 才是有源站的后端。
            site_url: env_or("SITE_URL", "https://c.kych.net"),
            admin_origin: env_or("ADMIN_ORIGIN", "https://m.kych.net"),
            // WebAuthn 的 RP ID 取可注册域本身（kych.net），而不是子域。
            // RP ID 是凭据的绑定域：写成 m.kych.net 的话，日后管理端再换子域
            // 就会让已注册的通行密钥全部失效。
            rp_id: env_or("WEBAUTHN_RP_ID", "kych.net"),
            rp_origin: env_or("WEBAUTHN_ORIGIN", "https://m.kych.net"),
            content_root: PathBuf::from(env_or("CONTENT_ROOT", "content")),
            build_out_dir: PathBuf::from(env_or("BUILD_OUT_DIR", "dist")),
            trust_proxy_headers: env_flag("TRUST_PROXY_HEADERS"),
            lfs_root: PathBuf::from(env_or("LFS_ROOT", "lfs")),
            // SSH 不设默认值：不显式开启就不监听，行为与升级前完全一致。
            ssh_bind: std::env::var("SSH_BIND").ok().filter(|value| !value.trim().is_empty()),
            ssh_host_key: PathBuf::from(env_or("SSH_HOST_KEY", "ssh_host_ed25519_key")),
            git_http_base: env_or("GIT_HTTP_BASE_URL", "https://kych.net"),
        })
    }
}
