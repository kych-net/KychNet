use anyhow::{bail, Context, Result};
use askama::Template;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

use ftml::data::PageRef;

use crate::models::{Content, ContentFormat, SiteSettings};
use crate::{config::Config, render, service};

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    site: SiteSettings,
    admin_url: String,
    posts: Vec<PostLink>,
    tree: String,
    styles: Vec<String>,
}

struct PostLink {
    title: String,
    href: String,
    date: String,
}

#[derive(Template)]
#[template(path = "page.html")]
struct PageTemplate {
    site: SiteSettings,
    admin_url: String,
    title: String,
    date: String,
    content: String,
    pdf_url: String,
    tree: String,
    styles: Vec<String>,
}

/// 一篇已经渲染完成的页面，先留在内存里，等全部成功后再统一落盘。
struct BuiltPage {
    slug: String,
    title: String,
    date: String,
    html: String,
    pdf: Option<Vec<u8>>,
}

/// 目录树节点。目录和文件分开存：目录名没有扩展名，文件名保留源格式扩展名。
#[derive(Default)]
struct TreeNode {
    dirs: BTreeMap<String, TreeNode>,
    files: Vec<TreeFile>,
}

struct TreeFile {
    name: String,
    title: String,
    url: String,
}

impl TreeNode {
    fn insert(&mut self, slug: &str, content: &Content) {
        let parts: Vec<&str> = slug.split('/').collect();
        let mut node = self;
        for (index, part) in parts.iter().enumerate() {
            if index + 1 == parts.len() {
                node.files.push(TreeFile {
                    name: source_filename(slug, content.format()),
                    title: content.title.clone(),
                    url: content_url(slug),
                });
            } else {
                node = node.dirs.entry((*part).to_string()).or_default();
            }
        }
    }

    fn sort(&mut self) {
        self.files.sort_by(|a, b| a.name.cmp(&b.name));
        for dir in self.dirs.values_mut() {
            dir.sort();
        }
    }

    fn render(&self) -> String {
        let mut out = String::from("<ul class=\"dir-tree\">");
        self.render_into(&mut out);
        out.push_str("</ul>");
        out
    }

