use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use uuid::Uuid;

use crate::git::lfs;
use crate::git::object::{
    commit_parents, encode_commit, ensure_text, identity_name, is_text, Oid,
};
use crate::git::store;
use crate::git::tree;
use crate::models::User;

/// 项目在内容站的固定命名空间。
pub const PROJECT_NAMESPACE: &str = "项目";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub default_branch: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileContent {
    pub path: String,
    pub oid: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteResult {
    pub commit: String,
    pub path: String,
    pub oid: String,
}

/// 项目名允许任意非特殊 UTF-8：只排除路径分隔符、URL 保留字符和控制字符。
pub fn valid_project_slug(slug: &str) -> bool {
    let trimmed = slug.trim();
    if trimmed.is_empty() || trimmed.len() > 100 || trimmed != slug {
        return false;
    }
    if slug == "." || slug == ".." {
        return false;
    }
    !slug.chars().any(|ch| {
        ch.is_control()
            || ch.is_whitespace()
            || matches!(ch, '/' | '\\' | '?' | '#' | '%' | '\0')
    })
}

pub fn valid_branch(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.len() > 200
        || trimmed != name
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name == "@"
        || name.split('/').any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        return false;
    }
    !name.chars().any(|ch| {
        ch.is_control()
            || ch.is_whitespace()
            || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\0')
    })
}

/// 仓库内文件路径：相对路径、无空段、无 `..`、无 `.git` 段。
pub fn valid_file_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed != path || path.len() > 512 {
        return false;
    }
    if path.starts_with('/') || path.ends_with('/') {
        return false;
    }
    path.split('/').all(|segment| {
        !segment.is_empty()
            && segment.len() <= 200
            && segment != "."
            && segment != ".."
            && segment != ".git"
            && !segment
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '\\' | '?' | '#' | '%' | '\0'))
    })
}

fn output_stem(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => path,
    }
}

fn parent_dir(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => "",
    }
}

fn is_root_index(path: &str) -> bool {
    !path.contains('/') && output_stem(path) == "index"
}

/// 同一目录下 `a.md` 与 `a.html` 会输出到同一个页面，必须提前拒绝。
fn ensure_no_output_conflict(path: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let dir = parent_dir(path);
    let stem = output_stem(path);
    for existing in files.keys() {
        if existing == path {
            continue;
        }
        if parent_dir(existing) == dir && output_stem(existing) == stem {
            bail!("{path} 与 {existing} 会输出到同一个路径，请先删除或改名其中一个");
        }
    }
    Ok(())
}

fn identity(user: &User) -> String {
    format!("{} <noreply@kych.net>", identity_name(&user.username))
}

fn message_or(default: &str, message: Option<&str>) -> String {
    match message {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => default.to_string(),
    }
}

async fn lock_project(conn: &mut PgConnection, project_id: Uuid) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(project_id.to_string())
        .fetch_one(conn)
        .await?;
    Ok(())
}

async fn commit_snapshot(
    conn: &mut PgConnection,
    project_id: Uuid,
    user: &User,
    branch: &str,
    parent: Option<Oid>,
    tree_oid: Oid,
    message: &str,
) -> Result<Oid> {
    let now = Utc::now();
    let parents: Vec<Oid> = parent.into_iter().collect();
    let data = encode_commit(tree_oid, &parents, &identity(user), message, now.timestamp());
    let commit =
        store::put_object(conn, project_id, &crate::git::object::Object::commit(data)).await?;
    store::set_ref(conn, project_id, &format!("refs/heads/{branch}"), commit).await?;
    store::insert_commit_index(
        conn,
        project_id,
        commit,
        parent,
        None,
        tree_oid,
        Some(user.id),
        &user.username,
        message,
        now,
    )
    .await?;
    Ok(commit)
}

#[allow(clippy::too_many_arguments)]
async fn commit_files(
    conn: &mut PgConnection,
    project_id: Uuid,
    user: &User,
    branch: &str,
    parent: Option<Oid>,
    tree_oid: Oid,
    message: &str,
    path: &str,
    change: &str,
    old_path: Option<&str>,
    blob: Option<Oid>,
) -> Result<Oid> {
    let commit =
        commit_snapshot(conn, project_id, user, branch, parent, tree_oid, message).await?;
    store::insert_commit_path(
        conn, project_id, commit, path, change, old_path, blob,
    )
    .await?;
    Ok(commit)
}

