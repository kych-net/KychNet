//! 从 `api.rs` 拆出的子模块。路由注册与公共类型见 `super`（`api/mod.rs`）。
use super::*;

#[derive(Deserialize)]
pub(crate) struct ProjectCreateBody {
    slug: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
pub(crate) struct ProjectUpdateBody {
    description: Option<String>,
    archived: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct ProjectListQuery {
    #[serde(default)]
    include_archived: bool,
}

#[derive(Deserialize)]
pub(crate) struct ProjectTreeQuery {
    #[serde(rename = "ref", default = "main_branch")]
    branch: String,
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
pub(crate) struct ProjectFileQuery {
    #[serde(rename = "ref", default = "main_branch")]
    branch: String,
    path: String,
}

#[derive(Deserialize)]
pub(crate) struct ProjectWriteBody {
    #[serde(rename = "ref", default = "main_branch")]
    branch: String,
    path: String,
    content: String,
    message: Option<String>,
    base_oid: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ProjectCommitChange {
    path: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    delete: bool,
}

#[derive(Deserialize)]
pub(crate) struct ProjectCommitBody {
    #[serde(rename = "ref", default = "main_branch")]
    branch: String,
    message: Option<String>,
    changes: Vec<ProjectCommitChange>,
}

#[derive(Deserialize)]
pub(crate) struct ProjectMoveBody {
    #[serde(rename = "ref", default = "main_branch")]
    branch: String,
    from: String,
    to: String,
    message: Option<String>,
}

pub(crate) fn main_branch() -> String {
    "main".to_string()
}

pub(crate) async fn project_list(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<ProjectListQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    Ok(Json(json!({
        "items": crate::project::list_projects(&state.pool, query.include_archived).await?
    })))
}

pub(crate) async fn project_create(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<ProjectCreateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let project = crate::project::create_project(
        &state.pool,
        &user,
        &body.slug,
        &body.description,
    )
    .await?;
    if let Err(error) = state.build_ctl.build_all().await {
        return Err(ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("项目已创建，但自动编译失败：{error}"),
        ));
    }
    Ok(Json(json!({ "item": project })))
}

pub(crate) async fn project_detail(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    let branches = crate::project::list_branches(&state.pool, id).await?;
    Ok(Json(json!({ "item": project, "branches": branches })))
}

pub(crate) async fn project_update(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<ProjectUpdateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let Some(existing) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    if body.archived == Some(true) {
        crate::project::archive_project(&state.pool, id).await?;
    } else if body.archived == Some(false) {
        crate::project::unarchive_project(&state.pool, id).await?;
    }
    let project = crate::project::update_project(
        &state.pool,
        id,
        body.description
            .as_deref()
            .unwrap_or(&existing.description),
    )
    .await?;
    if let Err(error) = state.build_ctl.build_all().await {
        return Err(ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("项目已更新，但自动编译失败：{error}"),
        ));
    }
    Ok(Json(json!({ "item": project })))
}

/// 永久删除已归档项目。要求管理员;删除前必须先归档,避免误删活跃仓库。
/// 数据库侧由 ON DELETE CASCADE 清空,磁盘侧移除 <LFS_ROOT>/<id>/ 整个目录。
pub(crate) async fn project_delete(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    current_admin(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    if project.archived_at.is_none() {
        api_reject!(StatusCode::BAD_REQUEST, "请先归档项目再删除");
    }
    crate::project::delete_project(&state.pool, id).await?;
    // LFS 磁盘对象按项目 id 分目录存放;删除失败不影响数据库已删除的事实,
    // 残留文件留待下次 GC 清理,故只记日志不回滚。
    let lfs_dir = state.lfs_root.join(id.to_string());
    match tokio::fs::remove_dir_all(&lfs_dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, "删除项目 LFS 目录失败: {}", lfs_dir.display()),
    }
    if let Err(error) = state.build_ctl.build_all().await {
        return Err(ApiError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("项目已删除，但自动编译失败：{error}"),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn project_tree(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(query): Query<ProjectTreeQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    let items = crate::project::list_dir(&state.pool, id, &query.branch, &query.path).await?;
    let branches = crate::project::list_branches(&state.pool, id).await?;
    Ok(Json(json!({
        "items": items,
        "branches": branches,
        "default_branch": project.default_branch
    })))
}

pub(crate) async fn project_file(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(query): Query<ProjectFileQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let item = crate::project::read_file(&state.pool, id, &query.branch, &query.path).await?;
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn project_write_file(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<ProjectWriteBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    let result = crate::project::write_file(
        &state.pool,
        &user,
        id,
        &body.branch,
        &body.path,
        &body.content,
        body.message.as_deref(),
        body.base_oid.as_deref(),
    )
    .await?;
    // 只有默认分支会被静态构建，PR 分支等合并后再发布。
    if body.branch == project.default_branch {
        if let Err(error) = state.build_ctl.build_all().await {
            return Err(ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("文件已保存，但自动编译失败：{error}"),
            ));
        }
    }
    Ok(Json(json!({ "item": result })))
}

pub(crate) async fn project_commit_changes(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<ProjectCommitBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    let changes: Vec<crate::project::FileChange> = body
        .changes
        .into_iter()
        .map(|change| crate::project::FileChange {
            path: change.path,
            content: change.content,
            delete: change.delete,
        })
        .collect();
    let result = crate::project::commit_changes(
        &state.pool,
        &user,
        id,
        &body.branch,
        body.message.as_deref(),
        &changes,
    )
    .await?;
    if body.branch == project.default_branch {
        if let Err(error) = state.build_ctl.build_all().await {
            return Err(ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("更改已提交，但自动编译失败：{error}"),
            ));
        }
    }
    Ok(Json(json!({ "item": result })))
}

pub(crate) async fn project_delete_file(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(query): Query<ProjectFileQuery>,
) -> ApiResult<StatusCode> {
    let user = current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    crate::project::delete_file(&state.pool, &user, id, &query.branch, &query.path, None).await?;
    if query.branch == project.default_branch {
        if let Err(error) = state.build_ctl.build_all().await {
            return Err(ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("文件已删除，但自动编译失败：{error}"),
            ));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn project_move_file(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<ProjectMoveBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    let result = crate::project::move_file(
        &state.pool,
        &user,
        id,
        &body.branch,
        &body.from,
        &body.to,
        body.message.as_deref(),
    )
    .await?;
    if body.branch == project.default_branch {
        if let Err(error) = state.build_ctl.build_all().await {
            return Err(ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("文件已移动，但自动编译失败：{error}"),
            ));
        }
    }
    Ok(Json(json!({ "item": result })))
}

#[derive(Deserialize)]
pub(crate) struct CommitListQuery {
    #[serde(rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct RevertBody {
    #[serde(rename = "ref", default = "main_branch")]
    branch: String,
    commit: String,
    message: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct BranchCreateBody {
    name: String,
    #[serde(default)]
    from: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct WorkflowToggleBody {
    path: String,
    enabled: bool,
}

#[derive(Deserialize)]
pub(crate) struct WorkflowDispatchBody {
    #[serde(default)]
    inputs: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(crate) struct RunnerCreateBody {
    name: String,
    #[serde(default)]
    labels: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct RunnerUpdateBody {
    enabled: bool,
}

pub(crate) async fn project_workflows(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let (workflow_dir, default_branch, items) =
        crate::workflow::management::list_for_project(&state.pool, id).await?;
    Ok(Json(json!({
        "workflow_dir": workflow_dir,
        "default_branch": default_branch,
        "items": items,
    })))
}

pub(crate) async fn project_workflows_update(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<WorkflowToggleBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let admin = current_admin(&state, &session).await?;
    crate::workflow::management::set_enabled(
        &state.pool,
        id,
        &body.path,
        body.enabled,
        admin.id,
    )
    .await?;
    Ok(Json(json!({ "path": body.path, "enabled": body.enabled })))
}

pub(crate) async fn project_workflow_dispatch(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<WorkflowDispatchBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let admin = current_admin(&state, &session).await?;
    crate::workflow::management::dispatch(
        &state.pool,
        id,
        admin.id,
        &admin.username,
        body.inputs.unwrap_or_else(|| json!({})),
    )
    .await?;
    Ok(Json(json!({ "queued": true })))
}

pub(crate) async fn project_runs(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let items = crate::workflow::management::runs(&state.pool, id).await?;
    Ok(Json(json!({ "items": items })))
}

pub(crate) async fn runner_list(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let items = crate::workflow::management::runners(&state.pool).await?;
    Ok(Json(json!({ "items": items })))
}

pub(crate) async fn runner_create(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<RunnerCreateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    let (id, code) = crate::workflow::management::create_registration_code(
        &state.pool,
        &body.name,
        body.labels,
    )
    .await?;
    Ok(Json(json!({ "runner_id": id, "registration_code": code })))
}

pub(crate) async fn runner_update(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<RunnerUpdateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    current_admin(&state, &session).await?;
    crate::workflow::management::set_runner_enabled(&state.pool, id, body.enabled).await?;
    Ok(Json(json!({ "id": id, "enabled": body.enabled })))
}

pub(crate) async fn project_commits(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Query(query): Query<CommitListQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let items = crate::project::list_commits(
        &state.pool,
        id,
        query.reference.as_deref(),
        query.limit.unwrap_or(50),
        query.offset.unwrap_or(0),
    )
    .await?;
    Ok(Json(json!({ "items": items })))
}

pub(crate) async fn project_commit_detail(
    State(state): State<AppState>,
    session: Session,
    Path((id, oid)): Path<(Uuid, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    let Some(item) = crate::project::commit_detail(&state.pool, id, &oid).await? else {
        api_reject!(StatusCode::NOT_FOUND, "提交不存在");
    };
    Ok(Json(json!({ "item": item })))
}

pub(crate) async fn project_revert(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<RevertBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    let result = crate::project::revert_commit(
        &state.pool,
        &user,
        id,
        &body.branch,
        &body.commit,
        body.message.as_deref(),
    )
    .await?;
    if body.branch == project.default_branch {
        if let Err(error) = state.build_ctl.build_all().await {
            return Err(ApiError::with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("已还原提交，但自动编译失败：{error}"),
            ));
        }
    }
    Ok(Json(json!({ "item": result })))
}

pub(crate) async fn project_branch_create(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
    Json(body): Json<BranchCreateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    let Some(project) = crate::project::get_project(&state.pool, id).await? else {
        api_reject!(StatusCode::NOT_FOUND, "项目不存在");
    };
    let from = body
        .from
        .as_deref()
        .unwrap_or(&project.default_branch);
    crate::project::create_branch(&state.pool, id, &body.name, from).await?;
    let branches = crate::project::list_branches(&state.pool, id).await?;
    Ok(Json(json!({ "branches": branches })))
}

pub(crate) async fn project_branch_delete(
    State(state): State<AppState>,
    session: Session,
    Path((id, name)): Path<(Uuid, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    current_user(&state, &session).await?;
    require_project(&state, id).await?;
    crate::project::delete_branch(&state.pool, id, &name).await?;
    let branches = crate::project::list_branches(&state.pool, id).await?;
    Ok(Json(json!({ "branches": branches })))
}
