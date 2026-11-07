//! 项目协作：Pull Request 与项目讨论。
//!
//! PR 差异使用三点比较（merge-base → 来源分支），合并时能快进就快进；
//! 不能快进时在服务端生成一个双亲 merge commit，树直接取来源分支，
//! 因此目标分支独有的改动会被覆盖，详情接口会通过警示字段告知前端。
//! 所有写操作都在项目级 advisory lock 下串行执行，与 `project.rs` 的文件编辑共用同一把锁。

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use similar::{ChangeTag, TextDiff};
use sqlx::{PgConnection, PgPool};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use uuid::Uuid;

use crate::git::object::{
    encode_commit, identity_name, is_text, Object, ObjectKind, Oid, TreeEntry,
};
use crate::git::store;
use crate::git::tree;
use crate::models::User;
use crate::project::valid_branch;

/// 与 `git/http.rs` 保持一致的提交遍历上限，防止过深历史拖垮请求。
const MAX_COMMIT_WALK: usize = 5000;

/// 行级差异的展示上限：超过任一限制就只显示文件名，不生成 hunks。
const DIFF_MAX_BYTES: usize = 200 * 1024;
const DIFF_MAX_LINES: usize = 4000;
/// 每个 hunk 上下保留的相等行数。
const DIFF_CONTEXT: usize = 3;

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
pub struct DiffLine {
    /// `context` / `add` / `del`
    pub kind: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffHunk {
    /// 形如 `@@ -1,3 +1,4 @@`。
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FileDiff {
    pub path: String,
    /// `add` / `modify` / `delete`
    pub status: String,
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub additions: usize,
    pub deletions: usize,
    /// 文件过大或不是文本时置为 true，此时 `hunks` 为空。
    pub truncated: bool,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrDetail {
    #[serde(flatten)]
    pub pr: PullRequest,
    pub files: Vec<FileDiff>,
    /// 两个分支都存在时就可以合并（非快进会生成 merge commit）。
    pub mergeable: bool,
    /// 目标分支是否已经是来源分支的祖先。
    pub is_fast_forward: bool,
    /// 非快进合并会用来源分支的树覆盖目标分支，前端需要明确警示。
    pub overwrites_target: bool,
    pub merge_base: Option<String>,
    pub comments: Vec<PrComment>,
}

/// 合并结果。非快进时 `overwrote_target` 为 true。
pub struct MergeOutcome {
    pub pr: PullRequest,
    pub overwrote_target: bool,
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

/// 读取提交父关系图，同时记录到起点的深度。
async fn walk_graph(
    conn: &mut PgConnection,
    project_id: Uuid,
    start: Oid,
    graph: &mut HashMap<Oid, Vec<Oid>>,
    depths: &mut HashMap<Oid, usize>,
) -> Result<HashSet<Oid>> {
    let mut seen = HashSet::new();
    let mut stack = vec![(start, 0usize)];
    while let Some((oid, depth)) = stack.pop() {
        if seen.len() >= MAX_COMMIT_WALK {
            bail!("提交历史过深，无法计算合并基点");
        }
        if !seen.insert(oid) {
            continue;
        }
        depths.entry(oid).or_insert(depth);
        if let Some(parents) = graph.get(&oid) {
            for parent in parents {
                stack.push((*parent, depth + 1));
            }
            continue;
        }
        let object = store::require_object(conn, project_id, oid).await?;
        if object.kind != ObjectKind::Commit {
            graph.insert(oid, Vec::new());
            continue;
        }
        let parents = crate::git::object::commit_parents(&object.data)?;
        for parent in &parents {
            stack.push((*parent, depth + 1));
        }
        graph.insert(oid, parents);
    }
    Ok(seen)
}

fn reachable_in_graph(graph: &HashMap<Oid, Vec<Oid>>, from: Oid, target: Oid) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![from];
    while let Some(oid) = stack.pop() {
        if oid == target {
            return true;
        }
        if !seen.insert(oid) {
            continue;
        }
        if let Some(parents) = graph.get(&oid) {
            stack.extend(parents.iter().copied());
        }
    }
    false
}

/// 计算两个提交的合并基点；没有公共祖先时返回 None。
pub(crate) async fn merge_base(
    conn: &mut PgConnection,
    project_id: Uuid,
    a: Oid,
    b: Oid,
) -> Result<Option<Oid>> {
    if a == b {
        return Ok(Some(a));
    }
    let mut graph = HashMap::new();
    let mut depths_a = HashMap::new();
    let mut depths_b = HashMap::new();
    let set_a = walk_graph(conn, project_id, a, &mut graph, &mut depths_a).await?;
    let set_b = walk_graph(conn, project_id, b, &mut graph, &mut depths_b).await?;

    let mut common: Vec<Oid> = set_a.intersection(&set_b).copied().collect();
    if common.is_empty() {
        return Ok(None);
    }
    // 公共祖先很多时用「到两侧距离之和」近似挑选最近的基点，
    // 避免在大仓库上做平方级的祖先判定。
    if common.len() > 64 {
        common.sort_by_key(|oid| {
            depths_a.get(oid).copied().unwrap_or(usize::MAX)
                .saturating_add(depths_b.get(oid).copied().unwrap_or(usize::MAX))
        });
        return Ok(common.first().copied());
    }

    let candidates = common.clone();
    for candidate in &candidates {
        let is_ancestor_of_other = candidates
            .iter()
            .any(|other| other != candidate && reachable_in_graph(&graph, *other, *candidate));
        if !is_ancestor_of_other {
            return Ok(Some(*candidate));
        }
    }
    Ok(candidates.first().copied())
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

async fn diff_roots(
    conn: &mut PgConnection,
    project_id: Uuid,
    old_root: Option<Oid>,
    new_root: Oid,
) -> Result<Vec<FileDiff>> {
    let old = match old_root {
        Some(root) => blob_map(conn, project_id, root).await?,
        None => BTreeMap::new(),
    };
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

        // 新增/删除时缺失的一侧按空文件处理，只有真实存在却读不出文本才视为无法展示。
        let old_text = match old_oid {
            None => Some(String::new()),
            Some(oid) => blob_text(conn, project_id, oid).await?,
        };
        let new_text = match new_oid {
            None => Some(String::new()),
            Some(oid) => blob_text(conn, project_id, oid).await?,
        };
        let (additions, deletions, hunks, truncated) = match (old_text, new_text) {
            (Some(old_text), Some(new_text)) => {
                if old_text.len() > DIFF_MAX_BYTES
                    || new_text.len() > DIFF_MAX_BYTES
                    || old_text.lines().count() > DIFF_MAX_LINES
                    || new_text.lines().count() > DIFF_MAX_LINES
                {
                    (0, 0, Vec::new(), true)
                } else {
                    let (hunks, additions, deletions) = compute_diff(&old_text, &new_text);
                    (additions, deletions, hunks, false)
                }
            }
            _ => (0, 0, Vec::new(), true),
        };

        diffs.push(FileDiff {
            path: path.clone(),
            status: status.to_string(),
            old_oid: old_oid.map(|oid| oid.to_hex()),
            new_oid: new_oid.map(|oid| oid.to_hex()),
            additions,
            deletions,
            truncated,
            hunks,
        });
    }
    Ok(diffs)
}

/// 比较两个提交；`base` 为 None 时按空树处理（根提交）。
pub async fn diff_commits(
    conn: &mut PgConnection,
    project_id: Uuid,
    base: Option<Oid>,
    head: Oid,
) -> Result<Vec<FileDiff>> {
    let new_root = tree::read_commit_tree(conn, project_id, head).await?;
    let old_root = match base {
        Some(base) => Some(tree::read_commit_tree(conn, project_id, base).await?),
        None => None,
    };
    diff_roots(conn, project_id, old_root, new_root).await
}

/// 读取 blob 的文本内容；不是 blob 或不是合法 UTF-8 时返回 None。
async fn blob_text(conn: &mut PgConnection, project_id: Uuid, oid: Oid) -> Result<Option<String>> {
    let object = store::require_object(conn, project_id, oid).await?;
    if object.kind != ObjectKind::Blob || !is_text(&object.data) {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&object.data).into_owned()))
}

/// 生成按 hunk 分组的行级差异，并统计新增/删除行数。
fn compute_diff(old: &str, new: &str) -> (Vec<DiffHunk>, usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let mut additions = 0;
    let mut deletions = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }

    let mut hunks = Vec::new();
    for hunk in diff
        .unified_diff()
        .context_radius(DIFF_CONTEXT)
        .iter_hunks()
    {
        let mut lines = Vec::new();
        for line in hunk.iter_changes() {
            let kind = match line.tag() {
                ChangeTag::Insert => "add",
                ChangeTag::Delete => "del",
                ChangeTag::Equal => "context",
            };
            lines.push(DiffLine {
                kind: kind.to_string(),
                old_line: line.old_index().map(|index| index + 1),
                new_line: line.new_index().map(|index| index + 1),
                // 行内容已按行切分，尾部换行不属于展示内容。
                text: line.to_string().trim_end_matches('\n').to_string(),
            });
        }
        hunks.push(DiffHunk {
            header: hunk.header().to_string(),
            lines,
        });
    }
    (hunks, additions, deletions)
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
    let (files, mergeable, is_fast_forward, overwrites_target, merge_base) =
        match (source, target) {
            (Some(source_oid), Some(target_oid)) => {
                // 三点差异：以两个分支的合并基点为基准，避免把目标分支
                // 独有的历史算进 PR 的改动里。
                let base = merge_base(&mut conn, project_id, target_oid, source_oid).await?;
                let files = diff_commits(&mut conn, project_id, base, source_oid).await?;
                let fast_forward = source_oid == target_oid
                    || is_ancestor(&mut conn, project_id, target_oid, source_oid).await?;
                (
                    files,
                    true,
                    fast_forward,
                    !fast_forward,
                    base.map(|oid| oid.to_hex()),
                )
            }
            _ => (Vec::new(), false, false, false, None),
        };

