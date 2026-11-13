use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use totp_rs::{Algorithm, Secret, TOTP};
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::models::{PasskeyRow, User};
use crate::service;

/// 一次没走完的用户注册：用户还不存在，资料暂存在认证流程里。
///
/// `code` 只在校验凭证成功后才拿去占用邀请码，所以中途放弃不消耗名额。
#[derive(Serialize, Deserialize)]
struct PendingRegistration {
    user_id: Uuid,
    username: String,
    code: String,
    registration: Value,
}

/// webauthn-rs 的 credential ID 是字节数组，库里按小写十六进制文本存。
fn hex_cred_id(passkey: &Passkey) -> String {
    passkey
        .cred_id()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

pub struct Auth {
    pub webauthn: Arc<Webauthn>,
}

impl Auth {
    /// `rp_name` 显示在通行密钥的凭据对话框里；空则回退 rp_id，
    /// 不再硬编码品牌名（改一次 env 即可换品牌）。
    pub fn new(rp_id: &str, rp_origin: &str, rp_name: &str) -> Result<Self> {
        let origin = Url::parse(rp_origin).context("WEBAUTHN_ORIGIN 不是合法 URL")?;
        let rp_name = if rp_name.trim().is_empty() { rp_id } else { rp_name.trim() };
        let builder = WebauthnBuilder::new(rp_id, &origin)
            .map_err(|e| anyhow!("WebAuthn 配置错误: {e}"))?
            .rp_name(rp_name);
        let webauthn = builder
            .build()
            .map_err(|e| anyhow!("WebAuthn 构建失败: {e}"))?;
        Ok(Self {
            webauthn: Arc::new(webauthn),
        })
    }

    async fn save_flow(
        &self,
        pool: &PgPool,
        user_id: Option<Uuid>,
        kind: &str,
        state: Value,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO auth_flows (id, user_id, kind, state, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(user_id)
        .bind(kind)
        .bind(state)
        .bind(Utc::now() + Duration::minutes(5))
        .execute(pool)
        .await?;
        Ok(id)
    }

    async fn take_flow(
        &self,
        pool: &PgPool,
        id: Uuid,
        expected: &str,
    ) -> Result<(Option<Uuid>, Value)> {
        // 先在同一事务里加锁读取并校验，通过后再删除。
        // 旧实现是「DELETE ... RETURNING」先消费再校验：只要攻击者拿到 flow_id
        // 并用错误的 kind / 过期流程去请求，就能把别人的认证流程消耗掉（拒绝服务）。
        let mut tx = pool.begin().await?;
        let row = sqlx::query_as::<_, (String, Option<Uuid>, Value, chrono::DateTime<Utc>)>(
            "SELECT kind, user_id, state, expires_at FROM auth_flows WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .context("认证流程不存在或已使用")?;

        let (kind, user_id, state, expires_at) = row;
        if kind != expected {
            bail!("认证流程类型不匹配");
        }
        if expires_at < Utc::now() {
            bail!("认证流程已过期");
        }

        sqlx::query("DELETE FROM auth_flows WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((user_id, state))
    }

    /// 生成一份 Passkey 注册挑战（纯内存，不访问数据库）。
    ///
    /// 新用户没有任何已注册凭证，所以 exclude 传空。返回的 state 交给
    /// `start_pending_registration` 暂存，等浏览器真正把凭证做出来才建用户。
    /// 注意 `user.id` 必须与最终落库的用户 ID 一致，它是 WebAuthn 的 user handle。
    pub fn create_registration_challenge(
        &self,
        user: &User,
    ) -> Result<(CreationChallengeResponse, Value)> {
        let (options, state) = self
            .webauthn
            .start_passkey_registration(user.id, &user.username, &user.username, None)
            .map_err(|e| anyhow!("无法开始 Passkey 注册: {e}"))?;
        Ok((options, serde_json::to_value(state)?))
    }

    /// 为「还没落库的新用户」保存注册流程。
    ///
    /// 用户资料与邀请码一起塞进 flow 的 state 里，`auth_flows.user_id` 留空：
    /// 这样浏览器放弃认证（关掉弹窗、超时、换设备）时数据库里什么都不剩，
    /// 邀请码也不会被一次没走完的仪式消耗掉。
    pub async fn start_pending_registration(
        &self,
        pool: &PgPool,
        user: &User,
        code: &str,
        registration: Value,
    ) -> Result<Uuid> {
        let pending = PendingRegistration {
            user_id: user.id,
            username: user.username.clone(),
            code: code.to_owned(),
            registration,
        };
        self.save_flow(pool, None, "register", serde_json::to_value(pending)?)
            .await
    }

    /// 走完新用户的注册：先验证凭证，再在一个事务里占用邀请码、建用户、存凭证。
    ///
    /// 顺序很重要——凭证校验是纯内存的，放在前面才能保证「失败的仪式不烧码」。
    pub async fn finish_pending_registration(
        &self,
        pool: &PgPool,
        flow_id: Uuid,
        credential: &RegisterPublicKeyCredential,
    ) -> Result<User> {
        let (user_id, state) = self.take_flow(pool, flow_id, "register").await?;
        if user_id.is_some() {
            bail!("这是已有用户的通行密钥绑定流程，请改用绑定接口");
        }
        let pending: PendingRegistration = serde_json::from_value(state)?;
        let registration: PasskeyRegistration = serde_json::from_value(pending.registration)?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(credential, &registration)
            .map_err(|e| anyhow!("Passkey 注册校验失败: {e}"))?;

        service::complete_registration(
            pool,
            pending.user_id,
            &pending.code,
            &pending.username,
            &hex_cred_id(&passkey),
            &serde_json::to_value(&passkey)?,
        )
        .await
    }

    pub async fn start_registration(
        &self,
        pool: &PgPool,
        user: &User,
    ) -> Result<(CreationChallengeResponse, Uuid)> {
        let existing = load_passkeys(pool, user.id).await?;
        let exclude = if existing.is_empty() {
            None
        } else {
            Some(existing.iter().map(|p| p.cred_id().clone()).collect())
        };
        let (options, state) = self
            .webauthn
            .start_passkey_registration(user.id, &user.username, &user.username, exclude)
            .map_err(|e| anyhow!("无法开始 Passkey 注册: {e}"))?;
        let flow_id = self
            .save_flow(pool, Some(user.id), "register", serde_json::to_value(state)?)
            .await?;
        Ok((options, flow_id))
    }

    pub async fn finish_registration(
        &self,
        pool: &PgPool,
        flow_id: Uuid,
        credential: &RegisterPublicKeyCredential,
    ) -> Result<Uuid> {
        let (user_id, state) = self.take_flow(pool, flow_id, "register").await?;
        // 已登录用户加通行密钥：流程必须绑着具体用户，否则会走到注册那条路。
        let user_id = user_id.context("认证流程缺少用户，请重新发起绑定")?;
        let state: PasskeyRegistration = serde_json::from_value(state)?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(credential, &state)
            .map_err(|e| anyhow!("Passkey 注册校验失败: {e}"))?;

        let credential_id = hex_cred_id(&passkey);
        sqlx::query(
            "INSERT INTO passkey_credentials (id, user_id, credential_id, passkey)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(credential_id)
        .bind(serde_json::to_value(&passkey)?)
        .execute(pool)
        .await?;
        Ok(user_id)
    }

    pub async fn start_authentication(
        &self,
        pool: &PgPool,
        user: &User,
    ) -> Result<(RequestChallengeResponse, Uuid)> {
        let passkeys = load_passkeys(pool, user.id).await?;
        if passkeys.is_empty() {
            bail!("该用户尚未绑定通行密钥");
        }
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(|e| anyhow!("无法开始 Passkey 登录: {e}"))?;
        let flow_id = self
            .save_flow(pool, Some(user.id), "login", serde_json::to_value(state)?)
            .await?;
        Ok((options, flow_id))
    }

    pub async fn finish_authentication(
        &self,
        pool: &PgPool,
        flow_id: Uuid,
        credential: &PublicKeyCredential,
    ) -> Result<Uuid> {
        let (user_id, state) = self.take_flow(pool, flow_id, "login").await?;
        let user_id = user_id.context("登录流程缺少用户")?;
        let state: PasskeyAuthentication = serde_json::from_value(state)?;
        let result = self
            .webauthn
            .finish_passkey_authentication(credential, &state)
            .map_err(|e| anyhow!("Passkey 登录校验失败: {e}"))?;

        if result.needs_update() {
            let rows = load_passkey_rows(pool, user_id).await?;
            for row in rows {
                let mut passkey: Passkey = serde_json::from_value(row.passkey)?;
                if passkey.cred_id() == result.cred_id() {
                    passkey.update_credential(&result);
                    sqlx::query("UPDATE passkey_credentials SET passkey = $2 WHERE id = $1")
                        .bind(row.id)
                        .bind(serde_json::to_value(&passkey)?)
                        .execute(pool)
                        .await?;
                }
            }
        }
        Ok(user_id)
    }
}

pub async fn load_passkeys(pool: &PgPool, user_id: Uuid) -> Result<Vec<Passkey>> {
    load_passkey_rows(pool, user_id)
        .await?
        .into_iter()
        .map(|row| serde_json::from_value(row.passkey).map_err(Into::into))
        .collect()
}

async fn load_passkey_rows(pool: &PgPool, user_id: Uuid) -> Result<Vec<PasskeyRow>> {
    Ok(sqlx::query_as::<_, PasskeyRow>(
        "SELECT * FROM passkey_credentials WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// `issuer` 出现在验证器 App 的条目名里；传空则不带 issuer 段，
/// 码值不受影响（TOTP 的 HMAC 只与密钥和时间有关）。

/// TOTP issuer：TOTP_ISSUER env，未设置则不带 issuer 段。
fn totp_issuer() -> String {
    std::env::var("TOTP_ISSUER").unwrap_or_default()
}

pub fn build_totp(username: &str, secret: &[u8], issuer: &str) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret.to_vec(),
        (!issuer.trim().is_empty()).then(|| issuer.trim().to_string()),
        username.to_string(),
    )
    .map_err(|e| anyhow!("TOTP 配置失败: {e}"))
}

pub async fn provision_totp(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
) -> Result<(String, String)> {
    let secret = Secret::generate_secret();
    let bytes = secret.to_bytes()?;
    // CLI 建管理员时必须立刻可用（没有人能替它走一遍确认流程），
    // 所以这里直接写成生效密钥，并把可能残留的待确认密钥清掉。
    sqlx::query(
        "INSERT INTO totp_secrets (user_id, secret, pending_secret, confirmed_at)
         VALUES ($1, $2, NULL, now())
         ON CONFLICT (user_id) DO UPDATE
         SET secret = EXCLUDED.secret, pending_secret = NULL, confirmed_at = now()",
    )
    .bind(user_id)
    .bind(&bytes)
    .execute(pool)
    .await?;

    let totp = build_totp(username, &bytes, &totp_issuer())?;
    Ok((totp.get_url(), totp.get_secret_base32()))
}

/// 生成一份**待确认**的 TOTP 密钥，不动已经生效的旧密钥。
///
/// 旧实现是这一步就直接 `secret = 新值, confirmed_at = NULL`，两个后果：
/// 1. 会话一旦被劫持，攻击者能把用户的 TOTP 换成自己的、再走一次确认，完成持久接管；
/// 2. 用户点了「生成验证码」却没走完确认，原来的 TOTP 会当场失效（`confirmed_at` 为空
///    时 `totp_verify` 直接返回 false），只能回 CLI 恢复。
/// 现在新密钥先进 `pending_secret`，只有确认成功后才提升为生效密钥。
pub async fn totp_setup(pool: &PgPool, user_id: Uuid, username: &str) -> Result<(String, String)> {
    let secret = Secret::generate_secret();
    let bytes = secret.to_bytes()?;
    sqlx::query(
        "INSERT INTO totp_secrets (user_id, pending_secret)
         VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET pending_secret = EXCLUDED.pending_secret",
    )
    .bind(user_id)
    .bind(&bytes)
    .execute(pool)
    .await?;

    let totp = build_totp(username, &bytes, &totp_issuer())?;
    Ok((totp.get_url(), totp.get_secret_base32()))
}

/// 用一次正确的验证码确认待生效的密钥；成功后才替换掉旧密钥。
pub async fn totp_confirm(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    code: &str,
) -> Result<bool> {
    let pending: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT pending_secret FROM totp_secrets WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    let Some(pending) = pending else {
        bail!("没有待确认的 TOTP 密钥，请重新生成后再验证");
    };
    let totp = build_totp(username, &pending, &totp_issuer())?;
    if !totp.check_current(code)? {
        return Ok(false);
    }
    // WHY: SELECT 与 UPDATE 之间并发的 totp_setup 可能换掉 pending_secret，
    // 无条件提升会把没验证过的新密钥直接生效；绑定已验证的字节，不匹配就拒绝。
    let updated = sqlx::query(
        "UPDATE totp_secrets
         SET secret = pending_secret, pending_secret = NULL, confirmed_at = now()
         WHERE user_id = $1 AND pending_secret = $2",
    )
    .bind(user_id)
    .bind(&pending)
    .execute(pool)
    .await?;
    if updated.rows_affected() == 0 {
        bail!("待确认的密钥已变化，请重新生成后再验证");
    }
    Ok(true)
}

pub async fn totp_verify(pool: &PgPool, user: &User, code: &str) -> Result<bool> {
    let row: Option<(Option<Vec<u8>>, Option<chrono::DateTime<Utc>>)> = sqlx::query_as(
        "SELECT secret, confirmed_at FROM totp_secrets WHERE user_id = $1",
    )
    .bind(user.id)
    .fetch_optional(pool)
    .await?;
    // 只认「已确认的生效密钥」：待确认密钥和从没绑定过的账号都不能登录。
    let Some((Some(secret), Some(_))) = row else {
        return Ok(false);
    };
    let totp = build_totp(&user.username, &secret, &totp_issuer())?;
    Ok(totp.check_current(code)?)
}
