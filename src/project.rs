use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, Row};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

use crate::git::lfs;
use crate::git::object::{
    commit_parents, encode_commit, encode_tag, ensure_text, identity_name, is_text, parse_commit,
    ObjectKind, Oid,
};
use similar::{DiffOp, TextDiff};
use crate::git::proto;
use crate::git::store;
use crate::git::tree;
use crate::models::{ContentFormat, User};

/// 项目在内容站的固定命名空间。
pub const PROJECT_NAMESPACE: &str = "项目";

/// 项目图标的候选文件名（按优先级），放在项目根目录即可被站点识别。
/// 平台把 `.svg` 也当作内容格式，因此构建时会对这些源文件跳过页面编译，
/// 只把内容原样输出成 `icon.svg`，不会被编译成内容页面。
pub const PROJECT_ICON_FILES: &[&str] = &["图标.svg", "favicon.svg", "logo.svg"];

/// 项目图标在内容站的固定输出名。
pub const PROJECT_ICON_ASSET: &str = "icon.svg";

/// 从项目默认分支的文件快照里挑出自定义图标（第一个存在的候选）。
pub fn project_icon_source<'a>(
    files: &'a BTreeMap<String, Vec<u8>>,
) -> Option<(&'a str, &'a Vec<u8>)> {
    for name in PROJECT_ICON_FILES {
        if let Some(data) = files.get(*name) {
            if is_text(data) && !lfs::is_pointer(data) {
                return Some((*name, data));
            }
        }
    }
    None
}

/// 没有自定义图标时按项目名生成一个确定性的字母图标。
/// 项目页面与 `/git/<项目>/icon.svg` 都用它，避免回退到站点默认蓝块图标。
pub fn project_icon_svg(slug: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    for byte in slug.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    const PALETTE: &[&str] = &[
        "#7c3aed", "#db2777", "#dc2626", "#ea580c", "#ca8a04", "#16a34a", "#0891b2", "#4f46e5",
        "#0d9488", "#be123c", "#4338ca", "#b45309",
    ];
    let color = PALETTE[(hash as usize) % PALETTE.len()];
    let escaped = match slug.chars().next().unwrap_or('?') {
        '&' => "&amp;".to_string(),
        '<' => "&lt;".to_string(),
        '>' => "&gt;".to_string(),
        '"' => "&quot;".to_string(),
        '\'' => "&apos;".to_string(),
        other => other.to_string(),
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">\
<rect width=\"64\" height=\"64\" rx=\"14\" fill=\"{color}\"/>\
<text x=\"32\" y=\"45\" text-anchor=\"middle\" font-family=\"system-ui, sans-serif\" font-size=\"34\" font-weight=\"700\" fill=\"#ffffff\">{escaped}</text>\
</svg>\n"
    )
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub slug: String,
    pub description: String,
    pub default_branch: String,
    #[serde(default)]
    pub workflow_dir: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileContent {
    pub path: String,
    pub oid: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteResult {
    pub commit: String,
    pub path: String,
    pub oid: String,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub content: Option<String>,
    pub delete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchWriteResult {
    pub commit: String,
    pub paths: Vec<String>,
}

/// 项目名允许任意非特殊 UTF-8：只排除路径分隔符、URL 保留字符和控制字符。
pub fn valid_project_slug(slug: &str) -> bool {
    let trimmed = slug.trim();
    if trimmed.is_empty() || trimmed.len() > 100 || trimmed != slug {
        return false;
    }
    if slug == "." || slug == ".." {
        return false;
    }
    !slug.chars().any(|ch| {
        ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '\\' | '?' | '#' | '%' | '\0')
    })
}

pub fn valid_branch(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.len() > 200
        || trimmed != name
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name == "@"
        || name
            .split('/')
            .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        return false;
    }
    !name.chars().any(|ch| {
        ch.is_control()
            || ch.is_whitespace()
            || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\0')
    })
}

/// 仓库内文件路径：相对路径、无空段、无 `..`、无 `.git` 段。
pub fn valid_file_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed != path || path.len() > 512 {
        return false;
    }
    if path.starts_with('/') || path.ends_with('/') {
        return false;
    }
    path.split('/').all(|segment| {
        !segment.is_empty()
            && segment.len() <= 200
            && segment != "."
            && segment != ".."
            && segment != ".git"
            && !segment
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '\\' | '?' | '#' | '%' | '\0'))
    })
}

fn output_stem(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => path,
    }
}

/// 项目文件在构建产物中的输出路径（相对项目根目录）。
/// 与 `build.rs` 的分类保持一致：页面类格式改写为 `<stem>.html`，
/// CSS / SVG 及未知扩展名按原路径作为静态资源输出。
fn project_output_path(path: &str) -> String {
    let ext = path
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or_default();
    match ContentFormat::from_extension(ext) {
        Some(
            ContentFormat::Markdown
            | ContentFormat::Html
            | ContentFormat::Typst
            | ContentFormat::Wikidot,
        ) => format!("{}.html", output_stem(path)),
        _ => path.to_string(),
    }
}

// 项目 Git 侧对文件名不做任何主页保护：项目里的 index.html / index.md /
// index.svg 都是普通文件，可以自由删除、改名。
// 「网站根目录只允许 index.html」的规则只作用于内容侧的 contents 表
// （见 service.rs::ensure_root_index_html_only），两套存储彼此独立。

/// 校验一批文件变更：路径合法、无重复、非删除变更必须是文本。
///
/// 这里刻意不检查任何「主页」文件名——项目 Git 侧不设限。
fn validate_changes(changes: &[FileChange]) -> Result<()> {
    if changes.is_empty() || changes.len() > 200 {
        bail!("一次提交需要 1-200 个文件变更");
    }
    let mut seen = std::collections::HashSet::new();
    for change in changes {
        if !valid_file_path(&change.path) {
            bail!("非法文件路径: {}", change.path);
        }
        if !seen.insert(change.path.as_str()) {
            bail!("重复的文件路径: {}", change.path);
        }
        if !change.delete {
            let content = change
                .content
                .as_deref()
                .context("非删除变更缺少 content")?;
            ensure_text(content.as_bytes())?;
        }
    }
    Ok(())
}

/// 构建后的输出路径冲突校验：
/// `a.md` 与 `a.typ` 都会输出成 `a.html`，完全相同必须拒绝；
/// `a.html` 与 `a.html/b.md` 是文件/目录冲突（写盘时 create_dir_all 会失败），
/// 同样拒绝——否则整站重建会在删旧产物之后的写盘阶段失败，留下半删的站点。
/// `a.typ` 与 `a.css` 分别输出 `a.html` / `a.css`，可以共存。
fn ensure_no_output_conflict(
    path: &str,
    files: &std::collections::BTreeSet<String>,
) -> Result<()> {
    let target = project_output_path(path);
    for existing in files {
        if existing == path {
            continue;
        }
        let other = project_output_path(existing);
        if other == target {
            bail!("{path} 与 {existing} 会输出到同一个路径，请先删除或改名其中一个");
        }
        if is_parent_path(&other, &target) || is_parent_path(&target, &other) {
            bail!("{path} 与 {existing} 的输出路径存在文件/目录冲突，请先删除或改名其中一个");
        }
    }
    Ok(())
}

/// parent 是否是 child 的父目录路径（按 `/` 分段判断，不是裸字符串前缀）。
fn is_parent_path(parent: &str, child: &str) -> bool {
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes()[parent.len()] == b'/'
}

fn identity(user: &User) -> String {
    format!("{} <noreply@kych.net>", identity_name(&user.username))
}

fn message_or(default: &str, message: Option<&str>) -> String {
    match message {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => default.to_string(),
    }
}

async fn lock_project(conn: &mut PgConnection, project_id: Uuid) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(project_id.to_string())
        .fetch_one(conn)
        .await?;
    Ok(())
}

async fn commit_snapshot(
    conn: &mut PgConnection,
    project_id: Uuid,
    user: &User,
    branch: &str,
    parent: Option<Oid>,
    tree_oid: Oid,
    message: &str,
) -> Result<Oid> {
    let now = Utc::now();
    let parents: Vec<Oid> = parent.into_iter().collect();
    let data = encode_commit(
        tree_oid,
        &parents,
        &identity(user),
        message,
        now.timestamp(),
    );
    let commit =
        store::put_object(conn, project_id, &crate::git::object::Object::commit(data)).await?;
    store::set_ref(conn, project_id, &format!("refs/heads/{branch}"), commit).await?;
    // `Uuid::nil()` 是「系统身份」（KychRunner）约定：runner 不是站内用户，
    // 作者名仍记 username（`kychrunner`），但 author_id 写 NULL，避免违反
    // git_commits.author_id → users(id) 的外键。
    store::insert_commit_index(
        conn,
        project_id,
        commit,
        parent,
        None,
        tree_oid,
        (!user.id.is_nil()).then_some(user.id),
        &user.username,
        message,
        now,
    )
    .await?;
    Ok(commit)
}

#[allow(clippy::too_many_arguments)]
async fn commit_files(
    conn: &mut PgConnection,
    project_id: Uuid,
    user: &User,
    branch: &str,
    parent: Option<Oid>,
    tree_oid: Oid,
    message: &str,
    path: &str,
    change: &str,
    old_path: Option<&str>,
    blob: Option<Oid>,
) -> Result<Oid> {
    let commit = commit_snapshot(conn, project_id, user, branch, parent, tree_oid, message).await?;
    store::insert_commit_path(conn, project_id, commit, path, change, old_path, blob).await?;
    Ok(commit)
}

