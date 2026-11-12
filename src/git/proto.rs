use anyhow::{bail, Result};
use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::io::Write;

use super::object::{hash_object, ObjectKind, Oid};

/// 客户端请求解析失败（畸形 pkt-line、非法 OID 等）的标记类型。
/// HTTP 层据此把错误映射成 400，而不是当成服务端故障返回 500。
#[derive(Debug, thiserror::Error)]
#[error("非法的 Git 请求: {0}")]
pub struct MalformedRequest(pub String);

fn malformed(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(MalformedRequest(error.to_string()))
}

/// 编码一条 pkt-line。长度包含 4 字节长度前缀本身。
pub fn pkt(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend_from_slice(data);
    out
}

pub fn pkt_str(data: &str) -> Vec<u8> {
    pkt(data.as_bytes())
}

pub fn flush() -> Vec<u8> {
    b"0000".to_vec()
}

/// protocol v2 的 delim-pkt，用来分隔命令/能力段与参数段。
pub fn delim() -> Vec<u8> {
    b"0001".to_vec()
}

/// 读取一条 pkt-line 头，返回 `(长度, 头占用字节)`。
fn read_header(data: &[u8], pos: usize) -> Result<usize> {
    if pos + 4 > data.len() {
        bail!("pkt-line 数据被截断");
    }
    let header = std::str::from_utf8(&data[pos..pos + 4])
        .map_err(|_| anyhow::anyhow!("pkt-line 长度不是 ASCII"))?;
    usize::from_str_radix(header, 16).map_err(|_| anyhow::anyhow!("pkt-line 长度非法: {header}"))
}

/// protocol v2 的命令请求。
///
/// 第一段是 `command=...` 与客户端能力，delim 之后是命令参数，flush 结束。
pub struct V2Request {
    pub command: String,
    pub args: Vec<String>,
}

pub fn parse_v2_request(data: &[u8]) -> Result<V2Request> {
    parse_v2_request_inner(data).map_err(malformed)
}

fn parse_v2_request_inner(data: &[u8]) -> Result<V2Request> {
    let mut command = String::new();
    let mut args = Vec::new();
    let mut in_args = false;
    let mut pos = 0usize;
    loop {
        let length = read_header(data, pos)?;
        match length {
            0 => break,
            1 => {
                in_args = true;
                pos += 4;
            }
            2 => break,
            3 => bail!("非法的 pkt-line 长度"),
            _ => {
                if length < 4 || pos + length > data.len() {
                    bail!("pkt-line 长度超出数据范围");
                }
                let payload = &data[pos + 4..pos + length];
                let text = String::from_utf8_lossy(payload)
                    .trim_end_matches('\n')
                    .to_string();
                if !in_args {
                    if let Some(value) = text.strip_prefix("command=") {
                        command = value.to_string();
                    }
                } else if !text.is_empty() {
                    args.push(text);
                }
                pos += length;
            }
        }
    }
    Ok(V2Request { command, args })
}

/// v0 upload-pack 请求里的协商参数。
#[derive(Default)]
pub struct UploadRequest {
    pub wants: Vec<Oid>,
    pub haves: Vec<Oid>,
    pub done: bool,
    /// `deepen <n>`，只处理非相对深度。
    pub deepen: Option<u32>,
    pub deepen_relative: bool,
    /// 客户端已有的浅边界。
    pub shallow: Vec<Oid>,
    /// 目前只支持 `filter blob:none`。
    pub filter_blob_none: bool,
}

pub fn parse_upload_request(data: &[u8]) -> Result<UploadRequest> {
    parse_upload_request_inner(data).map_err(malformed)
}

