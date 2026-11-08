//! `/api/runner/*` HTTP 契约（模式 B / protocol v2）。
//!
//! 只认 runner token（SHA-256 hash），与管理端 session 认证完全隔离。
//! 快照端点在返回任何字节前，必须校验：token 有效 + executor_id 归属 +
//! `status='in_progress'` + 租约未过期 + commit 属于该 run，任一不满足 403。
//!
//! v2 起执行器直接读写 `/git/<项目>/` 工作树；`runner_write_scope()` 把同一份
//! token 复用到文件树写，但额外把写入目标限制在该 run 绑定的分支上。

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use super::snapshot;
use super::PROTOCOL_VERSION;
use crate::api::BuildCtl;
use crate::workflow::queue::BuildQueue;

#[derive(Clone)]
pub struct WorkflowState {
    pub pool: PgPool,
    pub cfg: Arc<WorkflowConfig>,
    pub build_queue: BuildQueue,
    #[allow(dead_code)]
    pub build_ctl: Arc<BuildCtl>,
}

#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    pub protocol_version: i32,
    pub queue_max: usize,
    pub run_lease_secs: i64,
    pub log_root: PathBuf,
    pub snapshot_cache: PathBuf,
    pub max_snapshot_bytes: u64,
    pub max_log_bytes: u64,
    pub lfs_root: PathBuf,
    #[allow(dead_code)]
    pub build_out_dir: PathBuf,
}

impl WorkflowConfig {
    /// 独立读环境变量，无需改动现有 `config.rs`。
    pub fn from_env() -> Self {
        fn s(k: &str, d: &str) -> String {
            std::env::var(k).unwrap_or_else(|_| d.to_string())
        }
        fn n<T: std::str::FromStr>(k: &str, d: T) -> T {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        }
        Self {
            protocol_version: PROTOCOL_VERSION,
            queue_max: n("WORKFLOW_QUEUE_MAX", 500usize),
            run_lease_secs: n("RUN_LEASE_SECS", 300i64),
            log_root: PathBuf::from(s("WORKFLOW_LOG_ROOT", "logs/workflows")),
            snapshot_cache: PathBuf::from(s(
                "WORKFLOW_SNAPSHOT_CACHE",
                "cache/workflow-snapshots",
            )),
            max_snapshot_bytes: n("MAX_SNAPSHOT_BYTES", 209_715_200u64),
            max_log_bytes: n("MAX_LOG_BYTES", 8_388_608u64),
            lfs_root: PathBuf::from(s("LFS_ROOT", "lfs")),
            build_out_dir: PathBuf::from(s("BUILD_OUT_DIR", "dist")),
        }
    }
}

/// 返回 `Router<()>`，由主服务在 `.with_state(state)` 之后
/// `.nest("/api/runner", workflow::router(wf_state))` 挂接。
pub fn router(state: WorkflowState) -> Router {
    Router::new()
        .route("/register", post(register))
        .route("/jobs/claim", get(claim))
        .route("/jobs/{id}/snapshot", get(snapshot_handler))
        .route("/jobs/{id}/start", post(start))
        .route("/jobs/{id}/logs", post(logs))
        .route("/jobs/{id}/steps", post(steps))
        .route("/jobs/{id}/complete", post(complete))
        .route("/heartbeat", post(heartbeat))
        .with_state(state)
}

fn internal(error: impl std::fmt::Display) -> StatusCode {
    tracing::warn!("workflow runner api: {error}");
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn authenticate(state: &WorkflowState, headers: &HeaderMap) -> Result<Uuid, StatusCode> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let token = raw
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let hash = Sha256::digest(token.as_bytes()).to_vec();
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM workflow_runners WHERE token_hash = $1 AND enabled",
    )
    .bind(&hash)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;
    row.map(|(id,)| id).ok_or(StatusCode::FORBIDDEN)
}

