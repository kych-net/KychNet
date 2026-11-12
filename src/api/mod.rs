use anyhow::{anyhow, bail, Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::{cookie::SameSite, ExpiredDeletion, Expiry, Session, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;
use uuid::Uuid;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
};

use crate::auth::Auth;
use crate::config::Config;
use crate::models::{ContentFormat, SiteSettings, User};
use crate::oauth;
use crate::rate::RateLimiter;
use crate::render;
use crate::service;

pub mod admin;
pub mod auth;
pub mod build;
pub mod content;
pub mod git;
pub mod pr;
pub mod project;

// 路由表集中在 serve() 里注册，把各子模块的 handler 展平到本模块作用域。
use admin::*;
use auth::*;
use build::*;
use content::*;
use git::*;
use pr::*;
use project::*;

// `BuildCtl` 原本定义在 api.rs 顶层，workflow 等模块按 `crate::api::BuildCtl` 引用。
pub(crate) use build::BuildCtl;

/// 全局请求体上限。Typst / Markdown 正文写满 2 MiB 已经是极限量级。
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Git 推送的 pack 与 Git LFS 上传共用的请求体上限。
/// LFS 二进制可能较大，这里放宽到 256 MiB；普通文本推送远小于此。
const GIT_MAX_BODY_BYTES: usize = 256 * 1024 * 1024;
/// 头像上限：比 svg.rs 自己的 256 KB 稍大，让「超长」由 svg.rs 报出中文提示，
/// 而不是被传输层截成一句 413。
const AVATAR_MAX_BODY_BYTES: usize = 256 * 1024 + 1024;

/// 会话最长存活时间。用 OnInactivity 表达，但只有会话被写入时才会顺延，
/// 实际效果就是「登录后最多 8 小时」，配合下面各处 `cycle_id` 使用。
const SESSION_TTL: time::Duration = time::Duration::hours(8);

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) pool: PgPool,
    pub(crate) auth: Arc<Auth>,
    pub(crate) site_url: String,
    /// 管理端可点击地址（含 /管理/ 子路径）；与 admin_origin 分开，后者只能给 CORS 用。
    pub(crate) admin_url: String,
    pub(crate) limits: RateLimiter,
    pub(crate) trust_proxy_headers: bool,
    /// 预览 Markdown / Wikidot 时需要的字体与资源根目录。
    pub(crate) content_root: PathBuf,
    /// 管理端触发整站重建的协调器。构建独立成子进程跑，serve 自身
    /// 不引入 Typst 渲染逻辑（见 `BuildCtl` 注释）。
    pub(crate) build_ctl: Arc<BuildCtl>,
    /// 与 workflow 模块共享的发布队列：worktree 写入不再同步阻塞构建。
    pub(crate) build_queue: crate::workflow::queue::BuildQueue,
    /// Git LFS 二进制对象的落盘根目录。
    pub(crate) lfs_root: PathBuf,
    /// archive 打包的并发上限。tar.gz 是同步 CPU 密集操作，必须放进
    /// `spawn_blocking` 并限制同时进行数，否则一次突发的大仓库归档会把
    /// Tokio worker 与 PG 连接占满，连 TLS 握手都排不上队。
    pub(crate) archive_slots: Arc<tokio::sync::Semaphore>,
}


pub(crate) struct ApiError {
    error: anyhow::Error,
    /// 显式指定的状态码；None 时业务错误按 400 处理。
    status: Option<StatusCode>,
}

