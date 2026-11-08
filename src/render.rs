use anyhow::{bail, Result};
use ftml::data::{PageInfo, PageRef, ScoreValue};
use ftml::includes::{FetchedPage, IncludeRef, Includer};
use ftml::layout::Layout;
use ftml::render::html::HtmlRender;
use ftml::render::Render;
use ftml::settings::{WikitextMode, WikitextSettings};
use pulldown_cmark::{html, Options, Parser};
use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::html::HtmlDocument;
use typst::layout::PagedDocument;
use typst::syntax::{FileId, Source, VirtualPath};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Feature, Library, World};

use crate::models::ContentFormat;

pub struct Rendered {
    pub html: String,
    pub pdf: Option<Vec<u8>>,
}

/// 渲染一页 Wikidot 所需的上下文。
///
/// `includes` 是构建时从数据库预取的 Wikidot 页面，key 是 ftml 归一化后的页面名
/// （例如 `component:header`），这样 include 阶段不需要同步查库。
pub struct DocContext<'a> {
    pub page: &'a str,
    pub title: &'a str,
    pub includes: &'a HashMap<String, String>,
}

impl DocContext<'_> {
    fn empty() -> Self {
        Self {
            page: "page",
            title: "",
            includes: &EMPTY_INCLUDES,
        }
    }
}

static EMPTY_INCLUDES: std::sync::LazyLock<HashMap<String, String>> =
    std::sync::LazyLock::new(HashMap::new);

pub fn render(format: ContentFormat, body: &str, root: &Path) -> Result<Rendered> {
    render_doc(format, body, root, &DocContext::empty())
}

pub fn render_doc(
    format: ContentFormat,
    body: &str,
    root: &Path,
    ctx: &DocContext,
) -> Result<Rendered> {
    render_doc_with_project(format, body, root, ctx, &HashMap::new(), "main.typ")
}

/// 渲染项目文件。`files` 是项目根下的完整文件快照，Typst 的相对 include
/// 会优先从这里读取；读不到再回退到磁盘上的全站内容目录。
pub fn render_doc_with_project(
    format: ContentFormat,
    body: &str,
    root: &Path,
    ctx: &DocContext,
    files: &HashMap<String, Vec<u8>>,
    main_path: &str,
) -> Result<Rendered> {
    match format {
        ContentFormat::Markdown => Ok(Rendered {
            html: render_markdown(body),
            pdf: None,
        }),
        ContentFormat::Html => Ok(Rendered {
            html: body.to_string(),
            pdf: None,
        }),
        ContentFormat::Typst => render_typst(body, root, files, main_path),
        ContentFormat::Wikidot => render_wikidot(body, ctx),
        ContentFormat::Css => bail!("CSS 文件不是页面，不能走页面渲染"),
    }
}

struct MapIncluder<'a> {
    pages: &'a HashMap<String, String>,
}

impl<'t> Includer<'t> for MapIncluder<'_> {
    type Error = Infallible;

    fn include_pages(
        &mut self,
        includes: &[IncludeRef<'t>],
    ) -> Result<Vec<FetchedPage<'t>>, Self::Error> {
        Ok(includes
            .iter()
            .map(|include| {
                let page_ref = include.page_ref().clone();
                let content = self
                    .pages
                    .get(page_ref.page())
                    .map(|body| Cow::Owned(body.clone()));
                FetchedPage { page_ref, content }
            })
            .collect())
    }

    fn no_such_include(
        &mut self,
        page_ref: &PageRef,
    ) -> Result<Cow<'t, str>, Self::Error> {
        Ok(Cow::Owned(format!(
            "<div class=\"wiki-error\">include 不存在: {}</div>",
            page_ref.page()
        )))
    }
}

const INCLUDE_EXPANSION_BUDGET: usize = 8 * 1024 * 1024;
const INCLUDE_TOTAL_LIMIT: usize = 16 * 1024 * 1024;

