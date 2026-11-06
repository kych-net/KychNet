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

/// 校验 slug 是否可作为静态页面的文件名与 URL 路径段。
///
/// 规则：**允许任意 Unicode 字母与数字**（中文、日文、西里尔字母等）外加 `-` 与 `_`，
/// 除此之外一律视为特殊字符并拒绝。被拒绝的包括：路径分隔符 `/` `\`、点号 `.`
/// （`.` / `..` 会造成路径穿越）、空白与控制字符、以及 URL 里有特殊含义的
/// `?` `#` `%` `:` `&` `=` `<` `>` `|` `*` `"` 等。这样既能用中文 slug，
/// 又保证 slug 拼进 `dist/posts/<slug>.html` 时不会越出目录或生成非法 URL。
///
/// 长度按 UTF-8 字节计，上限 180：中文一个字占 3 字节，拼上 `.html` / `.pdf`
/// 后缀后仍落在常见文件系统 255 字节的文件名限制内。
///
/// 这条规则必须在「写入 API」与「构建」两处共用：如果只在构建时校验，
/// 一条含 `/` 或 `..` 的脏数据就会让整站构建永久失败。
///
/// 注意：`char::is_alphanumeric()` 不接受组合音标（如 `e` + U+0301），
/// 因此带变音符号的文字请使用 NFC 预组合形式（如 `é`）。
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 180
        && slug
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// 邀请码可用性校验，抽成纯函数供独立调用与注册事务共用。
fn check_invite(invite: &Invite) -> Result<()> {
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
    Ok(())
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

/// 只读校验邀请码：让注册在开始前就能报出准确原因，同时不占用名额。
pub async fn peek_invite(pool: &PgPool, code: &str) -> Result<Invite> {
    let invite = sqlx::query_as::<_, Invite>("SELECT * FROM invites WHERE code = $1")
        .bind(code)
        .fetch_optional(pool)
        .await?
        .context("邀请码无效")?;
    check_invite(&invite)?;
    Ok(invite)
}

/// 注册的最后一步：在一个事务里占用邀请码、创建用户、保存通行密钥。
///
/// 等到凭证校验通过才动手，是因为 WebAuthn 仪式随时可能被用户取消：先扣码
/// 再建号的话，一次取消就永久烧掉一个邀请名额，还会留下没有凭证、谁也登不
/// 进去的孤立用户。用户名唯一性交给数据库兜底，撞名时整体回滚并给出提示。
/// `user_id` 由调用方提前生成，必须与 WebAuthn 挑战里的 user handle 完全一致。
pub async fn complete_registration(
    pool: &PgPool,
    user_id: Uuid,
    code: &str,
    username: &str,
    display_name: &str,
    credential_id: &str,
    passkey: &serde_json::Value,
) -> Result<User> {
    let mut tx = pool.begin().await?;

    let invite = sqlx::query_as::<_, Invite>("SELECT * FROM invites WHERE code = $1 FOR UPDATE")
        .bind(code)
        .fetch_optional(&mut *tx)
        .await?
        .context("邀请码无效")?;
    check_invite(&invite)?;

    let user = match sqlx::query_as::<_, User>(
        "INSERT INTO users (id, username, display_name, is_admin)
         VALUES ($1, $2, $3, FALSE) RETURNING *",
    )
    .bind(user_id)
    .bind(username)
    .bind(display_name)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(user) => user,
        // 23505 = unique_violation：两个并行注册抢同一个用户名，后到的失败。
        Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
            bail!("用户名已被占用");
        }
        Err(e) => return Err(e.into()),
    };

    sqlx::query("UPDATE invites SET used_count = used_count + 1 WHERE code = $1")
        .bind(code)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO passkey_credentials (id, user_id, credential_id, passkey)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(credential_id)
    .bind(passkey)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(user)
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

pub async fn update_avatar(pool: &PgPool, id: Uuid, svg: &str) -> Result<User> {
    let user =
        sqlx::query_as::<_, User>("UPDATE users SET avatar_svg = $2 WHERE id = $1 RETURNING *")
            .bind(id)
            .bind(svg)
            .fetch_one(pool)
            .await?;
    Ok(user)
}

pub async fn get_avatar(pool: &PgPool, id: Uuid) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT avatar_svg FROM users WHERE id = $1 AND avatar_svg IS NOT NULL",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?
        .flatten(),
    )
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

/// 后台「禁用/启用账号」的安全版本。
///
/// 裸的 `set_user_disabled` 会让管理员把自己关在门外，也会让最后一个管理员
/// 被禁用后全站再也进不去（只有 CLI 能救）。两种情况在同一事务里锁住相关行
/// 再判断，避免两个管理员同时点「禁用」把彼此关掉。
pub async fn set_user_disabled_guarded(
    pool: &PgPool,
    actor_id: Uuid,
    target_id: Uuid,
    disabled: bool,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    let target_is_admin =
        sqlx::query_as::<_, (bool,)>("SELECT is_admin FROM users WHERE id = $1 FOR UPDATE")
            .bind(target_id)
            .fetch_optional(&mut *tx)
            .await?
            .context("用户不存在")?
            .0;

    if disabled {
        if target_id == actor_id {
            bail!("不能禁用自己的账号");
        }
        if target_is_admin {
            // 锁定所有可用管理员再计数：并发操作下两个管理员互相禁用时，
            // 后提交的那个会看到 count 已经变成 1 而失败。
            let active = sqlx::query_as::<_, (Uuid,)>(
                "SELECT id FROM users WHERE is_admin AND NOT disabled FOR UPDATE",
            )
            .fetch_all(&mut *tx)
            .await?;
            if active.len() <= 1 {
                bail!("这是最后一个可用管理员，禁用后无人能进后台");
            }
        }
    }

    sqlx::query("UPDATE users SET disabled = $2 WHERE id = $1")
        .bind(target_id)
        .bind(disabled)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete_passkeys(pool: &PgPool, user_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM passkey_credentials WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 清理过期的认证流程，返回删除的行数。
///
/// `auth_flows` 过去只在流程被正常走完时删除，而 `login_start` 是匿名可调的：
/// 每次调用都会插入一行，放弃登录的人越多、试探登录的脚本越欢，表就单调增长。
/// 现在由 `serve` 里的后台任务定期调用。
pub async fn prune_auth_flows(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM auth_flows WHERE expires_at < now()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
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

#[cfg(test)]
mod tests {
    use super::valid_slug;

    #[test]
    fn allows_unicode_letters_and_digits() {
        for slug in [
            "hello",
            "a-b_c123",
            "你好世界",
            "日本語_2026",
            "Русский",
            "café-notes",
            "２０２６",
        ] {
            assert!(valid_slug(slug), "应当允许: {slug:?}");
        }
    }

    #[test]
    fn rejects_special_characters() {
        for slug in [
            "",
            "a/b",
            "a\\b",
            "..",
            "a.b",
            "a b",
            "a?b=1",
            "a#b",
            "a%20b",
            "a:b",
            "a<b>",
            "a|b",
            "a*b",
            "a\"b",
            "a&b",
            "a\nb",
            "a\tb",
            "a\0b",
            "😀",
            "a/b/../../etc",
        ] {
            assert!(!valid_slug(slug), "应当拒绝: {slug:?}");
        }
    }

    #[test]
    fn enforces_byte_length_limit() {
        assert!(valid_slug(&"a".repeat(180)));
        assert!(!valid_slug(&"a".repeat(181)));
        assert!(valid_slug(&"中".repeat(60))); // 正好 180 字节
        assert!(!valid_slug(&"中".repeat(61))); // 183 字节,超限
    }
}