pub async fn create_project(
    pool: &PgPool,
    user: &User,
    slug: &str,
    title: &str,
    description: &str,
) -> Result<Project> {
    if !valid_project_slug(slug) {
        bail!("项目名只能包含非特殊 UTF-8 字符，不能含 / \\ ? # % 或空白");
    }
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        bail!("项目标题需为 1-200 个字符");
    }

    let mut tx = pool.begin().await?;
    let pattern = format!("{PROJECT_NAMESPACE}/{slug}/%");
    let occupied: Option<(String,)> =
        sqlx::query_as("SELECT slug FROM contents WHERE slug LIKE $1 LIMIT 1")
            .bind(pattern)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((slug,)) = occupied {
        bail!("/{slug} 已被普通内容占用，项目无法使用这个路径");
    }

    let project = sqlx::query_as::<_, Project>(
        "INSERT INTO projects (id, slug, title, description, created_by)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(slug)
    .bind(title)
    .bind(description.trim())
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| match &error {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            anyhow::anyhow!("项目名已被占用")
        }
        _ => error.into(),
    })?;

    let mut files = BTreeMap::new();
    let index = format!(
        "---\ntitle: {title}\n---\n\n{}\n",
        description.trim()
    );
    files.insert("index.md".to_string(), index.into_bytes());
    let tree_oid = tree::build_from_files(&mut tx, project.id, &files).await?;
    commit_files(
        &mut tx,
        project.id,
        user,
        &project.default_branch,
        None,
        tree_oid,
        "初始化项目",
        "index.md",
        "add",
        None,
        files.get("index.md").map(|body| crate::git::object::hash_object(
            crate::git::object::ObjectKind::Blob,
            body,
        )),
    )
    .await?;

    tx.commit().await?;
    Ok(project)
}

pub async fn list_projects(pool: &PgPool) -> Result<Vec<Project>> {
    Ok(sqlx::query_as::<_, Project>(
        "SELECT * FROM projects WHERE archived_at IS NULL ORDER BY updated_at DESC",
    )
    .fetch_all(pool)
    .await?)
}

