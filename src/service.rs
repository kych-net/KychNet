use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Content, ContentFormat, Invite, SiteSettings, User};

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
    // 按 char 取末位而不是按字节 split_at：多字节字符（如 `7天`）结尾时
    // 字节切分会因不在 char 边界直接 panic。
    let mut chars = spec.chars();
    let unit = chars.next_back().context("有效期不能为空")?;
    let value: i64 = chars
        .as_str()
        .trim()
        .parse()
        .context("有效期格式应为 7d / 24h")?;
    let duration = match unit {
        'd' => Duration::days(value),
        'h' => Duration::hours(value),
        _ => bail!("有效期单位只支持 d 或 h"),
    };
    Ok(Some(Utc::now() + duration))
}

/// 网站主页在数据库里的固定路径。它可编辑，但不可删除。
pub const HOME_SLUG: &str = "index.html";

const MAX_PATH_BYTES: usize = 512;
const MAX_SEGMENT_BYTES: usize = 200;

/// 路径段只允许 Unicode 字母/数字、`-` 和 `_`。
fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_SEGMENT_BYTES
        && segment
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// 校验数据库里已经归一化的输出路径，例如 `index.html`、`xef/index.html`。
pub fn valid_content_slug(slug: &str) -> bool {
    let Some(stem) = slug.strip_suffix(".html") else {
        return false;
    };
    if slug.len() > MAX_PATH_BYTES {
        return false;
    }
    let segments: Vec<&str> = stem.split('/').collect();
    if segments.is_empty() || segments.iter().any(|s| !valid_segment(s)) {
        return false;
    }
    // `/api` 与 `/api/...` 是后端保留路径，不能作为内容页。
    !segments[0].eq_ignore_ascii_case("api")
}

/// 把用户输入的路径归一化成数据库里的输出路径，并从扩展名推断格式。
///
/// 规则：
/// - 允许 `/xef/`（目录主页 → `xef/index.html`）；
/// - 允许 `/reff/93df/l.html`（页面）；
/// - 允许 `/eoe/index.md`、`/eoe/index.typ`，但最终都存成 `.html`；
/// - 同一路径只能存在一条记录，因此 `.html/.md/.typ` 天然互斥；
/// - 扩展名和请求格式不一致时直接报错，避免静默改格式；
/// - `/api` 与 `/api/...` 保留，拒绝写入。
pub fn normalize_content_path(
    input: &str,
    requested: ContentFormat,
) -> Result<(String, ContentFormat)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("路径不能为空");
    }
    if trimmed
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("路径不能包含空白或控制字符");
    }

    let mut raw = trimmed.trim_start_matches('/').to_string();
    if trimmed.ends_with('/') {
        raw.push_str("index");
    }
    if raw.is_empty() {
        raw.push_str("index");
    }

    let mut parts: Vec<String> = raw.split('/').map(str::to_string).collect();
    if parts.iter().any(|s| s.is_empty() || s == "." || s == "..") {
        bail!("路径不能包含空段、`.` 或 `..`");
    }

    let last = parts.last_mut().expect("parts 至少有一个元素");
    let lower = last.to_ascii_lowercase();
    let (base, inferred) = match lower.rsplit_once('.') {
        Some((base, ext)) => match ContentFormat::from_extension(ext) {
            Some(format) => (base.to_string(), Some(format)),
            // 未知扩展名保留下来，交给 valid_segment 拒绝。
            None => (last.clone(), None),
        },
        None => (last.clone(), None),
    };

    if base.is_empty() {
        bail!("路径缺少文件名");
    }
    let last_display = last.clone();
    *last = base;

    let slug = format!("{}.html", parts.join("/"));
    // 主页固定叫 index.html，但允许用 Markdown/Typst 编辑它；
    // 其他路径的 .html 扩展名仍然强制对应 HTML 格式。
    if let Some(inferred) = inferred {
        if inferred != requested && slug != HOME_SLUG {
            bail!(
                "扩展名与格式不一致：{} 应对应 {}",
                last_display,
                inferred.as_str()
            );
        }
    }
    if slug.len() > MAX_PATH_BYTES {
        bail!("路径不能超过 {MAX_PATH_BYTES} 字节");
    }
    if !valid_content_slug(&slug) {
        bail!("路径只能包含 Unicode 字母/数字、-、_ 和 /，且不能使用 /api 保留路径");
    }
    let format = if slug == HOME_SLUG {
        requested
    } else {
        inferred.unwrap_or(requested)
    };
    Ok((slug, format))
}

