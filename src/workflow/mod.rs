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
