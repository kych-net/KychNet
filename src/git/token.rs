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

/// SSH 侧 `git-lfs-authenticate` 签发的临时令牌名称前缀。
///
/// `create_ephemeral` 会在后面接一个 UUID，保证每次会话独占一行。
pub const LFS_EPHEMERAL_NAME: &str = "git-lfs-ssh";

/// 签发一个限时令牌（SSH 侧的 `git-lfs-authenticate` 用）。
///
/// LFS 对象走 HTTP basic 传输，上传需要 `git_write_user()` 认可的令牌，
/// 但 SSH 用户手上只有公钥。这里为已通过公钥认证的用户临时发一个
/// Bearer 令牌，明文只在创建时返回一次，库里仍只存 SHA-256。
///
/// **每次签发独立一行**：git-lfs 会用多个 worker 并发执行
/// `git-lfs-authenticate … upload`。旧实现按固定名称先删同名行再插入，
/// 后签发的会话会把前一个会话仍在使用的令牌删掉，导致先发出的 batch
/// 请求立刻 401、上传失败。现在名称带 UUID 后缀，清理只回收**已过期**
/// 的行，并且与插入放在同一事务里。
pub async fn create_ephemeral(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    ttl_secs: i64,
) -> Result<String> {
    let token = new_token();
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_secs);
    let unique_name = format!("{name}-{}", Uuid::new_v4());
    let mut tx = pool.begin().await?;
    // 长期令牌的 expires_at 为 NULL，不受影响；顺手回收该用户积压的
    // 过期临时行，避免每次 LFS 会话都往 git_tokens 里堆新行。
    sqlx::query(
        "DELETE FROM git_tokens
         WHERE user_id = $1 AND expires_at IS NOT NULL AND expires_at <= now()",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO git_tokens (id, user_id, name, token_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(unique_name.as_str())
    .bind(&hash(&token)[..])
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(token)
}

pub async fn list(pool: &PgPool, user_id: Uuid) -> Result<Vec<GitToken>> {
    // SSH LFS 会话签发的临时令牌不进入管理列表：一小时后自动失效，由
    // create_ephemeral 顺带回收；展示出来只会刷屏，且无法主动续期。
    Ok(sqlx::query_as::<_, GitToken>(
        "SELECT id, name, created_at, expires_at, last_used_at
         FROM git_tokens
         WHERE user_id = $1 AND name NOT LIKE $2
         ORDER BY created_at DESC",
    )
    .bind(user_id)
    .bind(format!("{}-%", LFS_EPHEMERAL_NAME))
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
