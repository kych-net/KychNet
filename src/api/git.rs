//! 从 `api.rs` 拆出的子模块。路由注册与公共类型见 `super`（`api/mod.rs`）。
use super::*;

#[derive(Deserialize)]
pub(crate) struct GitTokenBody {
    name: String,
}

pub(crate) async fn git_token_list(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    Ok(Json(json!({
        "items": crate::git::token::list(&state.pool, user.id).await?
    })))
}

pub(crate) async fn git_token_create(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<GitTokenBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = current_user(&state, &session).await?;
    let (item, token) = crate::git::token::create(&state.pool, user.id, &body.name).await?;
    Ok(Json(json!({ "item": item, "token": token })))
}

pub(crate) async fn git_token_revoke(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let user = current_user(&state, &session).await?;
    crate::git::token::revoke(&state.pool, user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn git_plain(status: StatusCode, message: &str) -> Response {
    (status, message.to_string()).into_response()
}

pub(crate) fn git_unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"kych git\"")],
        "需要 Git 访问令牌",
    )
        .into_response()
}

pub(crate) fn git_binary(content_type: &'static str, body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

pub(crate) fn git_json(content_type: &'static str, body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

pub(crate) async fn git_project(
    state: &AppState,
    slug: &str,
) -> std::result::Result<crate::project::Project, Response> {
    match crate::project::find_project_by_slug(&state.pool, slug).await {
        Ok(Some(project)) if project.archived_at.is_none() => Ok(project),
        Ok(_) => Err(git_plain(StatusCode::NOT_FOUND, "项目不存在")),
        Err(error) => Err(git_plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            &error.to_string(),
        )),
    }
}

/// `/git/` 写操作认证：Git 令牌优先，登录会话兜底。
///
/// 登录用户不必另建 Git 令牌就能 push、上传 LFS、编辑文件树；匿名客户端
/// 仍必须带令牌。禁用账号两条路径都不放行。
pub(crate) async fn git_write_user(
    state: &AppState,
    headers: &HeaderMap,
    session: &Session,
) -> std::result::Result<Option<User>, String> {
    match crate::git::token::authenticate(&state.pool, headers).await {
        Ok(Some(user)) => return Ok(Some(user)),
        Ok(None) => {}
        Err(error) => return Err(error.to_string()),
    }
    let Ok(Some(id)) = session.get::<Uuid>("user_id").await else {
        return Ok(None);
    };
    match service::find_user_by_id(&state.pool, id).await {
        Ok(Some(user)) if !user.disabled => Ok(Some(user)),
        Ok(_) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// 把认证结果翻译成 handler 可直接返回的用户 / 响应。
/// `Response` 较大是 axum 固有，与既有 `git_project()` 同构。
#[allow(clippy::result_large_err)]
pub(crate) fn git_need_user(
    result: std::result::Result<Option<User>, String>,
) -> std::result::Result<User, Response> {
    match result {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(git_unauthorized()),
        Err(error) => Err(git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error)),
    }
}

/// 文件树写身份。Git 令牌 / 登录会话可写任意分支；runner token 只能写
/// 自己正在执行的那个 run 绑定的分支（`allowed_ref`）。
pub(crate) struct WorktreeWriteAuth {
    user: User,
    allowed_ref: Option<String>,
}

/// runner token 不是站内用户，用合成身份生成 commit。`id = Uuid::nil()` 是
/// 「系统身份」约定：`project::commit_snapshot()` 据此把 `git_commits.author_id`
/// 写成 NULL，避免指向不存在的 users 行；作者名记为 `kychrunner`。
pub(crate) fn runner_identity() -> User {
    User {
        id: Uuid::nil(),
        username: "kychrunner".to_string(),
        disabled: false,
        is_admin: false,
        created_at: Utc::now(),
    }
}

/// 文件树写认证：Git 令牌 → 登录会话 → runner token（带 run 绑定）。
///
/// smart HTTP 的 push / LFS 仍只走 `git_write_user()`，**不放给 runner token**：
/// runner 只需要写工作树文件，不需要 push 任意 ref 或上传 LFS 对象。
pub(crate) async fn worktree_write_auth(
    state: &AppState,
    headers: &HeaderMap,
    session: &Session,
    project_id: Uuid,
) -> std::result::Result<Option<WorktreeWriteAuth>, String> {
    if let Some(user) = git_write_user(state, headers, session).await? {
        return Ok(Some(WorktreeWriteAuth {
            user,
            allowed_ref: None,
        }));
    }
    match crate::workflow::runner_api::runner_write_scope(&state.pool, headers, project_id).await {
        Ok(Some(branch)) => Ok(Some(WorktreeWriteAuth {
            user: runner_identity(),
            allowed_ref: Some(branch),
        })),
        Ok(None) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// 文件树写要更新分支 ref，所以只接受分支名，不接受 commit OID。
#[allow(clippy::result_large_err)]
pub(crate) fn worktree_write_ref(params: &HashMap<String, String>) -> std::result::Result<&str, Response> {
    let value = params
        .get("ref")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| git_plain(StatusCode::BAD_REQUEST, "缺少 ref 参数（分支名）"))?;
    if !crate::project::valid_branch(value) {
        return Err(git_plain(StatusCode::BAD_REQUEST, "非法分支名"));
    }
    Ok(value)
}

#[allow(clippy::result_large_err)]
pub(crate) fn worktree_write_path(params: &HashMap<String, String>) -> std::result::Result<&str, Response> {
    let value = params
        .get("path")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| git_plain(StatusCode::BAD_REQUEST, "缺少 path 参数"))?;
    if !crate::project::valid_file_path(value) {
        return Err(git_plain(StatusCode::BAD_REQUEST, "非法文件路径"));
    }
    Ok(value)
}

/// runner 不能回写工作流定义目录。定义恒从默认分支 head 读取，但 runner
/// token 对绑定分支的任意路径有写权限；这里按项目实际 workflow_dir（含
/// 动态 `.站点名/工作流`）做纵深防御。解析失败时 fail-closed。
async fn runner_path_forbidden(
    state: &AppState,
    auth: &WorktreeWriteAuth,
    project: &crate::project::Project,
    path: &str,
) -> bool {
    if auth.allowed_ref.is_none() {
        return false;
    }
    let dir = match crate::workflow::resolve_workflow_dir(&state.pool, &project.workflow_dir).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, project = %project.slug, "解析工作流目录失败，拒绝 runner 回写");
            return true;
        }
    };
    let dir = dir.trim_matches('/');
    !dir.is_empty() && (path == dir || path.starts_with(&format!("{dir}/")))
}