impl ApiError {
    fn with_status(code: StatusCode, message: impl std::fmt::Display) -> Self {
        Self {
            error: anyhow!("{message}"),
            status: Some(code),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 数据库、文件系统等底层错误可能携带约束名、列名、绝对路径等内部细节，
        // 不能回显给客户端；只记日志并返回统一的 500。
        let internal = self
            .error
            .chain()
            .any(|cause| cause.is::<sqlx::Error>() || cause.is::<std::io::Error>());
        let (status, message) = if internal {
            tracing::error!("API 内部错误: {:#}", self.error);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "服务器内部错误".to_string(),
            )
        } else {
            let status = self.status.unwrap_or(StatusCode::BAD_REQUEST);
            tracing::warn!("API 请求被拒绝({status}): {}", self.error);
            (status, self.error.to_string())
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self {
            error: error.into(),
            status: None,
        }
    }
}

type ApiResult<T> = Result<T, ApiError>;

macro_rules! api_bail {
    ($($arg:tt)*) => {
        return Err(ApiError::from(anyhow!($($arg)*)))
    };
}

/// 带明确状态码的拒绝，用于 401 / 403 等语义错误。
macro_rules! api_reject {
    ($code:expr, $($arg:tt)*) => {
        return Err(ApiError::with_status($code, format!($($arg)*)))
    };
}

pub(crate) use api_bail;
pub(crate) use api_reject;

#[derive(Clone)]
pub(crate) struct OriginPolicy {
    admin_origin: String,
    /// OAuth 授权页/表单运行在 API 裸域上，允许该 origin 提交登录与同意表单。
    self_origin: String,
}

/// 拒绝来自管理端以外 Origin 的状态变更请求。
///
/// 内容站 `www` 与 API 属于同一 eTLD+1（同站），`SameSite` 不会阻止同站跨源请求，
/// CORS 也只阻止读取响应、不阻止带凭据的写请求被发出。因此内容站一旦出现 XSS，
/// 就能用 `fetch(credentials:'include')` 冒用管理员会话调用写接口。
/// 这里显式校验 Origin，是「子域隔离」真正生效所必需的一环。
/// 没有 Origin 头的请求（curl、服务端调用）保持放行，不影响 CLI 与非浏览器客户端。
pub(crate) async fn guard_origin(
    State(policy): State<OriginPolicy>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let is_write = method == Method::POST
        || method == Method::PUT
        || method == Method::PATCH
        || method == Method::DELETE;
    if !is_write {
        return next.run(req).await;
    }

    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return next.run(req).await;
    };

    let origin = origin.trim_end_matches('/');
    if origin == policy.admin_origin.trim_end_matches('/')
        || (req.uri().path().starts_with("/oauth/")
            && origin == policy.self_origin.trim_end_matches('/'))
    {
        return next.run(req).await;
    }

    tracing::warn!("拒绝来自非管理端 Origin 的写请求: {origin}");
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "请求来源不被允许" })),
    )
        .into_response()
}

pub(crate) async fn current_user(state: &AppState, session: &Session) -> ApiResult<User> {
    let id: Uuid = match session.get("user_id").await {
        Ok(Some(id)) => id,
        Ok(None) => api_reject!(StatusCode::UNAUTHORIZED, "未登录"),
        Err(e) => api_bail!("读取会话失败: {e}"),
    };
    let user = service::find_user_by_id(&state.pool, id)
        .await?
        .context("会话对应的用户不存在")?;
    if user.disabled {
        api_reject!(StatusCode::FORBIDDEN, "账号已被禁用");
    }
    Ok(user)
}

pub(crate) async fn current_admin(state: &AppState, session: &Session) -> ApiResult<User> {
    let user = current_user(state, session).await?;
    if !user.is_admin {
        api_reject!(StatusCode::FORBIDDEN, "需要管理员权限");
    }
    Ok(user)
}

/// 用户名中禁止出现的特殊字符。空白与控制字符另行判断。
pub(crate) const USERNAME_SPECIAL_CHARS: &[char] = &[
    '/', '\\', '.', ',', ':', ';', '"', '\'', '<', '>', '&', '%', '#', '?', '=', '+', '*', '(',
    ')', '[', ']', '{', '}', '|', '@', '!', '^', '~', '$', '`',
];

