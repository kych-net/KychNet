# 挂接清单：把 `src/workflow/` 接入现有代码

本 PR 只新增文件，**不改动任何现有代码**，因此合入后不会破坏当前构建。
下面给出让模块真正生效的最小改动，建议在同一个 feature 分支上追加一个 commit 完成。

> 所有片段按现有 `anyhow` + `sqlx 0.8` + `axum 0.8` 风格写就；
> 挂接后请以 `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test` 为准微调。

## 1. 注册模块（`src/main.rs`，约第 1 行）

```rust
mod api;
// ... 现有模块 ...
mod workflow;   // 新增
```

## 2. 构造状态并挂载路由（`src/api.rs`）

现有 `router()` 里 `let app = Router::new()...with_state(state)`（约 1329 / 1436 行）。
`workflow::router()` 返回 `Router<()>`，所以要在 **`with_state` 之后** nest：

```rust
// 在 with_state 之前先把资源克隆出来（state 会被 move）
let wf_pool = state.pool.clone();
let wf_build_ctl = state.build_ctl.clone();

let app = Router::new()
    // ... 现有全部 route ...
    .with_state(state);                       // 现在 app: Router<()>

let wf_state = workflow::build_state(wf_pool, wf_build_ctl);

// 后台任务：事件分发 + 租约/过期扫描
workflow::spawn_pruner(wf_state.clone());
workflow::dispatch::spawn_dispatcher(
    wf_state.clone(),
    wf_state.build_queue.clone(),
);

let app = app
    .nest("/api/runner", workflow::router(wf_state))
    // 管理端（P1）：/api/projects/{id}/runs|workflows|dispatch|checks
    .layer(middleware::from_fn_with_state(/* ... 现有 layer ... */));
```

`workflow::router()` 内部路径是 `/jobs/claim`、`/register` 等，nest 后即为
`/api/runner/jobs/claim`。管理端 API 仍走 session 认证，与 runner token 互不影响。

## 3. 配置项（无需改 `src/config.rs`）

`WorkflowConfig::from_env()` 独立读环境变量：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `WORKFLOW_QUEUE_MAX` | `500` | queued run 上限 |
| `RUN_LEASE_SECS` | `300` | 租约时长 |
| `WORKFLOW_LOG_ROOT` | `logs/workflows` | 运行日志目录（DB 只存路径） |
| `WORKFLOW_SNAPSHOT_CACHE` | `cache/workflow-snapshots` | tar.gz 内容寻址缓存 |
| `MAX_SNAPSHOT_BYTES` | `209715200` | 快照原始字节上限 |
| `MAX_LOG_BYTES` | `8388608` | 单 run 日志上限 |
| `LFS_ROOT` | `lfs` | 与现有配置一致，用于还原 LFS pointer |

**不需要** `ACT_BIN`、Docker 配置、`pg_dsn`——主服务不碰这些。

## 4. `src/git/http.rs`：`ReceiveOutcome` 改成完整 ref 变更列表

现状（约 22 / 901 / 963 行）只有一个 `default_branch_updated: bool`：

```rust
pub struct ReceiveOutcome {
    pub default_branch_updated: bool,
    // ...
}
```

改为：

```rust
pub struct ReceiveOutcome {
    pub updates: Vec<workflow::event::RefUpdate>,
    // ... 其余字段保持不变
}
```

在 `receive_pack` 的循环里，把每次 ref 更新收集成
`RefUpdate { name, before, after }`（含 `refs/tags/*`），并在
**`tx.commit()` 之前**写 outbox：

```rust
// tx 仍在处理 refs 更新的同一个事务里
for u in &updates {
    let kind = if u.name.starts_with("refs/tags/") { "tag" } else { "push" };
    workflow::event::enqueue(
        &mut tx,
        &workflow::event::WorkflowEvent {
            project_id,
            kind: kind.to_string(),
            ref_name: u.name.clone(),
            before_oid: u.before.clone(),
            after_oid: u.after.clone(),
            pr_number: None,
            actor_id: Some(actor_id),
            payload: serde_json::json!({
                "ref": u.name,
                "before": u.before.as_ref().map(hex::encode),
                "after": u.after.as_ref().map(hex::encode),
            }),
            dedupe_key: Some(workflow::event::dedupe_key(project_id, kind, &u.name, None)),
        },
    ).await?;
}
tx.commit().await?;
```

