use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool, Row};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

use super::object::{
    commit_tree, ensure_text, Object, ObjectKind, Oid, Tree, TreeEntry,
};
use super::proto;
use super::store;
use crate::models::User;
use crate::project::Project;

const UPLOAD_CAPS: &str = "agent=kych-net/0.3";
const RECEIVE_CAPS: &str = "report-status delete-refs ofs-delta agent=kych-net/0.3";

/// 单次推送最多解析的提交数量，避免超大仓库把内存与数据库拖垮。
const MAX_COMMIT_WALK: usize = 5000;

pub struct ReceiveOutcome {
    pub response: Vec<u8>,
    pub default_branch_updated: bool,
}

fn advertise_line(oid: Oid, name: &str, caps: Option<&str>) -> Vec<u8> {
    match caps {
        Some(caps) => proto::pkt_str(&format!("{} {name}\0{caps}\n", oid.to_hex())),
        None => proto::pkt_str(&format!("{} {name}\n", oid.to_hex())),
    }
}

/// 生成 smart HTTP 的引用广告。
pub async fn advertise(pool: &PgPool, project: &Project, service: &str) -> Result<Vec<u8>> {
    let mut conn = pool.acquire().await?;
    let refs = store::list_refs(&mut conn, project.id).await?;
    let heads: Vec<(String, Oid)> = refs
        .into_iter()
        .filter(|(name, _)| name.starts_with("refs/heads/"))
        .collect();

    let caps = if service == "git-upload-pack" {
        UPLOAD_CAPS
    } else {
        RECEIVE_CAPS
    };

    let mut out = Vec::new();
    out.extend(proto::pkt_str(&format!("# service={service}\n")));
    out.extend(proto::flush());

    if heads.is_empty() {
        out.extend(proto::pkt_str(&format!(
            "{} capabilities^{{}}\0{caps}\n",
            Oid::zero().to_hex()
        )));
    } else {
        let default_ref = format!("refs/heads/{}", project.default_branch);
        let head_target = heads
            .iter()
            .find(|(name, _)| name == &default_ref)
            .map(|(_, oid)| *oid);
        let mut first = true;
        if let Some(oid) = head_target {
            out.extend(advertise_line(oid, "HEAD", Some(caps)));
            first = false;
        }
        for (name, oid) in &heads {
            let line_caps = if first { Some(caps) } else { None };
            out.extend(advertise_line(*oid, name, line_caps));
            first = false;
        }
    }
    out.extend(proto::flush());
    Ok(out)
}

fn read_wants(body: &[u8]) -> Result<Vec<Oid>> {
    let (lines, _) = proto::parse_pkt_lines(body)?;
    let mut wants = Vec::new();
    for line in lines {
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches('\n');
        if let Some(rest) = text.strip_prefix("want ") {
            let hex = rest.split(' ').next().unwrap_or_default();
            wants.push(Oid::from_hex(hex)?);
        }
    }
    Ok(wants)
}

/// 收集 want 可达的全部对象（忽略 have，代价是可能多传一些对象，但语义正确）。
async fn collect_reachable(
    conn: &mut PgConnection,
    project_id: Uuid,
    wants: &[Oid],
) -> Result<Vec<(ObjectKind, Vec<u8>)>> {
    let mut seen = HashSet::new();
    let mut objects = Vec::new();
    let mut stack: Vec<Oid> = wants.to_vec();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let Some(object) = store::get_object(conn, project_id, oid).await? else {
            bail!("请求的对象不存在: {oid}");
        };
        match object.kind {
            ObjectKind::Commit => {
                stack.push(commit_tree(&object.data)?);
                stack.extend(super::object::commit_parents(&object.data)?);
            }
            ObjectKind::Tree => {
                let tree = Tree::parse(&object.data)?;
                for entry in tree.entries.values() {
                    match entry {
                        TreeEntry::Blob(oid) | TreeEntry::Dir(oid) => stack.push(*oid),
                    }
                }
            }
            _ => {}
        }
        objects.push((object.kind, object.data));
    }
    Ok(objects)
}

