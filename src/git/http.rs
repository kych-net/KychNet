use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::object::{
    commit_tree, ensure_text, Object, ObjectKind, Oid, Tree, TreeEntry,
};
use super::proto;
use super::store;
use crate::models::User;
use crate::project::Project;

/// 单次遍历最多处理的对象数量，避免超大仓库把内存与数据库拖垮。
/// 超过上限时推送会失败，fetch 会退化为全量传输。
const MAX_COMMIT_WALK: usize = 5000;
const MAX_OBJECT_WALK: usize = 200_000;

/// `receive_pack` 的返回体。ref 变更已在**同一事务**内写入事件 outbox，
/// 因此这里只回传 Git 协议响应，不再把 updates 交给上层重复触发。
pub struct ReceiveOutcome {
    pub response: Vec<u8>,
}

fn upload_caps(project: &Project) -> String {
    format!(
        "agent=kych-net/0.3 ofs-delta shallow deepen-relative filter multi_ack_detailed \
         allow-tip-sha1-in-want allow-reachable-sha1-in-want \
         symref=HEAD:refs/heads/{}",
        project.default_branch
    )
}

const RECEIVE_CAPS: &str =
    "report-status delete-refs ofs-delta atomic push-options agent=kych-net/0.3";

fn advertise_line(oid: Oid, name: &str, caps: Option<&str>) -> Vec<u8> {
    match caps {
        Some(caps) => proto::pkt_str(&format!("{} {name}\0{caps}\n", oid.to_hex())),
        None => proto::pkt_str(&format!("{} {name}\n", oid.to_hex())),
    }
}

/// 生成引用广告（不带 `# service=` 前缀）。
///
/// 分支与标签都会广告；`HEAD` 通过 `symref` 能力指向默认分支，
/// 这样客户端不需要再猜远端默认分支名。
///
/// SSH 传输（`ssh.rs`）也复用这段广告：SSH 协议没有 `# service=` 行，
/// 那是 smart HTTP 独有的前缀，所以拆成两层。
pub async fn advertise_refs(pool: &PgPool, project: &Project, service: &str) -> Result<Vec<u8>> {
    let mut conn = pool.acquire().await?;
    let refs = store::list_refs(&mut conn, project.id).await?;

    let caps = if service == "git-upload-pack" {
        upload_caps(project)
    } else {
        RECEIVE_CAPS.to_string()
    };

    let mut out = Vec::new();
    if refs.is_empty() {
        out.extend(proto::pkt_str(&format!(
            "{} capabilities^{{}}\0{caps}\n",
            Oid::zero().to_hex()
        )));
        out.extend(proto::flush());
        return Ok(out);
    }

    let default_ref = format!("refs/heads/{}", project.default_branch);
    let head_target = refs
        .iter()
        .find(|(name, _)| name == &default_ref)
        .map(|(_, oid)| *oid);

    let mut first = true;
    if let Some(oid) = head_target {
        out.extend(advertise_line(oid, "HEAD", Some(&caps)));
        first = false;
    }
    for (name, oid) in &refs {
        // 只广告分支与标签，其他内部引用不对外暴露。
        if !name.starts_with("refs/heads/") && !name.starts_with("refs/tags/") {
            continue;
        }
        let line_caps = if first { Some(caps.as_str()) } else { None };
        out.extend(advertise_line(*oid, name, line_caps));
        first = false;
    }
    out.extend(proto::flush());
    Ok(out)
}

/// smart HTTP 的引用广告：引用广告 + `# service=` 前缀。
pub async fn advertise(pool: &PgPool, project: &Project, service: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend(proto::pkt_str(&format!("# service={service}\n")));
    out.extend(proto::flush());
    out.extend(advertise_refs(pool, project, service).await?);
    Ok(out)
}

/// protocol v2 的能力广告（只有 upload-pack 支持 v2）。
pub fn advertise_v2(service: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(proto::pkt_str(&format!("# service={service}\n")));
    out.extend(proto::flush());
    out.extend(v2_capabilities());
    out
}

/// protocol v2 的能力段（不带 `# service=` 前缀），SSH 传输复用。
pub fn v2_capabilities() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(proto::pkt_str("version 2\n"));
    out.extend(proto::pkt_str("agent=kych-net/0.3\n"));
    out.extend(proto::pkt_str("ls-refs=unborn\n"));
    out.extend(proto::pkt_str("fetch=shallow filter\n"));
    out.extend(proto::pkt_str("server-option\n"));
    out.extend(proto::pkt_str("object-format=sha1\n"));
    out.extend(proto::flush());
    out
}

/// 读取 tag 对象指向的目标 OID。
fn tag_target(data: &[u8]) -> Result<Option<Oid>> {
    let text = std::str::from_utf8(data)
        .map_err(|_| anyhow::anyhow!("tag 对象不是 UTF-8"))?;
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("object ") {
            return Ok(Some(Oid::from_hex(value.trim())?));
        }
    }
    Ok(None)
}

/// 逐层剥开 annotated tag，直到拿到一个普通对象。
async fn peel_tag_once(
    conn: &mut PgConnection,
    project_id: Uuid,
    oid: Oid,
) -> Result<Oid> {
    let mut current = oid;
    for _ in 0..8 {
        let Some(object) = store::get_object(conn, project_id, current).await? else {
            break;
        };
        if object.kind != ObjectKind::Tag {
            break;
        }
        match tag_target(&object.data)? {
            Some(target) => current = target,
            None => break,
        }
    }
    Ok(current)
}