    fn render_into(&self, out: &mut String) {
        for (name, dir) in &self.dirs {
            out.push_str("<li class=\"dir\"><button class=\"dir-toggle\" type=\"button\" aria-expanded=\"false\">");
            out.push_str(&escape_html(name));
            out.push_str("</button><ul class=\"dir-children\" hidden>");
            dir.render_into(out);
            out.push_str("</ul></li>");
        }
        for file in &self.files {
            out.push_str("<li class=\"file\"><a href=\"");
            out.push_str(&escape_html(&file.url));
            out.push_str("\" title=\"");
            out.push_str(&escape_html(&file.title));
            out.push_str("\">");
            out.push_str(&escape_html(&file.name));
            out.push_str("</a></li>");
        }
    }
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 把单个路径段编码成 URL 片段：非 ASCII 字节转成 `%XX`，ASCII 字母数字与 `-` `_` 原样保留。
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for &byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// 输出路径对应的公开 URL。目录主页会得到 `/xef/`，主页得到 `/`。
fn content_url(slug: &str) -> String {
    if slug == service::HOME_SLUG {
        return "/".to_string();
    }
    if let Some(dir) = slug.strip_suffix("/index.html") {
        return format!("/{}/", encode_path(dir));
    }
    format!("/{}", encode_path(slug))
}

fn pdf_path(slug: &str) -> String {
    match slug.strip_suffix(".html") {
        Some(stem) => format!("{stem}.pdf"),
        None => format!("{slug}.pdf"),
    }
}

fn source_filename(slug: &str, format: ContentFormat) -> String {
    service::source_path(slug, format)
        .rsplit('/')
        .next()
        .unwrap_or(slug)
        .to_string()
}

/// 管理端触发的重建运行在 UMask=0077 的服务进程里，新文件默认 600/700，
/// nginx 会直接 403。这里在写完后显式修正为 755/644。
#[cfg(unix)]
fn make_public(path: &Path, directory: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(if directory { 0o755 } else { 0o644 });
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_public(_path: &Path, _directory: bool) -> Result<()> {
    Ok(())
}

fn make_parent_dirs_public(out: &Path, parent: &Path) -> Result<()> {
    let mut current = Some(parent);
    while let Some(dir) = current {
        if dir == out {
            make_public(dir, true)?;
            break;
        }
        if !dir.starts_with(out) {
            break;
        }
        make_public(dir, true)?;
        current = dir.parent();
    }
    Ok(())
}

/// 页面顶部显式声明要加载的 CSS：
///
/// ```text
/// ---
/// css: /css/theme.css, /css/wikidot.css
/// ---
/// ```
///
/// 返回 (链接列表, 去掉声明头后的正文)。格式未知或没有声明时原样返回。
pub(crate) fn parse_page_styles(body: &str) -> Result<(Vec<String>, String)> {
    parse_page_styles_with_base(body, None)
}

/// `base` 为 Some 时用于项目文件：相对 CSS 路径按 `/项目/<项目>/` 解析。
fn parse_page_styles_with_base(
    body: &str,
    base: Option<&str>,
) -> Result<(Vec<String>, String)> {
    let lines: Vec<&str> = body.lines().collect();
    if lines.first().map(|line| line.trim()) != Some("---") {
        return Ok((Vec::new(), body.to_string()));
    }
    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(1)
        .take(60)
        .find(|(_, line)| line.trim() == "---")
        .map(|(index, _)| index)
    else {
        return Ok((Vec::new(), body.to_string()));
    };

    let mut styles = Vec::new();
    for line in &lines[1..end] {
        let Some(value) = line
            .trim()
            .strip_prefix("css:")
            .or_else(|| line.trim().strip_prefix("styles:"))
        else {
            continue;
        };
        for token in value.split([',', ';', ' ', '\t']) {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let href = match base {
                Some(base) => project_style_href(base, token)
                    .with_context(|| format!("项目 CSS 声明非法: {token}"))?,
                None => {
                    let (slug, _) = service::normalize_content_path(token, ContentFormat::Css)
                        .with_context(|| format!("页面 CSS 声明非法: {token}"))?;
                    format!("/{slug}")
                }
            };
            if !styles.contains(&href) {
                styles.push(href);
            }
        }
    }

    let stripped = lines[end + 1..].join("\n");
    let stripped = if body.ends_with('\n') && !stripped.is_empty() {
        format!("{stripped}\n")
    } else {
        stripped
    };
    Ok((styles, stripped))
}

/// 项目页面的 CSS 可以写相对路径，例如 `css: theme.css`；
/// 以 `/` 开头时仍按全站绝对路径解析。
fn project_style_href(base: &str, token: &str) -> Result<String> {
    let raw = if token.starts_with('/') {
        token.trim_start_matches('/').to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), token.trim_start_matches('/'))
    };
    let mut parts = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            bail!("CSS 路径不能包含空段、. 或 ..");
        }
        if segment.chars().any(|ch| ch.is_control() || ch == '\\' || ch == '\0') {
            bail!("CSS 路径包含非法字符");
        }
        parts.push(segment);
    }
    let joined = parts.join("/");
    if !joined.to_ascii_lowercase().ends_with(".css") {
        bail!("CSS 路径必须以 .css 结尾");
    }
    Ok(format!("/{joined}"))
}

/// 项目页面在内容站的输出路径。
fn project_page_slug(project_slug: &str, path: &str) -> String {
    let stem = match path.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => path,
    };
    format!("{}/{}/{}.html", crate::project::PROJECT_NAMESPACE, project_slug, stem)
}

fn project_asset_slug(project_slug: &str, path: &str) -> String {
    format!("{}/{}/{}", crate::project::PROJECT_NAMESPACE, project_slug, path)
}

fn project_home_slug(project_slug: &str, files: &BTreeMap<String, Vec<u8>>) -> Option<String> {
    for name in ["index.md", "index.html", "index.typ", "index.wj"] {
        if files.contains_key(name) {
            return Some(project_page_slug(project_slug, name));
        }
    }
    files
        .keys()
        .find(|path| {
            path.rsplit_once('.')
                .and_then(|(_, ext)| ContentFormat::from_extension(ext))
                .is_some_and(|format| format != ContentFormat::Css)
        })
        .map(|path| project_page_slug(project_slug, path))
}

