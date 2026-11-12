//! S3 的磁盘 Git 对象存储：**标准 packfile v2 + `.idx` v2 + OFS_DELTA**。
//!
//! ```text
//! $GIT_ROOT/<project_id>/objects/pack/pack-<sha1>.{pack,idx}
//! $GIT_ROOT/<project_id>/FULL          # 记录「全量快照」的 pack 文件名
//! ```
//!
//! 与 loose object 一对象一文件不同，这里把一组对象编成**一个** pack：
//! 对象体是标准 zlib 流，跨版本冗余用 OFS_DELTA 差掉，随机访问靠 `.idx` v2
//! 的 fanout 表 + 二分定位 offset。全量 clone 时 pack 字节可以原样发出去。
//!
//! 格式严格遵循 Git 规范，`git verify-pack` / `git cat-file` / `git fsck`
//! 能直接校验和读取这里产出的文件——把权威源从 PG 挪到磁盘后，能用独立
//! 工具验证数据完整性是底线要求。
//!
//! ## 一致性
//!
//! 磁盘是对象字节的权威，PG 的 `git_objects` 退化为可丢弃的热缓存。启用
//! 后所有写入**双写**（PG 行 + 磁盘 pack），默认读 PG；运维执行
//! `db offload --drop-cold` 删掉冷行后，读路径自动回退到磁盘。
//! 写入顺序永远是「磁盘 pack 原子落盘 → PG 事务提交 refs/元数据」：
//! PG 失败只会留下不可达的孤儿 pack（无害，repack 可清），反过来会
//! 产生悬空引用（无法恢复）。
//!
//! ## 为什么必须 repack
//!
//! `write_pack` 产出的每个 pack 都**自包含**：OFS_DELTA 只能引用本 pack
//! 内的偏移。增量 push 逐个追加小 pack 时，历史版本链被切断，压缩率会
//! 持续退化。`repack` 把所有 pack 合并成一个，重新做全局 delta 选择，
//! 才能回到单 pack 的体积。

use anyhow::{bail, Context, Result};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha1::{Digest, Sha1};
use sqlx::{PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

use super::object::{commit_tree, hash_object, Object, ObjectKind, Oid, Tree, TreeEntry};
use super::proto;
use super::store::{self, PreparedObject};
use crate::project::Project;

/// delta 选基准时的滑动窗口与最大链深。
const WINDOW: usize = 50;
const MAX_DEPTH: usize = 50;
/// 块签名 / delta 匹配的粒度。
const BLOCK: usize = 16;
const IDX_MAGIC: [u8; 4] = [0xff, 0x74, 0x4f, 0x63]; // "\377tOc"
const IDX_VERSION: u32 = 2;
const PACK_VERSION: u32 = 2;
const TYPE_OFS_DELTA: u8 = 6;
const TYPE_REF_DELTA: u8 = 7;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn kind_code(kind: ObjectKind) -> u8 {
    // ObjectKind 的 repr(i16) 恰好等于 Git pack 的 type code。
    kind as i16 as u8
}

fn kind_from_code(code: u8) -> Result<ObjectKind> {
    match code {
        1 => Ok(ObjectKind::Commit),
        2 => Ok(ObjectKind::Tree),
        3 => Ok(ObjectKind::Blob),
        4 => Ok(ObjectKind::Tag),
        _ => bail!("pack 对象类型非法: {code}"),
    }
}

/// 待写入磁盘的对象：`data` 是**解压后的对象原文**。
#[derive(Clone)]
pub struct PackedObject {
    pub oid: Oid,
    pub kind: ObjectKind,
    pub data: Vec<u8>,
}

impl PackedObject {
    pub fn from_object(object: &Object) -> Self {
        Self {
            oid: object.oid(),
            kind: object.kind,
            data: object.data.clone(),
        }
    }
}

/// 把 push / 导入路径产出的入库行还原成对象原文。
/// 库内 `data` 可能是 zlib 流，pack 里必须是原文（再统一压缩）。
pub fn objects_from_prepared(rows: &[PreparedObject]) -> Result<Vec<PackedObject>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let kind = ObjectKind::from_i16(row.kind)
            .ok_or_else(|| anyhow::anyhow!("未知的 Git 对象类型: {}", row.kind))?;
        let data = if row.compressed {
            store::decompress_blob(&row.data, row.size)?
        } else {
            row.data.clone()
        };
        out.push(PackedObject {
            oid: Oid::from_slice(&row.oid)?,
            kind,
            data,
        });
    }
    Ok(out)
}

