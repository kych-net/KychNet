-- 项目：/项目/<slug>/ 下的内容来自项目仓库，Git 对象全部存放在 PostgreSQL。
CREATE TABLE IF NOT EXISTS projects (
    id UUID PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    default_branch TEXT NOT NULL DEFAULT 'main',
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    archived_at TIMESTAMPTZ
);

-- Git 对象。oid 是 SHA-1("<type> <len>\0" + data)，data 存对象原文。
CREATE TABLE IF NOT EXISTS git_objects (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    oid BYTEA NOT NULL CHECK (octet_length(oid) = 20),
    kind SMALLINT NOT NULL CHECK (kind IN (1, 2, 3, 4)),
    size BIGINT NOT NULL,
    data BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, oid)
);

CREATE TABLE IF NOT EXISTS git_refs (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    target BYTEA NOT NULL CHECK (octet_length(target) = 20),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, name)
);

-- 提交索引：权威数据仍在 git_objects，这里只服务于管理端分页与筛选。
CREATE TABLE IF NOT EXISTS git_commits (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    oid BYTEA NOT NULL,
    parent_oid BYTEA,
    second_parent_oid BYTEA,
    tree_oid BYTEA NOT NULL,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    author_name TEXT NOT NULL,
    message TEXT NOT NULL,
    committed_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (project_id, oid)
);
CREATE INDEX IF NOT EXISTS git_commits_time_idx
    ON git_commits (project_id, committed_at DESC);

CREATE TABLE IF NOT EXISTS git_commit_paths (
    project_id UUID NOT NULL,
    commit_oid BYTEA NOT NULL,
    path TEXT NOT NULL,
    change TEXT NOT NULL CHECK (change IN ('add', 'modify', 'delete', 'rename')),
    old_path TEXT,
    blob_oid BYTEA,
    PRIMARY KEY (project_id, commit_oid, path)
);
CREATE INDEX IF NOT EXISTS git_commit_paths_path_idx
    ON git_commit_paths (project_id, path);

CREATE TABLE IF NOT EXISTS pull_requests (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    number INT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    source_branch TEXT NOT NULL,
    target_branch TEXT NOT NULL,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'merged', 'closed')),
    merge_commit_oid BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    merged_at TIMESTAMPTZ,
    UNIQUE (project_id, number)
);

CREATE TABLE IF NOT EXISTS pr_comments (
    id UUID PRIMARY KEY,
    pr_id UUID NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    body TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS project_discussions (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS discussion_comments (
    id UUID PRIMARY KEY,
    discussion_id UUID NOT NULL REFERENCES project_discussions(id) ON DELETE CASCADE,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    body TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Git 写操作令牌。只存 SHA-256，明文只在创建时展示一次。
CREATE TABLE IF NOT EXISTS git_tokens (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash BYTEA NOT NULL UNIQUE,
    scopes TEXT[] NOT NULL DEFAULT '{write}',
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);
