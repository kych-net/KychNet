//! Git SSH 公钥的解析、登记与认证。
//!
//! 公钥以 OpenSSH 单行格式存储，指纹（SHA256:...）唯一：SSH 服务端认证时
//! 拿客户端出示的公钥算指纹反查用户，与 HTTP 侧的 Git 令牌（`token.rs`）
//! 完全平行，但不共享凭据——SSH key 泄露不影响令牌，反之亦然。

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use ssh_key::{HashAlg, PublicKey as SshPublicKey};
use uuid::Uuid;

use crate::models::User;

/// Git SSH 公钥（不含私钥，库里只有公钥文本与指纹）。
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GitSshKey {
    pub id: Uuid,
    pub name: String,
    pub fingerprint: String,
    pub public_key: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

fn hash_key(key: &SshPublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// 解析并规范化 OpenSSH 公钥。
///
/// 接受 `algo base64 [comment]` 单行或多行（ authorized_keys 拼贴整段）输入，
/// 返回 `(规范化单行文本, 指纹)`。规范化后同一把钥匙重复登记会命中唯一索引。
pub fn normalize(public_key: &str) -> Result<(String, String)> {
    let text = public_key.trim();
    if text.is_empty() || text.contains('\n') {
        bail!("请粘贴一行完整的 OpenSSH 公钥（ssh-ed25519 AAAA… [注释]）");
    }
    let key = SshPublicKey::from_openssh(text).map_err(|_| anyhow::anyhow!("不是合法的 OpenSSH 公钥"))?;
    let fingerprint = hash_key(&key);
    // 重新编码，去掉多余空白与任意格式差异；注释保留，便于辨认。
    let normalized = key.to_openssh().map_err(|error| anyhow::anyhow!("公钥编码失败: {error}"))?;
    Ok((normalized.trim_end().to_string(), fingerprint))
}

pub async fn create(pool: &PgPool, user_id: Uuid, name: &str, public_key: &str) -> Result<GitSshKey> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        bail!("公钥名称需为 1-100 个字符");
    }
    let (normalized, fingerprint) = normalize(public_key)?;
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM git_ssh_keys WHERE fingerprint = $1 AND user_id <> $2")
            .bind(&fingerprint)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    if existing.is_some() {
        bail!("这把公钥已被其他账号登记");
    }
    let item = sqlx::query_as::<_, GitSshKey>(
        "INSERT INTO git_ssh_keys (id, user_id, name, public_key, fingerprint)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, name, fingerprint, public_key, created_at, last_used_at",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(name)
    .bind(&normalized)
    .bind(&fingerprint)
    .fetch_one(pool)
    .await?;
    Ok(item)
}

pub async fn list(pool: &PgPool, user_id: Uuid) -> Result<Vec<GitSshKey>> {
    Ok(sqlx::query_as::<_, GitSshKey>(
        "SELECT id, name, fingerprint, public_key, created_at, last_used_at
         FROM git_ssh_keys WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub async fn revoke(pool: &PgPool, user_id: Uuid, id: Uuid) -> Result<()> {
    let result = sqlx::query("DELETE FROM git_ssh_keys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("公钥不存在");
    }
    Ok(())
}

/// SSH 公钥认证：按指纹反查用户。
///
/// 与 HTTP 侧语义一致：禁用账号一律拒绝；命中时顺带刷新 last_used_at。
pub async fn authenticate(pool: &PgPool, key: &SshPublicKey) -> Result<Option<User>> {
    let fingerprint = hash_key(key);
    let row = sqlx::query(
        "SELECT u.id, u.username, u.disabled, u.is_admin, u.created_at
         FROM git_ssh_keys k
         JOIN users u ON u.id = k.user_id
         WHERE k.fingerprint = $1",
    )
    .bind(&fingerprint)
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
    sqlx::query("UPDATE git_ssh_keys SET last_used_at = now() WHERE fingerprint = $1")
        .bind(&fingerprint)
        .execute(pool)
        .await?;
    Ok(Some(user))
}
