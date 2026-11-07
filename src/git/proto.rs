use anyhow::{bail, Result};
use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::io::Write;

use super::object::{hash_object, ObjectKind, Oid};

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
fn inflate(data: &[u8], expected: usize) -> Result<(Vec<u8>, usize)> {
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

fn read_object_header(data: &[u8], mut pos: usize) -> Result<(u8, usize, usize)> {
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
fn read_ofs_offset(data: &[u8], mut pos: usize, object_offset: usize) -> Result<(usize, usize)> {
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

fn read_varint(data: &[u8], mut pos: usize) -> Result<(usize, usize)> {
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

fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
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

/// 生成不含 delta 的 pack。对象数量不大时，这比做 delta 压缩更简单可靠。
pub fn build_pack(objects: &[(ObjectKind, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for (kind, data) in objects {
        let type_number = (*kind as i16) as u8;
        let mut size = data.len() as u64;
        let mut byte = (type_number << 4) | (size & 0x0f) as u8;
        size >>= 4;
        while size > 0 {
            out.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.push(byte);

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(data)
            .expect("写入内存中的 deflate 流不会失败");
        out.extend_from_slice(&encoder.finish().expect("结束内存中的 deflate 流不会失败"));
    }
    let digest = Sha1::digest(&out);
    out.extend_from_slice(&digest);
    out
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
}
