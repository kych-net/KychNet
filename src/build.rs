use anyhow::{bail, Context, Result};
use askama::Template;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

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

pub async fn run(pool: &PgPool, cfg: &Config, out: &Path) -> Result<()> {
    let settings = service::get_settings(pool).await?;
    let all = service::list_published(pool).await?;

    let contents: Vec<Content> = all
        .into_iter()
        .filter(|content| {
            if service::valid_content_slug(&content.slug) {
                true
            } else {
                // 历史脏数据不该让整站构建永久失败。新增数据已在 save_content 拦住，
                // 这里只作兜底：跳过并告警，索引和目录树里也不会出现它。
                tracing::warn!("跳过非法 slug: {:?}", content.slug);
                false
            }
        })
        .collect();

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
        let ctx = render::DocContext {
            page: &page,
            title: &content.title,
            includes: &wikidot_includes,
        };
        let rendered = render::render_doc(content.format(), &content.body, &cfg.content_root, &ctx)
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
        };
        pages.push(BuiltPage {
            slug: content.slug.clone(),
            title: content.title.clone(),
            date,
            html: page.render()?,
            pdf: rendered.pdf,
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
                site: settings,
                admin_url: cfg.admin_origin.clone(),
                posts: links,
                tree: tree_html,
            }
            .render()?,
        )
    };

    // 全部渲染成功后才清理旧产物：删除内容、改路径后，旧 .html / .pdf 不再残留。
    // 只删除本程序生成的文件，保留目录里可能存在的其它资源。
    std::fs::create_dir_all(out)?;
    make_public(out, true)?;
    clean_generated(out)?;

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
    if !service::valid_content_slug(only) {
        bail!("非法内容路径: {only}");
    }
    if let Some(old) = remove {
        if old != only && service::valid_content_slug(old) {
            remove_output(out, old)?;
        }
    }

    let settings = service::get_settings(pool).await?;
    let all = service::list_published(pool).await?;
    let contents: Vec<Content> = all
        .into_iter()
        .filter(|content| service::valid_content_slug(&content.slug))
        .collect();

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
    let rendered = render::render_doc(content.format(), &content.body, &cfg.content_root, &ctx)
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
    if !service::valid_content_slug(slug) {
        return Ok(());
    }
    let html = out.join(slug);
    let pdf = out.join(pdf_path(slug));
    for path in [&html, &pdf] {
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
