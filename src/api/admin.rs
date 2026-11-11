//! 从 `api.rs` 拆出的子模块。路由注册与公共类型见 `super`（`api/mod.rs`）。
use super::*;

pub(crate) async fn invites(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let items = service::list_invites(&state.pool).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub(crate) struct CreateInviteRequest {
    #[serde(default = "default_uses")]
    max_uses: i32,
    #[serde(default = "default_expiry")]
    expires_in_days: i64,
}

pub(crate) fn default_uses() -> i32 {
    1
}

pub(crate) fn default_expiry() -> i64 {
    7
}

pub(crate) async fn create_invite(
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

pub(crate) async fn revoke_invite(
    State(state): State<AppState>,
    session: Session,
    Path(code): Path<String>,
) -> ApiResult<StatusCode> {
    current_admin(&state, &session).await?;
    service::revoke_invite(&state.pool, &code).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn users(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let items = service::list_users(&state.pool).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub(crate) struct PatchUserRequest {
    disabled: bool,
}

pub(crate) async fn patch_user(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchUserRequest>,
) -> ApiResult<StatusCode> {
    let admin = current_admin(&state, &session).await?;
    service::set_user_disabled_guarded(&state.pool, admin.id, id, req.disabled).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let site = service::get_settings(&state.pool).await?;
    Ok(Json(json!({ "site": site, "site_url": state.site_url })))
}

pub(crate) async fn put_settings(
    State(state): State<AppState>,
    session: Session,
    Json(site): Json<SiteSettings>,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    if site.site_name.trim().is_empty() || site.site_name.chars().count() > 64 {
        api_bail!("站点名称需为 1-64 个字符");
    }
    if site.description.chars().count() > 300 {
        api_bail!("站点描述最多 300 个字符");
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
    if site.icp_beian.chars().count() > 64 || site.police_beian.chars().count() > 64 {
        api_bail!("备案号最多 64 个字符");
    }
    if site.police_beian_code.chars().count() > 32
        || !site.police_beian_code.chars().all(|c| c.is_ascii_digit())
    {
        api_bail!("公安备案编号只能填写数字");
    }
    service::save_settings(&state.pool, &site).await?;
    Ok(Json(json!({ "site": site })))
}

pub(crate) async fn upload_avatar(
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

pub(crate) async fn avatar(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Response> {
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
