-- 0013_workflows.sql
-- 站内 Git 项目工作流（只做模式 B）：执行器注册 / 定义注册表 / 事件 outbox / 运行记录 / 步骤 / PR 检查。
--
-- 约定 1：事件写入必须与 refs / PR 状态更新处在同一个事务里（见 src/workflow/event.rs）。
-- 约定 2：KychRunner 不直连本库，也不持有任何数据库凭据；
--         只通过 /api/runner/* HTTP 契约交互，快照由本服务代读 PG 后打包成 tar.gz 下发。
--         因此本迁移不创建只读角色、不创建视图、不开放 5432 给执行器。
-- 约定 3：内置动作（site.build / build_page）不写入 workflow_runs 的执行器支路，
--         执行器缺席、离线、临时禁用时静态站照常重建。
-- 约定 4：工作流定义只从默认分支 head（workflow_runs.def_commit_oid）读取，
--         PR 分支里改工作流不生效；默认分支新出现的路径登记为 enabled=false。

-- 项目级开关。执行器可缺席，故所有开关都有安全默认值。
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS require_workflow_checks BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS require_executor_for_merge BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS workflow_dir TEXT NOT NULL DEFAULT '.网站名称/工作流',
    ADD COLUMN IF NOT EXISTS workflow_executor_enabled BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS publish_on_check_success BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE projects ALTER COLUMN workflow_dir SET DEFAULT '.网站名称/工作流';
UPDATE projects SET workflow_dir = '.网站名称/工作流'
 WHERE workflow_dir = '.github/workflows';

-- 可选执行器注册表。token 明文只在注册时回显一次，服务端只存 SHA-256 hash。
-- enabled=false 即「临时禁用」；撤销 token 后所有请求（含快照接口）立即 403。
-- 无任何存活记录 / last_seen_at 超时 → executor_available=false，
-- 新 run 记为 skipped(no_runner)，内置动作照常执行。
CREATE TABLE IF NOT EXISTS workflow_runners (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    token_hash BYTEA NOT NULL UNIQUE,
    enabled BOOLEAN NOT NULL DEFAULT true,
    protocol_version INT NOT NULL DEFAULT 1,
    act_version TEXT,
    snapshot_source TEXT NOT NULL DEFAULT 'api',
    labels TEXT[] NOT NULL DEFAULT '{}',
    registration_code_hash BYTEA,
    last_seen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS workflow_runners_online_idx
    ON workflow_runners (enabled, last_seen_at DESC);

-- 工作流定义注册表。定义本体在仓库 <workflow_dir>/*.yml；
-- 默认分支里新出现的路径注册为 enabled=false，需管理员显式启用，
-- 避免通过一次合并把任意命令悄悄塞进默认分支工作流。
CREATE TABLE IF NOT EXISTS project_workflows (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,                       -- 例如 .网站名称/工作流/site.yml
    enabled BOOLEAN NOT NULL DEFAULT false,
    approved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    approved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, path)
);

-- 事件 outbox。dedupe_key 折叠连续 push（对齐 GitHub concurrency group）；
-- expires_at 防止执行器长期缺席时队列无限堆积。
CREATE TABLE IF NOT EXISTS workflow_events (
    id BIGSERIAL PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN
        ('push', 'tag', 'pr_opened', 'pr_sync', 'pr_merged', 'pr_closed',
         'manual', 'schedule')),
    ref_name TEXT NOT NULL DEFAULT '',
    before_oid BYTEA,
    after_oid BYTEA,
    pr_number INT,
    actor_id UUID REFERENCES users(id) ON DELETE SET NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    dedupe_key TEXT,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'superseded', 'claimed', 'done', 'failed', 'expired')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    consumed_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (now() + interval '24 hours')
);

-- 部分唯一索引：只有 queued 行参与去重；inference 谓词必须与
-- src/workflow/event.rs 里 ON CONFLICT 的 WHERE 一致。
CREATE UNIQUE INDEX IF NOT EXISTS workflow_events_queued_dedupe_idx
    ON workflow_events (dedupe_key)
    WHERE status = 'queued';

CREATE INDEX IF NOT EXISTS workflow_events_queue_idx
    ON workflow_events (created_at)
    WHERE status = 'queued';
CREATE INDEX IF NOT EXISTS workflow_events_expiry_idx
    ON workflow_events (expires_at)
    WHERE status = 'queued';
CREATE INDEX IF NOT EXISTS workflow_events_project_idx
    ON workflow_events (project_id, created_at DESC);

-- 一次工作流执行。状态词对齐 GitHub Checks：
-- queued / in_progress / success / failure / cancelled / skipped / expired
CREATE TABLE IF NOT EXISTS workflow_runs (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    event_id BIGINT REFERENCES workflow_events(id) ON DELETE SET NULL,
    executor_id UUID REFERENCES workflow_runners(id) ON DELETE SET NULL,
    workflow_path TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    ref_name TEXT NOT NULL DEFAULT '',
    -- 代码快照（事件对应 commit；PR 事件为来源分支头）
    commit_oid BYTEA,
    -- 定义快照，恒为默认分支 head —— 工作流 YAML 只从这里读
    def_commit_oid BYTEA,
    pr_number INT,
    actor_id UUID REFERENCES users(id) ON DELETE SET NULL,
    trust_level TEXT NOT NULL DEFAULT 'pr'
        CHECK (trust_level IN ('pr', 'branch', 'default', 'manual')),
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'in_progress', 'success', 'failure',
                          'cancelled', 'skipped', 'expired')),
    conclusion TEXT,
    skipped_reason TEXT
        CHECK (skipped_reason IN ('no_runner', 'disabled', 'incompatible_version')),
    exit_code INT,
    -- 租约：claim 后 now()+5min，runner 心跳续租；超时打回 queued 并 attempt+1。
    lease_until TIMESTAMPTZ,
    attempt INT NOT NULL DEFAULT 0,
    -- 日志落文件，DB 只存路径（照 LFS 的思路，不把日志塞进 PG）
    log_path TEXT,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (now() + interval '24 hours'),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS workflow_runs_project_idx
    ON workflow_runs (project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS workflow_runs_pr_idx
    ON workflow_runs (project_id, pr_number, created_at DESC)
    WHERE pr_number IS NOT NULL;
-- 租约扫描：找出 in_progress 且已过期的 run。
CREATE INDEX IF NOT EXISTS workflow_runs_lease_idx
    ON workflow_runs (lease_until)
    WHERE status = 'in_progress';
-- 未被领取的 run。
CREATE INDEX IF NOT EXISTS workflow_runs_claim_idx
    ON workflow_runs (created_at)
    WHERE status = 'queued';
-- gc 协调：清理不可达对象时跳过仍被排队/执行中 run 引用的 commit（两种 oid 都要看）。
CREATE INDEX IF NOT EXISTS workflow_runs_pending_commit_idx
    ON workflow_runs (commit_oid)
    WHERE status IN ('queued', 'in_progress');

-- 步骤级结果。由 KychRunner 解析 act --json 后批量回报。
CREATE TABLE IF NOT EXISTS workflow_steps (
    id BIGSERIAL PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    job_id TEXT NOT NULL,
    step_index INT NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'in_progress', 'success', 'failure', 'skipped')),
    exit_code INT,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    UNIQUE (run_id, job_id, step_index)
);

-- PR 检查状态。commit_oid 绑定来源分支头，门禁只认与当前 source head 一致的结果。
-- 执行器缺席时写 conclusion='skipped' 且 required=false，避免「没装执行器就永远不能合并」。
CREATE TABLE IF NOT EXISTS pr_checks (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL,
    pr_number INT NOT NULL,
    run_id UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    conclusion TEXT NOT NULL
        CHECK (conclusion IN ('pending', 'success', 'failure', 'cancelled', 'skipped')),
    required BOOLEAN NOT NULL DEFAULT true,
    commit_oid BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (run_id, name),
    FOREIGN KEY (project_id, pr_number)
        REFERENCES pull_requests (project_id, number) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS pr_checks_lookup_idx
    ON pr_checks (project_id, pr_number, commit_oid, created_at DESC);

-- ---------------------------------------------------------------------------
-- 只做模式 B：这里没有任何数据库角色创建、权限授予或 runner_* 只读视图。
-- 执行器零 DB 凭据；快照由 GET /api/runner/jobs/{id}/snapshot 在本服务内
-- 代读 git_objects 后用 tar.gz 下发，见 src/workflow/snapshot.rs。
--
-- gc 协调：清理不可达对象时跳过仍被排队/执行中 run 引用的 commit：
--   SELECT DISTINCT commit_oid FROM workflow_runs
--    WHERE status IN ('queued','in_progress') AND expires_at > now()
--   UNION
--   SELECT DISTINCT def_commit_oid FROM workflow_runs
--    WHERE status IN ('queued','in_progress') AND expires_at > now();
-- 实现方式见 src/workflow/dispatch.rs。
-- ---------------------------------------------------------------------------
