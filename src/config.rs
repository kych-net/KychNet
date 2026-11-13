use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub site_url: String,
    /// 管理端的 origin，必须是纯 scheme://host[:port]：CORS 与写接口的
    /// Origin 白名单都拿它做等值比较。
    pub admin_origin: String,
    /// 管理端的可点击地址（含 `/管理/` 子路径），用于页面里「管理端」之类的链接。
    pub admin_url: String,
    pub rp_id: String,
    pub rp_origin: String,
    /// 通行密钥对话框里显示的依赖方名称；缺省回退 rp_id。
    pub webauthn_rp_name: String,
    pub content_root: PathBuf,
    /// 站点产物输出目录，`build` 子命令与管理端触发的重建共用这个路径。
    /// serve 进程需要知道它，才能把重建请求落在对的地方。
    pub build_out_dir: PathBuf,
    /// 是否信任 `X-Forwarded-For`。只有当服务确实只经由可信 CDN/反代进来时才开。
    pub trust_proxy_headers: bool,
    /// Git LFS 二进制对象的落盘根目录。
    pub lfs_root: PathBuf,
    /// S3 磁盘 Git 对象的根目录（packfile + `.idx`）。只有
    /// `GIT_OBJECT_BACKEND=disk` 时才会用到，默认与升级前行为一致。
    pub git_root: PathBuf,
    /// Git 对象后端：`pg`（默认，PG 唯一权威）或 `disk`（双写 + 可 offload）。
    pub git_object_backend: String,
    /// 自动 offload 门槛：项目 PG 里的 git_objects 行数超过该值时，后台任务
    /// 自动把冷历史导出磁盘 pack 并删除 PG 冷行，PG 只留默认分支 tip 热缓存。
    /// 0 表示禁用。仅在 GIT_OBJECT_BACKEND=disk 时生效。
    pub git_offload_min_objects: usize,
    /// 自动 offload 扫描间隔（秒）。
    pub git_offload_interval_secs: u64,
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

/// 数值环境变量：未设置或解析失败一律回落默认值。
fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
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
        // 内容站与管理端同域：内容站在 `/`，管理端在 `/管理/`（nginx alias）。
        // ADMIN_ORIGIN 会被 CORS 与写接口的 Origin 校验直接比较，必须是纯
        // origin（带路径会永远匹配不上），所以页面里可点击的管理端地址另用
        // ADMIN_URL，默认由 ADMIN_ORIGIN 拼出。
        let site_url = env_or("SITE_URL", "https://kych.net");
        let admin_origin = env_or("ADMIN_ORIGIN", "https://kych.net");
        let admin_url = env_or(
            "ADMIN_URL",
            &format!("{}/管理/", admin_origin.trim_end_matches('/')),
        );
        Ok(Self {
            bind: env_or("BIND", "127.0.0.1:8080"),
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL 未设置")?,
            site_url,
            admin_origin,
            admin_url,
            // WebAuthn 的 RP ID 取可注册域本身（kych.net），而不是子域。
            // RP ID 是凭据的绑定域：写成子域的话，日后管理端再换域
            // 就会让已注册的通行密钥全部失效。
            rp_id: env_or("WEBAUTHN_RP_ID", "kych.net"),
            rp_origin: env_or("WEBAUTHN_ORIGIN", "https://kych.net"),
            webauthn_rp_name: env_or("WEBAUTHN_RP_NAME", ""),
            content_root: PathBuf::from(env_or("CONTENT_ROOT", "content")),
            build_out_dir: PathBuf::from(env_or("BUILD_OUT_DIR", "dist")),
            trust_proxy_headers: env_flag("TRUST_PROXY_HEADERS"),
            lfs_root: PathBuf::from(env_or("LFS_ROOT", "lfs")),
            git_root: PathBuf::from(env_or("GIT_ROOT", "git-objects")),
            git_object_backend: env_or("GIT_OBJECT_BACKEND", "pg"),
            // 默认 5000 行起步：小仓库 offload 收益抵不过复杂度，大仓库才值得。
            git_offload_min_objects: env_num("GIT_OFFLOAD_MIN_OBJECTS", 5000),
            git_offload_interval_secs: env_num("GIT_OFFLOAD_INTERVAL_SECS", 3600),
            // SSH 不设默认值：不显式开启就不监听，行为与升级前完全一致。
            ssh_bind: std::env::var("SSH_BIND").ok().filter(|value| !value.trim().is_empty()),
            ssh_host_key: PathBuf::from(env_or("SSH_HOST_KEY", "ssh_host_ed25519_key")),
            git_http_base: env_or("GIT_HTTP_BASE_URL", "https://kych.net"),
        })
    }
}
