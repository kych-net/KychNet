//! P1 管理端所需的工作流/执行器读写。
//!
//! 这里只提供管理员 API 的数据层；认证与 step-up 在 `api.rs` 的 handler 里做。

use anyhow::{bail, Context, Result};
use rand::{rngs::OsRng, RngCore};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::dispatch::manual_payload;
use super::event::{enqueue, WorkflowEvent};
use super::resolve_workflow_dir;
use crate::git::{store, tree};
use crate::project;

#[derive(Debug, Serialize)]
pub struct WorkflowItem {
    pub path: String,
    pub enabled: bool,
    pub approved_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct RunItem {
    pub id: Uuid,
    pub workflow_path: String,
    pub event_kind: String,
    pub ref_name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub skipped_reason: Option<String>,
    pub attempt: i32,
    pub workflow_dir: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct RunnerItem {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub protocol_version: i32,
    pub labels: Vec<String>,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<(String, String, Vec<WorkflowItem>)> {
    let project = project::get_project(pool, project_id)
        .await?
        .context("项目不存在")?;
    let workflow_dir = resolve_workflow_dir(pool, &project.workflow_dir).await?;
    let mut conn = pool.acquire().await?;
    let Some(head) =
        store::resolve_branch(&mut conn, project_id, &project.default_branch).await?
    else {
        return Ok((workflow_dir, project.default_branch, Vec::new()));
    };
    let prefix = format!("{}/", workflow_dir.trim_end_matches('/'));
    let files = tree::read_files(&mut conn, project_id, head).await?;
    let discovered: Vec<String> = files
        .into_keys()
        .filter(|path| {
            path.starts_with(&prefix)
                && (path.ends_with(".yml") || path.ends_with(".yaml"))
                && path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
        })
        .collect();

    let enabled_rows: Vec<(String, bool, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as(
            "SELECT path, enabled, approved_at FROM project_workflows WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut enabled = std::collections::HashMap::new();
    for (path, flag, approved) in enabled_rows {
        enabled.insert(path, (flag, approved));
    }
    let mut items = Vec::new();
    for path in discovered {
        let (flag, approved) = enabled.get(&path).copied().unwrap_or((false, None));
        items.push(WorkflowItem {
            path,
            enabled: flag,
            approved_at: approved,
        });
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((workflow_dir, project.default_branch, items))
}

pub async fn set_enabled(
    pool: &PgPool,
    project_id: Uuid,
    path: &str,
    enabled: bool,
    admin_id: Uuid,
) -> Result<()> {
    let (_, _, items) = list_for_project(pool, project_id).await?;
    if !items.iter().any(|item| item.path == path) {
        bail!("工作流定义不存在: {path}");
    }
    sqlx::query(
        r#"INSERT INTO project_workflows (id, project_id, path, enabled, approved_by, approved_at)
           VALUES ($1, $2, $3, $4, $5, CASE WHEN $4 THEN now() ELSE NULL END)
           ON CONFLICT (project_id, path) DO UPDATE
             SET enabled = EXCLUDED.enabled,
                 approved_by = CASE WHEN EXCLUDED.enabled THEN EXCLUDED.approved_by ELSE project_workflows.approved_by END,
                 approved_at = CASE WHEN EXCLUDED.enabled THEN now() ELSE project_workflows.approved_at END,
                 updated_at = now()"#,
    )
    .bind(Uuid::new_v4())
    .bind(project_id)
    .bind(path)
    .bind(enabled)
    .bind(admin_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn dispatch(
    pool: &PgPool,
    project_id: Uuid,
    admin_id: Uuid,
    actor_name: &str,
    inputs: Value,
) -> Result<()> {
    let project = project::get_project(pool, project_id)
        .await?
        .context("项目不存在")?;
    let mut conn = pool.acquire().await?;
    let head = store::resolve_branch(&mut conn, project_id, &project.default_branch)
        .await?
        .context("默认分支不存在")?;
    drop(conn);
    let mut tx = pool.begin().await?;
    enqueue(
        &mut tx,
        &WorkflowEvent {
            project_id,
            kind: "manual".to_string(),
            ref_name: format!("refs/heads/{}", project.default_branch),
            before_oid: None,
            after_oid: Some(head.as_bytes().to_vec()),
            pr_number: None,
            actor_id: Some(admin_id),
            payload: manual_payload(actor_name, inputs),
            dedupe_key: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn runs(pool: &PgPool, project_id: Uuid) -> Result<Vec<RunItem>> {
    let workflow_dir: String =
        sqlx::query_scalar("SELECT workflow_dir FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_one(pool)
            .await?;
    let workflow_dir = resolve_workflow_dir(pool, &workflow_dir).await?;
    let rows: Vec<(Uuid, String, String, String, String, Option<String>, Option<String>, i32, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as(
            "SELECT id, workflow_path, event_kind, ref_name, status, conclusion,
                    skipped_reason, attempt, created_at, finished_at
               FROM workflow_runs WHERE project_id = $1
              ORDER BY created_at DESC LIMIT 50",
        )
        .bind(project_id)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| RunItem {
            id: r.0,
            workflow_path: r.1,
            event_kind: r.2,
            ref_name: r.3,
            status: r.4,
            conclusion: r.5,
            skipped_reason: r.6,
            attempt: r.7,
            workflow_dir: workflow_dir.clone(),
            created_at: r.8,
            finished_at: r.9,
        })
        .collect())
}

pub async fn runners(pool: &PgPool) -> Result<Vec<RunnerItem>> {
    Ok(sqlx::query_as::<_, RunnerItem>(
        "SELECT id, name, enabled, protocol_version, labels, last_seen_at
           FROM workflow_runners ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?)
}

pub async fn create_registration_code(
    pool: &PgPool,
    name: &str,
    labels: Vec<String>,
) -> Result<(Uuid, String)> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        bail!("执行器名称需为 1-80 个字符");
    }
    let mut code_bytes = [0u8; 24];
    OsRng.fill_bytes(&mut code_bytes);
    let code = hex::encode(code_bytes);
    let registration_hash = Sha256::digest(code.as_bytes()).to_vec();
    let mut placeholder = [0u8; 32];
    OsRng.fill_bytes(&mut placeholder);
    let placeholder_hash = Sha256::digest(placeholder).to_vec();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_runners
           (id, name, token_hash, registration_code_hash, labels)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(name)
    .bind(placeholder_hash)
    .bind(registration_hash)
    .bind(labels)
    .execute(pool)
    .await
    .context("创建注册码失败（名称可能重复）")?;
    Ok((id, code))
}

pub async fn set_runner_enabled(pool: &PgPool, id: Uuid, enabled: bool) -> Result<()> {
    let result = sqlx::query("UPDATE workflow_runners SET enabled = $2 WHERE id = $1")
        .bind(id)
        .bind(enabled)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("执行器不存在");
    }
    Ok(())
}