/// 模板快照允许复制的对象数与总字节上限。模板是站内项目，正常规模远小于
/// 这个量级；上限只用来兜住异常大仓库，避免一次创建把事务拖到超时。
const TEMPLATE_MAX_OBJECTS: usize = 20_000;
const TEMPLATE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// 把源项目某棵 tree 下的全部对象复制到目标项目，返回复制的对象数。
///
/// 只走 tree/blob，不碰 commit：模板的提交历史属于模板自己，新项目只拿
/// 最新文件快照（浅克隆 depth=1 的语义）。
/// 模板快照里的 LFS 指针必须连同对象一起复制。pointer 本身只是文本 blob，
/// 真正内容在 `lfs_objects` 索引 + `LFS_ROOT` 里**按项目隔离**；只搬 tree/blob
/// 会让新项目 clone 后 `git lfs` 下载 404、checkout 直接失败。
fn lfs_pointers_in_snapshot(objects: &[crate::git::object::Object]) -> Vec<String> {
    use crate::git::object::ObjectKind;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for object in objects {
        if object.kind != ObjectKind::Blob || !lfs::is_pointer(&object.data) {
            continue;
        }
        if let Some(oid) = lfs::pointer_oid(&object.data) {
            if seen.insert(oid.clone()) {
                out.push(oid);
            }
        }
    }
    out
}

/// 同步部分：把源项目的 LFS 文件硬链接到目标项目路径。
/// 硬链接零拷贝；GC 按项目删除，一个项目删掉不会影响另一个项目的链接。
/// 跨文件系统时硬链接可能失败，退回字节复制。
/// 项目创建失败时清掉它的 LFS 目录。
///
/// 硬链接是文件系统操作，不受事务回滚约束；而 `GC` 只按 `lfs_objects`
/// 索引行找文件，没有索引行的目录它看不见。项目 id 是新生成的 UUID，
/// 整个目录只属于这个没落地的项目，直接删掉最干净。
fn remove_project_lfs_dir(root: &Path, project_id: Uuid) {
    let dir = root.join(project_id.to_string());
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn link_lfs_files(root: &Path, source: Uuid, target: Uuid, oids: &[String]) -> Result<()> {
    for oid in oids {
        let from = lfs::object_path(root, source, oid);
        let to = lfs::object_path(root, target, oid);
        if to.exists() {
            continue;
        }
        if !from.exists() {
            bail!("模板的 LFS 内容缺失，无法创建完整快照: {oid}");
        }
        let parent = to.parent().context("LFS 目标路径没有父目录")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建 LFS 目录: {}", parent.display()))?;
        if std::fs::hard_link(&from, &to).is_err() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("无法复制 LFS 文件: {}", from.display()))?;
        }
    }
    Ok(())
}

async fn copy_lfs_objects(
    conn: &mut PgConnection,
    root: &Path,
    source: Uuid,
    target: Uuid,
    oids: &[String],
) -> Result<()> {
    if oids.is_empty() {
        return Ok(());
    }
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT oid, size FROM lfs_objects WHERE project_id = $1 AND oid = ANY($2::text[])",
    )
    .bind(source)
    .bind(oids.to_vec())
    .fetch_all(&mut *conn)
    .await?;
    let sizes: HashMap<String, i64> = rows.into_iter().collect();
    // 索引行缺失说明源项目的 LFS 对象还没上传，此时生成快照只会得到一个
    // 永远拉不到内容的坏项目，不如直接拒绝。
    if sizes.len() != oids.len() {
        bail!("模板里有 LFS 对象尚未上传，无法创建完整快照");
    }
    let keys: Vec<String> = oids.to_vec();
    let values: Vec<i64> = keys.iter().map(|oid| sizes[oid]).collect();
    sqlx::query(
        "INSERT INTO lfs_objects (project_id, oid, size)
         SELECT $1, u.oid, u.size FROM unnest($2::text[], $3::bigint[]) AS u(oid, size)
         ON CONFLICT (project_id, oid) DO NOTHING",
    )
    .bind(target)
    .bind(keys)
    .bind(values)
    .execute(&mut *conn)
    .await?;

    let root = root.to_path_buf();
    let oids = oids.to_vec();
    tokio::task::spawn_blocking(move || link_lfs_files(&root, source, target, &oids))
        .await
        .context("复制模板 LFS 文件任务失败")??;
    Ok(())
}

async fn copy_tree_snapshot(
    conn: &mut PgConnection,
    lfs_root: &Path,
    source: Uuid,
    target: Uuid,
    tree_root: Oid,
) -> Result<usize> {
    let mut stack = vec![tree_root];
    let mut seen = HashSet::new();
    let mut objects = 0usize;
    let mut bytes = 0usize;
    let mut collected: Vec<crate::git::object::Object> = Vec::new();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let object = store::require_object(conn, source, oid).await?;
        objects += 1;
        bytes += object.data.len();
        if objects > TEMPLATE_MAX_OBJECTS || bytes > TEMPLATE_MAX_BYTES {
            bail!("模板项目内容过大，无法作为模板");
        }
        match object.kind {
            crate::git::object::ObjectKind::Tree => {
                let tree = crate::git::object::Tree::parse(&object.data)?;
                for entry in tree.entries.values() {
                    match entry {
                        crate::git::object::TreeEntry::Dir(child)
                        | crate::git::object::TreeEntry::Blob(child) => stack.push(*child),
                        // gitlink 的 OID 是外部仓库的提交，对象库中不存在；
                        // 条目本身会随 tree 原样复制，不展开遍历。
                        crate::git::object::TreeEntry::Gitlink(_) => {}
                    }
                }
            }
            crate::git::object::ObjectKind::Blob => ensure_text(&object.data)?,
            _ => bail!("模板快照里出现了非 tree/blob 对象"),
        }
        collected.push(object);
    }
    let lfs_oids = lfs_pointers_in_snapshot(&collected);
    // 压缩与 OID 计算放到 blocking 线程，写库收敛为一次批量 INSERT。
    let prepared = tokio::task::spawn_blocking(move || store::prepare_objects(&collected))
        .await
        .context("模板对象压缩任务失败")??;
    store::insert_prepared(conn, target, &prepared).await?;
    copy_lfs_objects(conn, lfs_root, source, target, &lfs_oids).await?;
    Ok(objects)
}

/// 创建项目。`template` 为站内模板项目 slug 时，新项目直接拿到模板默认
/// 分支的最新文件快照，并生成一条无父提交 —— 相当于对模板做了一次浅克隆：
/// 文件齐全、历史只有一条，不会引用模板里的提交对象。
pub async fn create_project(
    pool: &PgPool,
    lfs_root: &Path,
    user: &User,
    slug: &str,
    description: &str,
    template: Option<&str>,
) -> Result<Project> {
    if !valid_project_slug(slug) {
        bail!("项目名只能包含非特殊 UTF-8 字符，不能含 / \\ ? # % 或空白");
    }

    let mut tx = pool.begin().await?;

    // 模板必须在同一个事务里读：否则校验完到复制之间模板可能被删掉。
    let template = match template.map(str::trim).filter(|value| !value.is_empty()) {
        Some(name) => {
            let source = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE slug = $1")
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| anyhow::anyhow!("模板项目不存在: {name}"))?;
            if source.archived_at.is_some() {
                bail!("模板项目「{}」已归档，不能作为模板", source.slug);
            }
            let tip = store::resolve_branch(&mut *tx, source.id, &source.default_branch)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!("模板项目「{}」还没有任何提交，没有内容可复制", source.slug)
                })?;
            let root = tree::read_commit_tree(&mut *tx, source.id, tip).await?;
            Some((source, root))
        }
        None => None,
    };

    let pattern = format!("{PROJECT_NAMESPACE}/{slug}/%");
    let occupied: Option<(String,)> =
        sqlx::query_as("SELECT slug FROM contents WHERE slug LIKE $1 LIMIT 1")
            .bind(pattern)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((slug,)) = occupied {
        bail!("/{slug} 已被普通内容占用，项目无法使用这个路径");
    }

    let project = sqlx::query_as::<_, Project>(
        "INSERT INTO projects (id, slug, description, created_by, default_branch)
         VALUES ($1, $2, $3, $4, COALESCE($5, 'main'))
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(slug)
    .bind(description.trim())
    .bind(user.id)
    .bind(template.as_ref().map(|(source, _)| &source.default_branch))
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| match &error {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            anyhow::anyhow!("项目名已被占用")
        }
        _ => error.into(),
    })?;

    match template {
        Some((source, root)) => {
            let snapshot: Result<()> = match copy_tree_snapshot(
                &mut *tx,
                lfs_root,
                source.id,
                project.id,
                root,
            )
            .await
            {
                // 提交信息里带上模板名，方便回溯来源；parent 留空即浅克隆。
                Ok(_) => commit_snapshot(
                    &mut tx,
                    project.id,
                    user,
                    &project.default_branch,
                    None,
                    root,
                    &format!("从模板「{}」创建", source.slug),
                )
                .await
                .map(|_| ()),
                Err(error) => Err(error),
            };
            if let Err(error) = snapshot {
                // tx 随即 drop 回滚，但已经链接好的 LFS 文件要手工回收。
                remove_project_lfs_dir(lfs_root, project.id);
                return Err(error);
            }
        }
        None => {
            let mut files = BTreeMap::new();
            let index = format!("# {slug}\n\n{}\n", description.trim());
            files.insert("index.md".to_string(), index.into_bytes());
            let tree_oid = tree::build_from_files(&mut tx, project.id, &files).await?;
            commit_files(
                &mut tx,
                project.id,
                user,
                &project.default_branch,
                None,
                tree_oid,
                "初始化项目",
                "index.md",
                "add",
                None,
                files.get("index.md").map(|body| {
                    crate::git::object::hash_object(crate::git::object::ObjectKind::Blob, body)
                }),
            )
            .await?;
        }
    }

    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project.id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(project)
}