pub(crate) async fn git_http_get(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    session: Session,
) -> Response {
    let (slug, action, channel) = match crate::git::http::split_git_path(&path) {
        Ok(value) => value,
        Err(error) => return git_plain(StatusCode::NOT_FOUND, &error.to_string()),
    };
    let project = match git_project(&state, &slug).await {
        Ok(project) => project,
        Err(response) => return response,
    };

    // 文件树通道：`/git/<项目>/{tree,file,archive,icon.svg}`，公开只读。
    // 调用方不需要 git 客户端、不需要 .git，也不必解析 packfile。
    if channel == crate::git::http::GitChannel::Worktree {
        return worktree_get(&state, &project, &action, &params).await;
    }

    // Git LFS 下载：公开可读，文件落在 LFS_ROOT 下。
    if let Some(oid) = action.strip_prefix("info/lfs/objects/") {
        return match crate::git::lfs::load(&state.pool, &state.lfs_root, project.id, oid).await {
            Ok(Some(data)) => git_binary("application/octet-stream", data),
            Ok(None) => git_plain(StatusCode::NOT_FOUND, "LFS 对象不存在"),
            Err(error) => git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        };
    }

    if action != "info/refs" {
        return git_plain(StatusCode::NOT_FOUND, "未知的 Git 路径");
    }
    let service = params.get("service").cloned().unwrap_or_default();
    if service != "git-upload-pack" && service != "git-receive-pack" {
        return git_plain(StatusCode::BAD_REQUEST, "缺少或非法的 service 参数");
    }
    // receive-pack 的广告也要求认证，Git 客户端才会主动带上凭据。
    // 登录会话也算数：浏览器里登录过的用户不必另建 Git 令牌。
    if service == "git-receive-pack" {
        match git_need_user(git_write_user(&state, &headers, &session).await) {
            Ok(_) => {}
            Err(response) => return response,
        }
    }

    // protocol v2 使用独立的能力广告；receive-pack 仍走 v0。
    if service == "git-upload-pack" && wants_v2(&headers) {
        return git_binary(
            "application/x-git-upload-pack-advertisement",
            crate::git::http::advertise_v2(&service),
        );
    }

    match crate::git::http::advertise(&state.pool, &project, &service).await {
        Ok(body) => git_binary(
            if service == "git-upload-pack" {
                "application/x-git-upload-pack-advertisement"
            } else {
                "application/x-git-receive-pack-advertisement"
            },
            body,
        ),
        Err(error) => git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

/// `/git/<项目>/{tree,file,archive,icon.svg}`（不带 `.git` 后缀）。
///
/// 与 `git-upload-pack` 保持同等可见性：站内仓库内容本来就能匿名 fetch，
/// 这里只是把 packfile 换成展开后的文件树，不额外放大暴露面。
/// 写操作不在这里，走 `git_http_put` / `git_http_delete`：Git 令牌或登录会话
/// 可写任意分支，runner token 只能写本次 run 绑定的分支。
///
/// `/git/<项目>/icon.svg` 是项目级图标：可直接作为仓库头像 URL，
/// 可带 `?ref=` 指定分支，缺省用默认分支。没有自定义图标时返回按项目名
/// 生成的字母图标，不会回退到站点默认图标。
pub(crate) async fn project_icon_response(
    state: &AppState,
    project: &crate::project::Project,
    params: &HashMap<String, String>,
) -> Response {
    let generated = || {
        git_binary(
            "image/svg+xml",
            crate::project::project_icon_svg(&project.slug).into_bytes(),
        )
    };
    let ref_name = params
        .get("ref")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(&project.default_branch);
    let commit = match crate::git::worktree::resolve_ref(&state.pool, project.id, ref_name).await {
        Ok(Some(oid)) => oid,
        _ => return generated(),
    };
    for name in crate::project::PROJECT_ICON_FILES {
        if let Ok(Some((_, data, _))) = crate::git::worktree::read(
            &state.pool,
            project.id,
            commit,
            name,
            &state.lfs_root,
        )
        .await
        {
            return git_binary("image/svg+xml", data);
        }
    }
    generated()
}

pub(crate) async fn worktree_get(
    state: &AppState,
    project: &crate::project::Project,
    action: &str,
    params: &HashMap<String, String>,
) -> Response {
    // 图标通道不要求 ref，独立处理。
    if action == "icon.svg" {
        return project_icon_response(state, project, params).await;
    }
    let Some(ref_name) = params
        .get("ref")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
    else {
        return git_plain(
            StatusCode::BAD_REQUEST,
            "缺少 ref 参数（分支名或 40 位 commit OID）",
        );
    };
    let commit = match crate::git::worktree::resolve_ref(&state.pool, project.id, ref_name).await {
        Ok(Some(oid)) => oid,
        Ok(None) => return git_plain(StatusCode::NOT_FOUND, "ref 不存在"),
        Err(error) => return git_plain(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let path = params.get("path").map(String::as_str).unwrap_or("");

    match action {
        "tree" => match crate::git::worktree::list(&state.pool, project.id, commit, path).await {
            Ok(items) => match serde_json::to_vec(&serde_json::json!({
                "commit": commit.to_hex(),
                "path": path,
                "items": items,
            })) {
                Ok(body) => git_json("application/json", body),
                Err(error) => git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
            },
            Err(error) => git_plain(StatusCode::BAD_REQUEST, &error.to_string()),
        },
        "file" => {
            if path.is_empty() {
                return git_plain(StatusCode::BAD_REQUEST, "缺少 path 参数");
            }
            match crate::git::worktree::read(
                &state.pool,
                project.id,
                commit,
                path,
                &state.lfs_root,
            )
            .await
            {
                Ok(Some((oid, data, from_lfs))) => {
                    let mut resp = git_binary("application/octet-stream", data);
                    let h = resp.headers_mut();
                    if let Ok(value) = HeaderValue::from_str(&oid.to_hex()) {
                        h.insert("x-git-blob-oid", value);
                    }
                    if from_lfs {
                        h.insert("x-git-lfs", HeaderValue::from_static("1"));
                    }
                    resp
                }
                Ok(None) => git_plain(StatusCode::NOT_FOUND, "文件不存在"),
                Err(error) => git_plain(StatusCode::BAD_REQUEST, &error.to_string()),
            }
        }
        "archive" => {
            // 许可覆盖预检、读工作树与压缩全过程；拿到许可的请求才允许
            // 占用 PG 连接和阻塞线程池，其余请求在信号量为零时排队。
            let _permit = match state.archive_slots.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    return git_plain(StatusCode::SERVICE_UNAVAILABLE, "归档服务不可用")
                }
            };
            if let Err(error) = crate::git::worktree::ensure_within_limit(
                &state.pool,
                project.id,
                commit,
                crate::git::worktree::MAX_WORKTREE_BYTES,
            )
            .await
            {
                return git_plain(StatusCode::PAYLOAD_TOO_LARGE, &error.to_string());
            }
            match crate::git::worktree::collect(
                &state.pool,
                project.id,
                commit,
                &state.lfs_root,
            )
            .await
            {
                Ok(files) => {
                    let bytes = crate::git::worktree::total_bytes(&files);
                    if bytes > crate::git::worktree::MAX_WORKTREE_BYTES {
                        return git_plain(StatusCode::PAYLOAD_TOO_LARGE, "工作树超过字节上限");
                    }
                    let count = files.len();
                    // tar 内直接是仓库相对路径，不带 code/ 前缀；打包实现与
                    // runner 快照共用一份，避免主服务出现第二份 USTAR 代码。
                    let entries: Vec<(String, Vec<u8>)> = files.into_iter().collect();
                    // gzip 压缩是同步 CPU 密集操作：放到阻塞线程池，避免
                    // 大仓库归档把 async worker 卡住导致其它请求排队超时。
                    let gz = match tokio::task::spawn_blocking(move || {
                        crate::workflow::snapshot::write_tar_gz(&entries)
                    })
                    .await
                    {
                        Ok(Ok(value)) => value,
                        Ok(Err(error)) => {
                            return git_plain(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                &error.to_string(),
                            )
                        }
                        Err(error) => {
                            return git_plain(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                &error.to_string(),
                            )
                        }
                    };
                    let mut resp = git_binary("application/gzip", gz);
                    let h = resp.headers_mut();
                    if let Ok(value) = HeaderValue::from_str(&commit.to_hex()) {
                        h.insert("x-git-commit", value);
                    }
                    if let Ok(value) = HeaderValue::from_str(&count.to_string()) {
                        h.insert("x-git-files", value);
                    }
                    if let Ok(value) = HeaderValue::from_str(&bytes.to_string()) {
                        h.insert("x-git-bytes", value);
                    }
                    resp
                }
                Err(error) => git_plain(StatusCode::BAD_REQUEST, &error.to_string()),
            }
        }
        _ => git_plain(StatusCode::NOT_FOUND, "未知的工作树路径"),
    }
}

pub(crate) fn wants_v2(headers: &HeaderMap) -> bool {
    headers
        .get("git-protocol")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(':')
                .any(|part| part.trim() == "version=2")
        })
        .unwrap_or(false)
}

