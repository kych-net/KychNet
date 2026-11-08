//! 内置动作的发布队列。
//!
//! 修掉现有的两个问题：
//! 1. `git_receive` / `pr_merge` 里同步 `build_all()` 失败只 `warn!` 就返回成功，
//!    而 `BuildCtl` 单槽并发遇到「已有构建在进行」直接 `bail!` —— 构建期间的
//!    连续 push 会静默丢掉内容更新。
//! 2. `pr_merge` 里构建失败时 PR 已合并却返回 500，前端容易误判为合并失败。
//!
//! 做法：push 响应不再等构建；队列把 300ms 窗口内的连续请求折叠成一次，
//! worker 串行调用 `BuildCtl::build_all()`，因为构建以数据库为唯一事实源，
//! 最后一次构建自然产出最新内容。

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use crate::api::BuildCtl;

/// 发布动作不写进 YAML、不进执行器容器：`kych-net build` 以数据库为唯一事实源，
/// 不能把 `DATABASE_URL` 交给能 push 分支的人。
#[derive(Clone)]
pub struct BuildQueue {
    tx: mpsc::UnboundedSender<()>,
}

impl BuildQueue {
    /// 在 serve 启动时调用一次，返回可 Clone 的句柄。
    pub fn start(build_ctl: Arc<BuildCtl>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // 合并窗口：把这 300ms 内到达的所有请求折叠成一次构建。
                tokio::time::sleep(Duration::from_millis(300)).await;
                while rx.try_recv().is_ok() {}
                // BuildCtl 是单槽：管理端构建正在跑时 build_all 会直接失败。
                // 短暂重试，直到当前构建结束，避免这期间到达的 push 被静默丢掉。
                let mut retries = 0;
                loop {
                    match build_ctl.build_all().await {
                        Ok(()) => break,
                        Err(error)
                            if error.to_string().contains("已有构建")
                                && retries < 120 =>
                        {
                            retries += 1;
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                        Err(error) => {
                            warn!("内置 site.build 失败：{error:#}");
                            break;
                        }
                    }
                }
            }
        });
        Self { tx }
    }

    /// 事件提交后调用，立即返回，不阻塞 push 响应。
    pub fn enqueue(&self) {
        if let Err(error) = self.tx.send(()) {
            warn!("发布队列已关闭：{error}");
        }
    }
}