/// 导入时单个项目允许的最长提交链。与历史/PR 侧的 MAX_COMMIT_WALK 对齐。
const IMPORT_MAX_COMMITS: usize = 5000;

#[derive(Debug, Serialize)]
pub struct ImportOutcome {
    pub project: Project,
    pub branches: Vec<String>,
    pub objects: usize,
    pub commits: usize,
}

/// 从外部公开 Git 仓库克隆出一个站内项目。
///
/// 流程：refs 协商 → 全量 fetch pack → 解析与校验（纯内存，失败零副作用）
/// → 单事务写库（projects 行 + git_objects + git_refs + git_commits 索引）。
/// 校验规则与推送通道一致：仓库历史里不能有二进制 blob（站内 Git 只存
/// UTF-8 文本），提交数量与对象数量都有硬上限。
pub async fn import_project(
    pool: &PgPool,
    user: &User,
    slug: &str,
    description: &str,
    url: &str,
    branch: Option<&str>,
) -> Result<ImportOutcome> {
    if !valid_project_slug(slug) {
        bail!("项目名只能包含非特殊 UTF-8 字符，不能含 / \\ ? # % 或空白");
    }
    let url = crate::git::fetch::normalize_remote_url(url)?;

    // refs 协商：确定分支清单与默认分支，此时还没有写任何数据。
    let remote = crate::git::fetch::fetch_refs(url.clone()).await?;
    if remote.branches.is_empty() {
        bail!("远端仓库没有可导入的分支（空仓库或不是 Git 仓库）");
    }
    // want 行只能请求远端宣告过的能力，先把广告里的能力名带下来。
    let capabilities = remote.capabilities.clone();
    let mut branches = remote.branches;
    if let Some(wanted) = branch.map(str::trim).filter(|value| !value.is_empty()) {
        branches.retain(|(name, _)| name == wanted);
        if branches.is_empty() {
            bail!("远端不存在分支: {wanted}");
        }
    }
    if branches.len() > crate::git::fetch::MAX_BRANCHES {
        bail!(
            "远端有 {} 个分支，一次最多导入 {} 个；可指定 branch 只导入单个分支",
            branches.len(),
            crate::git::fetch::MAX_BRANCHES
        );
    }
    // 默认分支优先取远端 HEAD（symref 能力），其次远端第一个分支。
    let default_branch = branch
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or(remote.default_branch)
        .unwrap_or_else(|| branches[0].0.clone());
    if !valid_branch(&default_branch) {
        bail!("远端默认分支名不合法: {default_branch}");
    }

    // 全量拉 pack；refs 里指向的必须是分支顶点提交。
    let wants: Vec<Oid> = branches.iter().map(|(_, oid)| *oid).collect();
    let pack = crate::git::fetch::fetch_pack(url, wants, capabilities).await?;
    // 解析与 delta 还原是同步 CPU，放到 blocking 线程池，
    // 避免阻塞 tokio worker。
    let resolved = tokio::task::spawn_blocking(move || {
        let raw = proto::parse_pack(&pack)?;
        proto::resolve(&raw, &HashMap::new())
    })
    .await
    .context("导入 pack 解析任务失败")??;
    if resolved.is_empty() {
        bail!("远端没有返回任何 Git 对象");
    }
    if resolved.len() > crate::git::fetch::MAX_OBJECTS {
        bail!(
            "仓库共有 {} 个对象，超出导入上限 {} 个",
            resolved.len(),
            crate::git::fetch::MAX_OBJECTS
        );
    }
    // 文本校验提前到写库之前：任何一个二进制 blob 都会导入失败，
    // 此时事务还没开，不留半拉子数据。
    for (oid, kind, data) in &resolved {
        match kind {
            crate::git::object::ObjectKind::Blob => {
                if !is_text(data) {
                    bail!(
                        "仓库历史里包含二进制文件（blob {oid}），无法导入；站内 Git 只支持 UTF-8 文本"
                    );
                }
            }
            crate::git::object::ObjectKind::Commit | crate::git::object::ObjectKind::Tag => {
                ensure_text(data)?
            }
            crate::git::object::ObjectKind::Tree => {}
        }
    }

    // OID 计算与 zlib 压缩在事务外完成，事务里只做一次批量 INSERT。
    // 导入上限 10 万对象，逐条 INSERT 会让事务随对象数线性变长。
    let imported_objects = resolved.len();
    let objects: Vec<crate::git::object::Object> = resolved
        .into_iter()
        .map(|(_oid, kind, data)| crate::git::object::Object { kind, data })
        .collect();
    let prepared = tokio::task::spawn_blocking(move || store::prepare_objects(&objects))
        .await
        .context("导入对象压缩任务失败")??;

    let mut tx = pool.begin().await?;
    // 与 create_project 一致：项目名不能撞上普通内容的路径空间。
    let pattern = format!("{PROJECT_NAMESPACE}/{slug}/%");
    let occupied: Option<(String,)> =
        sqlx::query_as("SELECT slug FROM contents WHERE slug LIKE $1 LIMIT 1")
            .bind(pattern)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((taken,)) = occupied {
        bail!("/{taken} 已被普通内容占用，项目无法使用这个路径");
    }

    let project = sqlx::query_as::<_, Project>(
        "INSERT INTO projects (id, slug, description, default_branch, created_by)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(slug)
    .bind(description.trim())
    .bind(&default_branch)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| match &error {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            anyhow::anyhow!("项目名已被占用")
        }
        _ => error.into(),
    })?;

    // OID 由对象内容算出，与 pack 里的一致；同 OID 复用已有行。
    store::insert_prepared(&mut tx, project.id, &prepared).await?;
    for (name, target) in &branches {
        if !valid_branch(name) {
            continue;
        }
        store::set_ref(&mut tx, project.id, &format!("refs/heads/{name}"), *target).await?;
    }

    // 提交索引：从各分支顶点向上遍历整条父链。外部提交不属于站内用户，
    // author_id 记 NULL（与 KychRunner 的系统身份约定一致），作者名取
    // 原始提交里的 committer。
    let mut seen = HashSet::new();
    let mut stack: Vec<Oid> = branches.iter().map(|(_, oid)| *oid).collect();
    let mut commits = 0usize;
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if seen.len() > IMPORT_MAX_COMMITS {
            bail!("提交历史超过 {IMPORT_MAX_COMMITS} 个提交，超出导入上限");
        }
        let Some(object) = store::get_object(&mut tx, project.id, oid).await? else {
            bail!("远端 pack 缺少提交对象 {oid}");
        };
        if object.kind != crate::git::object::ObjectKind::Commit {
            continue;
        }
        let info = crate::git::object::parse_commit(&object.data)?;
        store::insert_commit_index(
            &mut tx,
            project.id,
            oid,
            info.parents.first().copied(),
            info.parents.get(1).copied(),
            info.tree,
            None,
            &info.author,
            &info.message,
            info.committed_at,
        )
        .await?;
        commits += 1;
        stack.extend(info.parents);
    }

    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project.id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    let imported_branches: Vec<String> = branches
        .into_iter()
        .filter(|(name, _)| valid_branch(name))
        .map(|(name, _)| name)
        .collect();
    Ok(ImportOutcome {
        project,
        branches: imported_branches,
        objects: imported_objects,
        commits,
    })
}

/// include_archived 为 true 时把已归档项目一并返回(管理台用于删除/取消归档);
/// 静态构建与 MCP 默认只看未归档项目。
pub async fn list_projects(pool: &PgPool, include_archived: bool) -> Result<Vec<Project>> {
    let sql = if include_archived {
        "SELECT * FROM projects ORDER BY archived_at IS NOT NULL, updated_at DESC"
    } else {
        "SELECT * FROM projects WHERE archived_at IS NULL ORDER BY updated_at DESC"
    };
    Ok(sqlx::query_as::<_, Project>(sql).fetch_all(pool).await?)
}

