-- Git SSH 公钥：用户在管理端登记公钥后，即可通过内建 SSH 服务推拉项目 Git。
-- 公钥只存 OpenSSH 单行文本与指纹；指纹唯一，认证时用它反查用户，
-- 传输层不依赖系统账号，与 HTTP 的 Git 令牌体系平行。
CREATE TABLE IF NOT EXISTS git_ssh_keys (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users(id),
    name         TEXT NOT NULL,
    public_key   TEXT NOT NULL,
    fingerprint  TEXT NOT NULL UNIQUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);