fn project_style_base(slug: &str) -> Option<&str> {
    let mut parts = slug.splitn(3, '/');
    let namespace = parts.next()?;
    let project = parts.next()?;
    if namespace != crate::project::PROJECT_NAMESPACE || project.is_empty() {
        return None;
    }
    Some(slug.rsplit_once('/').map(|(base, _)| base).unwrap_or(slug))
}

/// 项目文件的标题：优先读头部 `title:`，否则用文件名。
fn project_file_title(body: &str, path: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    if lines.first().map(|line| line.trim()) == Some("---") {
        if let Some(end) = lines
            .iter()
            .enumerate()
            .skip(1)
            .take(60)
            .find(|(_, line)| line.trim() == "---")
            .map(|(index, _)| index)
        {
            for line in &lines[1..end] {
                if let Some(value) = line.trim().strip_prefix("title:") {
                    let value = value.trim();
                    if !value.is_empty() {
                        return value.to_string();
                    }
                }
            }
        }
    }
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(path);
    stem.to_string()
}

fn write_raw(out: &Path, slug: &str, data: &[u8]) -> Result<()> {
    let target = out.join(slug);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
        make_parent_dirs_public(out, parent)?;
    }
    std::fs::write(&target, data)?;
    make_public(&target, false)?;
    Ok(())
}

fn write_style(out: &Path, content: &Content) -> Result<()> {
    if !service::valid_style_slug(&content.slug) {
        bail!("非法 CSS 路径: {}", content.slug);
    }
    let target = out.join(&content.slug);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
        make_parent_dirs_public(out, parent)?;
    }
    std::fs::write(&target, &content.body)?;
    make_public(&target, false)?;
    Ok(())
}

/// /项目/ 完全由仓库生成，整站构建时整体重建，避免删除/改名后的旧文件残留。
fn clean_project_output(root: &Path) -> Result<()> {
    let dir = root.join(crate::project::PROJECT_NAMESPACE);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// 全站构建时清掉 /css 下由内容生成的 .css；其它静态资源保留。
fn clean_styles(root: &Path) -> Result<()> {
    let dir = root.join("css");
    if !dir.exists() {
        return Ok(());
    }
    fn clean(dir: &Path) -> Result<bool> {
        let mut empty = true;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if clean(&path)? {
                    std::fs::remove_dir(&path)?;
                } else {
                    empty = false;
                }
            } else if file_type.is_file() {
                if path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("css")) {
                    std::fs::remove_file(&path)?;
                } else {
                    empty = false;
                }
            } else {
                empty = false;
            }
        }
        Ok(empty)
    }
    clean(&dir)?;
    Ok(())
}

