//! MCP（Model Context Protocol）服务器：把站点内容以只读工具的形式暴露给 AI 客户端。
//!
//! 设计要点：
//! - **同进程挂载**：`rmcp` 的 Streamable HTTP 服务是一个 Tower service，
//!   直接 `nest_service` 到 axum 子路由后 merge 进主 Router，不新增部署单元；
//! - **只读**：只暴露查询类工具，不暴露任何写操作。写接口有 step-up、
//!   邀请码、Origin 白名单等多重护栏，MCP 一开始不应绕过它们；
//! - **鉴权**：`MCP_TOKEN` 未设置时整个 `/mcp` 路由不存在（而不是返回 404），
//!   设置后要求 `Authorization: Bearer <token>`。比较走常量时间，避免时序侧信道；
//! - **限流**：复用 `rate::RateLimiter`，按客户端 IP 独立计数，
//!   防止 AI 批量拉取打爆数据库连接池；
//! - **不复用主应用的层**：会话（Cookie）、CORS、Origin 白名单都是给浏览器
//!   和管理端设计的，MCP 客户端不是浏览器，merge 放在主应用 layer 之后，
//!   天然绕开这些不相关的层（含 `guard_origin`：MCP 的 POST 没有 Origin 头，
//!   即便走主应用层也不会被拦，但分开挂更清晰）。

use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum::Router;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;

use crate::models::{ContentFormat, SiteSettings};
use crate::rate::RateLimiter;
use crate::service;

/// MCP 请求按 IP 的限流额度：每分钟 120 次。
///
/// MCP 客户端一次会话只发少量 JSON-RPC 请求，但 AI 循环里可能高频调工具；
/// 120/min 对单人使用足够宽松，又能在 token 泄露时兜住批量拉取。
const MCP_REQUESTS_PER_MINUTE: u32 = 120;

/// MCP 工具层。`Clone` 是必需的：Streamable HTTP 的 service 工厂
/// 会在（无状态模式下）每次请求时克隆一份 handler。
#[derive(Clone)]
pub struct KychMcp {
    pool: PgPool,
    content_root: PathBuf,
    site_url: String,
}

/// 单条搜索结果（SQL 查询 + 摘要截取共用）。
#[derive(Debug, sqlx::FromRow)]
struct SearchRow {
    slug: String,
    title: String,
    format: String,
    body: String,
}

#[tool_router]
impl KychMcp {
    pub fn new(pool: PgPool, content_root: PathBuf, site_url: String) -> Self {
        Self {
            pool,
            content_root,
            site_url,
        }
    }

    /// 把用户输入的页面路径归一化成数据库里的输出路径。
    ///
    /// 接受 `xef/index`、`/xef/index`、`xef/index.html` 三种写法；
    /// 其他校验交给数据库精确匹配，不做宽松推断。
    fn normalize_slug(input: &str) -> String {
        let trimmed = input.trim().trim_start_matches('/');
        if trimmed.ends_with(".html") {
            trimmed.to_string()
        } else {
            format!("{trimmed}.html")
        }
    }

