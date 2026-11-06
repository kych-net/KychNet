use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Content, Invite, SiteSettings, User};

/// 使用 CSPRNG 生成 16 字节、Base32(Crockford) 编码的邀请码(约 80 bit 熵)。
pub fn generate_invite_code() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    base32::encode(base32::Alphabet::Crockford, &bytes)
}

pub fn parse_expiry(spec: &str) -> Result<Option<DateTime<Utc>>> {
    if spec == "never" || spec.is_empty() {
        return Ok(None);
    }
    let (num, unit) = spec.split_at(spec.len().saturating_sub(1));
    let value: i64 = num.parse().context("有效期格式应为 7d / 24h")?;
    let duration = match unit {
        "d" => Duration::days(value),
        "h" => Duration::hours(value),
        _ => bail!("有效期单位只支持 d 或 h"),
    };
    Ok(Some(Utc::now() + duration))
}

pub async fn create_invite(
    pool: &PgPool,
    created_by: Option<Uuid>,
    max_uses: i32,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Invite> {
    let code = generate_invite_code();
    let invite = sqlx::query_as::<_, Invite>(
        "INSERT INTO invites (code, created_by, max_uses, expires_at)
         VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(&code)
    .bind(created_by)
    .bind(max_uses)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;
    Ok(invite)
}

pub async fn list_invites(pool: &PgPool) -> Result<Vec<Invite>> {
    Ok(
        sqlx::query_as::<_, Invite>("SELECT * FROM invites ORDER BY created_at DESC")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn revoke_invite(pool: &PgPool, code: &str) -> Result<()> {
    let result = sqlx::query("UPDATE invites SET revoked_at = now() WHERE code = $1")
        .bind(code)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("邀请码不存在");
    }
    Ok(())
}

/// 校验并占用一次邀请码。整体在事务内完成,并发下不会超发。
pub async fn consume_invite(pool: &PgPool, code: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    let invite = sqlx::query_as::<_, Invite>("SELECT * FROM invites WHERE code = $1 FOR UPDATE")
        .bind(code)
        .fetch_optional(&mut *tx)
        .await?
        .context("邀请码无效")?;

    if invite.revoked_at.is_some() {
        bail!("邀请码已作废");
    }
    if let Some(expires_at) = invite.expires_at {
        if expires_at < Utc::now() {
            bail!("邀请码已过期");
        }
    }
    if invite.used_count >= invite.max_uses {
        bail!("邀请码已用尽");
    }

    sqlx::query("UPDATE invites SET used_count = used_count + 1 WHERE code = $1")
        .bind(code)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn create_user(
    pool: &PgPool,
    username: &str,
    display_name: &str,
    is_admin: bool,
) -> Result<User> {
    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, username, display_name, is_admin)
         VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(username)
    .bind(display_name)
    .bind(is_admin)
    .fetch_one(pool)
    .await?;
    Ok(user)
}

pub async fn update_display_name(pool: &PgPool, id: Uuid, display_name: &str) -> Result<User> {
    let user = sqlx::query_as::<_, User>(
        "UPDATE users SET display_name = $2 WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(display_name)
    .fetch_one(pool)
    .await?;
    Ok(user)
}

pub async fn get_settings(pool: &PgPool) -> Result<SiteSettings> {
    let row = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT value FROM site_settings WHERE key = 'site'",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row
        .and_then(|(value,)| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

pub async fn save_settings(pool: &PgPool, settings: &SiteSettings) -> Result<()> {
    let value = serde_json::to_value(settings)?;
    sqlx::query(
        "INSERT INTO site_settings (key, value) VALUES ('site', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_user_by_username(pool: &PgPool, username: &str) -> Result<Option<User>> {
    Ok(
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = $1")
            .bind(username)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn find_user_by_id(pool: &PgPool, id: Uuid) -> Result<Option<User>> {
    Ok(
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn list_users(pool: &PgPool) -> Result<Vec<User>> {
    Ok(
        sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at DESC")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn set_user_disabled(pool: &PgPool, id: Uuid, disabled: bool) -> Result<()> {
    let result = sqlx::query("UPDATE users SET disabled = $2 WHERE id = $1")
        .bind(id)
        .bind(disabled)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("用户不存在");
    }
    Ok(())
}

pub async fn delete_passkeys(pool: &PgPool, user_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM passkey_credentials WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_contents(pool: &PgPool) -> Result<Vec<Content>> {
    Ok(
        sqlx::query_as::<_, Content>("SELECT * FROM contents ORDER BY updated_at DESC")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn list_published(pool: &PgPool) -> Result<Vec<Content>> {
    Ok(sqlx::query_as::<_, Content>(
        "SELECT * FROM contents WHERE published = TRUE ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?)
}

pub async fn upsert_content(
    pool: &PgPool,
    slug: &str,
    title: &str,
    format: &str,
    body: &str,
    published: bool,
) -> Result<Content> {
    let content = sqlx::query_as::<_, Content>(
        "INSERT INTO contents (id, slug, title, format, body, published)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (slug) DO UPDATE
         SET title = EXCLUDED.title, format = EXCLUDED.format,
             body = EXCLUDED.body, published = EXCLUDED.published, updated_at = now()
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(slug)
    .bind(title)
    .bind(format)
    .bind(body)
    .bind(published)
    .fetch_one(pool)
    .await?;
    Ok(content)
}
