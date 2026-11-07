use anyhow::{anyhow, Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
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

use crate::auth::{self, Auth};
use crate::config::Config;
use crate::models::{SiteSettings, User};
use crate::rate::RateLimiter;
use crate::service;

/// 认证相关接口的限流额度，格式为 `(窗口内最多几次, 窗口秒数)`。
///
/// 这里挡住的是「在线暴力尝试」，不是「离线爆破数据库」——TOTP 密钥明文入库，
/// 数据库一旦泄露就只剩下密钥派生强度，限流帮不上忙。
const LOGIN_START_PER_USER: (u32, u64) = (10, 60);
const LOGIN_START_PER_IP: (u32, u64) = (30, 60);
const TOTP_LOGIN_PER_IP: (u32, u64) = (10, 60);
/// 验证码 6 位、单步 30 秒（`check_current` 允许 ±1 步），
/// 因此每账号 15 分钟内最多 5 次，猜中的概率被压到可以忽略。
const TOTP_ATTEMPTS_PER_USER: (u32, u64) = (5, 900);
/// 绑定流程里确认密钥的尝试次数；猜中一次就等于换绑成功，所以比登录更紧。
const TOTP_CONFIRM_PER_USER: (u32, u64) = (5, 900);
const REGISTER_PER_IP: (u32, u64) = (10, 60);

/// 全局请求体上限。Typst / Markdown 正文写满 2 MiB 已经是极限量级。
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// 头像上限：比 svg.rs 自己的 256 KB 稍大，让「超长」由 svg.rs 报出中文提示，
/// 而不是被传输层截成一句 413。
const AVATAR_MAX_BODY_BYTES: usize = 256 * 1024 + 1024;

/// 会话最长存活时间。用 OnInactivity 表达，但只有会话被写入时才会顺延，
/// 实际效果就是「登录后最多 8 小时」，配合下面各处 `cycle_id` 使用。
const SESSION_TTL: time::Duration = time::Duration::hours(8);

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    auth: Arc<Auth>,
    site_url: String,
    limits: RateLimiter,
    trust_proxy_headers: bool,
    /// 管理端触发整站重建的协调器。构建独立成子进程跑，serve 自身
    /// 不引入 Typst 渲染逻辑（见 `BuildCtl` 注释）。
    build_ctl: Arc<BuildCtl>,
}

/// 一次构建的执行结果快照。
#[derive(Clone, Serialize)]
struct BuildRecord {
    started_at: i64,
    finished_at: Option<i64>,
    /// None = 仍在运行；Some = 退出码。
    exit_code: Option<i32>,
    /// 输出尾部（stdout + stderr 合并截断），失败时用来定位原因。
    tail: String,
}

#[derive(Serialize)]
struct BuildStatus {
    running: bool,
    last: Option<BuildRecord>,
}

/// 管理端触发的整站重建。
///
/// serve 进程不直接调 `build::run`，而是用「当前二进制 + build 子命令」拉起
/// 独立进程：Typst 及其内嵌字体是重依赖，进程隔离让渲染崩溃、内存尖峰只损失
/// 一次构建，不会波及常驻的 API 服务。可执行文件取 `current_exe()`，
/// 与 serve 是同一个二进制，不存在路径配置问题。
struct BuildCtl {
    /// 同一时刻只允许一个构建。用原子量而不是锁，避免排队语义——
    /// 并发触发直接 409，让前端提示「已在构建」。
    running: AtomicBool,
    last: tokio::sync::Mutex<Option<BuildRecord>>,
    out_dir: PathBuf,
    exe: PathBuf,
}

/// 输出尾部保留长度，超过即从头部截断。
const BUILD_TAIL_BYTES: usize = 4096;

impl BuildCtl {
    fn new(out_dir: PathBuf) -> Self {
        Self {
            running: AtomicBool::new(false),
            last: tokio::sync::Mutex::new(None),
            out_dir,
            // 兜底名与二进制名一致：拿不到自身路径时，重建子进程仍能起得来。
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kych-net")),
        }
    }
}