pub async fn get_project(pool: &PgPool, id: Uuid) -> Result<Option<Project>> {
    Ok(sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn find_project_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Project>> {
    Ok(
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE slug = $1")
            .bind(slug)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn update_project(
    pool: &PgPool,
    id: Uuid,
    title: &str,
    description: &str,
) -> Result<Project> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        bail!("项目标题需为 1-200 个字符");
    }
    sqlx::query_as::<_, Project>(
        "UPDATE projects SET title = $2, description = $3, updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(title)
    .bind(description.trim())
    .fetch_optional(pool)
    .await?
    .context("项目不存在")
}

pub async fn archive_project(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE projects SET archived_at = now(), updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_branches(pool: &PgPool, project_id: Uuid) -> Result<Vec<String>> {
    let mut conn = pool.acquire().await?;
    let refs = store::list_refs(&mut conn, project_id).await?;
    Ok(refs
        .into_iter()
        .filter_map(|(name, _)| name.strip_prefix("refs/heads/").map(str::to_string))
        .collect())
}

pub async fn read_file(
    pool: &PgPool,
    project_id: Uuid,
    branch: &str,
    path: &str,
) -> Result<Option<FileContent>> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    let mut conn = pool.acquire().await?;
    let Some(commit) = store::resolve_branch(&mut conn, project_id, branch).await? else {
        bail!("分支不存在: {branch}");
    };
    let Some((oid, data)) = tree::find_file(&mut conn, project_id, commit, path).await? else {
        return Ok(None);
    };
    if !crate::git::object::is_text(&data) {
        bail!("该文件不是 UTF-8 文本；二进制请用 GET /git/<项目>/file 读取（LFS 已支持）");
    }
    Ok(Some(FileContent {
        path: path.to_string(),
        oid: oid.to_hex(),
        content: String::from_utf8(data)?,
    }))
}

pub async fn list_dir(
    pool: &PgPool,
    project_id: Uuid,
    branch: &str,
    dir: &str,
) -> Result<Vec<tree::DirItem>> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !dir.is_empty() && !valid_file_path(dir) {
        bail!("非法目录路径");
    }
    let mut conn = pool.acquire().await?;
    let Some(commit) = store::resolve_branch(&mut conn, project_id, branch).await? else {
        bail!("分支不存在: {branch}");
    };
    tree::list_dir(&mut conn, project_id, commit, dir).await
}

pub async fn write_file(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    path: &str,
    content: &str,
    message: Option<&str>,
    base_oid: Option<&str>,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    ensure_text(content.as_bytes())?;

    get_project(pool, project_id).await?.context("项目不存在")?;
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;

    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let files = match parent {
        Some(commit) => tree::read_files(&mut tx, project_id, commit).await?,
        None => BTreeMap::new(),
    };
    let existing_oid = files.get(path).map(|data| {
        crate::git::object::hash_object(crate::git::object::ObjectKind::Blob, data)
    });
    store::check_old_oid(existing_oid, base_oid)?;
    ensure_no_output_conflict(path, &files)?;

    let blob = tree::write_blob(&mut tx, project_id, content.as_bytes()).await?;
    let root = match parent {
        Some(commit) => Some(tree::read_commit_tree(&mut tx, project_id, commit).await?),
        None => None,
    };
    let tree_oid = tree::update_path(&mut tx, project_id, root, path, Some(blob)).await?;
    let change = if existing_oid.is_some() { "modify" } else { "add" };
    let default_message = format!(
        "{} {path}",
        if existing_oid.is_some() { "更新" } else { "新增" }
    );
    let message = message_or(&default_message, message);
    let commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        parent,
        tree_oid,
        &message,
        path,
        change,
        None,
        Some(blob),
    )
    .await?;

    tx.commit().await?;
    Ok(WriteResult {
        commit: commit.to_hex(),
        path: path.to_string(),
        oid: blob.to_hex(),
    })
}

/// 写文件树（原始字节）：UTF-8 文本直接进 blob，二进制走站内已实现的
/// LFS——对象落盘并登记，仓库里只存 pointer。与管理端 `write_file` 共用
/// 同一套项目锁、乐观锁、输出冲突校验与 commit 记录。
#[allow(clippy::too_many_arguments)]
pub async fn write_file_bytes(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    path: &str,
    data: &[u8],
    message: Option<&str>,
    base_oid: Option<&str>,
    lfs_root: &Path,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    get_project(pool, project_id).await?.context("项目不存在")?;

    // LFS 落盘在事务外完成：它自己要用 pool 取连接，避免和 lock_project
    // 持有的事务互相等连接。万一事务回滚，留下的孤儿对象由 gc 清理。
    let blob_bytes = if is_text(data) {
        data.to_vec()
    } else {
        let oid = hex::encode(Sha256::digest(data));
        lfs::store(pool, lfs_root, project_id, &oid, data).await?;
        format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
            data.len()
        )
        .into_bytes()
    };

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let files = match parent {
        Some(commit) => tree::read_files(&mut tx, project_id, commit).await?,
        None => BTreeMap::new(),
    };
    let existing_oid = files.get(path).map(|old| {
        crate::git::object::hash_object(crate::git::object::ObjectKind::Blob, old)
    });
    store::check_old_oid(existing_oid, base_oid)?;
    ensure_no_output_conflict(path, &files)?;

    let blob = tree::write_blob(&mut tx, project_id, &blob_bytes).await?;
    let root = match parent {
        Some(commit) => Some(tree::read_commit_tree(&mut tx, project_id, commit).await?),
        None => None,
    };
    let tree_oid = tree::update_path(&mut tx, project_id, root, path, Some(blob)).await?;
    let change = if existing_oid.is_some() { "modify" } else { "add" };
    let default_message = format!(
        "{} {path}",
        if existing_oid.is_some() { "更新" } else { "新增" }
    );
    let message = message_or(&default_message, message);
    let commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        parent,
        tree_oid,
        &message,
        path,
        change,
        None,
        Some(blob),
    )
    .await?;

    tx.commit().await?;
    Ok(WriteResult {
        commit: commit.to_hex(),
        path: path.to_string(),
        oid: blob.to_hex(),
    })
}