pub async fn get_project(pool: &PgPool, id: Uuid) -> Result<Option<Project>> {
    Ok(
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn find_project_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Project>> {
    Ok(
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE slug = $1")
            .bind(slug)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn update_project(pool: &PgPool, id: Uuid, description: &str) -> Result<Project> {
    sqlx::query_as::<_, Project>(
        "UPDATE projects SET description = $2, updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(description.trim())
    .fetch_optional(pool)
    .await?
    .context("项目不存在")
}

pub async fn archive_project(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE projects SET archived_at = now(), updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 取消归档,让项目重新出现在列表、Git 通道与站点构建中。
pub async fn unarchive_project(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE projects SET archived_at = NULL, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 永久删除项目。projects 的所有关联表(对象、提交、refs、分支、PR、讨论、
/// LFS 索引等)都带 ON DELETE CASCADE,一个事务即可清空数据库侧数据。
/// 磁盘上的 LFS 文件由 API 层按 <LFS_ROOT>/<id>/ 整个目录移除。
pub async fn delete_project(pool: &PgPool, id: Uuid) -> Result<()> {
    let mut tx = pool.begin().await?;
    let removed = sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        bail!("项目不存在");
    }
    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(())
}

pub async fn list_branches(pool: &PgPool, project_id: Uuid) -> Result<Vec<String>> {
    let mut conn = pool.acquire().await?;
    let refs = store::list_refs(&mut conn, project_id).await?;
    Ok(refs
        .into_iter()
        .filter_map(|(name, _)| name.strip_prefix("refs/heads/").map(str::to_string))
        .collect())
}

/// 只读入口统一的 ref 解析：分支名 / 标签名 / 40 位 commit OID 都接受。
///
/// 也兼容 `refs/heads/...`、`refs/tags/...` 全名；附注标签会逐层剥到
/// commit。写接口仍然只接受分支名（要更新 ref），不要调用这里。
pub async fn resolve_read_ref(
    conn: &mut PgConnection,
    project_id: Uuid,
    reference: &str,
) -> Result<Option<Oid>> {
    let value = reference.trim();
    if value.is_empty() {
        bail!("引用名不能为空");
    }
    if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let oid = Oid::from_hex(value)?;
        let Some(object) = store::get_object(conn, project_id, oid).await? else {
            return Ok(None);
        };
        return match object.kind {
            ObjectKind::Commit => Ok(Some(oid)),
            ObjectKind::Tag => Ok(Some(store::peel_to_commit(conn, project_id, oid).await?)),
            _ => bail!("目标不是提交对象: {value}"),
        };
    }
    // 显式写了完整 ref 时只查对应 namespace，避免 `refs/tags/v1` 与同名分支
    // `refs/heads/v1` 同时存在时误命中分支；短名才按 Git 的消歧顺序先 heads 后 tags。
    let (short, prefixes): (&str, &[&str]) = if let Some(rest) = value.strip_prefix("refs/heads/") {
        (rest, &["refs/heads/"])
    } else if let Some(rest) = value.strip_prefix("refs/tags/") {
        (rest, &["refs/tags/"])
    } else {
        (value, &["refs/heads/", "refs/tags/"])
    };
    if !valid_branch(short) {
        bail!("非法引用名: {reference:?}");
    }
    for prefix in prefixes {
        if let Some(oid) = store::get_ref(conn, project_id, &format!("{prefix}{short}")).await? {
            return Ok(Some(store::peel_to_commit(conn, project_id, oid).await?));
        }
    }
    Ok(None)
}

/// 标签列表项。`oid` 始终是标签最终指向的 commit（附注标签已剥开）。
#[derive(Debug, Clone, Serialize)]
pub struct TagInfo {
    pub name: String,
    pub oid: String,
    /// 附注标签的对象 OID；轻量标签与 ref 的 target 相同。
    pub tag_oid: Option<String>,
    pub message: String,
    pub author_name: String,
    pub created_at: DateTime<Utc>,
}

/// 列出项目标签。附注标签的 tagger / 说明从 tag 对象里读出来。
pub async fn list_tags(pool: &PgPool, project_id: Uuid) -> Result<Vec<TagInfo>> {
    let mut conn = pool.acquire().await?;
    // 直接取 updated_at：轻量标签没有 tagger 行，用 ref 的写入时间才稳定，
    // 否则每次列表都退化成「现在」，排序会随刷新跳动。
    let rows = sqlx::query(
        "SELECT name, target, updated_at FROM git_refs
         WHERE project_id = $1 AND name LIKE 'refs/tags/%' ORDER BY name",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut tags = Vec::new();
    for row in rows {
        let name: String = row.try_get("name")?;
        let raw: Vec<u8> = row.try_get("target")?;
        let target = Oid::from_slice(&raw)?;
        let ref_updated: DateTime<Utc> = row.try_get("updated_at")?;
        let Some(tag_name) = name.strip_prefix("refs/tags/") else {
            continue;
        };
        let object = store::get_object(&mut conn, project_id, target).await?;
        let (commit, tag_oid, message, author_name, created_at) = match object {
            Some(object) if object.kind == ObjectKind::Tag => {
                let text = String::from_utf8_lossy(&object.data);
                let mut msg = String::new();
                let mut tagger = String::new();
                let mut timestamp = 0i64;
                // tag 对象以第一个空行分隔 header 与 message；空行之后的内容
                // 一律算说明，避免说明里出现 "object " 之类的行被当 header 丢掉。
                let mut in_message = false;
                for line in text.lines() {
                    if !in_message {
                        if line.is_empty() {
                            in_message = true;
                            continue;
                        }
                        if let Some(value) = line.strip_prefix("tagger ") {
                            tagger = value.split('<').next().unwrap_or("").trim().to_string();
                            timestamp = value
                                .split_whitespace()
                                .rev()
                                .nth(1)
                                .and_then(|part| part.parse().ok())
                                .unwrap_or(0);
                        }
                        continue;
                    }
                    if !msg.is_empty() {
                        msg.push('\n');
                    }
                    msg.push_str(line);
                }
                let peeled = store::peel_to_commit(&mut conn, project_id, target).await?;
                let created_at = DateTime::from_timestamp(timestamp, 0).unwrap_or(ref_updated);
                (
                    peeled,
                    Some(target.to_hex()),
                    msg.trim().to_string(),
                    if tagger.is_empty() { "未知".to_string() } else { tagger },
                    created_at,
                )
            }
            _ => (
                target,
                None,
                String::new(),
                "轻量标签".to_string(),
                ref_updated,
            ),
        };
        tags.push(TagInfo {
            name: tag_name.to_string(),
            oid: commit.to_hex(),
            tag_oid,
            message,
            author_name,
            created_at,
        });
    }
    tags.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.name.cmp(&b.name)));
    Ok(tags)
}

/// 创建标签。`target_ref` 缺省为项目默认分支；给了 message 就写成附注标签。
pub async fn create_tag(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    name: &str,
    target_ref: Option<&str>,
    message: Option<&str>,
) -> Result<TagInfo> {
    if !valid_branch(name) {
        bail!("非法标签名");
    }
    let project = get_project(pool, project_id).await?.context("项目不存在")?;
    let reference = target_ref.unwrap_or(&project.default_branch);
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let full = format!("refs/tags/{name}");
    if store::get_ref(&mut tx, project_id, &full).await?.is_some() {
        bail!("标签已存在: {name}");
    }
    let commit = resolve_read_ref(&mut tx, project_id, reference)
        .await?
        .with_context(|| format!("引用不存在: {reference}"))?;
    let target = match message {
        Some(message) if !message.trim().is_empty() => {
            let data = encode_tag(
                commit,
                name,
                &identity(user),
                message,
                Utc::now().timestamp(),
            );
            store::put_object(
                &mut tx,
                project_id,
                &crate::git::object::Object { kind: ObjectKind::Tag, data },
            )
            .await?
        }
        _ => commit,
    };
    store::set_ref(&mut tx, project_id, &full, target).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "创建标签后作废项目缓存失败");
    }
    Ok(TagInfo {
        name: name.to_string(),
        oid: commit.to_hex(),
        tag_oid: (target != commit).then(|| target.to_hex()),
        message: message.unwrap_or("").trim().to_string(),
        author_name: user.username.clone(),
        created_at: Utc::now(),
    })
}

pub async fn delete_tag(pool: &PgPool, project_id: Uuid, name: &str) -> Result<()> {
    if !valid_branch(name) {
        bail!("非法标签名");
    }
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let full = format!("refs/tags/{name}");
    if store::get_ref(&mut tx, project_id, &full).await?.is_none() {
        bail!("标签不存在: {name}");
    }
    store::delete_ref(&mut tx, project_id, &full).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "删除标签后作废项目缓存失败");
    }
    Ok(())
}

/// 读取单个文件。`branch` 参数实际接受任意只读 ref（分支 / 标签 / commit OID）。
pub async fn read_file(
    pool: &PgPool,
    project_id: Uuid,
    branch: &str,
    path: &str,
) -> Result<Option<FileContent>> {
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    let mut conn = pool.acquire().await?;
    let Some(commit) = resolve_read_ref(&mut conn, project_id, branch).await? else {
        bail!("引用不存在: {branch}");
    };
    let Some((oid, data)) = tree::find_file(&mut conn, project_id, commit, path).await? else {
        return Ok(None);
    };
    if !crate::git::object::is_text(&data) {
        bail!("该文件不是 UTF-8 文本；二进制请用 GET /git/<项目>/file 读取（LFS 已支持）");
    }
    Ok(Some(FileContent {
        path: path.to_string(),
        oid: oid.to_hex(),
        content: String::from_utf8(data)?,
    }))
}

/// 列目录。`branch` 参数实际接受任意只读 ref（分支 / 标签 / commit OID）。
pub async fn list_dir(
    pool: &PgPool,
    project_id: Uuid,
    branch: &str,
    dir: &str,
) -> Result<Vec<tree::DirItem>> {
    if !dir.is_empty() && !valid_file_path(dir) {
        bail!("非法目录路径");
    }
    let mut conn = pool.acquire().await?;
    let Some(commit) = resolve_read_ref(&mut conn, project_id, branch).await? else {
        bail!("引用不存在: {branch}");
    };
    tree::list_dir(&mut conn, project_id, commit, dir).await
}

pub async fn write_file(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    path: &str,
    content: &str,
    message: Option<&str>,
    base_oid: Option<&str>,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    ensure_text(content.as_bytes())?;

    get_project(pool, project_id).await?.context("项目不存在")?;
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;

    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    // 只取目标文件的 OID 与全仓库的路径集合：既做 base_oid 校验又做输出
    // 冲突检查，但不再把每个 blob 都解压一遍（仓库大时这是主要开销）。
    let existing_oid = match parent {
        Some(commit) => tree::find_blob_oid(&mut tx, project_id, commit, path).await?,
        None => None,
    };
    store::check_old_oid(existing_oid, base_oid)?;
    let paths = match parent {
        Some(commit) => tree::list_paths(&mut tx, project_id, commit).await?,
        None => std::collections::BTreeSet::new(),
    };
    ensure_no_output_conflict(path, &paths)?;

    let blob = tree::write_blob(&mut tx, project_id, content.as_bytes()).await?;
    let root = match parent {
        Some(commit) => Some(tree::read_commit_tree(&mut tx, project_id, commit).await?),
        None => None,
    };
    let tree_oid = tree::update_path(&mut tx, project_id, root, path, Some(blob)).await?;
    let change = if existing_oid.is_some() {
        "modify"
    } else {
        "add"
    };
    let default_message = format!(
        "{} {path}",
        if existing_oid.is_some() {
            "更新"
        } else {
            "新增"
        }
    );
    let message = message_or(&default_message, message);
    let commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        parent,
        tree_oid,
        &message,
        path,
        change,
        None,
        Some(blob),
    )
    .await?;

    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(WriteResult {
        commit: commit.to_hex(),
        path: path.to_string(),
        oid: blob.to_hex(),
    })
}