/// 触发一次整站重建。立即返回 200 并异步执行，前端轮询 GET 拿进度与结果。
async fn rebuild_site(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<BuildStatus>> {
    current_admin(&state, &session).await?;
    require_step_up(&session).await?;
    let ctl = &state.build_ctl;
    if ctl.running.swap(true, Ordering::SeqCst) {
        return Err(ApiError::with_status(
            StatusCode::CONFLICT,
            "已有一次构建在进行，请等待其结束",
        ));
    }
    let record = BuildRecord {
        started_at: Utc::now().timestamp(),
        finished_at: None,
        exit_code: None,
        tail: String::new(),
    };
    *ctl.last.lock().await = Some(record);

    let ctl = Arc::clone(&state.build_ctl);
    tokio::spawn(async move {
        let output = tokio::process::Command::new(ctl.exe.clone())
            .arg("build")
            .arg("--out")
            .arg(&ctl.out_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        let (exit_code, tail) = match output {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                let cut = text.len().saturating_sub(BUILD_TAIL_BYTES);
                let tail = if cut > 0 {
                    // 按字符边界截，避免中文被腰斩成乱码。
                    let start = (cut..=text.len())
                        .find(|&i| text.is_char_boundary(i))
                        .unwrap_or(text.len());
                    format!("…{}", &text[start..])
                } else {
                    text
                };
                (Some(out.status.code().unwrap_or(-1)), tail)
            }
            Err(e) => (None, format!("构建进程启动失败: {e}")),
        };
        let mut last = ctl.last.lock().await;
        if let Some(rec) = last.as_mut() {
            rec.finished_at = Some(Utc::now().timestamp());
            rec.exit_code = exit_code;
            rec.tail = tail;
        }
        drop(last);
        ctl.running.store(false, Ordering::SeqCst);
    });

    let last = state.build_ctl.last.lock().await.clone();
    Ok(Json(BuildStatus {
        running: true,
        last,
    }))
}

/// 查询最近一次构建的状态。
async fn build_status(State(state): State<AppState>) -> ApiResult<Json<BuildStatus>> {
    Ok(Json(BuildStatus {
        running: state.build_ctl.running.load(Ordering::SeqCst),
        last: state.build_ctl.last.lock().await.clone(),
    }))
}

struct ApiError {
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

#[derive(Clone)]
struct OriginPolicy {
    admin_origin: String,
}

/// 拒绝来自管理端以外 Origin 的状态变更请求。
///
/// 内容站 `www` 与 API 属于同一 eTLD+1（同站），`SameSite` 不会阻止同站跨源请求，
/// CORS 也只阻止读取响应、不阻止带凭据的写请求被发出。因此内容站一旦出现 XSS，
/// 就能用 `fetch(credentials:'include')` 冒用管理员会话调用写接口。
/// 这里显式校验 Origin，是「子域隔离」真正生效所必需的一环。
/// 没有 Origin 头的请求（curl、服务端调用）保持放行，不影响 CLI 与非浏览器客户端。
async fn guard_origin(
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

    if origin.trim_end_matches('/') == policy.admin_origin.trim_end_matches('/') {
        return next.run(req).await;
    }

    tracing::warn!("拒绝来自非管理端 Origin 的写请求: {origin}");
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "请求来源不被允许" })),
    )
        .into_response()
}

