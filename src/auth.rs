use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use totp_rs::{Algorithm, Secret, TOTP};
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use crate::models::{PasskeyRow, User};

pub struct Auth {
    pub webauthn: Arc<Webauthn>,
}

impl Auth {
    pub fn new(rp_id: &str, rp_origin: &str) -> Result<Self> {
        let origin = Url::parse(rp_origin).context("WEBAUTHN_ORIGIN 不是合法 URL")?;
        let builder = WebauthnBuilder::new(rp_id, &origin)
            .map_err(|e| anyhow!("WebAuthn 配置错误: {e}"))?
            .rp_name("KychNet");
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
        user_id: Uuid,
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

    async fn take_flow(&self, pool: &PgPool, id: Uuid, expected: &str) -> Result<(Uuid, Value)> {
        let row = sqlx::query_as::<_, (String, Uuid, Value, chrono::DateTime<Utc>)>(
            "DELETE FROM auth_flows WHERE id = $1 RETURNING kind, user_id, state, expires_at",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?
        .context("认证流程不存在或已使用")?;

        let (kind, user_id, state, expires_at) = row;
        if kind != expected {
            bail!("认证流程类型不匹配");
        }
        if expires_at < Utc::now() {
            bail!("认证流程已过期");
        }
        Ok((user_id, state))
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
            .start_passkey_registration(user.id, &user.username, &user.display_name, exclude)
            .map_err(|e| anyhow!("无法开始 Passkey 注册: {e}"))?;
        let flow_id = self
            .save_flow(pool, user.id, "register", serde_json::to_value(state)?)
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
        let state: PasskeyRegistration = serde_json::from_value(state)?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(credential, &state)
            .map_err(|e| anyhow!("Passkey 注册校验失败: {e}"))?;

        let credential_id = passkey
            .cred_id()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
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
            .save_flow(pool, user.id, "login", serde_json::to_value(state)?)
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

pub fn build_totp(username: &str, secret: &[u8]) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret.to_vec(),
        Some("KychNet".to_string()),
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
    sqlx::query(
        "INSERT INTO totp_secrets (user_id, secret, confirmed_at)
         VALUES ($1, $2, now())
         ON CONFLICT (user_id) DO UPDATE SET secret = EXCLUDED.secret, confirmed_at = now()",
    )
    .bind(user_id)
    .bind(&bytes)
    .execute(pool)
    .await?;

    let totp = build_totp(username, &bytes)?;
    Ok((totp.get_url(), totp.get_secret_base32()))
}

pub async fn totp_setup(pool: &PgPool, user_id: Uuid, username: &str) -> Result<(String, String)> {
    let secret = Secret::generate_secret();
    let bytes = secret.to_bytes()?;
    sqlx::query(
        "INSERT INTO totp_secrets (user_id, secret, confirmed_at)
         VALUES ($1, $2, NULL)
         ON CONFLICT (user_id) DO UPDATE SET secret = EXCLUDED.secret, confirmed_at = NULL",
    )
    .bind(user_id)
    .bind(&bytes)
    .execute(pool)
    .await?;

    let totp = build_totp(username, &bytes)?;
    Ok((totp.get_url(), totp.get_secret_base32()))
}

pub async fn totp_confirm(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    code: &str,
) -> Result<bool> {
    let secret: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT secret FROM totp_secrets WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    let Some(secret) = secret else {
        bail!("尚未生成 TOTP 密钥");
    };
    let totp = build_totp(username, &secret)?;
    if !totp.check_current(code)? {
        return Ok(false);
    }
    sqlx::query("UPDATE totp_secrets SET confirmed_at = now() WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(true)
}

pub async fn totp_verify(pool: &PgPool, user: &User, code: &str) -> Result<bool> {
    let secret: Option<(Vec<u8>, Option<chrono::DateTime<Utc>>)> =
        sqlx::query_as("SELECT secret, confirmed_at FROM totp_secrets WHERE user_id = $1")
            .bind(user.id)
            .fetch_optional(pool)
            .await?;
    let Some((secret, confirmed_at)) = secret else {
        return Ok(false);
    };
    if confirmed_at.is_none() {
        return Ok(false);
    }
    let totp = build_totp(&user.username, &secret)?;
    Ok(totp.check_current(code)?)
}