fn parse_upload_request_inner(data: &[u8]) -> Result<UploadRequest> {
    let mut request = UploadRequest::default();
    // upload 请求由多个 flush 分隔的段组成：want 段、可选的 have 段、
    // 以及最后的 `done` 段。`done` 在首个 flush 之后，所以这里不能
    // 像 parse_pkt_lines 那样遇到 flush 就停止，否则协商轮会被误判成
    // done 轮（或反过来），导致 packfile 发早/发丢。
    let mut pos = 0usize;
    while pos < data.len() {
        if pos + 4 > data.len() {
            bail!("pkt-line 数据被截断");
        }
        let header = std::str::from_utf8(&data[pos..pos + 4])
            .map_err(|_| anyhow::anyhow!("pkt-line 长度不是 ASCII"))?;
        let length = usize::from_str_radix(header, 16)
            .map_err(|_| anyhow::anyhow!("pkt-line 长度非法: {header}"))?;
        match length {
            0 => pos += 4,
            1 | 2 => pos += 4,
            3 => bail!("非法的 pkt-line 长度"),
            _ => {
                if length < 4 || pos + length > data.len() {
                    bail!("pkt-line 长度超出数据范围");
                }
                let text = String::from_utf8_lossy(&data[pos + 4..pos + length]);
                let text = text.trim_end_matches('\n');
                if let Some(rest) = text.strip_prefix("want ") {
                    let hex = rest.split(' ').next().unwrap_or_default();
                    request.wants.push(Oid::from_hex(hex)?);
                    // git 把 deepen-relative 内联在首个 want 行的能力列表里,
                    // 不一定发独立的 `deepen-relative` 行。
                    if rest.split(' ').any(|token| token == "deepen-relative") {
                        request.deepen_relative = true;
                    }
                } else if let Some(rest) = text.strip_prefix("have ") {
                    request
                        .haves
                        .push(Oid::from_hex(rest.split(' ').next().unwrap_or_default())?);
                } else if let Some(rest) = text.strip_prefix("shallow ") {
                    request
                        .shallow
                        .push(Oid::from_hex(rest.split(' ').next().unwrap_or_default())?);
                } else if let Some(rest) = text.strip_prefix("deepen ") {
                    request.deepen = rest.trim().parse().ok();
                } else if text == "deepen-relative" {
                    request.deepen_relative = true;
                } else if text == "done" {
                    request.done = true;
                } else if text.starts_with("filter blob:none") {
                    request.filter_blob_none = true;
                }
                pos += length;
            }
        }
    }
    Ok(request)
}

/// v0 receive-pack 请求：命令列表、可选的推送选项、以及随后的 pack。
pub struct ReceiveRequest {
    pub commands: Vec<(Oid, Oid, String)>,
    /// 第一条命令里 `\0` 之后的能力列表，例如 `atomic`、`push-options`。
    pub capabilities: Vec<String>,
    pub options: Vec<String>,
    pub pack: Vec<u8>,
}

pub fn parse_receive_request(data: &[u8]) -> Result<ReceiveRequest> {
    parse_receive_request_inner(data).map_err(malformed)
}

fn parse_receive_request_inner(data: &[u8]) -> Result<ReceiveRequest> {
    let (lines, mut consumed) = parse_pkt_lines(data)?;
    let mut commands = Vec::new();
    let mut capabilities = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\n');
        if text.is_empty() {
            continue;
        }
        let (head, caps) = match text.split_once('\0') {
            Some((head, caps)) => (head, Some(caps)),
            None => (text, None),
        };
        if index == 0 {
            if let Some(caps) = caps {
                capabilities.extend(
                    caps.split_whitespace().map(|item| item.to_string()),
                );
            }
        }
        let mut parts = head.split(' ');
        let old = Oid::from_hex(parts.next().unwrap_or_default())?;
        let new = Oid::from_hex(parts.next().unwrap_or_default())?;
        let name = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("推送命令缺少引用名"))?;
        commands.push((old, new, name.to_string()));
    }

    // 客户端启用 push-options 时，会在命令列表的 flush 之后再发一段
    // pkt-line 选项并以 flush 结束；pack 以 "PACK" 开头，很容易区分。
    let mut options = Vec::new();
    if let Some(rest) = data.get(consumed..) {
        if !rest.is_empty() && !rest.starts_with(b"PACK") {
            let (option_lines, used) = parse_pkt_lines(rest)?;
            for line in option_lines {
                let text = String::from_utf8_lossy(&line)
                    .trim_end_matches('\n')
                    .to_string();
                if let Some(value) = text.strip_prefix("push-option ") {
                    options.push(value.to_string());
                }
            }
            consumed += used;
        }
    }

    Ok(ReceiveRequest {
        commands,
        capabilities,
        options,
        pack: data[consumed..].to_vec(),
    })
}