async fn current_user(state: &AppState, session: &Session) -> ApiResult<User> {
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

async fn current_admin(state: &AppState, session: &Session) -> ApiResult<User> {
    let user = current_user(state, session).await?;
    if !user.is_admin {
        api_reject!(StatusCode::FORBIDDEN, "需要管理员权限");
    }
    Ok(user)
}

/// 用户名中禁止出现的特殊字符。空白与控制字符另行判断。
const USERNAME_SPECIAL_CHARS: &[char] = &[
    '/', '\\', '.', ',', ':', ';', '"', '\'', '<', '>', '&', '%', '#', '?', '=', '+', '*', '(',
    ')', '[', ']', '{', '}', '|', '@', '!', '^', '~', '$', '`',
];

fn validate_username(username: &str) -> ApiResult<()> {
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
fn safe_url(value: &str) -> bool {
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
fn limited(state: &AppState, key: &str, quota: (u32, u64)) -> ApiResult<()> {
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
fn client_ip(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> String {
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
fn login_failed(reason: impl std::fmt::Display) -> ApiError {
    tracing::debug!("登录失败: {reason}");
    ApiError::with_status(StatusCode::UNAUTHORIZED, "用户名或凭证无效")
}

/// 会话里记录「本次认证成功的时间」，改二次验证时要求它足够新鲜。
const STEP_UP_WINDOW_SECS: i64 = 300;

/// 登录/注册成功后轮换会话 ID。
///
/// 不换 ID 的话，攻击者可以事先拿到一个会话 ID 让受害者用上（会话固定），
/// 受害者完成认证后攻击者手里那个 ID 就带着已登录的身份。
///
/// `auth_at` 同时是「最近一次真实认证」的时间戳，用来做敏感操作的 step-up
/// 判断。它写在会话里而不是数据库里，所以换 store、重启进程都会自然失效。
async fn establish_session(session: &Session, user_id: Uuid) -> ApiResult<()> {
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
fn step_up_fresh(auth_at: Option<i64>, now: i64) -> bool {
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
async fn require_step_up(session: &Session) -> ApiResult<()> {
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

#[derive(Deserialize)]
struct RegisterRequest {
    code: String,
    username: String,
}

#[derive(Serialize)]
struct RegisterResponse {
    user: User,
    flow_id: Uuid,
    options: CreationChallengeResponse,
}

async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Json<RegisterResponse>> {
    // 注册要求邀请码，但邀请码是 16 字节随机值、不需要被爆破；这里限的是
    // 「拿到一个码之后批量建号占用户名」以及凭空造 flow 行。
    let ip = client_ip(&state, &headers, peer);
    limited(&state, &format!("register:ip:{ip}"), REGISTER_PER_IP)?;
    validate_username(&req.username)?;
    if service::find_user_by_username(&state.pool, &req.username)
        .await?
        .is_some()
    {
        api_bail!("用户名已被占用");
    }

    // 邀请码在这里只做只读校验，不扣次数：真正的占用发生在 finish 里、
    // 凭证校验通过之后的那个事务内。用户中途取消 WebAuthn 仪式时，
    // 只留下一条 5 分钟后被清理掉的 flow 行。
    service::peek_invite(&state.pool, &req.code).await?;

    // user_id 提前生成：它同时是 WebAuthn 的 user handle，必须与最终落库的
    // 用户 ID 完全一致，否则后续 Passkey 登录会找不到对应用户。
    let user_id = Uuid::new_v4();
    let pending_user = User {
        id: user_id,
        username: req.username.clone(),
        disabled: false,
        is_admin: false,
        created_at: Utc::now(),
    };
    let (options, flow_state) = state.auth.create_registration_challenge(&pending_user)?;
    let flow_id = state
        .auth
        .start_pending_registration(&state.pool, &pending_user, &req.code, flow_state)
        .await?;

    // 此时用户还不存在，返回的 user 只是给前端展示将要注册的账号信息。
    Ok(Json(RegisterResponse {
        user: pending_user,
        flow_id,
        options,
    }))
}

#[derive(Deserialize)]
struct FinishRegistrationRequest {
    flow_id: Uuid,
    credential: RegisterPublicKeyCredential,
}

async fn finish_registration(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<FinishRegistrationRequest>,
) -> ApiResult<Json<User>> {
    // 凭证有效才占用邀请码并建用户；无效或已被用过时数据库不会有任何变化。
    let user = state
        .auth
        .finish_pending_registration(&state.pool, req.flow_id, &req.credential)
        .await?;
    establish_session(&session, user.id).await?;
    Ok(Json(user))
}

#[derive(Deserialize)]
struct LoginStartRequest {
    username: String,
}

#[derive(Serialize)]
struct LoginStartResponse {
    flow_id: Uuid,
    options: webauthn_rs::prelude::RequestChallengeResponse,
}

async fn login_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginStartRequest>,
) -> ApiResult<Json<LoginStartResponse>> {
    // 每次发起都会往 auth_flows 插一行，所以必须先限流再查库。
    let ip = client_ip(&state, &headers, peer);
    limited(&state, &format!("login-start:ip:{ip}"), LOGIN_START_PER_IP)?;
    limited(
        &state,
        &format!("login-start:user:{}", req.username.to_ascii_lowercase()),
        LOGIN_START_PER_USER,
    )?;

    let Some(user) = service::find_user_by_username(&state.pool, &req.username).await? else {
        return Err(login_failed("用户名不存在"));
    };
    if user.disabled {
        return Err(login_failed("账号已被禁用"));
    }
    let (options, flow_id) = state
        .auth
        .start_authentication(&state.pool, &user)
        .await
        .map_err(|e| login_failed(format!("无法发起 Passkey 登录: {e:#}")))?;
    Ok(Json(LoginStartResponse { flow_id, options }))
}

#[derive(Deserialize)]
struct LoginFinishRequest {
    flow_id: Uuid,
    credential: PublicKeyCredential,
}

async fn login_finish(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<LoginFinishRequest>,
) -> ApiResult<Json<User>> {
    let user_id = state
        .auth
        .finish_authentication(&state.pool, req.flow_id, &req.credential)
        .await
        .map_err(|e| login_failed(format!("Passkey 登录校验失败: {e:#}")))?;
    let user = service::find_user_by_id(&state.pool, user_id)
        .await?
        .context("用户不存在")?;
    if user.disabled {
        return Err(login_failed("账号已被禁用"));
    }
    establish_session(&session, user_id).await?;
    Ok(Json(user))
}

#[derive(Deserialize)]
struct TotpLoginRequest {
    username: String,
    code: String,
}

async fn totp_login(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<TotpLoginRequest>,
) -> ApiResult<Json<User>> {
    // 验证码只有 6 位，不限流就是给在线爆破开门。
    // 按用户名的额度就是「每账号 15 分钟 5 次」，成功的登录会把计数清掉。
    //
    // 权衡要说清楚：固定窗口意味着有人故意拿错的验证码刷某个用户名，就能让这个人
    // 15 分钟内用不了 TOTP（Passkey 登录不受影响，计数也会自己滚掉，不会永久锁死）。
    // 只靠 TOTP 登录的账号如果在意这点，把额度放宽到 10 次/15 分钟，
    // 或者按「同一 IP 至少成功过一次」再放宽，都是可以的；这里先取保守值。
    let ip = client_ip(&state, &headers, peer);
    let user_key = format!("totp:user:{}", req.username.to_ascii_lowercase());
    limited(&state, &format!("totp:ip:{ip}"), TOTP_LOGIN_PER_IP)?;
    limited(&state, &user_key, TOTP_ATTEMPTS_PER_USER)?;

    let Some(user) = service::find_user_by_username(&state.pool, &req.username).await? else {
        return Err(login_failed("TOTP 登录：用户名不存在"));
    };
    if user.disabled {
        return Err(login_failed("TOTP 登录：账号已被禁用"));
    }
    if !auth::totp_verify(&state.pool, &user, &req.code).await? {
        return Err(login_failed("TOTP 登录：验证码错误"));
    }
    state.limits.reset(&user_key);
    establish_session(&session, user.id).await?;
    Ok(Json(user))
}

async fn me(State(state): State<AppState>, session: Session) -> ApiResult<Json<Option<User>>> {
    let id: Option<Uuid> = session
        .get("user_id")
        .await
        .map_err(|e| anyhow!("读取会话失败: {e}"))?;
    match id {
        Some(id) => Ok(Json(
            service::find_user_by_id(&state.pool, id)
                .await?
                .filter(|u| !u.disabled),
        )),
        None => Ok(Json(None)),
    }
}

async fn logout(session: Session) -> ApiResult<StatusCode> {
    session
        .flush()
        .await
        .map_err(|e| anyhow!("清除会话失败: {e}"))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn totp_setup(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_step_up(&session).await?;
    let (url, secret) = auth::totp_setup(&state.pool, user.id, &user.username).await?;
    Ok(Json(json!({ "otpauth_url": url, "secret": secret })))
}

async fn passkey_setup(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_step_up(&session).await?;
    let (options, flow_id) = state.auth.start_registration(&state.pool, &user).await?;
    Ok(Json(json!({ "flow_id": flow_id, "options": options })))
}

#[derive(Deserialize)]
struct PasskeySetupFinishRequest {
    flow_id: Uuid,
    credential: RegisterPublicKeyCredential,
}

async fn passkey_setup_finish(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<PasskeySetupFinishRequest>,
) -> ApiResult<StatusCode> {
    let user = current_user(&state, &session).await?;
    require_step_up(&session).await?;
    let user_id = state
        .auth
        .finish_registration(&state.pool, req.flow_id, &req.credential)
        .await?;
    if user_id != user.id {
        api_bail!("认证流程不属于当前用户");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TotpConfirmRequest {
    code: String,
}

async fn totp_confirm(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<TotpConfirmRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    // 这里不再要求 step-up：待确认的密钥只能由已经通过 step-up 的 totp_setup
    // 产生，攻击者拿不到旧会话就无法生成它。但确认本身是一次 6 位验证码的
    // 在线判断，仍然限流，避免有人对着 pending 密钥硬猜。
    limited(
        &state,
        &format!("totp-confirm:user:{}", user.id),
        TOTP_CONFIRM_PER_USER,
    )?;
    let ok = auth::totp_confirm(&state.pool, user.id, &user.username, &req.code).await?;
    if !ok {
        api_bail!("验证码错误");
    }
    state.limits.reset(&format!("totp-confirm:user:{}", user.id));
    Ok(Json(json!({ "ok": true })))
}

async fn contents(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let items = service::list_contents(&state.pool).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct ContentRequest {
    /// 编辑已有内容时带 id，改名才会更新原记录而不是新建一条。
    #[serde(default)]
    id: Option<Uuid>,
    slug: String,
    title: String,
    format: String,
    body: String,
    #[serde(default)]
    published: bool,
}

async fn save_content(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<ContentRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let Some(format) = crate::models::ContentFormat::parse(&req.format) else {
        api_bail!("format 只能是 markdown、html 或 typst");
    };
    // 路径在这里归一化成输出路径：
    //   /xef/ -> xef/index.html
    //   /eoe/index.md -> eoe/index.html
    // 同一路径只有一条记录，因此 .html/.md/.typ 天然互斥。
    let (slug, format) = service::normalize_content_path(&req.slug, format)?;
    if req.title.trim().is_empty() || req.title.chars().count() > 200 {
        api_bail!("标题需为 1-200 个字符");
    }
    let content = if let Some(id) = req.id {
        let existing = service::find_content_by_id(&state.pool, id)
            .await?
            .context("内容不存在")?;
        if existing.slug == service::HOME_SLUG && slug != service::HOME_SLUG {
            api_reject!(StatusCode::FORBIDDEN, "网站主页路径不能修改");
        }
        service::update_content(
            &state.pool,
            id,
            &slug,
            &req.title,
            format.as_str(),
            &req.body,
            req.published,
        )
        .await?
    } else {
        service::upsert_content(
            &state.pool,
            &slug,
            &req.title,
            format.as_str(),
            &req.body,
            req.published,
        )
        .await?
    };
    Ok(Json(json!({ "item": content })))
}

async fn delete_content(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    current_user(&state, &session).await?;
    let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM contents WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    if slug.as_deref() == Some(service::HOME_SLUG) {
        api_reject!(StatusCode::FORBIDDEN, "网站主页不能删除，只能编辑");
    }
    sqlx::query("DELETE FROM contents WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn invites(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let items = service::list_invites(&state.pool).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct CreateInviteRequest {
    #[serde(default = "default_uses")]
    max_uses: i32,
    #[serde(default = "default_expiry")]
    expires_in_days: i64,
}

fn default_uses() -> i32 {
    1
}

fn default_expiry() -> i64 {
    7
}

async fn create_invite(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<CreateInviteRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_admin(&state, &session).await?;
    if req.max_uses < 1 || req.max_uses > 100 {
        api_bail!("使用次数需在 1-100 之间");
    }
    // 只卡上界：不卡的话 `Utc::now() + Duration::days(i64)` 会因超出 DateTime 范围直接 panic。
    // 下界保留原来的语义，0 或负数表示不过期。
    if req.expires_in_days > 3650 {
        api_bail!("有效期最多 3650 天（0 或负数表示不过期）");
    }
    let expires_at = if req.expires_in_days <= 0 {
        None
    } else {
        Some(Utc::now() + chrono::Duration::days(req.expires_in_days))
    };
    let invite =
        service::create_invite(&state.pool, Some(user.id), req.max_uses, expires_at).await?;
    Ok(Json(json!({ "item": invite })))
}

async fn revoke_invite(
    State(state): State<AppState>,
    session: Session,
    Path(code): Path<String>,
) -> ApiResult<StatusCode> {
    current_admin(&state, &session).await?;
    service::revoke_invite(&state.pool, &code).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn users(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let items = service::list_users(&state.pool).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct PatchUserRequest {
    disabled: bool,
}

async fn patch_user(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchUserRequest>,
) -> ApiResult<StatusCode> {
    let admin = current_admin(&state, &session).await?;
    service::set_user_disabled_guarded(&state.pool, admin.id, id, req.disabled).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let site = service::get_settings(&state.pool).await?;
    Ok(Json(json!({ "site": site, "site_url": state.site_url })))
}

async fn put_settings(
    State(state): State<AppState>,
    session: Session,
    Json(site): Json<SiteSettings>,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    if site.site_name.trim().is_empty() || site.site_name.chars().count() > 64 {
        api_bail!("站点名称需为 1-64 个字符");
    }
    if site.nav.len() > 12 {
        api_bail!("导航链接最多 12 条");
    }
    if site
        .nav
        .iter()
        .any(|link| link.label.trim().is_empty() || link.href.trim().is_empty())
    {
        api_bail!("导航链接的标签与地址不能为空");
    }
    if !safe_url(&site.logo_url) || site.nav.iter().any(|link| !safe_url(&link.href)) {
        api_bail!("站点图标与导航链接不能是脚本或数据协议");
    }
    service::save_settings(&state.pool, &site).await?;
    Ok(Json(json!({ "site": site })))
}

async fn upload_avatar(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<User>> {
    let user = current_user(&state, &session).await?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.starts_with("image/svg+xml") {
        api_bail!("头像必须使用 image/svg+xml");
    }
    crate::svg::validate_svg(&body)?;
    let svg = std::str::from_utf8(&body).context("SVG 必须是 UTF-8 文本")?;
    let user = service::update_avatar(&state.pool, user.id, svg).await?;
    Ok(Json(user))
}

async fn avatar(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Response> {
    match service::get_avatar(&state.pool, id).await? {
        Some(svg) => {
            let mut response = svg.into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/svg+xml; charset=utf-8"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300"),
            );
            headers.insert(
                axum::http::HeaderName::from_static("content-security-policy"),
                HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"),
            );
            // 防止浏览器把 SVG 嗅探成 HTML 后执行其中的内容。
            headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            Ok(response)
        }
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

pub async fn serve(cfg: Config) -> Result<()> {
    let pool = crate::db::connect(&cfg.database_url).await?;
    crate::db::migrate(&pool).await?;
    spawn_flow_pruner(pool.clone());

    let auth = Arc::new(Auth::new(&cfg.rp_id, &cfg.rp_origin)?);
    let state = AppState {
        pool,
        auth,
        site_url: cfg.site_url.clone(),
        limits: RateLimiter::default(),
        trust_proxy_headers: cfg.trust_proxy_headers,
        build_ctl: Arc::new(BuildCtl::new(cfg.build_out_dir.clone())),
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
    // SameSite 选 Strict 不影响管理端调 API：`m.kych.net` 与 `api.kych.net`
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
    };

    let app = Router::new()
        .route("/api/health", get(|| async { Json(json!({"ok": true})) }))
        .route("/api/auth/register", post(register))
        .route("/api/auth/register/finish", post(finish_registration))
        .route("/api/auth/login/start", post(login_start))
        .route("/api/auth/login/finish", post(login_finish))
        .route("/api/auth/totp/login", post(totp_login))
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
        .route("/api/contents/{id}", delete(delete_content))
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

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("API 监听 {}", cfg.bind);
    // 限流要按客户端 IP 计数，所以要把对端地址注入请求扩展。
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}

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

#[cfg(test)]
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