fn render_wikidot(body: &str, ctx: &DocContext) -> Result<Rendered> {
    let mut text = body.to_string();
    let mut cumulative_growth = 0usize;
    let mut settings = WikitextSettings::from_mode(WikitextMode::Page, Layout::Wikidot);
    settings.use_true_ids = false;
    settings.isolate_user_ids = true;
    settings.allow_local_paths = false;

    // 支持 include 的递归展开，同时限制深度，避免 A include B、B include A 时无限增长。
    for _ in 0..8 {
        if text.len() > INCLUDE_TOTAL_LIMIT {
            anyhow::bail!("Wikidot include 展开超过总大小上限");
        }
        let before = text.len();
        let (next, _pages) = ftml::include(
            &text,
            &settings,
            MapIncluder {
                pages: ctx.includes,
            },
            || -> Infallible { unreachable!("ftml include 不会返回这个错误") },
        )?;
        if next == text {
            break;
        }
        if next.len() > INCLUDE_TOTAL_LIMIT {
            anyhow::bail!("Wikidot include 展开超过总大小上限");
        }
        cumulative_growth = cumulative_growth.saturating_add(next.len().saturating_sub(before));
        if cumulative_growth > INCLUDE_EXPANSION_BUDGET {
            anyhow::bail!("Wikidot include 展开超过累计增长上限");
        }
        text = next;
    }

    ftml::preprocess(&mut text);
    let page = PageRef::page_only(ctx.page.replace('/', ":")).page;
    let info = PageInfo {
        page: Cow::Owned(page),
        category: None,
        site: Cow::Borrowed("kych.net"),
        title: Cow::Owned(ctx.title.to_string()),
        alt_title: None,
        score: ScoreValue::Integer(0),
        tags: vec![],
        language: Cow::Borrowed("zh-CN"),
    };
    let tokenization = ftml::tokenize(&text);
    let parsed = ftml::parse(&tokenization, &info, &settings);
    if !parsed.errors().is_empty() {
        tracing::warn!(errors = ?parsed.errors(), "Wikidot 解析警告");
    }
    let output = HtmlRender.render(parsed.value(), &info, &settings);
    Ok(Rendered {
        html: output.body,
        pdf: None,
    })
}

fn render_markdown(body: &str) -> String {
    let mut output = String::new();
    let parser = Parser::new_ext(body, Options::all());
    html::push_html(&mut output, parser);
    output
}

struct TypstWorld {
    root: PathBuf,
    /// `@preview/...` 外部包缓存根目录；结构为 <namespace>/<name>/<version>/。
    package_root: PathBuf,
    main: FileId,
    source: Source,
    /// 项目仓库文件，key 是相对项目根的路径。
    files: HashMap<String, Vec<u8>>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    library: LazyHash<Library>,
}

