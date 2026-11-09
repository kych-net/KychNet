-- MCP 请求限流：跨实例共享的固定窗口计数。
--
-- 进程内 `rate::RateLimiter` 在多实例部署时每台各自计数，120/min 会被摊薄
-- 成 120×N/min（线上实测 590 次请求全部放行）。这里把窗口计数放进数据库，
-- 所有实例通过一条原子 UPSERT 共享同一份额度。
-- 每个客户端 IP 只占一行；过期行由 API 进程的定时任务按小时清理。
CREATE TABLE IF NOT EXISTS mcp_rate_limits (
    client_key   TEXT PRIMARY KEY,
    hits         INTEGER NOT NULL,
    window_until TIMESTAMPTZ NOT NULL
);
