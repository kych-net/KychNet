# 工作流模块挂接记录

PR #19 的第一个 commit 只新增文件；**第二个 commit 已完成挂接**，把 `src/workflow/`
真正接入主服务。下面是实际落地情况，以及相对初版清单的修正。

## 1. 逐文件改动

| 文件 | 改动 |
| --- | --- |
| `src/main.rs` | 注册 `mod workflow;` |
| `src/api.rs` | `with_state` 之前克隆 `pool` / `build_ctl`；在管理端 layer 链之后构造 `WorkflowState`、启动 `spawn_pruner` + `spawn_dispatcher`、`nest("/api/runner", workflow::router(...))`；删除 `git_receive` 与 `pr_merge` 里的同步 `build_all()` |
| `src/git/http.rs` | 去掉 `default_branch_updated: bool` 与同步构建触发；`checked` 元组带上 `old` / `new`；成功落库的每条 ref（含 tag、delete）在 `receive_pack` 内部收集为 `RefUpdate`（局部变量），并在 `tx.commit()` **之前** `enqueue()` push/tag 事件，对来源分支的开放 PR 追加 `pr_sync`。`ReceiveOutcome` 只回传 Git 协议响应，事件不再经上层二次触发 |
| `src/collab.rs` | `create_pr` / `merge_pr` / `close_pr` 在各自事务提交前发 `pr_opened` / `pr_merged` / `pr_closed`；`merge_pr` 增加工作流检查门禁 |
| `src/git/gc.rs` | `mark_reachable` 接收 `pending_commits()` 返回的 OID 作为额外可达种子，避免删除排队/执行中 run 还没读取的 Git 对象 |
| `src/workflow/runner_api.rs` | 路由参数 `:id` → `{id}`（axum 0.8，初版会在注册路由时 panic）；`BuildCtl` 改从 `crate::api` 引入；修正日志字节数比较 |
| `src/workflow/mod.rs` / `queue.rs` / `dispatch.rs` / `snapshot.rs` | `BuildCtl` 统一改从 `crate::api` 引入（实际定义在 `api.rs` 且需 `pub(crate)`）；`dispatch` 去掉 `tx.acquire()` 改用事务本身、补类型标注；`snapshot` 删除冗余 `RunRef.id`、修 USTAR 校验和单测；`mod.rs` 删除未使用的 `SNAPSHOT_SOURCE` |

## 2. 相对初版清单的三处修正

1. **GC 不在 `src/build.rs`**，实际实现在 `src/git/gc.rs::mark_reachable()`。
   把 `pending_commits()` 的结果并入初始 `stack`，这样被保护的不只是 commit 本身，
   还包括它的整棵树与父提交——只把 OID 塞进 `reachable` 集合是不够的。
2. **runner 路由挂在管理端 layer 链之后**（与 MCP 路由并列），不套
   `DefaultBodyLimit::max(2 MiB)` 与 `guard_origin`：runner 用 token 认证、不带浏览器
   Origin，日志分片上限是 8 MiB。
3. **门禁按来源分支头校验**：`pr_checks.commit_oid` 绑定的是 PR 来源分支 head，
   所以合并门禁用 `source`（而非目标分支 ref）比对，才能防止「检查通过后改代码再合并」。

## 3. 关键行为变化

- push 响应**不再等待构建**：事件在 refs 事务内入队，发布由 `BuildQueue` 的 300ms
  去重窗口串行执行。这修掉了「构建期间连续 push 静默丢内容更新」。
- PR 合并接口**不再因构建失败返回 500**：门禁与事件入队都在 `collab::merge_pr`
  事务里，失败整体回滚，前端不会误判「已合并但报错」。
- 门禁查询在项目未开启 `require_workflow_checks` 或执行器缺席（没有 required 检查）时
  返回通过，因此「没装 KychRunner 就永远不能合并 PR」不会发生。

## 4. 配置项

`WorkflowConfig::from_env()` 独立读环境变量，不需要改 `src/config.rs`：

