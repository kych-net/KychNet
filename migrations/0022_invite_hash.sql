-- 邀请码不再明文入库：库里只保留 SHA-256 摘要 + 前 6 位识别前缀。
-- 明文仅在生成接口/命令行那一次响应里返回，之后无法恢复。
-- 与 Rust 侧 hex::encode(Sha256::digest(code.as_bytes())) 保持一致。
CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE invites ADD COLUMN id UUID;
UPDATE invites SET id = gen_random_uuid();
ALTER TABLE invites ALTER COLUMN id SET NOT NULL;

ALTER TABLE invites ADD COLUMN code_hash TEXT;
ALTER TABLE invites ADD COLUMN code_prefix TEXT;

-- 先把旧明文的前 6 位留作管理端识别，再整体换成摘要；顺序不能颠倒。
UPDATE invites SET code_prefix = left(code, 6) WHERE code_prefix IS NULL;
UPDATE invites SET code_hash = encode(digest(convert_to(code, 'UTF8'), 'sha256'), 'hex')
    WHERE code_hash IS NULL;

ALTER TABLE invites ALTER COLUMN code_hash SET NOT NULL;
ALTER TABLE invites ALTER COLUMN code_prefix SET NOT NULL;

ALTER TABLE invites DROP CONSTRAINT invites_pkey;
ALTER TABLE invites ADD PRIMARY KEY (id);
ALTER TABLE invites ADD CONSTRAINT invites_code_hash_key UNIQUE (code_hash);
ALTER TABLE invites DROP COLUMN code;