pub async fn run(pool: &PgPool, cfg: &Config, out: &Path) -> Result<()> {
    let settings = service::get_settings(pool).await?;
    let all = service::list_published(pool).await?;

    let mut contents = Vec::new();
    let mut styles = Vec::new();
    for content in all {
        if content.format() == ContentFormat::Css {
            if service::valid_style_slug(&content.slug) {
                styles.push(content);
            } else {
                tracing::warn!("跳过非法 CSS slug: {:?}", content.slug);
            }
        } else if service::valid_content_slug(&content.slug) {
            contents.push(content);
        } else {
            // 历史脏数据不该让整站构建永久失败。新增数据已在 save_content 拦住，
            // 这里只作兜底：跳过并告警，索引和目录树里也不会出现它。
            tracing::warn!("跳过非法 slug: {:?}", content.slug);
        }
    }
    styles.sort_by(|a, b| a.slug.cmp(&b.slug));

    // 项目仓库是 /项目/ 下内容的唯一来源：把默认分支的文件编译成普通页面。
    let snapshots = crate::project::snapshots(pool).await?;
    let mut project_styles = Vec::new();
    let mut project_assets = Vec::new();
    let mut project_typst: HashMap<String, (HashMap<String, Vec<u8>>, String)> = HashMap::new();
    let mut project_links = Vec::new();
    let mut seen_slugs: HashSet<String> = contents.iter().map(|item| item.slug.clone()).collect();

    for snapshot in &snapshots {
        let files_map: HashMap<String, Vec<u8>> = snapshot
            .files
            .iter()
            .map(|(path, data)| (path.clone(), data.clone()))
            .collect();
        for (path, data) in &snapshot.files {
            if !crate::git::object::is_text(data) {
                tracing::warn!(
                    project = %snapshot.project.slug,
                    path = %path,
                    "跳过非文本项目文件；二进制需要 LFS，目前尚未实现"
                );
                continue;
            }
            let extension = path.rsplit_once('.').map(|(_, ext)| ext).unwrap_or_default();
            match ContentFormat::from_extension(extension) {
                Some(ContentFormat::Css) => {
                    project_styles.push((
                        project_asset_slug(&snapshot.project.slug, path),
                        data.clone(),
                    ));
                }
                Some(format) => {
                    let slug = project_page_slug(&snapshot.project.slug, path);
                    if !seen_slugs.insert(slug.clone()) {
                        bail!("项目 {path} 与其他文件输出到同一个路径: {slug}");
                    }
                    let body = String::from_utf8(data.clone())
                        .with_context(|| format!("项目文件不是 UTF-8: {path}"))?;
                    let title = project_file_title(&body, path);
                    contents.push(Content {
                        id: Uuid::new_v4(),
                        slug: slug.clone(),
                        title,
                        format: format.as_str().to_string(),
                        body,
                        published: true,
                        created_at: snapshot.committed_at,
                        updated_at: snapshot.committed_at,
                    });
                    if format == ContentFormat::Typst {
                        project_typst.insert(slug, (files_map.clone(), path.clone()));
                    }
                }
                None => {
                    project_assets.push((
                        project_asset_slug(&snapshot.project.slug, path),
                        data.clone(),
                    ));
                }
            }
        }
        if let Some(home) = project_home_slug(&snapshot.project.slug, &snapshot.files) {
            project_links.push(PostLink {
                title: snapshot.project.title.clone(),
                href: content_url(&home),
                date: snapshot.committed_at.format("%Y-%m-%d").to_string(),
            });
        }
    }
    project_links.sort_by(|a, b| a.title.cmp(&b.title));

    let mut tree = TreeNode::default();
    let mut wikidot_includes = HashMap::new();
    for content in &contents {
        tree.insert(&content.slug, content);
        if content.format() == ContentFormat::Wikidot {
            let stem = content.slug.strip_suffix(".html").unwrap_or(&content.slug);
            let page = stem.replace('/', ":");
            wikidot_includes.insert(page.clone(), content.body.clone());
            let normalized = PageRef::page_only(&page).page;
            wikidot_includes.insert(normalized, content.body.clone());
        }
    }
    tree.sort();
    let tree_html = tree.render();

    // 先把所有页面渲染到内存。任何一篇 Typst 编译失败都不会触碰磁盘。
    let mut pages = Vec::new();
    for content in &contents {
        let page = content
            .slug
            .strip_suffix(".html")
            .unwrap_or(&content.slug)
            .replace('/', ":");
        let base = project_style_base(&content.slug);
        let (styles, body) = parse_page_styles_with_base(&content.body, base)?;
        let ctx = render::DocContext {
            page: &page,
            title: &content.title,
            includes: &wikidot_includes,
        };
        let rendered = match project_typst.get(&content.slug) {
            Some((files, main_path)) => render::render_doc_with_project(
                content.format(),
                &body,
                &cfg.content_root,
                &ctx,
                files,
                main_path,
            ),
            None => render::render_doc(content.format(), &body, &cfg.content_root, &ctx),
        }
        .with_context(|| format!("渲染 {} 失败", content.slug))?;

        let date = content.updated_at.format("%Y-%m-%d").to_string();
        let pdf_url = if rendered.pdf.is_some() {
            format!("/{}", encode_path(&pdf_path(&content.slug)))
        } else {
            String::new()
        };
        let page = PageTemplate {
            site: settings.clone(),
            admin_url: cfg.admin_origin.clone(),
            title: content.title.clone(),
            date: date.clone(),
            content: rendered.html,
            pdf_url,
            tree: tree_html.clone(),
            styles,
        };
        pages.push(BuiltPage {
            slug: content.slug.clone(),
            title: content.title.clone(),
            date,
            html: page.render()?,
            pdf: rendered.pdf,
        });
    }

    // /项目/ 索引页由项目列表自动生成，不允许被普通内容占用。
    if !snapshots.is_empty() {
        let html = IndexTemplate {
            site: settings.clone(),
            admin_url: cfg.admin_origin.clone(),
            posts: project_links,
            tree: tree_html.clone(),
            styles: Vec::new(),
        }
        .render()?;
        pages.push(BuiltPage {
            slug: format!("{}/index.html", crate::project::PROJECT_NAMESPACE),
            title: "项目".to_string(),
            date: String::new(),
            html,
            pdf: None,
        });
    }

    // 只有数据库里没有主页内容时才回退到自动文章列表。
    // 正常情况下 index.html 是内容，且 API 禁止删除。
    let index_html = if pages.iter().any(|page| page.slug == service::HOME_SLUG) {
        None
    } else {
        let mut links: Vec<PostLink> = pages
            .iter()
            .map(|page| PostLink {
                title: page.title.clone(),
                href: content_url(&page.slug),
                date: page.date.clone(),
            })
            .collect();
        links.sort_by(|a, b| b.date.cmp(&a.date));
        Some(
            IndexTemplate {
                site: settings.clone(),
                admin_url: cfg.admin_origin.clone(),
                posts: links,
                tree: tree_html.clone(),
                styles: Vec::new(),
            }
            .render()?,
        )
    };

    // 全部渲染成功后才清理旧产物：删除内容、改路径后，旧 .html / .pdf 不再残留。
    // 只删除本程序生成的文件，保留目录里可能存在的其它资源。
    std::fs::create_dir_all(out)?;
    make_public(out, true)?;
    clean_generated(out)?;
    clean_styles(out)?;
    clean_project_output(out)?;

    for style in &styles {
        write_style(out, style)?;
    }
    for (slug, data) in &project_styles {
        write_raw(out, slug, data)?;
    }

    for page in &pages {
        let target = out.join(&page.slug);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
            make_parent_dirs_public(out, parent)?;
        }
        std::fs::write(&target, &page.html)?;
        make_public(&target, false)?;
        if let Some(pdf) = &page.pdf {
            let pdf_target = out.join(pdf_path(&page.slug));
            if let Some(parent) = pdf_target.parent() {
                std::fs::create_dir_all(parent)?;
                make_parent_dirs_public(out, parent)?;
            }
            std::fs::write(&pdf_target, pdf)?;
            make_public(&pdf_target, false)?;
        }
    }
    for (slug, data) in &project_assets {
        write_raw(out, slug, data)?;
    }
    if let Some(index_html) = index_html {
        let index_path = out.join(service::HOME_SLUG);
        std::fs::write(&index_path, index_html)?;
        make_public(&index_path, false)?;
    }
    Ok(())
}

