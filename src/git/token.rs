use anyhow::{bail, Result};
use axum::http::{header, HeaderMap};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::models::User;

/// Git HTTP 写操作令牌。明文只在创建时返回一次，库里只存 SHA-256。
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GitToken {
    pub id: Uuid,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("kych_{}", hex::encode(bytes))
}

pub async fn create(pool: &PgPool, user_id: Uuid, name: &str) -> Result<(GitToken, String)> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        bail!("令牌名称需为 1-100 个字符");
    }
    let token = new_token();
    let item = sqlx::query_as::<_, GitToken>(
        "INSERT INTO git_tokens (id, user_id, name, token_hash)
         VALUES ($1, $2, $3, $4)
         RETURNING id, name, created_at, expires_at, last_used_at",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(name)
    .bind(&hash(&token)[..])
    .fetch_one(pool)
    .await?;
    Ok((item, token))
}

pub async fn list(pool: &PgPool, user_id: Uuid) -> Result<Vec<GitToken>> {
    Ok(sqlx::query_as::<_, GitToken>(
        "SELECT id, name, created_at, expires_at, last_used_at
         FROM git_tokens WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub async fn revoke(pool: &PgPool, user_id: Uuid, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM git_tokens WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 从 `Authorization` 头里取令牌并解析用户。
///
/// 支持 `Bearer <token>` 与 Git 客户端常用的 `Basic <base64(用户名:令牌)>`。
/// 用户名部分不参与校验，真正的凭据是令牌本身。
pub async fn authenticate(pool: &PgPool, headers: &HeaderMap) -> Result<Option<User>> {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };
    let token = if let Some(rest) = value.strip_prefix("Bearer ") {
        rest.trim().to_string()
    } else if let Some(rest) = value.strip_prefix("Basic ") {
        let decoded = STANDARD
            .decode(rest.trim())
            .map_err(|_| anyhow::anyhow!("Basic 认证头不是合法 base64"))?;
        let text = String::from_utf8(decoded)
            .map_err(|_| anyhow::anyhow!("Basic 认证头不是 UTF-8"))?;
        match text.split_once(':') {
            Some((_, password)) => password.to_string(),
            None => return Ok(None),
        }
    } else {
        return Ok(None);
    };
    if token.is_empty() {
        return Ok(None);
    }

    let row = sqlx::query(
        "SELECT u.id, u.username, u.disabled, u.is_admin, u.created_at
         FROM git_tokens t
         JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = $1
           AND (t.expires_at IS NULL OR t.expires_at > now())",
    )
    .bind(&hash(&token)[..])
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };
    let user = User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        disabled: row.try_get("disabled")?,
        is_admin: row.try_get("is_admin")?,
        created_at: row.try_get("created_at")?,
    };
    if user.disabled {
        return Ok(None);
    }

    sqlx::query("UPDATE git_tokens SET last_used_at = now() WHERE token_hash = $1")
        .bind(&hash(&token)[..])
        .execute(pool)
        .await?;
    Ok(Some(user))
}