fn block_hash(chunk: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in chunk {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 非重叠块的哈希签名（排序去重），用于快速估算两个对象的内容重合度。
fn signature(data: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + BLOCK <= data.len() {
        out.push(block_hash(&data[pos..pos + BLOCK]));
        pos += BLOCK;
    }
    out.sort_unstable();
    out.dedup();
    // 大对象签名截断，避免评分阶段 O(n²)；只用于挑候选，不影响正确性。
    out.truncate(512);
    out
}

fn common_blocks(a: &[u64], b: &[u64]) -> usize {
    let (mut i, mut j, mut count) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                count += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    count
}

fn write_varint(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn flush_insert(out: &mut Vec<u8>, pending: &mut Vec<u8>) {
    for chunk in pending.chunks(127) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    pending.clear();
}

/// Git delta copy 指令：0x80 + offset 字节掩码(bit0..3) + size 字节掩码(bit4..6)，
/// 后跟相应非零字节。参数顺序必须与 `proto::apply_delta` 的读取顺序一致。
fn write_copy(out: &mut Vec<u8>, offset: usize, size: usize) {
    let mut op = 0x80u8;
    let mut args = Vec::with_capacity(7);
    for bit in 0..4 {
        let byte = ((offset >> (8 * bit)) & 0xff) as u8;
        if byte != 0 {
            op |= 1 << bit;
            args.push(byte);
        }
    }
    // size == 0x10000 用「一个 size 字节都不写」表示。
    let encoded_size = if size == 0x10000 { 0 } else { size };
    for bit in 0..3 {
        let byte = ((encoded_size >> (8 * bit)) & 0xff) as u8;
        if byte != 0 {
            op |= 1 << (4 + bit);
            args.push(byte);
        }
    }
    out.push(op);
    out.extend_from_slice(&args);
}

/// 生成标准 Git delta 指令流：varint(base_len) + varint(result_len) + 指令。
/// 用块哈希索引在 base 里找最长匹配，其余字节走 insert。不求最优压缩率，
/// 但保证 `proto::apply_delta` 能无损还原（有单测覆盖）。
fn make_delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    let mut index: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut at = 0;
    while at + BLOCK <= base.len() {
        index
            .entry(block_hash(&base[at..at + BLOCK]))
            .or_default()
            .push(at);
        at += BLOCK;
    }

    let mut out = Vec::new();
    write_varint(&mut out, base.len());
    write_varint(&mut out, target.len());
    let mut pos = 0;
    let mut pending: Vec<u8> = Vec::new();
    while pos < target.len() {
        let mut best: Option<(usize, usize)> = None;
        if pos + BLOCK <= target.len() {
            let block = &target[pos..pos + BLOCK];
            if let Some(candidates) = index.get(&block_hash(block)) {
                // 只看最近的少量候选，避免大文件上退化为 O(n²)。
                for &base_pos in candidates.iter().rev().take(32) {
                    if &base[base_pos..base_pos + BLOCK] != block {
                        continue;
                    }
                    let mut length = BLOCK;
                    while pos + length < target.len()
                        && base_pos + length < base.len()
                        && base[base_pos + length] == target[pos + length]
                    {
                        length += 1;
                    }
                    if best.map_or(true, |(_, best_len)| length > best_len) {
                        best = Some((base_pos, length));
                    }
                }
            }
        }
        if let Some((base_pos, length)) = best {
            flush_insert(&mut out, &mut pending);
            // copy 的 size 只占 3 字节，单次最长 0xffffff；0x10000 有简写。
            let length = length.min(0x10000);
            write_copy(&mut out, base_pos, length);
            pos += length;
        } else {
            pending.push(target[pos]);
            pos += 1;
            if pending.len() >= 127 {
                flush_insert(&mut out, &mut pending);
            }
        }
    }
    flush_insert(&mut out, &mut pending);
    out
}

fn write_object_header(out: &mut Vec<u8>, type_code: u8, size: usize) {
    let mut byte = (type_code << 4) | (size as u8 & 0x0f);
    let mut rest = size >> 4;
    if rest > 0 {
        byte |= 0x80;
    }
    out.push(byte);
    while rest > 0 {
        let mut next = (rest & 0x7f) as u8;
        rest >>= 7;
        if rest > 0 {
            next |= 0x80;
        }
        out.push(next);
    }
}

/// OFS_DELTA 的负偏移变长编码，与 `proto::read_ofs_offset` 互为逆运算。
fn encode_ofs(offset: usize) -> Vec<u8> {
    let mut n = offset;
    let mut buf = vec![(n & 0x7f) as u8];
    loop {
        n >>= 7;
        if n == 0 {
            break;
        }
        // Git 的编码每多一个字节都先对剩余高位减 1，解码端对应
        // `((offset + 1) << 7) | next`，两边必须严格互逆。
        n -= 1;
        buf.push(0x80 | ((n & 0x7f) as u8));
    }
    buf.reverse();
    buf
}

/// CRC-32 (IEEE 802.3)，`.idx` 里存的是**压缩后数据**的 CRC。
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

struct EncodedPack {
    pack: Vec<u8>,
    idx: Vec<u8>,
    checksum: [u8; 20],
}

struct PackEntry {
    object: PackedObject,
    /// 选中的 delta 基准在 `ordered` 里的下标；None 表示完整对象。
    base: Option<usize>,
    depth: usize,
    payload: Vec<u8>,
}

/// 把一组对象编成自包含 pack + 配套 `.idx` v2。
fn encode_pack(objects: &[PackedObject]) -> Result<EncodedPack> {
    if objects.is_empty() {
        bail!("没有可写入的 Git 对象");
    }
    // 同类型、尺寸相近的对象排在一起，delta 命中率最高；大对象在前当 base。
    let mut ordered: Vec<PackedObject> = objects.to_vec();
    ordered.sort_by(|a, b| {
        (a.kind as i16)
            .cmp(&(b.kind as i16))
            .then(b.data.len().cmp(&a.data.len()))
            .then(a.oid.cmp(&b.oid))
    });
    ordered.dedup_by(|a, b| a.oid == b.oid);
    let count = ordered.len();

    let signatures: Vec<Vec<u64>> = ordered.iter().map(|o| signature(&o.data)).collect();
    let mut entries: Vec<PackEntry> = Vec::with_capacity(count);
    for index in 0..count {
        let object = &ordered[index];
        let mut chosen: Option<(usize, usize)> = None; // (共同块数, 候选下标)
        let start = index.saturating_sub(WINDOW);
        for candidate in start..index {
            let base = &entries[candidate];
            if base.object.kind != object.kind || base.depth >= MAX_DEPTH {
                continue;
            }
            let score = common_blocks(&signatures[candidate], &signatures[index]);
            // 至少两个 16 字节块重合才值得试，否则 delta 多半比原文还大。
            if score >= 2 && chosen.map_or(true, |(best, _)| score > best) {
                chosen = Some((score, candidate));
            }
        }
        let mut best: Option<(usize, Vec<u8>, usize)> = None; // (base_idx, delta, depth)
        if let Some((_, candidate)) = chosen {
            let delta = make_delta(&entries[candidate].object.data, &object.data);
            // 只有明显变小才用 delta，避免为了几字节付出回溯代价。
            if delta.len() + 5 < object.data.len() * 9 / 10 {
                best = Some((candidate, delta, entries[candidate].depth + 1));
            }
        }
        match best {
            Some((base, payload, depth)) => entries.push(PackEntry {
                object: object.clone(),
                base: Some(base),
                depth,
                payload,
            }),
            None => entries.push(PackEntry {
                object: object.clone(),
                base: None,
                depth: 0,
                payload: object.data.clone(),
            }),
        }
    }

    let mut pack = Vec::new();
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&PACK_VERSION.to_be_bytes());
    pack.extend_from_slice(&(count as u32).to_be_bytes());
    let mut offsets: Vec<u32> = Vec::with_capacity(count);
    let mut ends: Vec<usize> = Vec::with_capacity(count);
    let mut crcs: Vec<u32> = Vec::with_capacity(count);
    for entry in &entries {
        let offset = pack.len();
        if offset > i32::MAX as usize {
            bail!("单个 pack 超过 2 GiB，`.idx` v2 需要 large-offset 表");
        }
        offsets.push(offset as u32);
        let type_code = if entry.base.is_some() {
            TYPE_OFS_DELTA
        } else {
            kind_code(entry.object.kind)
        };
        write_object_header(&mut pack, type_code, entry.payload.len());
        if let Some(base_index) = entry.base {
            let distance = offset - offsets[base_index] as usize;
            pack.extend_from_slice(&encode_ofs(distance));
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&entry.payload)?;
        let compressed = encoder.finish().context("压缩 pack 对象失败")?;
        crcs.push(0);
        pack.extend_from_slice(&compressed);
        ends.push(pack.len());
    }
    // `.idx` 里的 CRC 覆盖「对象头 + 压缩数据」整段，不是只有 zlib 流。
    for i in 0..count {
        crcs[i] = crc32(&pack[offsets[i] as usize..ends[i]]);
    }
    let mut hasher = Sha1::new();
    hasher.update(&pack);
    let checksum: [u8; 20] = hasher.finalize().into();
    pack.extend_from_slice(&checksum);

    // `.idx` v2：按 oid 字典序排列三个数组，fanout 是累计计数。
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&i| ordered[i].oid);
    let mut fanout = [0u32; 256];
    for &i in &order {
        fanout[ordered[i].oid.as_bytes()[0] as usize] += 1;
    }
    let mut running = 0u32;
    for value in fanout.iter_mut() {
        running += *value;
        *value = running;
    }
    let mut idx = Vec::new();
    idx.extend_from_slice(&IDX_MAGIC);
    idx.extend_from_slice(&IDX_VERSION.to_be_bytes());
    for value in fanout {
        idx.extend_from_slice(&value.to_be_bytes());
    }
    for &i in &order {
        idx.extend_from_slice(ordered[i].oid.as_bytes());
    }
    for &i in &order {
        idx.extend_from_slice(&crcs[i].to_be_bytes());
    }
    for &i in &order {
        idx.extend_from_slice(&offsets[i].to_be_bytes());
    }
    idx.extend_from_slice(&checksum);
    let idx_hash = Sha1::digest(&idx);
    idx.extend_from_slice(&idx_hash);

    Ok(EncodedPack {
        pack,
        idx,
        checksum,
    })
}

