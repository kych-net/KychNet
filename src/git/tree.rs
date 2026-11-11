use anyhow::{bail, Result};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use uuid::Uuid;

use super::object::{commit_tree, ensure_text, Object, Oid, Tree, TreeEntry};
use super::store::{get_object, put_object, require_object};

pub async fn read_tree(conn: &mut PgConnection, project_id: Uuid, oid: Oid) -> Result<Tree> {
    let object = require_object(conn, project_id, oid).await?;
    if object.kind != super::object::ObjectKind::Tree {
        bail!("{oid} 不是 tree 对象");
    }
    Tree::parse(&object.data)
}

pub async fn read_commit_tree(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
) -> Result<Oid> {
    let object = require_object(conn, project_id, commit).await?;
    if object.kind != super::object::ObjectKind::Commit {
        bail!("{commit} 不是 commit 对象");
    }
    commit_tree(&object.data)
}

/// 读取某个提交的完整文件快照，路径相对于仓库根。
pub async fn read_files(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let root = read_commit_tree(conn, project_id, commit).await?;
    let mut files = BTreeMap::new();
    let mut stack = vec![(String::new(), root)];
    while let Some((prefix, tree_oid)) = stack.pop() {
        let tree = read_tree(conn, project_id, tree_oid).await?;
        for (name, entry) in tree.entries {
            // Tree::parse 已经校验过；这里在拼接完整路径前再复检一次，
            // 防止未来新增的 tree 构造路径绕开解析层。
            if !super::object::valid_tree_entry_name(&name) {
                bail!("tree 对象包含非法条目名: {name:?}");
            }
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if path
                .split('/')
                .any(|part| !super::object::valid_tree_entry_name(part))
            {
                bail!("tree 对象拼接出非法路径: {path:?}");
            }
            match entry {
                TreeEntry::Dir(oid) => stack.push((path, oid)),
                TreeEntry::Blob(oid) => {
                    let object = require_object(conn, project_id, oid).await?;
                    files.insert(path, object.data);
                }
                // 子模块占位：不参与归档/快照内容。
                TreeEntry::Gitlink(_) => {}
            }
        }
    }
    Ok(files)
}

/// 返回某个目录下的直接子项。`dir` 为空表示仓库根。
pub async fn list_dir(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
    dir: &str,
) -> Result<Vec<DirItem>> {
    let mut current = read_commit_tree(conn, project_id, commit).await?;
    if !dir.is_empty() {
        for part in dir.split('/') {
            let tree = read_tree(conn, project_id, current).await?;
            let Some(entry) = tree.get(part) else {
                return Ok(Vec::new());
            };
            match entry {
                TreeEntry::Dir(oid) => current = oid,
                TreeEntry::Blob(_) => bail!("{dir} 不是目录"),
                TreeEntry::Gitlink(_) => bail!("{dir} 是子模块，站内不托管其内容"),
            }
        }
    }

    let tree = read_tree(conn, project_id, current).await?;
    let mut items = Vec::with_capacity(tree.entries.len());
    for (name, entry) in tree.entries {
        let path = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        match entry {
            TreeEntry::Dir(oid) => items.push(DirItem {
                name,
                path,
                is_dir: true,
                size: 0,
                oid: oid.to_hex(),
            }),
            TreeEntry::Blob(oid) => {
                let size = get_object(conn, project_id, oid)
                    .await?
                    .map(|object| object.data.len())
                    .unwrap_or(0);
                items.push(DirItem {
                    name,
                    path,
                    is_dir: false,
                    size,
                    oid: oid.to_hex(),
                });
            }
            // 子模块：工作树里是目录占位，列表里按目录展示，
            // 深入时由上面的 Gitlink 分支给出明确报错。
            TreeEntry::Gitlink(oid) => {
                items.push(DirItem {
                    name,
                    path,
                    is_dir: true,
                    size: 0,
                    oid: oid.to_hex(),
                });
            }
        }
    }
    items.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(items)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DirItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: usize,
    pub oid: String,
}