/// `/git/<项目>/file` 写授权：runner token 只能写「自己正在执行的那个 run」。
///
/// 返回该 run 绑定的分支名（`refs/heads/` 已去掉）；命中条件全部满足才放行：
/// token 有效且 executor 未禁用、run 处于 `in_progress`、租约未过期、
/// run 属于当前项目。tag / 非 refs 事件返回 None（没有可写的分支）。
///
/// 这条校验是文件树写放开给 runner token 后的**唯一**约束来源，不能省略。
pub(crate) async fn runner_write_scope(
    pool: &PgPool,
    headers: &HeaderMap,
    project_id: Uuid,
) -> anyhow::Result<Option<String>> {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    let hash = Sha256::digest(token.as_bytes()).to_vec();
    let row: Option<(String,)> = sqlx::query_as(
        r#"SELECT r.ref_name
             FROM workflow_runners e
             JOIN workflow_runs r ON r.executor_id = e.id
            WHERE e.token_hash = $1 AND e.enabled
              AND r.project_id = $2
              AND r.status = 'in_progress' AND r.lease_until > now()
            ORDER BY r.started_at DESC NULLS LAST
            LIMIT 1"#,
    )
    .bind(&hash)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(name,)| {
        name.strip_prefix("refs/heads/")
            .filter(|b| !b.is_empty())
            .map(str::to_string)
    }))
}

