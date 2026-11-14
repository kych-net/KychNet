//! 从 `api.rs` 拆出的子模块。路由注册与公共类型见 `super`（`api/mod.rs`）。
use super::*;

#[derive(Deserialize)]
pub(crate) struct PrCreateBody {
    title: String,
    #[serde(default)]
    body: String,
    source_branch: String,
    target_branch: String,
}

#[derive(Deserialize)]
pub(crate) struct CommentBody {
    body: String,
}

#[derive(Deserialize)]
pub(crate) struct DiscussionCreateBody {
    title: String,
    body: String,
}



pub(crate) async fn pr_list(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(page): Query<PageQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    Ok(Json(json!({
        "items": crate::collab::list_prs(&state.pool, id, page.limit(), page.offset()).await?
    })))
}

pub(crate) async fn pr_create(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<PrCreateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let item = crate::collab::create_pr(
        &state.pool,
        &user,
        id,
        &body.title,
        &body.body,
        &body.source_branch,
        &body.target_branch,
    )
    .await?;
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn pr_detail(
    State(state): State<AppState>,
    session: Session,
    Path((id, number)): Path<(Uuid, i32)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let Some(item) = crate::collab::pr_detail(&state.pool, id, number).await? else {
        api_reject!(StatusCode::NOT_FOUND, "PR 不存在");
    };
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn pr_add_comment(
    State(state): State<AppState>,
    session: Session,
    Path((id, number)): Path<(Uuid, i32)>,
    Json(body): Json<CommentBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let item =
        crate::collab::add_pr_comment(&state.pool, &user, id, number, &body.body).await?;
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn pr_merge(
    State(state): State<AppState>,
    session: Session,
    Path((id, number)): Path<(Uuid, i32)>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_project(&state, id).await?;
    // 工作流检查门禁与事件入队都在 collab::merge_pr 的事务里完成；
    // 发布由后台队列执行，合并接口不再同步等待构建（避免 PR 已合并却返回 500）。
    let outcome = crate::collab::merge_pr(&state.pool, &user, id, number).await?;
    Ok(Json(json!({
        "item": outcome.pr,
        "overwrote_target": outcome.overwrote_target
    })))
}

pub(crate) async fn pr_close(
    State(state): State<AppState>,
    session: Session,
    Path((id, number)): Path<(Uuid, i32)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let item = crate::collab::close_pr(&state.pool, id, number).await?;
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn discussion_list(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(page): Query<PageQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    Ok(Json(json!({
        "items": crate::collab::list_discussions(&state.pool, id, page.limit(), page.offset())
            .await?
    })))
}

pub(crate) async fn discussion_create(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<DiscussionCreateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let item =
        crate::collab::create_discussion(&state.pool, &user, id, &body.title, &body.body).await?;
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn discussion_detail(
    State(state): State<AppState>,
    session: Session,
    Path((id, discussion_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let Some(item) = crate::collab::discussion_detail(&state.pool, id, discussion_id).await?
    else {
        api_reject!(StatusCode::NOT_FOUND, "讨论不存在");
    };
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn discussion_add_comment(
    State(state): State<AppState>,
    session: Session,
    Path((id, discussion_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CommentBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let item = crate::collab::add_discussion_comment(
        &state.pool,
        &user,
        id,
        discussion_id,
        &body.body,
    )
    .await?;
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn discussion_close(
    State(state): State<AppState>,
    session: Session,
    Path((id, discussion_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let item = crate::collab::close_discussion(&state.pool, id, discussion_id).await?;
    Ok(Json(json!({ "item": item })))
}
