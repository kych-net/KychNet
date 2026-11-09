//! MCP（Model Context Protocol）服务器：把站点内容与项目文件暴露给 AI 客户端。
//!
//! 设计要点：
//! - **同进程挂载**：`rmcp` 的 Streamable HTTP 服务是一个 Tower service，
//!   直接 `nest_service` 到 axum 子路由后 merge 进主 Router，不新增部署单元；
//! - **读写**：内容与项目文件写入需要 MCP OAuth 登录；项目 Git 提交身份取当前 token 对应用户。
//!   写操作仍走 service/project 层的路径校验，并在写入后触发构建；
//! - **鉴权**：标准 MCP OAuth 2.1，`src/oauth.rs` 提供发现、动态注册、授权码 + PKCE；
//!   access token 存 SHA-256 哈希，短过期，refresh token 轮换；
//! - **限流**：按客户端 IP 计数，额度记录落在 Postgres（`mcp_rate_limits`），
//!   多实例部署时共享同一份计数，防止 AI 批量拉取打爆数据库连接池；
//! - **不复用主应用的层**：会话（Cookie）、CORS、Origin 白名单都是给浏览器
//!   和管理端设计的，MCP 客户端不是浏览器，merge 放在主应用 layer 之后；
//!   鉴权由 MCP 自己完成。

use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum::Router;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{ContentFormat, SiteSettings, User};
use crate::project;
use crate::service;

/// MCP 请求按 IP 的限流额度：每分钟 120 次，窗口 60 秒。
///
/// MCP 客户端一次会话只发少量 JSON-RPC 请求，但 AI 循环里可能高频调工具；
/// 120/min 对单人使用足够宽松，又能在 token 泄露时兜住批量拉取。
const MCP_REQUESTS_PER_MINUTE: i32 = 120;
const MCP_WINDOW_SECS: f64 = 60.0;
/// 限流查询不能拖垮正常请求：超时或出错时放行，不让限流器本身变成单点。
const MCP_LIMIT_TIMEOUT: Duration = Duration::from_secs(2);

/// MCP 工具层。`Clone` 是必需的：Streamable HTTP 的 service 工厂
/// 会为每个会话克隆一份 handler。
#[derive(Clone)]
pub struct KychMcp {
    pool: PgPool,
    content_root: PathBuf,
    site_url: String,
    build_out_dir: PathBuf,
    exe: PathBuf,
}

/// 由 MCP 鉴权层写入 HTTP request extensions，再通过 rmcp 的
/// `Extension<Parts>` 传给工具；项目提交使用当前登录用户。
#[derive(Clone)]
pub struct McpUser {
    pub user: User,
    pub scope: String,
}

/// 单条搜索结果（SQL 查询 + 摘要截取共用）。
#[derive(Debug, sqlx::FromRow)]
struct SearchRow {
    slug: String,
    format: String,
    body: String,
}

#[tool_router]
impl KychMcp {
    pub fn new(
        pool: PgPool,
        content_root: PathBuf,
        site_url: String,
        build_out_dir: PathBuf,
        exe: PathBuf,
    ) -> Self {
        Self {
            pool,
            content_root,
            site_url,
            build_out_dir,
            exe,
        }
    }