/// 解析一串 pkt-line，遇到 flush-pkt 停止。
///
/// 返回 `(lines, consumed)`，`consumed` 是 flush-pkt 之后的偏移，
/// receive-pack 用它来定位随后的 pack 数据。
pub fn parse_pkt_lines(data: &[u8]) -> Result<(Vec<Vec<u8>>, usize)> {
    let mut lines = Vec::new();
    let mut pos = 0usize;
    loop {
        if pos + 4 > data.len() {
            bail!("pkt-line 数据被截断");
        }
        let header = std::str::from_utf8(&data[pos..pos + 4])
            .map_err(|_| anyhow::anyhow!("pkt-line 长度不是 ASCII"))?;
        let length = usize::from_str_radix(header, 16)
            .map_err(|_| anyhow::anyhow!("pkt-line 长度非法: {header}"))?;
        match length {
            0 => {
                pos += 4;
                break;
            }
            1 | 2 => {
                // delim / response-end，这里没有使用场景，直接跳过。
                pos += 4;
            }
            3 => bail!("非法的 pkt-line 长度"),
            _ => {
                if length < 4 || pos + length > data.len() {
                    bail!("pkt-line 长度超出数据范围");
                }
                let payload = data[pos + 4..pos + length].to_vec();
                lines.push(payload);
                pos += length;
            }
        }
    }
    Ok((lines, pos))
}

/// 解压一段 zlib 数据，返回解压结果与消耗的压缩字节数。
pub(crate) fn inflate(data: &[u8], expected: usize) -> Result<(Vec<u8>, usize)> {
    let mut out = vec![0u8; expected.max(1)];
    let mut decoder = Decompress::new(true);
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let start = (before_out as usize).min(out.len());
        let status = decoder
            .decompress(data, &mut out[start..], FlushDecompress::Finish)
            .map_err(|error| anyhow::anyhow!("zlib 解压失败: {error}"))?;
        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError => {
                if decoder.total_in() == before_in && decoder.total_out() == before_out {
                    bail!("zlib 数据不完整");
                }
            }
        }
    }
    if decoder.total_out() as usize != expected {
        bail!("对象长度与 pack 头记录不一致");
    }
    out.truncate(expected);
    Ok((out, decoder.total_in() as usize))
}

pub(crate) fn read_object_header(data: &[u8], mut pos: usize) -> Result<(u8, usize, usize)> {
    let first = *data
        .get(pos)
        .ok_or_else(|| anyhow::anyhow!("pack 对象头被截断"))?;
    pos += 1;
    let kind = (first >> 4) & 0x07;
    let mut size = (first & 0x0f) as u64;
    let mut shift = 4u32;
    let mut byte = first;
    while byte & 0x80 != 0 {
        byte = *data
            .get(pos)
            .ok_or_else(|| anyhow::anyhow!("pack 对象头被截断"))?;
        pos += 1;
        size |= ((byte & 0x7f) as u64) << shift;
        shift += 7;
        if shift > 63 {
            bail!("pack 对象长度溢出");
        }
    }
    Ok((kind, size as usize, pos))
}