#[derive(Clone)]
struct Idx {
    oids: Vec<Oid>,
    offsets: Vec<u32>,
    crcs: Vec<u32>,
}

impl Idx {
    fn lookup(&self, oid: &Oid) -> Option<u32> {
        self.oids.binary_search(oid).ok().map(|i| self.offsets[i])
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 + 1024 + 40 {
            bail!("`.idx` 文件过短");
        }
        if bytes[..4] != IDX_MAGIC || u32::from_be_bytes(bytes[4..8].try_into()?) != IDX_VERSION {
            bail!("`.idx` magic / 版本不匹配");
        }
        let mut fanout = [0u32; 256];
        for (i, slot) in fanout.iter_mut().enumerate() {
            let at = 8 + i * 4;
            *slot = u32::from_be_bytes(bytes[at..at + 4].try_into()?);
        }
        let count = fanout[255] as usize;
        let oids_at = 8 + 1024;
        let crcs_at = oids_at + count * 20;
        let offsets_at = crcs_at + count * 4;
        let tail_at = offsets_at + count * 4;
        if bytes.len() < tail_at + 40 {
            bail!("`.idx` 长度与 fanout 记录的对象数不符");
        }
        let expected = Sha1::digest(&bytes[..tail_at + 20]);
        if expected.as_slice() != &bytes[tail_at + 20..tail_at + 40] {
            bail!("`.idx` 校验和错误");
        }
        let mut oids = Vec::with_capacity(count);
        for i in 0..count {
            let at = oids_at + i * 20;
            oids.push(Oid::from_slice(&bytes[at..at + 20])?);
        }
        let mut crcs = Vec::with_capacity(count);
        for i in 0..count {
            let at = crcs_at + i * 4;
            crcs.push(u32::from_be_bytes(bytes[at..at + 4].try_into()?));
        }
        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            let at = offsets_at + i * 4;
            offsets.push(u32::from_be_bytes(bytes[at..at + 4].try_into()?));
        }
        Ok(Self {
            oids,
            offsets,
            crcs,
        })
    }
}