pub(crate) fn validate_username(username: &str) -> ApiResult<()> {
    let length = username.chars().count();
    let valid = (3..=32).contains(&length)
        && !username
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || USERNAME_SPECIAL_CHARS.contains(&c));
    if !valid {
        api_bail!("用户名需为 3-32 个字符,不能包含空白、控制字符或 / \\ . 等特殊符号(支持中文与 emoji)");
    }
    Ok(())
}

/// 拒绝 `javascript:` / `data:` / `vbscript:` 等危险协议。
/// 浏览器解析 URL 时会忽略其中的 tab、换行、回车与空格，必须先剔除再比较，
/// 否则 `java\nscript:` 这类写法可以绕过前缀判断。
/// 站点图标与导航链接会原样写进模板的 src / href，属于管理员可控的输入面。
pub(crate) fn safe_url(value: &str) -> bool {
    let normalized: String = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r' | '\0' | ' '))
        .collect();
    !(normalized.starts_with("javascript:")
        || normalized.starts_with("data:")
        || normalized.starts_with("vbscript:"))
}

/// 消耗一个限流额度，超额就返回 429。
pub(crate) fn limited(state: &AppState, key: &str, quota: (u32, u64)) -> ApiResult<()> {
    let (max, seconds) = quota;
    if let Some(retry_after) = state
        .limits
        .check(key, max, std::time::Duration::from_secs(seconds))
    {
        let secs = retry_after.as_secs().max(1);
        tracing::warn!("限流拦截: {key}");
        api_reject!(StatusCode::TOO_MANY_REQUESTS, "尝试过于频繁，请 {secs} 秒后重试");
    }
    Ok(())
}