/// OFS_DELTA 的负偏移采用 Git 特有的变长编码。
pub(crate) fn read_ofs_offset(data: &[u8], mut pos: usize, object_offset: usize) -> Result<(usize, usize)> {
    let mut byte = *data
        .get(pos)
        .ok_or_else(|| anyhow::anyhow!("OFS_DELTA 偏移被截断"))?;
    pos += 1;
    let mut offset = (byte & 0x7f) as usize;
    while byte & 0x80 != 0 {
        byte = *data
            .get(pos)
            .ok_or_else(|| anyhow::anyhow!("OFS_DELTA 偏移被截断"))?;
        pos += 1;
        offset = ((offset + 1) << 7) | (byte & 0x7f) as usize;
    }
    if offset > object_offset {
        bail!("OFS_DELTA 基准偏移越界");
    }
    Ok((object_offset - offset, pos))
}

pub enum RawEntry {
    Base {
        offset: usize,
        kind: ObjectKind,
        data: Vec<u8>,
    },
    OfsDelta {
        offset: usize,
        base_offset: usize,
        delta: Vec<u8>,
    },
    RefDelta {
        offset: usize,
        base: Oid,
        delta: Vec<u8>,
    },
}

pub struct RawPack {
    pub entries: Vec<RawEntry>,
}

/// 解析 pack 文件（v2/v3），delta 只做切分，留到 `resolve` 阶段处理。
pub fn parse_pack(data: &[u8]) -> Result<RawPack> {
    if data.len() < 12 + 20 {
        bail!("pack 数据过短");
    }
    if &data[..4] != b"PACK" {
        bail!("不是 Git pack 数据");
    }
    let version = u32::from_be_bytes(data[4..8].try_into().expect("固定长度"));
    if version != 2 && version != 3 {
        bail!("不支持的 pack 版本: {version}");
    }
    let count = u32::from_be_bytes(data[8..12].try_into().expect("固定长度")) as usize;
    let body_end = data.len() - 20;
    let digest = Sha1::digest(&data[..body_end]);
    if digest[..] != data[body_end..] {
        bail!("pack 校验和不匹配");
    }

    let mut entries = Vec::with_capacity(count);
    let mut pos = 12usize;
    for _ in 0..count {
        let offset = pos;
        let (kind, size, next) = read_object_header(data, pos)?;
        pos = next;
        match kind {
            1..=4 => {
                let kind = ObjectKind::from_i16(kind as i16)
                    .ok_or_else(|| anyhow::anyhow!("未知的 pack 对象类型: {kind}"))?;
                let (object, consumed) = inflate(&data[pos..body_end], size)?;
                pos += consumed;
                entries.push(RawEntry::Base {
                    offset,
                    kind,
                    data: object,
                });
            }
            6 => {
                let (base_offset, next) = read_ofs_offset(data, pos, offset)?;
                pos = next;
                let (delta, consumed) = inflate(&data[pos..body_end], size)?;
                pos += consumed;
                entries.push(RawEntry::OfsDelta {
                    offset,
                    base_offset,
                    delta,
                });
            }
            7 => {
                if pos + 20 > body_end {
                    bail!("REF_DELTA 基准 OID 被截断");
                }
                let base = Oid::from_slice(&data[pos..pos + 20])?;
                pos += 20;
                let (delta, consumed) = inflate(&data[pos..body_end], size)?;
                pos += consumed;
                entries.push(RawEntry::RefDelta {
                    offset,
                    base,
                    delta,
                });
            }
            other => bail!("不支持的 pack 对象类型: {other}"),
        }
    }
    if pos != body_end {
        bail!("pack 尾部存在多余数据");
    }
    Ok(RawPack { entries })
}

pub(crate) fn read_varint(data: &[u8], mut pos: usize) -> Result<(usize, usize)> {
    let mut value = 0usize;
    let mut shift = 0u32;
    loop {
        let byte = *data
            .get(pos)
            .ok_or_else(|| anyhow::anyhow!("delta 头部被截断"))?;
        pos += 1;
        value |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, pos));
        }
        shift += 7;
        if shift > 63 {
            bail!("delta 长度溢出");
        }
    }
}