/// 只编码 URL 路径段里必须编码的字节（与 `api::percent_encode_segment` 同规则）。
fn encode_slug(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 幂等 + 越权校验：只接受「本人正在执行、租约有效、attempt 匹配」的回报。
async fn guard(
    state: &WorkflowState,
    run_id: Uuid,
    executor_id: Uuid,
    body: &Value,
) -> Result<(), StatusCode> {
    let attempt = body
        .get("attempt")
        .and_then(|v| v.as_i64())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let row: Option<(i32,)> = sqlx::query_as(
        r#"SELECT attempt FROM workflow_runs
            WHERE id = $1 AND executor_id = $2
              AND status = 'in_progress' AND lease_until > now()"#,
    )
    .bind(run_id)
    .bind(executor_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;
    match row {
        Some((a,)) if a as i64 == attempt => Ok(()),
        _ => Err(StatusCode::FORBIDDEN),
    }
}

fn parse_run_id(raw: &str) -> Result<Uuid, StatusCode> {
    Uuid::parse_str(raw).map_err(|_| StatusCode::BAD_REQUEST)
}

fn event_name(kind: &str) -> &'static str {
    match kind {
        "push" | "tag" => "push",
        "pr_opened" | "pr_sync" | "pr_merged" | "pr_closed" => "pull_request",
        "manual" => "workflow_dispatch",
        "schedule" => "schedule",
        _ => "push",
    }
}

// ---------------------------------------------------------------------------
// register：管理员预先 INSERT 一行带 registration_code_hash 的记录，
// 这里用一次性注册码换 token；注册码随即作废，明文 token 只回显一次。
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RegisterReq {
    name: String,
    code: String,
    labels: Option<Vec<String>>,
    protocol_version: Option<i32>,
    snapshot_source: Option<String>,
}

async fn register(
    State(state): State<WorkflowState>,
    Json(req): Json<RegisterReq>,
) -> Result<Json<Value>, StatusCode> {
    if req.protocol_version.unwrap_or(PROTOCOL_VERSION) != PROTOCOL_VERSION {
        return Err(StatusCode::BAD_REQUEST);
    }
    // 只做模式 B：拒绝任何声明 PG 直连来源的执行器。
    if req.snapshot_source.as_deref().unwrap_or("api") != "api" {
        return Err(StatusCode::BAD_REQUEST);
    }
    let code_hash = Sha256::digest(req.code.as_bytes()).to_vec();
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let token_hash = Sha256::digest(token.as_bytes()).to_vec();
    // 单条 UPDATE 完成「查注册码 + 消费 + 发 token」，并发请求同一注册码
    // 只有一个能返回 row，避免 SELECT 后再 UPDATE 的双发竞态。
    let runner: Option<(Uuid,)> = sqlx::query_as(
        r#"UPDATE workflow_runners
              SET token_hash = $1, name = $2, labels = $3,
                  last_seen_at = now(), registration_code_hash = NULL
            WHERE registration_code_hash = $4 AND enabled
            RETURNING id"#,
    )
    .bind(&token_hash)
    .bind(&req.name)
    .bind(req.labels.unwrap_or_default())
    .bind(&code_hash)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;
    let Some((runner_id,)) = runner else {
        return Err(StatusCode::FORBIDDEN);
    };

    Ok(Json(json!({ "runner_id": runner_id, "token": token })))
}

// ---------------------------------------------------------------------------
// claim：长轮询 + FOR UPDATE SKIP LOCKED 抢占
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ClaimQuery {
    protocol_version: Option<i32>,
    wait: Option<u64>,
    #[allow(dead_code)]
    labels: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ClaimRow {
    id: Uuid,
    project_id: Uuid,
    workflow_path: String,
    event_kind: String,
    ref_name: String,
    commit_oid: Option<Vec<u8>>,
    def_commit_oid: Option<Vec<u8>>,
    pr_number: Option<i32>,
    trust_level: String,
    attempt: i32,
    event_id: Option<i64>,
}

async fn claim(
    State(state): State<WorkflowState>,
    Query(q): Query<ClaimQuery>,
    headers: HeaderMap,
) -> Response {
    let executor_id = match authenticate(&state, &headers).await {
        Ok(id) => id,
        Err(s) => return s.into_response(),
    };
    if q.protocol_version.unwrap_or(PROTOCOL_VERSION) != PROTOCOL_VERSION {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let wait = q.wait.unwrap_or(30).min(60);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
    loop {
        match claim_one(&state, executor_id).await {
            Ok(Some(v)) => return Json(v).into_response(),
            Ok(None) => {
                if tokio::time::Instant::now() >= deadline {
                    return StatusCode::NO_CONTENT.into_response();
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(s) => return s.into_response(),
        }
    }
}

async fn claim_one(state: &WorkflowState, executor_id: Uuid) -> Result<Option<Value>, StatusCode> {
    let row: Option<ClaimRow> = sqlx::query_as(
        r#"UPDATE workflow_runs
              SET status = 'in_progress', executor_id = $1,
                  lease_until = now() + ($2::int * interval '1 second'),
                  attempt = attempt + 1, started_at = now()
            WHERE id = (
              SELECT id FROM workflow_runs
               WHERE status = 'queued' AND expires_at > now()
               ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED)
            RETURNING id, project_id, workflow_path, event_kind, ref_name,
                      commit_oid, def_commit_oid, pr_number, trust_level,
                      attempt, event_id"#,
    )
    .bind(executor_id)
    .bind(state.cfg.run_lease_secs as i32)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;
    let Some(row) = row else { return Ok(None) };

    let project: (String, String, String, String) = sqlx::query_as(
        "SELECT id::text, slug, default_branch, workflow_dir FROM projects WHERE id = $1",
    )
    .bind(row.project_id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;

    let payload: Value = match row.event_id {
        Some(id) => sqlx::query_scalar("SELECT payload FROM workflow_events WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?
            .unwrap_or_else(|| json!({})),
        None => json!({}),
    };

    // protocol v2：执行器直接读写 `/git/<项目>/` 工作树，不再默认走
    // `/api/runner/jobs/{id}/snapshot`。写目标只给分支事件，tag 事件为 null。
    let git_base = format!("/git/{}", encode_slug(&project.1));
    let write_ref = row
        .ref_name
        .strip_prefix("refs/heads/")
        .filter(|b| !b.is_empty())
        .map(str::to_string);

    Ok(Some(json!({
        "run_id": row.id,
        "attempt": row.attempt,
        "project": {
            "id": project.0,
            "slug": project.1,
            "default_branch": project.2,
            "workflow_dir": project.3
        },
        "commit_oid": hex::encode(row.commit_oid.unwrap_or_default()),
        "def_commit_oid": hex::encode(row.def_commit_oid.unwrap_or_default()),
        "ref_name": row.ref_name,
        "git_base": git_base,
        "write_ref": write_ref,
        "workflow_path": row.workflow_path,
        "pr_number": row.pr_number,
        "trust_level": row.trust_level,
        // P0 不下发 secrets；PR / 非默认分支恒为空，由 trust_level 强制。
        "secrets": {},
        "event": { "name": event_name(&row.event_kind), "payload": payload },
        "limits": {
            "timeout_secs": 900,
            "max_snapshot_bytes": state.cfg.max_snapshot_bytes,
            "max_log_bytes": state.cfg.max_log_bytes,
            "network": "none"
        }
    })))
}

// ---------------------------------------------------------------------------
// ★ 模式 B 快照端点
// ---------------------------------------------------------------------------

async fn snapshot_handler(
    State(state): State<WorkflowState>,
    AxumPath(raw_id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let executor_id = match authenticate(&state, &headers).await {
        Ok(id) => id,
        Err(s) => return s.into_response(),
    };
    let id = match parse_run_id(&raw_id) {
        Ok(id) => id,
        Err(s) => return s.into_response(),
    };
    // 零新表、零签名密钥：只放行本人正在执行且租约有效的 run。
    let valid: Option<(Uuid,)> = sqlx::query_as(
        r#"SELECT id FROM workflow_runs
            WHERE id = $1 AND executor_id = $2
              AND status = 'in_progress' AND lease_until > now()"#,
    )
    .bind(id)
    .bind(executor_id)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);
    if valid.is_none() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let run = match snapshot::load_run(&state.pool, id).await {
        Ok(r) => r,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let snap = match snapshot::build_snapshot(&state, &run).await {
        Ok(s) => s,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("snapshot_unavailable: {error:#}"))
                .into_response()
        }
    };

    let mut resp = snap.gz.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
    if let Ok(v) = HeaderValue::from_str(&snap.headers.commit_oid) {
        h.insert("x-snapshot-commit", v);
    }
    if let Ok(v) = HeaderValue::from_str(&snap.headers.def_commit_oid) {
        h.insert("x-snapshot-def-commit", v);
    }
    if let Ok(v) = HeaderValue::from_str(&snap.headers.files.to_string()) {
        h.insert("x-snapshot-files", v);
    }
    if let Ok(v) = HeaderValue::from_str(&snap.headers.bytes.to_string()) {
        h.insert("x-snapshot-bytes", v);
    }
    resp
}

// ---------------------------------------------------------------------------
// 状态回报
// ---------------------------------------------------------------------------

async fn start(
    State(state): State<WorkflowState>,
    AxumPath(raw_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<StatusCode, StatusCode> {
    let executor_id = authenticate(&state, &headers).await?;
    let id = parse_run_id(&raw_id)?;
    guard(&state, id, executor_id, &body).await?;
    let name = body
        .get("workflow_path")
        .and_then(|v| v.as_str())
        .unwrap_or("workflow");
    // 预建 pending check；执行器缺席时 dispatch 已写 required=false 的 skipped。
    if let Some(pr) = body.get("jobs").and_then(|v| v.as_array()) {
        tracing::debug!("run {id} start: {name}, {} jobs", pr.len());
    }
    Ok(StatusCode::OK)
}

async fn logs(
    State(state): State<WorkflowState>,
    AxumPath(raw_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<StatusCode, StatusCode> {
    let executor_id = authenticate(&state, &headers).await?;
    let id = parse_run_id(&raw_id)?;
    guard(&state, id, executor_id, &body).await?;
    tokio::fs::create_dir_all(&state.cfg.log_root)
        .await
        .map_err(internal)?;
    let path = state.cfg.log_root.join(format!("{id}.log"));
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .map_err(internal)?;
    if let Ok(meta) = file.metadata().await {
        if meta.len() >= state.cfg.max_log_bytes {
            return Ok(StatusCode::OK); // 截断，不再追加
        }
    }
    if let Some(lines) = body.get("lines").and_then(|v| v.as_array()) {
        use tokio::io::AsyncWriteExt;
        for line in lines.iter().filter_map(|v| v.as_str()) {
            file.write_all(line.as_bytes()).await.map_err(internal)?;
            file.write_all(b"\n").await.map_err(internal)?;
        }
    }
    sqlx::query("UPDATE workflow_runs SET log_path = $1 WHERE id = $2")
        .bind(path.to_string_lossy().to_string())
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(internal)?;
    Ok(StatusCode::OK)
}

async fn steps(
    State(state): State<WorkflowState>,
    AxumPath(raw_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<StatusCode, StatusCode> {
    let executor_id = authenticate(&state, &headers).await?;
    let id = parse_run_id(&raw_id)?;
    guard(&state, id, executor_id, &body).await?;
    if let Some(steps) = body.get("steps").and_then(|v| v.as_array()) {
        for s in steps {
            sqlx::query(
                r#"INSERT INTO workflow_steps
                     (run_id, job_id, step_index, name, status, exit_code, finished_at)
                   VALUES ($1,$2,$3,$4,$5,$6,
                           CASE WHEN $5 IN ('success','failure','skipped') THEN now() ELSE NULL END)
                   ON CONFLICT (run_id, job_id, step_index) DO UPDATE
                     SET status = EXCLUDED.status,
                         exit_code = EXCLUDED.exit_code,
                         name = EXCLUDED.name,
                         finished_at = EXCLUDED.finished_at"#,
            )
            .bind(id)
            .bind(s.get("job_id").and_then(|v| v.as_str()).unwrap_or("job"))
            .bind(s.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0) as i32)
            .bind(s.get("name").and_then(|v| v.as_str()).unwrap_or(""))
            .bind(s.get("status").and_then(|v| v.as_str()).unwrap_or("pending"))
            .bind(s.get("exit_code").and_then(|v| v.as_i64()).map(|v| v as i32))
            .execute(&state.pool)
            .await
            .map_err(internal)?;
        }
    }
    Ok(StatusCode::OK)
}

async fn complete(
    State(state): State<WorkflowState>,
    AxumPath(raw_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<StatusCode, StatusCode> {
    let executor_id = authenticate(&state, &headers).await?;
    let id = parse_run_id(&raw_id)?;
    guard(&state, id, executor_id, &body).await?;
    let conclusion = body
        .get("conclusion")
        .and_then(|v| v.as_str())
        .unwrap_or("failure");
    if !matches!(conclusion, "success" | "failure" | "cancelled") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let status = if conclusion == "cancelled" { "cancelled" } else { conclusion };
    sqlx::query(
        r#"UPDATE workflow_runs
              SET status = $2, conclusion = $3, exit_code = $4,
                  finished_at = now(), lease_until = NULL
            WHERE id = $1"#,
    )
    .bind(id)
    .bind(status)
    .bind(conclusion)
    .bind(body.get("exit_code").and_then(|v| v.as_i64()).map(|v| v as i32))
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    // 把结果回写到该 run 关联的 PR checks（按 commit_oid 绑定来源分支头）。
    sqlx::query(
        r#"UPDATE pr_checks c
              SET conclusion = $2
             FROM workflow_runs r
            WHERE c.run_id = r.id AND r.id = $1
              AND c.conclusion = 'pending'"#,
    )
    .bind(id)
    .bind(if conclusion == "success" { "success" } else { "failure" })
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    Ok(StatusCode::OK)
}

async fn heartbeat(
    State(state): State<WorkflowState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<StatusCode, StatusCode> {
    let executor_id = authenticate(&state, &headers).await?;
    let version = body
        .get("act_version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    sqlx::query(
        r#"UPDATE workflow_runners
              SET last_seen_at = now(), act_version = $2,
                  protocol_version = $3, snapshot_source = 'api'
            WHERE id = $1 AND enabled"#,
    )
    .bind(executor_id)
    .bind(version)
    .bind(PROTOCOL_VERSION)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    // 续租该 executor 名下所有在跑的 run。
    sqlx::query(
        r#"UPDATE workflow_runs
              SET lease_until = now() + ($2::int * interval '1 second')
            WHERE executor_id = $1 AND status = 'in_progress'"#,
    )
    .bind(executor_id)
    .bind(state.cfg.run_lease_secs as i32)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    Ok(StatusCode::OK)
}