    #[tool(
        description = "列出站点所有已发布页面：输出路径（slug）、标题、格式与更新时间。返回 JSON 数组，slug 可直接用于 get_page。"
    )]
    async fn list_pages(&self) -> Result<CallToolResult, McpError> {
        let pages = service::list_published(&self.pool)
            .await
            .map_err(internal_error)?;
        let items: Vec<_> = pages
            .iter()
            .map(|page| {
                json!({
                    "slug": page.slug,
                    "title": page.title,
                    "format": page.format,
                    "updated_at": page.updated_at.to_rfc3339(),
                })
            })
            .collect();
        Ok(text_result(json!({ "count": items.len(), "pages": items }).to_string()))
    }

    #[tool(
        description = "按路径读取一篇已发布页面的源内容。路径参数接受 xef/index、/xef/index 或 xef/index.html 三种写法。返回标题、格式（markdown/html/typst/wikidot）与正文源码。"
    )]
    async fn get_page(
        &self,
        Parameters(GetPageParams { slug }): Parameters<GetPageParams>,
    ) -> Result<CallToolResult, McpError> {
        let normalized = Self::normalize_slug(&slug);
        let page = sqlx::query_as::<_, crate::models::Content>(
            "SELECT * FROM contents WHERE slug = $1 AND published = TRUE",
        )
        .bind(&normalized)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            McpError::invalid_params(format!("页面不存在或未发布: {slug}"), None)
        })?;

        Ok(text_result(
            json!({
                "slug": page.slug,
                "title": page.title,
                "format": page.format,
                "updated_at": page.updated_at.to_rfc3339(),
                "body": page.body,
            })
            .to_string(),
        ))
    }

    #[tool(
        description = "全文搜索已发布页面的标题与正文（大小写不敏感的子串匹配）。返回每条结果的路径、标题、格式与命中位置附近的一段摘要。"
    )]
    async fn search_content(
        &self,
        Parameters(SearchParams { query }): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(McpError::invalid_params("搜索词不能为空", None));
        }
        if query.len() > 200 {
            return Err(McpError::invalid_params("搜索词过长（上限 200 字节）", None));
        }

        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let rows = sqlx::query_as::<_, SearchRow>(
            "SELECT slug, title, format, body FROM contents
             WHERE published = TRUE AND (title ILIKE $1 OR body ILIKE $1)
             ORDER BY updated_at DESC
             LIMIT 20",
        )
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(internal_error)?;

        let items: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "slug": row.slug,
                    "title": row.title,
                    "format": row.format,
                    "excerpt": excerpt(&row.body, query, 80),
                })
            })
            .collect();
        Ok(text_result(json!({ "count": items.len(), "hits": items }).to_string()))
    }

    #[tool(
        description = "获取站点基本信息：站点名称、导航、logo、页面总数与 API 地址。调用其他工具前可先用它了解站点概况。"
    )]
    async fn site_info(&self) -> Result<CallToolResult, McpError> {
        let settings: SiteSettings = service::get_settings(&self.pool)
            .await
            .map_err(internal_error)?;
        let (published, total) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*) FILTER (WHERE published), COUNT(*) FROM contents",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(internal_error)?;

        Ok(text_result(
            json!({
                "site_name": settings.site_name,
                "logo_url": settings.logo_url,
                "nav": settings.nav,
                "site_url": self.site_url,
                "published_pages": published,
                "total_pages": total,
            })
            .to_string(),
        ))
    }
}

#[tool_handler(
    name = "kych-net",
    version = "0.3.0",
    instructions = "KychNet 个人站点的内容只读接口。查询内容用 search_content 定位页面，再用 get_page 读全文；list_pages 可枚举全部已发布页面。"
)]
impl ServerHandler for KychMcp {}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetPageParams {
    /// 页面路径，例如 xef/index、/xef/index 或 xef/index.html。
    slug: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchParams {
    /// 搜索词（大小写不敏感的子串匹配）。
    query: String,
}

fn text_result(payload: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(payload)])
}

fn internal_error(error: impl std::fmt::Display) -> McpError {
    tracing::warn!(%error, "MCP 工具执行失败");
    McpError::internal_error("内部错误", None)
}

/// 在正文里找第一个命中位置，返回前后各 `context` 个字符的摘要。
///
/// 大小写不敏感（用 Unicode 小写折叠比较），窗口按字符边界裁剪，
/// 换行压成空格，让结果适合直接给模型读。
fn excerpt(body: &str, needle: &str, context: usize) -> String {
    let lower_body = body.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let Some(byte_pos) = lower_body.find(&lower_needle) else {
        // 没命中（比如命中来自 title）就返回开头一段当摘要。
        let head: String = body.chars().take(context * 2 + 2).collect();
        return head.replace(['\n', '\r'], " ");
    };

    // 字节位置换算成字符位置，再按字符数开窗，避免切进多字节字符。
    let char_pos = lower_body[..byte_pos].chars().count();
    let total_chars = body.chars().count();
    let start = char_pos.saturating_sub(context);
    let end = (char_pos + lower_needle.chars().count() + context).min(total_chars);
    let snippet: String = body.chars().skip(start).take(end - start).collect();
    snippet.replace(['\n', '\r'], " ")
}

/// Bearer token 的常量时间比较。
///
/// 先各自 SHA-256 折叠成等长摘要再逐字节异或累积，比较耗时与内容无关；
/// 顺手也把「长度不同提前返回」这条短路径藏进了哈希里。
fn token_matches(provided: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    let (a, b) = (Sha256::digest(provided.as_bytes()), Sha256::digest(expected.as_bytes()));
    a.iter()
        .zip(b.iter())
        .fold(0u8, |diff, (x, y)| diff | (x ^ y))
        == 0
}

/// `/mcp` 的鉴权 + 限流中间件状态。
#[derive(Clone)]
struct McpGuard {
    token: String,
    limits: RateLimiter,
    trust_proxy_headers: bool,
}

/// 从请求头推导限流用的客户端 IP，口径与 `api::client_ip` 一致：
/// 只有显式声明信任代理头时才读 `X-Forwarded-For`，否则用 TCP 对端地址。
fn guard_ip(guard: &McpGuard, headers: &HeaderMap, peer: std::net::SocketAddr) -> String {
    if guard.trust_proxy_headers {
        if let Some(value) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
        {
            if let Some(first) = value.split(',').next().map(str::trim) {
                if !first.is_empty() {
                    return first.to_ascii_lowercase();
                }
            }
        }
    }
    peer.ip().to_string()
}

