use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use uuid::Uuid;

use super::object::{Object, ObjectKind, Oid};

/// 小于这个字节数就不压缩：zlib 的头尾开销约 11 字节，
/// 小对象压完往往更大，反而多一次解压。
const COMPRESS_MIN_BYTES: usize = 256;

/// S3 磁盘对象存储。只有显式设置 `GIT_OBJECT_BACKEND=disk` 才初始化；
/// 未初始化时所有读写路径行为与升级前完全一致，PR 可以零风险回滚。
static DISK: OnceLock<super::disk::DiskStore> = OnceLock::new();

pub fn init_disk_store(root: impl Into<PathBuf>) -> &'static super::disk::DiskStore {
    DISK.get_or_init(|| super::disk::DiskStore::new(root))
}

pub fn disk_store() -> Option<&'static super::disk::DiskStore> {
    DISK.get()
}

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
pub(crate) fn decompress_blob(data: &[u8], size: i64) -> Result<Vec<u8>> {
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

/// 已备好的入库行：OID、原始长度与压缩结果都在事务外算好。
/// 事务里只做一次批量 INSERT，持锁时间不再随对象数线性增长。
pub struct PreparedObject {
    pub oid: Vec<u8>,
    pub kind: i16,
    pub size: i64,
    pub data: Vec<u8>,
    pub compressed: bool,
}

/// 纯 CPU：算 OID + 按需 zlib 压缩，不碰数据库。
/// 批量场景适合整体放进 `spawn_blocking`，避免阻塞 tokio worker。
pub fn prepare_objects(objects: &[Object]) -> Result<Vec<PreparedObject>> {
    let mut out = Vec::with_capacity(objects.len());
    for object in objects {
        let oid = object.oid();
        // 压缩是可选的：压不小就存原样，读取端按 compressed 列判断。
        let (payload, compressed) = match compress_blob(&object.data)? {
            Some(packed) => (packed, true),
            None => (object.data.clone(), false),
        };
        out.push(PreparedObject {
            oid: oid.as_bytes().to_vec(),
            kind: object.kind as i16,
            size: object.data.len() as i64,
            data: payload,
            compressed,
        });
    }
    Ok(out)
}

/// 一次往返批量写入。`ON CONFLICT DO NOTHING` 与逐条 `put_object` 语义一致：
/// 同 OID 已存在时直接复用，不覆盖已有行。
pub async fn insert_prepared(
    conn: &mut PgConnection,
    project_id: Uuid,
    rows: &[PreparedObject],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    // 磁盘权威模式：对象字节先落盘（原子 rename），再进 PG 事务。
    // 顺序不能反——PG 提交成功而磁盘没落稳会产生无法恢复的悬空引用；
    // 反过来只会留下不可达的孤儿 pack，repack 即可清掉。
    // 未启用磁盘时这段完全跳过，行为与升级前一致。
    if let Some(disk) = disk_store() {
        let objects = super::disk::objects_from_prepared(rows)?;
        tokio::task::spawn_blocking(move || disk.write_pack(project_id, &objects, false))
            .await
            .context("写入磁盘 Git 对象失败")??;
    }
    let oids: Vec<Vec<u8>> = rows.iter().map(|r| r.oid.clone()).collect();
    let kinds: Vec<i16> = rows.iter().map(|r| r.kind).collect();
    let sizes: Vec<i64> = rows.iter().map(|r| r.size).collect();
    let datas: Vec<Vec<u8>> = rows.iter().map(|r| r.data.clone()).collect();
    let compressed: Vec<bool> = rows.iter().map(|r| r.compressed).collect();
    sqlx::query(
        "INSERT INTO git_objects (project_id, oid, kind, size, data, compressed)
         SELECT $1, u.oid, u.kind, u.size, u.data, u.compressed
         FROM unnest($2::bytea[], $3::smallint[], $4::bigint[], $5::bytea[], $6::boolean[])
              AS u(oid, kind, size, data, compressed)
         ON CONFLICT (project_id, oid) DO NOTHING",
    )
    .bind(project_id)
    .bind(oids)
    .bind(kinds)
    .bind(sizes)
    .bind(datas)
    .bind(compressed)
    .execute(conn)
    .await?;
    Ok(())
}

/// 写入单个对象并返回 OID。同 OID 已存在时直接复用。
/// 批量写入请用 `prepare_objects` + `insert_prepared`。
pub async fn put_object(conn: &mut PgConnection, project_id: Uuid, object: &Object) -> Result<Oid> {
    let oid = object.oid();
    let rows = prepare_objects(std::slice::from_ref(object))?;
    insert_prepared(conn, project_id, &rows).await?;
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
        return disk_get_one(project_id, oid).await;
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

/// PG 未命中时回退到磁盘 pack。对象不可变、oid 即内容哈希，
/// 所以缓存不需要失效协议，只可能被淘汰。
async fn disk_get_one(project_id: Uuid, oid: Oid) -> Result<Option<Object>> {
    let Some(disk) = disk_store() else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || disk.get(project_id, &oid))
        .await
        .context("读取磁盘 Git 对象失败")?
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

/// 库里一行的原始形态：`data` 可能是已压缩的 zlib 流。
/// fetch 热路径直接把压缩字节写进 pack，只有需要解析 tree/commit 时才解压。
#[derive(Clone)]
pub struct StoredObject {
    pub oid: Oid,
    pub kind: ObjectKind,
    pub size: i64,
    pub data: Vec<u8>,
    pub compressed: bool,
}

impl StoredObject {
    pub fn into_object(self) -> Result<Object> {
        let data = if self.compressed {
            decompress_blob(&self.data, self.size)?
        } else {
            self.data
        };
        Ok(Object {
            kind: self.kind,
            data,
        })
    }
}

/// 批量取对象原始行，不做解压。
pub async fn get_objects_raw(
    conn: &mut PgConnection,
    project_id: Uuid,
    oids: &[Oid],
) -> Result<Vec<StoredObject>> {
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
    let mut found = HashSet::with_capacity(rows.len());
    for row in rows {
        let raw: Vec<u8> = row.try_get("oid")?;
        let oid = Oid::from_slice(&raw)?;
        let kind: i16 = row.try_get("kind")?;
        let kind = ObjectKind::from_i16(kind)
            .ok_or_else(|| anyhow::anyhow!("未知的 Git 对象类型: {kind}"))?;
        found.insert(oid);
        out.push(StoredObject {
            oid,
            kind,
            size: row.try_get("size")?,
            data: row.try_get("data")?,
            compressed: row.try_get("compressed")?,
        });
    }
    // 冷行被 offload 删掉后会在这里 miss。磁盘对象还原后是原文，
    // 以 compressed=false 交给 PackBuilder 重新压缩——正确性优先，
    // 冷路径这点 CPU 换来的是热路径完全不用改。
    if found.len() < oids.len() {
        if let Some(disk) = disk_store() {
            let missing: Vec<Oid> = oids
                .iter()
                .copied()
                .filter(|oid| !found.contains(oid))
                .collect();
            let got = tokio::task::spawn_blocking(move || disk.get_many(project_id, &missing))
                .await
                .context("批量读取磁盘 Git 对象失败")??;
            for (oid, object) in got {
                out.push(StoredObject {
                    oid,
                    kind: object.kind,
                    size: object.data.len() as i64,
                    data: object.data,
                    compressed: false,
                });
            }
        }
    }
    Ok(out)
}

/// 批量取对象：fetch 热路径按批拉取，替代每个对象一次 `SELECT` 的逐行查询。
/// 返回按数据库顺序的结果，缺失的 oid 不返回，由调用方决定容忍或报错。
pub async fn get_objects(
    conn: &mut PgConnection,
    project_id: Uuid,
    oids: &[Oid],
) -> Result<Vec<(Oid, Object)>> {
    let rows = get_objects_raw(conn, project_id, oids).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let oid = row.oid;
        out.push((oid, row.into_object()?));
    }
    Ok(out)
}

/// 一批待更新行的三个 `unnest` 数组，以及压不动的行数。
struct CompressBatch {
    project_ids: Vec<Uuid>,
    oids: Vec<Vec<u8>>,
    payloads: Vec<Vec<u8>>,
    skipped: usize,
}

/// 纯 CPU：对本批原始行做 zlib 压缩，分流出可更新行与压不动行。
/// SQL 已经滤掉小于阈值的行，这里再兜一次 `compress_blob` 的判断。
fn prepare_compress_batch(rows: &[(Uuid, Vec<u8>, Vec<u8>)]) -> Result<CompressBatch> {
    let mut batch = CompressBatch {
        project_ids: Vec::with_capacity(rows.len()),
        oids: Vec::with_capacity(rows.len()),
        payloads: Vec::with_capacity(rows.len()),
        skipped: 0,
    };
    for (project_id, oid, data) in rows {
        match compress_blob(data)? {
            Some(packed) => {
                batch.project_ids.push(*project_id);
                batch.oids.push(oid.clone());
                batch.payloads.push(packed);
            }
            None => batch.skipped += 1,
        }
    }
    Ok(batch)
}

/// 把历史遗留的未压缩对象批量压成 zlib。可重复执行：
/// 已压缩的行、小于阈值的行，以及压不动的对象都会自然跳过。
///
/// 按 `(project_id, oid)` 游标分页，避免一次把整个对象库读进内存；
/// 每批只做一次 `unnest` 批量 UPDATE，不再逐行往返。
/// 返回 (本次压缩数, 压不动跳过数)。
pub async fn compress_existing_objects(pool: &PgPool) -> Result<(usize, usize)> {
    let mut compressed_count = 0usize;
    let mut skipped = 0usize;
    let mut conn = pool.acquire().await?;
    let mut cursor: Option<(Uuid, Vec<u8>)> = None;
    let min_size = COMPRESS_MIN_BYTES as i64;
    loop {
        // 小于阈值的行必然压不动，直接在 SQL 里滤掉；否则重复执行时
        // 每次都要把这些小对象读出来再判一次，规模一大就是固定开销。
        let rows = match &cursor {
            Some((project_id, oid)) => {
                sqlx::query(
                    "SELECT project_id, oid, data FROM git_objects
                     WHERE NOT compressed AND size >= $3
                       AND (project_id, oid) > ($1, $2)
                     ORDER BY project_id, oid LIMIT 500",
                )
                .bind(project_id)
                .bind(oid)
                .bind(min_size)
                .fetch_all(&mut *conn)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT project_id, oid, data FROM git_objects
                     WHERE NOT compressed AND size >= $1
                     ORDER BY project_id, oid LIMIT 500",
                )
                .bind(min_size)
                .fetch_all(&mut *conn)
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        let mut raw = Vec::with_capacity(rows.len());
        for row in &rows {
            let project_id: Uuid = row.try_get("project_id")?;
            let oid: Vec<u8> = row.try_get("oid")?;
            let data: Vec<u8> = row.try_get("data")?;
            // 游标始终推进：压不动的对象在本次执行里不会再被选中。
            cursor = Some((project_id, oid.clone()));
            raw.push((project_id, oid, data));
        }
        let batch = prepare_compress_batch(&raw)?;
        skipped += batch.skipped;
        if batch.oids.is_empty() {
            continue;
        }
        // 单条语句原子更新整批；`NOT g.compressed` 避免覆盖并发写入。
        // rows_affected 才是真正落库的条数，不用预估数记账。
        let result = sqlx::query(
            "UPDATE git_objects AS g
             SET data = u.data, compressed = true
             FROM unnest($1::uuid[], $2::bytea[], $3::bytea[])
                  AS u(project_id, oid, data)
             WHERE g.project_id = u.project_id AND g.oid = u.oid
               AND NOT g.compressed",
        )
        .bind(batch.project_ids)
        .bind(batch.oids)
        .bind(batch.payloads)
        .execute(&mut *conn)
        .await?;
        compressed_count += result.rows_affected() as usize;
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
    // ref 变了，该项目的全量 clone pack 缓存与磁盘全量快照标记都要作废。
    super::pack_cache::invalidate_project(project_id);
    if let Some(disk) = disk_store() {
        disk.clear_full(project_id)?;
    }
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
    super::pack_cache::invalidate_project(project_id);
    if let Some(disk) = disk_store() {
        disk.clear_full(project_id)?;
    }
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
    fn prepare_objects_keeps_oid_size_and_roundtrip() {
        let objects = vec![
            Object::blob(b"tiny".to_vec()),
            Object::blob("可压缩文本行\n".repeat(300).into_bytes()),
            Object::commit(
                b"tree 0000000000000000000000000000000000000000\n"
                    .repeat(20)
                    .to_vec(),
            ),
        ];
        let oids: Vec<Oid> = objects.iter().map(|o| o.oid()).collect();
        let rows = prepare_objects(&objects).unwrap();
        assert_eq!(rows.len(), objects.len());
        for (i, row) in rows.iter().enumerate() {
            // OID 必须由原始内容算出，不能由压缩后的字节算。
            assert_eq!(row.oid, oids[i].as_bytes().to_vec());
            assert_eq!(row.kind, objects[i].kind as i16);
            assert_eq!(row.size, objects[i].data.len() as i64);
            let data = if row.compressed {
                decompress_blob(&row.data, row.size).unwrap()
            } else {
                row.data.clone()
            };
            assert_eq!(data, objects[i].data);
        }
        assert!(!rows[0].compressed, "小对象存原样");
        assert!(rows[1].compressed, "大文本应压缩");
        assert!(prepare_objects(&[]).unwrap().is_empty(), "空批次不产生行");
    }

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

    fn high_entropy_bytes(len: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(len);
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..len {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            data.push((seed & 0xFF) as u8);
        }
        data
    }

    #[test]
    fn incompressible_blob_is_stored_raw() {
        // 高熵数据压不动时返回 None，避免多存一份更大的副本。
        assert!(compress_blob(&high_entropy_bytes(2048)).unwrap().is_none());
    }

    #[test]
    fn compress_batch_splits_rows_and_counts_skipped() {
        // SQL 已经滤掉小于阈值的行，这里同时验证纯 CPU 分流的兜底判断。
        let id = Uuid::nil();
        let compressible = "line of markdown content\n".repeat(200).into_bytes();
        let rows = vec![
            (id, vec![1u8; 20], b"tiny".to_vec()),
            (id, vec![2u8; 20], compressible.clone()),
            (id, vec![3u8; 20], high_entropy_bytes(2048)),
        ];
        let batch = prepare_compress_batch(&rows).unwrap();
        assert_eq!(batch.project_ids.len(), 1);
        assert_eq!(batch.oids, vec![vec![2u8; 20]]);
        assert_eq!(batch.payloads.len(), 1);
        assert_eq!(batch.skipped, 2, "小对象与高熵对象都应跳过");
        assert_eq!(
            decompress_blob(&batch.payloads[0], compressible.len() as i64).unwrap(),
            compressible
        );
        assert!(prepare_compress_batch(&[]).unwrap().oids.is_empty());
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
