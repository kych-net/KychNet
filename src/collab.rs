//! 项目协作：Pull Request 与项目讨论。
//!
//! PR 只做快进合并：目标分支必须是来源分支的祖先，否则要求先把目标分支
//! 合并进来源分支，避免在服务端做可能产生冲突的三方合并。所有写操作都在
//! 项目级 advisory lock 下串行执行，与 `project.rs` 的文件编辑共用同一把锁。

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use uuid::Uuid;

use crate::git::object::{TreeEntry, Oid};
use crate::git::store;
use crate::git::tree;
use crate::models::User;
use crate::project::valid_branch;

/// 与 `git/http.rs` 保持一致的提交遍历上限，防止过深历史拖垮请求。
const MAX_COMMIT_WALK: usize = 5000;

const PR_SELECT: &str = "SELECT pr.id, pr.project_id, pr.number, pr.title, pr.body,
        pr.source_branch, pr.target_branch, pr.status, pr.created_at, pr.updated_at,
        pr.merged_at, COALESCE(u.username, '已注销用户') AS author_name
     FROM pull_requests pr
     LEFT JOIN users u ON u.id = pr.author_id";

const DISCUSSION_SELECT: &str = "SELECT d.id, d.project_id, d.title, d.body,
        COALESCE(u.username, '已注销用户') AS author_name,
        d.created_at, d.closed_at,
        (SELECT COUNT(*) FROM discussion_comments c WHERE c.discussion_id = d.id) AS comment_count
     FROM project_discussions d
     LEFT JOIN users u ON u.id = d.author_id";

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct PullRequest {
    pub id: Uuid,
    pub project_id: Uuid,
    pub number: i32,
    pub title: String,
    pub body: String,
    pub source_branch: String,
    pub target_branch: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub merged_at: Option<DateTime<Utc>>,
    pub author_name: String,
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct PrComment {
    pub id: Uuid,
    pub author_name: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FileDiff {
    pub path: String,
    /// `add` / `modify` / `delete`
    pub status: String,
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrDetail {
    #[serde(flatten)]
    pub pr: PullRequest,
    pub files: Vec<FileDiff>,
    pub mergeable: bool,
    pub comments: Vec<PrComment>,
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Discussion {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub body: String,
    pub author_name: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub comment_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct DiscussionComment {
    pub id: Uuid,
    pub author_name: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscussionDetail {
    #[serde(flatten)]
    pub discussion: Discussion,
    pub comments: Vec<DiscussionComment>,
}

async fn lock_project(conn: &mut PgConnection, project_id: Uuid) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(project_id.to_string())
        .fetch_one(conn)
        .await?;
    Ok(())
}

fn validate_title(title: &str, what: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        bail!("{what}需为 1-200 个字符");
    }
    Ok(title.to_string())
}

fn validate_body(body: &str, what: &str, max: usize) -> Result<String> {
    let body = body.trim();
    if body.is_empty() {
        bail!("{what}不能为空");
    }
    if body.chars().count() > max {
        bail!("{what}不能超过 {max} 个字符");
    }
    Ok(body.to_string())
}

async fn is_ancestor(
    conn: &mut PgConnection,
    project_id: Uuid,
    ancestor: Oid,
    descendant: Oid,
) -> Result<bool> {
    let mut seen = HashSet::new();
    let mut stack = vec![descendant];
    while let Some(oid) = stack.pop() {
        if oid == ancestor {
            return Ok(true);
        }
        if seen.len() >= MAX_COMMIT_WALK {
            bail!("提交历史过深，暂时无法校验快进关系");
        }
        if !seen.insert(oid) {
            continue;
        }
        let object = store::require_object(conn, project_id, oid).await?;
        if object.kind != crate::git::object::ObjectKind::Commit {
            continue;
        }
        for parent in crate::git::object::commit_parents(&object.data)? {
            stack.push(parent);
        }
    }
    Ok(false)
}

/// 收集某棵树下的全部文件路径与 blob OID。
async fn blob_map(
    conn: &mut PgConnection,
    project_id: Uuid,
    root: Oid,
) -> Result<BTreeMap<String, Oid>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![(String::new(), root)];
    while let Some((prefix, tree_oid)) = stack.pop() {
        let tree = tree::read_tree(conn, project_id, tree_oid).await?;
        for (name, entry) in tree.entries {
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            match entry {
                TreeEntry::Blob(oid) => {
                    files.insert(path, oid);
                }
                TreeEntry::Dir(oid) => stack.push((path, oid)),
            }
        }
    }
    Ok(files)
}

async fn diff_trees(
    conn: &mut PgConnection,
    project_id: Uuid,
    target_commit: Oid,
    source_commit: Oid,
) -> Result<Vec<FileDiff>> {
    let old_root = tree::read_commit_tree(conn, project_id, target_commit).await?;
    let new_root = tree::read_commit_tree(conn, project_id, source_commit).await?;
    let old = blob_map(conn, project_id, old_root).await?;
    let new = blob_map(conn, project_id, new_root).await?;

    let mut paths: BTreeSet<&String> = BTreeSet::new();
    paths.extend(old.keys());
    paths.extend(new.keys());

    let mut diffs = Vec::new();
    for path in paths {
        let old_oid = old.get(path).copied();
        let new_oid = new.get(path).copied();
        if old_oid == new_oid {
            continue;
        }
        let status = match (old_oid, new_oid) {
            (None, Some(_)) => "add",
            (Some(_), None) => "delete",
            _ => "modify",
        };
        diffs.push(FileDiff {
            path: path.clone(),
            status: status.to_string(),
            old_oid: old_oid.map(|oid| oid.to_hex()),
            new_oid: new_oid.map(|oid| oid.to_hex()),
        });
    }
    Ok(diffs)
}

async fn fetch_pr(
    conn: &mut PgConnection,
    project_id: Uuid,
    number: i32,
) -> Result<Option<PullRequest>> {
    Ok(sqlx::query_as::<_, PullRequest>(&format!(
        "{PR_SELECT} WHERE pr.project_id = $1 AND pr.number = $2"
    ))
    .bind(project_id)
    .bind(number)
    .fetch_optional(conn)
    .await?)
}

async fn list_pr_comments(
    conn: &mut PgConnection,
    pr_id: Uuid,
) -> Result<Vec<PrComment>> {
    Ok(sqlx::query_as::<_, PrComment>(
        "SELECT c.id, COALESCE(u.username, '已注销用户') AS author_name,
                c.body, c.created_at
         FROM pr_comments c
         LEFT JOIN users u ON u.id = c.author_id
         WHERE c.pr_id = $1
         ORDER BY c.created_at ASC",
    )
    .bind(pr_id)
    .fetch_all(conn)
    .await?)
}

pub async fn list_prs(pool: &PgPool, project_id: Uuid) -> Result<Vec<PullRequest>> {
    Ok(sqlx::query_as::<_, PullRequest>(&format!(
        "{PR_SELECT} WHERE pr.project_id = $1 ORDER BY pr.number DESC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn create_pr(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    title: &str,
    body: &str,
    source_branch: &str,
    target_branch: &str,
) -> Result<PullRequest> {
    let title = validate_title(title, "标题")?;
    if body.chars().count() > 20000 {
        bail!("说明不能超过 20000 个字符");
    }
    if !valid_branch(source_branch) || !valid_branch(target_branch) {
        bail!("非法分支名");
    }
    if source_branch == target_branch {
        bail!("来源分支与目标分支不能相同");
    }

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;

    if store::resolve_branch(&mut tx, project_id, source_branch)
        .await?
        .is_none()
    {
        bail!("来源分支不存在: {source_branch}");
    }
    if store::resolve_branch(&mut tx, project_id, target_branch)
        .await?
        .is_none()
    {
        bail!("目标分支不存在: {target_branch}");
    }

    let duplicate: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM pull_requests
         WHERE project_id = $1 AND source_branch = $2 AND target_branch = $3
           AND status = 'open'
         LIMIT 1",
    )
    .bind(project_id)
    .bind(source_branch)
    .bind(target_branch)
    .fetch_optional(&mut *tx)
    .await?;
    if duplicate.is_some() {
        bail!("已存在相同来源分支与目标分支的开放 PR");
    }

    let (number,): (i32,) = sqlx::query_as(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM pull_requests WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO pull_requests
            (id, project_id, number, title, body, source_branch, target_branch, author_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(project_id)
    .bind(number)
    .bind(&title)
    .bind(body.trim())
    .bind(source_branch)
    .bind(target_branch)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    let mut conn = pool.acquire().await?;
    fetch_pr(&mut conn, project_id, number)
        .await?
        .context("PR 创建后无法读取")
}

pub async fn pr_detail(
    pool: &PgPool,
    project_id: Uuid,
    number: i32,
) -> Result<Option<PrDetail>> {
    let mut conn = pool.acquire().await?;
    let Some(pr) = fetch_pr(&mut conn, project_id, number).await? else {
        return Ok(None);
    };
    let comments = list_pr_comments(&mut conn, pr.id).await?;

    let source = store::resolve_branch(&mut conn, project_id, &pr.source_branch).await?;
    let target = store::resolve_branch(&mut conn, project_id, &pr.target_branch).await?;
    let (files, mergeable) = match (source, target) {
        (Some(source_oid), Some(target_oid)) => {
            let files = diff_trees(&mut conn, project_id, target_oid, source_oid).await?;
            let mergeable = source_oid == target_oid
                || is_ancestor(&mut conn, project_id, target_oid, source_oid).await?;
            (files, mergeable)
        }
        _ => (Vec::new(), false),
    };

    Ok(Some(PrDetail {
        pr,
        files,
        mergeable,
        comments,
    }))
}

pub async fn add_pr_comment(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    number: i32,
    body: &str,
) -> Result<PrComment> {
    let body = validate_body(body, "评论内容", 10000)?;
    let mut conn = pool.acquire().await?;
    let pr = fetch_pr(&mut conn, project_id, number)
        .await?
        .context("PR 不存在")?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO pr_comments (id, pr_id, author_id, body) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(pr.id)
        .bind(user.id)
        .bind(&body)
        .execute(&mut *conn)
        .await?;
    Ok(sqlx::query_as::<_, PrComment>(
        "SELECT c.id, COALESCE(u.username, '已注销用户') AS author_name,
                c.body, c.created_at
         FROM pr_comments c
         LEFT JOIN users u ON u.id = c.author_id
         WHERE c.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?)
}

pub async fn merge_pr(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    number: i32,
) -> Result<PullRequest> {
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;

    let pr = fetch_pr(&mut tx, project_id, number)
        .await?
        .context("PR 不存在")?;
    if pr.status != "open" {
        bail!("只有开放的 PR 可以合并");
    }

    let source = store::resolve_branch(&mut tx, project_id, &pr.source_branch)
        .await?
        .context("来源分支不存在")?;
    let target = store::resolve_branch(&mut tx, project_id, &pr.target_branch)
        .await?
        .context("目标分支不存在")?;

    if target == source {
        // 目标分支已经指向来源分支，直接标记合并。
    } else if is_ancestor(&mut tx, project_id, target, source).await? {
        store::set_ref(
            &mut tx,
            project_id,
            &format!("refs/heads/{}", pr.target_branch),
            source,
        )
        .await?;
        crate::git::http::index_commits(&mut tx, project_id, source, user).await?;
        sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
    } else {
        bail!("目标分支包含来源分支没有的提交，请先把目标分支合并进来源分支");
    }

    sqlx::query(
        "UPDATE pull_requests
         SET status = 'merged', merge_commit_oid = $3, merged_at = now(), updated_at = now()
         WHERE project_id = $1 AND number = $2",
    )
    .bind(project_id)
    .bind(number)
    .bind(&source.as_bytes()[..])
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    let mut conn = pool.acquire().await?;
    fetch_pr(&mut conn, project_id, number)
        .await?
        .context("PR 合并后无法读取")
}

pub async fn close_pr(pool: &PgPool, project_id: Uuid, number: i32) -> Result<PullRequest> {
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;

    let pr = fetch_pr(&mut tx, project_id, number)
        .await?
        .context("PR 不存在")?;
    if pr.status != "open" {
        bail!("只有开放的 PR 可以关闭");
    }

    sqlx::query(
        "UPDATE pull_requests SET status = 'closed', updated_at = now()
         WHERE project_id = $1 AND number = $2",
    )
    .bind(project_id)
    .bind(number)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    let mut conn = pool.acquire().await?;
    fetch_pr(&mut conn, project_id, number)
        .await?
        .context("PR 关闭后无法读取")
}

async fn fetch_discussion(
    conn: &mut PgConnection,
    project_id: Uuid,
    discussion_id: Uuid,
) -> Result<Option<Discussion>> {
    Ok(sqlx::query_as::<_, Discussion>(&format!(
        "{DISCUSSION_SELECT} WHERE d.project_id = $1 AND d.id = $2"
    ))
    .bind(project_id)
    .bind(discussion_id)
    .fetch_optional(conn)
    .await?)
}

async fn list_discussion_comments(
    conn: &mut PgConnection,
    discussion_id: Uuid,
) -> Result<Vec<DiscussionComment>> {
    Ok(sqlx::query_as::<_, DiscussionComment>(
        "SELECT c.id, COALESCE(u.username, '已注销用户') AS author_name,
                c.body, c.created_at
         FROM discussion_comments c
         LEFT JOIN users u ON u.id = c.author_id
         WHERE c.discussion_id = $1
         ORDER BY c.created_at ASC",
    )
    .bind(discussion_id)
    .fetch_all(conn)
    .await?)
}

pub async fn list_discussions(pool: &PgPool, project_id: Uuid) -> Result<Vec<Discussion>> {
    Ok(sqlx::query_as::<_, Discussion>(&format!(
        "{DISCUSSION_SELECT} WHERE d.project_id = $1 ORDER BY d.created_at DESC"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?)
}

pub async fn create_discussion(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    title: &str,
    body: &str,
) -> Result<Discussion> {
    let title = validate_title(title, "标题")?;
    let body = validate_body(body, "正文", 20000)?;
    let id = Uuid::new_v4();
    let mut conn = pool.acquire().await?;
    sqlx::query(
        "INSERT INTO project_discussions (id, project_id, title, body, author_id)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(project_id)
    .bind(&title)
    .bind(&body)
    .bind(user.id)
    .execute(&mut *conn)
    .await?;
    fetch_discussion(&mut conn, project_id, id)
        .await?
        .context("讨论创建后无法读取")
}

pub async fn discussion_detail(
    pool: &PgPool,
    project_id: Uuid,
    discussion_id: Uuid,
) -> Result<Option<DiscussionDetail>> {
    let mut conn = pool.acquire().await?;
    let Some(discussion) = fetch_discussion(&mut conn, project_id, discussion_id).await? else {
        return Ok(None);
    };
    let comments = list_discussion_comments(&mut conn, discussion_id).await?;
    Ok(Some(DiscussionDetail {
        discussion,
        comments,
    }))
}

pub async fn add_discussion_comment(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    discussion_id: Uuid,
    body: &str,
) -> Result<DiscussionComment> {
    let body = validate_body(body, "评论内容", 10000)?;
    let mut conn = pool.acquire().await?;
    let discussion = fetch_discussion(&mut conn, project_id, discussion_id)
        .await?
        .context("讨论不存在")?;
    if discussion.closed_at.is_some() {
        bail!("讨论已关闭，不能再评论");
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO discussion_comments (id, discussion_id, author_id, body)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(discussion_id)
    .bind(user.id)
    .bind(&body)
    .execute(&mut *conn)
    .await?;
    Ok(sqlx::query_as::<_, DiscussionComment>(
        "SELECT c.id, COALESCE(u.username, '已注销用户') AS author_name,
                c.body, c.created_at
         FROM discussion_comments c
         LEFT JOIN users u ON u.id = c.author_id
         WHERE c.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?)
}

pub async fn close_discussion(
    pool: &PgPool,
    project_id: Uuid,
    discussion_id: Uuid,
) -> Result<Discussion> {
    let mut conn = pool.acquire().await?;
    let closed: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE project_discussions
         SET closed_at = COALESCE(closed_at, now())
         WHERE project_id = $1 AND id = $2
         RETURNING id",
    )
    .bind(project_id)
    .bind(discussion_id)
    .fetch_optional(&mut *conn)
    .await?;
    if closed.is_none() {
        bail!("讨论不存在");
    }
    fetch_discussion(&mut conn, project_id, discussion_id)
        .await?
        .context("讨论关闭后无法读取")
}