pub async fn upload_pack(pool: &PgPool, project: &Project, body: &[u8]) -> Result<Vec<u8>> {
    let wants = read_wants(body)?;
    let mut out = Vec::new();
    if wants.is_empty() {
        out.extend(proto::pkt_str("NAK\n"));
        return Ok(out);
    }
    let mut conn = pool.acquire().await?;
    let objects = collect_reachable(&mut conn, project.id, &wants).await?;
    out.extend(proto::pkt_str("NAK\n"));
    out.extend(proto::build_pack(&objects));
    Ok(out)
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

fn parse_commands(lines: &[Vec<u8>]) -> Result<Vec<(Oid, Oid, String)>> {
    let mut commands = Vec::new();
    for line in lines {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\n');
        if text.is_empty() {
            continue;
        }
        let head = text.split('\0').next().unwrap_or(text);
        let mut parts = head.split(' ');
        let old = Oid::from_hex(parts.next().unwrap_or_default())?;
        let new = Oid::from_hex(parts.next().unwrap_or_default())?;
        let name = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("推送命令缺少引用名"))?;
        commands.push((old, new, name.to_string()));
    }
    Ok(commands)
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
        if *kind == ObjectKind::Blob {
            ensure_text(object)?;
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

pub async fn receive_pack(
    pool: &PgPool,
    project: &Project,
    user: &User,
    body: &[u8],
) -> Result<ReceiveOutcome> {
    let (lines, consumed) = proto::parse_pkt_lines(body)?;
    let commands = parse_commands(&lines)?;
    let pack_data = &body[consumed..];

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(project.id.to_string())
        .fetch_one(&mut *tx)
        .await?;

    let mut unpack_error = None;
    if let Err(error) = unpack_pack(&mut tx, project.id, pack_data).await {
        unpack_error = Some(error.to_string());
    }

    let mut statuses: Vec<(String, bool, String)> = Vec::new();
    let mut default_branch_updated = false;

    for (old, new, name) in &commands {
        let Some(branch) = name.strip_prefix("refs/heads/") else {
            statuses.push((name.clone(), false, "只允许推送分支".to_string()));
            continue;
        };
        if !crate::project::valid_branch(branch) {
            statuses.push((name.clone(), false, "非法分支名".to_string()));
            continue;
        }
        if let Some(error) = &unpack_error {
            statuses.push((name.clone(), false, error.clone()));
            continue;
        }

        let current = store::get_ref(&mut tx, project.id, name)
            .await?
            .unwrap_or_else(Oid::zero);
        if current != *old {
            statuses.push((
                name.clone(),
                false,
                "远端引用已变化，请先 fetch 后重试".to_string(),
            ));
            continue;
        }

        if *new == Oid::zero() {
            if branch == project.default_branch {
                statuses.push((name.clone(), false, "不能删除默认分支".to_string()));
                continue;
            }
            store::delete_ref(&mut tx, project.id, name).await?;
            statuses.push((name.clone(), true, String::new()));
            continue;
        }

        let Some(object) = store::get_object(&mut tx, project.id, *new).await? else {
            statuses.push((name.clone(), false, "提交对象缺失".to_string()));
            continue;
        };
        if object.kind != ObjectKind::Commit {
            statuses.push((name.clone(), false, "引用只能指向提交".to_string()));
            continue;
        }
        if *old != Oid::zero() && !is_ancestor(&mut tx, project.id, *old, *new).await? {
            statuses.push((name.clone(), false, "只接受快进推送".to_string()));
            continue;
        }

        store::set_ref(&mut tx, project.id, name, *new).await?;
        index_commits(&mut tx, project.id, *new, user).await?;
        if branch == project.default_branch {
            default_branch_updated = true;
        }
        statuses.push((name.clone(), true, String::new()));
    }

    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project.id)
        .execute(&mut *tx)
        .await?;
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
    Ok(ReceiveOutcome {
        response: out,
        default_branch_updated,
    })
}

/// 从 `/git/<项目>.git/...` 的剩余路径里解析仓库名与动作。
pub fn split_target(path: &str) -> Result<(String, String)> {
    let (repo, action) = path
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Git 路径不完整"))?;
    let slug = repo
        .strip_suffix(".git")
        .ok_or_else(|| anyhow::anyhow!("Git 仓库地址必须以 .git 结尾"))?;
    if slug.is_empty() {
        bail!("缺少项目名");
    }
    Ok((percent_decode(slug)?, action.to_string()))
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

/// 供构建流程使用：读取某个提交的文件快照（未来 PR 预览会用到）。
pub async fn commit_files(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
) -> Result<BTreeMap<String, Vec<u8>>> {
    super::tree::read_files(conn, project_id, commit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_git_target_and_decodes_unicode() {
        let (slug, action) = split_target("%E6%88%91%E7%9A%84%E9%A1%B9%E7%9B%AE.git/info/refs").unwrap();
        assert_eq!(slug, "我的项目");
        assert_eq!(action, "info/refs");
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
}