    async fn run_build(&self, only: Option<&str>) -> Result<(), McpError> {
        let mut command = tokio::process::Command::new(&self.exe);
        command
            .arg("build")
            .arg("--out")
            .arg(&self.build_out_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(slug) = only {
            command.arg("--only").arg(slug);
        }
        let output = command
            .output()
            .await
            .map_err(|e| McpError::internal_error(format!("构建进程启动失败: {e}"), None))?;
        if !output.status.success() {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let tail: String = text.chars().rev().take(2000).collect::<String>().chars().rev().collect();
            return Err(McpError::internal_error(
                format!("编译失败: {tail}"),
                None,
            ));
        }
        Ok(())
    }

    async fn find_project(&self, slug: &str) -> Result<project::Project, McpError> {
        let p = project::find_project_by_slug(&self.pool, slug)
            .await
            .map_err(tool_error)?
            .ok_or_else(|| McpError::invalid_params(format!("项目不存在: {slug}"), None))?;
        // Git/HTTP 通道的 `git_project` 已拦下归档项目，这里补齐 MCP 侧：
        // 否则即使 `list_projects` 不再列出，直接指定 slug 仍可读写。
        if p.archived_at.is_some() {
            return Err(McpError::invalid_params(
                format!("项目已归档，不能读写: {slug}"),
                None,
            ));
        }
        Ok(p)
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
        description = "列出站点所有已发布页面：输出路径（slug）、源文件名、格式与更新时间。返回 JSON 数组，slug 可直接用于 get_page。"
    )]
    async fn list_pages(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "content:read")?;
        let pages = service::list_published(&self.pool)
            .await
            .map_err(tool_error)?;
        let items: Vec<_> = pages
            .iter()
            .map(|page| {
                json!({
                    "slug": page.slug,
                    "filename": mcp_filename(&page.slug, &page.format),
                    "format": page.format,
                    "updated_at": page.updated_at.to_rfc3339(),
                })
            })
            .collect();
        Ok(text_result(json!({ "count": items.len(), "pages": items }).to_string()))
    }

    #[tool(
        description = "按路径读取一篇已发布页面的源内容。路径参数接受 xef/index、/xef/index 或 xef/index.html 三种写法。返回源文件名、格式（markdown/html/typst/wikidot）与正文源码。"
    )]
    async fn get_page(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(GetPageParams { slug }): Parameters<GetPageParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "content:read")?;
        let normalized = Self::normalize_slug(&slug);
        let page = sqlx::query_as::<_, crate::models::Content>(
            "SELECT * FROM contents WHERE slug = $1 AND published = TRUE",
        )
        .bind(&normalized)
        .fetch_optional(&self.pool)
        .await
        .map_err(tool_error)?
        .ok_or_else(|| {
            McpError::invalid_params(format!("页面不存在或未发布: {slug}"), None)
        })?;

        Ok(text_result(
            json!({
                "slug": page.slug,
                "filename": mcp_filename(&page.slug, &page.format),
                "format": page.format,
                "updated_at": page.updated_at.to_rfc3339(),
                "body": page.body,
            })
            .to_string(),
        ))
    }

    #[tool(
        description = "全文搜索已发布页面的正文（大小写不敏感的子串匹配）。返回每条结果的路径、源文件名、格式与命中位置附近的一段摘要。"
    )]
    async fn search_content(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(SearchParams { query }): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "content:read")?;
        let query = query.trim();
        if query.is_empty() {
            return Err(McpError::invalid_params("搜索词不能为空", None));
        }
        if query.len() > 200 {
            return Err(McpError::invalid_params("搜索词过长（上限 200 字节）", None));
        }

        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let rows = sqlx::query_as::<_, SearchRow>(
            "SELECT slug, format, body FROM contents
             WHERE published = TRUE AND body ILIKE $1
             ORDER BY updated_at DESC
             LIMIT 20",
        )
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(tool_error)?;

        let items: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "slug": row.slug,
                    "filename": mcp_filename(&row.slug, &row.format),
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
    async fn site_info(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "content:read")?;
        let settings: SiteSettings = service::get_settings(&self.pool)
            .await
            .map_err(tool_error)?;
        let (published, total) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*) FILTER (WHERE published), COUNT(*) FROM contents",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(tool_error)?;

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
    #[tool(
        description = "新增或更新一篇内容并自动编译该页。format 支持 markdown/html/typst/wikidot/css；slug 使用 /xef/index、/reff/93df/l.html 或 /css/theme 形式。"
    )]
    async fn save_content(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SaveContentParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "content:write")?;
        let author_id = parts
            .extensions
            .get::<McpUser>()
            .map(|context| context.user.id)
            .ok_or_else(|| McpError::invalid_params("缺少认证上下文", None))?;
        let requested = ContentFormat::parse(params.format.trim().to_ascii_lowercase().as_str())
            .ok_or_else(|| McpError::invalid_params(format!("未知格式: {}", params.format), None))?;
        let (slug, format) = service::normalize_content_path(&params.slug, requested)
            .map_err(|e| McpError::invalid_params(format!("路径非法: {e}"), None))?;
        let item = service::upsert_content(
            &self.pool,
            &slug,
            format.as_str(),
            &params.body,
            params.published,
            author_id,
            "",
        )
        .await
        .map_err(tool_error)?;
        self.run_build(Some(&slug)).await?;
        Ok(text_result(json!({ "item": item, "built": true }).to_string()))
    }

    #[tool(description = "删除一篇内容并清理静态产物；网站主页 index.html 不允许删除。")]
    async fn delete_content(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<DeleteContentParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "content:write")?;
        let (slug, _) = service::normalize_content_path(&params.slug, ContentFormat::Html)
            .map_err(|e| McpError::invalid_params(format!("路径非法: {e}"), None))?;
        if slug == service::HOME_SLUG {
            return Err(McpError::invalid_params("网站主页不能删除，只能编辑", None));
        }
        let row: Option<(uuid::Uuid, String)> =
            sqlx::query_as("SELECT id, slug FROM contents WHERE slug = $1")
                .bind(&slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(tool_error)?;
        let Some((id, slug)) = row else {
            return Err(McpError::invalid_params(format!("页面不存在: {}", params.slug), None));
        };
        sqlx::query("DELETE FROM contents WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(tool_error)?;
        self.run_build(Some(&slug)).await?;
        Ok(text_result(json!({ "deleted": slug }).to_string()))
    }

    #[tool(description = "列出所有项目（项目名 slug、描述、默认分支、更新时间）。")]
    async fn list_projects(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "project:read")?;
        let projects = project::list_projects(&self.pool, false)
            .await
            .map_err(tool_error)?;
        Ok(text_result(json!({ "projects": projects }).to_string()))
    }

    #[tool(description = "列出项目某个目录下的文件；branch 省略时用默认分支，path 省略表示根目录。")]
    async fn list_project_files(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ProjectListParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "project:read")?;
        let p = self.find_project(&params.project).await?;
        let branch = params.branch.unwrap_or_else(|| p.default_branch.clone());
        let path = params.path.unwrap_or_default();
        let items = project::list_dir(&self.pool, p.id, &branch, &path)
            .await
            .map_err(tool_error)?;
        Ok(text_result(json!({ "project": p.slug, "branch": branch, "path": path, "items": items }).to_string()))
    }

    #[tool(description = "读取项目中的单个 UTF-8 文件内容。")]
    async fn read_project_file(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ProjectReadParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "project:read")?;
        let p = self.find_project(&params.project).await?;
        let branch = params.branch.unwrap_or_else(|| p.default_branch.clone());
        let item = project::read_file(&self.pool, p.id, &branch, &params.path)
            .await
            .map_err(tool_error)?
            .ok_or_else(|| McpError::invalid_params(format!("文件不存在: {}", params.path), None))?;
        Ok(text_result(json!({ "project": p.slug, "branch": branch, "file": item }).to_string()))
    }

    #[tool(description = "新增或更新项目文件并提交；默认分支的修改会自动触发整站构建。")]
    async fn write_project_file(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ProjectWriteParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "project:write")?;
        let p = self.find_project(&params.project).await?;
        let branch = params.branch.unwrap_or_else(|| p.default_branch.clone());
        let actor = mcp_actor(&parts)?;
        let result = project::write_file(
            &self.pool,
            actor,
            p.id,
            &branch,
            &params.path,
            &params.content,
            params.message.as_deref(),
            params.base_oid.as_deref(),
        )
        .await
        .map_err(tool_error)?;
        if branch == p.default_branch {
            self.run_build(None).await?;
        }
        Ok(text_result(json!({ "project": p.slug, "branch": branch, "item": result }).to_string()))
    }

    #[tool(description = "删除项目文件并提交；默认分支的修改会自动触发整站构建。")]
    async fn delete_project_file(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ProjectDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        require_scope(&parts, "project:write")?;
        let p = self.find_project(&params.project).await?;
        let branch = params.branch.unwrap_or_else(|| p.default_branch.clone());
        let actor = mcp_actor(&parts)?;
        let result = project::delete_file(
            &self.pool,
            actor,
            p.id,
            &branch,
            &params.path,
            params.message.as_deref(),
        )
        .await
        .map_err(tool_error)?;
        if branch == p.default_branch {
            self.run_build(None).await?;
        }
        Ok(text_result(json!({ "project": p.slug, "branch": branch, "item": result }).to_string()))
    }
}

