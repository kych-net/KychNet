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
    // `/api`、`/css` 与 `/管理` 是保留命名空间：前两者是后端接口与样式，
    // `/管理` 被 nginx 优先匹配到管理端 SPA，内容写进去会被遮蔽。
    !segments[0].eq_ignore_ascii_case("api")
        && !segments[0].eq_ignore_ascii_case("css")
        && segments[0] != "管理"
}

/// 校验原始 SVG 路径（数据库里的输出路径，例如 `media/logo.svg`）。
pub fn valid_svg_slug(slug: &str) -> bool {
    let Some(stem) = slug.strip_suffix(".svg") else {
        return false;
    };
    if slug.len() > MAX_PATH_BYTES {
        return false;
    }
    let segments: Vec<&str> = stem.split('/').collect();
    !segments.is_empty()
        && segments.iter().all(|segment| valid_segment(segment))
        && !segments[0].eq_ignore_ascii_case("api")
        && !segments[0].eq_ignore_ascii_case("css")
}

/// 校验 `/css/...css` 路径（数据库里的输出路径，例如 `css/theme.css`）。
pub fn valid_style_slug(slug: &str) -> bool {
    let Some(stem) = slug.strip_suffix(".css") else {
        return false;
    };
    if slug.len() > MAX_PATH_BYTES {
        return false;
    }
    let segments: Vec<&str> = stem.split('/').collect();
    segments.len() >= 2
        && segments[0] == "css"
        && segments.iter().all(|segment| valid_segment(segment))
}

/// 网站根目录（`slug` 不含 `/`）只允许 `index.html` 一个主页文件。
///
/// 页面类路径归一化后都会变成 `index.html`，但 SVG 这类原始资源可以带着自己
/// 的扩展名落到根目录（例如 `index.svg`），从而把别的 index 带进网站根目录。
/// 其它目录（`xef/index.md`、`media/index.svg` …）一律不限制。
pub fn is_foreign_root_index(slug: &str) -> bool {
    if slug.contains('/') || slug == HOME_SLUG {
        return false;
    }
    let lower = slug.to_ascii_lowercase();
    lower == "index" || lower.starts_with("index.")
}

fn ensure_root_index_html_only(slug: &str) -> Result<()> {
    if is_foreign_root_index(slug) {
        bail!("网站根目录只能有 index.html，不能创建 {slug}；请放到子目录下");
    }
    Ok(())
}

