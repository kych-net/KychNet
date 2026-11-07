use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::object::{Object, ObjectKind, Oid};

/// 写入对象并返回 OID。同 OID 已存在时直接复用。
pub async fn put_object(
    conn: &mut PgConnection,
    project_id: Uuid,
    object: &Object,
) -> Result<Oid> {
    let oid = object.oid();
    sqlx::query(
        "INSERT INTO git_objects (project_id, oid, kind, size, data)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (project_id, oid) DO NOTHING",
    )
    .bind(project_id)
    .bind(&oid.as_bytes()[..])
    .bind(object.kind as i16)
    .bind(object.data.len() as i64)
    .bind(&object.data)
    .execute(conn)
    .await?;
    Ok(oid)
}

pub async fn get_object(
    conn: &mut PgConnection,
    project_id: Uuid,
    oid: Oid,
) -> Result<Option<Object>> {
    let row = sqlx::query("SELECT kind, data FROM git_objects WHERE project_id = $1 AND oid = $2")
        .bind(project_id)
        .bind(&oid.as_bytes()[..])
        .fetch_optional(conn)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let kind: i16 = row.try_get("kind")?;
    let data: Vec<u8> = row.try_get("data")?;
    let kind = ObjectKind::from_i16(kind)
        .ok_or_else(|| anyhow::anyhow!("未知的 Git 对象类型: {kind}"))?;
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
