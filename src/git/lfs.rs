use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::project::Project;

/// LFS 指针文件的首行。
pub const POINTER_PREFIX: &str = "version https://git-lfs.github.com/spec/v1";

/// 判断一个文件内容是否是 Git LFS 指针。
pub fn is_pointer(data: &[u8]) -> bool {
    if data.len() > 1024 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return false;
    };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(POINTER_PREFIX) {
        return false;
    }
    let rest = lines.collect::<Vec<_>>().join("\n");
    rest.contains("oid sha256:") && rest.contains("size ")
}

/// 从 LFS 指针里取出 sha256 OID。
pub fn pointer_oid(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("oid sha256:") {
            let oid = value.trim();
            if valid_oid(oid) {
                return Some(oid.to_string());
            }
        }
    }
    None
}

pub fn valid_oid(oid: &str) -> bool {
    oid.len() == 64
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 磁盘路径：`<根>/<项目>/<前2位>/<次2位>/<完整 OID>`，避免单目录文件过多。
pub fn object_path(root: &Path, project_id: Uuid, oid: &str) -> PathBuf {
    root.join(project_id.to_string())
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(oid)
}

#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    pub operation: String,
    #[serde(default)]
    pub transfers: Vec<String>,
    #[serde(default)]
    pub objects: Vec<BatchObject>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BatchObject {
    pub oid: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub authenticated: Option<bool>,
}

/// 批量接口的返回：`unauthorized` 由 HTTP 层翻译成 401。
pub struct BatchResponse {
    pub body: Value,
    pub unauthorized: bool,
}

async fn exists(pool: &PgPool, project_id: Uuid, oid: &str) -> Result<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT size FROM lfs_objects WHERE project_id = $1 AND oid = $2")
            .bind(project_id)
            .bind(oid)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

/// `POST /info/lfs/objects/batch`。
pub async fn batch(
    pool: &PgPool,
    root: &Path,
    project: &Project,
    body: &[u8],
    base_url: &str,
    authenticated: bool,
) -> Result<BatchResponse> {
    let request: BatchRequest =
        serde_json::from_slice(body).context("LFS 批量请求不是合法 JSON")?;
    let upload = match request.operation.as_str() {
        "upload" => true,
        "download" => false,
        other => bail!("不支持的 LFS 操作: {other}"),
    };
    if upload && !authenticated {
        return Ok(BatchResponse {
            body: json!({ "message": "上传 LFS 对象需要 Git 令牌" }),
            unauthorized: true,
        });
    }

    // 客户端可能请求多种传输方式，目前只实现 basic。
    if !request.transfers.is_empty()
        && !request.transfers.iter().any(|item| item == "basic")
    {
        bail!("只支持 basic 传输方式");
    }

    let mut objects = Vec::with_capacity(request.objects.len());
    for object in &request.objects {
        if !valid_oid(&object.oid) {
            objects.push(json!({
                "oid": object.oid,
                "size": object.size,
                "error": { "code": 422, "message": "OID 必须是 64 位小写十六进制" },
            }));
            continue;
        }
        let present = exists(pool, project.id, &object.oid).await?;
        if upload {
            objects.push(json!({
                "oid": object.oid,
                "size": object.size,
                "authenticated": true,
                "actions": {
                    "upload": {
                        "href": format!("{base_url}/{}", object.oid),
                    }
                },
            }));
        } else if present && object_path(root, project.id, &object.oid).exists() {
            objects.push(json!({
                "oid": object.oid,
                "size": object.size,
                "actions": {
                    "download": {
                        "href": format!("{base_url}/{}", object.oid),
                    }
                },
            }));
        } else {
            objects.push(json!({
                "oid": object.oid,
                "size": object.size,
                "error": { "code": 404, "message": "对象不存在" },
            }));
        }
    }

    Ok(BatchResponse {
        body: json!({ "transfer": "basic", "objects": objects }),
        unauthorized: false,
    })
}

/// 保存一个上传的 LFS 对象，并校验 SHA-256。
pub async fn store(
    pool: &PgPool,
    root: &Path,
    project_id: Uuid,
    oid: &str,
    data: &[u8],
) -> Result<()> {
    if !valid_oid(oid) {
        bail!("LFS OID 必须是 64 位小写十六进制");
    }
    let digest = hex::encode(Sha256::digest(data));
    if digest != oid {
        bail!("LFS 对象内容与 OID 不匹配");
    }
    if let Some((size,)) =
        sqlx::query_as::<_, (i64,)>("SELECT size FROM lfs_objects WHERE project_id = $1 AND oid = $2")
            .bind(project_id)
            .bind(oid)
            .fetch_optional(pool)
            .await?
    {
        if size != data.len() as i64 {
            bail!("LFS 对象大小与已登记记录不一致");
        }
    }

    let path = object_path(root, project_id, oid);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("无法创建 LFS 目录: {}", parent.display()))?;
    }
    let temp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    tokio::fs::write(&temp, data)
        .await
        .with_context(|| format!("无法写入 LFS 临时文件: {}", temp.display()))?;
    tokio::fs::rename(&temp, &path)
        .await
        .with_context(|| format!("无法保存 LFS 文件: {}", path.display()))?;

    sqlx::query(
        "INSERT INTO lfs_objects (project_id, oid, size)
         VALUES ($1, $2, $3)
         ON CONFLICT (project_id, oid) DO UPDATE SET size = EXCLUDED.size",
    )
    .bind(project_id)
    .bind(oid)
    .bind(data.len() as i64)
    .execute(pool)
    .await?;
    Ok(())
}

/// 读取 LFS 对象；不存在时返回 None。
pub async fn load(
    pool: &PgPool,
    root: &Path,
    project_id: Uuid,
    oid: &str,
) -> Result<Option<Vec<u8>>> {
    if !valid_oid(oid) {
        return Ok(None);
    }
    if !exists(pool, project_id, oid).await? {
        return Ok(None);
    }
    let path = object_path(root, project_id, oid);
    match tokio::fs::read(&path).await {
        Ok(data) => Ok(Some(data)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("无法读取 LFS 文件: {}", path.display())),
    }
}

/// 删除数据库记录与落盘文件（gc 使用）。
pub async fn remove(pool: &PgPool, root: &Path, project_id: Uuid, oid: &str) -> Result<()> {
    remove_row(pool, project_id, oid).await?;
    remove_file(root, project_id, oid).await
}

/// 只删数据库索引行。gc 在持有项目锁的事务里调用，文件删除留到提交后。
pub async fn remove_row(
    conn: impl sqlx::PgExecutor<'_>,
    project_id: Uuid,
    oid: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM lfs_objects WHERE project_id = $1 AND oid = $2")
        .bind(project_id)
        .bind(oid)
        .execute(conn)
        .await?;
    Ok(())
}

/// 只删磁盘文件，索引行已由调用方删除。
pub async fn remove_file(root: &Path, project_id: Uuid, oid: &str) -> Result<()> {
    let path = object_path(root, project_id, oid);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("无法删除 LFS 文件: {}", path.display()))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_lfs_pointer() {
        let data = b"version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n";
        assert!(is_pointer(data));
        assert_eq!(
            pointer_oid(data).as_deref(),
            Some("4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393")
        );
    }

    #[test]
    fn rejects_normal_text() {
        assert!(!is_pointer(b"hello world\n"));
        assert!(!is_pointer(&vec![0u8; 2000]));
    }

    #[test]
    fn validates_oids() {
        assert!(valid_oid(
            "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393"
        ));
        assert!(!valid_oid("ABCDEF"));
        assert!(!valid_oid("zz"));
    }
}