#[tool_handler(
    name = "kych-net",
    version = "0.3.0",
    instructions = "KychNet 内容与项目接口。查询内容用 search_content/get_page；写内容用 save_content/delete_content；项目用 list_projects、list_project_files、read_project_file、write_project_file、delete_project_file。"
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

#[derive(Debug, Deserialize, JsonSchema)]
struct SaveContentParams {
    slug: String,
    format: String,
    body: String,
    #[serde(default = "default_true")]
    published: bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Deserialize, JsonSchema)]
struct DeleteContentParams { slug: String }

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectListParams {
    project: String,
    branch: Option<String>,
    path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectReadParams {
    project: String,
    path: String,
    branch: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectWriteParams {
    project: String,
    path: String,
    content: String,
    branch: Option<String>,
    message: Option<String>,
    base_oid: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectDeleteParams {
    project: String,
    path: String,
    branch: Option<String>,
    message: Option<String>,
}

fn mcp_actor(parts: &Parts) -> Result<&User, McpError> {
    parts
        .extensions
        .get::<McpUser>()
        .map(|user| &user.user)
        .ok_or_else(|| McpError::internal_error("缺少 MCP 登录用户", None))
}

fn require_scope(parts: &Parts, needed: &str) -> Result<(), McpError> {
    let allowed = parts
        .extensions
        .get::<McpUser>()
        .map(|user| user.scope.split_whitespace().any(|scope| scope == needed))
        .unwrap_or(false);
    if allowed {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            format!("当前授权缺少权限: {needed}"),
            None,
        ))
    }
}

fn mcp_filename(slug: &str, format: &str) -> String {
    let name = slug.rsplit('/').next().unwrap_or(slug);
    let ext = match format {
        "markdown" => ".md",
        "html" => ".html",
        "typst" => ".typ",
        "wikidot" => ".wj",
        "css" => ".css",
        _ => ".html",
    };
    match name.strip_suffix(".html") {
        Some(stem) => format!("{stem}{ext}"),
        None => name.to_string(),
    }
}

fn text_result(payload: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(payload)])
}