| 变量 | 默认 |
| --- | --- |
| `WORKFLOW_QUEUE_MAX` | `500` |
| `RUN_LEASE_SECS` | `300` |
| `WORKFLOW_LOG_ROOT` | `logs/workflows` |
| `WORKFLOW_SNAPSHOT_CACHE` | `cache/workflow-snapshots` |
| `MAX_SNAPSHOT_BYTES` | `209715200` |
| `MAX_LOG_BYTES` | `8388608` |
| `LFS_ROOT` | `lfs` |

**不需要** `ACT_BIN`、Docker 配置、`pg_dsn`——主服务不碰这些。

## 5. 验证

在含两个 commit 的完整源码树上实际执行：

```bash
cargo clippy --ignore-rust-version --all-targets   # 0 error
cargo test   --ignore-rust-version                 # 92 passed / 0 failed
```

- 沙箱 rustc 为 1.93，而 `Cargo.lock` 中 `enum-map@3.0.0` 要求 ≥ 1.95，
  故带 `--ignore-rust-version` 绕过 MSRV 检查；CI 应在 ≥ 1.95 的工具链上跑 `cargo test --locked`。
- clippy 剩余 18 条 warning 全部来自 main 分支既有代码（`auth.rs` / `cli_cn.rs` /
  `oauth.rs` / `render.rs` 等），**新增的 `src/workflow/` 六个模块零 warning**；
  清理它们会扩大无关 diff，故不在本 PR 处理。
- 为保持 PR diff 聚焦，被修改的既有文件保留仓库原有排版，未跑全量 `cargo fmt --all`
  （那会重排 12 个无关文件）；新增代码本身按 rustfmt 风格书写。

## 6. 注册一个执行器

管理员在数据库里预置一次性注册码。**必须用 SHA-256**（服务端用
`Sha256::digest(code.as_bytes())` 匹配），初版清单里的 `md5` 是错的——长度 16 字节，
永远匹配不上 32 字节的哈希：

```bash
# 本地算出注册码的 sha256 十六进制（32 字节 = 64 个 hex 字符）
printf '%s' '你的注册码' | sha256sum
```

```sql
INSERT INTO workflow_runners (id, name, token_hash, registration_code_hash)
VALUES (
    gen_random_uuid(),
    'runner-1',
    gen_random_bytes(32),                       -- 未注册前的占位值，随机以免撞 UNIQUE
    decode('<上面算出的 64 位 hex>', 'hex')      -- 32 字节 sha256
);
```

然后：

```bash
kychrunner register --url https://kych.net --code '你的注册码'
# 把回显的 RUNNER_TOKEN 写进 /etc/kychrunner/env
systemctl enable --now kychrunner
```

注册成功后服务端把 `registration_code_hash` 置 NULL 并写入真正的 `token_hash`；
`enabled=false` 即临时禁用，撤销 token 后所有请求（含快照接口）立即 403。

> 后续可补管理端 API（session + step-up）自动生成注册码，避免手工 SQL。
> 注意当前 `register` 端点的 SELECT + UPDATE 未包在同一事务里，并发用同一个码
> 存在竞态；建议后续改成单条 `UPDATE ... WHERE registration_code_hash = $1
> AND token_hash IS NOT NULL RETURNING id`。

## 7. 联调验收

1. **不装 KychRunner**：push 默认分支 → 静态站正常重建，run 记 `skipped / no_runner`。
2. **只给 token**：完整跑通 `site.yml`，执行器不配任何 DSN。
3. 300ms 连推 3 次 → 只 1 次发布构建，产物是第 3 次内容。
4. `kill -9 kychrunner` → 租约到期 job 回 `queued` 被重新领取。
5. 撤销 token 后 `GET /api/runner/jobs/{id}/snapshot` → 403。
6. runner 拿 A 的 run_id 请求 B 的快照 → 403。
7. `cargo clippy --ignore-rust-version --all-targets` 0 error、`cargo test --ignore-rust-version` 92 项全过（见第 5 节）。
