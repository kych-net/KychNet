-- 注册改为「凭证校验通过后才建用户」，进行中的流程没有对应行，
-- 因此 auth_flows.user_id 允许为空；具体资料暂存在 state 里。
ALTER TABLE auth_flows ALTER COLUMN user_id DROP NOT NULL;