/// 写文件树（原始字节）：UTF-8 文本直接进 blob，二进制走站内已实现的
/// LFS——对象落盘并登记，仓库里只存 pointer。与管理端 `write_file` 共用
/// 同一套项目锁、乐观锁、输出冲突校验与 commit 记录。
#[allow(clippy::too_many_arguments)]
pub async fn write_file_bytes(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    path: &str,
    data: &[u8],
    message: Option<&str>,
    base_oid: Option<&str>,
    lfs_root: &Path,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    get_project(pool, project_id).await?.context("项目不存在")?;

    // LFS 落盘在事务外完成：它自己要用 pool 取连接，避免和 lock_project
    // 持有的事务互相等连接。万一事务回滚，留下的孤儿对象由 gc 清理。
    let blob_bytes = if is_text(data) {
        data.to_vec()
    } else {
        let oid = hex::encode(Sha256::digest(data));
        lfs::store(pool, lfs_root, project_id, &oid, data).await?;
        format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
            data.len()
        )
        .into_bytes()
    };

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let existing_oid = match parent {
        Some(commit) => tree::find_blob_oid(&mut tx, project_id, commit, path).await?,
        None => None,
    };
    store::check_old_oid(existing_oid, base_oid)?;
    let paths = match parent {
        Some(commit) => tree::list_paths(&mut tx, project_id, commit).await?,
        None => std::collections::BTreeSet::new(),
    };
    ensure_no_output_conflict(path, &paths)?;

    let blob = tree::write_blob(&mut tx, project_id, &blob_bytes).await?;
    let root = match parent {
        Some(commit) => Some(tree::read_commit_tree(&mut tx, project_id, commit).await?),
        None => None,
    };
    let tree_oid = tree::update_path(&mut tx, project_id, root, path, Some(blob)).await?;
    let change = if existing_oid.is_some() {
        "modify"
    } else {
        "add"
    };
    let default_message = format!(
        "{} {path}",
        if existing_oid.is_some() {
            "更新"
        } else {
            "新增"
        }
    );
    let message = message_or(&default_message, message);
    let commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        parent,
        tree_oid,
        &message,
        path,
        change,
        None,
        Some(blob),
    )
    .await?;

    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(WriteResult {
        commit: commit.to_hex(),
        path: path.to_string(),
        oid: blob.to_hex(),
    })
}

/// 把多个文件变更合成一个项目级 commit。
pub async fn commit_changes(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    message: Option<&str>,
    changes: &[FileChange],
) -> Result<BatchWriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    validate_changes(changes)?;
    get_project(pool, project_id).await?.context("项目不存在")?;

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let mut files = match parent {
        Some(commit) => tree::read_files(&mut tx, project_id, commit).await?,
        None => BTreeMap::new(),
    };
    let mut root = match parent {
        Some(commit) => Some(tree::read_commit_tree(&mut tx, project_id, commit).await?),
        None => None,
    };
    let mut metadata: Vec<(String, String, Option<Oid>, Option<Oid>)> = Vec::new();

    // 先删除，再写入；这样同批里的删除不会和新增输出路径撞车。
    let ordered: Vec<&FileChange> = changes
        .iter()
        .filter(|change| change.delete)
        .chain(changes.iter().filter(|change| !change.delete))
        .collect();
    for change in ordered {
        if change.delete {
            let Some((old_oid, _)) = tree::find_file(
                &mut tx,
                project_id,
                parent.context("分支不存在")?,
                &change.path,
            )
            .await?
            else {
                bail!("文件不存在: {}", change.path);
            };
            root = Some(tree::update_path(&mut tx, project_id, root, &change.path, None).await?);
            files.remove(&change.path);
            metadata.push((
                change.path.clone(),
                "delete".to_string(),
                None,
                Some(old_oid),
            ));
        } else {
            let existing_paths: std::collections::BTreeSet<String> = files.keys().cloned().collect();
            ensure_no_output_conflict(&change.path, &existing_paths)?;
            let content = change.content.as_deref().unwrap_or_default();
            let blob = tree::write_blob(&mut tx, project_id, content.as_bytes()).await?;
            let existed = files.contains_key(&change.path);
            root =
                Some(tree::update_path(&mut tx, project_id, root, &change.path, Some(blob)).await?);
            files.insert(change.path.clone(), content.as_bytes().to_vec());
            metadata.push((
                change.path.clone(),
                if existed { "modify" } else { "add" }.to_string(),
                Some(blob),
                None,
            ));
        }
    }

    let tree_oid = root.context("分支不存在")?;
    let default_message = if metadata.len() == 1 {
        format!(
            "{} {}",
            match metadata[0].1.as_str() {
                "delete" => "删除",
                "add" => "新增",
                _ => "更新",
            },
            metadata[0].0
        )
    } else {
        format!("更新 {} 个文件", metadata.len())
    };
    let message = message_or(&default_message, message);
    let commit = commit_snapshot(
        &mut tx, project_id, user, branch, parent, tree_oid, &message,
    )
    .await?;
    for (path, change, blob, old_blob) in &metadata {
        store::insert_commit_path(
            &mut tx,
            project_id,
            commit,
            path,
            change,
            None,
            blob.or(*old_blob),
        )
        .await?;
    }
    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(BatchWriteResult {
        commit: commit.to_hex(),
        paths: metadata.into_iter().map(|(path, ..)| path).collect(),
    })
}

pub async fn delete_file(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    path: &str,
    message: Option<&str>,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let Some(commit) = parent else {
        bail!("分支不存在: {branch}");
    };
    let root = tree::read_commit_tree(&mut tx, project_id, commit).await?;
    let Some((old_oid, _)) = tree::find_file(&mut tx, project_id, commit, path).await? else {
        bail!("文件不存在: {path}");
    };
    let tree_oid = tree::update_path(&mut tx, project_id, Some(root), path, None).await?;
    let message = message_or(&format!("删除 {path}"), message);
    let new_commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        Some(commit),
        tree_oid,
        &message,
        path,
        "delete",
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(WriteResult {
        commit: new_commit.to_hex(),
        path: path.to_string(),
        oid: old_oid.to_hex(),
    })
}

pub async fn move_file(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    from: &str,
    to: &str,
    message: Option<&str>,
) -> Result<WriteResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    if !valid_file_path(from) || !valid_file_path(to) {
        bail!("非法文件路径");
    }
    if from == to {
        bail!("源路径与目标路径相同");
    }

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let parent = store::resolve_branch(&mut tx, project_id, branch).await?;
    let Some(commit) = parent else {
        bail!("分支不存在: {branch}");
    };
    let root = tree::read_commit_tree(&mut tx, project_id, commit).await?;
    let Some((blob, data)) = tree::find_file(&mut tx, project_id, commit, from).await? else {
        bail!("文件不存在: {from}");
    };
    let all_paths = tree::list_paths(&mut tx, project_id, commit).await?;
    if all_paths.contains(to) {
        bail!("目标路径已存在: {to}");
    }
    // 冲突检查要排除源文件本身：同 stem 换格式改名（a.md → a.typ）
    // 输出路径相同是改名的目的，不是冲突。
    let others: std::collections::BTreeSet<String> =
        all_paths.into_iter().filter(|key| key != from).collect();
    ensure_no_output_conflict(to, &others)?;
    ensure_text(&data)?;

    let mut current = Some(root);
    current = Some(tree::update_path(&mut tx, project_id, current, from, None).await?);
    current = Some(tree::update_path(&mut tx, project_id, current, to, Some(blob)).await?);
    let tree_oid = current.expect("至少执行过一次 update_path");
    let message = message_or(&format!("移动 {from} 到 {to}"), message);
    let new_commit = commit_files(
        &mut tx,
        project_id,
        user,
        branch,
        Some(commit),
        tree_oid,
        &message,
        to,
        "rename",
        Some(from),
        Some(blob),
    )
    .await?;
    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(WriteResult {
        commit: new_commit.to_hex(),
        path: to.to_string(),
        oid: blob.to_hex(),
    })
}