/// 计算浅克隆边界。
///
/// 返回 `(新的 shallow 边界, 需要 unshallow 的旧边界, 遍历时不再展开父提交的集合)`。
/// SSH 传输的会话式 upload-pack 在「带 deepen 但还没 done」的协商轮里
/// 也要先回浅边界段，所以这里必须对内开放。
pub(crate) async fn compute_shallow(
    conn: &mut PgConnection,
    project_id: Uuid,
    wants: &[Oid],
    request: &proto::UploadRequest,
) -> Result<(Vec<Oid>, Vec<Oid>, HashSet<Oid>)> {
    let Some(depth) = request.deepen else {
        // 没有 deepen 时沿用客户端已有的浅边界，避免把它没有的历史发回去。
        let stop: HashSet<Oid> = request.shallow.iter().copied().collect();
        return Ok((Vec::new(), Vec::new(), stop));
    };
    if depth == 0 {
        let stop: HashSet<Oid> = request.shallow.iter().copied().collect();
        return Ok((Vec::new(), Vec::new(), stop));
    }

    // 相对加深从客户端现有边界继续往下走；绝对深度则直接从 want 重新计算。
    let relative = request.deepen_relative && !request.shallow.is_empty();
    let mut current: Vec<(Oid, u32)> = Vec::new();
    if relative {
        for oid in &request.shallow {
            current.push((*oid, 0));
        }
    } else {
        let rows = store::get_objects(conn, project_id, wants).await?;
        let mut by_oid: HashMap<Oid, Object> = rows.into_iter().collect();
        for want in wants {
            let Some(mut object) = by_oid.remove(want) else {
                continue;
            };
            let mut oid = *want;
            if object.kind == ObjectKind::Tag {
                oid = peel_tag_once(conn, project_id, *want).await?;
                object = store::require_object(conn, project_id, oid).await?;
            }
            if object.kind == ObjectKind::Commit {
                current.push((oid, 0));
            }
        }
    }

    let mut boundaries = Vec::new();
    let mut stop = HashSet::new();
    let mut seen = HashSet::new();
    // 按深度分层遍历,同一层的提交合并成一次查询,避免逐提交访问数据库。
    while !current.is_empty() {
        let mut next: Vec<(Oid, u32)> = Vec::new();
        let mut pending: Vec<(Oid, u32)> = Vec::new();
        for (oid, level) in current.drain(..) {
            if !seen.insert(oid) {
                continue;
            }
            if seen.len() > MAX_COMMIT_WALK {
                bail!("提交历史过深，无法计算浅克隆边界");
            }
            // 绝对深度以 want 为第 1 层；相对加深从旧边界再往下数 depth 层。
            let at_boundary = if relative {
                level >= depth
            } else {
                level + 1 >= depth
            };
            if at_boundary {
                stop.insert(oid);
                boundaries.push(oid);
                continue;
            }
            pending.push((oid, level));
        }
        let oids: Vec<Oid> = pending.iter().map(|(oid, _)| *oid).collect();
        let rows = store::get_objects(conn, project_id, &oids).await?;
        let mut found: HashMap<Oid, Object> = rows.into_iter().collect();
        for (oid, level) in pending {
            let Some(object) = found.remove(&oid) else {
                bail!("Git 对象不存在: {oid}");
            };
            if object.kind != ObjectKind::Commit {
                continue;
            }
            for parent in super::object::commit_parents(&object.data)? {
                next.push((parent, level + 1));
            }
        }
        current = next;
    }
    boundaries.sort();
    boundaries.dedup();

    let boundary_set: HashSet<Oid> = boundaries.iter().copied().collect();
    let mut unshallow: Vec<Oid> = request
        .shallow
        .iter()
        .copied()
        .filter(|oid| !boundary_set.contains(oid))
        .collect();
    unshallow.sort();
    unshallow.dedup();
    Ok((boundaries, unshallow, stop))
}

/// 收集 want 可达的对象。
///
/// - `exclude`：客户端已经拥有的对象，不发送也不继续遍历；
/// - `shallow`：这些提交不再向父提交展开；
/// - `filter_blob_none`：省略非直接 want 的 blob；
/// - `deepen`：本轮是加深请求。此时 `exclude` 中的提交是客户端已有的
///   通道（含旧浅边界），必须沿提交父链继续穿过它们才能走到新浅边界，
///   否则 unshallow 之后客户端向上遍历会因缺父对象而失败。
async fn collect_objects(
    conn: &mut PgConnection,
    project_id: Uuid,
    wants: &[Oid],
    exclude: &HashSet<Oid>,
    shallow: &HashSet<Oid>,
    filter_blob_none: bool,
    deepen: bool,
) -> Result<Vec<(ObjectKind, Vec<u8>)>> {
    let direct: HashSet<Oid> = wants.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut objects = Vec::new();
    let mut stack: Vec<Oid> = wants.to_vec();
    // 按批拉取:每批 oid 一次 `ANY($2)` 查询,把原先每对象一行的
    // 逐行 SELECT 收敛为常数次往返,对象数再多也不会线性放大延迟。
    const BATCH: usize = 512;
    while !stack.is_empty() {
        let take = stack.len().min(BATCH);
        let candidates: Vec<Oid> = stack.drain(stack.len() - take..).collect();
        let mut fetch: Vec<Oid> = Vec::with_capacity(candidates.len());
        for oid in candidates {
            if exclude.contains(&oid) && !deepen {
                // 客户端已有的对象:不发送也不再拉取。
                continue;
            }
            if seen.insert(oid) {
                if seen.len() > MAX_OBJECT_WALK {
                    bail!("需要传输的对象过多，请缩小拉取范围");
                }
                fetch.push(oid);
            }
        }
        let rows = store::get_objects(conn, project_id, &fetch).await?;
        let mut found: HashMap<Oid, Object> = rows.into_iter().collect();
        for oid in fetch {
            let Some(object) = found.remove(&oid) else {
                if exclude.contains(&oid) {
                    continue;
                }
                bail!("请求的对象不存在: {oid}");
            };
            if exclude.contains(&oid) {
                // 加深请求:已有对象本身不发送,只沿提交链向下走到新浅边界,
                // 否则 unshallow 之后客户端向上遍历会因缺父对象而失败。
                if object.kind == ObjectKind::Commit && !shallow.contains(&oid) {
                    stack.extend(super::object::commit_parents(&object.data)?);
                }
                continue;
            }
            match object.kind {
                ObjectKind::Commit => {
                    stack.push(commit_tree(&object.data)?);
                    if !shallow.contains(&oid) {
                        stack.extend(super::object::commit_parents(&object.data)?);
                    }
                }
                ObjectKind::Tree => {
                    let tree = Tree::parse(&object.data)?;
                    for entry in tree.entries.values() {
                        match entry {
                            TreeEntry::Blob(oid) | TreeEntry::Dir(oid) => stack.push(*oid),
                        }
                    }
                }
                ObjectKind::Tag => {
                    if let Some(target) = tag_target(&object.data)? {
                        stack.push(target);
                    }
                }
                ObjectKind::Blob => {
                    if filter_blob_none && !direct.contains(&oid) {
                        continue;
                    }
                }
            }
            objects.push((object.kind, object.data));
        }
    }
    Ok(objects)
}