/// 读取单个文件；路径不存在时返回 None。
pub async fn find_file(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
    path: &str,
) -> Result<Option<(Oid, Vec<u8>)>> {
    let mut current = read_commit_tree(conn, project_id, commit).await?;
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return Ok(None);
    }
    for (index, part) in parts.iter().enumerate() {
        let tree = read_tree(conn, project_id, current).await?;
        let Some(entry) = tree.get(part) else {
            return Ok(None);
        };
        if index + 1 == parts.len() {
            return match entry {
                TreeEntry::Dir(_) | TreeEntry::Gitlink(_) => Ok(None),
                TreeEntry::Blob(oid) => {
                    let object = require_object(conn, project_id, oid).await?;
                    Ok(Some((oid, object.data)))
                }
            };
        }
        match entry {
            TreeEntry::Dir(oid) => current = oid,
            TreeEntry::Blob(_) | TreeEntry::Gitlink(_) => return Ok(None),
        }
    }
    Ok(None)
}

/// 在已有 tree 上增删一个路径，只重建沿途节点。
///
/// `blob` 为 None 表示删除。返回新的根 tree OID。
pub async fn update_path(
    conn: &mut PgConnection,
    project_id: Uuid,
    root: Option<Oid>,
    path: &str,
    blob: Option<Oid>,
) -> Result<Oid> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        bail!("文件路径不能为空");
    }

    let mut current = match root {
        Some(oid) => read_tree(conn, project_id, oid).await?,
        None => Tree::default(),
    };

    // 记录从根到叶子目录的每一层，用于自底向上回写。
    let mut parents: Vec<Tree> = Vec::new();
    for part in &parts[..parts.len() - 1] {
        match current.get(part) {
            Some(TreeEntry::Dir(oid)) => {
                parents.push(current.clone());
                current = read_tree(conn, project_id, oid).await?;
            }
            Some(TreeEntry::Blob(_)) | Some(TreeEntry::Gitlink(_)) => {
                bail!("路径 {path} 的中间段是文件")
            }
            None => {
                parents.push(current.clone());
                current = Tree::default();
            }
        }
    }

    let leaf = parts[parts.len() - 1];
    match blob {
        Some(oid) => {
            current
                .entries
                .insert(leaf.to_string(), TreeEntry::Blob(oid));
        }
        None => {
            current.entries.remove(leaf);
            // 文件本来就不存在时，删除是幂等的。
            if current.entries.is_empty() && parents.is_empty() {
                return write_tree(conn, project_id, &current).await;
            }
        }
    }

    let mut child = if current.entries.is_empty() {
        None
    } else {
        Some(write_tree(conn, project_id, &current).await?)
    };

    for index in (0..parents.len()).rev() {
        let mut parent = parents[index].clone();
        let name = parts[index];
        match child {
            Some(oid) => {
                parent.entries.insert(name.to_string(), TreeEntry::Dir(oid));
            }
            None => {
                parent.entries.remove(name);
            }
        }
        child = if parent.entries.is_empty() {
            None
        } else {
            Some(write_tree(conn, project_id, &parent).await?)
        };
    }

    match child {
        Some(oid) => Ok(oid),
        None => write_tree(conn, project_id, &Tree::default()).await,
    }
}

pub async fn write_tree(conn: &mut PgConnection, project_id: Uuid, tree: &Tree) -> Result<Oid> {
    put_object(conn, project_id, &Object::tree(tree.encode())).await
}

pub async fn write_blob(conn: &mut PgConnection, project_id: Uuid, content: &[u8]) -> Result<Oid> {
    ensure_text(content)?;
    put_object(conn, project_id, &Object::blob(content.to_vec())).await
}

/// 从空的仓库一次性构建整棵树；仅用于初始提交与导入。
pub async fn build_from_files(
    conn: &mut PgConnection,
    project_id: Uuid,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Oid> {
    let mut root: Option<Oid> = None;
    for (path, content) in files {
        let blob = write_blob(conn, project_id, content).await?;
        root = Some(update_path(conn, project_id, root, path, Some(blob)).await?);
    }
    match root {
        Some(oid) => Ok(oid),
        None => write_tree(conn, project_id, &Tree::default()).await,
    }
}
