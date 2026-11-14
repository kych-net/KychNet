//! 从 `api.rs` 拆出的子模块。路由注册与公共类型见 `super`（`api/mod.rs`）。
use super::*;

pub(crate) async fn contents(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let items: Vec<serde_json::Value> = service::list_contents(&state.pool)
        .await?
        .into_iter()
        .map(|content| {
            let mut value = json!(content);
            if let Some(object) = value.as_object_mut() {
                object.insert("filename".to_string(), json!(content.filename()));
            }
            value
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub(crate) struct PreviewRequest {
    format: String,
    body: String,
}

#[derive(Deserialize)]
pub(crate) struct ContentRequest {
    /// 编辑已有内容时带 id，改名才会更新原记录而不是新建一条。
    #[serde(default)]
    id: Option<Uuid>,
    slug: String,
    format: String,
    body: String,
    /// 页面自定义 <head> 内容。仅对能渲染成 HTML 的格式生效，管理员可写。
    #[serde(default)]
    head: String,
    #[serde(default)]
    published: bool,
    /// 可选的版本说明，落入精简版历史；缺省为空。
    #[serde(default)]
    message: String,
}

pub(crate) async fn preview_content(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<PreviewRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let format = ContentFormat::parse(&req.format)
        .ok_or_else(|| ApiError::with_status(StatusCode::BAD_REQUEST, "未知内容格式"))?;
    if format == ContentFormat::Svg {
        return Ok(Json(json!({ "html": req.body })));
    }
    if !matches!(
        format,
        ContentFormat::Markdown | ContentFormat::Html | ContentFormat::Wikidot
    ) {
        return Err(ApiError::with_status(
            StatusCode::BAD_REQUEST,
            "实时预览目前只支持 Markdown、HTML、Wikidot 和 SVG",
        ));
    }

    let (_, body) = crate::build::parse_page_styles(&req.body)?;
    let mut includes = HashMap::new();
    if format == ContentFormat::Wikidot {
        let published = service::list_published(&state.pool).await?;
        for content in published
            .into_iter()
            .filter(|content| content.format() == ContentFormat::Wikidot)
        {
            let stem = content.slug.strip_suffix(".html").unwrap_or(&content.slug);
            let page = stem.replace('/', ":");
            includes.insert(page.clone(), content.body.clone());
            let normalized = ftml::data::PageRef::page_only(&page).page;
            includes.insert(normalized, content.body.clone());
        }
    }
    let ctx = render::DocContext {
        page: "preview",
        title: "预览",
        includes: &includes,
    };
    let rendered = render::render_doc(format, &body, &state.content_root, &ctx)?;
    Ok(Json(json!({ "html": rendered.html })))
}

pub(crate) async fn save_content(
    State(state): State<AppState>,
    session: Session,
    Json(req): Json<ContentRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let Some(format) = crate::models::ContentFormat::parse(&req.format) else {
        api_bail!("format 只能是 markdown、html、typst、wikidot、css 或 svg");
    };
    // 路径在这里归一化成输出路径：
    //   /xef/ -> xef/index.html
    //   /eoe/index.md -> eoe/index.html
    // 同一路径只有一条记录，因此 .html/.md/.typ 天然互斥。
    let (slug, format) = service::normalize_content_path(&req.slug, format)?;
    // 自定义 head 只对会产出 HTML 页面的格式有意义；css/svg 直接丢弃，
    // 免得内容作者以为写了会生效。
    let head = if format.renders_html() {
        validate_page_head(&req.head)?
    } else {
        String::new()
    };
    if format == crate::models::ContentFormat::Svg {
        crate::svg::validate_svg(req.body.as_bytes())?;
    }
    let existing = if let Some(id) = req.id {
        Some(
            service::find_content_by_id(&state.pool, id)
                .await?
                .context("内容不存在")?,
        )
    } else {
        None
    };
    if let Some(existing) = &existing {
        if existing.slug == service::HOME_SLUG && slug != service::HOME_SLUG {
            api_reject!(StatusCode::FORBIDDEN, "网站主页路径不能修改");
        }
    }
    let content = if let Some(id) = req.id {
        service::update_content(
            &state.pool,
            id,
            &slug,
            format.as_str(),
            &req.body,
            &head,
            req.published,
            user.id,
            req.message.trim(),
        )
        .await?
    } else {
        service::upsert_content(
            &state.pool,
            &slug,
            format.as_str(),
            &req.body,
            &head,
            req.published,
            user.id,
            req.message.trim(),
        )
        .await?
    };

    // 保存后立即编译当前页；若改过路径，顺带删除旧路径产物。
    let old_slug = existing.as_ref().map(|content| content.slug.as_str());
    if let Err(error) = state.build_ctl.build_page(&content.slug, old_slug).await {
        return Err(ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("内容已保存，但自动编译失败：{error}"),
        ));
    }
    Ok(Json(json!({ "item": content })))
}

pub(crate) async fn delete_content(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    current_user(&state, &session).await?;
    let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM contents WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    if slug.as_deref() == Some(service::HOME_SLUG) {
        api_reject!(StatusCode::FORBIDDEN, "网站主页不能删除，只能编辑");
    }
    sqlx::query("DELETE FROM contents WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if let Some(slug) = slug {
        if let Err(error) = state.build_ctl.build_page(&slug, None).await {
            return Err(ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("内容已删除，但清理产物失败：{error}"),
            ));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 精简版历史：版本列表。data 不下发，避免把大正文拖出来。
pub(crate) async fn content_revisions(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let rev_seq: i32 = sqlx::query_scalar("SELECT rev_seq FROM contents WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::with_status(StatusCode::NOT_FOUND, "内容不存在"))?;
    let items: Vec<serde_json::Value> = crate::history::list_revisions(&state.pool, id)
        .await?
        .into_iter()
        .map(|revision| {
            let mut value = json!(revision);
            if let Some(object) = value.as_object_mut() {
                object.insert("is_current".to_string(), json!(revision.seq == rev_seq));
            }
            value
        })
        .collect();
    Ok(Json(json!({ "items": items, "current_seq": rev_seq })))
}

/// 单个版本的完整正文（重建增量后返回），用于版本预览与回滚确认。
pub(crate) async fn content_revision_body(
    State(state): State<AppState>,
    session: Session,
    Path((id, seq)): Path<(Uuid, i32)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let (format, body) = crate::history::read_revision(&state.pool, id, seq)
        .await
        .map_err(|error| ApiError::with_status(StatusCode::NOT_FOUND, format!("{error}")))?;
    Ok(Json(json!({ "seq": seq, "format": format, "body": body })))
}

#[derive(Deserialize)]
pub(crate) struct RevisionDiffQuery {
    /// 缺省与当前指针版本比较。
    #[serde(default)]
    against: Option<i32>,
}

/// 两个版本间的 unified diff（源文本行级比较）。
pub(crate) async fn content_revision_diff(
    State(state): State<AppState>,
    session: Session,
    Path((id, seq)): Path<(Uuid, i32)>,
    Query(query): Query<RevisionDiffQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let diff = crate::history::diff_revisions(&state.pool, id, seq, query.against)
        .await
        .map_err(|error| ApiError::with_status(StatusCode::NOT_FOUND, format!("{error}")))?;
    Ok(Json(json!({ "diff": diff })))
}

/// 回滚：直接移动指针到目标版本（不产生新版本），并重建该页静态产物。
pub(crate) async fn content_revision_revert(
    State(state): State<AppState>,
    session: Session,
    Path((id, seq)): Path<(Uuid, i32)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let (content, old_slug) = crate::history::revert(&state.pool, id, seq)
        .await
        .map_err(|error| ApiError::with_status(StatusCode::NOT_FOUND, format!("{error}")))?;
    // 回滚恢复了旧路径时，旧路径的产物要一并删除。
    let remove = (old_slug != content.slug).then_some(old_slug.as_str());
    if let Err(error) = state.build_ctl.build_page(&content.slug, remove).await {
        return Err(ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("已回滚，但自动编译失败：{error}"),
        ));
    }
    Ok(Json(json!({ "item": content })))
}

/// 校验并规整页面自定义 <head> 内容。
///
/// 这段内容是管理员手写的原始 HTML，原样注入模板，因此只需挡住会破坏
/// 文档结构的情况：出现 `</head>` 闭合标签、或整体长度失控。
fn validate_page_head(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.to_ascii_lowercase().contains("</head") {
        anyhow::bail!("自定义 head 里不能出现 </head> 闭合标签");
    }
    // 32 KiB 足够放 meta / link / script，再大基本是误操作。
    const MAX_HEAD_BYTES: usize = 32 * 1024;
    if trimmed.len() > MAX_HEAD_BYTES {
        anyhow::bail!("自定义 head 超过 {} KiB 上限", MAX_HEAD_BYTES / 1024);
    }
    Ok(trimmed.to_string())
}
