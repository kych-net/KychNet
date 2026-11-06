use anyhow::Result;
use pulldown_cmark::{html, Options, Parser};
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

pub fn render(format: ContentFormat, body: &str, root: &Path) -> Result<Rendered> {
    match format {
        ContentFormat::Markdown => Ok(Rendered {
            html: render_markdown(body),
            pdf: None,
        }),
        ContentFormat::Html => Ok(Rendered {
            html: body.to_string(),
            pdf: None,
        }),
        ContentFormat::Typst => render_typst(body, root),
    }
}

fn render_markdown(body: &str) -> String {
    let mut output = String::new();
    let parser = Parser::new_ext(body, Options::all());
    html::push_html(&mut output, parser);
    output
}

struct TypstWorld {
    root: PathBuf,
    main: FileId,
    source: Source,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    library: LazyHash<Library>,
}

impl TypstWorld {
    fn new(body: &str, root: &Path) -> Self {
        let main = FileId::new(None, VirtualPath::new("main.typ"));
        let source = Source::new(main, body.to_string());

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
            main,
            source,
            book: LazyHash::new(book),
            fonts,
            library: LazyHash::new(library),
        }
    }

    fn path_for(&self, id: FileId) -> PathBuf {
        self.root.join(id.vpath().as_rooted_path())
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
        let path = self.path_for(id);
        let text = std::fs::read_to_string(&path).map_err(|_| FileError::NotFound(path))?;
        Ok(Source::new(id, text))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main {
            return Ok(Bytes::new(self.source.text().as_bytes().to_vec()));
        }
        let path = self.path_for(id);
        let data = std::fs::read(&path).map_err(|_| FileError::NotFound(path))?;
        Ok(Bytes::new(data))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<i64>) -> Option<Datetime> {
        None
    }
}

fn render_typst(body: &str, root: &Path) -> Result<Rendered> {
    let world = TypstWorld::new(body, root);

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
}