pub(crate) fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let (base_size, mut pos) = read_varint(delta, 0)?;
    if base_size != base.len() {
        bail!("delta 基准长度不匹配");
    }
    let (result_size, next) = read_varint(delta, pos)?;
    pos = next;
    let mut out = Vec::with_capacity(result_size);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            let mut offset = 0usize;
            let mut size = 0usize;
            for bit in 0..4 {
                if op & (1 << bit) != 0 {
                    offset |= (*delta
                        .get(pos)
                        .ok_or_else(|| anyhow::anyhow!("delta copy 参数被截断"))?
                        as usize)
                        << (8 * bit);
                    pos += 1;
                }
            }
            for bit in 0..3 {
                if op & (1 << (4 + bit)) != 0 {
                    size |= (*delta
                        .get(pos)
                        .ok_or_else(|| anyhow::anyhow!("delta copy 参数被截断"))?
                        as usize)
                        << (8 * bit);
                    pos += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            let end = offset
                .checked_add(size)
                .ok_or_else(|| anyhow::anyhow!("delta copy 范围溢出"))?;
            if end > base.len() {
                bail!("delta copy 超出基准对象");
            }
            out.extend_from_slice(&base[offset..end]);
        } else if op != 0 {
            let length = op as usize;
            if pos + length > delta.len() {
                bail!("delta insert 数据被截断");
            }
            out.extend_from_slice(&delta[pos..pos + length]);
            pos += length;
        } else {
            bail!("delta 指令非法");
        }
    }
    if out.len() != result_size {
        bail!("delta 结果长度不匹配");
    }
    Ok(out)
}

/// 解析 pack 中的 delta 并算出每个对象的 OID。
///
/// `external` 提供 pack 之外（数据库里已有）的基准对象，用于 thin pack。
pub fn resolve(
    pack: &RawPack,
    external: &HashMap<Oid, (ObjectKind, Vec<u8>)>,
) -> Result<Vec<(Oid, ObjectKind, Vec<u8>)>> {
    let mut by_offset: HashMap<usize, usize> = HashMap::new();
    for (index, entry) in pack.entries.iter().enumerate() {
        let offset = match entry {
            RawEntry::Base { offset, .. }
            | RawEntry::OfsDelta { offset, .. }
            | RawEntry::RefDelta { offset, .. } => *offset,
        };
        by_offset.insert(offset, index);
    }

    let mut resolved: Vec<Option<(Oid, ObjectKind, Vec<u8>)>> =
        vec![None; pack.entries.len()];
    let mut by_oid: HashMap<Oid, (ObjectKind, Vec<u8>)> = external.clone();
    let mut pending: Vec<usize> = (0..pack.entries.len()).collect();

    while !pending.is_empty() {
        let mut progress = false;
        let mut next = Vec::new();
        for index in pending {
            let item: Option<(ObjectKind, Vec<u8>)> = match &pack.entries[index] {
                RawEntry::Base { kind, data, .. } => Some((*kind, data.clone())),
                RawEntry::OfsDelta {
                    base_offset, delta, ..
                } => match by_offset
                    .get(base_offset)
                    .and_then(|base_index| resolved[*base_index].as_ref())
                {
                    Some((_, kind, base)) => Some((*kind, apply_delta(base, delta)?)),
                    None => None,
                },
                RawEntry::RefDelta { base, delta, .. } => match by_oid.get(base) {
                    Some((kind, base)) => Some((*kind, apply_delta(base, delta)?)),
                    None => None,
                },
            };
            match item {
                Some((kind, data)) => {
                    let oid = hash_object(kind, &data);
                    by_oid.insert(oid, (kind, data.clone()));
                    resolved[index] = Some((oid, kind, data));
                    progress = true;
                }
                None => next.push(index),
            }
        }
        if next.is_empty() {
            break;
        }
        if !progress {
            bail!("pack 中 delta 的基准对象缺失");
        }
        pending = next;
    }

    Ok(resolved.into_iter().flatten().collect())
}

