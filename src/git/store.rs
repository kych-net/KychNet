use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sqlx::{PgConnection, PgPool, Row};
use std::io::{Read, Write};
use uuid::Uuid;

use super::object::{Object, ObjectKind, Oid};

/// 小于这个字节数就不压缩：zlib 的头尾开销约 11 字节，
/// 小对象压完往往更大，反而多一次解压。
const COMPRESS_MIN_BYTES: usize = 256;

/// 按需压缩对象内容。返回 None 表示不该压缩（太小或压不动）。
fn compress_blob(data: &[u8]) -> Result<Option<Vec<u8>>> {
    if data.len() < COMPRESS_MIN_BYTES {
        return Ok(None);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).context("压缩 Git 对象失败")?;
    let packed = encoder.finish().context("压缩 Git 对象失败")?;
    // 压不小就存原样，免得白多一次解压。
    if packed.len() >= data.len() {
        return Ok(None);
    }
    Ok(Some(packed))
}

/// 解开 zlib 流。`size` 是原始大小，用来预分配并顺带校验。
fn decompress_blob(data: &[u8], size: i64) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size.max(0) as usize);
    ZlibDecoder::new(data)
        .read_to_end(&mut out)
        .context("解压 Git 对象失败")?;
    if out.len() as i64 != size {
        bail!(
            "Git 对象解压后大小不符：期望 {size} 字节，实际 {} 字节",
            out.len()
        );
    }
    Ok(out)
}

/// 写入对象并返回 OID。同 OID 已存在时直接复用。
pub async fn put_object(
    conn: &mut PgConnection,
    project_id: Uuid,
    object: &Object,
) -> Result<Oid> {
    let oid = object.oid();
    // 压缩是可选的：压不小就存原样，读取端按 compressed 列判断。
    let packed = compress_blob(&object.data)?;
    let (payload, compressed) = match &packed {
        Some(packed) => (packed.as_slice(), true),
        None => (object.data.as_slice(), false),
    };
    sqlx::query(
        "INSERT INTO git_objects (project_id, oid, kind, size, data, compressed)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (project_id, oid) DO NOTHING",
    )
    .bind(project_id)
    .bind(&oid.as_bytes()[..])
    .bind(object.kind as i16)
    .bind(object.data.len() as i64)
    .bind(payload)
    .bind(compressed)
    .execute(conn)
    .await?;
    Ok(oid)
}