/// 取客户端 IP 作为限流 key 的一部分。
///
/// 只有显式打开 `TRUST_PROXY_HEADERS` 才读 `X-Forwarded-For`：这个头任何人都能写，
/// 在没有可信 CDN/反代改写它的部署里当成来源，等于让攻击者自己挑限流额度。
/// 另外要注意，放到 Cloudflare/EdgeOne 后面时 `trust_proxy_headers = false`
/// 会让所有请求共用同一个回源 IP，此时按 IP 的额度实际是全局限额，应按部署调整。
pub(crate) fn client_ip(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> String {
    if state.trust_proxy_headers {
        if let Some(value) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
        {
            if let Some(first) = value.split(',').next().map(str::trim) {
                if !first.is_empty() {
                    return first.to_ascii_lowercase();
                }
            }
        }
    }
    peer.ip().to_string()
}

/// 登录相关接口统一的失败响应。
///
/// 「用户不存在」「账号已禁用」「没绑定通行密钥」必须长得一模一样：
/// 这几个接口都是匿名可调的，返回可区分的错误就等于把用户名和每个人的 2FA
/// 配置情况摊开给外面看。真实原因只写日志。
pub(crate) fn login_failed(reason: impl std::fmt::Display) -> ApiError {
    tracing::debug!("登录失败: {reason}");
    ApiError::with_status(StatusCode::UNAUTHORIZED, "用户名或凭证无效")
}

/// 会话里记录「本次认证成功的时间」，改二次验证时要求它足够新鲜。
pub(crate) const STEP_UP_WINDOW_SECS: i64 = 300;

/// 登录/注册成功后轮换会话 ID。
///
/// 不换 ID 的话，攻击者可以事先拿到一个会话 ID 让受害者用上（会话固定），
/// 受害者完成认证后攻击者手里那个 ID 就带着已登录的身份。
///
/// `auth_at` 同时是「最近一次真实认证」的时间戳，用来做敏感操作的 step-up
/// 判断。它写在会话里而不是数据库里，所以换 store、重启进程都会自然失效。
pub(crate) async fn establish_session(session: &Session, user_id: Uuid) -> ApiResult<()> {
    session
        .cycle_id()
        .await
        .map_err(|e| anyhow!("轮换会话失败: {e}"))?;
    session
        .insert("user_id", user_id)
        .await
        .map_err(|e| anyhow!("写入会话失败: {e}"))?;
    session
        .insert("auth_at", Utc::now().timestamp())
        .await
        .map_err(|e| anyhow!("写入会话失败: {e}"))?;
    Ok(())
}

/// 认证成功的时间是否在 step-up 窗口内。抽成纯函数方便单测。
pub(crate) fn step_up_fresh(auth_at: Option<i64>, now: i64) -> bool {
    match auth_at {
        Some(at) => (now - at).abs() < STEP_UP_WINDOW_SECS,
        None => false,
    }
}

/// 修改凭证（绑定 / 替换 / 确认 TOTP、新增通行密钥）前要求最近 5 分钟内
/// 有过一次真实认证。
///
/// 没有这一步的话，只要会话被短暂借用（共享电脑没锁屏、XSS 拿到 cookie、
/// 或者只读到 `/api/me` 的 CSRF），攻击者就能把受害者的二次验证换绑到自己
/// 的设备上，之后用受害者身份正常登录，看起来完全合法。
pub(crate) async fn require_step_up(session: &Session) -> ApiResult<()> {
    let auth_at: Option<i64> = session
        .get("auth_at")
        .await
        .map_err(|e| anyhow!("读取会话失败: {e}"))?;
    if !step_up_fresh(auth_at, Utc::now().timestamp()) {
        api_reject!(
            StatusCode::FORBIDDEN,
            "修改二次验证或通行密钥需要重新登录确认（登录后的 5 分钟内有效）"
        );
    }
    Ok(())
}

/// 协作接口都只对管理端登录用户开放，先确认项目存在再进入具体逻辑。
pub(crate) async fn require_project(state: &AppState, id: Uuid) -> ApiResult<crate::project::Project> {
    match crate::project::get_project(&state.pool, id).await? {
        Some(project) => Ok(project),
        None => Err(ApiError::with_status(StatusCode::NOT_FOUND, "项目不存在")),
    }
}

pub async fn serve(cfg: Config) -> Result<()> {
    let pool = crate::db::connect(&cfg.database_url).await?;
    crate::db::migrate(&pool).await?;
    spawn_flow_pruner(pool.clone());
    if let Err(error) = oauth::gc(&pool).await {
        tracing::warn!(%error, "OAuth 过期数据清理失败");
    }

    // 磁盘对象后端开启时，把大仓库的早期历史定期自动 offload 到磁盘。
    // 未开 disk 后端或门槛为 0 则什么都不做，行为与之前一致。
    if cfg.git_object_backend.eq_ignore_ascii_case("disk") && cfg.git_offload_min_objects > 0 {
        match crate::git::store::disk_store() {
            Some(disk) => spawn_auto_offloader(
                pool.clone(),
                disk,
                cfg.git_offload_min_objects,
                cfg.git_offload_interval_secs,
            ),
            None => tracing::warn!(
                "GIT_OBJECT_BACKEND=disk 但磁盘存储未初始化，自动 offload 不生效"
            ),
        }
    }

    // Git over SSH：显式配置 SSH_BIND 才启动，独立端口监听，
    // 与系统 sshd（登录用）互不相干，也不需要系统账号。
    if let Some(bind) = cfg.ssh_bind.clone() {
        let ssh_pool = pool.clone();
        let host_key = cfg.ssh_host_key.clone();
        let git_http_base = cfg.git_http_base.clone();
        tokio::spawn(async move {
            let ssh_cfg = crate::git::ssh::SshConfig {
                bind,
                host_key,
                git_http_base,
            };
            if let Err(error) = crate::git::ssh::serve(ssh_cfg, ssh_pool).await {
                tracing::error!(%error, "Git SSH 服务启动失败");
            }
        });
    }

    let auth = Arc::new(Auth::new(&cfg.rp_id, &cfg.rp_origin)?);
    let build_ctl = Arc::new(BuildCtl::new(cfg.build_out_dir.clone()));
    let build_queue = crate::workflow::queue::BuildQueue::start(build_ctl.clone());

    // 升级时的一次性清理：早期版本可能把 index.md / index.typ / index.svg
    // 存成网站根目录的独立内容，和主页 index.html 争夺根目录。规则收紧后
    // 自动删掉这些记录（版本历史随外键级联删除），并触发全量构建清掉产物。
    // 幂等：没有匹配记录时什么都不做，重复启动不会重复删。
    match service::cleanup_legacy_root_indexes(&pool).await {
        Ok(removed) if !removed.is_empty() => {
            tracing::info!(?removed, "已清理网站根目录遗留的非 index.html 内容");
            for slug in &removed {
                // slug 已确认不含 `/`，只是输出目录下的一个文件名。
                let path = cfg.build_out_dir.join(slug);
                if path.is_file() {
                    if let Err(error) = std::fs::remove_file(&path) {
                        tracing::warn!(%error, "清理根目录遗留产物失败");
                    }
                }
            }
            build_queue.enqueue();
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "清理网站根目录遗留内容失败"),
    }

    // 归档压缩是同步 CPU 操作：并发数按 CPU 核数封顶 4，留足 worker 给
    // 其它请求。许可覆盖「预检 → 读工作树 → 压缩」整个阶段，避免同一时刻
    // 有太多请求把 PG 连接池也吃满。
    let archive_parallel = std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2)
        .max(1);
    let state = AppState {
        pool,
        auth,
        site_url: cfg.site_url.clone(),
        admin_url: cfg.admin_url.clone(),
        limits: RateLimiter::default(),
        trust_proxy_headers: cfg.trust_proxy_headers,
        content_root: cfg.content_root.clone(),
        build_ctl: build_ctl.clone(),
        build_queue: build_queue.clone(),
        lfs_root: cfg.lfs_root.clone(),
        archive_slots: Arc::new(tokio::sync::Semaphore::new(archive_parallel)),
    };

    let admin_origin: HeaderValue = cfg
        .admin_origin
        .parse()
        .context("ADMIN_ORIGIN 不是合法 origin")?;
    let cors = CorsLayer::new()
        .allow_origin(admin_origin)
        // PUT 必须在列出的方法里：`/api/settings` 用的就是 PUT，而管理端与 API
        // 不同源、请求又带 application/json，一定触发预检。之前漏掉它，
        // 生产环境保存站点设置会被浏览器拦死，本地因为走 vite 代理（同源）测不出来。
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::COOKIE, header::ACCEPT])
        .allow_credentials(true);

    // 会话属性全部写出来，不吃库的默认值：默认虽然是 HttpOnly=true、
    // SameSite=Strict、Secure=true，但有效期是两周且不显式配置的话，
    // 升级依赖就会悄悄变松。
    //
    // SameSite 选 Strict 不影响管理端调 API：`eo.kych.net` 与 `kych.net`
    // 同属 kych.net，属于同站请求，Strict 照样带 Cookie；它只挡住跨站的导航与请求。
    //
    // 会话改放 Postgres（schema `tower_sessions`、表 `session`，由 store 自己建）：
    // MemoryStore 的过期记录只过滤不删除、重启即全员掉登录、多实例不共享，
    // 这三条在这里一次性解决。代价是每个请求多一次按主键的会话读；对个人站
    // 来说远小于它换来的确定性。过期行由下面 spawn 的清理任务按 `ExpiredDeletion`
    // 每 5 分钟删一次。
    let session_store = PostgresStore::new(state.pool.clone());
    session_store
        .migrate()
        .await
        .context("会话表迁移失败")?;
    tokio::spawn(
        session_store
            .clone()
            .continuously_delete_expired(std::time::Duration::from_secs(300)),
    );
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(cfg.rp_origin.starts_with("https"))
        .with_http_only(true)
        .with_same_site(SameSite::Strict)
        .with_expiry(Expiry::OnInactivity(SESSION_TTL));

    let origin_policy = OriginPolicy {
        admin_origin: cfg.admin_origin.clone(),
        self_origin: format!("https://{}", cfg.rp_id),
    };

    // MCP 路由需要的共享资源先克隆出来：state 在 with_state 时会被 move，
    // 而挂载判断要等 layer 链拼完之后才能做。
    let state_pool_for_mcp = state.pool.clone();
    // 工作流模块同样要在 state 被 move 之前取出所需资源。
    let wf_pool = state.pool.clone();
    let wf_build_ctl = state.build_ctl.clone();

    let app = Router::new()
        .route("/api/health", get(|| async { Json(json!({"ok": true})) }))
        .route("/.well-known/oauth-protected-resource", get(oauth::protected_resource))
        .route("/.well-known/oauth-protected-resource/mcp", get(oauth::protected_resource))
        .route("/.well-known/oauth-authorization-server", get(oauth::authorization_server))
        .route("/oauth/register", post(oauth::register))
        .route("/oauth/authorize", get(oauth::authorize))
        .route("/oauth/login", post(oauth::login))
        .route("/oauth/approve", post(oauth::approve))
        .route("/oauth/token", post(oauth::token))
        .route("/api/auth/register", post(register))
        .route("/api/auth/register/finish", post(finish_registration))
        .route("/api/auth/login/start", post(login_start))
        .route("/api/auth/login/finish", post(login_finish))
        .route("/api/auth/totp/login", post(totp_login))
        .route("/api/auth/step-up/totp", post(step_up_totp))
        .route("/api/auth/step-up/passkey/start", post(step_up_passkey_start))
        .route("/api/auth/step-up/passkey/finish", post(step_up_passkey_finish))
        .route("/api/auth/logout", post(logout))
        .route("/api/me", get(me))
        .route(
            "/api/me/avatar",
            // 头像是纯矢量 SVG，svg.rs 自己限 256 KB，这里给一个更小的
            // 传输上限，避免先收满 2 MiB 再判长度。
            post(upload_avatar).layer(DefaultBodyLimit::max(AVATAR_MAX_BODY_BYTES)),
        )
        .route("/api/avatar/{id}", get(avatar))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/totp/setup", post(totp_setup))
        .route("/api/totp/confirm", post(totp_confirm))
        .route("/api/passkey/setup", post(passkey_setup))
        .route("/api/passkey/setup/finish", post(passkey_setup_finish))
        .route("/api/contents", get(contents).post(save_content))
        .route("/api/preview", post(preview_content))
        .route("/api/contents/{id}", delete(delete_content))
        .route("/api/contents/{id}/revisions", get(content_revisions))
        .route("/api/contents/{id}/revisions/{seq}", get(content_revision_body))
        .route(
            "/api/contents/{id}/revisions/{seq}/diff",
            get(content_revision_diff),
        )
        .route(
            "/api/contents/{id}/revisions/{seq}/revert",
            post(content_revision_revert),
        )
        .route("/api/projects", get(project_list).post(project_create))
        // 静态段优先于 `{id}` 匹配：/api/projects/import 不会被当成项目 id。
        .route("/api/projects/import", post(project_import))
        .route(
            "/api/projects/{id}",
            get(project_detail)
                .patch(project_update)
                .delete(project_delete),
        )
        .route("/api/projects/{id}/tree", get(project_tree))
        .route(
            "/api/projects/{id}/file",
            get(project_file)
                .put(project_write_file)
                .delete(project_delete_file),
        )
        .route("/api/projects/{id}/move", post(project_move_file))
        .route("/api/projects/{id}/commit", post(project_commit_changes))
        .route("/api/projects/{id}/commits", get(project_commits))
        .route("/api/projects/{id}/compare", get(project_compare))
        .route(
            "/api/projects/{id}/workflows",
            get(project_workflows).put(project_workflows_update),
        )
        .route(
            "/api/projects/{id}/workflows/dispatch",
            post(project_workflow_dispatch),
        )
        .route("/api/projects/{id}/runs", get(project_runs))
        .route(
            "/api/projects/{id}/commits/{oid}",
            get(project_commit_detail),
        )
        .route("/api/projects/{id}/revert", post(project_revert))
        .route(
            "/api/projects/{id}/branches",
            post(project_branch_create),
        )
        .route(
            "/api/projects/{id}/branches/{*name}",
            delete(project_branch_delete),
        )
        .route("/api/projects/{id}/prs", get(pr_list).post(pr_create))
        .route("/api/projects/{id}/prs/{number}", get(pr_detail))
        .route(
            "/api/projects/{id}/prs/{number}/comments",
            post(pr_add_comment),
        )
        .route("/api/projects/{id}/prs/{number}/merge", post(pr_merge))
        .route("/api/projects/{id}/prs/{number}/close", post(pr_close))
        .route(
            "/api/projects/{id}/discussions",
            get(discussion_list).post(discussion_create),
        )
        .route(
            "/api/projects/{id}/discussions/{discussion_id}",
            get(discussion_detail),
        )
        .route(
            "/api/projects/{id}/discussions/{discussion_id}/comments",
            post(discussion_add_comment),
        )
        .route(
            "/api/projects/{id}/discussions/{discussion_id}/close",
            post(discussion_close),
        )
        .route(
            "/api/git-tokens",
            get(git_token_list).post(git_token_create),
        )
        .route("/api/git-tokens/{id}", delete(git_token_revoke))
        .route(
            "/api/ssh-keys",
            get(ssh_key_list).post(ssh_key_create),
        )
        .route("/api/ssh-keys/{id}", delete(ssh_key_revoke))
        .route("/api/runners", get(runner_list).post(runner_create))
        .route("/api/runners/{id}", patch(runner_update))
        // /git/ 下有两类通道：<slug>.git/... 是 Git smart HTTP（匿名 clone，
        // 写入需 Git 令牌或登录会话）；<slug>/... 是站内文件树接口（公开只读，
        // 登录用户可写）。推送的 pack 可能远大于普通 API 请求，放宽上限。
        .route(
            "/git/{*path}",
            get(git_http_get)
                .post(git_http_post)
                .put(git_http_put)
                .delete(git_http_delete)
                .layer(DefaultBodyLimit::max(GIT_MAX_BODY_BYTES)),
        )
        .route("/api/invites", get(invites).post(create_invite))
        .route("/api/invites/{code}", delete(revoke_invite))
        .route("/api/users", get(users))
        .route("/api/users/{id}", patch(patch_user))
        // 整站重建：POST 触发、GET 查询状态，共用一个路径。挂管理员 +
        // step-up 双闸，构建会覆盖线上产物目录，等于发布权限。
        .route("/api/admin/rebuild", post(rebuild_site).get(build_status))
        .with_state(state)
        // 请求体上限写成显式常量，不依赖 axum 的默认 2 MiB：正文里塞一个
        // 几十 MB 的字符串除了把内存和 SQL 连接拖死之外没有任何作用，
        // 而 `Bytes` 提取器会整块缓冲，限制必须在读之前就在扩展里声明。
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(session_layer)
        .layer(cors)
        .layer(middleware::from_fn_with_state(
            origin_policy,
            guard_origin,
        ))
        .layer(TraceLayer::new_for_http());

    // 工作流 runner API 与管理端中间件分开挂：runner 用 token 认证、不带浏览器
    // Origin，且日志分片可能超过管理端的 2 MiB 请求体上限。
    let wf_state =
        crate::workflow::build_state(wf_pool, wf_build_ctl, build_queue.clone());
    crate::workflow::spawn_pruner(wf_state.clone());
    crate::workflow::dispatch::spawn_dispatcher(wf_state.clone(), wf_state.build_queue.clone());
    let app = app.nest("/api/runner", crate::workflow::router(wf_state));

    // MCP Streamable HTTP：鉴权由 MCP 自己按 OAuth access token 校验。
    let mcp_state = crate::mcp::KychMcp::new(
        state_pool_for_mcp.clone(),
        cfg.content_root.clone(),
        cfg.site_url.clone(),
        cfg.build_out_dir.clone(),
        std::env::current_exe()?,
    );
    let app = app.merge(crate::mcp::router(
        mcp_state,
        state_pool_for_mcp.clone(),
        cfg.trust_proxy_headers,
    ));

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("API 监听 {}", cfg.bind);
    // 限流要按客户端 IP 计数，所以要把对端地址注入请求扩展。
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}