/// 计算客户端已有对象集合。
///
/// 返回 `(可确认的公共提交, 排除集合)`；遍历超出上限时返回 None，
/// 调用方退化为全量传输，避免发出错误的历史。SSH 传输按轮复用。
pub(crate) async fn have_reachable(
    conn: &mut PgConnection,
    project_id: Uuid,
    haves: &[Oid],
    client_shallow: &[Oid],
) -> Result<Option<(Vec<Oid>, HashSet<Oid>)>> {
    let client_shallow: HashSet<Oid> = client_shallow.iter().copied().collect();
    let mut common = Vec::new();
    let mut exclude = HashSet::new();
    let mut stack: Vec<Oid> = Vec::new();
    // have 逐个 resolve_commit 会每个 have 查 2~3 次库;这里先批量取回,
    // 只有标签才单独逐层剥开。
    let rows = store::get_objects(conn, project_id, haves).await?;
    let mut by_oid: HashMap<Oid, Object> = rows.into_iter().collect();
    for have in haves {
        // 即使 have 是标签对象，也从排除集合里剔掉，避免重复发送。
        exclude.insert(*have);
        let Some(mut object) = by_oid.remove(have) else {
            continue;
        };
        let mut oid = *have;
        if object.kind == ObjectKind::Tag {
            oid = peel_tag_once(conn, project_id, *have).await?;
            object = store::require_object(conn, project_id, oid).await?;
        }
        if object.kind == ObjectKind::Commit {
            common.push(oid);
            stack.push(oid);
        }
    }
    if common.is_empty() {
        return Ok(Some((common, exclude)));
    }

    let mut seen = HashSet::new();
    // 与 collect_objects 同样按批拉取:增量 fetch 每次都会 walk 客户端
    // 已有的全部 tree/blob,逐行查询会让延迟随仓库对象数线性放大。
    const BATCH: usize = 512;
    while !stack.is_empty() {
        let take = stack.len().min(BATCH);
        let candidates: Vec<Oid> = stack.drain(stack.len() - take..).collect();
        let mut fetch: Vec<Oid> = Vec::with_capacity(candidates.len());
        for oid in candidates {
            if seen.insert(oid) {
                if seen.len() > MAX_OBJECT_WALK {
                    return Ok(None);
                }
                fetch.push(oid);
            }
        }
        let rows = store::get_objects(conn, project_id, &fetch).await?;
        let mut found: HashMap<Oid, Object> = rows.into_iter().collect();
        for oid in fetch {
            let Some(object) = found.remove(&oid) else {
                continue;
            };
            match object.kind {
                ObjectKind::Commit => {
                    stack.push(commit_tree(&object.data)?);
                    if !client_shallow.contains(&oid) {
                        stack.extend(super::object::commit_parents(&object.data)?);
                    }
                }
                ObjectKind::Tree => {
                    let tree = Tree::parse(&object.data)?;
                    for entry in tree.entries.values() {
                        match entry {
                            TreeEntry::Blob(oid) | TreeEntry::Dir(oid) => stack.push(*oid),
                        }
                    }
                }
                ObjectKind::Tag => {
                    if let Some(target) = tag_target(&object.data)? {
                        stack.push(target);
                    }
                }
                ObjectKind::Blob => {}
            }
            exclude.insert(oid);
        }
    }
    Ok(Some((common, exclude)))
}

pub(crate) fn shallow_response(boundaries: &[Oid], unshallow: &[Oid]) -> Vec<u8> {
    let mut out = Vec::new();
    for oid in boundaries {
        out.extend(proto::pkt_str(&format!("shallow {}\n", oid.to_hex())));
    }
    for oid in unshallow {
        out.extend(proto::pkt_str(&format!("unshallow {}\n", oid.to_hex())));
    }
    out
}

