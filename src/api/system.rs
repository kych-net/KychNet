//! 管理端资源监控：只读的系统与存储统计。
//!
//! 全部指标按需现读（/proc、statvfs、PG），不缓存不采样——管理端手动
//! 打开才查，10 秒轮询的频率对一台个人站服务器没有压力。

use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use std::path::Path;

use super::*;

#[derive(Serialize)]
pub(crate) struct DiskUsage {
    pub label: String,
    pub path: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Serialize)]
pub(crate) struct SystemResources {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub cpu_cores: usize,
    pub uptime_secs: u64,
    pub mem_total: u64,
    pub mem_available: u64,
    /// serve 进程自身的常驻内存。
    pub process_rss: u64,
    pub disks: Vec<DiskUsage>,
    /// PG 当前库占用。
    pub db_bytes: u64,
    /// git_objects 行数与原始字节总量（压缩前后大小不同，这里只看行体）。
    pub git_objects: i64,
    pub git_objects_bytes: i64,
    /// LFS 二进制对象占用的磁盘字节。
    pub lfs_bytes: u64,
}

pub(crate) async fn system_resources(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<SystemResources>> {
    current_admin(&state, &session).await?;

    let mut disks = vec![
        ("内容库".into(), state.content_root.clone()),
        ("构建产物".into(), state.build_ctl.out_dir().clone()),
        ("Git LFS".into(), state.lfs_root.clone()),
    ];
    if let Some(disk) = crate::git::store::disk_store() {
        disks.push(("Git 磁盘对象".into(), disk.root().to_path_buf()));
    }

    // 文件系统遍历放进阻塞线程：git 磁盘对象目录可能有几千个文件。
    let lfs_root = state.lfs_root.clone();
    let disks_for_fs = disks.clone();
    let (disks, lfs_bytes) = tokio::task::spawn_blocking(move || {
        let disks = disks_for_fs
            .into_iter()
            .map(|(label, path)| {
                let (used_bytes, total_bytes) =
                    fs_usage(&path).unwrap_or((0, 0));
                DiskUsage {
                    label,
                    path: path.display().to_string(),
                    used_bytes,
                    total_bytes,
                }
            })
            .collect();
        (disks, dir_size(&lfs_root).unwrap_or(0))
    })
    .await
    .context("读取文件系统统计失败")?;

    let row = sqlx::query(
        "SELECT pg_database_size(current_database())::bigint AS db_bytes,
                (SELECT count(*) FROM git_objects)::bigint AS git_objects,
                (SELECT COALESCE(sum(octet_length(data)), 0)::bigint FROM git_objects)
                    AS git_objects_bytes",
    )
    .fetch_one(&state.pool)
    .await
    .context("查询数据库统计失败")?;
    let db_bytes: i64 = row.try_get("db_bytes")?;
    let git_objects: i64 = row.try_get("git_objects")?;
    let git_objects_bytes: i64 = row.try_get("git_objects_bytes")?;

    let load = proc_loadavg().unwrap_or((0.0, 0.0, 0.0));
    let mem = proc_meminfo().unwrap_or((0, 0));

    Ok(Json(SystemResources {
        load1: load.0,
        load5: load.1,
        load15: load.2,
        cpu_cores: std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1),
        uptime_secs: proc_uptime().unwrap_or(0),
        mem_total: mem.0,
        mem_available: mem.1,
        process_rss: proc_rss().unwrap_or(0),
        disks,
        db_bytes: db_bytes.max(0) as u64,
        git_objects,
        git_objects_bytes: git_objects_bytes.max(0),
        lfs_bytes,
    }))
}

/// statvfs 读挂载点用量。路径不存在或跨文件系统读失败时返回 Err，
/// 调用方按 0 处理，监控页不因一个目录挂掉而整页 500。
fn fs_usage(path: &Path) -> anyhow::Result<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .context("路径包含非法字节")?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY：c_path 指向合法的 NUL 结尾字符串，stat 指向足够容纳 statvfs 的缓冲。
    let code = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if code != 0 {
        anyhow::bail!("statvfs 失败: {}", path.display());
    }
    // f_blocks/f_bavail 在 Linux 是 u64、macOS 是 u32，统一 cast 保证两平台可编译。
    let block = stat.f_frsize as u64;
    let total = stat.f_blocks as u64 * block;
    // f_bavail 是非 root 用户可用块；这里展示的是"还能放多少东西"，
    // 用 avail 而不是 free（free 含 root 保留块，会显得没满却写不进）。
    let available = stat.f_bavail as u64 * block;
    Ok((total - available, total))
}

/// 递归统计目录字节。符号链接不跟随，避免把站点产物里的环读死。
fn dir_size(root: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else {
                total += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            }
        }
    }
    Ok(total)
}

/// /proc/loadavg："0.12 0.34 0.56 1/234 5678"。
fn proc_loadavg() -> anyhow::Result<(f64, f64, f64)> {
    let text = std::fs::read_to_string("/proc/loadavg")?;
    let mut values = text.split_whitespace();
    let load1 = values.next().context("loadavg 缺 load1")?.parse()?;
    let load5 = values.next().context("loadavg 缺 load5")?.parse()?;
    let load15 = values.next().context("loadavg 缺 load15")?.parse()?;
    Ok((load1, load5, load15))
}

/// /proc/uptime：秒数部分。
fn proc_uptime() -> anyhow::Result<u64> {
    let text = std::fs::read_to_string("/proc/uptime")?;
    let secs: f64 = text
        .split_whitespace()
        .next()
        .context("uptime 为空")?
        .parse()?;
    Ok(secs as u64)
}

/// /proc/meminfo：返回 (MemTotal, MemAvailable)，单位统一成字节。
fn proc_meminfo() -> anyhow::Result<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo")?;
    let mut fields: HashMap<&str, u64> = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split(':');
        let Some(key) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        // "16384 kB" → 16384 * 1024
        let number = value
            .trim()
            .split_whitespace()
            .next()
            .and_then(|number| number.parse::<u64>().ok());
        if let Some(number) = number {
            fields.insert(key.trim(), number * 1024);
        }
    }
    let total = fields
        .get("MemTotal")
        .copied()
        .context("meminfo 缺 MemTotal")?;
    let available = fields
        .get("MemAvailable")
        .or_else(|| fields.get("MemFree"))
        .copied()
        .context("meminfo 缺 MemAvailable")?;
    Ok((total, available))
}

/// /proc/self/status 的 VmRSS。
fn proc_rss() -> anyhow::Result<u64> {
    let text = std::fs::read_to_string("/proc/self/status")?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let number = rest
                .trim()
                .split_whitespace()
                .next()
                .and_then(|number| number.parse::<u64>().ok())
                .context("VmRSS 解析失败")?;
            return Ok(number * 1024);
        }
    }
    anyhow::bail!("status 缺 VmRSS")
}