/// 输出路径对应的源文件路径，用于编辑器和目录树展示。
pub fn source_path(slug: &str, format: ContentFormat) -> String {
    match slug.strip_suffix(".html") {
        Some(stem) => format!("{stem}{}", format.extension()),
        None => slug.to_string(),
    }
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
        "INSERT INTO users (id, username, is_admin)
         VALUES ($1, $2, FALSE) RETURNING *",
    )
    .bind(user_id)
    .bind(username)
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

pub async fn create_user(pool: &PgPool, username: &str, is_admin: bool) -> Result<User> {
    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, username, is_admin)
         VALUES ($1, $2, $3) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(username)
    .bind(is_admin)
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

pub async fn find_content_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Content>> {
    Ok(
        sqlx::query_as::<_, Content>("SELECT * FROM contents WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// 按 id 更新内容，允许修改路径。路径唯一冲突时返回可读错误。
pub async fn update_content(
    pool: &PgPool,
    id: Uuid,
    slug: &str,
    title: &str,
    format: &str,
    body: &str,
    published: bool,
) -> Result<Content> {
    let result = sqlx::query_as::<_, Content>(
        "UPDATE contents
         SET slug = $2, title = $3, format = $4, body = $5, published = $6, updated_at = now()
         WHERE id = $1
         RETURNING *",
    )
    .bind(id)
    .bind(slug)
    .bind(title)
    .bind(format)
    .bind(body)
    .bind(published)
    .fetch_one(pool)
    .await;

    match result {
        Ok(content) => Ok(content),
        Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
            bail!("路径已被占用")
        }
        Err(sqlx::Error::RowNotFound) => bail!("内容不存在"),
        Err(error) => Err(error.into()),
    }
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
    use super::{
        normalize_content_path, parse_expiry, source_path, valid_content_slug, HOME_SLUG,
    };
    use crate::models::ContentFormat;

    #[test]
    fn parses_expiry_specs() {
        assert!(parse_expiry("7d").is_ok());
        assert!(parse_expiry("24h").is_ok());
        assert!(parse_expiry("never").unwrap().is_none());
        assert!(parse_expiry("").unwrap().is_none());
    }

    #[test]
    fn rejects_multibyte_unit_without_panicking() {
        // 旧实现按字节切，`7天` 会因不在 char 边界直接 panic。
        assert!(parse_expiry("7天").is_err());
        assert!(parse_expiry("天").is_err());
    }

    #[test]
    fn normalizes_directory_and_file_paths() {
        assert_eq!(
            normalize_content_path("/xef/", ContentFormat::Markdown).unwrap(),
            ("xef/index.html".to_string(), ContentFormat::Markdown)
        );
        assert_eq!(
            normalize_content_path("/reff/93df/l.html", ContentFormat::Html).unwrap(),
            ("reff/93df/l.html".to_string(), ContentFormat::Html)
        );
        assert_eq!(
            normalize_content_path("/eoe/index.md", ContentFormat::Markdown).unwrap(),
            ("eoe/index.html".to_string(), ContentFormat::Markdown)
        );
        assert_eq!(
            normalize_content_path("/eoe/", ContentFormat::Typst).unwrap(),
            ("eoe/index.html".to_string(), ContentFormat::Typst)
        );
        assert_eq!(
            normalize_content_path("你好/世界.html", ContentFormat::Html).unwrap(),
            ("你好/世界.html".to_string(), ContentFormat::Html)
        );
        assert_eq!(
            normalize_content_path("/", ContentFormat::Markdown).unwrap(),
            (HOME_SLUG.to_string(), ContentFormat::Markdown)
        );
        assert_eq!(
            normalize_content_path("index.html", ContentFormat::Markdown).unwrap(),
            (HOME_SLUG.to_string(), ContentFormat::Markdown)
        );
        assert_eq!(
            normalize_content_path("index.md", ContentFormat::Markdown).unwrap(),
            (HOME_SLUG.to_string(), ContentFormat::Markdown)
        );
        assert_eq!(
            normalize_content_path("/wiki.wj", ContentFormat::Wikidot).unwrap(),
            ("wiki.html".to_string(), ContentFormat::Wikidot)
        );
    }

    #[test]
    fn rejects_extension_format_mismatch() {
        assert!(normalize_content_path("/eoe/index.md", ContentFormat::Html).is_err());
        assert!(normalize_content_path("/eoe/index.typ", ContentFormat::Markdown).is_err());
        assert!(normalize_content_path("/eoe/index.html", ContentFormat::Typst).is_err());
    }

    #[test]
    fn rejects_protected_and_unsafe_paths() {
        for (path, format) in [
            ("/api", ContentFormat::Html),
            ("/api/index.html", ContentFormat::Html),
            ("/API/foo", ContentFormat::Html),
            ("/a//b", ContentFormat::Html),
            ("/a/../b", ContentFormat::Html),
            ("/a/./b", ContentFormat::Html),
            ("/a b", ContentFormat::Html),
            ("/a.txt", ContentFormat::Html),
            ("/a\\b", ContentFormat::Html),
        ] {
            assert!(
                normalize_content_path(path, format).is_err(),
                "应当拒绝: {path:?}"
            );
        }
        // apiary 不是保留路径。
        assert!(normalize_content_path("/apiary/x", ContentFormat::Html).is_ok());
    }

    #[test]
    fn allows_unicode_letters_and_digits() {
        for slug in [
            "hello.html",
            "a-b_c123.html",
            "你好世界.html",
            "日本語_2026.html",
            "Русский.html",
            "café-notes.html",
            "２０２６.html",
            "xef/index.html",
            "reff/93df/l.html",
            "apiary.html",
        ] {
            assert!(valid_content_slug(slug), "应当允许: {slug:?}");
        }
    }

    #[test]
    fn rejects_invalid_stored_slugs() {
        for slug in [
            "",
            ".html",
            "index",
            "a/b",
            "a/b/",
            "a//b.html",
            "a/../b.html",
            "a.b.html",
            "a b.html",
            "a?b.html",
            "a#b.html",
            "api.html",
            "API/index.html",
            "😀.html",
        ] {
            assert!(!valid_content_slug(slug), "应当拒绝: {slug:?}");
        }
    }

    #[test]
    fn enforces_segment_and_path_length() {
        assert!(valid_content_slug(&format!("{}.html", "a".repeat(200))));
        assert!(!valid_content_slug(&format!("{}.html", "a".repeat(201))));
        assert!(valid_content_slug(&format!("{}/index.html", "a".repeat(200))));
        assert!(!valid_content_slug(&format!("{}/index.html", "a".repeat(400))));
    }

    #[test]
    fn derives_source_path_for_editor() {
        assert_eq!(source_path("index.html", ContentFormat::Markdown), "index.md");
        assert_eq!(source_path("eoe/index.html", ContentFormat::Typst), "eoe/index.typ");
        assert_eq!(source_path("reff/93df/l.html", ContentFormat::Html), "reff/93df/l.html");
        assert_eq!(source_path("wiki.html", ContentFormat::Wikidot), "wiki.wj");
    }
}
