//! ★ 模式 B 核心：主服务代读 PG 里的 Git 对象，打包成 tar.gz 下发给 KychRunner。
//!
//! tar 布局（与 KychRunner `src/snapshot.rs` 严格对应）：
//!   code/<仓库内相对路径>      完整代码快照，来自 workflow_runs.commit_oid
//!   definitions/<文件名>       默认分支 workflow_dir 下的 yml/yaml，来自 def_commit_oid
//!
//! 零新增依赖：项目已有 flate2，USTAR 头手写约 60 行即可，不需要引入 tar crate。
//! LFS pointer 在这里被还原成真实字节，执行器不需要挂载 LFS_ROOT。

use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::io::Write;
use uuid::Uuid;

use super::runner_api::WorkflowState;
use crate::git::object::Oid;
use crate::git::tree;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotHeaders {
    pub commit_oid: String,
    pub def_commit_oid: String,
    pub files: usize,
    pub bytes: u64,
}

pub struct Snapshot {
    pub gz: Vec<u8>,
    pub headers: SnapshotHeaders,
}

#[derive(Debug, Clone)]
pub struct RunRef {
    pub project_id: Uuid,
    pub commit_oid: Vec<u8>,
    pub def_commit_oid: Vec<u8>,
    pub workflow_dir: String,
}

/// 生成快照。命中 `(project_id, commit_oid, def_commit_oid)` 内容寻址缓存时直接返回，
/// Git 对象不可变，缓存可长期复用。
pub async fn build_snapshot(state: &WorkflowState, run: &RunRef) -> Result<Snapshot> {
    let key = format!(
        "{}-{}-{}",
        run.project_id,
        hex::encode(&run.commit_oid),
        hex::encode(&run.def_commit_oid)
    );
    let cache = state.cfg.snapshot_cache.join(format!("{key}.tar.gz"));
    let meta = state.cfg.snapshot_cache.join(format!("{key}.meta.json"));
    if let (Ok(gz), Ok(text)) = (
        tokio::fs::read(&cache).await,
        tokio::fs::read_to_string(&meta).await,
    ) {
        if let Ok(headers) = serde_json::from_str::<SnapshotHeaders>(&text) {
            return Ok(Snapshot { gz, headers });
        }
    }

    let mut conn = state.pool.acquire().await.context("获取数据库连接失败")?;
    let code = collect_files(
        &mut conn,
        run.project_id,
        &run.commit_oid,
        &state.cfg.lfs_root,
    )
    .await
    .context("读取代码快照失败")?;
    let defs_all = collect_files(
        &mut conn,
        run.project_id,
        &run.def_commit_oid,
        &state.cfg.lfs_root,
    )
    .await
    .context("读取定义快照失败")?;
    drop(conn);

    let prefix = format!("{}/", run.workflow_dir.trim_end_matches('/'));
    let mut definitions: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, data) in &defs_all {
        if !path.starts_with(&prefix) {
            continue;
        }
        let lower = path.to_ascii_lowercase();
        if !lower.ends_with(".yml") && !lower.ends_with(".yaml") {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        validate_path_segment(name)?;
        definitions.push((format!("definitions/{name}"), data.clone()));
    }

    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(code.len() + definitions.len());
    let mut total: u64 = 0;
    for (path, data) in code {
        for seg in path.split('/') {
            validate_path_segment(seg)?;
        }
        total = total.saturating_add(data.len() as u64);
        entries.push((format!("code/{path}"), data));
    }
    for (path, data) in definitions {
        total = total.saturating_add(data.len() as u64);
        entries.push((path, data));
    }
    if total > state.cfg.max_snapshot_bytes {
        bail!(
            "快照 {} 字节超过限额 {} 字节",
            total,
            state.cfg.max_snapshot_bytes
        );
    }

    let files = entries.len();
    let gz = write_tar_gz(&entries).context("打包 tar.gz 失败")?;
    let headers = SnapshotHeaders {
        commit_oid: hex::encode(&run.commit_oid),
        def_commit_oid: hex::encode(&run.def_commit_oid),
        files,
        bytes: total,
    };

    // 缓存写入失败不影响本次响应。
    if tokio::fs::create_dir_all(&state.cfg.snapshot_cache).await.is_ok() {
        let _ = tokio::fs::write(&cache, &gz).await;
        let _ = tokio::fs::write(&meta, serde_json::to_string(&headers).unwrap_or_default()).await;
    }
    Ok(Snapshot { gz, headers })
}

/// 路径段复检：tree::read_files 已校验过，这里再做一次纵深防御。
fn validate_path_segment(seg: &str) -> Result<()> {
    if seg.is_empty()
        || seg == "."
        || seg == ".."
        || seg.contains('\\')
        || seg.contains('\0')
        || seg.eq_ignore_ascii_case(".git")
        || seg.chars().any(char::is_control)
    {
        bail!("快照路径包含非法段：{seg:?}");
    }
    Ok(())
}

/// 读某个 commit 的完整文件树，并把 LFS pointer 还原成真实字节。
async fn collect_files(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
    commit_oid: &[u8],
    lfs_root: &std::path::Path,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let oid = Oid::from_slice(commit_oid).context("非法 commit oid")?;
    let mut files = tree::read_files(conn, project_id, oid).await?;
    for (path, data) in files.iter_mut() {
        if let Some(p) = parse_lfs_pointer(data) {
            let file = lfs_root
                .join(project_id.to_string())
                .join(&p.oid[..2])
                .join(&p.oid[2..4])
                .join(&p.oid);
            let real = tokio::fs::read(&file)
                .await
                .with_context(|| format!("LFS 对象缺失：{}（引用自 {path}）", file.display()))?;
            if real.len() as u64 != p.size {
                bail!("LFS 对象 {} 大小不符", p.oid);
            }
            *data = real;
        }
    }
    Ok(files)
}

