-- TOTP 绑定改成「先待确认、确认成功后才替换」：
-- 待确认的密钥放在 pending_secret，生效密钥 secret 允许为空
-- （用户生成后没走完确认流程时，旧密钥必须原样留着）。
ALTER TABLE totp_secrets ADD COLUMN IF NOT EXISTS pending_secret BYTEA;
ALTER TABLE totp_secrets ALTER COLUMN secret DROP NOT NULL;