/// 只在流程被正常走完时才删除——放弃登录的人和试探登录的脚本都能让表单调增长。
/// 定期清理过期的认证流程。
///
/// `login_start` 是匿名可调的，每调一次就往 `auth_flows` 插一行，而过去的代码
/// 只在流程被正常走完时才删除——放弃登录的人和试探登录的脚本都能让表单调增长。
fn spawn_flow_pruner(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match service::prune_auth_flows(&pool).await {
                Ok(0) => {}
                Ok(removed) => tracing::info!("清理过期认证流程 {removed} 条"),
                Err(e) => tracing::warn!("清理过期认证流程失败: {e:#}"),
            }
        }
    });
}

/// 定期把 PG 对象数超门槛的项目 offload 到磁盘。每轮先睡满间隔再扫，
/// 启动阶段不抢资源。
fn spawn_auto_offloader(
    pool: PgPool,
    disk: &'static crate::git::disk::DiskStore,
    min_objects: usize,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(60)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // interval 第一次 tick 立即触发，这里跳过，让服务先稳定启动。
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match crate::git::disk::auto_offload_sweep(&pool, disk, min_objects).await {
                Ok(swept) if !swept.is_empty() => {
                    tracing::info!("本轮自动 offload 处理了 {} 个大仓库", swept.len());
                }
                Ok(_) => {}
                Err(error) => tracing::warn!("自动 offload 扫描失败: {error:#}"),
            }
        }
    });
}