impl TypstWorld {
    fn new(
        body: &str,
        root: &Path,
        main_path: &str,
        files: &HashMap<String, Vec<u8>>,
    ) -> Self {
        let main = FileId::new(None, VirtualPath::new(main_path));
        let source = Source::new(main, body.to_string());
        let package_root = std::env::var_os("TYPST_PACKAGE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("typst-packages"));

        let mut book = FontBook::new();
        let mut fonts = Vec::new();
        for data in typst_assets::fonts() {
            let bytes = Bytes::new(data);
            for font in Font::iter(bytes) {
                book.push(font.info().clone());
                fonts.push(font);
            }
        }

        let library = Library::builder()
            .with_features(std::iter::once(Feature::Html).collect())
            .build();

        Self {
            root: root.to_path_buf(),
            package_root,
            main,
            source,
            files: files.clone(),
            book: LazyHash::new(book),
            fonts,
            library: LazyHash::new(library),
        }
    }

    /// 项目仓库文件优先于磁盘内容目录。
    fn memory_file(&self, id: FileId) -> Option<&Vec<u8>> {
        // 包文件不能落到项目仓库的同名路径上（例如 typst.toml）。
        if id.package().is_some() {
            return None;
        }
        let key = id
            .vpath()
            .as_rooted_path()
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
        self.files.get(&key)
    }

    /// 把 Typst 的虚拟路径安全地解析到内容根目录内。
    ///
    /// 注意:`VirtualPath::as_rooted_path()` 返回带前导 `/` 的路径,直接交给
    /// `PathBuf::join` 会丢弃左侧 root(实测 `content.join("/foo.typ") == "/foo.typ"`),
    /// 使 `CONTENT_ROOT` 完全失效,并允许内容读取构建机上的任意文件。
    /// 必须使用库自带的 `resolve(root)`,它会在路径词法逃逸时返回 None。
    fn path_for(&self, id: FileId) -> FileResult<PathBuf> {
        let path = id
            .vpath()
            .resolve(&self.root)
            .ok_or_else(|| FileError::NotFound(id.vpath().as_rooted_path().to_path_buf()))?;

        // resolve 只做词法校验,符号链接仍可能逃出根目录。
        // 再对父目录做一次真实路径校验,确保最终读取位置确实位于 root 内。
        let root = self
            .root
            .canonicalize()
            .map_err(|_| FileError::NotFound(self.root.clone()))?;
        let parent = path.parent().unwrap_or(&self.root);
        let real_parent = parent
            .canonicalize()
            .map_err(|_| FileError::NotFound(path.clone()))?;
        if !real_parent.starts_with(&root) {
            return Err(FileError::NotFound(path));
        }
        let target = real_parent.join(path.file_name().unwrap_or_default());

        // 父目录安全不等于文件本身安全:`content/img/link.png -> /etc/passwd`
        // 照样会被后面的 fs::read 跟进去。构建目录里不该存在符号链接,直接拒绝。
        let meta = std::fs::symlink_metadata(&target)
            .map_err(|_| FileError::NotFound(target.clone()))?;
        if meta.file_type().is_symlink() {
            return Err(FileError::NotFound(target));
        }
        Ok(target)
    }

    /// 外部包文件走独立的包缓存根目录。Typst 对 `@preview/foo:1.2.3`
    /// 会构造带 package spec 的 FileId，不能用 `path_for` 的内容根解析。
    fn package_path(&self, id: FileId) -> FileResult<PathBuf> {
        let spec = id.package().ok_or_else(|| {
            FileError::NotFound(id.vpath().as_rooted_path().to_path_buf())
        })?;
        let root = self
            .package_root
            .canonicalize()
            .map_err(|_| FileError::NotFound(self.package_root.clone()))?;
        let package_dir = root
            .join(spec.namespace.as_str())
            .join(spec.name.as_str())
            .join(spec.version.to_string());
        let path = id
            .vpath()
            .resolve(&package_dir)
            .ok_or_else(|| FileError::NotFound(id.vpath().as_rooted_path().to_path_buf()))?;
        let parent = path.parent().unwrap_or(&package_dir);
        let real_parent = parent
            .canonicalize()
            .map_err(|_| FileError::NotFound(path.clone()))?;
        if !real_parent.starts_with(&package_dir) {
            return Err(FileError::NotFound(path));
        }
        let target = real_parent.join(path.file_name().unwrap_or_default());
        let meta = std::fs::symlink_metadata(&target)
            .map_err(|_| FileError::NotFound(target.clone()))?;
        if meta.file_type().is_symlink() {
            return Err(FileError::NotFound(target));
        }
        // 防止包缓存里出现版本目录穿越。
        if !target.starts_with(&package_dir) {
            return Err(FileError::NotFound(target));
        }
        Ok(target)
    }
}

impl World for TypstWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main {
            return Ok(self.source.clone());
        }
        if let Some(data) = self.memory_file(id) {
            let text = String::from_utf8(data.to_vec())
                .map_err(|_| FileError::NotFound(id.vpath().as_rooted_path().to_path_buf()))?;
            return Ok(Source::new(id, text));
        }
        let path = if id.package().is_some() {
            self.package_path(id)?
        } else {
            self.path_for(id)?
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|_| FileError::NotFound(path.clone()))?;
        Ok(Source::new(id, text))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main {
            return Ok(Bytes::new(self.source.text().as_bytes().to_vec()));
        }
        if let Some(data) = self.memory_file(id) {
            return Ok(Bytes::new(data.to_vec()));
        }
        let path = if id.package().is_some() {
            self.package_path(id)?
        } else {
            self.path_for(id)?
        };
        let data = std::fs::read(&path).map_err(|_| FileError::NotFound(path.clone()))?;
        Ok(Bytes::new(data))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<i64>) -> Option<Datetime> {
        None
    }
}