/// 一个已加载的 pack：字节 + 索引。内容寻址的不可变文件，进程内缓存即可。
struct PackFile {
    bytes: Arc<Vec<u8>>,
    idx: Arc<Idx>,
}

impl PackFile {
    /// 从指定偏移还原对象，沿 OFS_DELTA 链递归回溯基准。
    fn object_at(&self, offset: u32, depth: usize) -> Result<(ObjectKind, Vec<u8>)> {
        if depth > MAX_DEPTH + 2 {
            bail!("pack 中的 delta 链过深");
        }
        let at = offset as usize;
        let (type_code, size, header_end) = proto::read_object_header(&self.bytes, at)?;
        let mut pos = header_end;
        let mut base_offset: Option<usize> = None;
        if type_code == TYPE_OFS_DELTA {
            let (base, next) = proto::read_ofs_offset(&self.bytes, pos, at)?;
            base_offset = Some(base);
            pos = next;
        } else if type_code == TYPE_REF_DELTA {
            // 本实现只产出 OFS_DELTA；外部 pack 若含 REF_DELTA 需要跨 pack
            // 查 oid → offset，这里明确拒绝，避免静默返回错误对象。
            bail!("暂不支持跨 pack 的 REF_DELTA 对象");
        }
        let (payload, _) = proto::inflate(&self.bytes[pos..], size)?;
        match type_code {
            1..=4 => Ok((kind_from_code(type_code)?, payload)),
            TYPE_OFS_DELTA => {
                let base = base_offset.context("OFS_DELTA 缺少基准偏移")?;
                let (kind, base_data) = self.object_at(base as u32, depth + 1)?;
                Ok((kind, proto::apply_delta(&base_data, &payload)?))
            }
            _ => bail!("pack 对象类型非法: {type_code}"),
        }
    }
}

fn bytes_cache() -> &'static Mutex<HashMap<PathBuf, Arc<Vec<u8>>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<u8>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn idx_cache() -> &'static Mutex<HashMap<PathBuf, Arc<Idx>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Idx>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// pack 文件被删除后同步清掉两个缓存里的条目。pack 名由内容校验和决定，
/// 不会复用，不清就是慢性泄漏。
fn evict_caches(pack_path: &Path) {
    let idx_path = pack_path.with_extension("idx");
    bytes_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(pack_path);
    idx_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&idx_path);
}