/// 编译单个内容页；管理端保存后调用。
///
/// 不清理整站，只写/删这一页。目录树会按当前全部已发布内容重建，
/// 因此这一页上的树是最新的；其它页面的树要等整站重建刷新。
pub async fn run_one(
    pool: &PgPool,
    cfg: &Config,
    out: &Path,
    only: &str,
    remove: Option<&str>,
) -> Result<()> {
    if !service::valid_content_slug(only) && !service::valid_style_slug(only) {
        bail!("非法内容路径: {only}");
    }
    if let Some(old) = remove {
        if old != only && (service::valid_content_slug(old) || service::valid_style_slug(old)) {
            remove_output(out, old)?;
        }
    }

    let settings = service::get_settings(pool).await?;
    let all = service::list_published(pool).await?;
    let mut contents = Vec::new();
    let mut styles = Vec::new();
    for content in all {
        if content.format() == ContentFormat::Css {
            if service::valid_style_slug(&content.slug) {
                styles.push(content);
            }
        } else if service::valid_content_slug(&content.slug) {
            contents.push(content);
        }
    }
    styles.sort_by(|a, b| a.slug.cmp(&b.slug));

    if service::valid_style_slug(only) {
        if let Some(style) = styles.iter().find(|style| style.slug == only) {
            write_style(out, style)?;
        } else {
            remove_output(out, only)?;
        }
        return Ok(());
    }

    let mut tree = TreeNode::default();
    let mut wikidot_includes = HashMap::new();
    for content in &contents {
        tree.insert(&content.slug, content);
        if content.format() == ContentFormat::Wikidot {
            let stem = content.slug.strip_suffix(".html").unwrap_or(&content.slug);
            let page = stem.replace('/', ":");
            wikidot_includes.insert(page.clone(), content.body.clone());
            let normalized = PageRef::page_only(&page).page;
            wikidot_includes.insert(normalized, content.body.clone());
        }
    }
    tree.sort();
    let tree_html = tree.render();

    let Some(content) = contents.iter().find(|content| content.slug == only) else {
        remove_output(out, only)?;
        return Ok(());
    };

    let (styles, body) = parse_page_styles(&content.body)?;
    let page = content
        .slug
        .strip_suffix(".html")
        .unwrap_or(&content.slug)
        .replace('/', ":");
    let ctx = render::DocContext {
        page: &page,
        title: &content.title,
        includes: &wikidot_includes,
    };
    let rendered = render::render_doc(content.format(), &body, &cfg.content_root, &ctx)
        .with_context(|| format!("渲染 {} 失败", content.slug))?;
    let date = content.updated_at.format("%Y-%m-%d").to_string();
    let pdf_url = if rendered.pdf.is_some() {
        format!("/{}", encode_path(&pdf_path(&content.slug)))
    } else {
        String::new()
    };
    let html = PageTemplate {
        site: settings.clone(),
        admin_url: cfg.admin_origin.clone(),
        title: content.title.clone(),
        date,
        content: rendered.html,
        pdf_url,
        tree: tree_html,
        styles,
    }
    .render()?;

    std::fs::create_dir_all(out)?;
    make_public(out, true)?;
    let target = out.join(&content.slug);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
        make_parent_dirs_public(out, parent)?;
    }
    std::fs::write(&target, html)?;
    make_public(&target, false)?;
    if let Some(pdf) = rendered.pdf {
        let pdf_target = out.join(pdf_path(&content.slug));
        if let Some(parent) = pdf_target.parent() {
            std::fs::create_dir_all(parent)?;
            make_parent_dirs_public(out, parent)?;
        }
        std::fs::write(&pdf_target, pdf)?;
        make_public(&pdf_target, false)?;
    }
    Ok(())
}