pub async fn get_object(
    conn: &mut PgConnection,
    project_id: Uuid,
    oid: Oid,
) -> Result<Option<Object>> {
    let row = sqlx::query(
        "SELECT kind, size, data, compressed FROM git_objects WHERE project_id = $1 AND oid = $2",
    )
    .bind(project_id)
    .bind(&oid.as_bytes()[..])
    .fetch_optional(conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let kind: i16 = row.try_get("kind")?;
    let size: i64 = row.try_get("size")?;
    let raw: Vec<u8> = row.try_get("data")?;
    let compressed: bool = row.try_get("compressed")?;
    let kind = ObjectKind::from_i16(kind)
        .ok_or_else(|| anyhow::anyhow!("未知的 Git 对象类型: {kind}"))?;
    let data = if compressed {
        decompress_blob(&raw, size)?
    } else {
        raw
    };
    Ok(Some(Object { kind, data }))
}

pub async fn require_object(
    conn: &mut PgConnection,
    project_id: Uuid,
    oid: Oid,
) -> Result<Object> {
    get_object(conn, project_id, oid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Git 对象不存在: {oid}"))
}

/// 批量取对象：fetch 热路径按批拉取，替代每个对象一次 `SELECT` 的逐行查询。
/// 返回按数据库顺序的结果，缺失的 oid 不返回，由调用方决定容忍或报错。
pub async fn get_objects(
    conn: &mut PgConnection,
    project_id: Uuid,
    oids: &[Oid],
) -> Result<Vec<(Oid, Object)>> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<Vec<u8>> = oids.iter().map(|oid| oid.as_bytes().to_vec()).collect();
    let rows = sqlx::query(
        "SELECT oid, kind, size, data, compressed
         FROM git_objects WHERE project_id = $1 AND oid = ANY($2::bytea[])",
    )
    .bind(project_id)
    .bind(keys)
    .fetch_all(conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let raw: Vec<u8> = row.try_get("oid")?;
        let oid = Oid::from_slice(&raw)?;
        let kind: i16 = row.try_get("kind")?;
        let size: i64 = row.try_get("size")?;
        let raw: Vec<u8> = row.try_get("data")?;
        let compressed: bool = row.try_get("compressed")?;
        let kind = ObjectKind::from_i16(kind)
            .ok_or_else(|| anyhow::anyhow!("未知的 Git 对象类型: {kind}"))?;
        let data = if compressed {
            decompress_blob(&raw, size)?
        } else {
            raw
        };
        out.push((oid, Object { kind, data }));
    }
    Ok(out)
}

/// 把历史遗留的未压缩对象批量压成 zlib。可重复执行：
/// 已压缩的行、以及压不动的对象都会自然跳过。
///
/// 按 `(project_id, oid)` 游标分页，避免一次把整个对象库读进内存。
/// 返回 (本次压缩数, 压不动跳过数)。
pub async fn compress_existing_objects(pool: &PgPool) -> Result<(usize, usize)> {
    let mut compressed_count = 0usize;
    let mut skipped = 0usize;
    let mut conn = pool.acquire().await?;
    let mut cursor: Option<(Uuid, Vec<u8>)> = None;
    loop {
        let rows = match &cursor {
            Some((project_id, oid)) => {
                sqlx::query(
                    "SELECT project_id, oid, data FROM git_objects
                     WHERE NOT compressed AND (project_id, oid) > ($1, $2)
                     ORDER BY project_id, oid LIMIT 500",
                )
                .bind(project_id)
                .bind(oid)
                .fetch_all(&mut *conn)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT project_id, oid, data FROM git_objects
                     WHERE NOT compressed
                     ORDER BY project_id, oid LIMIT 500",
                )
                .fetch_all(&mut *conn)
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let project_id: Uuid = row.try_get("project_id")?;
            let oid: Vec<u8> = row.try_get("oid")?;
            let data: Vec<u8> = row.try_get("data")?;
            // 游标始终推进：压不动的对象下一轮不会再被选中。
            cursor = Some((project_id, oid.clone()));
            match compress_blob(&data)? {
                Some(packed) => {
                    sqlx::query(
                        "UPDATE git_objects SET data = $3, compressed = true
                         WHERE project_id = $1 AND oid = $2",
                    )
                    .bind(project_id)
                    .bind(&oid)
                    .bind(&packed)
                    .execute(&mut *conn)
                    .await?;
                    compressed_count += 1;
                }
                None => skipped += 1,
            }
        }
    }
    Ok((compressed_count, skipped))
}

pub async fn get_ref(
    conn: &mut PgConnection,
    project_id: Uuid,
    name: &str,
) -> Result<Option<Oid>> {
    let row = sqlx::query("SELECT target FROM git_refs WHERE project_id = $1 AND name = $2")
        .bind(project_id)
        .bind(name)
        .fetch_optional(conn)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let target: Vec<u8> = row.try_get("target")?;
    Ok(Some(Oid::from_slice(&target)?))
}

pub async fn set_ref(
    conn: &mut PgConnection,
    project_id: Uuid,
    name: &str,
    target: Oid,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO git_refs (project_id, name, target, updated_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (project_id, name)
         DO UPDATE SET target = EXCLUDED.target, updated_at = now()",
    )
    .bind(project_id)
    .bind(name)
    .bind(&target.as_bytes()[..])
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn delete_ref(
    conn: &mut PgConnection,
    project_id: Uuid,
    name: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM git_refs WHERE project_id = $1 AND name = $2")
        .bind(project_id)
        .bind(name)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn list_refs(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<Vec<(String, Oid)>> {
    let rows = sqlx::query("SELECT name, target FROM git_refs WHERE project_id = $1 ORDER BY name")
        .bind(project_id)
        .fetch_all(conn)
        .await?;
    let mut refs = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.try_get("name")?;
        let target: Vec<u8> = row.try_get("target")?;
        refs.push((name, Oid::from_slice(&target)?));
    }
    Ok(refs)
}

pub async fn resolve_branch(
    conn: &mut PgConnection,
    project_id: Uuid,
    branch: &str,
) -> Result<Option<Oid>> {
    get_ref(conn, project_id, &format!("refs/heads/{branch}")).await
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_commit_index(
    conn: &mut PgConnection,
    project_id: Uuid,
    oid: Oid,
    parent: Option<Oid>,
    second_parent: Option<Oid>,
    tree: Oid,
    author_id: Option<Uuid>,
    author_name: &str,
    message: &str,
    committed_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO git_commits
            (project_id, oid, parent_oid, second_parent_oid, tree_oid,
             author_id, author_name, message, committed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (project_id, oid) DO NOTHING",
    )
    .bind(project_id)
    .bind(&oid.as_bytes()[..])
    .bind(parent.map(|value| value.as_bytes().to_vec()))
    .bind(second_parent.map(|value| value.as_bytes().to_vec()))
    .bind(&tree.as_bytes()[..])
    .bind(author_id)
    .bind(author_name)
    .bind(message)
    .bind(committed_at)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn insert_commit_path(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
    path: &str,
    change: &str,
    old_path: Option<&str>,
    blob: Option<Oid>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO git_commit_paths
            (project_id, commit_oid, path, change, old_path, blob_oid)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (project_id, commit_oid, path) DO NOTHING",
    )
    .bind(project_id)
    .bind(&commit.as_bytes()[..])
    .bind(path)
    .bind(change)
    .bind(old_path)
    .bind(blob.map(|value| value.as_bytes().to_vec()))
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn commit_time(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
) -> Result<Option<DateTime<Utc>>> {
    let row = sqlx::query("SELECT committed_at FROM git_commits WHERE project_id = $1 AND oid = $2")
        .bind(project_id)
        .bind(&commit.as_bytes()[..])
        .fetch_optional(conn)
        .await?;
    match row {
        Some(row) => Ok(Some(row.try_get("committed_at")?)),
        None => Ok(None),
    }
}

/// 校验命令里传来的旧 OID。
pub fn check_old_oid(expected: Option<Oid>, supplied: Option<&str>) -> Result<()> {
    let supplied = match supplied {
        Some(value) if !value.trim().is_empty() => Some(Oid::from_hex(value)?),
        _ => None,
    };
    if supplied != expected {
        bail!("文件已被其他人修改，请刷新后重试");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_roundtrip_keeps_bytes() {
        // 可压缩的文本：压完必须明显变小，且解压后逐字节一致。
        let data = "line of markdown content\n".repeat(200).into_bytes();
        let packed = compress_blob(&data).unwrap().expect("应被压缩");
        assert!(packed.len() < data.len(), "压缩后应更小");
        assert_eq!(decompress_blob(&packed, data.len() as i64).unwrap(), data);
    }

    #[test]
    fn tiny_blob_is_stored_raw() {
        // 小于阈值不压：zlib 头尾开销会让它更大。
        assert!(compress_blob(b"tiny").unwrap().is_none());
    }

    #[test]
    fn incompressible_blob_is_stored_raw() {
        // 高熵数据压不动时返回 None，避免多存一份更大的副本。
        let mut data = Vec::new();
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..2048 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            data.push((seed & 0xFF) as u8);
        }
        assert!(compress_blob(&data).unwrap().is_none());
    }

    #[test]
    fn size_mismatch_is_rejected() {
        // size 列是原始长度：解压结果对不上说明数据被截断或串了，必须报错而不是静默返回。
        let data = "abcde".repeat(500).into_bytes();
        let packed = compress_blob(&data).unwrap().expect("应被压缩");
        assert!(decompress_blob(&packed, 1).is_err());
        assert!(decompress_blob(&packed, data.len() as i64).is_ok());
    }
}
