//! 标准 MCP OAuth 2.1 授权服务器。
//!
//! MCP 客户端先访问 `/mcp` 得到 401 和资源元数据，再通过动态注册拿 client_id，
//! 走授权码 + PKCE，最后用 access token 调 `/mcp`。不再接受任何静态 token。

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use base64::Engine;
use chrono::{Duration, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower_sessions::Session;
use url::Url;
use std::net::SocketAddr;
use uuid::Uuid;

use crate::api::AppState;
use crate::auth;
use crate::service;

pub const MCP_RESOURCE: &str = "https://kych.net/mcp";
const OAUTH_ORIGIN: &str = "https://kych.net";
const SCOPES: &str = "content:read content:write project:read project:write";
const ACCESS_TTL_SECS: i64 = 3600;
const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;
const OAUTH_LOGIN_PER_IP: (u32, u64) = (10, 60);
const OAUTH_LOGIN_PER_USER: (u32, u64) = (5, 900);
const OAUTH_REGISTER_PER_IP: (u32, u64) = (10, 60);

fn random_token(bytes: usize) -> String {
    let mut raw = vec![0u8; bytes];
    OsRng.fill_bytes(&mut raw);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn oauth_ip(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> String {
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

fn too_many(retry_after: std::time::Duration) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(
            header::RETRY_AFTER,
            retry_after.as_secs().max(1).to_string(),
        )],
        "请求过于频繁，请稍后再试",
    )
        .into_response()
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>body{{font-family:system-ui,-apple-system,sans-serif;max-width:42rem;margin:3rem auto;padding:0 1.2rem;line-height:1.6;color:#1f2937}}h1{{font-size:1.4rem}}label{{display:block;margin:1rem 0 .3rem}}input,button{{font:inherit;padding:.6rem .8rem;width:100%;box-sizing:border-box}}button{{margin-top:1.2rem;background:#111827;color:#fff;border:0;border-radius:.4rem;cursor:pointer}}code{{background:#f3f4f6;padding:.1rem .3rem;border-radius:.25rem}}.error{{color:#b91c1c}}.muted{{color:#6b7280;font-size:.9rem}}</style></head><body>{body}</body></html>"#,
        title = html_escape(title),
        body = body
    ))
}

fn hidden_fields(f: &OAuthFields) -> String {
    format!(
        r#"<input type="hidden" name="client_id" value="{client_id}"><input type="hidden" name="redirect_uri" value="{redirect_uri}"><input type="hidden" name="state" value="{state}"><input type="hidden" name="scope" value="{scope}"><input type="hidden" name="code_challenge" value="{challenge}"><input type="hidden" name="code_challenge_method" value="{method}"><input type="hidden" name="resource" value="{resource}">"#,
        client_id = html_escape(&f.client_id),
        redirect_uri = html_escape(&f.redirect_uri),
        state = html_escape(&f.state),
        scope = html_escape(&f.scope),
        challenge = html_escape(&f.code_challenge),
        method = html_escape(&f.code_challenge_method),
        resource = html_escape(&f.resource),
    )
}

fn authorize_query(f: &OAuthFields) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    q.append_pair("response_type", "code")
        .append_pair("client_id", &f.client_id)
        .append_pair("redirect_uri", &f.redirect_uri)
        .append_pair("state", &f.state)
        .append_pair("scope", &f.scope)
        .append_pair("code_challenge", &f.code_challenge)
        .append_pair("code_challenge_method", &f.code_challenge_method)
        .append_pair("resource", &f.resource);
    format!("/oauth/authorize?{}", q.finish())
}

fn login_page(f: &OAuthFields, error: Option<&str>) -> Html<String> {
    let error = error
        .map(|message| format!("<p class=\"error\">{}</p>", html_escape(message)))
        .unwrap_or_default();
    let next = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("next", &authorize_query(f))
        .finish();
    page(
        "登录 KychNet MCP",
        &format!(
            r#"<h1>登录 KychNet MCP</h1><p class="muted">客户端正在请求访问站点。请使用管理员或成员账号登录。</p>{error}<form method="post" action="/oauth/login">{hidden}<label>用户名<input name="username" autocomplete="username" required></label><label>动态验证码<input name="code" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9]{{6}}" required></label><button type="submit">登录并继续</button></form><p class="muted">使用通行密钥：<a href="https://m.kych.net/?{next}">先在管理端登录，再返回授权</a></p>"#,
            hidden = hidden_fields(f),
            next = html_escape(&next),
        ),
    )
}

