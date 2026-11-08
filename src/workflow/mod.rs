//! 站内 Git 项目工作流（只做模式 B）。
//!
//! - 主服务永远在线，负责事件 outbox、定义发现、run 生成、租约扫描、
//!   以及**代读 PG 里的 Git 对象并打包成 tar.gz 下发**给执行器。
//! - KychRunner 是可选执行器，只有 runner token，零数据库凭据。
//! - 内置动作 `site.build` 由主服务独立排队，与执行器是否存在无关。
//!
//! 本模块源码为新增文件；与现有代码的挂接点见 `docs/workflow-integration.md`。

pub mod dispatch;
pub mod event;
pub mod queue;
pub mod runner_api;
pub mod snapshot;

use std::sync::Arc;
use sqlx::PgPool;

pub use runner_api::{router, WorkflowConfig, WorkflowState};

/// 与 KychRunner 协商的契约版本，必须与其 `PROTOCOL_VERSION` 一致。
pub const PROTOCOL_VERSION: i32 = 1;

/// 在 `api::router()` 里构造一次，传给 `workflow::router()` 与 `spawn_pruner()`。
pub fn build_state(pool: PgPool, build_ctl: Arc<crate::api::BuildCtl>) -> WorkflowState {
    WorkflowState {
        pool,
        cfg: Arc::new(WorkflowConfig::from_env()),
        build_queue: queue::BuildQueue::start(build_ctl.clone()),
        build_ctl,
    }
}

/// serve 启动时调用：后台扫描过期事件 / 超时租约 / 队列积压。
pub fn spawn_pruner(state: WorkflowState) -> tokio::task::JoinHandle<()> {
    dispatch::spawn_pruner(state)
}