/// protocol v2 的 packfile 数据必须走 sideband pkt-line：
/// 4 字节长度 + 1 字节 channel + 数据；单包数据上限 65515 字节。
fn sideband_pack(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(65_515) {
        let mut payload = Vec::with_capacity(chunk.len() + 1);
        payload.push(1);
        payload.extend_from_slice(chunk);
        out.extend(proto::pkt(&payload));
    }
    out.extend(proto::flush());
    out
}

/// v0 upload-pack。
pub async fn upload_pack(pool: &PgPool, project: &Project, body: &[u8]) -> Result<Vec<u8>> {
    let request = proto::parse_upload_request(body)?;
    upload_pack_request(pool, project, &request).await
}

/// 已解析请求版本的上传处理。
///
/// SSH 的会话式 upload-pack 需要把多轮 want/have/done 累积成**一个**请求
/// 再处理（客户端发 `done` 时不会重复 wants，HTTP 的无状态 RPC 不存在
/// 这个问题），所以把「解析」和「处理」拆开，两边共用同一套遍历逻辑。
pub(crate) async fn upload_pack_request(
    pool: &PgPool,
    project: &Project,
    request: &proto::UploadRequest,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if request.wants.is_empty() {
        out.extend(proto::pkt_str("NAK\n"));
        return Ok(out);
    }
    let mut conn = pool.acquire().await?;
    let (common, exclude) =
        match have_reachable(&mut conn, project.id, &request.haves, &request.shallow).await? {
            Some(value) => value,
            None => (Vec::new(), HashSet::new()),
        };
    let (boundaries, unshallow, shallow) =
        compute_shallow(&mut conn, project.id, &request.wants, &request).await?;

    // `deepen` 请求:必须先回浅边界段并以 flush 收尾,客户端读完这段才进入 ACK/NAK
    // 协商(git-upload-pack 即此顺序)。若把 ACK/NAK 提前,客户端会报
    // "expected shallow/unshallow, got NAK",或在协商结束前就收到 pack 而报错。
    if request.deepen.is_some() {
        out.extend(shallow_response(&boundaries, &unshallow));
        out.extend(proto::flush());
    }

    // 协商轮(请求里没有 `done`)按官方 multi_ack_detailed 序列响应:
    //   只有 want 段      → 空响应;
    //   have 段无公共提交 → NAK(等客户端补发 haves);
    //   have 段有公共提交 → `ACK … common` … `ACK … ready` + `NAK`。
    // `ready`+`NAK` 会让客户端补发 `done` 轮,packfile 只在 `done` 轮发送。
    // 这里绝不能像官方单请求 EOF 形态那样把 final ACK+pack 一并塞进本段:
    // 分轮(stateless)下客户端不消费这些字节,它们会滞留并与 `done` 轮的
    // 响应错位,导致 unpack 读到混杂的两个 pack 而挂死/报
    // "bad line length character: PACK"。
    if !request.done {
        if request.haves.is_empty() {
            return Ok(out);
        }
        if common.is_empty() {
            out.extend(proto::pkt_str("NAK\n"));
            return Ok(out);
        }
        for oid in &common {
            out.extend(proto::pkt_str(&format!("ACK {} common\n", oid.to_hex())));
        }
        let last = common.last().expect("common 非空");
        out.extend(proto::pkt_str(&format!("ACK {} ready\n", last.to_hex())));
        out.extend(proto::pkt_str("NAK\n"));
        return Ok(out);
    }

    // `done` 轮:ACK/NAK 后紧跟 packfile。
    let objects = collect_objects(
        &mut conn,
        project.id,
        &request.wants,
        &exclude,
        &shallow,
        request.filter_blob_none,
        request.deepen.is_some(),
    )
    .await?;
    match common.last() {
        Some(oid) => out.extend(proto::pkt_str(&format!("ACK {}\n", oid.to_hex()))),
        None => out.extend(proto::pkt_str("NAK\n")),
    }
    out.extend(proto::build_pack(&objects));
    Ok(out)
}

/// v2 `ls-refs`。
async fn ls_refs(
    conn: &mut PgConnection,
    project: &Project,
    args: &[String],
) -> Result<Vec<u8>> {
    let mut prefixes: Vec<String> = Vec::new();
    let mut symrefs = false;
    let mut peel = false;
    let mut unborn = false;
    for arg in args {
        match arg.as_str() {
            "symrefs" => symrefs = true,
            "peel" => peel = true,
            "unborn" => unborn = true,
            _ => {
                if let Some(value) = arg.strip_prefix("ref-prefix ") {
                    prefixes.push(value.to_string());
                }
            }
        }
    }

    let refs = store::list_refs(conn, project.id).await?;
    let default_ref = format!("refs/heads/{}", project.default_branch);
    let head_target = refs
        .iter()
        .find(|(name, _)| name == &default_ref)
        .map(|(_, oid)| *oid);

    let mut lines: Vec<String> = Vec::new();
    if symrefs {
        match head_target {
            Some(oid) => lines.push(format!(
                "{} HEAD symref-target:{}\n",
                oid.to_hex(),
                default_ref
            )),
            None if unborn => lines.push(format!(
                "unborn HEAD symref-target:{}\n",
                default_ref
            )),
            None => {}
        }
    }

    for (name, oid) in &refs {
        if !name.starts_with("refs/heads/") && !name.starts_with("refs/tags/") {
            continue;
        }
        if !prefixes.is_empty() && !prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let mut line = format!("{} {}", oid.to_hex(), name);
        if peel && name.starts_with("refs/tags/") {
            let peeled = peel_tag_once(conn, project.id, *oid).await?;
            if peeled != *oid {
                line.push_str(&format!(" peeled:{}", peeled.to_hex()));
            }
        }
        line.push('\n');
        lines.push(line);
    }

    let mut out = Vec::new();
    for line in lines {
        out.extend(proto::pkt_str(&line));
    }
    out.extend(proto::flush());
    Ok(out)
}