fn consent_page(f: &OAuthFields, csrf: &str, username: &str) -> Html<String> {
    page(
        "授权 KychNet MCP",
        &format!(
            r#"<h1>授权 KychNet MCP</h1><p>已登录为 <strong>{username}</strong>。</p><p>客户端 <code>{client}</code> 请求以下权限：</p><ul><li>读取站点内容</li><li>写入/删除站点内容</li><li>读取项目 Git 文件</li><li>写入/删除项目 Git 文件</li></ul><form method="post" action="/oauth/approve">{hidden}<input type="hidden" name="csrf" value="{csrf}"><button type="submit">允许并继续</button></form><p class="muted">授权后可随时通过重新登录或撤销 token 停止访问。</p>"#,
            username = html_escape(username),
            client = html_escape(&f.client_id),
            hidden = hidden_fields(f),
            csrf = html_escape(csrf),
        ),
    )
}

fn login_error_page(f: &OAuthFields, message: &str) -> Html<String> {
    login_page(f, Some(message))
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    scope: String,
    code_challenge: String,
    code_challenge_method: String,
    #[serde(default)]
    resource: String,
}

impl AuthorizeQuery {
    fn fields(&self) -> OAuthFields {
        OAuthFields {
            client_id: self.client_id.clone(),
            redirect_uri: self.redirect_uri.clone(),
            state: self.state.clone(),
            scope: self.scope.clone(),
            code_challenge: self.code_challenge.clone(),
            code_challenge_method: self.code_challenge_method.clone(),
            resource: self.resource.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct OAuthFields {
    client_id: String,
    redirect_uri: String,
    state: String,
    scope: String,
    code_challenge: String,
    code_challenge_method: String,
    resource: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    username: String,
    code: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    scope: String,
    code_challenge: String,
    code_challenge_method: String,
    #[serde(default)]
    resource: String,
}

impl LoginForm {
    fn fields(&self) -> OAuthFields {
        OAuthFields {
            client_id: self.client_id.clone(),
            redirect_uri: self.redirect_uri.clone(),
            state: self.state.clone(),
            scope: self.scope.clone(),
            code_challenge: self.code_challenge.clone(),
            code_challenge_method: self.code_challenge_method.clone(),
            resource: self.resource.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ApproveForm {
    csrf: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    scope: String,
    code_challenge: String,
    code_challenge_method: String,
    #[serde(default)]
    resource: String,
}

impl ApproveForm {
    fn fields(&self) -> OAuthFields {
        OAuthFields {
            client_id: self.client_id.clone(),
            redirect_uri: self.redirect_uri.clone(),
            state: self.state.clone(),
            scope: self.scope.clone(),
            code_challenge: self.code_challenge.clone(),
            code_challenge_method: self.code_challenge_method.clone(),
            resource: self.resource.clone(),
        }
    }
}

async fn session_user(state: &AppState, session: &Session) -> Option<crate::models::User> {
    let user_id: Option<Uuid> = session.get("user_id").await.ok().flatten();
    let user_id = user_id?;
    service::find_user_by_id(&state.pool, user_id)
        .await
        .ok()
        .flatten()
        .filter(|user| !user.disabled)
}

async fn validate_authorize(state: &AppState, f: &OAuthFields) -> Result<(), String> {
    if f.redirect_uri.is_empty() || f.client_id.is_empty() {
        return Err("缺少 OAuth 参数".into());
    }
    if f.code_challenge_method != "S256" || f.code_challenge.len() < 43 {
        return Err("必须使用 PKCE S256".into());
    }
    let row: Option<(String,)> =
        sqlx::query_as("SELECT redirect_uris::text FROM oauth_clients WHERE client_id = $1")
            .bind(&f.client_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| format!("读取客户端失败: {e}"))?;
    let Some((redirects,)) = row else {
        return Err("未知客户端".into());
    };
    let uris: Vec<String> = serde_json::from_str(&redirects).unwrap_or_default();
    if !uris.iter().any(|uri| uri == &f.redirect_uri) {
        return Err("redirect_uri 未注册".into());
    }
    if !f.resource.is_empty() && f.resource != MCP_RESOURCE {
        return Err("resource 不是本站 MCP 地址".into());
    }
    Ok(())
}

pub async fn authorize(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    if query.response_type != "code" {
        return (StatusCode::BAD_REQUEST, "response_type 必须是 code").into_response();
    }
    let fields = query.fields();
    if let Err(error) = validate_authorize(&state, &fields).await {
        return (StatusCode::BAD_REQUEST, Html(html_escape(&error))).into_response();
    }
    match session_user(&state, &session).await {
        None => login_page(&fields, None).into_response(),
        Some(user) => {
            let csrf = random_token(24);
            if session.insert("oauth_csrf", csrf.clone()).await.is_err() {
                return (StatusCode::INTERNAL_SERVER_ERROR, "会话写入失败").into_response();
            }
            consent_page(&fields, &csrf, &user.username).into_response()
        }
    }
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    session: Session,
    Form(form): Form<LoginForm>,
) -> Response {
    let ip = oauth_ip(&state, &headers, peer);
    if let Some(retry_after) = state
        .limits
        .check(&format!("oauth:login:ip:{ip}"), OAUTH_LOGIN_PER_IP.0, std::time::Duration::from_secs(OAUTH_LOGIN_PER_IP.1))
    {
        return too_many(retry_after);
    }
    let user_key = form.username.trim().to_lowercase();
    if let Some(retry_after) = state.limits.check(
        &format!("oauth:login:user:{user_key}"),
        OAUTH_LOGIN_PER_USER.0,
        std::time::Duration::from_secs(OAUTH_LOGIN_PER_USER.1),
    ) {
        return too_many(retry_after);
    }
    let fields = form.fields();
    if let Err(error) = validate_authorize(&state, &fields).await {
        return (StatusCode::BAD_REQUEST, Html(html_escape(&error))).into_response();
    }
    let Some(user) = service::find_user_by_username(&state.pool, form.username.trim())
        .await
        .ok()
        .flatten()
    else {
        return login_error_page(&fields, "用户名或验证码错误").into_response();
    };
    if user.disabled {
        return login_error_page(&fields, "账号已禁用").into_response();
    }
    match auth::totp_verify(&state.pool, &user, form.code.trim()).await {
        Ok(true) => {}
        _ => return login_error_page(&fields, "用户名或验证码错误").into_response(),
    }
    if let Err(error) = establish_session(&session, user.id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("建立会话失败: {error}")).into_response();
    }
    Redirect::to(&authorize_query(&fields)).into_response()
}

async fn establish_session(session: &Session, user_id: Uuid) -> anyhow::Result<()> {
    session.cycle_id().await?;
    session.insert("user_id", user_id).await?;
    session.insert("auth_at", Utc::now()).await?;
    Ok(())
}

pub async fn approve(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ApproveForm>,
) -> Response {
    let fields = form.fields();
    if let Err(error) = validate_authorize(&state, &fields).await {
        return (StatusCode::BAD_REQUEST, Html(html_escape(&error))).into_response();
    }
    let Some(user) = session_user(&state, &session).await else {
        return Redirect::to(&authorize_query(&fields)).into_response();
    };
    let expected: Option<String> = session.get("oauth_csrf").await.ok().flatten();
    if expected.as_deref() != Some(form.csrf.as_str()) {
        return (StatusCode::FORBIDDEN, "CSRF 校验失败").into_response();
    }
    let code = random_token(32);
    let scope = if fields.scope.trim().is_empty() {
        SCOPES.to_string()
    } else {
        fields.scope.clone()
    };
    let result = sqlx::query(
        "INSERT INTO oauth_codes
         (code_hash, client_id, user_id, redirect_uri, scope, resource, code_challenge, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(hash_token(&code))
    .bind(&fields.client_id)
    .bind(user.id)
    .bind(&fields.redirect_uri)
    .bind(&scope)
    .bind(MCP_RESOURCE)
    .bind(&fields.code_challenge)
    .bind(Utc::now() + Duration::minutes(10))
    .execute(&state.pool)
    .await;
    if let Err(error) = result {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("创建授权码失败: {error}")).into_response();
    }
    let _ = session.remove::<String>("oauth_csrf").await;
    let mut target = Url::parse(&fields.redirect_uri).expect("已校验的 redirect_uri");
    target.query_pairs_mut().append_pair("code", &code);
    if !fields.state.is_empty() {
        target.query_pairs_mut().append_pair("state", &fields.state);
    }
    Redirect::to(target.as_str()).into_response()
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    #[serde(default)]
    client_name: String,
    redirect_uris: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    token_endpoint_auth_method: Option<String>,
}

fn valid_redirect_uri(raw: &str) -> bool {
    if raw.len() > 2048 {
        return false;
    }
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    if url.fragment().is_some() || url.username() != "" || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1")),
        "https" => url.host_str().is_some(),
        _ => false,
    }
}

pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> Response {
    let ip = oauth_ip(&state, &headers, peer);
    if let Some(retry_after) = state
        .limits
        .check(&format!("oauth:register:ip:{ip}"), OAUTH_REGISTER_PER_IP.0, std::time::Duration::from_secs(OAUTH_REGISTER_PER_IP.1))
    {
        return too_many(retry_after);
    }
    if req.redirect_uris.is_empty() || req.redirect_uris.len() > 5 {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_redirect_uri"}))).into_response();
    }
    if !req.redirect_uris.iter().all(|uri| valid_redirect_uri(uri)) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_redirect_uri"}))).into_response();
    }
    let client_id = format!("mcp_{}", random_token(24));
    let name = if req.client_name.trim().is_empty() {
        "MCP client".to_string()
    } else {
        req.client_name.chars().take(120).collect()
    };
    let redirects = serde_json::to_string(&req.redirect_uris).unwrap_or_else(|_| "[]".into());
    if let Err(error) = sqlx::query(
        "INSERT INTO oauth_clients (client_id, client_name, redirect_uris) VALUES ($1, $2, $3::jsonb)",
    )
    .bind(&client_id)
    .bind(&name)
    .bind(&redirects)
    .execute(&state.pool)
    .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"server_error","detail":error.to_string()}))).into_response();
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "client_name": name,
            "redirect_uris": req.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"]
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct TokenForm {
    grant_type: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    redirect_uri: String,
    client_id: String,
    #[serde(default)]
    code_verifier: String,
    #[serde(default)]
    refresh_token: String,
}

#[derive(Debug, sqlx::FromRow)]
struct CodeRow {
    client_id: String,
    user_id: Uuid,
    redirect_uri: String,
    scope: String,
    resource: String,
    code_challenge: String,
    expires_at: chrono::DateTime<Utc>,
    used_at: Option<chrono::DateTime<Utc>>,
}

async fn issue_tokens(
    pool: &PgPool,
    user_id: Uuid,
    client_id: &str,
    scope: &str,
    resource: &str,
) -> anyhow::Result<Value> {
    let access = random_token(32);
    let refresh = random_token(32);
    sqlx::query(
        "INSERT INTO oauth_tokens
         (id, access_hash, refresh_hash, user_id, client_id, scope, resource,
          access_expires_at, refresh_expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::new_v4())
    .bind(hash_token(&access))
    .bind(hash_token(&refresh))
    .bind(user_id)
    .bind(client_id)
    .bind(scope)
    .bind(resource)
    .bind(Utc::now() + Duration::seconds(ACCESS_TTL_SECS))
    .bind(Utc::now() + Duration::seconds(REFRESH_TTL_SECS))
    .execute(pool)
    .await?;
    Ok(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL_SECS,
        "refresh_token": refresh,
        "scope": scope
    }))
}