mod tests {
    use super::*;

    #[test]
    fn username_accepts_unicode_and_emoji() {
        assert!(validate_username("张三丰").is_ok());
        assert!(validate_username("kych_net-01").is_ok());
        assert!(validate_username("🎉🎉🎉").is_ok());
        assert!(validate_username("あい_う").is_ok());
    }

    #[test]
    fn username_rejects_special_and_whitespace() {
        assert!(validate_username("ab").is_err());
        assert!(validate_username("a b c").is_err());
        assert!(validate_username("a/b/c").is_err());
        assert!(validate_username("a.b.c").is_err());
        assert!(validate_username("a;b;c").is_err());
        assert!(validate_username("a<b>c").is_err());
    }

    #[test]
    fn step_up_requires_fresh_authentication() {
        let now = 1_700_000_000;
        // 没有 auth_at（旧会话、或改动之前建立的会话）一律要求重新登录。
        assert!(!step_up_fresh(None, now));
        assert!(step_up_fresh(Some(now), now));
        assert!(step_up_fresh(Some(now - STEP_UP_WINDOW_SECS + 1), now));
        assert!(!step_up_fresh(Some(now - STEP_UP_WINDOW_SECS), now));
        assert!(!step_up_fresh(Some(now - 3600), now));
        // 客户端与服务器时钟差几秒不应该把有效会话判成过期。
        assert!(step_up_fresh(Some(now + 60), now));
        assert!(!step_up_fresh(Some(now + 3600), now));
    }

    #[test]
    fn body_limits_are_below_database_column_checks() {
        // 头像：传输层上限必须不小于 svg.rs 的内容上限，否则合法头像会被 413 拦下。
        assert!(AVATAR_MAX_BODY_BYTES >= crate::svg::MAX_BYTES);
        // 全局上限必须大于头像上限，否则上一条的「让 svg.rs 报错」意图就反了。
        assert!(MAX_BODY_BYTES > AVATAR_MAX_BODY_BYTES);
    }
}