/// 增量构建不含 delta 的 pack。
///
/// `push` 可以直接写入库里已经 zlib 压缩过的字节：packfile 每个对象体本身
/// 就是一条标准 zlib 流，与 `store::compress_blob` 产出的格式一致，因此
/// 全量 clone 不必「解压 → 再压缩」，也无需把所有对象同时留在内存里。
pub struct PackBuilder {
    out: Vec<u8>,
    count: u32,
}

impl PackBuilder {
    pub fn new() -> Self {
        let mut out = Vec::new();
        out.extend_from_slice(b"PACK");
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // 对象数占位，finish 时回填
        Self { out, count: 0 }
    }

    /// `size` 是对象**原始**长度；`data` 是待写入的字节。
    /// `compressed=true` 表示 data 已是完整 zlib 流，可直接复用。
    pub fn push(&mut self, kind: ObjectKind, size: usize, data: &[u8], compressed: bool) {
        let type_number = (kind as i16) as u8;
        let mut remaining = size as u64;
        let mut byte = (type_number << 4) | (remaining & 0x0f) as u8;
        remaining >>= 4;
        while remaining > 0 {
            self.out.push(byte | 0x80);
            byte = (remaining & 0x7f) as u8;
            remaining >>= 7;
        }
        self.out.push(byte);

        if compressed {
            self.out.extend_from_slice(data);
        } else {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(data)
                .expect("写入内存中的 deflate 流不会失败");
            self.out.extend_from_slice(
                &encoder
                    .finish()
                    .expect("结束内存中的 deflate 流不会失败"),
            );
        }
        self.count += 1;
    }

    pub fn finish(mut self) -> Vec<u8> {
        // 对象数在头部 8..12 字节；先回填，再对整个 pack 算 SHA-1 尾。
        self.out[8..12].copy_from_slice(&self.count.to_be_bytes());
        let digest = Sha1::digest(&self.out);
        self.out.extend_from_slice(&digest);
        self.out
    }
}

