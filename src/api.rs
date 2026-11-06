use anyhow::{anyhow, Context, Result};
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::{MemoryStore, Session, SessionManagerLayer};
use uuid::Uuid;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
};

use crate::auth::{self, Auth};
use crate::config::Config;
use crate::models::{SiteSettings, User};
use crate::service;

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    auth: Arc<Auth>,
    site_url: String,
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

fn validate_username(username: &str) -> ApiResult<()> {
    let valid = (3..=32).contains(&username.len())
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        api_bail!("用户名需为 3-32 位字母、数字、下划线或连字符");
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

#[derive(Deserialize)]
struct RegisterRequest {
    code: String,
    username: String,
    display_name: String,
}

#[derive(Serialize)]
struct RegisterResponse {
    user: User,
    flow_id: Uuid,
    options: CreationChallengeResponse,
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Json<RegisterResponse>> {
    validate_username(&req.username)?;
    if req.display_name.trim().is_empty() || req.display_name.chars().count() > 64 {
        api_bail!("显示名称需为 1-64 个字符");
    }
    if service::find_user_by_username(&state.pool, &req.username)
        .await?
        .is_some()
    {
        api_bail!("用户名已被占用");
    }

    // user_id 提前生成：它同时是 WebAuthn 的 user handle，必须与最终落库的
    // 用户 ID 完全一致，否则后续 Passkey 登录会找不到对应用户。
    let user_id = Uuid::new_v4();
    let pending_user = User {
        id: user_id,
        username: req.username.clone(),
        display_name: req.display_name.clone(),
        disabled: false,
        is_admin: false,
        created_at: Utc::now(),
    };
    let (options, flow_state) = state.auth.create_registration_challenge(&pending_user)?;
    let flow_id = Uuid::new_v4();

    // 占用邀请码、创建用户、保存认证流程在同一个事务里完成，任一步失败整体回滚。
    let user = service::register_user(
        &state.pool,
        user_id,
        &req.code,
        &req.username,
        &req.display_name,
        flow_id,
        flow_state,
    )
    .await?;

    Ok(Json(RegisterResponse {
        user,
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
    let user_id = state
        .auth
        .finish_registration(&state.pool, req.flow_id, &req.credential)
        .await?;
    let user = service::find_user_by_id(&state.pool, user_id)
        .await?
        .context("用户不存在")?;
    session
        .insert("user_id", user_id)
        .await
        .map_err(|e| anyhow!("写入会话失败: {e}"))?;
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
    Json(req): Json<LoginStartRequest>,
) -> ApiResult<Json<LoginStartResponse>> {
    let user = service::find_user_by_username(&state.pool, &req.username)
        .await?
        .context("用户名或凭证无效")?;
    if user.disabled {
        api_bail!("账号已被禁用");
    }
    let (options, flow_id) = state.auth.start_authentication(&state.pool, &user).await?;
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
        .await?;
    let user = service::find_user_by_id(&state.pool, user_id)
        .await?
        .context("用户不存在")?;
    if user.disabled {
        api_bail!("账号已被禁用");
    }
    session
        .insert("user_id", user_id)
        .await
        .map_err(|e| anyhow!("写入会话失败: {e}"))?;
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
    Json(req): Json<TotpLoginRequest>,
) -> ApiResult<Json<User>> {
    let user = service::find_user_by_username(&state.pool, &req.username)
        .await?
        .context("用户名或验证码错误")?;
    if user.disabled {
        api_bail!("账号已被禁用");
    }
    if !auth::totp_verify(&state.pool, &user, &req.code).await? {
        api_bail!("用户名或验证码错误");
    }
    session
        .insert("user_id", user.id)
        .await
        .map_err(|e| anyhow!("写入会话失败: {e}"))?;
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
    let (url, secret) = auth::totp_setup(&state.pool, user.id, &user.username).await?;
    Ok(Json(json!({ "otpauth_url": url, "secret": secret })))
}

async fn passkey_setup(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
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
    let ok = auth::totp_confirm(&state.pool, user.id, &user.username, &req.code).await?;
    if !ok {
        api_bail!("验证码错误");
    }
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
    if crate::models::ContentFormat::parse(&req.format).is_none() {
        api_bail!("format 只能是 markdown、html 或 typst");
    }
    // slug 直接拼进 dist/posts/<slug>.html，必须在写入时就校验。
    // 过去只在 build 阶段 bail，一条 `a/b` 就会让整站永久构建失败。
    if !service::valid_slug(&req.slug) {
        api_bail!("slug 只能包含 Unicode 字母/数字、- 和 _,不能含空格、标点、路径分隔符等特殊字符,且不超过 180 字节");
    }
    if req.title.trim().is_empty() || req.title.chars().count() > 200 {
        api_bail!("标题需为 1-200 个字符");
    }
    let content = service::upsert_content(
        &state.pool,
        &req.slug,
        &req.title,
        &req.format,
        &req.body,
        req.published,
    )
    .await?;
    Ok(Json(json!({ "item": content })))
}

async fn delete_content(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    current_user(&state, &session).await?;
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
    current_admin(&state, &session).await?;
    service::set_user_disabled(&state.pool, id, req.disabled).await?;
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

#[derive(Deserialize)]
struct PatchMeRequest {
    display_name: String,
}

async fn patch_me(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<PatchMeRequest>,
) -> ApiResult<Json<User>> {
    let user = current_user(&state, &session).await?;
    let display_name = req.display_name.trim();
    if display_name.is_empty() || display_name.chars().count() > 64 {
        api_bail!("显示名称需为 1-64 个字符");
    }
    let user = service::update_display_name(&state.pool, user.id, display_name).await?;
    Ok(Json(user))
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

    let auth = Arc::new(Auth::new(&cfg.rp_id, &cfg.rp_origin)?);
    let state = AppState {
        pool,
        auth,
        site_url: cfg.site_url.clone(),
    };

    let admin_origin: HeaderValue = cfg
        .admin_origin
        .parse()
        .context("ADMIN_ORIGIN 不是合法 origin")?;
    let cors = CorsLayer::new()
        .allow_origin(admin_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::COOKIE, header::ACCEPT])
        .allow_credentials(true);

    let session_layer = SessionManagerLayer::new(MemoryStore::default())
        .with_secure(cfg.rp_origin.starts_with("https"));

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
        .route("/api/me", get(me).patch(patch_me))
        .route("/api/me/avatar", post(upload_avatar))
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
        .with_state(state)
        .layer(session_layer)
        .layer(cors)
        .layer(middleware::from_fn_with_state(
            origin_policy,
            guard_origin,
        ))
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("API 监听 {}", cfg.bind);
    axum::serve(listener, app).await?;
    Ok(())
}