> 来源分支的 push 还要额外发一条 `pr_sync`（按 `pull_requests` 查 source branch），
> 见第 6 节。

## 5. `src/api.rs::git_receive`（约 2322 行）：删同步构建

现状：

```rust
if outcome.default_branch_updated {
    if let Err(error) = state.build_ctl.build_all().await {
        tracing::warn!("重建站点失败: {error:#}");
    }
}
```

删掉整段。事件已经在 `receive_pack` 的事务里入队，分发器会触发 `BuildQueue`。
**push 响应不再等待构建**，构建失败也不影响已提交的 refs。

## 6. `src/collab.rs`：PR 事件

在各自事务提交前调用 `workflow::event::enqueue()`：

| 位置 | kind | 备注 |
| --- | --- | --- |
| `create_pr` | `pr_opened` | payload 为 GitHub 兼容 `pull_request` 事件 |
| 来源分支 push | `pr_sync` | `pr_number` 带上 |
| `merge_pr` | `pr_merged` | 目标分支是默认分支才允许发布 |
| `close_pr` | `pr_closed` | |

`merge_pr` 里加门禁（执行器缺席时不写 required check，所以默认不阻塞合并）：

```sql
SELECT COALESCE(bool_and(c.conclusion = 'success'), true)
  FROM projects p
  LEFT JOIN pr_checks c
    ON c.project_id = p.id AND c.pr_number = $2 AND c.required
   AND c.commit_oid = (SELECT target FROM git_refs
                       WHERE project_id = p.id AND name = 'refs/heads/' || $3)
 WHERE p.id = $1 AND p.require_workflow_checks;
```

## 7. `src/api.rs::pr_merge`（约 1923 行）

删掉同步 `build_all()`（现在构建失败时 PR 已合并却返回 500，前端容易误判），
改成入队 + 上面的门禁查询。

## 8. `src/build.rs`：`gc` 协调

清理不可达对象前，跳过仍被排队/执行中 run 引用的 commit：

```rust
let keep = workflow::dispatch::pending_commits(pool).await?;
// 把 keep 里的 oid 加入 gc 的 reachable 集合
```

否则 run 排队期间跑了 gc，快照接口会读不到对象。
`workflow_runs_pending_commit_idx` 索引已为这个查询准备。

## 9. 管理端（P1）

`admin/src/views/` 新增：

- `WorkflowsView`：列表 / 启用新路径 / 手动触发 / 历史；明示「工作区无 `.git`」。
- `WorkflowRunView`：步骤与日志（日志走 `log_path` 文件，不进 PG）。
- `ExecutorsView`：在线状态、禁用、重放、快照来源（恒为 `api`）。
- PR 页：检查条，区分 `skipped / no_runner` 与 `failure`。

## 10. 注册一个执行器

管理员在数据库里预置一次性注册码（`token_hash` 先填随机值占位）：

```sql
INSERT INTO workflow_runners (id, name, token_hash, registration_code_hash)
VALUES (gen_random_uuid(), 'runner-1', gen_random_bytes(32),
        decode(md5('你的注册码'), 'hex'));
```

> 生产环境请用 SHA-256：`registration_code_hash = sha256('你的注册码')`（`bytea`）。
> `kychrunner register` 会用 `sha256(code)` 匹配，成功后立刻把该列置 NULL 并写入真正的 `token_hash`。

然后：

```bash
kychrunner register --url https://kych.net --code '你的注册码'
# 把回显的 RUNNER_TOKEN 写进 /etc/kychrunner/env
systemctl enable --now kychrunner
```

## 11. 联调验收

1. **不装 KychRunner**：push 默认分支 → 静态站正常重建，run 记 `skipped / no_runner`，UI 明示。
2. **只给 token**：完整跑通 `site.yml`，执行器不配任何 DSN。
3. 300ms 连推 3 次 → 只 1 次发布构建，产物是第 3 次内容。
4. `kill -9 kychrunner` → 租约到期 job 回 `queued` 被重新领取。
5. 撤销 token 后 `GET /api/runner/jobs/{id}/snapshot` → 403。
6. runner 拿 A 的 run_id 请求 B 的快照 → 403。
7. `cargo clippy --all-targets -- -D warnings` 与 `cargo test` 通过。