pub async fn delete_file(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    path: &str,
    message: Option<&str>,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    if is_root_index(path) {
        bail!("项目主页 index 不能删除，只能编辑");
    }

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let Some(commit) = parent else {
        bail!("分支不存在: {branch}");
    };
    let root = tree::read_commit_tree(&mut tx, project_id, commit).await?;
    let Some((old_oid, _)) = tree::find_file(&mut tx, project_id, commit, path).await? else {
        bail!("文件不存在: {path}");
    };
    let tree_oid = tree::update_path(&mut tx, project_id, Some(root), path, None).await?;
    let message = message_or(&format!("删除 {path}"), message);
    let new_commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        Some(commit),
        tree_oid,
        &message,
        path,
        "delete",
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(WriteResult {
        commit: new_commit.to_hex(),
        path: path.to_string(),
        oid: old_oid.to_hex(),
    })
}

pub async fn move_file(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    from: &str,
    to: &str,
    message: Option<&str>,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(from) || !valid_file_path(to) {
        bail!("非法文件路径");
    }
    if from == to {
        bail!("源路径与目标路径相同");
    }
    if is_root_index(from) {
        bail!("项目主页 index 不能改名");
    }

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let Some(commit) = parent else {
        bail!("分支不存在: {branch}");
    };
    let root = tree::read_commit_tree(&mut tx, project_id, commit).await?;
    let Some((blob, data)) = tree::find_file(&mut tx, project_id, commit, from).await? else {
        bail!("文件不存在: {from}");
    };
    let files = tree::read_files(&mut tx, project_id, commit).await?;
    if files.contains_key(to) {
        bail!("目标路径已存在: {to}");
    }
    ensure_no_output_conflict(to, &files)?;
    ensure_text(&data)?;

    let mut current = Some(root);
    current = Some(tree::update_path(&mut tx, project_id, current, from, None).await?);
    current = Some(tree::update_path(&mut tx, project_id, current, to, Some(blob)).await?);
    let tree_oid = current.expect("至少执行过一次 update_path");
    let message = message_or(&format!("移动 {from} 到 {to}"), message);
    let new_commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        Some(commit),
        tree_oid,
        &message,
        to,
        "rename",
        Some(from),
        Some(blob),
    )
    .await?;
    tx.commit().await?;
    Ok(WriteResult {
        commit: new_commit.to_hex(),
        path: to.to_string(),
        oid: blob.to_hex(),
    })
}