/// 提交历史列表项。OID 统一用十六进制文本返回，前端无需处理 bytea。
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CommitSummary {
    pub oid: String,
    pub parent_oid: Option<String>,
    pub second_parent_oid: Option<String>,
    pub tree_oid: String,
    pub author_name: String,
    pub message: String,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitDetail {
    #[serde(flatten)]
    pub commit: CommitSummary,
    pub files: Vec<crate::collab::FileDiff>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevertResult {
    pub commit: String,
}

const COMMIT_SELECT: &str = "SELECT encode(oid, 'hex') AS oid,
        encode(parent_oid, 'hex') AS parent_oid,
        encode(second_parent_oid, 'hex') AS second_parent_oid,
        encode(tree_oid, 'hex') AS tree_oid,
        author_name, message, committed_at
     FROM git_commits";

/// 从引用出发遍历全部父提交，用于按分支过滤历史。
async fn walk_commit_oids(conn: &mut PgConnection, project_id: Uuid, tip: Oid) -> Result<Vec<Oid>> {
    let mut seen = HashSet::new();
    let mut stack = vec![tip];
    let mut out = Vec::new();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if seen.len() > 5000 {
            bail!("提交历史过深，暂时无法列出");
        }
        let object = store::require_object(conn, project_id, oid).await?;
        if object.kind != crate::git::object::ObjectKind::Commit {
            continue;
        }
        out.push(oid);
        stack.extend(commit_parents(&object.data)?);
    }
    Ok(out)
}

pub async fn list_commits(
    pool: &PgPool,
    project_id: Uuid,
    reference: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<CommitSummary>> {
    let limit = limit.clamp(1, 200);
    let offset = offset.max(0);
    let mut conn = pool.acquire().await?;

    match reference {
        Some(reference) => {
            // 分支 / 标签 / commit OID 都能作为历史起点，方便按标签回溯版本。
            let Some(tip) = resolve_read_ref(&mut conn, project_id, reference).await? else {
                return Ok(Vec::new());
            };
            let oids = walk_commit_oids(&mut conn, project_id, tip).await?;
            let bytes: Vec<Vec<u8>> = oids.iter().map(|oid| oid.as_bytes().to_vec()).collect();
            let rows = sqlx::query_as::<_, CommitSummary>(&format!(
                "{COMMIT_SELECT} WHERE project_id = $1 AND oid = ANY($2)
                 ORDER BY committed_at DESC"
            ))
            .bind(project_id)
            .bind(&bytes)
            .fetch_all(&mut *conn)
            .await?;
            Ok(rows
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect())
        }
        None => Ok(sqlx::query_as::<_, CommitSummary>(&format!(
            "{COMMIT_SELECT} WHERE project_id = $1
             ORDER BY committed_at DESC LIMIT $2 OFFSET $3"
        ))
        .bind(project_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *conn)
        .await?),
    }
}

/// 提交详情：与第一个父提交做差异，根提交按空树处理。
pub async fn commit_detail(
    pool: &PgPool,
    project_id: Uuid,
    oid_hex: &str,
) -> Result<Option<CommitDetail>> {
    let oid = Oid::from_hex(oid_hex)?;
    let mut conn = pool.acquire().await?;
    let row = sqlx::query_as::<_, CommitSummary>(&format!(
        "{COMMIT_SELECT} WHERE project_id = $1 AND oid = $2"
    ))
    .bind(project_id)
    .bind(&oid.as_bytes()[..])
    .fetch_optional(&mut *conn)
    .await?;
    let Some(commit) = row else {
        return Ok(None);
    };
    let base = commit
        .parent_oid
        .as_deref()
        .map(Oid::from_hex)
        .transpose()?;
    let files = crate::collab::diff_commits(&mut conn, project_id, base, oid).await?;
    Ok(Some(CommitDetail { commit, files }))
}

/// 还原某个提交：把该提交相对父提交的改动反向应用到分支顶部。
pub async fn revert_commit(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    oid_hex: &str,
    message: Option<&str>,
) -> Result<RevertResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    let commit = Oid::from_hex(oid_hex)?;

    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;

    let tip = store::resolve_branch(&mut tx, project_id, branch)
        .await?
        .context("分支不存在")?;
    // 只有当前分支历史里的提交才允许还原。
    if crate::collab::merge_base(&mut tx, project_id, tip, commit).await? != Some(commit) {
        bail!("只能还原当前分支历史中的提交");
    }

    let object = store::require_object(&mut tx, project_id, commit).await?;
    if object.kind != crate::git::object::ObjectKind::Commit {
        bail!("目标不是提交对象");
    }
    let parent = commit_parents(&object.data)?.first().copied();
    let commit_files_map = tree::read_files(&mut tx, project_id, commit).await?;
    let parent_files = match parent {
        Some(parent) => tree::read_files(&mut tx, project_id, parent).await?,
        None => BTreeMap::new(),
    };
    let mut current = tree::read_files(&mut tx, project_id, tip).await?;

    let mut paths: BTreeSet<&String> = BTreeSet::new();
    paths.extend(commit_files_map.keys());
    paths.extend(parent_files.keys());

    let mut changes: Vec<(String, String, Option<Oid>)> = Vec::new();
    for path in paths {
        let before = commit_files_map.get(path);
        let after = parent_files.get(path);
        if before == after {
            continue;
        }
        match after {
            Some(data) => {
                let existed = current.contains_key(path);
                let blob = tree::write_blob(&mut tx, project_id, data).await?;
                current.insert(path.clone(), data.clone());
                changes.push((
                    path.clone(),
                    if existed { "modify" } else { "add" }.to_string(),
                    Some(blob),
                ));
            }
            None => {
                current.remove(path);
                changes.push((path.clone(), "delete".to_string(), None));
            }
        }
    }
    if changes.is_empty() {
        bail!("该提交没有可还原的改动");
    }
    let current_paths: std::collections::BTreeSet<String> = current.keys().cloned().collect();
    for path in current.keys() {
        ensure_no_output_conflict(path, &current_paths)?;
    }

    let root = tree::build_from_files(&mut tx, project_id, &current).await?;
    let short: String = oid_hex.chars().take(8).collect();
    let default_message = format!("还原提交 {short}");
    let message = message_or(&default_message, message);
    let new_commit =
        commit_snapshot(&mut tx, project_id, user, branch, Some(tip), root, &message).await?;
    for (path, change, blob) in &changes {
        store::insert_commit_path(&mut tx, project_id, new_commit, path, change, None, *blob)
            .await?;
    }
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(RevertResult {
        commit: new_commit.to_hex(),
    })
}

/// 从指定 ref（分支 / 标签 / commit OID）拉出新分支。
pub async fn create_branch(pool: &PgPool, project_id: Uuid, name: &str, from: &str) -> Result<()> {
    if !valid_branch(name) {
        bail!("非法分支名");
    }
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    if store::resolve_branch(&mut tx, project_id, name)
        .await?
        .is_some()
    {
        bail!("分支已存在: {name}");
    }
    let tip = resolve_read_ref(&mut tx, project_id, from)
        .await?
        .with_context(|| format!("来源引用不存在: {from}"))?;
    store::set_ref(&mut tx, project_id, &format!("refs/heads/{name}"), tip).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(())
}

pub async fn delete_branch(pool: &PgPool, project_id: Uuid, name: &str) -> Result<()> {
    if !valid_branch(name) {
        bail!("非法分支名");
    }
    let project = get_project(pool, project_id).await?.context("项目不存在")?;
    if name == project.default_branch {
        bail!("不能删除默认分支");
    }
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    if store::resolve_branch(&mut tx, project_id, name)
        .await?
        .is_none()
    {
        bail!("分支不存在: {name}");
    }
    store::delete_ref(&mut tx, project_id, &format!("refs/heads/{name}")).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // 提交后再失效一次：堵住「提交前失效 → 并发 fetch 用旧 ref 重建缓存」的窗口。
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "提交后作废项目缓存失败");
    }
    Ok(())
}

/// 分支对比结果：source 相对 target 领先的提交与文件差异。
#[derive(Debug, serde::Serialize)]
pub struct BranchCompare {
    pub merge_base: Option<String>,
    pub commits: Vec<CommitSummary>,
    pub files: Vec<crate::collab::FileDiff>,
}

/// 对比两个分支：返回 source 独有的提交（在 merge_base 即停）和两分支的
/// 文件差异。目标分支不存在或无差异时返回空集，不视为错误。
pub async fn compare_branches(
    pool: &PgPool,
    project_id: Uuid,
    source: &str,
    target: &str,
) -> Result<BranchCompare> {
    let mut conn = pool.acquire().await?;
    // 比较同样接受分支 / 标签 / commit OID：任意两个版本都能看差异。
    let Some(source_tip) = resolve_read_ref(&mut conn, project_id, source).await? else {
        bail!("来源引用不存在: {source}");
    };
    let Some(target_tip) = resolve_read_ref(&mut conn, project_id, target).await? else {
        bail!("目标引用不存在: {target}");
    };
    let base = crate::collab::merge_base(&mut conn, project_id, source_tip, target_tip).await?;
    // 领先提交：从 source tip 走父链，走到 merge_base 停。共同历史都在
    // base 的祖先里，不会重复遍历；5000 上限与 walk_commit_oids 一致。
    let mut seen = HashSet::new();
    let mut stack = vec![source_tip];
    let mut oids: Vec<Oid> = Vec::new();
    while let Some(oid) = stack.pop() {
        if Some(oid) == base || !seen.insert(oid) {
            continue;
        }
        if oids.len() > 5000 {
            bail!("提交历史过深，暂时无法比较");
        }
        let object = store::require_object(&mut conn, project_id, oid).await?;
        if object.kind != crate::git::object::ObjectKind::Commit {
            continue;
        }
        oids.push(oid);
        stack.extend(commit_parents(&object.data)?);
    }
    let bytes: Vec<Vec<u8>> = oids.iter().map(|oid| oid.as_bytes().to_vec()).collect();
    let commits = if bytes.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, CommitSummary>(&format!(
            "{COMMIT_SELECT} WHERE project_id = $1 AND oid = ANY($2)
             ORDER BY committed_at DESC"
        ))
        .bind(project_id)
        .bind(&bytes)
        .fetch_all(&mut *conn)
        .await?
    };
    let files = crate::collab::diff_commits(&mut conn, project_id, base, source_tip).await?;
    Ok(BranchCompare {
        merge_base: base.map(|oid| oid.to_hex()),
        commits,
        files,
    })
}

/// 分支重置结果。
#[derive(Debug, Clone, Serialize)]
pub struct ResetResult {
    pub commit: String,
    pub previous: String,
}