/// 把工具层的错误映射成 JSON-RPC 错误。
///
/// 口径与管理端 `ApiError::into_response` 一致：错误链里带 `sqlx::Error` /
/// `std::io::Error` 的是底层故障，只记日志并统一回「内部错误」，避免把约束名、
/// 绝对路径等内部细节回显给模型；其余是项目层的业务校验（非法路径、乐观锁
/// 冲突、分支不存在……），原文对调用方是可操作的提示，必须透传。
fn tool_error(error: impl Into<anyhow::Error>) -> McpError {
    let error = error.into();
    let internal = error
        .chain()
        .any(|cause| cause.is::<sqlx::Error>() || cause.is::<std::io::Error>());
    if internal {
        tracing::warn!(%error, "MCP 工具执行失败");
        McpError::internal_error("内部错误", None)
    } else {
        McpError::invalid_params(error.to_string(), None)
    }
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

/// `/mcp` 的鉴权 + 限流中间件状态。
#[derive(Clone)]
struct McpGuard {
    pool: PgPool,
    trust_proxy_headers: bool,
}

/// 消耗一次限流额度，超额返回还需等待的时间。
///
/// 计数落在 Postgres 而不是进程内存：线上多实例部署时 `rate::RateLimiter`
/// 每台各自计数，会把 120/min 摊薄成 120×N/min，实测完全不触发。用一条
/// 原子 UPSERT 让所有实例共享同一份额度。
///
/// 查询失败或超时时放行（fail-open）：限流器本就是为保护数据库而存在，
/// 数据库异常时再拒绝请求只会让故障扩大。
async fn mcp_limit(pool: &PgPool, key: &str) -> Option<Duration> {
    let query = sqlx::query_as::<_, (i32, f64)>(
        "INSERT INTO mcp_rate_limits (client_key, hits, window_until)
         VALUES ($1, 1, now() + make_interval(secs => $2))
         ON CONFLICT (client_key) DO UPDATE SET
           hits = CASE WHEN mcp_rate_limits.window_until <= now()
                       THEN 1 ELSE mcp_rate_limits.hits + 1 END,
           window_until = CASE WHEN mcp_rate_limits.window_until <= now()
                               THEN now() + make_interval(secs => $2)
                               ELSE mcp_rate_limits.window_until END
         RETURNING hits,
           GREATEST(0.0::float8,
                    EXTRACT(EPOCH FROM (window_until - now()))::float8)",
    )
    .bind(key)
    .bind(MCP_WINDOW_SECS)
    .fetch_optional(pool);

    let row = match tokio::time::timeout(MCP_LIMIT_TIMEOUT, query).await {
        Ok(Ok(row)) => row,
        Ok(Err(error)) => {
            tracing::warn!(%error, "MCP 限流计数失败，放行本次请求");
            None
        }
        Err(_) => {
            tracing::warn!("MCP 限流计数超时，放行本次请求");
            None
        }
    }?;
    let (hits, retry_secs) = row;
    (hits > MCP_REQUESTS_PER_MINUTE).then(|| Duration::from_secs_f64(retry_secs))
}

/// 从请求头推导限流用的客户端 IP，口径与 `api::client_ip` 一致：
/// 只有显式声明信任代理头时才读 `X-Forwarded-For`，否则用 TCP 对端地址。
fn guard_ip(trust_proxy_headers: bool, headers: &HeaderMap, peer: std::net::SocketAddr) -> String {
    if trust_proxy_headers {
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
    let ip = guard_ip(guard.trust_proxy_headers, &headers, peer);

    if let Some(retry_after) = mcp_limit(&guard.pool, &format!("mcp:ip:{ip}")).await {
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

    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(token) = provided else {
        return unauthorized();
    };
    let hash = Sha256::digest(token.as_bytes()).to_vec();
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT user_id, scope, resource FROM oauth_tokens
         WHERE access_hash = $1 AND revoked_at IS NULL AND access_expires_at > now()",
    )
    .bind(hash)
    .fetch_optional(&guard.pool)
    .await
    .ok()
    .flatten();
    let Some((user_id, scope, resource)) = row else {
        return unauthorized();
    };
    if resource != crate::oauth::MCP_RESOURCE {
        return unauthorized();
    }
    let user: Option<User> = sqlx::query_as(
        "SELECT * FROM users WHERE id = $1 AND NOT disabled",
    )
    .bind(user_id)
    .fetch_optional(&guard.pool)
    .await
    .ok()
    .flatten();
    let Some(user) = user else {
        return unauthorized();
    };
    let mut req = req;
    req.extensions_mut().insert(McpUser { user, scope });
    next.run(req).await
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            "Bearer resource_metadata=\"https://kych.net/.well-known/oauth-protected-resource\"",
        )],
        Json(json!({ "error": "未授权" })),
    )
        .into_response()
}