async fn mcp_guard(
    State(guard): State<McpGuard>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let headers = req.headers().clone();
    let ip = guard_ip(&guard, &headers, peer);

    if let Some(retry_after) =
        guard
            .limits
            .check(&format!("mcp:ip:{ip}"), MCP_REQUESTS_PER_MINUTE, Duration::from_secs(60))
    {
        tracing::warn!(%ip, retry_after = ?retry_after, "MCP 请求超限");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                header::RETRY_AFTER,
                retry_after.as_secs().max(1).to_string(),
            )],
            Json(json!({ "error": "请求过于频繁" })),
        )
            .into_response();
    }

    // 失败响应统一成同一条消息，不区分「没带 token」「带错 token」，
    // 避免给试探者任何可区分的信号。
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let authorized = provided.is_some_and(|token| token_matches(token, &guard.token));
    if !authorized {
        tracing::warn!(%ip, "MCP 鉴权失败");
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({ "error": "未授权" })),
        )
            .into_response();
    }

    next.run(req).await
}

/// 构建挂在 `/mcp` 上的 MCP 子路由。
///
/// 返回的 Router 自带鉴权与限流层，应 merge 到主应用 layer 链之后，
/// 避免沾上会话、CORS、Origin 白名单这些浏览器侧的层。
pub fn router(state: KychMcp, token: String, limits: RateLimiter, trust_proxy_headers: bool) -> Router {
    // 无状态模式（config 默认即 stateless 的新版本语义按 Default 走）：
    // 每个请求独立处理，不做会话亲和，单实例足够。
    // rmcp 默认只允许 localhost 的 Host 头；线上经 nginx 反代后 Host 是
    // api.kych.net / kych.net，必须显式放行，否则会得到 "Host header is not allowed"。
    let config = StreamableHttpServerConfig::default().with_allowed_hosts([
        "api.kych.net",
        "kych.net",
        "localhost",
        "127.0.0.1",
        "::1",
    ]);
    let service = StreamableHttpService::new(
        move || Ok(state.clone()),
        LocalSessionManager::default().into(),
        config,
    );

    let guard = McpGuard {
        token,
        limits,
        trust_proxy_headers,
    };

    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(guard, mcp_guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_page_slugs() {
        assert_eq!(KychMcp::normalize_slug("xef/index"), "xef/index.html");
        assert_eq!(KychMcp::normalize_slug("/xef/index"), "xef/index.html");
        assert_eq!(KychMcp::normalize_slug("xef/index.html"), "xef/index.html");
        assert_eq!(KychMcp::normalize_slug(" index "), "index.html");
        assert_eq!(KychMcp::normalize_slug("index.html"), "index.html");
    }

    #[test]
    fn builds_excerpt_around_first_hit() {
        let body = "第一行\n第二行包含目标词\n第三行";
        let out = excerpt(body, "目标词", 10);
        assert!(out.contains("目标词"), "{out}");
        assert!(!out.contains('\n'), "{out}");
    }

    #[test]
    fn excerpt_is_case_insensitive_and_char_safe() {
        let body = "Hello 世界 World";
        assert!(excerpt(body, "世界", 3).contains("世界"));
        // 多字节字符不被切碎：摘要应当是无乱码的有效字符串。
        assert!(excerpt(body, "hello", 2).contains("Hello"));
    }

    #[test]
    fn excerpt_falls_back_to_head_when_no_hit() {
        let out = excerpt("abcdef\nghij", "zzz", 4);
        assert_eq!(out, "abcdef ghi");
    }

    #[test]
    fn excerpt_handles_multibyte_needle_positions() {
        // 命中位置在多字节字符之后时，字符窗口必须仍然落在边界上。
        let body = "一二三四五六七八九十目标词其后内容";
        let out = excerpt(body, "目标词", 5);
        assert!(out.contains("目标词"));
        assert!(out.contains("其后"));
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(token_matches("secret", "secret"));
        assert!(!token_matches("secret", "secreT"));
        assert!(!token_matches("", "secret"));
        assert!(!token_matches("secret", ""));
    }

    #[test]
    fn formats_are_parsed_like_db_strings() {
        // get_page / list_pages 返回的 format 字符串要能映射回枚举，
        // 未识别的值按 HTML 兜底（与 models::Content::format 一致）。
        assert_eq!(ContentFormat::parse("markdown"), Some(ContentFormat::Markdown));
        assert_eq!(ContentFormat::parse("nope"), None);
    }
}