    Ok(Some(PrDetail {
        pr,
        files,
        mergeable,
        is_fast_forward,
        overwrites_target,
        merge_base,
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
) -> Result<MergeOutcome> {
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

    let fast_forward = target == source
        || is_ancestor(&mut tx, project_id, target, source).await?;
    let merge_oid = if fast_forward {
        source
    } else {
        // 非快进：创建双亲 merge commit，树直接取来源分支。
        // 目标分支独有的改动会被覆盖，详情接口已提前给出警示。
        let tree_oid = tree::read_commit_tree(&mut tx, project_id, source).await?;
        let identity = format!("{} <noreply@kych.net>", identity_name(&user.username));
        let timestamp = Utc::now().timestamp();
        let message = format!(
            "合并 PR #{number}: {}\n\n来源分支: {}\n目标分支: {}",
            pr.title, pr.source_branch, pr.target_branch
        );
        let data = encode_commit(
            tree_oid,
            &[target, source],
            &identity,
            &message,
            timestamp,
        );
        store::put_object(
            &mut tx,
            project_id,
            &Object {
                kind: ObjectKind::Commit,
                data,
            },
        )
        .await?
    };

    store::set_ref(
        &mut tx,
        project_id,
        &format!("refs/heads/{}", pr.target_branch),
        merge_oid,
    )
    .await?;
    crate::git::http::index_commits(&mut tx, project_id, merge_oid, user).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE pull_requests
         SET status = 'merged', merge_commit_oid = $3, merged_at = now(), updated_at = now()
         WHERE project_id = $1 AND number = $2",
    )
    .bind(project_id)
    .bind(number)
    .bind(&merge_oid.as_bytes()[..])
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    let mut conn = pool.acquire().await?;
    let pr = fetch_pr(&mut conn, project_id, number)
        .await?
        .context("PR 合并后无法读取")?;
    Ok(MergeOutcome {
        pr,
        overwrote_target: !fast_forward,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_line_numbers_and_stats() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nd\ne\n";
        let (hunks, additions, deletions) = compute_diff(old, new);
        assert_eq!(additions, 2);
        assert_eq!(deletions, 1);
        assert_eq!(hunks.len(), 1);
        let lines = &hunks[0].lines;
        // 3 行上下文 + 1 删 + 2 增。
        assert_eq!(lines.len(), 6);
        assert!(lines.iter().any(|line| line.kind == "del"
            && line.old_line == Some(2)
            && line.new_line.is_none()
            && line.text == "b"));
        assert!(lines.iter().any(|line| line.kind == "add"
            && line.old_line.is_none()
            && line.new_line == Some(2)
            && line.text == "B"));
        assert_eq!(hunks[0].header, "@@ -1,4 +1,5 @@");
    }

    #[test]
    fn added_file_marks_every_line() {
        let (hunks, additions, deletions) = compute_diff("", "x\ny\n");
        assert_eq!((additions, deletions), (2, 0));
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0]
            .lines
            .iter()
            .all(|line| line.kind == "add" && line.old_line.is_none()));
    }

    #[test]
    fn walks_parent_graph_for_ancestry() {
        let a = Oid::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let b = Oid::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let c = Oid::from_hex("cccccccccccccccccccccccccccccccccccccccc").unwrap();
        let mut graph = HashMap::new();
        graph.insert(b, vec![a]);
        graph.insert(c, vec![b]);
        assert!(reachable_in_graph(&graph, c, a));
        assert!(!reachable_in_graph(&graph, a, c));
    }
}