/// 构建挂在 `/mcp` 上的 MCP 子路由。
///
/// 返回的 Router 自带鉴权与限流层，应 merge 到主应用 layer 链之后，
/// 避免沾上会话、CORS、Origin 白名单这些浏览器侧的层。
pub fn router(state: KychMcp, pool: PgPool, trust_proxy_headers: bool) -> Router {
    // Streamable HTTP 是**有状态会话**：`initialize` 返回 Mcp-Session-Id，
    // 后续请求必须带回，`DELETE /mcp` 会终止会话。LocalSessionManager 是
    // 进程内实现，多实例部署需要会话亲和；限流已改成跨实例共享。
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
        pool: pool.clone(),
        trust_proxy_headers,
    };

    // 过期计数行定期清理，避免表随历史 IP 无限增长。
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let cleanup_pool = pool;
        handle.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(600));
            ticker.tick().await; // 首轮立即触发，跳过以免启动瞬间扫表
            loop {
                ticker.tick().await;
                if let Err(error) = sqlx::query(
                    "DELETE FROM mcp_rate_limits WHERE window_until < now() - interval '1 hour'",
                )
                .execute(&cleanup_pool)
                .await
                {
                    tracing::warn!(%error, "清理 MCP 限流记录失败");
                }
            }
        });
    }

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
    fn formats_are_parsed_like_db_strings() {
        // get_page / list_pages 返回的 format 字符串要能映射回枚举，
        // 未识别的值按 HTML 兜底（与 models::Content::format 一致）。
        assert_eq!(ContentFormat::parse("markdown"), Some(ContentFormat::Markdown));
        assert_eq!(ContentFormat::parse("nope"), None);
    }

    #[test]
    fn tool_error_keeps_business_messages() {
        // 乐观锁冲突、路径校验这些提示必须原样透给调用方，才能按提示重试。
        let err = tool_error(anyhow::anyhow!("文件已被其他人修改，请刷新后重试"));
        assert!(err.message.contains("文件已被其他人修改"));
        let err = tool_error(anyhow::anyhow!("非法文件路径"));
        assert!(err.message.contains("非法文件路径"));
    }

    #[test]
    fn tool_error_hides_low_level_details() {
        let err = tool_error(std::io::Error::other("/var/lib/db/secret"));
        assert_eq!(err.message, "内部错误");
        // 被 context 包裹的底层错误同样算内部错误，不能泄漏路径。
        let wrapped = anyhow::Error::new(std::io::Error::other("/etc/passwd"))
            .context("写入失败");
        let err = tool_error(wrapped);
        assert_eq!(err.message, "内部错误");
    }

    #[test]
    fn tool_error_hides_sql_errors_even_when_wrapped() {
        let wrapped = anyhow::Error::new(sqlx::Error::RowNotFound).context("查询项目失败");
        assert_eq!(tool_error(wrapped).message, "内部错误");
    }

    #[test]
    fn guard_ip_only_trusts_forwarded_header_when_configured() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.9, 10.0.0.1".parse().unwrap());
        let peer: std::net::SocketAddr = "192.0.2.1:1234".parse().unwrap();
        assert_eq!(guard_ip(true, &headers, peer), "203.0.113.9");
        assert_eq!(guard_ip(false, &headers, peer), "192.0.2.1");
    }
}