/// 生成 LFS 批量接口里可访问的绝对地址。
pub(crate) fn lfs_base_url(state: &AppState, project: &crate::project::Project, headers: &HeaderMap) -> String {
    let scheme = if state.trust_proxy_headers {
        headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_else(|| {
                if state.site_url.starts_with("https") {
                    "https"
                } else {
                    "http"
                }
            })
    } else if state.site_url.starts_with("https") {
        "https"
    } else {
        "http"
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    format!(
        "{scheme}://{host}/git/{}.git/info/lfs/objects",
        percent_encode_segment(&project.slug)
    )
}

/// 只编码 URL 路径段里必须编码的字节。
pub(crate) fn percent_encode_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub(crate) async fn git_http_post(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
    session: Session,
    body: Bytes,
) -> Response {
    let (slug, action, channel) = match crate::git::http::split_git_path(&path) {
        Ok(value) => value,
        Err(error) => return git_plain(StatusCode::NOT_FOUND, &error.to_string()),
    };
    if channel == crate::git::http::GitChannel::Worktree {
        return git_plain(
            StatusCode::METHOD_NOT_ALLOWED,
            "文件树写请用 PUT / DELETE /git/<项目>/file",
        );
    }
    let project = match git_project(&state, &slug).await {
        Ok(project) => project,
        Err(response) => return response,
    };
    match action.as_str() {
        "info/lfs/objects/batch" => {
            let authenticated = git_write_user(&state, &headers, &session)
                .await
                .map(|user| user.is_some())
                .unwrap_or(false);
            let base = lfs_base_url(&state, &project, &headers);
            match crate::git::lfs::batch(
                &state.pool,
                &state.lfs_root,
                &project,
                &body,
                &base,
                authenticated,
            )
            .await
            {
                Ok(result) if result.unauthorized => {
                    let mut response = git_plain(
                        StatusCode::UNAUTHORIZED,
                        "上传 LFS 对象需要登录会话或 Git 令牌",
                    );
                    response.headers_mut().insert(
                        "lfs-authenticate",
                        HeaderValue::from_static("Basic realm=\"Git LFS\""),
                    );
                    response
                }
                Ok(result) => match serde_json::to_vec(&result.body) {
                    Ok(data) => git_json("application/vnd.git-lfs+json", data),
                    Err(error) => {
                        git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
                    }
                },
                Err(error) => git_plain(StatusCode::BAD_REQUEST, &error.to_string()),
            }
        }
        "git-upload-pack" => {
            let body = if wants_v2(&headers) {
                crate::git::http::upload_pack_v2(&state.pool, &project, &body).await
            } else {
                crate::git::http::upload_pack(&state.pool, &project, &body).await
            };
            match body {
                Ok(data) => git_binary("application/x-git-upload-pack-result", data),
                Err(error)
                    if error
                        .downcast_ref::<crate::git::proto::MalformedRequest>()
                        .is_some() =>
                {
                    git_plain(StatusCode::BAD_REQUEST, &error.to_string())
                }
                // 协商进入 packfile 阶段后客户端只认 pkt-line；HTTP 500 会变成
                // 「expected packfile to be sent after ready」这类谜语。
                Err(error) => git_binary(
                    "application/x-git-upload-pack-result",
                    git_error_pkt(&error.to_string()),
                ),
            }
        }
        "git-receive-pack" => git_receive(&state, &project, &headers, &session, &body).await,
        _ => git_plain(StatusCode::NOT_FOUND, "未知的 Git 路径"),
    }
}

/// PUT：`<slug>.git/info/lfs/objects/<oid>` 上传 LFS 对象；`<slug>/file`
/// 写文件树。两者都要求 Git 令牌或登录会话。
/// 把服务端错误包装成 pkt-line `ERR ...`，按 UTF-8 字符截断。
fn git_error_pkt(message: &str) -> Vec<u8> {
    let one_line = message.lines().collect::<Vec<_>>().join("; ");
    let count = one_line.chars().count();
    let mut truncated: String = one_line.chars().take(512).collect();
    if count > 512 {
        truncated.push('…');
    }
    let mut out = crate::git::proto::pkt_str(&format!("ERR {truncated}\n"));
    out.extend(crate::git::proto::flush());
    out
}

pub(crate) async fn git_http_put(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    session: Session,
    body: Bytes,
) -> Response {
    let (slug, action, channel) = match crate::git::http::split_git_path(&path) {
        Ok(value) => value,
        Err(error) => return git_plain(StatusCode::NOT_FOUND, &error.to_string()),
    };
    let project = match git_project(&state, &slug).await {
        Ok(project) => project,
        Err(response) => return response,
    };
    if channel == crate::git::http::GitChannel::Worktree {
        return worktree_put(&state, &project, &action, &params, &headers, &session, &body).await;
    }
    let Some(oid) = action.strip_prefix("info/lfs/objects/") else {
        return git_plain(StatusCode::NOT_FOUND, "未知的 Git 路径");
    };
    match git_need_user(git_write_user(&state, &headers, &session).await) {
        Ok(_) => {}
        Err(response) => return response,
    }
    match crate::git::lfs::store(&state.pool, &state.lfs_root, project.id, oid, &body).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => git_plain(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn git_receive(
    state: &AppState,
    project: &crate::project::Project,
    headers: &HeaderMap,
    session: &Session,
    body: &[u8],
) -> Response {
    let user = match git_need_user(git_write_user(state, headers, session).await) {
        Ok(user) => user,
        Err(response) => return response,
    };
    let outcome = match crate::git::http::receive_pack(&state.pool, project, &user, body).await {
        Ok(outcome) => outcome,
        // 畸形 pkt-line / 非法 OID 是客户端错误，不应记成 5xx。
        Err(error)
            if error
                .downcast_ref::<crate::git::proto::MalformedRequest>()
                .is_some() =>
        {
            return git_plain(StatusCode::BAD_REQUEST, &error.to_string())
        }
        Err(error) => {
            return git_binary(
                "application/x-git-receive-pack-result",
                git_error_pkt(&format!("推送被服务端拒绝：{error}")),
            )
        }
    };
    // 事件已在 receive_pack 的事务里入队，分发器负责触发内置 site.build；
    // push 响应不等构建，构建失败也不影响已提交的 refs（修掉原来的静默丢更新）。
    git_binary("application/x-git-receive-pack-result", outcome.response)
}

/// DELETE `/git/<项目>/file`：登录用户删除文件并生成一个新 commit。
pub(crate) async fn git_http_delete(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    session: Session,
) -> Response {
    let (slug, action, channel) = match crate::git::http::split_git_path(&path) {
        Ok(value) => value,
        Err(error) => return git_plain(StatusCode::NOT_FOUND, &error.to_string()),
    };
    if channel != crate::git::http::GitChannel::Worktree {
        return git_plain(
            StatusCode::NOT_FOUND,
            "smart HTTP 通道不支持 DELETE",
        );
    }
    let project = match git_project(&state, &slug).await {
        Ok(project) => project,
        Err(response) => return response,
    };
    worktree_delete(&state, &project, &action, &params, &headers, &session).await
}

/// PUT `/git/<项目>/file`：写文件并生成一个新 commit。
///
/// 请求体就是文件原始字节：UTF-8 文本直接进 blob，二进制走站内已实现的
/// LFS（对象落盘 + 仓库里存 pointer）。可选 `message` 与 `base_oid`（乐观锁）。
/// runner token 也能写，但只能写本次 run 绑定的分支，作者记为 `kychrunner`。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn worktree_put(
    state: &AppState,
    project: &crate::project::Project,
    action: &str,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
    session: &Session,
    body: &[u8],
) -> Response {
    if action != "file" {
        return git_plain(StatusCode::NOT_FOUND, "未知的文件树路径");
    }
    let auth = match worktree_write_auth(state, headers, session, project.id).await {
        Ok(Some(auth)) => auth,
        Ok(None) => return git_unauthorized(),
        Err(error) => return git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error),
    };
    let branch = match worktree_write_ref(params) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Some(allowed) = &auth.allowed_ref {
        if branch != allowed {
            return git_plain(
                StatusCode::FORBIDDEN,
                "runner 只能写本次 run 绑定的分支",
            );
        }
    }
    let path = match worktree_write_path(params) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if runner_path_forbidden(state, &auth, project, path).await {
        return git_plain(StatusCode::FORBIDDEN, "runner 不得回写工作流定义目录");
    }
    let message = params.get("message").map(String::as_str);
    let base_oid = params.get("base_oid").map(String::as_str);
    let result = match crate::project::write_file_bytes(
        &state.pool,
        &auth.user,
        project.id,
        branch,
        path,
        body,
        message,
        base_oid,
        &state.lfs_root,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return git_plain(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    if branch == project.default_branch {
        state.build_queue.enqueue();
    }
    match serde_json::to_vec(&serde_json::json!({ "item": result, "build": "queued" })) {
        Ok(data) => git_json("application/json", data),
        Err(error) => git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

/// 文件树删除：复用管理端 `project::delete_file`，语义完全一致。
/// runner token 同样只能删本次 run 绑定分支上的文件。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn worktree_delete(
    state: &AppState,
    project: &crate::project::Project,
    action: &str,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
    session: &Session,
) -> Response {
    if action != "file" {
        return git_plain(StatusCode::NOT_FOUND, "未知的文件树路径");
    }
    let auth = match worktree_write_auth(state, headers, session, project.id).await {
        Ok(Some(auth)) => auth,
        Ok(None) => return git_unauthorized(),
        Err(error) => return git_plain(StatusCode::INTERNAL_SERVER_ERROR, &error),
    };
    let branch = match worktree_write_ref(params) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Some(allowed) = &auth.allowed_ref {
        if branch != allowed {
            return git_plain(
                StatusCode::FORBIDDEN,
                "runner 只能写本次 run 绑定的分支",
            );
        }
    }
    let path = match worktree_write_path(params) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if runner_path_forbidden(state, &auth, project, path).await {
        return git_plain(StatusCode::FORBIDDEN, "runner 不得回写工作流定义目录");
    }
    let message = params.get("message").map(String::as_str);
    if let Err(error) = crate::project::delete_file(
        &state.pool,
        &auth.user,
        project.id,
        branch,
        path,
        message,
    )
    .await
    {
        return git_plain(StatusCode::BAD_REQUEST, &error.to_string());
    }
    if branch == project.default_branch {
        state.build_queue.enqueue();
    }
    StatusCode::NO_CONTENT.into_response()
}