fn render_typst(
    body: &str,
    root: &Path,
    files: &HashMap<String, Vec<u8>>,
    main_path: &str,
) -> Result<Rendered> {
    let world = TypstWorld::new(body, root, main_path, files);

    let paged = typst::compile::<PagedDocument>(&world)
        .output
        .map_err(|errors| anyhow::anyhow!("Typst 排版失败: {errors:?}"))?;
    let pdf = typst_pdf::pdf(&paged, &typst_pdf::PdfOptions::default())
        .map_err(|errors| anyhow::anyhow!("Typst PDF 导出失败: {:?}", errors))?;

    let html_doc = typst::compile::<HtmlDocument>(&world)
        .output
        .map_err(|errors| anyhow::anyhow!("Typst HTML 编译失败: {errors:?}"))?;
    let full = typst_html::html(&html_doc)
        .map_err(|errors| anyhow::anyhow!("Typst HTML 导出失败: {:?}", errors))?;

    Ok(Rendered {
        html: extract_typst_fragment(&full),
        pdf: Some(pdf),
    })
}

/// Typst 的 HTML 导出是完整文档,这里取出 body 内容并把样式块一并保留,
/// 以便嵌入站点统一的 Askama 布局。
fn extract_typst_fragment(full: &str) -> String {
    let lower = full.to_ascii_lowercase();
    let body_start = lower
        .find("<body")
        .and_then(|i| full[i..].find('>').map(|j| i + j + 1))
        .unwrap_or(0);
    let body_end = lower.rfind("</body>").unwrap_or(full.len());

    let mut out = String::new();
    let mut cursor = 0;
    while let Some(start) = lower[cursor..].find("<style") {
        let start = cursor + start;
        let Some(end) = lower[start..].find("</style>") else {
            break;
        };
        let end = start + end + "</style>".len();
        out.push_str(&full[start..end]);
        out.push('\n');
        cursor = end;
    }
    out.push_str(&full[body_start..body_end]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ContentFormat;
    use std::path::Path;

    #[test]
    fn typst_exports_html_and_pdf() {
        let body = "= Hello\n\nThis is *Typst*.";
        let out = render(ContentFormat::Typst, body, Path::new(".")).unwrap();
        assert!(out.html.contains("Hello"));
        assert!(out.pdf.is_some());
    }

    #[test]
    fn markdown_keeps_raw_html() {
        let body = "# Hi\n\n<script>alert(1)</script>";
        let out = render(ContentFormat::Markdown, body, Path::new(".")).unwrap();
        assert!(out.html.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn wikidot_renders_bold() {
        let out = render(ContentFormat::Wikidot, "**bold**", Path::new(".")).unwrap();
        assert!(out.html.contains("<strong>bold</strong>"), "{}", out.html);
    }

    #[test]
    fn wikidot_include_uses_prefetched_pages() {
        let mut includes = HashMap::new();
        includes.insert(
            "component:header".to_string(),
            "included **text**".to_string(),
        );
        let ctx = DocContext {
            page: "test",
            title: "Test",
            includes: &includes,
        };
        let out = render_doc(
            ContentFormat::Wikidot,
            "[[include component:header]]\n\nafter",
            Path::new("."),
            &ctx,
        )
        .unwrap();
        assert!(out.html.contains("included"), "{}", out.html);
        assert!(out.html.contains("after"), "{}", out.html);
    }

    /// 内容目录里的符号链接不能成为读取 root 之外文件的口子。
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escaping_content_root() {
        use std::os::unix::fs::symlink;
        use typst::diag::FileError;

        let base = std::env::temp_dir().join(format!("kych-net-symlink-{}", std::process::id()));
        let content = base.join("content");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&content).expect("创建临时目录");
        std::fs::write(base.join("secret.txt"), "top secret").expect("写入 root 外的文件");
        symlink(base.join("secret.txt"), content.join("link.txt")).expect("创建符号链接");
        std::fs::write(content.join("normal.txt"), "fine").expect("写入普通文件");

        let world = TypstWorld::new("", &content, "main.typ", &HashMap::new());
        // 普通文件仍然可以解析。
        let ok = world.path_for(FileId::new(None, VirtualPath::new("normal.txt")));
        assert!(ok.is_ok(), "普通文件应通过: {ok:?}");
        // 指向 root 外的符号链接必须被拒绝，而不是被 read 跟进去。
        let link = world.path_for(FileId::new(None, VirtualPath::new("link.txt")));
        assert!(
            matches!(link, Err(FileError::NotFound(_))),
            "符号链接应被拒绝: {link:?}"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