struct LfsPointer {
    oid: String,
    size: u64,
}

fn parse_lfs_pointer(data: &[u8]) -> Option<LfsPointer> {
    if data.len() > 512 || !data.starts_with(b"version https://git-lfs.github.com/spec/v1") {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let mut oid = None;
    let mut size = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("oid sha256:") {
            let v = v.trim();
            if v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit()) {
                oid = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("size ") {
            size = v.trim().parse::<u64>().ok();
        }
    }
    Some(LfsPointer { oid: oid?, size: size? })
}

/// 手写 USTAR：512 字节头 + 数据按 512 补齐 + 两个零块，外层 gzip。
fn write_tar_gz(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let enc = GzEncoder::new(Vec::new(), Compression::default());
    let mut w = enc;
    for (path, data) in entries {
        let header = tar_header(path, data.len() as u64)?;
        w.write_all(&header)?;
        w.write_all(data)?;
        let pad = (512 - data.len() % 512) % 512;
        if pad > 0 {
            w.write_all(&vec![0u8; pad])?;
        }
    }
    w.write_all(&[0u8; 1024])?;
    Ok(w.finish()?)
}

fn tar_header(path: &str, size: u64) -> Result<[u8; 512]> {
    let mut h = [0u8; 512];
    if path.len() <= 100 {
        h[..path.len()].copy_from_slice(path.as_bytes());
    } else {
        // USTAR prefix：在最后一个让 name <= 100 的 '/' 处切分。
        let cut = path
            .char_indices()
            .filter(|(i, c)| *c == '/' && path.len() - i - 1 <= 100)
            .map(|(i, _)| i)
            .next_back()
            .context("tar 路径过长且无法切分 prefix")?;
        let prefix = &path[..cut];
        let name = &path[cut + 1..];
        if prefix.len() > 155 || name.len() > 100 {
            bail!("tar 路径过长：{path}");
        }
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    }
    write_oct(&mut h, 100, 8, 0o644)?; // mode
    write_oct(&mut h, 108, 8, 0)?; // uid
    write_oct(&mut h, 116, 8, 0)?; // gid
    write_oct(&mut h, 124, 12, size)?;
    write_oct(&mut h, 136, 12, 0)?; // mtime
    for b in &mut h[148..156] {
        *b = b' ';
    }
    h[156] = b'0'; // typeflag: 普通文件
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u64 = h.iter().map(|&b| b as u64).sum();
    let chk = format!("{:06o}\0 ", sum);
    h[148..156].copy_from_slice(chk.as_bytes());
    Ok(h)
}

fn write_oct(h: &mut [u8; 512], off: usize, width: usize, value: u64) -> Result<()> {
    let s = format!("{:0width$o}\0", value, width = width - 1);
    if s.len() > width {
        bail!("tar 数值字段溢出");
    }
    h[off..off + s.len()].copy_from_slice(s.as_bytes());
    Ok(())
}

/// 供 `runner_api::snapshot` 从数据库装载 run 信息。
pub async fn load_run(pool: &PgPool, run_id: Uuid) -> Result<RunRef> {
    let row: (Uuid, Vec<u8>, Vec<u8>, String) = sqlx::query_as(
        r#"SELECT r.project_id, r.commit_oid, r.def_commit_oid, p.workflow_dir
             FROM workflow_runs r JOIN projects p ON p.id = r.project_id
            WHERE r.id = $1"#,
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .context("run 不存在")?;
    Ok(RunRef {
        project_id: row.0,
        commit_oid: row.1,
        def_commit_oid: row.2,
        workflow_dir: row.3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tar_header_checksum_is_valid_ustar() {
        let h = tar_header("code/README.md", 5).unwrap();
        assert_eq!(&h[257..263], b"ustar\0");
        assert_eq!(h[156], b'0');
        // USTAR checksum = 6 位八进制数字 + NUL + 空格；只解析前 6 字节。
        assert_eq!(&h[154..156], b"\0 ");
        let stored = std::str::from_utf8(&h[148..154]).unwrap();
        let expected = u64::from_str_radix(stored, 8).unwrap();
        let mut copy = h;
        for b in &mut copy[148..156] {
            *b = b' ';
        }
        let sum: u64 = copy.iter().map(|&b| b as u64).sum();
        assert_eq!(sum, expected);
    }

    #[test]
    fn lfs_pointer_is_parsed() {
        let p = b"version https://git-lfs.github.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 42\n";
        let parsed = parse_lfs_pointer(p).unwrap();
        assert_eq!(parsed.size, 42);
        assert_eq!(parsed.oid.len(), 64);
        assert!(parse_lfs_pointer(b"plain text").is_none());
    }

    #[test]
    fn rejects_unsafe_segments() {
        assert!(validate_path_segment("..").is_err());
        assert!(validate_path_segment(".git").is_err());
        assert!(validate_path_segment("a/b").is_ok()); // 调用方按 '/' 切分后传单段
        assert!(validate_path_segment("site.yml").is_ok());
    }

    #[test]
    fn tar_gz_roundtrip() {
        let entries = vec![("code/a.txt".to_string(), b"hello".to_vec())];
        let gz = write_tar_gz(&entries).unwrap();
        assert_eq!(&gz[..2], &[0x1f, 0x8b]);
    }
}