/// 临时文件 → fsync → 原子 rename。rename 后引用可见时字节必须已落稳。
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("目标路径没有父目录")?;
    fs::create_dir_all(parent).with_context(|| format!("创建目录失败: {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".tmp-{}-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut file =
            File::create(&tmp).with_context(|| format!("创建临时文件失败: {}", tmp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            // 另一个进程刚好写完同内容：视为成功。
            if path.is_file() {
                Ok(())
            } else {
                Err(error).with_context(|| format!("写入文件失败: {}", path.display()))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiskStore {
    root: PathBuf,
}

impl DiskStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 用 UUID 而不是 slug：项目改名不会让磁盘对象失联。
    fn project_dir(&self, project_id: Uuid) -> PathBuf {
        self.root.join(project_id.to_string())
    }

    fn pack_dir(&self, project_id: Uuid) -> PathBuf {
        self.project_dir(project_id).join("objects").join("pack")
    }

    fn full_marker(&self, project_id: Uuid) -> PathBuf {
        self.project_dir(project_id).join("FULL")
    }

    /// 写入一组对象为一个自包含 pack。pack 名由内容校验和决定，重复写幂等。
    /// `mark_full` 表示这个 pack 是该项目的完整快照，可用于全量 clone 直发。
    pub fn write_pack(
        &self,
        project_id: Uuid,
        objects: &[PackedObject],
        mark_full: bool,
    ) -> Result<PathBuf> {
        let encoded = encode_pack(objects)?;
        let dir = self.pack_dir(project_id);
        let name = format!("pack-{}", hex::encode(encoded.checksum));
        let pack_path = dir.join(format!("{name}.pack"));
        let idx_path = dir.join(format!("{name}.idx"));
        // idx 后写：只有完整 pair 存在的文件才会被扫描到，不会读到半截 pack。
        if !pack_path.is_file() {
            atomic_write(&pack_path, &encoded.pack)?;
        }
        if !idx_path.is_file() {
            atomic_write(&idx_path, &encoded.idx)?;
        }
        if mark_full {
            atomic_write(&self.full_marker(project_id), name.as_bytes())?;
        } else {
            self.clear_full(project_id)?;
        }
        Ok(pack_path)
    }

    /// ref 一变，全量快照就不再成立。
    pub fn clear_full(&self, project_id: Uuid) -> Result<()> {
        match fs::remove_file(self.full_marker(project_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// 全量 clone 可直接发送的 pack 路径。
    pub fn full_pack(&self, project_id: Uuid) -> Option<PathBuf> {
        let name = fs::read_to_string(self.full_marker(project_id)).ok()?;
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            return None;
        }
        let pack = self.pack_dir(project_id).join(format!("{name}.pack"));
        let idx = self.pack_dir(project_id).join(format!("{name}.idx"));
        (pack.is_file() && idx.is_file()).then_some(pack)
    }

    /// 列出所有完整的 pack pair（有 `.pack` 没 `.idx` 的视为未完成写入）。
    fn packs(&self, project_id: Uuid) -> Result<Vec<(PathBuf, Arc<PackFile>)>> {
        let dir = self.pack_dir(project_id);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".idx") {
                if dir.join(format!("{stem}.pack")).is_file() {
                    names.push(stem.to_string());
                }
            }
        }
        names.sort();
        let mut out = Vec::with_capacity(names.len());
        for stem in names {
            let idx_path = dir.join(format!("{stem}.idx"));
            let pack_path = dir.join(format!("{stem}.pack"));
            let idx = {
                let mut cache = idx_cache().lock().unwrap_or_else(|e| e.into_inner());
                match cache.get(&idx_path) {
                    Some(hit) => hit.clone(),
                    None => {
                        let parsed = Arc::new(Idx::parse(&fs::read(&idx_path)?)?);
                        cache.insert(idx_path.clone(), parsed.clone());
                        parsed
                    }
                }
            };
            let bytes = {
                let mut cache = bytes_cache().lock().unwrap_or_else(|e| e.into_inner());
                match cache.get(&pack_path) {
                    Some(hit) => hit.clone(),
                    None => {
                        let loaded = Arc::new(fs::read(&pack_path)?);
                        cache.insert(pack_path.clone(), loaded.clone());
                        loaded
                    }
                }
            };
            out.push((pack_path, Arc::new(PackFile { bytes, idx })));
        }
        Ok(out)
    }

    pub fn get(&self, project_id: Uuid, oid: &Oid) -> Result<Option<Object>> {
        for (_, pack) in self.packs(project_id)? {
            let Some(offset) = pack.idx.lookup(oid) else {
                continue;
            };
            let (kind, data) = pack.object_at(offset, 0)?;
            // 内容寻址：重算 OID 必须匹配，损坏的字节不能静默返回。
            if &hash_object(kind, &data) != oid {
                bail!("磁盘 Git 对象校验失败: {oid}");
            }
            return Ok(Some(Object { kind, data }));
        }
        Ok(None)
    }

    pub fn get_many(&self, project_id: Uuid, oids: &[Oid]) -> Result<Vec<(Oid, Object)>> {
        let packs = self.packs(project_id)?;
        let mut out = Vec::with_capacity(oids.len());
        for oid in oids {
            for (_, pack) in &packs {
                let Some(offset) = pack.idx.lookup(oid) else {
                    continue;
                };
                let (kind, data) = pack.object_at(offset, 0)?;
                if &hash_object(kind, &data) != oid {
                    bail!("磁盘 Git 对象校验失败: {oid}");
                }
                out.push((*oid, Object { kind, data }));
                break;
            }
        }
        Ok(out)
    }

    /// 枚举磁盘上的全部 oid，供 repack / gc 使用。
    pub fn list(&self, project_id: Uuid) -> Result<Vec<Oid>> {
        let mut seen = HashSet::new();
        for (_, pack) in self.packs(project_id)? {
            for oid in pack.idx.oids.iter() {
                seen.insert(*oid);
            }
        }
        let mut out: Vec<Oid> = seen.into_iter().collect();
        out.sort();
        Ok(out)
    }

    /// 把所有 pack 合并成一个，重新做全局 delta 选择，并清掉旧 pack。
    /// 这是维持磁盘体积的必需运维动作：增量 push 追加的小 pack 无法跨
    /// pack 复用历史版本做 delta。
    ///
    /// `keep` 传入时只保留可达对象（供 gc 使用）；为 None 时保留全部。
    ///
    /// 单个对象损坏只跳过并告警，不让 repack 整体失败——坏对象本来就不
    /// 可恢复，阻断 repack 反而让其余健康对象的体积问题也一起恶化。
    pub fn repack(&self, project_id: Uuid, keep: Option<&HashSet<Oid>>) -> Result<usize> {
        let mut oids = self.list(project_id)?;
        if let Some(keep) = keep {
            oids.retain(|oid| keep.contains(oid));
        }
        let mut objects: Vec<(Oid, Object)> = Vec::with_capacity(oids.len());
        let mut skipped = 0usize;
        for oid in &oids {
            match self.get(project_id, oid) {
                Ok(Some(object)) => objects.push((*oid, object)),
                Ok(None) => {}
                Err(error) => {
                    skipped += 1;
                    tracing::warn!("repack 跳过损坏对象 {oid}: {error:#}");
                }
            }
        }
        if objects.is_empty() {
            return Ok(0);
        }
        let packed: Vec<PackedObject> = objects
            .into_iter()
            .map(|(oid, object)| PackedObject {
                oid,
                kind: object.kind,
                data: object.data,
            })
            .collect();
        let new_pack = self.write_pack(project_id, &packed, true)?;
        let new_stem = new_pack
            .file_stem()
            .context("pack 文件名异常")?
            .to_string_lossy()
            .into_owned();
        let dir = self.pack_dir(project_id);
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".pack")
                .or_else(|| name.strip_suffix(".idx"))
                .unwrap_or("");
            if stem.starts_with("pack-") && stem != new_stem {
                let _ = fs::remove_file(entry.path());
                if name.ends_with(".pack") {
                    // 文件没了，缓存条目也不能留：pack 名是校验和，不会复用。
                    evict_caches(&entry.path());
                }
            }
        }
        if skipped > 0 {
            tracing::warn!("repack({project_id}) 完成，{} 个损坏对象被跳过", skipped);
        }
        Ok(packed.len())
    }
}

/// 把 PG 里的对象导出成一个全量 pack，可选删掉冷行让 PG 退化为热缓存。
///
/// 热集 = 默认分支 tip 的**单棵快照**（commit + 递归 tree + blob），不追
/// 父提交。按「所有 refs 可达」定义会把长期分支的大批对象全算成热数据，
/// 缓存就失去意义；同时模板浅克隆复制的正是 tip 快照，留在 PG 里也让它
/// 那套纯 SQL `INSERT ... SELECT` 继续成立。
pub struct OffloadStats {
    pub objects_written: usize,
    pub hot_kept: usize,
    pub cold_dropped: usize,
}

pub async fn offload(
    pool: &PgPool,
    project: &Project,
    disk: &DiskStore,
    drop_cold: bool,
) -> Result<OffloadStats> {
    let hot = tip_snapshot(pool, project).await?;
    let mut all: Vec<PackedObject> = Vec::new();
    let mut cursor: Vec<u8> = Vec::new();
    loop {
        let rows = sqlx::query(
            "SELECT oid, kind, size, data, compressed FROM git_objects
             WHERE project_id = $1 AND oid > $2 ORDER BY oid LIMIT 500",
        )
        .bind(project.id)
        .bind(&cursor)
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        let mut prepared = Vec::with_capacity(rows.len());
        for row in &rows {
            let oid: Vec<u8> = row.try_get("oid")?;
            cursor = oid.clone();
            prepared.push(PreparedObject {
                oid,
                kind: row.try_get("kind")?,
                size: row.try_get("size")?,
                data: row.try_get("data")?,
                compressed: row.try_get("compressed")?,
            });
        }
        let mut batch = objects_from_prepared(&prepared)?;
        all.append(&mut batch);
    }
    // 磁盘里已有的对象（增量 push 写的 pack）也要一起收进全量 pack。
    for (oid, object) in disk.get_many(project.id, &disk.list(project.id)?)? {
        if !all.iter().any(|item| item.oid == oid) {
            all.push(PackedObject {
                oid,
                kind: object.kind,
                data: object.data,
            });
        }
    }
    if all.is_empty() {
        bail!("项目里没有可导出的 Git 对象");
    }
    let written = all.len();
    // 在 `all` 被 move 进阻塞任务之前记下本轮导出的 oid 清单，删冷行时
    // 只允许删这份清单里的行。
    let exported: Vec<Vec<u8>> = all.iter().map(|object| object.oid.as_bytes().to_vec()).collect();
    let disk = disk.clone();
    let project_id = project.id;
    tokio::task::spawn_blocking(move || disk.repack_from(project_id, all))
        .await
        .context("导出 Git 对象任务失败")??;

    if !drop_cold || hot.is_empty() {
        return Ok(OffloadStats {
            objects_written: written,
            hot_kept: hot.len(),
            cold_dropped: 0,
        });
    }
    let hot_oids: Vec<Vec<u8>> = hot.iter().map(|oid| oid.as_bytes().to_vec()).collect();
    // 只删这一轮已经写进磁盘 pack 的行。热集快照之后新 push 进来的对象
    // 不在 `exported` 里，留着它们——ref 已指向这些提交，删了就是悬空
    // 引用；磁盘 pack 没有的行下一轮扫描自然覆盖。
    let dropped = sqlx::query(
        "DELETE FROM git_objects
         WHERE project_id = $1 AND NOT (oid = ANY($2::bytea[])) AND oid = ANY($3::bytea[])",
    )
    .bind(project.id)
    .bind(hot_oids)
    .bind(exported)
    .execute(pool)
    .await?
    .rows_affected() as usize;
    // ref 未变，但缓存内容变了，全量 clone 的组包结果需要重算。
    super::pack_cache::invalidate_project(project.id);
    Ok(OffloadStats {
        objects_written: written,
        hot_kept: hot.len(),
        cold_dropped: dropped,
    })
}

/// 自动 offload 的一轮扫描结果，后台任务打印用。
pub struct SweepStats {
    pub project: String,
    pub stats: OffloadStats,
}

/// 扫描 PG 里对象数超过 `min_objects` 的未归档项目，逐个执行
/// offload（含删冷行）。大仓库的早期历史因此自动进磁盘，PG 退化为
/// 默认分支 tip 的热缓存；小仓库留在 PG，不值得动。
///
/// 串行执行：offload 本身是大 IO 操作，并行只会互相抢带宽。
/// 单个项目失败不中断整轮，下轮重试。
pub async fn auto_offload_sweep(
    pool: &PgPool,
    disk: &DiskStore,
    min_objects: usize,
) -> Result<Vec<SweepStats>> {
    let rows = sqlx::query(
        "SELECT p.slug FROM projects p
         JOIN git_objects g ON g.project_id = p.id
         WHERE p.archived = false
         GROUP BY p.id, p.slug
         HAVING count(*) >= $1
         ORDER BY count(*) DESC",
    )
    .bind(min_objects as i64)
    .fetch_all(pool)
    .await?;
    let mut swept = Vec::new();
    for row in rows {
        let slug: String = row.try_get("slug")?;
        let Some(project) = crate::project::find_project_by_slug(pool, &slug).await? else {
            continue;
        };
        match offload(pool, &project, disk, true).await {
            Ok(stats) => {
                tracing::info!(
                    %slug,
                    objects = stats.objects_written,
                    hot = stats.hot_kept,
                    dropped = stats.cold_dropped,
                    "自动 offload 完成"
                );
                swept.push(SweepStats { project: slug, stats });
            }
            Err(error) => {
                tracing::warn!(%slug, %error, "自动 offload 失败，等待下轮重试");
            }
        }
    }
    Ok(swept)
}

impl DiskStore {
    /// 直接用给定对象集重建单个全量 pack（offload 专用）。
    fn repack_from(&self, project_id: Uuid, objects: Vec<PackedObject>) -> Result<()> {
        let new_pack = self.write_pack(project_id, &objects, true)?;
        let new_stem = new_pack
            .file_stem()
            .context("pack 文件名异常")?
            .to_string_lossy()
            .into_owned();
        let dir = self.pack_dir(project_id);
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".pack")
                .or_else(|| name.strip_suffix(".idx"))
                .unwrap_or("");
            if stem.starts_with("pack-") && stem != new_stem {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
}

/// 默认分支 tip 的工作树快照（不沿父提交回溯）。
async fn tip_snapshot(pool: &PgPool, project: &Project) -> Result<HashSet<Oid>> {
    let mut conn = pool.acquire().await?;
    let ref_name = format!("refs/heads/{}", project.default_branch);
    let Some(tip) = store::get_ref(&mut conn, project.id, &ref_name).await? else {
        return Ok(HashSet::new());
    };
    let mut hot = HashSet::new();
    let mut stack = vec![tip];
    while let Some(oid) = stack.pop() {
        if !hot.insert(oid) {
            continue;
        }
        let Some(object) = store::get_object(&mut conn, project.id, oid).await? else {
            continue;
        };
        match object.kind {
            ObjectKind::Commit => stack.push(commit_tree(&object.data)?),
            ObjectKind::Tree => {
                let tree = Tree::parse(&object.data)?;
                for entry in tree.entries.values() {
                    match entry {
                        TreeEntry::Blob(child) | TreeEntry::Dir(child) => stack.push(*child),
                        TreeEntry::Gitlink(_) => {}
                    }
                }
            }
            ObjectKind::Tag | ObjectKind::Blob => {}
        }
    }
    Ok(hot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp_store(tag: &str) -> (DiskStore, PathBuf, Uuid) {
        let root = std::env::temp_dir().join(format!(
            "kychnet-pack-test-{tag}-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        (DiskStore::new(root.clone()), root, Uuid::new_v4())
    }

    /// 顺序走一遍 pack，读出每个对象的 type code（含 delta）。
    fn pack_type_codes(pack: &[u8]) -> Vec<u8> {
        let count = u32::from_be_bytes(pack[8..12].try_into().unwrap()) as usize;
        let mut pos = 12;
        let mut out = Vec::new();
        for _ in 0..count {
            let (type_code, size, mut next) = proto::read_object_header(pack, pos).unwrap();
            out.push(type_code);
            if type_code == TYPE_OFS_DELTA {
                (_, next) = proto::read_ofs_offset(pack, next, pos).unwrap();
            }
            let (_, consumed) = proto::inflate(&pack[next..], size).unwrap();
            pos = next + consumed;
        }
        out
    }

    fn similar_blobs() -> Vec<PackedObject> {
        let base = "fn main() {\n    println!(\"hello\");\n}\n// line\n".repeat(20);
        let mut versions = Vec::new();
        for i in 0..6 {
            let text = format!("{base}// revision {i}\n");
            let data = text.into_bytes();
            versions.push(PackedObject {
                oid: hash_object(ObjectKind::Blob, &data),
                kind: ObjectKind::Blob,
                data,
            });
        }
        versions
    }

    #[test]
    fn delta_bytes_match_git_format() {
        let base = b"hello delta world\n".repeat(40);
        let mut target = base.clone();
        target.extend_from_slice(b"a small change\n");
        let delta = make_delta(&base, &target);
        assert_eq!(proto::apply_delta(&base, &delta).unwrap(), target);
        // copy 指令跨 128 字节偏移，验证变长参数编码。
        let mut big = vec![b'x'; 300];
        big.extend_from_slice(&base);
        let delta = make_delta(&base, &big);
        assert_eq!(proto::apply_delta(&base, &delta).unwrap(), big);
    }

    #[test]
    fn ofs_encoding_roundtrips() {
        for offset in [1usize, 2, 127, 128, 129, 1000, 16_384, 1_000_000] {
            let encoded = encode_ofs(offset);
            let mut data = encoded.clone();
            data.extend_from_slice(&[0u8; 4]);
            let (decoded, _) = proto::read_ofs_offset(&data, 0, offset).unwrap();
            assert_eq!(decoded, 0, "offset {offset} 应回退到本例的基准偏移 0");
        }
    }

    #[test]
    fn pack_is_readable_by_git() {
        let (store, root, project) = temp_store("git-compat");
        let objects = similar_blobs();
        let pack_path = store.write_pack(project, &objects, true).unwrap();
        let idx_path = pack_path.with_extension("idx");

        // git 自己的校验：CRC、pack checksum、delta 链都会一并检查。
        let verify = Command::new("git")
            .args(["verify-pack", "-v"])
            .arg(&idx_path)
            .output()
            .expect("需要 git 可执行文件");
        assert!(
            verify.status.success(),
            "git verify-pack 失败: {}",
            String::from_utf8_lossy(&verify.stderr)
        );

        // 在标准 bare 仓库布局里用 git cat-file 逐个读回。
        let git_dir = root.join("bare.git");
        Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&git_dir)
            .status()
            .unwrap();
        let dest = git_dir.join("objects").join("pack");
        fs::create_dir_all(&dest).unwrap();
        fs::copy(&pack_path, dest.join(pack_path.file_name().unwrap())).unwrap();
        fs::copy(&idx_path, dest.join(idx_path.file_name().unwrap())).unwrap();
        for object in &objects {
            let out = Command::new("git")
                .arg(format!("--git-dir={}", git_dir.display()))
                .args(["cat-file", "blob"])
                .arg(object.oid.to_hex())
                .output()
                .unwrap();
            assert!(out.status.success(), "git cat-file 读不到 {}", object.oid);
            assert_eq!(out.stdout, object.data);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delta_objects_roundtrip_through_store() {
        let (store, root, project) = temp_store("roundtrip");
        let objects = similar_blobs();
        let pack_path = store.write_pack(project, &objects, true).unwrap();
        let pack_bytes = fs::read(&pack_path).unwrap();
        let codes = pack_type_codes(&pack_bytes);
        assert!(
            codes.iter().filter(|&&code| code == TYPE_OFS_DELTA).count() >= 4,
            "6 个相似 blob 里至少应有 4 个编成 OFS_DELTA: {codes:?}"
        );
        let raw_zlib: usize = objects
            .iter()
            .map(|o| {
                let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
                enc.write_all(&o.data).unwrap();
                enc.finish().unwrap().len()
            })
            .sum();
        assert!(
            pack_bytes.len() < raw_zlib,
            "delta pack ({}) 应小于逐个 zlib 之和 ({raw_zlib})",
            pack_bytes.len()
        );
        for object in &objects {
            let got = store.get(project, &object.oid).unwrap().unwrap();
            assert_eq!(got.kind, object.kind);
            assert_eq!(got.data, object.data);
        }
        assert_eq!(store.list(project).unwrap().len(), objects.len());
        assert_eq!(store.full_pack(project), Some(pack_path));
        store.clear_full(project).unwrap();
        assert_eq!(store.full_pack(project), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn all_object_kinds_roundtrip() {
        let (store, root, project) = temp_store("kinds");
        let raw = vec![
            (ObjectKind::Blob, b"hello\n".to_vec()),
            (ObjectKind::Tree, vec![1, 2, 3]),
            (
                ObjectKind::Commit,
                b"tree 0000000000000000000000000000000000000000\n".to_vec(),
            ),
            (
                ObjectKind::Tag,
                b"object 0000000000000000000000000000000000000000\n".to_vec(),
            ),
        ];
        let objects: Vec<PackedObject> = raw
            .into_iter()
            .map(|(kind, data)| PackedObject {
                oid: hash_object(kind, &data),
                kind,
                data,
            })
            .collect();
        store.write_pack(project, &objects, false).unwrap();
        let oids: Vec<Oid> = objects.iter().map(|o| o.oid).collect();
        let got = store.get_many(project, &oids).unwrap();
        assert_eq!(got.len(), objects.len());
        for (oid, object) in got {
            let expected = objects.iter().find(|o| o.oid == oid).unwrap();
            assert_eq!(object.kind, expected.kind);
            assert_eq!(object.data, expected.data);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repack_merges_packs_and_keeps_objects() {
        let (store, root, project) = temp_store("repack");
        let objects = similar_blobs();
        // 故意分成两个自包含 pack，制造「增量 push」的形态。
        store.write_pack(project, &objects[..3], false).unwrap();
        store.write_pack(project, &objects[3..], false).unwrap();
        assert_eq!(store.list(project).unwrap().len(), objects.len());
        assert_eq!(store.repack(project, None).unwrap(), objects.len());
        let dir = store.pack_dir(project);
        let pairs = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".idx"))
            .count();
        assert_eq!(pairs, 1, "repack 后应只剩一个 pack");
        assert!(store.full_pack(project).is_some());
        for object in &objects {
            assert_eq!(
                store.get(project, &object.oid).unwrap().unwrap().data,
                object.data
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_bytes_are_rejected() {
        let (store, root, project) = temp_store("corrupt");
        let object = Object::blob(b"a longer blob that gets compressed\n".repeat(20));
        let packed = PackedObject::from_object(&object);
        let path = store.write_pack(project, &[packed.clone()], false).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        // 破坏对象体中部，不能影响 pack 头。
        let at = bytes.len() / 2;
        bytes[at] ^= 0xff;
        fs::write(&path, bytes).unwrap();
        idx_cache().lock().unwrap().clear();
        bytes_cache().lock().unwrap().clear();
        assert!(store.get(project, &packed.oid).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
