//! 事件 outbox：与 refs / PR 状态更新处在**同一个事务**里写入。
//!
//! 这样保证「提交失败不会触发构建」「构建不会读到旧 refs」，并且用
//! dedupe_key 把构建期间的连续 push 折叠成一条（对齐 GitHub concurrency group）。

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// `receive_pack` 里一次 ref 变更。替代原来的 `default_branch_updated: bool`。
#[derive(Debug, Clone)]
pub struct RefUpdate {
    pub name: String,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct WorkflowEvent {
    pub project_id: Uuid,
    /// push | tag | pr_opened | pr_sync | pr_merged | pr_closed | manual | schedule
    pub kind: String,
    pub ref_name: String,
    pub before_oid: Option<Vec<u8>>,
    pub after_oid: Option<Vec<u8>>,
    pub pr_number: Option<i32>,
    pub actor_id: Option<Uuid>,
    /// GitHub 兼容的 event.json 内容
    pub payload: Value,
    pub dedupe_key: Option<String>,
}

/// 同一项目 / 同一事件 / 同一 ref / 同一 PR 的连续 push 折叠成一条。
pub fn dedupe_key(
    project_id: Uuid,
    kind: &str,
    ref_name: &str,
    pr_number: Option<i32>,
) -> String {
    format!("{project_id}:{kind}:{ref_name}:{pr_number:?}")
}

/// 信任分级由主服务强制，绝不读 YAML 里的 `permissions:` 自声明。
pub fn trust_level(kind: &str, ref_name: &str, default_branch: &str) -> &'static str {
    match kind {
        "pr_opened" | "pr_sync" => "pr",
        "manual" => "manual",
        "push" | "tag" | "pr_merged" | "pr_closed" => {
            if ref_name == format!("refs/heads/{default_branch}") {
                "default"
            } else {
                "branch"
            }
        }
        _ => "branch",
    }
}

/// 在事务内入队。调用方必须在 `tx.commit()` **之前**调用本函数。
pub async fn enqueue(tx: &mut Transaction<'_, Postgres>, e: &WorkflowEvent) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO workflow_events
             (project_id, kind, ref_name, before_oid, after_oid, pr_number,
              actor_id, payload, dedupe_key, expires_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now() + interval '24 hours')
           ON CONFLICT (dedupe_key) WHERE status = 'queued'
           DO UPDATE SET after_oid  = EXCLUDED.after_oid,
                         payload    = EXCLUDED.payload,
                         created_at = now()"#,
    )
    .bind(e.project_id)
    .bind(&e.kind)
    .bind(&e.ref_name)
    .bind(e.before_oid.as_deref())
    .bind(e.after_oid.as_deref())
    .bind(e.pr_number)
    .bind(e.actor_id)
    .bind(&e.payload)
    .bind(e.dedupe_key.as_deref())
    .execute(&mut **tx)
    .await
    .context("写入 workflow_events 失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_level_is_forced_by_event_source() {
        assert_eq!(trust_level("pr_opened", "", "main"), "pr");
        assert_eq!(trust_level("push", "refs/heads/main", "main"), "default");
        assert_eq!(trust_level("push", "refs/heads/feat", "main"), "branch");
        assert_eq!(trust_level("manual", "refs/heads/main", "main"), "manual");
    }

    #[test]
    fn dedupe_key_is_stable() {
        let id = Uuid::nil();
        assert_eq!(
            dedupe_key(id, "push", "refs/heads/main", None),
            dedupe_key(id, "push", "refs/heads/main", None)
        );
        assert_ne!(
            dedupe_key(id, "push", "refs/heads/a", None),
            dedupe_key(id, "push", "refs/heads/b", None)
        );
    }
}