/// v2 `fetch`。
async fn v2_fetch(
    conn: &mut PgConnection,
    project: &Project,
    args: &[String],
) -> Result<Vec<u8>> {
    let mut request = proto::UploadRequest::default();
    for arg in args {
        if let Some(rest) = arg.strip_prefix("want ") {
            request
                .wants
                .push(Oid::from_hex(rest.split(' ').next().unwrap_or_default())?);
        } else if let Some(rest) = arg.strip_prefix("have ") {
            request
                .haves
                .push(Oid::from_hex(rest.split(' ').next().unwrap_or_default())?);
        } else if let Some(rest) = arg.strip_prefix("shallow ") {
            request
                .shallow
                .push(Oid::from_hex(rest.split(' ').next().unwrap_or_default())?);
        } else if let Some(rest) = arg.strip_prefix("deepen ") {
            request.deepen = rest.trim().parse().ok();
        } else if arg == "deepen-relative" {
            request.deepen_relative = true;
        } else if arg == "done" {
            request.done = true;
        } else if arg.starts_with("filter blob:none") {
            request.filter_blob_none = true;
        }
    }

    let mut out = Vec::new();
    if request.wants.is_empty() {
        out.extend(proto::pkt_str("acknowledgments\n"));
        out.extend(proto::pkt_str("NAK\n"));
        out.extend(proto::flush());
        return Ok(out);
    }

    let (common, exclude) =
        match have_reachable(conn, project.id, &request.haves, &request.shallow).await? {
            Some(value) => value,
            None => (vec![Oid::zero()], HashSet::new()),
        };
    let common_known = common.iter().any(|oid| !oid.is_zero());
    let (boundaries, unshallow, shallow) =
        compute_shallow(conn, project.id, &request.wants, &request).await?;

    // 还没 done 又没有公共提交时只回复 NAK，让客户端继续协商。
    if !request.done && !common_known {
        out.extend(proto::pkt_str("acknowledgments\n"));
        out.extend(proto::pkt_str("NAK\n"));
        out.extend(proto::flush());
        return Ok(out);
    }

    let objects = collect_objects(
        conn,
        project.id,
        &request.wants,
        &exclude,
        &shallow,
        request.filter_blob_none,
        request.deepen.is_some(),
    )
    .await?;

    // packfile 段：ready 与 done 两条路径共用（v2 规范要求 ready 之后
    // 必须紧跟 delim 并在同一响应里带上 shallow-info / packfile 段，
    // 与真实 upload-pack 的 process_haves_and_send_acks 一致）。
    let mut pack_sections = |out: &mut Vec<u8>| {
        if !boundaries.is_empty() || !unshallow.is_empty() {
            out.extend(proto::pkt_str("shallow-info\n"));
            for oid in &boundaries {
                out.extend(proto::pkt_str(&format!("shallow {}\n", oid.to_hex())));
            }
            for oid in &unshallow {
                out.extend(proto::pkt_str(&format!("unshallow {}\n", oid.to_hex())));
            }
            out.extend(proto::delim());
        }
        out.extend(proto::pkt_str("packfile\n"));
        out.extend(sideband_pack(&proto::build_pack(&objects)));
    };

    if !request.done {
        out.extend(proto::pkt_str("acknowledgments\n"));
        if common_known {
            for oid in common.iter().filter(|oid| !oid.is_zero()) {
                out.extend(proto::pkt_str(&format!("ACK {}\n", oid.to_hex())));
            }
            // 只有客户端已有的对象能覆盖全部 wants（ok_to_give_up）时才能
            // 发 ready；ready 后跟 delim + packfile，本轮响应直接收尾。
            if request.wants.iter().all(|oid| exclude.contains(oid)) {
                out.extend(proto::pkt_str("ready\n"));
                out.extend(proto::delim());
                pack_sections(&mut out);
                return Ok(out);
            }
        } else {
            out.extend(proto::pkt_str("NAK\n"));
        }
        out.extend(proto::flush());
        return Ok(out);
    }

    // done 之后不再回 acknowledgments；客户端已经在等 packfile 段。
    pack_sections(&mut out);
    Ok(out)
}

/// v2 请求入口：根据 `command=` 分发。
pub async fn upload_pack_v2(
    pool: &PgPool,
    project: &Project,
    body: &[u8],
) -> Result<Vec<u8>> {
    let request = proto::parse_v2_request(body)?;
    let mut conn = pool.acquire().await?;
    match request.command.as_str() {
        "ls-refs" => ls_refs(&mut conn, project, &request.args).await,
        "fetch" => v2_fetch(&mut conn, project, &request.args).await,
        other => bail!("不支持的 protocol v2 命令: {other}"),
    }
}

struct CommitInfo {
    tree: Oid,
    parents: Vec<Oid>,
    author: String,
    committed_at: DateTime<Utc>,
    message: String,
}