impl Default for PackBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// 生成不含 delta 的 pack。对象数量不大时，这比做 delta 压缩更简单可靠。
///
/// 生产路径已改用 `PackBuilder` 增量构建；这里保留一次性打包的实现，
/// 作为测试里校验「直接复用库内 zlib 字节」是否字节级一致的基准。
#[allow(dead_code)]
pub fn build_pack(objects: &[(ObjectKind, Vec<u8>)]) -> Vec<u8> {
    let mut builder = PackBuilder::new();
    for (kind, data) in objects {
        builder.push(*kind, data.len(), data, false);
    }
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkt_round_trip_stops_at_flush() {
        let mut data = pkt_str("want abc\n");
        data.extend(flush());
        data.extend_from_slice(b"PACKtail");
        let (lines, consumed) = parse_pkt_lines(&data).unwrap();
        assert_eq!(lines, vec![b"want abc\n".to_vec()]);
        assert_eq!(&data[consumed..], b"PACKtail");
    }

    #[test]
    fn pack_builder_reuses_stored_zlib_bytes() {
        let objects = vec![
            (ObjectKind::Blob, b"tiny\n".to_vec()),
            (
                ObjectKind::Blob,
                "compressible text line\n".repeat(200).into_bytes(),
            ),
        ];
        let direct = build_pack(&objects);

        // 模拟库里存的行：小对象存原样，大对象存 zlib 流。
        let mut builder = PackBuilder::new();
        for (kind, data) in &objects {
            if data.len() >= 256 {
                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(data).unwrap();
                let zlib = encoder.finish().unwrap();
                builder.push(*kind, data.len(), &zlib, true);
            } else {
                builder.push(*kind, data.len(), data, false);
            }
        }
        assert_eq!(
            direct,
            builder.finish(),
            "库内 zlib 字节必须能原样写进 pack，字节级一致"
        );
    }

    #[test]
    fn pack_round_trips_without_delta() {
        let objects = vec![
            (ObjectKind::Blob, b"hello\n".to_vec()),
            (ObjectKind::Blob, b"world\n".to_vec()),
        ];
        let pack = build_pack(&objects);
        let raw = parse_pack(&pack).unwrap();
        let resolved = resolve(&raw, &HashMap::new()).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(
            resolved[0].0.to_hex(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn object_header(kind: u8, size: usize) -> Vec<u8> {
        let mut size = size as u64;
        let mut byte = (kind << 4) | (size & 0x0f) as u8;
        size >>= 4;
        let mut out = Vec::new();
        while size > 0 {
            out.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.push(byte);
        out
    }

    #[test]
    fn resolves_ref_delta_against_pack_base() {
        let base = b"hello world\n".to_vec();
        let base_oid = hash_object(ObjectKind::Blob, &base);
        // copy 前 6 字节，再插入 "there\n"，得到 "hello there\n"。
        let delta = vec![12u8, 12, 0x90, 0x06, 6, b't', b'h', b'e', b'r', b'e', b'\n'];
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        pack.extend_from_slice(&2u32.to_be_bytes());
        pack.extend_from_slice(&2u32.to_be_bytes());
        pack.extend_from_slice(&object_header(3, base.len()));
        pack.extend_from_slice(&zlib(&base));
        pack.extend_from_slice(&object_header(7, delta.len()));
        pack.extend_from_slice(base_oid.as_bytes());
        pack.extend_from_slice(&zlib(&delta));
        let digest = Sha1::digest(&pack);
        pack.extend_from_slice(&digest);

        let raw = parse_pack(&pack).unwrap();
        let resolved = resolve(&raw, &HashMap::new()).unwrap();
        let expected = hash_object(ObjectKind::Blob, b"hello there\n");
        assert!(resolved.iter().any(|(oid, kind, _)| *oid == expected && *kind == ObjectKind::Blob));
    }

    /// thin pack：基准对象不在包里，由调用方从对象库查出来传进来。
    /// `prepare_receive_pack` 批量取 REF_DELTA base 走的就是这条路径。
    #[test]
    fn resolves_ref_delta_against_external_base() {
        let base = b"hello world\n".to_vec();
        let base_oid = hash_object(ObjectKind::Blob, &base);
        let delta = vec![12u8, 12, 0x90, 0x06, 6, b't', b'h', b'e', b'r', b'e', b'\n'];
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        pack.extend_from_slice(&2u32.to_be_bytes());
        pack.extend_from_slice(&1u32.to_be_bytes()); // 包里只有 delta，没有 base
        pack.extend_from_slice(&object_header(7, delta.len()));
        pack.extend_from_slice(base_oid.as_bytes());
        pack.extend_from_slice(&zlib(&delta));
        let digest = Sha1::digest(&pack);
        pack.extend_from_slice(&digest);

        let raw = parse_pack(&pack).unwrap();
        let mut external = HashMap::new();
        external.insert(base_oid, (ObjectKind::Blob, base));
        let resolved = resolve(&raw, &external).unwrap();
        let expected = hash_object(ObjectKind::Blob, b"hello there\n");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, expected);
        assert_eq!(resolved[0].2, b"hello there\n");

        // 库里查不到 base 时必须报错，不能静默丢掉这个对象。
        assert!(resolve(&raw, &HashMap::new()).is_err());
    }

    #[test]
    fn parses_receive_request_with_atomic_and_push_options() {
        let old = "0".repeat(40);
        let new = "1".repeat(40);
        let mut data = pkt_str(&format!(
            "{old} {new} refs/heads/main\0report-status atomic push-options\n"
        ));
        data.extend(flush());
        data.extend(pkt_str("push-option ci.skip\n"));
        data.extend(flush());
        data.extend_from_slice(b"PACKrest");

        let request = parse_receive_request(&data).unwrap();
        assert_eq!(request.commands.len(), 1);
        assert_eq!(request.commands[0].2, "refs/heads/main");
        assert!(request.capabilities.iter().any(|item| item == "atomic"));
        assert_eq!(request.options, vec!["ci.skip".to_string()]);
        assert_eq!(request.pack, b"PACKrest");
    }

    #[test]
    fn parses_receive_request_pack_without_options() {
        let old = "0".repeat(40);
        let new = "2".repeat(40);
        let mut data = pkt_str(&format!(
            "{old} {new} refs/tags/v1\0report-status\n"
        ));
        data.extend(flush());
        data.extend_from_slice(b"PACKbody");
        let request = parse_receive_request(&data).unwrap();
        assert!(request.options.is_empty());
        assert_eq!(request.pack, b"PACKbody");
    }

    #[test]
    fn parses_v2_command_and_arguments() {
        let mut data = pkt_str("command=ls-refs\n");
        data.extend(pkt_str("agent=git/2.40\n"));
        data.extend(delim());
        data.extend(pkt_str("peel\n"));
        data.extend(pkt_str("symrefs\n"));
        data.extend(pkt_str("ref-prefix refs/heads/\n"));
        data.extend(flush());

        let request = parse_v2_request(&data).unwrap();
        assert_eq!(request.command, "ls-refs");
        assert_eq!(
            request.args,
            vec![
                "peel".to_string(),
                "symrefs".to_string(),
                "ref-prefix refs/heads/".to_string()
            ]
        );
    }

    #[test]
    fn parses_upload_request_flags() {
        let want = "3".repeat(40);
        let have = "4".repeat(40);
        let shallow = "5".repeat(40);
        let mut data = pkt_str(&format!("want {want} multi_ack\n"));
        data.extend(pkt_str(&format!("have {have}\n")));
        data.extend(pkt_str(&format!("shallow {shallow}\n")));
        data.extend(pkt_str("deepen 2\n"));
        data.extend(pkt_str("filter blob:none\n"));
        data.extend(pkt_str("done\n"));
        data.extend(flush());

        let request = parse_upload_request(&data).unwrap();
        assert_eq!(request.wants.len(), 1);
        assert_eq!(request.haves.len(), 1);
        assert_eq!(request.shallow.len(), 1);
        assert_eq!(request.deepen, Some(2));
        assert!(request.filter_blob_none);
        assert!(request.done);
    }

    #[test]
    fn parses_deepen_relative_from_want_capabilities() {
        // git 只把 deepen-relative 内联在 want 行的能力列表,不发独立行。
        let want = "3".repeat(40);
        let shallow = "5".repeat(40);
        let mut data = pkt_str(&format!(
            "want {want} multi_ack_detailed deepen-relative ofs-delta agent=git/2.43.0\n"
        ));
        data.extend(pkt_str(&format!("shallow {shallow}\n")));
        data.extend(pkt_str("deepen 5\n"));
        data.extend(flush());

        let request = parse_upload_request(&data).unwrap();
        assert!(request.deepen_relative);
        assert_eq!(request.deepen, Some(5));
        assert_eq!(request.shallow.len(), 1);
    }

    #[test]
    fn malformed_requests_are_classified_as_client_errors() {
        // HTTP 层靠这个标记把畸形包映射成 400，不能退化成 500。
        let errors = [
            parse_receive_request(b"not-a-valid-pack")
                .err()
                .expect("畸形推送请求应解析失败"),
            parse_upload_request(b"garbage")
                .err()
                .expect("畸形 upload-pack 请求应解析失败"),
            parse_v2_request(b"bad")
                .err()
                .expect("畸形 v2 请求应解析失败"),
        ];
        for error in errors {
            assert!(
                error.downcast_ref::<MalformedRequest>().is_some(),
                "未标记: {error}"
            );
        }
    }
}