/// 提交历史列表项。OID 统一用十六进制文本返回，前端无需处理 bytea。
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CommitSummary {
    pub oid: String,
    pub parent_oid: Option<String>,
    pub second_parent_oid: Option<String>,
    pub tree_oid: String,
    pub author_name: String,
    pub message: String,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitDetail {
    #[serde(flatten)]
    pub commit: CommitSummary,
    pub files: Vec<crate::collab::FileDiff>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevertResult {
    pub commit: String,
}

const COMMIT_SELECT: &str = "SELECT encode(oid, 'hex') AS oid,
        encode(parent_oid, 'hex') AS parent_oid,
        encode(second_parent_oid, 'hex') AS second_parent_oid,
        encode(tree_oid, 'hex') AS tree_oid,
        author_name, message, committed_at
     FROM git_commits";

/// 从引用出发遍历全部父提交，用于按分支过滤历史。
async fn walk_commit_oids(
    conn: &mut PgConnection,
    project_id: Uuid,
    tip: Oid,
) -> Result<Vec<Oid>> {
    let mut seen = HashSet::new();
    let mut stack = vec![tip];
    let mut out = Vec::new();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if seen.len() > 5000 {
            bail!("提交历史过深，暂时无法列出");
        }
        let object = store::require_object(conn, project_id, oid).await?;
        if object.kind != crate::git::object::ObjectKind::Commit {
            continue;
        }
        out.push(oid);
        stack.extend(commit_parents(&object.data)?);
    }
    Ok(out)
}

pub async fn list_commits(
    pool: &PgPool,
    project_id: Uuid,
    reference: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<CommitSummary>> {
    let limit = limit.clamp(1, 200);
    let offset = offset.max(0);
    let mut conn = pool.acquire().await?;

    match reference {
        Some(reference) => {
            if !valid_branch(reference) {
                bail!("非法分支名");
            }
            let Some(tip) = store::resolve_branch(&mut conn, project_id, reference).await? else {
                return Ok(Vec::new());
            };
            let oids = walk_commit_oids(&mut conn, project_id, tip).await?;
            let bytes: Vec<Vec<u8>> = oids.iter().map(|oid| oid.as_bytes().to_vec()).collect();
            let rows = sqlx::query_as::<_, CommitSummary>(&format!(
                "{COMMIT_SELECT} WHERE project_id = $1 AND oid = ANY($2)
                 ORDER BY committed_at DESC"
            ))
            .bind(project_id)
            .bind(&bytes)
            .fetch_all(&mut *conn)
            .await?;
            Ok(rows
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect())
        }
        None => Ok(sqlx::query_as::<_, CommitSummary>(&format!(
            "{COMMIT_SELECT} WHERE project_id = $1
             ORDER BY committed_at DESC LIMIT $2 OFFSET $3"
        ))
        .bind(project_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *conn)
        .await?),
    }
}

/// 提交详情：与第一个父提交做差异，根提交按空树处理。
pub async fn commit_detail(
    pool: &PgPool,
    project_id: Uuid,
    oid_hex: &str,
) -> Result<Option<CommitDetail>> {
    let oid = Oid::from_hex(oid_hex)?;
    let mut conn = pool.acquire().await?;
    let row = sqlx::query_as::<_, CommitSummary>(&format!(
        "{COMMIT_SELECT} WHERE project_id = $1 AND oid = $2"
    ))
    .bind(project_id)
    .bind(&oid.as_bytes()[..])
    .fetch_optional(&mut *conn)
    .await?;
    let Some(commit) = row else {
        return Ok(None);
    };
    let base = commit
        .parent_oid
        .as_deref()
        .map(Oid::from_hex)
        .transpose()?;
    let files = crate::collab::diff_commits(&mut conn, project_id, base, oid).await?;
    Ok(Some(CommitDetail { commit, files }))
}

/// 还原某个提交：把该提交相对父提交的改动反向应用到分支顶部。
pub async fn revert_commit(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    oid_hex: &str,
    message: Option<&str>,
) -> Result<RevertResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    let commit = Oid::from_hex(oid_hex)?;

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;

    let tip = store::resolve_branch(&mut tx, project_id, branch)
        .await?
        .context("分支不存在")?;
    // 只有当前分支历史里的提交才允许还原。
    if crate::collab::merge_base(&mut tx, project_id, tip, commit).await? != Some(commit) {
        bail!("只能还原当前分支历史中的提交");
    }

    let object = store::require_object(&mut tx, project_id, commit).await?;
    if object.kind != crate::git::object::ObjectKind::Commit {
        bail!("目标不是提交对象");
    }
    let parent = commit_parents(&object.data)?.first().copied();
    let commit_files_map = tree::read_files(&mut tx, project_id, commit).await?;
    let parent_files = match parent {
        Some(parent) => tree::read_files(&mut tx, project_id, parent).await?,
        None => BTreeMap::new(),
    };
    let mut current = tree::read_files(&mut tx, project_id, tip).await?;

    let mut paths: BTreeSet<&String> = BTreeSet::new();
    paths.extend(commit_files_map.keys());
    paths.extend(parent_files.keys());

    let mut changes: Vec<(String, String, Option<Oid>)> = Vec::new();
    for path in paths {
        let before = commit_files_map.get(path);
        let after = parent_files.get(path);
        if before == after {
            continue;
        }
        match after {
            Some(data) => {
                let existed = current.contains_key(path);
                let blob = tree::write_blob(&mut tx, project_id, data).await?;
                current.insert(path.clone(), data.clone());
                changes.push((
                    path.clone(),
                    if existed { "modify" } else { "add" }.to_string(),
                    Some(blob),
                ));
            }
            None => {
                current.remove(path);
                changes.push((path.clone(), "delete".to_string(), None));
            }
        }
    }
    if changes.is_empty() {
        bail!("该提交没有可还原的改动");
    }
    for path in current.keys() {
        ensure_no_output_conflict(path, &current)?;
    }

    let root = tree::build_from_files(&mut tx, project_id, &current).await?;
    let short: String = oid_hex.chars().take(8).collect();
    let default_message = format!("还原提交 {short}");
    let message = message_or(&default_message, message);
    let new_commit =
        commit_snapshot(&mut tx, project_id, user, branch, Some(tip), root, &message).await?;
    for (path, change, blob) in &changes {
        store::insert_commit_path(
            &mut tx,
            project_id,
            new_commit,
            path,
            change,
            None,
            *blob,
        )
        .await?;
    }
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(RevertResult {
        commit: new_commit.to_hex(),
    })
}

/// 从指定分支（或默认分支）拉出新分支。
pub async fn create_branch(
    pool: &PgPool,
    project_id: Uuid,
    name: &str,
    from: &str,
) -> Result<()> {
    if !valid_branch(name) || !valid_branch(from) {
        bail!("非法分支名");
    }
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    if store::resolve_branch(&mut tx, project_id, name).await?.is_some() {
        bail!("分支已存在: {name}");
    }
    let tip = store::resolve_branch(&mut tx, project_id, from)
        .await?
        .with_context(|| format!("来源分支不存在: {from}"))?;
    store::set_ref(&mut tx, project_id, &format!("refs/heads/{name}"), tip).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete_branch(pool: &PgPool, project_id: Uuid, name: &str) -> Result<()> {
    if !valid_branch(name) {
        bail!("非法分支名");
    }
    let project = get_project(pool, project_id).await?.context("项目不存在")?;
    if name == project.default_branch {
        bail!("不能删除默认分支");
    }
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    if store::resolve_branch(&mut tx, project_id, name).await?.is_none() {
        bail!("分支不存在: {name}");
    }
    store::delete_ref(&mut tx, project_id, &format!("refs/heads/{name}")).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// 项目默认分支的完整快照，供静态构建使用。
pub struct ProjectSnapshot {
    pub project: Project,
    pub files: BTreeMap<String, Vec<u8>>,
    pub commit: Oid,
    pub committed_at: DateTime<Utc>,
}

pub async fn snapshots(pool: &PgPool) -> Result<Vec<ProjectSnapshot>> {
    let projects = list_projects(pool).await?;
    let mut conn = pool.acquire().await?;
    let mut snapshots = Vec::new();
    for project in projects {
        let result: Result<Option<ProjectSnapshot>> = async {
            let Some(commit) =
                store::resolve_branch(&mut conn, project.id, &project.default_branch).await?
            else {
                return Ok(None);
            };
            let files = tree::read_files(&mut conn, project.id, commit).await?;
            let committed_at = store::commit_time(&mut conn, project.id, commit)
                .await?
                .unwrap_or(project.updated_at);
            Ok(Some(ProjectSnapshot {
                project: project.clone(),
                files,
                commit,
                committed_at,
            }))
        }
        .await;
        match result {
            Ok(Some(snapshot)) => snapshots.push(snapshot),
            Ok(None) => {}
            // 单个坏仓库不能永久毒化整站构建；保留其它项目继续构建。
            Err(error) => tracing::warn!(
                project = %project.slug,
                %error,
                "项目快照读取失败，跳过该项目"
            ),
        }
    }
    Ok(snapshots)
}

/// 供后续 receive-pack 使用：读取提交的父链。
pub async fn commit_parent_chain(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
) -> Result<Vec<Oid>> {
    let object = store::require_object(conn, project_id, commit).await?;
    commit_parents(&object.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_slug_allows_unicode_but_rejects_separators() {
        assert!(valid_project_slug("kych-net"));
        assert!(valid_project_slug("我的项目"));
        assert!(valid_project_slug("hello🚀"));
        assert!(!valid_project_slug("../x"));
        assert!(!valid_project_slug("a/b"));
        assert!(!valid_project_slug("a b"));
        assert!(!valid_project_slug(""));
    }

    #[test]
    fn file_path_rejects_escape_and_git_dir() {
        assert!(valid_file_path("index.md"));
        assert!(valid_file_path("docs/说明.md"));
        assert!(!valid_file_path("/index.md"));
        assert!(!valid_file_path("a/../b"));
        assert!(!valid_file_path(".git/config"));
        assert!(!valid_file_path("a//b"));
    }

    #[test]
    fn branch_name_rejects_git_special_forms() {
        assert!(valid_branch("main"));
        assert!(valid_branch("pr/12"));
        assert!(!valid_branch("a..b"));
        assert!(!valid_branch("a b"));
        assert!(!valid_branch("/main"));
        assert!(!valid_branch("main@{1}"));
        assert!(!valid_branch("@"));
        assert!(!valid_branch(".hidden"));
        assert!(!valid_branch("feature/.hidden"));
        assert!(!valid_branch("feature/main.lock"));
    }
}