/// 升级时的一次性清理：删掉网站根目录里非 `index.html` 的 index 内容记录。
///
/// 早期版本允许把 `index.md` / `index.typ` / `index.svg` 直接存成独立内容，
/// 它们会和网站主页争夺根目录。归一化规则上线后不会再产生这类记录，但存量
/// 数据需要清掉。`content_revisions` 有 `ON DELETE CASCADE`，版本历史会一并
/// 删除。幂等：没有匹配记录时不产生任何写入，返回空列表；子目录不受影响。
pub async fn cleanup_legacy_root_indexes(pool: &PgPool) -> Result<Vec<String>> {
    let candidates: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, slug FROM contents WHERE slug NOT LIKE '%/%'")
            .fetch_all(pool)
            .await?;
    let mut removed = Vec::new();
    for (id, slug) in candidates {
        if !is_foreign_root_index(&slug) {
            continue;
        }
        sqlx::query("DELETE FROM contents WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;
        removed.push(slug);
    }
    Ok(removed)
}

/// 把用户输入的路径归一化成数据库里的输出路径，并从扩展名推断格式。
///
/// 规则：
/// - 目录主页必须显式写文件名，例如 `/xef/index`；
/// - 允许 `/reff/93df/l.html`（页面）；
/// - 允许 `/eoe/index.md`、`/eoe/index.typ`，但最终都存成 `.html`；
/// - 同一路径只能存在一条记录，因此 `.html/.md/.typ` 天然互斥；
/// - 扩展名和请求格式不一致时直接报错，避免静默改格式；
/// - 网站根目录只允许 `index.html`，其它格式的 index 一律拒绝；其它目录不限制；
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

    if trimmed.ends_with('/') {
        bail!("目录路径请显式写出文件名，例如 /xef/index");
    }

    // /项目/ 是项目仓库的命名空间，普通内容不能写进去，避免同一路径有两个来源。
    let first_segment = trimmed
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    if first_segment == crate::project::PROJECT_NAMESPACE {
        bail!("/项目/ 下的内容由项目仓库管理，请从「项目」入口编辑");
    }

    if requested == ContentFormat::Svg {
        let raw = trimmed.trim_start_matches('/');
        let mut parts: Vec<String> = raw.split('/').map(str::to_string).collect();
        if parts.iter().any(|s| s.is_empty() || s == "." || s == "..") {
            bail!("路径不能包含空段、`.` 或 `..`");
        }
        let last = parts.last_mut().expect("parts 至少有一个元素");
        let lower = last.to_ascii_lowercase();
        let Some((stem, ext)) = lower.rsplit_once('.') else {
            bail!("SVG 文件必须使用 .svg 扩展名");
        };
        if ext != "svg" {
            bail!("SVG 文件必须使用 .svg 扩展名");
        }
        if stem.is_empty() {
            bail!("路径缺少文件名");
        }
        *last = stem.to_string();
        let slug = format!("{}.svg", parts.join("/"));
        if !valid_svg_slug(&slug) {
            bail!("SVG 路径只能包含 Unicode 字母/数字、-、_ 和 /，且不能使用保留路径");
        }
        ensure_root_index_html_only(&slug)?;
        return Ok((slug, ContentFormat::Svg));
    }

    if requested == ContentFormat::Css {
        let raw = trimmed.trim_start_matches('/');
        let mut parts: Vec<String> = raw.split('/').map(str::to_string).collect();
        if parts.len() < 2 || !parts[0].eq_ignore_ascii_case("css") {
            bail!("CSS 路径必须放在 /css/ 下，例如 /css/theme");
        }
        parts[0] = "css".to_string();
        let last = parts.last_mut().expect("parts 至少有两个元素");
        if let Some((stem, ext)) = last.rsplit_once('.') {
            if !ext.eq_ignore_ascii_case("css") {
                bail!("CSS 文件必须使用 .css 扩展名");
            }
            *last = stem.to_string();
        }
        let slug = format!("{}.css", parts.join("/"));
        if !valid_style_slug(&slug) {
            bail!("CSS 路径只能包含 Unicode 字母/数字、-、_ 和 /");
        }
        ensure_root_index_html_only(&slug)?;
        return Ok((slug, ContentFormat::Css));
    }

    let raw = trimmed.trim_start_matches('/').to_string();

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
    if inferred == Some(ContentFormat::Svg) {
        if requested != ContentFormat::Svg {
            bail!("扩展名与格式不一致：{} 应对应 svg", last);
        }
        let mut svg_parts = parts.clone();
        if let Some(last) = svg_parts.last_mut() {
            *last = base.clone();
        }
        let slug = format!("{}.svg", svg_parts.join("/"));
        if !valid_svg_slug(&slug) {
            bail!("SVG 路径只能包含 Unicode 字母/数字、-、_ 和 /，且不能使用保留路径");
        }
        ensure_root_index_html_only(&slug)?;
        return Ok((slug, ContentFormat::Svg));
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
    ensure_root_index_html_only(&slug)?;
    Ok((slug, format))
}

/// 输出路径对应的源文件路径，用于编辑器和目录树展示。
pub fn source_path(slug: &str, format: ContentFormat) -> String {
    if format == ContentFormat::Css {
        return slug.to_string();
    }
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
///
/// 正文或格式实际变化时在同一事务里落一条版本行（精简版历史），
/// 只改路径 / 发布状态不产生新版本。
#[allow(clippy::too_many_arguments)]
pub async fn update_content(
    pool: &PgPool,
    id: Uuid,
    slug: &str,
    format: &str,
    body: &str,
    published: bool,
    author_id: Uuid,
    message: &str,
) -> Result<Content> {
    let mut tx = pool.begin().await?;
    // 与回滚共用一把行锁：锁住后再读旧正文、算增量、落版本，
    // 并发保存与回滚在行级串行，版本链不会交叉。
    let old: Option<(String, String)> =
        sqlx::query_as("SELECT body, format FROM contents WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((old_body, old_format)) = old else {
        bail!("内容不存在");
    };
    let content_changed = old_body != body || old_format != format;
    let new_rev_seq = if content_changed {
        Some(
            crate::history::record_revision(
                &mut tx, id, slug, format, &old_body, body, author_id, message,
            )
            .await?,
        )
    } else {
        None
    };
    let result = if let Some(rev_seq) = new_rev_seq {
        sqlx::query_as::<_, Content>(
            "UPDATE contents
             SET slug = $2, format = $3, body = $4, published = $5,
                 rev_seq = $6, updated_at = now()
             WHERE id = $1
             RETURNING *",
        )
        .bind(id)
        .bind(slug)
        .bind(format)
        .bind(body)
        .bind(published)
        .bind(rev_seq)
        .fetch_one(&mut *tx)
        .await
    } else {
        sqlx::query_as::<_, Content>(
            "UPDATE contents
             SET slug = $2, format = $3, body = $4, published = $5, updated_at = now()
             WHERE id = $1
             RETURNING *",
        )
        .bind(id)
        .bind(slug)
        .bind(format)
        .bind(body)
        .bind(published)
        .fetch_one(&mut *tx)
        .await
    };
    tx.commit().await?;

    match result {
        Ok(content) => Ok(content),
        Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
            bail!("路径已被占用")
        }
        Err(sqlx::Error::RowNotFound) => bail!("内容不存在"),
        Err(error) => Err(error.into()),
    }
}

/// 按 slug 新建或更新内容。已存在同名记录时行为同 `update_content`
/// （含落版本），否则插入并落第一条全文基线版本。
#[allow(clippy::too_many_arguments)]
pub async fn upsert_content(
    pool: &PgPool,
    slug: &str,
    format: &str,
    body: &str,
    published: bool,
    author_id: Uuid,
    message: &str,
) -> Result<Content> {
    let mut tx = pool.begin().await?;
    let existing: Option<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, body, format FROM contents WHERE slug = $1 FOR UPDATE")
            .bind(slug)
            .fetch_optional(&mut *tx)
            .await?;
    let content = if let Some((id, old_body, old_format)) = existing {
        let content_changed = old_body != body || old_format != format;
        let new_rev_seq = if content_changed {
            Some(
                crate::history::record_revision(
                    &mut tx, id, slug, format, &old_body, body, author_id, message,
                )
                .await?,
            )
        } else {
            None
        };
        if let Some(rev_seq) = new_rev_seq {
            sqlx::query_as::<_, Content>(
                "UPDATE contents
                 SET format = $2, body = $3, published = $4, rev_seq = $5, updated_at = now()
                 WHERE id = $1
                 RETURNING *",
            )
            .bind(id)
            .bind(format)
            .bind(body)
            .bind(published)
            .bind(rev_seq)
            .fetch_one(&mut *tx)
            .await?
        } else {
            sqlx::query_as::<_, Content>(
                "UPDATE contents
                 SET format = $2, body = $3, published = $4, updated_at = now()
                 WHERE id = $1
                 RETURNING *",
            )
            .bind(id)
            .bind(format)
            .bind(body)
            .bind(published)
            .fetch_one(&mut *tx)
            .await?
        }
    } else {
        let content = sqlx::query_as::<_, Content>(
            "INSERT INTO contents (id, slug, format, body, published, rev_seq)
             VALUES ($1, $2, $3, $4, $5, 1)
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(slug)
        .bind(format)
        .bind(body)
        .bind(published)
        .fetch_one(&mut *tx)
        .await?;
        // 新内容的第一版就是全文基线。
        crate::history::record_full_baseline(
            &mut tx,
            content.id,
            slug,
            format,
            body,
            author_id,
            message,
        )
        .await?;
        content
    };
    tx.commit().await?;
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
            normalize_content_path("/xef/index", ContentFormat::Markdown).unwrap(),
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
            normalize_content_path("/eoe/index", ContentFormat::Typst).unwrap(),
            ("eoe/index.html".to_string(), ContentFormat::Typst)
        );
        assert_eq!(
            normalize_content_path("你好/世界.html", ContentFormat::Html).unwrap(),
            ("你好/世界.html".to_string(), ContentFormat::Html)
        );
        assert!(normalize_content_path("/", ContentFormat::Markdown).is_err());
        assert!(normalize_content_path("/xef/", ContentFormat::Markdown).is_err());
        assert!(normalize_content_path("/eoe/index/", ContentFormat::Typst).is_err());
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
        assert_eq!(
            normalize_content_path("/css/theme", ContentFormat::Css).unwrap(),
            ("css/theme.css".to_string(), ContentFormat::Css)
        );
        assert_eq!(
            normalize_content_path("/css/theme.css", ContentFormat::Css).unwrap(),
            ("css/theme.css".to_string(), ContentFormat::Css)
        );
        assert!(normalize_content_path("/css/", ContentFormat::Css).is_err());
        assert!(normalize_content_path("/css/theme.md", ContentFormat::Css).is_err());
        assert!(normalize_content_path("/other/theme.css", ContentFormat::Css).is_err());
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
            "css/index.html",
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
        assert_eq!(source_path("css/theme.css", ContentFormat::Css), "css/theme.css");
    }
    #[test]
    fn normalizes_svg_raw_files() {
        assert_eq!(
            normalize_content_path("/media/logo.svg", ContentFormat::Svg).unwrap(),
            ("media/logo.svg".to_string(), ContentFormat::Svg)
        );
        assert!(normalize_content_path("/media/logo", ContentFormat::Svg).is_err());
        assert!(normalize_content_path("/media/logo.svg", ContentFormat::Markdown).is_err());
        assert_eq!(
            source_path("media/logo.svg", ContentFormat::Svg),
            "media/logo.svg"
        );
        assert!(super::valid_svg_slug("media/logo.svg"));
        assert!(!super::valid_svg_slug("api/logo.svg"));
    }

    #[test]
    fn website_root_only_allows_index_html() {
        // 网站根目录：其它格式的 index 一律拒绝，包括大小写变体。
        for path in ["/index.svg", "index.svg", "/INDEX.SVG"] {
            assert!(
                normalize_content_path(path, ContentFormat::Svg).is_err(),
                "{path} 不应写入网站根目录"
            );
        }
        // 主页仍可用任意格式编辑：源文件显示 index.md，记录本身是 index.html。
        assert_eq!(
            normalize_content_path("/index.md", ContentFormat::Markdown).unwrap(),
            (HOME_SLUG.to_string(), ContentFormat::Markdown)
        );
        assert_eq!(
            normalize_content_path("/index.typ", ContentFormat::Typst).unwrap(),
            (HOME_SLUG.to_string(), ContentFormat::Typst)
        );
        assert_eq!(
            normalize_content_path("/index.html", ContentFormat::Html).unwrap(),
            (HOME_SLUG.to_string(), ContentFormat::Html)
        );
        // 其它目录一律不限制。
        for path in ["/media/index.svg", "/xef/index.md", "/docs/INDEX.SVG"] {
            let format = if path.to_ascii_lowercase().ends_with(".svg") {
                ContentFormat::Svg
            } else {
                ContentFormat::Markdown
            };
            assert!(
                normalize_content_path(path, format).is_ok(),
                "{path} 不应受根目录规则影响"
            );
        }
    }

    #[test]
    fn detects_legacy_root_index_slugs() {
        for slug in [
            "index.md",
            "index.typ",
            "index.wj",
            "index.svg",
            "index",
            "INDEX.MD",
            "Index.svg",
        ] {
            assert!(super::is_foreign_root_index(slug), "{slug} 应被自动清理");
        }
        assert!(!super::is_foreign_root_index("index.html"));
        // 其它目录、以及只是名字里带 index 的普通文件都不动。
        for slug in [
            "xef/index.md",
            "media/index.svg",
            "项目/xef/index.typ",
            "index2.md",
            "reindex.svg",
        ] {
            assert!(
                !super::is_foreign_root_index(slug),
                "{slug} 不应被自动清理"
            );
        }
    }
}