fn parse_commit(data: &[u8]) -> Result<CommitInfo> {
    let text = std::str::from_utf8(data)
        .map_err(|_| anyhow::anyhow!("commit 对象不是 UTF-8，暂不支持"))?;
    let (head, message) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = "KychNet".to_string();
    let mut committed_at = Utc::now();
    for line in head.lines() {
        if let Some(value) = line.strip_prefix("tree ") {
            tree = Some(Oid::from_hex(value)?);
        } else if let Some(value) = line.strip_prefix("parent ") {
            parents.push(Oid::from_hex(value)?);
        } else if let Some(value) = line.strip_prefix("committer ") {
            if let Some((name, timestamp)) = split_identity(value) {
                author = name;
                committed_at = DateTime::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now);
            }
        }
    }
    Ok(CommitInfo {
        tree: tree.context("commit 缺少 tree 行")?,
        parents,
        author,
        committed_at,
        message: message.trim().to_string(),
    })
}

fn split_identity(value: &str) -> Option<(String, i64)> {
    let close = value.rfind('>')?;
    let name = value[..close]
        .split('<')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let rest = value[close + 1..].trim();
    let timestamp = rest.split_whitespace().next()?.parse().ok()?;
    Some((crate::git::object::identity_name(&name), timestamp))
}

async fn unpack_pack(
    conn: &mut PgConnection,
    project_id: Uuid,
    data: &[u8],
) -> Result<usize> {
    if data.is_empty() {
        return Ok(0);
    }
    let raw = proto::parse_pack(data)?;
    let mut external = HashMap::new();
    for entry in &raw.entries {
        if let proto::RawEntry::RefDelta { base, .. } = entry {
            if let Some(object) = store::get_object(conn, project_id, *base).await? {
                external.insert(*base, (object.kind, object.data));
            }
        }
    }
    let resolved = proto::resolve(&raw, &external)?;
    for (oid, kind, object) in &resolved {
        match kind {
            ObjectKind::Blob | ObjectKind::Commit | ObjectKind::Tag => ensure_text(object)?,
            ObjectKind::Tree => {}
        }
        store::put_object(
            conn,
            project_id,
            &Object {
                kind: *kind,
                data: object.clone(),
            },
        )
        .await?;
        let _ = oid;
    }
    Ok(resolved.len())
}

async fn is_ancestor(
    conn: &mut PgConnection,
    project_id: Uuid,
    ancestor: Oid,
    descendant: Oid,
) -> Result<bool> {
    let mut seen = HashSet::new();
    let mut stack = vec![descendant];
    while let Some(oid) = stack.pop() {
        if oid == ancestor {
            return Ok(true);
        }
        if seen.len() >= MAX_COMMIT_WALK {
            bail!("提交历史过深，暂时无法校验快进关系");
        }
        if !seen.insert(oid) {
            continue;
        }
        let object = store::require_object(conn, project_id, oid).await?;
        if object.kind != ObjectKind::Commit {
            continue;
        }
        for parent in super::object::commit_parents(&object.data)? {
            stack.push(parent);
        }
    }
    Ok(false)
}

/// 把新提交补进索引表；遇到已索引的提交就停止向上遍历。
pub(crate) async fn index_commits(
    conn: &mut PgConnection,
    project_id: Uuid,
    tip: Oid,
    user: &User,
) -> Result<()> {
    let mut seen = HashSet::new();
    let mut stack = vec![tip];
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if seen.len() > MAX_COMMIT_WALK {
            bail!("提交历史过深，无法建立索引");
        }
        let known: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM git_commits WHERE project_id = $1 AND oid = $2",
        )
        .bind(project_id)
        .bind(&oid.as_bytes()[..])
        .fetch_optional(&mut *conn)
        .await?;
        if known.is_some() {
            continue;
        }
        let object = store::require_object(conn, project_id, oid).await?;
        if object.kind != ObjectKind::Commit {
            continue;
        }
        let info = parse_commit(&object.data)?;
        store::insert_commit_index(
            conn,
            project_id,
            oid,
            info.parents.first().copied(),
            info.parents.get(1).copied(),
            info.tree,
            Some(user.id),
            &info.author,
            &info.message,
            info.committed_at,
        )
        .await?;
        for parent in info.parents {
            stack.push(parent);
        }
    }
    Ok(())
}

/// 校验一条推送命令；通过时返回要执行的动作。
enum CommandAction {
    Delete(String),
    Set {
        name: String,
        target: Oid,
        is_head: bool,
    },
}

fn validate_command(
    name: &str,
    _old: Oid,
    new: Oid,
    default_branch: &str,
) -> Result<CommandAction, String> {
    let kind = if name.starts_with("refs/heads/") {
        "head"
    } else if name.starts_with("refs/tags/") {
        "tag"
    } else {
        return Err("只允许推送分支与标签".to_string());
    };
    let short = if kind == "head" {
        &name["refs/heads/".len()..]
    } else {
        &name["refs/tags/".len()..]
    };
    if !crate::project::valid_branch(short) {
        return Err("非法的引用名".to_string());
    }
    if new.is_zero() {
        if kind == "head" && short == default_branch {
            return Err("不能删除默认分支".to_string());
        }
        return Ok(CommandAction::Delete(name.to_string()));
    }
    Ok(CommandAction::Set {
        name: name.to_string(),
        target: new,
        is_head: kind == "head",
    })
}