/// 把分支指针移动到指定提交（等价于无工作树的 `git reset --hard`）。
///
/// 站内没有 checkout 的概念，重置只是移动 ref：默认只允许退回当前分支历史
/// 里的提交，避免把分支指到无关历史；显式 `force` 才允许任意提交。
/// 默认分支被重置后由调用方触发整站重建。
pub async fn reset_branch(
    pool: &PgPool,
    user: &User,
    project_id: Uuid,
    branch: &str,
    target_ref: &str,
    force: bool,
) -> Result<ResetResult> {
    if !valid_branch(branch) {
        bail!("非法分支名");
    }
    let mut tx = pool.begin().await?;
    lock_project(&mut tx, project_id).await?;
    let tip = store::resolve_branch(&mut tx, project_id, branch)
        .await?
        .with_context(|| format!("分支不存在: {branch}"))?;
    let target = resolve_read_ref(&mut tx, project_id, target_ref)
        .await?
        .with_context(|| format!("引用不存在: {target_ref}"))?;
    if target == tip {
        return Ok(ResetResult {
            commit: target.to_hex(),
            previous: tip.to_hex(),
        });
    }
    if !force {
        let history = walk_commit_oids(&mut tx, project_id, tip).await?;
        if !history.contains(&target) {
            bail!("目标提交不在当前分支历史中；如确要重置到其它提交，请勾选强制重置");
        }
    }
    store::set_ref(&mut tx, project_id, &format!("refs/heads/{branch}"), target).await?;
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    crate::workflow::event::enqueue(
        &mut tx,
        &crate::workflow::event::WorkflowEvent {
            project_id,
            kind: "push".to_string(),
            ref_name: format!("refs/heads/{branch}"),
            before_oid: Some(tip.as_bytes().to_vec()),
            after_oid: Some(target.as_bytes().to_vec()),
            pr_number: None,
            actor_id: Some(user.id),
            payload: serde_json::json!({
                "action": "reset",
                "target": target_ref,
                "sender": { "login": user.username },
            }),
            dedupe_key: None,
        },
    )
    .await?;
    tx.commit().await?;
    if let Err(error) = store::invalidate_project_caches(project_id) {
        tracing::warn!(%error, "分支重置后作废项目缓存失败");
    }
    Ok(ResetResult {
        commit: target.to_hex(),
        previous: tip.to_hex(),
    })
}

/// 单文件提交历史的一项。`change` 为 add / modify / delete。
#[derive(Debug, Clone, Serialize)]
pub struct FileHistoryEntry {
    pub oid: String,
    pub author_name: String,
    pub message: String,
    pub committed_at: DateTime<Utc>,
    pub change: String,
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
}

/// 追踪某个路径的变更历史（沿第一父链，与 `git log --first-parent -- <path>` 同构）。
///
/// 用 tree 里的 blob OID 判断变化，不依赖 `git_commit_paths` 索引——外部导入
/// 的仓库与 git push 上来的提交都没有写那张表，只有 blob OID 是权威的。
pub async fn file_history(
    pool: &PgPool,
    project_id: Uuid,
    reference: &str,
    path: &str,
    limit: i64,
) -> Result<Vec<FileHistoryEntry>> {
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    let limit = limit.clamp(1, 200) as usize;
    let mut conn = pool.acquire().await?;
    let tip = resolve_read_ref(&mut conn, project_id, reference)
        .await?
        .with_context(|| format!("引用不存在: {reference}"))?;
    let mut current = Some(tip);
    let mut walked = 0usize;
    let mut out = Vec::new();
    while let Some(commit) = current {
        walked += 1;
        if walked > 5000 {
            break;
        }
        let object = store::require_object(&mut conn, project_id, commit).await?;
        if object.kind != ObjectKind::Commit {
            break;
        }
        let info = parse_commit(&object.data)?;
        let parent = info.parents.first().copied();
        let new_oid = tree::find_blob_oid(&mut conn, project_id, commit, path).await?;
        let old_oid = match parent {
            Some(parent) => tree::find_blob_oid(&mut conn, project_id, parent, path).await?,
            None => None,
        };
        if new_oid != old_oid {
            let change = match (old_oid, new_oid) {
                (None, Some(_)) => "add",
                (Some(_), None) => "delete",
                _ => "modify",
            };
            out.push(FileHistoryEntry {
                oid: commit.to_hex(),
                author_name: identity_name(&info.author),
                message: info.message.lines().next().unwrap_or("").to_string(),
                committed_at: info.committed_at,
                change: change.to_string(),
                old_oid: old_oid.map(|oid| oid.to_hex()),
                new_oid: new_oid.map(|oid| oid.to_hex()),
            });
            if out.len() >= limit {
                break;
            }
        }
        current = parent;
    }
    Ok(out)
}

/// blame 的一行：`commit` 是最后改动该行的提交 OID。
#[derive(Debug, Clone, Serialize)]
pub struct BlameLine {
    pub line: usize,
    pub commit: String,
    pub author_name: String,
    pub message: String,
    pub committed_at: DateTime<Utc>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlameResult {
    pub path: String,
    pub oid: String,
    pub truncated: bool,
    pub lines: Vec<BlameLine>,
}

const BLAME_MAX_BYTES: usize = 512 * 1024;
const BLAME_MAX_LINES: usize = 4000;
const BLAME_MAX_COMMITS: usize = 1000;

/// 保留行尾换行符切行，与 `similar` 的行切分保持一致。
fn blame_split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    }
}

async fn read_text_blob(
    conn: &mut PgConnection,
    project_id: Uuid,
    oid: Oid,
) -> Result<String> {
    let object = store::require_object(conn, project_id, oid).await?;
    if object.kind != ObjectKind::Blob || !is_text(&object.data) {
        bail!("历史里包含非文本版本，无法逐行追溯");
    }
    Ok(String::from_utf8_lossy(&object.data).into_owned())
}

/// 逐行追溯：从 ref 的文件内容出发，沿第一父链把每一行归到最后改动它的提交。
///
/// 站内没有工作树，所以不做 checkout：对相邻两个版本做行级 diff，把「当前
/// 行号」逐层映射回父版本；在本次提交里新出现（没有父版本对应行）的行就
/// 归属该提交，其余行继续向历史回溯。合并提交只跟第一父，与文件历史一致。
pub async fn blame(
    pool: &PgPool,
    project_id: Uuid,
    reference: &str,
    path: &str,
) -> Result<BlameResult> {
    if !valid_file_path(path) {
        bail!("非法文件路径");
    }
    let mut conn = pool.acquire().await?;
    let tip = resolve_read_ref(&mut conn, project_id, reference)
        .await?
        .with_context(|| format!("引用不存在: {reference}"))?;
    let (blob_oid, data) = tree::find_file(&mut conn, project_id, tip, path)
        .await?
        .with_context(|| format!("文件不存在: {path}"))?;
    if !is_text(&data) {
        bail!("二进制文件不支持逐行追溯");
    }
    let truncated_bytes = data.len() > BLAME_MAX_BYTES;
    let text = String::from_utf8_lossy(&data);
    let all_lines: Vec<&str> = blame_split_lines(&text);
    let truncated_lines = all_lines.len() > BLAME_MAX_LINES;
    let count = all_lines.len().min(BLAME_MAX_LINES);
    let display_lines: Vec<String> = all_lines[..count]
        .iter()
        .map(|line| line.trim_end_matches('\n').trim_end_matches('\r').to_string())
        .collect();

    // positions[i]：当前展示的第 i 行，在「正在处理的版本」里的行号。
    let mut positions: Vec<Option<usize>> = (0..count).map(Some).collect();
    let mut owners: Vec<Option<String>> = vec![None; count];
    let mut infos: HashMap<String, (String, String, DateTime<Utc>)> = HashMap::new();

    let mut current = Some(tip);
    let mut walked = 0usize;
    let mut last_commit = tip.to_hex();
    let mut truncated_commits = false;
    while let Some(commit) = current {
        if walked >= BLAME_MAX_COMMITS {
            truncated_commits = true;
            break;
        }
        if positions.iter().all(Option::is_none) {
            break;
        }
        walked += 1;
        let object = store::require_object(&mut conn, project_id, commit).await?;
        if object.kind != ObjectKind::Commit {
            break;
        }
        let info = parse_commit(&object.data)?;
        let parent = info.parents.first().copied();
        let cur_oid = tree::find_blob_oid(&mut conn, project_id, commit, path).await?;
        let par_oid = match parent {
            Some(parent) => tree::find_blob_oid(&mut conn, project_id, parent, path).await?,
            None => None,
        };
        if cur_oid == par_oid {
            current = parent;
            continue;
        }
        let cur_text = match cur_oid {
            Some(oid) => read_text_blob(&mut conn, project_id, oid).await?,
            None => String::new(),
        };
        let par_text = match par_oid {
            Some(oid) => read_text_blob(&mut conn, project_id, oid).await?,
            None => String::new(),
        };
        let diff = TextDiff::from_lines(&par_text, &cur_text);
        let cur_len = blame_split_lines(&cur_text).len();
        let mut map: Vec<Option<usize>> = vec![None; cur_len];
        for &op in diff.ops() {
            if let DiffOp::Equal {
                old_index,
                new_index,
                len,
            } = op
            {
                for offset in 0..len {
                    if new_index + offset < map.len() {
                        map[new_index + offset] = Some(old_index + offset);
                    }
                }
            }
        }
        let short = commit.to_hex();
        for (i, position) in positions.iter_mut().enumerate() {
            if let Some(index) = *position {
                match map.get(index).copied().flatten() {
                    Some(old_index) => *position = Some(old_index),
                    None => {
                        owners[i] = Some(short.clone());
                        *position = None;
                    }
                }
            }
        }
        infos.insert(
            short.clone(),
            (
                identity_name(&info.author),
                info.message.lines().next().unwrap_or("").to_string(),
                info.committed_at,
            ),
        );
        last_commit = short;
        current = parent;
    }

    // 走到历史尽头仍未归属的行，归给最早遍历到的那个提交。
    let fallback = infos
        .get(&last_commit)
        .cloned()
        .unwrap_or_else(|| ("unknown".to_string(), String::new(), Utc::now()));
    infos.entry(last_commit.clone()).or_insert(fallback.clone());
    let lines = display_lines
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let oid = owners[index].clone().unwrap_or_else(|| last_commit.clone());
            let (author_name, message, committed_at) = infos
                .get(&oid)
                .cloned()
                .unwrap_or_else(|| fallback.clone());
            BlameLine {
                line: index + 1,
                commit: oid,
                author_name,
                message,
                committed_at,
                text,
            }
        })
        .collect();
    Ok(BlameResult {
        path: path.to_string(),
        oid: blob_oid.to_hex(),
        truncated: truncated_bytes || truncated_lines || truncated_commits,
        lines,
    })
}

