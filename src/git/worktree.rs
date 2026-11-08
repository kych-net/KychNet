//! 工作树读取（`/git/<slug>.git/worktree/*` 的数据层）。
//!
//! 站内没有磁盘工作树，也没有 `.git`：这里把 PG `git_objects` 里的
//! commit / tree / blob 即时展开成「路径 → 字节」，和 `git-upload-pack`
//! 读的是同一份对象。管理端 `/api/projects/{id}/tree|file`、静态站构建
//! （`project::snapshots`）、runner 快照用的也都是 `git::tree` 这一层。
//!
//! 与 packfile 通道的区别：packfile 要客户端解包再 checkout 才能拿到文件；
//! 这里直接给出展开后的工作树，调用方不需要 git 二进制、不需要 `.git`。

use anyhow::{bail, Context, Result};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::path::Path;
use uuid::Uuid;

use super::lfs;
use super::object::Oid;
use super::store;
use super::tree::{self, DirItem};
use crate::project::{valid_branch, valid_file_path};

/// 全量工作树导出的字节上限，避免超大仓库把主服务内存拖垮。
pub const MAX_WORKTREE_BYTES: u64 = 209_715_200;

/// `ref` 支持分支名，也支持 40 位 commit OID。
///
/// 执行器从 claim 里拿到的是 `commit_oid`，而不是分支名；分支名可能会在
/// run 排队期间被新的 push 顶掉，按 OID 读才能保证代码与定义是同一时刻的。
/// 返回 `None` 表示分支不存在。
pub async fn resolve_ref(pool: &PgPool, project_id: Uuid, ref_name: &str) -> Result<Option<Oid>> {
    if ref_name.len() == 40 && ref_name.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Oid::from_hex(ref_name)
            .map(Some)
            .context("非法的 commit OID");
    }
    if !valid_branch(ref_name) {
        bail!("非法 ref：{ref_name:?}");
    }
    let mut conn = pool.acquire().await?;
    store::resolve_branch(&mut conn, project_id, ref_name).await
}

/// 单层目录列表，与管理端 `project_tree` 的 `items` 同构。
pub async fn list(pool: &PgPool, project_id: Uuid, commit: Oid, dir: &str) -> Result<Vec<DirItem>> {
    if !dir.is_empty() && !valid_file_path(dir) {
        bail!("非法目录路径：{dir:?}");
    }
    let mut conn = pool.acquire().await?;
    tree::list_dir(&mut conn, project_id, commit, dir).await
}

/// 读单个文件。返回 `(blob oid, 字节, 是否由 LFS pointer 还原)`。
///
/// 管理端的 `project::read_file` 会拒绝非 UTF-8 文本，这里不做该限制：
/// 工作树接口要能交出二进制内容，并把 LFS pointer 还原成真实字节。
pub async fn read(
    pool: &PgPool,
    project_id: Uuid,
    commit: Oid,
    path: &str,
    lfs_root: &Path,
) -> Result<Option<(Oid, Vec<u8>, bool)>> {
    if !valid_file_path(path) {
        bail!("非法文件路径：{path:?}");
    }
    let mut conn = pool.acquire().await?;
    let Some((oid, data)) = tree::find_file(&mut conn, project_id, commit, path).await? else {
        return Ok(None);
    };
    drop(conn);
    if lfs::is_pointer(&data) {
        if let Some(object_oid) = lfs::pointer_oid(&data) {
            let real = lfs::load(pool, lfs_root, project_id, &object_oid)
                .await?
                .with_context(|| format!("LFS 对象缺失：{object_oid}（引用自 {path}）"))?;
            return Ok(Some((oid, real, true)));
        }
    }
    Ok(Some((oid, data, false)))
}

/// 完整工作树：路径 → 字节，LFS pointer 已还原。
///
/// 路径键是仓库内相对路径（不带 `code/` 之类的前缀）；调用方拿去直接铺
/// 工作区即可。`tree::read_files` 已校验过 tree 条目名，这里再复检一次。
pub async fn collect(
    pool: &PgPool,
    project_id: Uuid,
    commit: Oid,
    lfs_root: &Path,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut conn = pool.acquire().await?;
    let mut files = tree::read_files(&mut conn, project_id, commit).await?;
    drop(conn);
    for (path, data) in files.iter_mut() {
        if path.split('/').any(|seg| {
            seg.is_empty()
                || seg == "."
                || seg == ".."
                || seg.eq_ignore_ascii_case(".git")
                || seg.chars().any(char::is_control)
        }) {
            bail!("工作树路径包含非法段：{path:?}");
        }
        if !lfs::is_pointer(data) {
            continue;
        }
        if let Some(object_oid) = lfs::pointer_oid(data) {
            let real = lfs::load(pool, lfs_root, project_id, &object_oid)
                .await?
                .with_context(|| format!("LFS 对象缺失：{object_oid}（引用自 {path}）"))?;
            *data = real;
        }
    }
    Ok(files)
}

/// 全量工作树的字节数，用于 archive 限额。
pub fn total_bytes(files: &BTreeMap<String, Vec<u8>>) -> u64 {
    files.values().map(|data| data.len() as u64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_bytes_counts_all_files() {
        let mut files = BTreeMap::new();
        files.insert("a.md".to_string(), vec![0; 3]);
        files.insert("dir/b.bin".to_string(), vec![0; 5]);
        assert_eq!(total_bytes(&files), 8);
    }

    #[test]
    fn rejects_unsafe_worktree_paths() {
        let bad = |seg: &str| {
            seg.is_empty()
                || seg == "."
                || seg == ".."
                || seg.eq_ignore_ascii_case(".git")
                || seg.chars().any(char::is_control)
        };
        assert!(bad(".."));
        assert!(bad(".git"));
        assert!(bad(".GIT"));
        assert!(bad("a\0b"));
        assert!(!bad("src/main.rs"));
    }
}