pub async fn token(State(state): State<AppState>, Form(form): Form<TokenForm>) -> Response {
    match form.grant_type.as_str() {
        "authorization_code" => {
            if form.code.is_empty() || form.code_verifier.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_request"}))).into_response();
            }
            let row: Option<CodeRow> = sqlx::query_as(
                "SELECT client_id, user_id, redirect_uri, scope, resource, code_challenge, expires_at, used_at
                 FROM oauth_codes WHERE code_hash = $1",
            )
            .bind(hash_token(&form.code))
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
            let Some(row) = row else {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))).into_response();
            };
            if row.client_id != form.client_id
                || row.redirect_uri != form.redirect_uri
                || row.used_at.is_some()
                || row.expires_at < Utc::now()
                || row.resource != MCP_RESOURCE
            {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))).into_response();
            }
            if pkce_challenge(&form.code_verifier) != row.code_challenge {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))).into_response();
            }
            let updated = sqlx::query(
                "UPDATE oauth_codes SET used_at = now() WHERE code_hash = $1 AND used_at IS NULL",
            )
            .bind(hash_token(&form.code))
            .execute(&state.pool)
            .await;
            let Ok(updated) = updated else {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"server_error"}))).into_response();
            };
            if updated.rows_affected() != 1 {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))).into_response();
            }
            match issue_tokens(&state.pool, row.user_id, &row.client_id, &row.scope, &row.resource).await {
                Ok(value) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
                Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"server_error","detail":error.to_string()}))).into_response(),
            }
        }
        "refresh_token" => {
            if form.refresh_token.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_request"}))).into_response();
            }
            let row: Option<(Uuid, String, String, String, Option<chrono::DateTime<Utc>>)> = sqlx::query_as(
                "SELECT user_id, client_id, scope, resource, refresh_expires_at
                 FROM oauth_tokens WHERE refresh_hash = $1 AND revoked_at IS NULL",
            )
            .bind(hash_token(&form.refresh_token))
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
            let Some((user_id, client_id, scope, resource, expires)) = row else {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))).into_response();
            };
            if client_id != form.client_id
                || expires.is_none_or(|expiry| expiry < Utc::now())
                || resource != MCP_RESOURCE
            {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))).into_response();
            }
            let _ = sqlx::query("UPDATE oauth_tokens SET revoked_at = now() WHERE refresh_hash = $1")
                .bind(hash_token(&form.refresh_token))
                .execute(&state.pool)
                .await;
            match issue_tokens(&state.pool, user_id, &client_id, &scope, &resource).await {
                Ok(value) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
                Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"server_error","detail":error.to_string()}))).into_response(),
            }
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"error":"unsupported_grant_type"}))).into_response(),
    }
}

