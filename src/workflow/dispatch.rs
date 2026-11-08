//! 分发器与后台扫描器。
//!
//! - `spawn_dispatcher`：轮询事件 outbox，把事件拆成「内置发布动作」与「YAML 工作流 run」两条支路。
//! - `spawn_pruner`：过期事件、租约超时、队列积压、run 过期。
//! - `pending_commits`：给 `gc` 用，避免清理掉排队 run 还需要的 Git 对象。

use anyhow::{Context, Result};
use serde_json::json;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

use super::runner_api::WorkflowState;
use crate::git::object::Oid;
use crate::git::store;
use crate::git::tree;
use crate::workflow::queue::BuildQueue;

/// 事件提交后由后台轮询处理，push 响应不等待任何构建或执行器。
pub fn spawn_dispatcher(state: WorkflowState, build_queue: BuildQueue) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match tick(&state, &build_queue).await {
                Ok(true) => {} // 处理了一条，立刻看下一跳
                Ok(false) => tokio::time::sleep(Duration::from_millis(500)).await,
                Err(error) => {
                    warn!("工作流分发失败：{error:#}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    })
}

/// 取一条未消费事件并在事务内完成分发，返回是否处理了事件。
async fn tick(state: &WorkflowState, build_queue: &BuildQueue) -> Result<bool> {
    let mut tx = state.pool.begin().await.context("开启分发事务失败")?;
    let event: Option<EventRow> = sqlx::query_as(
        r#"SELECT id, project_id, kind, ref_name, after_oid, pr_number, actor_id, payload
             FROM workflow_events
            WHERE status = 'queued' AND expires_at > now()
            ORDER BY created_at
            LIMIT 1
            FOR UPDATE SKIP LOCKED"#,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(event) = event else {
        tx.rollback().await.ok();
        return Ok(false);
    };

    // ---- 支路 1：内置发布动作，恒执行，与执行器是否存在无关 ----
    let project_meta: (String, String, bool) = sqlx::query_as(
        "SELECT default_branch, workflow_dir, require_workflow_checks
           FROM projects WHERE id = $1",
    )
    .bind(event.project_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap_or_else(|_| ("main".to_string(), String::new(), false));
    let (default_branch, workflow_dir, require_checks) = project_meta;
    let workflow_dir = super::resolve_workflow_dir(&state.pool, &workflow_dir).await?;
    let on_default = event.ref_name == format!("refs/heads/{default_branch}");
    let wants_publish = matches!(event.kind.as_str(), "push" | "pr_merged" | "manual") && on_default;
    if wants_publish {
        build_queue.enqueue();
    }

    // ---- 支路 2：YAML 工作流，定义只从默认分支 head 读 ----
    let def_ref = format!("refs/heads/{default_branch}");
    let def_oid = store::get_ref(&mut tx, event.project_id, &def_ref).await?;
    let definitions = match def_oid {
        Some(oid) => discover_definitions(&mut tx, event.project_id, oid, &workflow_dir).await?,
        None => Vec::new(),
    };

    // 新路径登记为 enabled=false，需管理员显式启用。
    for path in &definitions {
        sqlx::query(
            "INSERT INTO project_workflows (id, project_id, path) VALUES ($1,$2,$3)
             ON CONFLICT (project_id, path) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(event.project_id)
        .bind(path)
        .execute(&mut *tx)
        .await?;
    }
    let enabled: Vec<String> =
        sqlx::query_scalar("SELECT path FROM project_workflows WHERE project_id = $1 AND enabled")
            .bind(event.project_id)
            .fetch_all(&mut *tx)
            .await?;

    let available = executor_available(&state.pool, state.cfg.protocol_version).await?;
    let project_enabled: bool = sqlx::query_scalar::<_, bool>(
        "SELECT workflow_executor_enabled FROM projects WHERE id = $1",
    )
    .bind(event.project_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap_or(false);

    let commit_oid = event.after_oid.clone().or_else(|| def_oid.map(|o| o.as_bytes().to_vec()));
    let trust = super::event::trust_level(&event.kind, &event.ref_name, &default_branch);

    for path in &enabled {
        let run_id = Uuid::new_v4();
        let (status, skipped_reason) = if !available {
            ("skipped", Some("no_runner"))
        } else if !project_enabled {
            ("skipped", Some("disabled"))
        } else {
            ("queued", None)
        };
        sqlx::query(
            r#"INSERT INTO workflow_runs
                 (id, project_id, event_id, workflow_path, event_kind, ref_name,
                  commit_oid, def_commit_oid, pr_number, actor_id, trust_level,
                  status, skipped_reason, lease_until)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,
                       CASE WHEN $12 = 'queued' THEN NULL ELSE NULL END)"#,
        )
        .bind(run_id)
        .bind(event.project_id)
        .bind(event.id)
        .bind(path)
        .bind(&event.kind)
        .bind(&event.ref_name)
        .bind(commit_oid.as_deref())
        .bind(def_oid.map(|o| o.as_bytes().to_vec()))
        .bind(event.pr_number)
        .bind(event.actor_id)
        .bind(trust)
        .bind(status)
        .bind(skipped_reason)
        .execute(&mut *tx)
        .await?;

        // 执行器缺席时不写 required check，避免「没装执行器就永远不能合并 PR」。
        if let Some(pr) = event.pr_number {
            sqlx::query(
                r#"INSERT INTO pr_checks
                     (id, project_id, pr_number, run_id, name, conclusion, required, commit_oid)
                   VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
                   ON CONFLICT (run_id, name) DO UPDATE
                     SET conclusion = EXCLUDED.conclusion,
                         required = EXCLUDED.required"#,
            )
            .bind(Uuid::new_v4())
            .bind(event.project_id)
            .bind(pr)
            .bind(run_id)
            .bind(path)
            .bind(if status == "skipped" { "skipped" } else { "pending" })
            .bind(status == "queued" && require_checks)
            .bind(commit_oid.as_deref())
            .execute(&mut *tx)
            .await?;
        }
    }

    sqlx::query("UPDATE workflow_events SET status='done', consumed_at=now() WHERE id=$1")
        .bind(event.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await.context("提交分发事务失败")?;
    info!(
        "事件 {} 分发完成：定义 {} 个 / 启用 {} 个 / 执行器{}",
        event.id,
        definitions.len(),
        enabled.len(),
        if available { "在线" } else { "缺席" }
    );
    Ok(true)
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    project_id: Uuid,
    kind: String,
    ref_name: String,
    after_oid: Option<Vec<u8>>,
    pr_number: Option<i32>,
    actor_id: Option<Uuid>,
    #[allow(dead_code)]
    payload: serde_json::Value,
}

/// 从默认分支快照里找 `<workflow_dir>/*.yml|*.yaml`。
async fn discover_definitions(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
    commit: Oid,
    workflow_dir: &str,
) -> Result<Vec<String>> {
    let prefix = format!("{}/", workflow_dir.trim_end_matches('/'));
    let files = tree::read_files(conn, project_id, commit).await?;
    let mut out: Vec<String> = files
        .into_keys()
        .filter(|p| {
            let lower = p.to_ascii_lowercase();
            p.starts_with(&prefix) && (lower.ends_with(".yml") || lower.ends_with(".yaml"))
        })
        .collect();
    out.sort();
    Ok(out)
}

pub async fn executor_available(pool: &PgPool, protocol_version: i32) -> Result<bool> {
    let ok: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM workflow_runners
              WHERE enabled AND protocol_version = $1
                AND last_seen_at > now() - interval '2 minutes')"#,
    )
    .bind(protocol_version)
    .fetch_one(pool)
    .await?;
    Ok(ok)
}

/// 后台扫描：过期事件、租约超时、队列积压、run 过期。
pub fn spawn_pruner(state: WorkflowState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = prune(&state).await {
                warn!("工作流清理扫描失败：{error:#}");
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    })
}

async fn prune(state: &WorkflowState) -> Result<()> {
    // 事件过期
    sqlx::query(
        "UPDATE workflow_events SET status='expired'
          WHERE status='queued' AND expires_at < now()",
    )
    .execute(&state.pool)
    .await?;

    // 租约超时：打回 queued，attempt+1；超过 3 次标 cancelled / runner_lost
    sqlx::query(
        r#"UPDATE workflow_runs
              SET status='queued', executor_id=NULL, lease_until=NULL,
                  started_at=NULL, attempt=attempt+1
            WHERE status='in_progress' AND lease_until < now() AND attempt < 3"#,
    )
    .execute(&state.pool)
    .await?;
    sqlx::query(
        r#"UPDATE workflow_runs
              SET status='cancelled', conclusion='runner_lost', finished_at=now()
            WHERE status='in_progress' AND lease_until < now() AND attempt >= 3"#,
    )
    .execute(&state.pool)
    .await?;

    // 队列积压：把超出上限的最老 queued run 标 expired
    sqlx::query(
        r#"UPDATE workflow_runs SET status='expired'
            WHERE id IN (
              SELECT id FROM workflow_runs WHERE status='queued'
              ORDER BY created_at
              OFFSET $1
            )"#,
    )
    .bind(state.cfg.queue_max as i64)
    .execute(&state.pool)
    .await?;

    // run 自身过期
    sqlx::query(
        "UPDATE workflow_runs SET status='expired'
          WHERE status='queued' AND expires_at < now()",
    )
    .execute(&state.pool)
    .await?;

    // 门禁不能永久停在 pending：run 被取消或过期时，把仍 pending 的 check
    // 落成 failure，让合并入口得到明确结果。
    sqlx::query(
        r#"UPDATE pr_checks c SET conclusion='failure'
             FROM workflow_runs r
            WHERE c.run_id = r.id AND c.conclusion = 'pending'
              AND r.status IN ('cancelled','expired')"#,
    )
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// 供 `gc` 调用：排队/执行中的 run 引用的 commit 不能被清理。
pub async fn pending_commits(pool: &PgPool) -> Result<Vec<Vec<u8>>> {
    type PendingRow = (Option<Vec<u8>>, Option<Vec<u8>>);
    let rows: Vec<PendingRow> = sqlx::query_as(
        r#"SELECT commit_oid, def_commit_oid FROM workflow_runs
            WHERE status IN ('queued','in_progress') AND expires_at > now()"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .flat_map(|(a, b)| [a, b].into_iter().flatten())
        .collect())
}

/// 手动触发 / 定时触发的事件构造 helper。
///
/// P1：管理端 `workflow_dispatch` 端点尚未接入，本 helper 供该端点直接调用；
/// 事件类型 `manual` 在 `event::trust_level` / 本文件分发逻辑中已完整支持。
#[allow(dead_code)]
pub fn manual_payload(actor: &str, inputs: serde_json::Value) -> serde_json::Value {
    json!({
        "inputs": inputs,
        "sender": { "login": actor },
        "kychnet": { "trigger": "manual" }
    })
}
