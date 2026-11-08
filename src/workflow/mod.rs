//! 站内 Git 项目工作流（只做模式 B）。
//!
//! - 主服务永远在线，负责事件 outbox、定义发现、run 生成、租约扫描，
//!   以及通过 `/git/<项目>/` 把 PG 里的 Git 对象**以工作树形式**暴露给执行器。
//! - KychRunner 是可选执行器，只有 runner token，零数据库凭据；
//!   protocol v2 起直接读写 `/git/` 工作树，不再依赖打包下发。
//! - 内置动作 `site.build` 由主服务独立排队，与执行器是否存在无关。
//!
//! 本模块源码为新增文件；与现有代码的挂接点见 `docs/workflow-integration.md`。

pub mod dispatch;
pub mod event;
pub mod management;
pub mod queue;
pub mod runner_api;
pub mod snapshot;

use std::sync::Arc;
use sqlx::PgPool;

pub use runner_api::{router, WorkflowConfig, WorkflowState};

/// 与 KychRunner 协商的契约版本，必须与其 `PROTOCOL_VERSION` 一致。
///
/// v2：执行器改走 `/git/<项目>/` 工作树读写，claim 新增 `git_base` / `write_ref`，
/// runner token 可用于受限的文件树写。v1 执行器会被 claim / register 拒绝，
/// 属**不兼容升级**，升级时需同步重启 KychRunner。
pub const PROTOCOL_VERSION: i32 = 2;


/// 旧版本硬编码/占位目录；现在都映射到 `.<site_name>/工作流`。
fn is_legacy_dir(value: &str) -> bool {
    matches!(
        value.trim().trim_end_matches('/'),
        "" | ".github/workflows" | ".网站名称/工作流"
    )
}

fn sanitize_site_dir(site_name: &str) -> String {
    let mut out = String::new();
    for ch in site_name.trim().chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('-');
        }
    }
    if out.is_empty() {
        "kych-net".to_string()
    } else {
        out
    }
}

/// 未显式配置时使用设置里的真实站点名称：`.<site_name>/工作流`。
pub async fn resolve_workflow_dir(pool: &PgPool, configured: &str) -> anyhow::Result<String> {
    let configured = configured.trim().trim_end_matches('/');
    if !is_legacy_dir(configured) {
        return Ok(configured.to_string());
    }
    let site_name = match crate::service::get_settings(pool).await {
        Ok(settings) => settings.site_name,
        Err(error) => {
            tracing::warn!(%error, "读取站点名称失败，工作流目录回退 kych-net");
            "kych-net".to_string()
        }
    };
    Ok(format!(".{}/工作流", sanitize_site_dir(&site_name)))
}
/// 在 `api::router()` 里构造一次，传给 `workflow::router()` 与 `spawn_pruner()`。
pub fn build_state(
    pool: PgPool,
    build_ctl: Arc<crate::api::BuildCtl>,
    build_queue: queue::BuildQueue,
) -> WorkflowState {
    WorkflowState {
        pool,
        cfg: Arc::new(WorkflowConfig::from_env()),
        build_queue,
        build_ctl,
    }
}

/// serve 启动时调用：后台扫描过期事件 / 超时租约 / 队列积压。
pub fn spawn_pruner(state: WorkflowState) -> tokio::task::JoinHandle<()> {
    dispatch::spawn_pruner(state)
}