pub async fn protected_resource() -> Json<Value> {
    Json(json!({
        "resource": MCP_RESOURCE,
        "authorization_servers": [OAUTH_ORIGIN],
        "scopes_supported": SCOPES.split(' ').collect::<Vec<_>>(),
        "bearer_methods_supported": ["header"]
    }))
}

pub async fn authorization_server() -> Json<Value> {
    Json(json!({
        "issuer": OAUTH_ORIGIN,
        "authorization_endpoint": format!("{OAUTH_ORIGIN}/oauth/authorize"),
        "token_endpoint": format!("{OAUTH_ORIGIN}/oauth/token"),
        "registration_endpoint": format!("{OAUTH_ORIGIN}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": SCOPES.split(' ').collect::<Vec<_>>()
    }))
}

/// 清理过期授权码、失效 token 和长期未使用的客户端；启动时执行一次即可。
pub async fn gc(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        "DELETE FROM oauth_codes
         WHERE expires_at < now() - interval '1 day'
            OR (used_at IS NOT NULL AND used_at < now() - interval '1 day')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "DELETE FROM oauth_tokens
         WHERE (access_expires_at < now()
                AND (refresh_expires_at IS NULL OR refresh_expires_at < now()))
            OR (revoked_at IS NOT NULL AND revoked_at < now() - interval '30 days')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "DELETE FROM oauth_clients c
         WHERE c.created_at < now() - interval '90 days'
           AND NOT EXISTS (SELECT 1 FROM oauth_tokens t WHERE t.client_id = c.client_id)
           AND NOT EXISTS (SELECT 1 FROM oauth_codes k WHERE k.client_id = c.client_id)",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{pkce_challenge, valid_redirect_uri};

    #[test]
    fn pkce_matches_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn redirect_uri_validation() {
        assert!(valid_redirect_uri("http://127.0.0.1:8765/callback"));
        assert!(valid_redirect_uri("http://localhost:3000/cb"));
        assert!(valid_redirect_uri("https://claude.ai/api/mcp/auth_callback"));
        assert!(!valid_redirect_uri("http://evil.com/cb"));
        assert!(!valid_redirect_uri("https://evil.com/cb#fragment"));
        assert!(!valid_redirect_uri("javascript:alert(1)"));
    }
}