/// 删除单页产物，并清掉因此变空的目录。
fn remove_output(out: &Path, slug: &str) -> Result<()> {
    if !service::valid_content_slug(slug) && !service::valid_style_slug(slug) {
        return Ok(());
    }
    let is_page = service::valid_content_slug(slug);
    let html = out.join(slug);
    let pdf = if is_page {
        Some(out.join(pdf_path(slug)))
    } else {
        None
    };
    for path in [Some(&html), pdf.as_ref()].into_iter().flatten() {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut current = html.parent();
    while let Some(dir) = current {
        if dir == out || !dir.starts_with(out) {
            break;
        }
        match std::fs::read_dir(dir).and_then(|mut entries| entries.next().transpose()) {
            Ok(None) => {
                std::fs::remove_dir(dir)?;
                current = dir.parent();
            }
            Ok(Some(_)) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = dir.parent();
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// 删除 dist 下旧的 HTML/PDF 产物，并移除因此变空的目录；其它资源保留。
fn clean_generated(root: &Path) -> Result<()> {
    fn clean_dir(dir: &Path) -> Result<bool> {
        let mut empty = true;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if clean_dir(&path)? {
                    std::fs::remove_dir(&path)?;
                } else {
                    empty = false;
                }
            } else if file_type.is_file() {
                let keep = match path.extension().and_then(|ext| ext.to_str()) {
                    Some(ext) if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("pdf") => {
                        std::fs::remove_file(&path)?;
                        false
                    }
                    _ => true,
                };
                if keep {
                    empty = false;
                }
            } else {
                empty = false;
            }
        }
        Ok(empty)
    }
    clean_dir(root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> SiteSettings {
        SiteSettings::default()
    }

    #[test]
    fn index_and_page_templates_render() {
        let tree = "<ul class=\"dir-tree\"></ul>".to_string();
        let index = IndexTemplate {
            site: settings(),
            admin_url: "https://m.kych.net".into(),
            posts: vec![PostLink {
                title: "第一篇".into(),
                href: "/hello.html".into(),
                date: "2026-11-06".into(),
            }],
            tree: tree.clone(),
            styles: Vec::new(),
        };
        let html = index.render().unwrap();
        assert!(html.contains("第一篇"));
        assert!(html.contains("管理端"));

        let page = PageTemplate {
            site: settings(),
            admin_url: "https://m.kych.net".into(),
            title: "标题".into(),
            date: "2026-11-06".into(),
            content: "<p>正文</p>".into(),
            pdf_url: "/demo.pdf".into(),
            tree,
            styles: Vec::new(),
        };
        let html = page.render().unwrap();
        assert!(html.contains("<p>正文</p>"));
        assert!(html.contains("下载 PDF"));
        assert!(html.contains("theme-toggle"));
    }

    #[test]
    fn encodes_unicode_segments_for_url() {
        assert_eq!(content_url("你好.html"), "/%E4%BD%A0%E5%A5%BD.html");
        assert_eq!(content_url("café.html"), "/caf%C3%A9.html");
        assert_eq!(content_url("a-b_1.html"), "/a-b_1.html");
        assert_eq!(content_url("index.html"), "/");
        assert_eq!(content_url("xef/index.html"), "/xef/");
        assert_eq!(content_url("reff/93df/l.html"), "/reff/93df/l.html");
        assert_eq!(content_url("你好/index.html"), "/%E4%BD%A0%E5%A5%BD/");
    }

    #[test]
    fn tree_renders_directories_and_files() {
        let mut tree = TreeNode::default();
        let content = |slug: &str, title: &str| Content {
            id: uuid::Uuid::new_v4(),
            slug: slug.to_string(),
            title: title.to_string(),
            format: "markdown".into(),
            body: String::new(),
            published: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        tree.insert("index.html", &content("index.html", "主页"));
        tree.insert("xef/index.html", &content("xef/index.html", "xef"));
        tree.insert("xef/a.html", &content("xef/a.html", "A"));
        tree.sort();
        let html = tree.render();
        assert!(html.contains("dir-toggle"));
        assert!(html.contains("href=\"/xef/\""));
        assert!(html.contains("href=\"/xef/a.html\""));
        assert!(html.contains("index.md"));
    }

    #[test]
    fn cleans_only_generated_files() {
        let base = std::env::temp_dir().join(format!("kych-net-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("old")).unwrap();
        std::fs::write(base.join("old/page.html"), "old").unwrap();
        std::fs::write(base.join("old/page.pdf"), "old").unwrap();
        std::fs::write(base.join("asset.png"), "keep").unwrap();
        clean_generated(&base).unwrap();
        assert!(!base.join("old").exists());
        assert!(base.join("asset.png").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn generated_files_are_readable_by_nginx() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("kych-net-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("nested")).unwrap();
        std::fs::write(base.join("nested/page.html"), "x").unwrap();
        std::fs::set_permissions(base.join("nested/page.html"), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(base.join("nested"), std::fs::Permissions::from_mode(0o700)).unwrap();
        make_parent_dirs_public(&base, &base.join("nested")).unwrap();
        make_public(&base.join("nested/page.html"), false).unwrap();
        assert_eq!(std::fs::metadata(base.join("nested")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::metadata(base.join("nested/page.html")).unwrap().permissions().mode() & 0o777, 0o644);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn parses_page_css_declaration() {
        let (styles, body) = parse_page_styles(
            "---\ncss: /css/theme.css, /css/wikidot\n---\n\n**正文**\n",
        )
        .unwrap();
        assert_eq!(styles, vec!["/css/theme.css", "/css/wikidot.css"]);
        assert_eq!(body, "\n**正文**\n");

        let (styles, body) = parse_page_styles("**正文**").unwrap();
        assert!(styles.is_empty());
        assert_eq!(body, "**正文**");
    }

    #[test]
    fn remove_output_removes_page_and_empty_dirs() {
        let base = std::env::temp_dir().join(format!("kych-net-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("a/b")).unwrap();
        std::fs::write(base.join("a/b/page.html"), "x").unwrap();
        std::fs::write(base.join("a/b/page.pdf"), "x").unwrap();
        remove_output(&base, "a/b/page.html").unwrap();
        assert!(!base.join("a/b/page.html").exists());
        assert!(!base.join("a/b/page.pdf").exists());
        assert!(!base.join("a").exists());
        std::fs::remove_dir_all(&base).ok();
    }
}