/// 项目默认分支的完整快照，供静态构建使用。
pub struct ProjectSnapshot {
    pub project: Project,
    pub files: BTreeMap<String, Vec<u8>>,
    pub commit: Oid,
    pub committed_at: DateTime<Utc>,
}

pub async fn snapshots(pool: &PgPool) -> Result<Vec<ProjectSnapshot>> {
    let projects = list_projects(pool, false).await?;
    let mut conn = pool.acquire().await?;
    let mut snapshots = Vec::new();
    for project in projects {
        let result: Result<Option<ProjectSnapshot>> = async {
            let Some(commit) =
                store::resolve_branch(&mut conn, project.id, &project.default_branch).await?
            else {
                return Ok(None);
            };
            let files = tree::read_files(&mut conn, project.id, commit).await?;
            let committed_at = store::commit_time(&mut conn, project.id, commit)
                .await?
                .unwrap_or(project.updated_at);
            Ok(Some(ProjectSnapshot {
                project: project.clone(),
                files,
                commit,
                committed_at,
            }))
        }
        .await;
        match result {
            Ok(Some(snapshot)) => snapshots.push(snapshot),
            Ok(None) => {}
            // 单个坏仓库不能永久毒化整站构建；保留其它项目继续构建。
            Err(error) => tracing::warn!(
                project = %project.slug,
                %error,
                "项目快照读取失败，跳过该项目"
            ),
        }
    }
    Ok(snapshots)
}

/// 供后续 receive-pack 使用：读取提交的父链。
pub async fn commit_parent_chain(
    conn: &mut PgConnection,
    project_id: Uuid,
    commit: Oid,
) -> Result<Vec<Oid>> {
    let object = store::require_object(conn, project_id, commit).await?;
    commit_parents(&object.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_conflict_only_when_built_paths_match() {
        let files: std::collections::BTreeSet<String> = [
            "模板.typ",
            "模板.css",
            "说明.md",
            "图标.svg",
            "notes.txt",
            "子目录/模板.typ",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();

        // 截图里的场景：修改 .typ 时同名的 .css 是静态资源，不该拦。
        assert!(ensure_no_output_conflict("模板.typ", &files).is_ok());
        assert!(ensure_no_output_conflict("模板.css", &files).is_ok());
        // 反过来新增同名静态资源也放行。
        assert!(ensure_no_output_conflict("说明.css", &files).is_ok());
        assert!(ensure_no_output_conflict("图标.png", &files).is_ok());
        assert!(ensure_no_output_conflict("notes.css", &files).is_ok());
        // 不同目录的同名页面互不干扰。
        assert!(ensure_no_output_conflict("子目录/说明.md", &files).is_ok());

        // 两个页面类文件同 stem 仍会输出成同一个 .html，必须拒绝。
        assert!(ensure_no_output_conflict("模板.md", &files).is_err());
        assert!(ensure_no_output_conflict("说明.typ", &files).is_err());
        let err = ensure_no_output_conflict("说明.html", &files).unwrap_err();
        assert!(err.to_string().contains("会输出到同一个路径"));
        // 大小写不敏感：.TYP 也是页面类。
        assert!(ensure_no_output_conflict("说明.TYP", &files).is_err());
    }

    #[test]
    fn project_output_path_matches_build_rules() {
        assert_eq!(project_output_path("模板.typ"), "模板.html");
        assert_eq!(project_output_path("a/b/说明.md"), "a/b/说明.html");
        assert_eq!(project_output_path("模板.css"), "模板.css");
        assert_eq!(project_output_path("图标.svg"), "图标.svg");
        assert_eq!(project_output_path("notes.txt"), "notes.txt");
        assert_eq!(project_output_path("README"), "README");
    }

    #[test]
    fn lfs_pointers_are_collected_and_deduplicated() {
        use crate::git::object::{Object, ObjectKind};
        let oid = "b435436345666e81778e90daa2d85cd1a13a4a27543f764cc7ac8815dbab1801";
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 4286\n"
        )
        .into_bytes();
        let objects = vec![
            Object {
                kind: ObjectKind::Blob,
                data: b"# plain text\n".to_vec(),
            },
            Object {
                kind: ObjectKind::Blob,
                data: pointer.clone(),
            },
            Object {
                kind: ObjectKind::Blob,
                data: pointer.clone(),
            },
            // 非 blob 即使长得像 pointer 也不能算。
            Object {
                kind: ObjectKind::Commit,
                data: pointer,
            },
        ];
        assert_eq!(lfs_pointers_in_snapshot(&objects), vec![oid.to_string()]);
        assert!(lfs_pointers_in_snapshot(&[]).is_empty());
    }

    #[test]
    fn link_lfs_files_copies_content_and_is_idempotent() {
        let oid = "b435436345666e81778e90daa2d85cd1a13a4a27543f764cc7ac8815dbab1801";
        let base = std::env::temp_dir().join(format!("kychnet-lfs-{}", uuid::Uuid::new_v4()));
        let source = uuid::Uuid::new_v4();
        let target = uuid::Uuid::new_v4();
        let from = lfs::object_path(&base, source, oid);
        std::fs::create_dir_all(from.parent().unwrap()).unwrap();
        std::fs::write(&from, b"binary payload").unwrap();

        let oids = [oid.to_string()];
        link_lfs_files(&base, source, target, &oids).unwrap();
        let to = lfs::object_path(&base, target, oid);
        assert_eq!(std::fs::read(&to).unwrap(), b"binary payload");
        // 目标已存在时重复调用不能报错，也不能破坏内容。
        link_lfs_files(&base, source, target, &oids).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"binary payload");

        let missing = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert!(link_lfs_files(&base, source, target, &[missing.to_string()]).is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn failed_snapshot_cleans_up_lfs_dir() {
        let base = std::env::temp_dir().join(format!("kychnet-lfs-clean-{}", uuid::Uuid::new_v4()));
        let project = uuid::Uuid::new_v4();
        let oid = "aabbaabbaabbaabbaabbaabbaabbaabbaabbaabbaabbaabbaabbaabbaabbaabb";
        let file = lfs::object_path(&base, project, oid);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"orphan").unwrap();
        assert!(file.exists());

        remove_project_lfs_dir(&base, project);
        assert!(!file.exists());
        // 目录本来就不存在时也不能 panic。
        remove_project_lfs_dir(&base, project);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn project_slug_allows_unicode_but_rejects_separators() {
        assert!(valid_project_slug("kych-net"));
        assert!(valid_project_slug("我的项目"));
        assert!(valid_project_slug("hello🚀"));
        assert!(!valid_project_slug("../x"));
        assert!(!valid_project_slug("a/b"));
        assert!(!valid_project_slug("a b"));
        assert!(!valid_project_slug(""));
    }

    #[test]
    fn file_path_rejects_escape_and_git_dir() {
        assert!(valid_file_path("index.md"));
        assert!(valid_file_path("docs/说明.md"));
        assert!(!valid_file_path("/index.md"));
        assert!(!valid_file_path("a/../b"));
        assert!(!valid_file_path(".git/config"));
        assert!(!valid_file_path("a//b"));
    }

    #[test]
    fn branch_name_rejects_git_special_forms() {
        assert!(valid_branch("main"));
        assert!(valid_branch("pr/12"));
        assert!(!valid_branch("a..b"));
        assert!(!valid_branch("a b"));
        assert!(!valid_branch("/main"));
        assert!(!valid_branch("main@{1}"));
        assert!(!valid_branch("@"));
        assert!(!valid_branch(".hidden"));
        assert!(!valid_branch("feature/.hidden"));
        assert!(!valid_branch("feature/main.lock"));
    }

    #[test]
    fn project_side_allows_deleting_any_index_file() {
        // 项目 Git 侧不设主页保护：index.html / index.md / 子目录同名文件
        // 都只是普通文件。「网站根目录只允许 index.html」只在内容侧生效。
        let changes: Vec<FileChange> = [
            "index.html",
            "index.md",
            "index.typ",
            "index.svg",
            "docs/index.html",
        ]
        .iter()
        .map(|path| FileChange {
            path: (*path).to_string(),
            content: None,
            delete: true,
        })
        .collect();
        assert!(validate_changes(&changes).is_ok());

        // 通用校验仍然生效：重复路径、非法路径照旧拒绝。
        let duplicated = vec![
            FileChange {
                path: "index.html".into(),
                content: None,
                delete: true,
            },
            FileChange {
                path: "index.html".into(),
                content: None,
                delete: true,
            },
        ];
        assert!(validate_changes(&duplicated).is_err());
        assert!(validate_changes(&[FileChange {
            path: "/index.html".into(),
            content: None,
            delete: true,
        }])
        .is_err());
    }
}
