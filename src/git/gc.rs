use anyhow::Result;
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use std::path::Path;

use super::object::{commit_tree, ObjectKind, Oid, Tree, TreeEntry};
use super::{lfs, store};
use crate::project::Project;

/// gc 的统计结果，CLI 直接打印。
pub struct GcStats {
    pub objects_removed: usize,
    pub commits_removed: usize,
    pub paths_removed: usize,
    pub lfs_removed: usize,
}

/// 从所有引用出发标记可达对象。
pub(crate) async fn mark_reachable(
    pool: &PgPool,
    project_id: uuid::Uuid,
    extra: Vec<Oid>,
) -> Result<(HashSet<Oid>, HashSet<Oid>, HashSet<String>)> {
    let mut conn = pool.acquire().await?;
    let refs = store::list_refs(&mut conn, project_id).await?;
    let mut reachable = HashSet::new();
    let mut commits = HashSet::new();
    let mut lfs_oids = HashSet::new();
    let mut stack: Vec<Oid> = refs.into_iter().map(|(_, oid)| oid).collect();
    // 排队 / 执行中的工作流 run 引用的 commit 也要连同其树一起保留，
    // 否则 gc 会删掉快照接口尚未读取的 Git 对象。
    stack.extend(extra);

    while let Some(oid) = stack.pop() {
        if !reachable.insert(oid) {
            continue;
        }
        let Some(object) = store::get_object(&mut conn, project_id, oid).await? else {
            continue;
        };
        match object.kind {
            ObjectKind::Commit => {
                commits.insert(oid);
                stack.push(commit_tree(&object.data)?);
                stack.extend(super::object::commit_parents(&object.data)?);
            }
            ObjectKind::Tree => {
                let tree = Tree::parse(&object.data)?;
                for entry in tree.entries.values() {
                    match entry {
                        TreeEntry::Blob(oid) | TreeEntry::Dir(oid) => stack.push(*oid),
                        // gitlink 指向外部仓库的提交，本仓库无此对象。
                        TreeEntry::Gitlink(_) => {}
                    }
                }
            }
            ObjectKind::Tag => {
                if let Some(target) = tag_target(&object.data)? {
                    stack.push(target);
                }
            }
            ObjectKind::Blob => {
                if let Some(oid) = lfs::pointer_oid(&object.data) {
                    lfs_oids.insert(oid);
                }
            }
        }
    }
    Ok((reachable, commits, lfs_oids))
}

fn tag_target(data: &[u8]) -> Result<Option<Oid>> {
    let text = std::str::from_utf8(data).map_err(|_| anyhow::anyhow!("tag 对象不是 UTF-8"))?;
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("object ") {
            return Ok(Some(Oid::from_hex(value.trim())?));
        }
    }
    Ok(None)
}

/// 清理项目内不可达对象、孤立提交索引与无引用的 LFS 文件。
pub async fn run(pool: &PgPool, root: &Path, project: &Project) -> Result<GcStats> {
    let pending = crate::workflow::dispatch::pending_commits(pool)
        .await?
        .into_iter()
        .filter_map(|bytes| Oid::from_slice(&bytes).ok())
        .collect();
    let (reachable, commits, lfs_oids) = mark_reachable(pool, project.id, pending).await?;

    let mut objects_removed = 0usize;
    let rows = sqlx::query("SELECT oid FROM git_objects WHERE project_id = $1")
        .bind(project.id)
        .fetch_all(pool)
        .await?;
    for row in rows {
        let bytes: Vec<u8> = row.try_get("oid")?;
        let oid = Oid::from_slice(&bytes)?;
        if !reachable.contains(&oid) {
            sqlx::query("DELETE FROM git_objects WHERE project_id = $1 AND oid = $2")
                .bind(project.id)
                .bind(&oid.as_bytes()[..])
                .execute(pool)
                .await?;
            objects_removed += 1;
        }
    }

    let mut commits_removed = 0usize;
    let rows = sqlx::query("SELECT oid FROM git_commits WHERE project_id = $1")
        .bind(project.id)
        .fetch_all(pool)
        .await?;
    for row in rows {
        let bytes: Vec<u8> = row.try_get("oid")?;
        let oid = Oid::from_slice(&bytes)?;
        if !commits.contains(&oid) {
            sqlx::query("DELETE FROM git_commits WHERE project_id = $1 AND oid = $2")
                .bind(project.id)
                .bind(&oid.as_bytes()[..])
                .execute(pool)
                .await?;
            commits_removed += 1;
        }
    }

    let paths_removed = sqlx::query(
        "DELETE FROM git_commit_paths p
         WHERE p.project_id = $1
           AND NOT EXISTS (
               SELECT 1 FROM git_commits c
               WHERE c.project_id = p.project_id AND c.oid = p.commit_oid
           )",
    )
    .bind(project.id)
    .execute(pool)
    .await?
    .rows_affected() as usize;

    let mut lfs_removed = 0usize;
    let rows = sqlx::query("SELECT oid FROM lfs_objects WHERE project_id = $1")
        .bind(project.id)
        .fetch_all(pool)
        .await?;
    for row in rows {
        let oid: String = row.try_get("oid")?;
        if !lfs_oids.contains(&oid) {
            lfs::remove(pool, root, project.id, &oid).await?;
            lfs_removed += 1;
        }
    }

    Ok(GcStats {
        objects_removed,
        commits_removed,
        paths_removed,
        lfs_removed,
    })
}
