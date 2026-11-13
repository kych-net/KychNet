//! 从 `api.rs` 拆出的子模块。路由注册与公共类型见 `super`（`api/mod.rs`）。
use super::*;

///
/// 这里挡住的是「在线暴力尝试」，不是「离线爆破数据库」——TOTP 密钥明文入库，
/// 数据库一旦泄露就只剩下密钥派生强度，限流帮不上忙。
pub(crate) const LOGIN_START_PER_USER: (u32, u64) = (10, 60);
pub(crate) const LOGIN_START_PER_IP: (u32, u64) = (30, 60);
pub(crate) const TOTP_LOGIN_PER_IP: (u32, u64) = (10, 60);
/// 验证码 6 位、单步 30 秒（`check_current` 允许 ±1 步），
/// 因此每账号 15 分钟内最多 5 次，猜中的概率被压到可以忽略。
pub(crate) const TOTP_ATTEMPTS_PER_USER: (u32, u64) = (5, 900);
/// 绑定流程里确认密钥的尝试次数；猜中一次就等于换绑成功，所以比登录更紧。
pub(crate) const TOTP_CONFIRM_PER_USER: (u32, u64) = (5, 900);
pub(crate) const REGISTER_PER_IP: (u32, u64) = (10, 60);
/// Passkey step-up / 绑定每次发起都会插一行 auth_flows，按用户限流防止刷表。
pub(crate) const PASSKEY_FLOW_PER_USER: (u32, u64) = (10, 60);

#[derive(Deserialize)]
pub(crate) struct RegisterRequest {
    code: String,
    username: String,
}

#[derive(Serialize)]
pub(crate) struct RegisterResponse {
    user: User,
    flow_id: Uuid,
    options: CreationChallengeResponse,
}

pub(crate) async fn register(
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
pub(crate) struct FinishRegistrationRequest {
    flow_id: Uuid,
    credential: RegisterPublicKeyCredential,
}

pub(crate) async fn finish_registration(
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
pub(crate) struct LoginStartRequest {
    username: String,
}

#[derive(Serialize)]
pub(crate) struct LoginStartResponse {
    flow_id: Uuid,
    options: webauthn_rs::prelude::RequestChallengeResponse,
}

pub(crate) async fn login_start(
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
pub(crate) struct LoginFinishRequest {
    flow_id: Uuid,
    credential: PublicKeyCredential,
}

pub(crate) async fn login_finish(
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
pub(crate) struct TotpLoginRequest {
    username: String,
    code: String,
}

pub(crate) async fn totp_login(
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
    if !crate::auth::totp_verify(&state.pool, &user, &req.code).await? {
        return Err(login_failed("TOTP 登录：验证码错误"));
    }
    state.limits.reset(&user_key);
    establish_session(&session, user.id).await?;
    Ok(Json(user))
}

pub(crate) async fn me(State(state): State<AppState>, session: Session) -> ApiResult<Json<Option<User>>> {
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

pub(crate) async fn logout(session: Session) -> ApiResult<StatusCode> {
    session
        .flush()
        .await
        .map_err(|e| anyhow!("清除会话失败: {e}"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct StepUpTotpRequest {
    code: String,
}

pub(crate) async fn mark_step_up(session: &Session) -> ApiResult<()> {
    session
        .insert("auth_at", Utc::now().timestamp())
        .await
        .map_err(|e| anyhow!("写入会话失败: {e}"))?;
    Ok(())
}

/// 用当前账号的 TOTP 重新确认身份，刷新会话里的 `auth_at`。
pub(crate) async fn step_up_totp(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<StepUpTotpRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let key = format!("stepup:totp:user:{}", user.id);
    limited(&state, &key, TOTP_ATTEMPTS_PER_USER)?;
    if !crate::auth::totp_verify(&state.pool, &user, &req.code).await? {
        api_bail!("动态验证码错误");
    }
    state.limits.reset(&key);
    mark_step_up(&session).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn step_up_passkey_start(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    // WHY: 每次发起都往 auth_flows 插一行，不限流就能被登录用户无限刷表。
    limited(
        &state,
        &format!("stepup:passkey:user:{}", user.id),
        PASSKEY_FLOW_PER_USER,
    )?;
    let (options, flow_id) = state.auth.start_authentication(&state.pool, &user).await?;
    Ok(Json(json!({ "flow_id": flow_id, "options": options })))
}

pub(crate) async fn step_up_passkey_finish(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<LoginFinishRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let user_id = state
        .auth
        .finish_authentication(&state.pool, req.flow_id, &req.credential)
        .await?;
    if user_id != user.id {
        api_bail!("认证流程不属于当前用户");
    }
    mark_step_up(&session).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn totp_setup(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_step_up(&session).await?;
    let (url, secret) = crate::auth::totp_setup(&state.pool, user.id, &user.username).await?;
    Ok(Json(json!({ "otpauth_url": url, "secret": secret })))
}

pub(crate) async fn passkey_setup(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_step_up(&session).await?;
    // WHY: 绑定流程同样每次发起都插 auth_flows 行，与 step-up 发起共用额度上限。
    limited(
        &state,
        &format!("passkey-setup:user:{}", user.id),
        PASSKEY_FLOW_PER_USER,
    )?;
    let (options, flow_id) = state.auth.start_registration(&state.pool, &user).await?;
    Ok(Json(json!({ "flow_id": flow_id, "options": options })))
}

#[derive(Deserialize)]
pub(crate) struct PasskeySetupFinishRequest {
    flow_id: Uuid,
    credential: RegisterPublicKeyCredential,
}

pub(crate) async fn passkey_setup_finish(
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
pub(crate) struct TotpConfirmRequest {
    code: String,
}

pub(crate) async fn totp_confirm(
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
    let ok = crate::auth::totp_confirm(&state.pool, user.id, &user.username, &req.code).await?;
    if !ok {
        api_bail!("验证码错误");
    }
    state.limits.reset(&format!("totp-confirm:user:{}", user.id));
    Ok(Json(json!({ "ok": true })))
}