pub async fn receive_pack(
    pool: &PgPool,
    project: &Project,
    user: &User,
    body: &[u8],
) -> Result<ReceiveOutcome> {
    let request = proto::parse_receive_request(body)?;
    let atomic = request
        .capabilities
        .iter()
        .any(|capability| capability == "atomic");
    if !request.options.is_empty() {
        tracing::info!(
            project = %project.slug,
            options = ?request.options,
            "收到 push-option"
        );
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(project.id.to_string())
        .fetch_one(&mut *tx)
        .await?;

    let mut unpack_error = None;
    if let Err(error) = unpack_pack(&mut tx, project.id, &request.pack).await {
        unpack_error = Some(error.to_string());
    }

    // 第一轮只做校验，收集每条命令的校验结果；atomic 推送据此决定是否整体放弃。
    // 同时保留 old/new，供 ref 变更后构造 RefUpdate（tag 与 delete 也需要）。
    let mut checked: Vec<(String, Oid, Oid, Result<CommandAction, String>)> = Vec::new();
    for (old, new, name) in &request.commands {
        let name = name.clone();
        let result = match &unpack_error {
            Some(error) => Err(error.clone()),
            None => match validate_command(&name, *old, *new, &project.default_branch) {
                Err(reason) => Err(reason),
                Ok(action) => {
                    let current = store::get_ref(&mut tx, project.id, &name)
                        .await
                        .unwrap_or(None)
                        .unwrap_or_else(Oid::zero);
                    if current != *old {
                        Err("远端引用已变化，请先 fetch 后重试".to_string())
                    } else if let CommandAction::Set {
                        target,
                        is_head,
                        ..
                    } = &action
                    {
                        let peeled = peel_tag_once(&mut tx, project.id, *target)
                            .await
                            .unwrap_or(*target);
                        let commit_ok = matches!(
                            store::get_object(&mut tx, project.id, peeled).await,
                            Ok(Some(object)) if object.kind == ObjectKind::Commit
                        );
                        if !commit_ok {
                            Err("引用只能指向提交或标签".to_string())
                        } else if *is_head
                            && *old != Oid::zero()
                            && !is_ancestor(&mut tx, project.id, *old, peeled)
                                .await
                                .unwrap_or(false)
                        {
                            Err("只接受快进推送".to_string())
                        } else {
                            Ok(action)
                        }
                    } else {
                        Ok(action)
                    }
                }
            },
        };
        checked.push((name, *old, *new, result));
    }

    let has_failure = checked.iter().any(|(_, _, _, result)| result.is_err());
    let mut statuses: Vec<(String, bool, String)> = Vec::new();
    let mut updates: Vec<crate::workflow::event::RefUpdate> = Vec::new();

    if atomic && has_failure {
        // 原子推送：任何一条失败都不落库，其余命令报告回滚。
        for (name, _, _, result) in checked {
            match result {
                Err(reason) => statuses.push((name, false, reason)),
                Ok(_) => statuses.push((
                    name,
                    false,
                    "原子推送失败，所有引用均未更新".to_string(),
                )),
            }
        }
    } else {
        for (name, old, _new, result) in checked {
            match result {
                Err(reason) => statuses.push((name, false, reason)),
                Ok(CommandAction::Delete(name)) => {
                    store::delete_ref(&mut tx, project.id, &name).await?;
                    updates.push(crate::workflow::event::RefUpdate {
                        name: name.clone(),
                        before: Some(old.as_bytes().to_vec()),
                        after: None,
                    });
                    statuses.push((name, true, String::new()));
                }
                Ok(CommandAction::Set { name, target, .. }) => {
                    store::set_ref(&mut tx, project.id, &name, target).await?;
                    let peeled = peel_tag_once(&mut tx, project.id, target).await?;
                    index_commits(&mut tx, project.id, peeled, user).await?;
                    updates.push(crate::workflow::event::RefUpdate {
                        name: name.clone(),
                        before: if old == Oid::zero() {
                            None
                        } else {
                            Some(old.as_bytes().to_vec())
                        },
                        after: Some(target.as_bytes().to_vec()),
                    });
                    statuses.push((name, true, String::new()));
                }
            }
        }
    }

    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project.id)
        .execute(&mut *tx)
        .await?;

    // 事件与 refs 更新同事务：提交失败不会触发构建，构建也不会读到旧 refs。
    for u in &updates {
        let kind = if u.name.starts_with("refs/tags/") {
            "tag"
        } else {
            "push"
        };
        let payload = serde_json::json!({
            "ref": u.name,
            "before": u.before.as_ref().map(hex::encode),
            "after": u.after.as_ref().map(hex::encode),
            "kychnet": { "trigger": kind, "snapshot_source": "api" },
        });
        crate::workflow::event::enqueue(
            &mut tx,
            &crate::workflow::event::WorkflowEvent {
                project_id: project.id,
                kind: kind.to_string(),
                ref_name: u.name.clone(),
                before_oid: u.before.clone(),
                after_oid: u.after.clone(),
                pr_number: None,
                actor_id: Some(user.id),
                payload,
                dedupe_key: Some(crate::workflow::event::dedupe_key(
                    project.id, kind, &u.name, None,
                )),
            },
        )
        .await?;
    }

    // 来源分支有新提交时，通知该分支上的开放 PR（对齐 pull_request.synchronize）。
    for u in &updates {
        let Some(after) = u.after.as_ref() else {
            continue;
        };
        let Some(branch) = u.name.strip_prefix("refs/heads/") else {
            continue;
        };
        let prs: Vec<(i32,)> = sqlx::query_as(
            "SELECT number FROM pull_requests
              WHERE project_id = $1 AND source_branch = $2 AND status = 'open'",
        )
        .bind(project.id)
        .bind(branch)
        .fetch_all(&mut *tx)
        .await?;
        for (number,) in prs {
            crate::workflow::event::enqueue(
                &mut tx,
                &crate::workflow::event::WorkflowEvent {
                    project_id: project.id,
                    kind: "pr_sync".to_string(),
                    ref_name: u.name.clone(),
                    before_oid: u.before.clone(),
                    after_oid: u.after.clone(),
                    pr_number: Some(number),
                    actor_id: Some(user.id),
                    payload: serde_json::json!({
                        "action": "synchronize",
                        "number": number,
                        "after": hex::encode(after),
                        "pull_request": { "head": { "ref": branch } },
                    }),
                    dedupe_key: Some(crate::workflow::event::dedupe_key(
                        project.id,
                        "pr_sync",
                        &u.name,
                        Some(number),
                    )),
                },
            )
            .await?;
        }
    }
    tx.commit().await?;

    let mut out = Vec::new();
    match &unpack_error {
        Some(error) => out.extend(proto::pkt_str(&format!("unpack {error}\n"))),
        None => out.extend(proto::pkt_str("unpack ok\n")),
    }
    for (name, ok, reason) in statuses {
        if ok {
            out.extend(proto::pkt_str(&format!("ok {name}\n")));
        } else {
            out.extend(proto::pkt_str(&format!("ng {name} {reason}\n")));
        }
    }
    out.extend(proto::flush());
    Ok(ReceiveOutcome { response: out })
}

/// `/git/` 下并列的两类通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitChannel {
    /// `<slug>.git/...`：Git 客户端用的 smart HTTP（info/refs、upload-pack、
    /// receive-pack、LFS），保留原有 Git 协议行为。
    Smart,
    /// `<slug>/...`：站内文件树接口（tree / file / archive），可读；
    /// 登录用户可写。
    Worktree,
}

/// 解析 `/git/<...>/...` 为项目名、动作与通道。
///
/// 以 `.git` 结尾的是 Git 客户端的仓库地址；不带后缀的是文件树接口。
/// 两者读的是同一份 PG `git_objects`，只是输出形式不同（packfile /
/// 展开后的文件树）。
pub fn split_git_path(path: &str) -> Result<(String, String, GitChannel)> {
    let (repo, action) = path
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Git 路径不完整"))?;
    let (slug, channel) = match repo.strip_suffix(".git") {
        Some(value) => (value, GitChannel::Smart),
        None => (repo, GitChannel::Worktree),
    };
    if slug.is_empty() {
        bail!("缺少项目名");
    }
    Ok((percent_decode(slug)?, action.to_string(), channel))
}

/// 只接受 `%XX` 形式的百分号编码，避免把项目名里的 `+` 误当成空格。
fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("百分号编码被截断"))?;
            let low = *bytes
                .get(index + 2)
                .ok_or_else(|| anyhow::anyhow!("百分号编码被截断"))?;
            let high = (high as char)
                .to_digit(16)
                .ok_or_else(|| anyhow::anyhow!("百分号编码非法"))?;
            let low = (low as char)
                .to_digit(16)
                .ok_or_else(|| anyhow::anyhow!("百分号编码非法"))?;
            out.push((high * 16 + low) as u8);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| anyhow::anyhow!("项目名不是合法 UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_git_target_and_decodes_unicode() {
        let (slug, action, channel) =
            split_git_path("%E6%88%91%E7%9A%84%E9%A1%B9%E7%9B%AE.git/info/refs").unwrap();
        assert_eq!(slug, "我的项目");
        assert_eq!(action, "info/refs");
        assert_eq!(channel, GitChannel::Smart);
    }

    #[test]
    fn splits_worktree_target_without_dot_git() {
        let (slug, action, channel) =
            split_git_path("%E6%88%91%E7%9A%84%E9%A1%B9%E7%9B%AE/tree").unwrap();
        assert_eq!(slug, "我的项目");
        assert_eq!(action, "tree");
        assert_eq!(channel, GitChannel::Worktree);
    }

    #[test]
    fn percent_decode_keeps_plus() {
        assert_eq!(percent_decode("a+b").unwrap(), "a+b");
        assert_eq!(percent_decode("a%2Bb").unwrap(), "a+b");
    }

    #[test]
    fn parses_commit_metadata() {
        let data = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
parent 0000000000000000000000000000000000000001\n\
author Kych <a@b.c> 100 +0000\n\
committer Kych <a@b.c> 200 +0000\n\
\n\
hello\n";
        let info = parse_commit(data).unwrap();
        assert_eq!(info.parents.len(), 1);
        assert_eq!(info.author, "Kych");
        assert_eq!(info.message, "hello");
        assert_eq!(info.committed_at.timestamp(), 200);
    }

    #[test]
    fn validates_branch_and_tag_push_commands() {
        let target = Oid::from_hex("1111111111111111111111111111111111111111").unwrap();
        let zero = Oid::zero();
        assert!(matches!(
            validate_command("refs/heads/main", zero, target, "main"),
            Ok(CommandAction::Set { is_head: true, .. })
        ));
        assert!(matches!(
            validate_command("refs/tags/v1", zero, target, "main"),
            Ok(CommandAction::Set { is_head: false, .. })
        ));
        assert!(validate_command("refs/notes/x", zero, target, "main").is_err());
        assert!(matches!(
            validate_command("refs/heads/main", target, zero, "main"),
            Err(_)
        ));
        assert!(matches!(
            validate_command("refs/tags/v1", target, zero, "main"),
            Ok(CommandAction::Delete(_))
        ));
    }

    #[test]
    fn parses_tag_object_target() {
        let oid = Oid::from_hex("2222222222222222222222222222222222222222").unwrap();
        let data = format!(
            "object {}\ntype commit\ntag v1\ntagger Kych <a@b.c> 1 +0000\n\nmsg\n",
            oid.to_hex()
        );
        assert_eq!(tag_target(data.as_bytes()).unwrap(), Some(oid));
    }
}
