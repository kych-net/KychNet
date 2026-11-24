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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use sha2::{Digest, Sha256};
use typst::diag::{FileError, FileResult};
use std::sync::{Arc, OnceLock};
use typst::foundations::{Bytes, Datetime};
use typst::LibraryExt;
use typst_html::HtmlDocument;
use typst_layout::PagedDocument;
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Feature, Library, World};

use crate::models::ContentFormat;

/// 一个项目的文件快照：路径 → 内容。
///
/// 用 `Arc` + `Bytes` 两层共享：同一项目下所有页面复用同一份快照，
/// 每个文件的内容也不在页面之间复制（`Bytes` 克隆只是引用计数）。
pub type ProjectFiles = Arc<HashMap<String, Bytes>>;

/// 计算文件快照的内容摘要（路径 + 内容），供渲染缓存键复用。
///
/// 之前每个页面在查缓存前都要对全项目文件排序并逐字节哈希一遍；
/// 一个项目几十个页面就是几十次重复计算。改为每个项目算一次。
pub fn files_digest(files: &ProjectFiles) -> String {
    let mut paths: Vec<&String> = files.keys().collect();
    paths.sort();
    let mut h = Sha256::new();
    for path in paths {
        h.update(path.as_bytes());
        h.update(b"\0");
        h.update(&files[path]);
        h.update(b"\0");
    }
    hex::encode(h.finalize())
}

/// 计算某个 Typst 页面**实际依赖**的文件摘要。
///
/// Typst 源里的图片/包含是通过字面量路径引用的（`#image("/图片/x.svg")`），
/// 因此可以只对这些文件求摘要：改一张没有被本页引用的图时，本页缓存不再失效。
/// 依赖集合沿 include/import 链递归展开：被包含的 .typ 自己引用的文件也算依赖。
///
/// 正确性优先：只要发现无法静态确定的写法（拼接路径、变量路径、函数别名、
/// eval 等），或一个引用都没找到，就回退到全项目摘要——宁可多失效，
/// 不能命中旧内容。
pub fn page_files_digest(body: &str, files: &ProjectFiles) -> String {
    let Some(refs) = collect_refs_recursive(body, files) else {
        return files_digest(files);
    };
    if refs.is_empty() {
        return files_digest(files);
    }
    let mut h = Sha256::new();
    h.update(b"refs\0");
    for path in &refs {
        h.update(path.as_bytes());
        h.update(b"\0");
        match files.get(path) {
            Some(data) => h.update(data),
            // 引用了项目外的文件（内置包等）：不影响本页内容的可变性。
            None => h.update(b"external"),
        }
        h.update(b"\0");
    }
    hex::encode(h.finalize())
}

/// 从页面正文出发，沿 include/import 的源文件链递归收集全部字面量依赖。
///
/// 任何一环无法静态解析（返回 None）都整体回退；被包含的源文件不在项目
/// 快照里（落在磁盘内容目录）时，它的依赖无法在这里展开，同样回退——
/// 磁盘文件本身的变化由缓存键里的内容目录快照覆盖，但它再引用的项目文件
/// 不在那份快照里，不能漏。`@preview/...` 包文件按版本固定、不可变，
/// 且读不到项目文件，无需展开。
fn collect_refs_recursive(body: &str, files: &ProjectFiles) -> Option<Vec<String>> {
    let mut all: Vec<String> = Vec::new();
    // (待展开的源文件路径, 深度)。include 渲染上限 8 层，import 链给到 16 层。
    let mut queue: Vec<(String, usize)> = Vec::new();
    let push_refs = |refs: Vec<(String, bool)>,
                         depth: usize,
                         all: &mut Vec<String>,
                         queue: &mut Vec<(String, usize)>|
     -> Option<()> {
        for (path, is_source) in refs {
            if !all.iter().any(|existing| existing == &path) {
                all.push(path.clone());
            }
            if !is_source || path.starts_with('@') {
                continue;
            }
            if depth >= 16 {
                return None;
            }
            match files.get(&path) {
                Some(_) => queue.push((path, depth + 1)),
                // 源文件在磁盘上，依赖无法展开，整体回退。
                None => return None,
            }
        }
        Some(())
    };
    push_refs(collect_literal_refs(body)?, 0, &mut all, &mut queue)?;
    while let Some((path, depth)) = queue.pop() {
        let data = files.get(&path)?;
        let text = std::str::from_utf8(data).ok()?;
        push_refs(collect_literal_refs(text)?, depth, &mut all, &mut queue)?;
    }
    all.sort();
    Some(all)
}

/// 会读取文件的 Typst 内置函数/关键字。任何一处出现都必须能被静态解析，
/// 否则整页回退到全项目摘要。
const FILE_READERS: &[&str] = &[
    "image", "read", "csv", "json", "yaml", "xml", "cbor", "toml", "plugin", "bibliography", "eval",
];
const SOURCE_READERS: &[&str] = &["include", "import"];

/// 提取源码里所有读文件写法的字面量路径，返回 (路径, 是否为 include/import 源文件)。
///
/// 全部按字节扫描：源码是 UTF-8，模式本身是 ASCII，用字节偏移匹配既能
/// 避免多字节字符的切片 panic，也不需要为解析引入额外依赖。
///
/// 与旧版只认识 `#image(` / `#include ` / `#import ` 三个前缀不同，这里按
/// **标识符**扫描全部读文件入口（含代码模式里的裸 `image(...)`、`#read(...)`、
/// `#let f = image("x")` 等），并覆盖 csv/json/yaml/xml/cbor/toml/plugin/
/// bibliography。识别到标识符但后面不是可静态解析的字面量调用（变量路径、
/// 函数别名如 `#let f = image`、把函数当参数传递、eval）一律返回 None，
/// 由调用方回退到全项目摘要。正文里偶然出现这些英文单词也会触发回退——
/// 这是「宁可多失效」的明确取舍。
fn collect_literal_refs(body: &str) -> Option<Vec<(String, bool)>> {
    let bytes = body.as_bytes();
    let mut refs: Vec<(String, bool)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // 找下一个 ASCII 标识符起点；非 ASCII 字节视为标识符的一部分
        // （Typst 标识符允许 Unicode，`中文image` 是一个整体，不能拆出 image）。
        if !bytes[i].is_ascii_alphabetic() && bytes[i] != b'_' {
            i += 1;
            continue;
        }
        if i > 0 && is_ident_byte(bytes[i - 1]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident_byte(bytes[i]) {
            i += 1;
        }
        let word = &body[start..i];
        let is_source = SOURCE_READERS.contains(&word);
        if !is_source && !FILE_READERS.contains(&word) {
            continue;
        }
        if word == "eval" {
            return None;
        }
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if !is_source {
            // 函数调用：必须是 `name("字面量"...)`。别名（`= image`）、
            // 传参（`map(image, ...)`）、动态路径都在这里回退。
            if j >= bytes.len() || bytes[j] != b'(' {
                return None;
            }
            j += 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
        }
        if j >= bytes.len() || bytes[j] != b'"' {
            return None;
        }
        j += 1;
        let str_start = j;
        while j < bytes.len() && bytes[j] != b'"' {
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        let raw = &body[str_start..j];
        j += 1;
        if !is_source {
            let mut k = j;
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            // 字面量之后应当是 `)` 或 `,`（还可能有命名参数）。
            if k >= bytes.len() || (bytes[k] != b')' && bytes[k] != b',') {
                return None;
            }
        } else {
            // include/import 后应当是行尾、空白、注释或 `:`
            let rest = body[j..].trim_start();
            if !rest.is_empty()
                && !rest.starts_with('\n')
                && !rest.starts_with("//")
                && !rest.starts_with(':')
            {
                return None;
            }
        }
        i = j;
        let path = raw.trim_start_matches('/').replace('\\', "/");
        if path.is_empty() || path.contains(' ') {
            return None;
        }
        if !refs.iter().any(|(existing, _)| existing == &path) {
            refs.push((path, is_source));
        }
    }
    Some(refs)
}

/// Typst 标识符字节：ASCII 字母/数字/`-`/`_`，以及所有非 ASCII 字节
/// （Unicode 标识符的一部分）。
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b >= 0x80
}

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
    render_doc_with_project(format, body, root, ctx, &Arc::new(HashMap::new()), "main.typ", "")
}

/// 渲染项目文件。`files` 是项目根下的完整文件快照，Typst 的相对 include
/// 会优先从这里读取；读不到再回退到磁盘上的全站内容目录。
pub fn render_doc_with_project(
    format: ContentFormat,
    body: &str,
    root: &Path,
    ctx: &DocContext,
    files: &ProjectFiles,
    main_path: &str,
    // 项目在站点里的 URL 基址（如 `/项目/地狱之下`）；空串表示非项目页。
    // Typst 内嵌图片若能在项目文件里找到同内容文件，就引用它而不是另存。
    asset_base: &str,
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
        ContentFormat::Typst => render_typst(body, root, files, main_path, asset_base),
        ContentFormat::Wikidot => render_wikidot(body, ctx),
        // SVG 是原样输出的静态资源，不套 HTML 模板。
        ContentFormat::Svg => Ok(Rendered {
            html: body.to_string(),
            pdf: None,
        }),
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

/// 找行内公式的闭合 `$`：不跨行，且闭合符前不能是空白（避免把 `$5 和 $6` 当成公式）。
fn find_inline_math_end(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '\n' => return None,
            '$' => {
                let has_body = i > start;
                let closed = has_body && !chars[i - 1].is_whitespace();
                return if closed { Some(i) } else { None };
            }
            _ => i += 1,
        }
    }
    None
}

/// 找块级公式的闭合 `$$`（允许跨行）。
fn find_block_math_end(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == '$' && chars[i + 1] == '$' {
            return if i > start { Some(i) } else { None };
        }
        i += 1;
    }
    None
}

/// 找 `\)` / `\]` 的闭合位置（跳过转义字符）。
fn find_escaped_math_end(chars: &[char], start: usize, close: char) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == '\\' && chars[i + 1] == close {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 只替换完整的命令：`\le` 不能碰 `\leq` 里的前缀。
fn replace_command(tex: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let mut rest = tex;
    while let Some(pos) = rest.find(from) {
        let after = &rest[pos + from.len()..];
        let longer = after.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false);
        out.push_str(&rest[..pos]);
        out.push_str(if longer { from } else { to });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// 把 latex2mathml 不认的常见写法归一化，否则整条公式会退化成错误文本。
///
/// - `\dfrac` / `\tfrac` / `\cfrac` 都是 amsmath 的分数别名，统一成 `\frac`
/// - `cases` / `aligned` 环境未被支持，用 `matrix` 近似（cases 补左大括号）
/// - `\displaystyle` 等纯排版命令直接去掉
fn normalize_latex(tex: &str) -> String {
    let mut out = tex.to_string();
    // latex2mathml 只认 \leq / \geq，\le / \ge 会被当成未知命令
    out = replace_command(&out, "\\le", "\\leq");
    out = replace_command(&out, "\\ge", "\\geq");
    for alias in ["\\dfrac", "\\tfrac", "\\cfrac"] {
        out = out.replace(alias, "\\frac");
    }
    out = out.replace("\\begin{cases}", "\\left\\{\\begin{matrix}");
    out = out.replace("\\end{cases}", "\\end{matrix}\\right.");
    out = out.replace("\\begin{aligned}", "\\begin{matrix}");
    out = out.replace("\\end{aligned}", "\\end{matrix}");
    for cmd in [
        "\\displaystyle",
        "\\textstyle",
        "\\scriptstyle",
        "\\scriptscriptstyle",
        "\\limits",
        "\\nolimits",
    ] {
        out = out.replace(cmd, "");
    }
    out
}

/// 归一化后转换成 MathML；latex2mathml 对未知命令不总是返回错误，
/// 偶尔会输出 `[PARSE ERROR: ...]` 文本，这种也算失败，让调用方回退原文。
fn convert_math(tex: &str, style: latex2mathml::DisplayStyle) -> Option<String> {
    let normalized = normalize_latex(tex.trim());
    match latex2mathml::latex_to_mathml(&normalized, style) {
        Ok(mathml) if !mathml.contains("PARSE ERROR") => Some(mathml),
        _ => None,
    }
}

/// 把正文里的 LaTeX 公式转成 MathML。
///
/// 支持 `$…$` / `\(…\)`（行内）与 `$$…$$` / `\[…\]`（块级）。
/// 判定偏保守：`\$` 视为转义，行内公式不跨行且首尾非空白，避免把价格之类的
/// 单个美元符号误当公式；单个公式解析失败只保留原文，不影响整篇渲染。
fn embed_latex_math(body: &str) -> String {
    use latex2mathml::{latex_to_mathml, DisplayStyle};

    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len() + 64);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if next == '(' || next == '[' {
                let close = if next == '(' { ')' } else { ']' };
                if let Some(end) = find_escaped_math_end(&chars, i + 2, close) {
                    let tex: String = chars[i + 2..end].iter().collect();
                    let style = if next == '(' { DisplayStyle::Inline } else { DisplayStyle::Block };
                    if let Some(mathml) = convert_math(&tex, style) {
                        if next == '[' {
                            out.push_str("\n\n");
                            out.push_str(&mathml);
                            out.push_str("\n\n");
                        } else {
                            out.push_str(&mathml);
                        }
                        i = end + 2;
                        continue;
                    }
                }
            }
            out.push(c);
            out.push(next);
            i += 2;
            continue;
        }
        if c == '$' {
            let block = i + 1 < chars.len() && chars[i + 1] == '$';
            let open = if block { 2 } else { 1 };
            let found = if block {
                find_block_math_end(&chars, i + open).map(|end| (end, open))
            } else {
                find_inline_math_end(&chars, i + open).map(|end| (end, open))
            };
            if let Some((end, open_len)) = found {
                let tex: String = chars[i + open_len..end].iter().collect();
                let style = if block { DisplayStyle::Block } else { DisplayStyle::Inline };
                if let Some(mathml) = convert_math(&tex, style) {
                    if block {
                        out.push_str("\n\n");
                        out.push_str(&mathml);
                        out.push_str("\n\n");
                    } else {
                        out.push_str(&mathml);
                    }
                    i = end + open_len;
                    continue;
                }
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn render_markdown(body: &str) -> String {
    // 先把 LaTeX 公式落成 MathML（浏览器原生渲染，静态站不需要额外 JS/字体），
    // 再交给 Markdown 解析：MathML 是内嵌 HTML，会被原样保留。
    let with_math = embed_latex_math(body);
    // pulldown-cmark 0.12 自带数学扩展，会把没转换成功的 `$…$` 包成
    // `<span class="math math-inline">` 半成品；公式由我们自己转 MathML，这里关掉它。
    let mut options = Options::all();
    options.remove(Options::ENABLE_MATH);
    let mut output = String::new();
    let parser = Parser::new_ext(&with_math, options);
    html::push_html(&mut output, parser);
    output
}

/// 所有文档共用的 Typst 环境：字体索引 + Library。
///
/// 之前每渲染一篇文档都要遍历全部内置字体建一次 `FontBook`、再建一次
/// `Library`。这些内容与文档无关，进程内建一次即可——站点有几十个 Typst
/// 页面时，这部分是纯重复开销。
struct TypstEnv {
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    library: LazyHash<Library>,
}

fn typst_env() -> &'static TypstEnv {
    static ENV: OnceLock<TypstEnv> = OnceLock::new();
    ENV.get_or_init(|| {
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
        TypstEnv {
            book: LazyHash::new(book),
            fonts,
            library: LazyHash::new(library),
        }
    })
}

struct TypstWorld {
    root: PathBuf,
    /// `@preview/...` 外部包缓存根目录；结构为 <namespace>/<name>/<version>/。
    package_root: PathBuf,
    main: FileId,
    source: Source,
    /// 项目仓库文件，key 是相对项目根的路径。与同项目的其他页面共享。
    files: ProjectFiles,
    env: &'static TypstEnv,
}

impl TypstWorld {
    fn new(
        body: &str,
        root: &Path,
        main_path: &str,
        files: &ProjectFiles,
    ) -> Self {
        let main = FileId::new(RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new(main_path).expect("主文件路径必须合法"),
        ));
        let source = Source::new(main, body.to_string());
        let package_root = std::env::var_os("TYPST_PACKAGE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("typst-packages"));

        Self {
            root: root.to_path_buf(),
            package_root,
            main,
            source,
            files: Arc::clone(files),
            env: typst_env(),
        }
    }

    /// 项目仓库文件优先于磁盘内容目录。
    fn memory_file(&self, id: FileId) -> Option<&Bytes> {
        // 包文件不能落到项目仓库的同名路径上（例如 typst.toml）。
        if matches!(id.root(), VirtualRoot::Package(_)) {
            return None;
        }
        self.files.get(id.vpath().get_without_slash())
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
            .realize(&self.root)
            .map_err(|_| FileError::NotFound(PathBuf::from(id.vpath().get_with_slash())))?;

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

    /// 外部包缺失时按需从 Typst 官方 registry 下载到缓存。
    /// 只允许 registry 包名/版本，不接受任意 URL；下载和缓存都加进程级锁。
    fn ensure_package(&self, spec: &typst::syntax::package::PackageSpec) -> anyhow::Result<()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let package_dir = self
            .package_root
            .join(spec.namespace.as_str())
            .join(spec.name.as_str())
            .join(spec.version.to_string());
        let manifest = package_dir.join("typst.toml");
        if manifest.is_file() {
            return Ok(());
        }
        std::fs::create_dir_all(
            package_dir
                .parent()
                .ok_or_else(|| anyhow::anyhow!("包缓存路径没有父目录"))?,
        )?;
        let url = format!(
            "https://packages.typst.org/{}/{}-{}.tar.gz",
            spec.namespace, spec.name, spec.version
        );
        tracing::info!(%url, "下载 Typst 包到缓存");
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(30))
            .build();
        let response = agent
            .get(&url)
            .call()
            .map_err(|error| anyhow::anyhow!("下载 {url} 失败: {error}"))?;
        if response.status() != 200 {
            anyhow::bail!("下载 {url} 返回 HTTP {}", response.status());
        }
        const MAX_PACKAGE_BYTES: u64 = 32 * 1024 * 1024;
        let mut archive_bytes = Vec::new();
        let mut reader = response.into_reader();
        (&mut reader)
            .take(MAX_PACKAGE_BYTES + 1)
            .read_to_end(&mut archive_bytes)?;
        if archive_bytes.len() as u64 > MAX_PACKAGE_BYTES {
            anyhow::bail!("Typst 包 {url} 超过 32 MiB");
        }
        let temp_dir = self
            .package_root
            .join(format!(".download-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir)?;
        let result = (|| -> anyhow::Result<()> {
            let decoder = flate2::read::GzDecoder::new(archive_bytes.as_slice());
            let mut archive = tar::Archive::new(decoder);
            for entry in archive.entries()? {
                let mut entry = entry?;
                let path = entry.path()?.into_owned();
                if path.components().any(|part| {
                    !matches!(
                        part,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                }) {
                    anyhow::bail!("Typst 包包含非法路径: {}", path.display());
                }
                if entry.header().entry_type().is_symlink()
                    || entry.header().entry_type().is_hard_link()
                {
                    anyhow::bail!("Typst 包包含链接文件，拒绝解压");
                }
                entry.unpack_in(&temp_dir)?;
            }
            // 官方包根目录是 ./typst.toml；解压后临时目录就是包根。
            if !temp_dir.join("typst.toml").is_file() {
                anyhow::bail!("Typst 包缺少 typst.toml");
            }
            std::fs::rename(&temp_dir, &package_dir)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&temp_dir);
        }
        result
    }

    /// 外部包文件走独立的包缓存根目录。Typst 对 `@preview/foo:1.2.3`
    /// 会构造带 package spec 的 FileId，不能用 `path_for` 的内容根解析。
    fn package_path(&self, id: FileId) -> FileResult<PathBuf> {
        let VirtualRoot::Package(spec) = id.root() else {
            return Err(FileError::NotFound(PathBuf::from(id.vpath().get_with_slash())));
        };
        let root = self
            .package_root
            .canonicalize()
            .map_err(|_| FileError::NotFound(self.package_root.clone()))?;
        if let Err(error) = self.ensure_package(spec) {
            tracing::warn!(%error, "Typst 包缓存准备失败");
            return Err(FileError::NotFound(id.vpath().as_rooted_path().to_path_buf()));
        }
        let package_dir = root
            .join(spec.namespace.as_str())
            .join(spec.name.as_str())
            .join(spec.version.to_string());
        let path = id
            .vpath()
            .resolve(&package_dir)
            .ok_or_else(|| FileError::NotFound(PathBuf::from(id.vpath().get_with_slash())))?;
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
        &self.env.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.env.book
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
                .map_err(|_| FileError::NotFound(PathBuf::from(id.vpath().get_with_slash())))?;
            return Ok(Source::new(id, text));
        }
        let path = if matches!(id.root(), VirtualRoot::Package(_)) {
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
            // Bytes 克隆是引用计数，不再复制文件内容。
            return Ok(data.clone());
        }
        let path = if matches!(id.root(), VirtualRoot::Package(_)) {
            self.package_path(id)?
        } else {
            self.path_for(id)?
        };
        let data = std::fs::read(&path).map_err(|_| FileError::NotFound(path.clone()))?;
        Ok(Bytes::new(data))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.env.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        None
    }
}

fn render_typst(
    body: &str,
    root: &Path,
    files: &ProjectFiles,
    main_path: &str,
    asset_base: &str,
) -> Result<Rendered> {
    let world = TypstWorld::new(body, root, main_path, files);

    let paged = typst::compile::<PagedDocument>(&world)
        .output
        .map_err(|errors| anyhow::anyhow!("Typst 排版失败: {errors:?}"))?;
    let pdf = typst_pdf::pdf(&paged, &typst_pdf::PdfOptions::default())
        .map_err(|errors| anyhow::anyhow!("Typst PDF 导出失败: {:?}", errors))?;

    let mut html_doc = typst::compile::<HtmlDocument>(&world)
        .output
        .map_err(|errors| anyhow::anyhow!("Typst HTML 编译失败: {errors:?}"))?;
    // 在序列化之前把 `image` 元素内嵌的 data URI 换回项目里的原路径。
    // Typst 的导出规则（IMAGE_RULE）无条件调 `to_base64_url()`，且
    // HtmlOptions 没有开关；直接在 DOM 上改属性比事后扫字符串更可靠。
    rewrite_dom_images(html_doc.root_mut(), files, asset_base);
    let full = typst_html::html(&html_doc, &typst_html::HtmlOptions::default())
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
mod ref_digest_tests {
    use super::*;
    use std::collections::HashMap;

    fn files(pairs: &[(&str, &str)]) -> ProjectFiles {
        Arc::new(
            pairs
                .iter()
                .map(|(p, d)| (p.to_string(), Bytes::new(d.as_bytes().to_vec())))
                .collect::<HashMap<_, _>>(),
        )
    }

    #[test]
    fn collects_literal_refs() {
        let src = r#"
= 标题
#image("/图片/地图.svg", width: 100%)
#image("/图片/分区图.svg", format: "svg")
#include "另一页.typ"
"#;
        let refs = collect_literal_refs(src).expect("应当能解析");
        assert_eq!(
            refs,
            vec![
                ("图片/地图.svg".to_string(), false),
                ("图片/分区图.svg".to_string(), false),
                ("另一页.typ".to_string(), true),
            ]
        );
    }

    #[test]
    fn dynamic_paths_fall_back() {
        let src = "#let n = \"x\"\n#image(\"/图片/\" + n + \".svg\")";
        assert!(collect_literal_refs(src).is_none(), "动态路径必须回退");
    }

    /// 代码模式里的裸调用与 #read 也必须被识别，不能漏依赖。
    #[test]
    fn bare_calls_and_read_are_collected() {
        let src = "#let x = {\n  image(\"图/a.svg\")\n}\n#read(\"数据.txt\")";
        let refs = collect_literal_refs(src).expect("应当能解析");
        assert_eq!(
            refs,
            vec![
                ("图/a.svg".to_string(), false),
                ("数据.txt".to_string(), false),
            ]
        );
    }

    /// 函数别名 / 把读文件函数当值传递：无法静态确定，必须回退。
    #[test]
    fn function_alias_falls_back() {
        assert!(collect_literal_refs("#let f = image\n#f(\"a.svg\")").is_none());
        assert!(collect_literal_refs("#let arr = (image, read)").is_none());
        assert!(collect_literal_refs("#eval(\"#include \\\"a.typ\\\"\")").is_none());
    }

    /// include/import 链上的间接依赖也要进摘要：改被包含页引用的图必须失效。
    #[test]
    fn transitive_deps_change_digest() {
        let body = "#include \"章节.typ\"";
        let base = files(&[
            ("章节.typ", "#image(\"图/a.svg\")"),
            ("图/a.svg", "<svg>a</svg>"),
            ("图/b.svg", "<svg>b</svg>"),
        ]);
        let touched = files(&[
            ("章节.typ", "#image(\"图/a.svg\")"),
            ("图/a.svg", "<svg>A2</svg>"),
            ("图/b.svg", "<svg>b</svg>"),
        ]);
        let untouched = files(&[
            ("章节.typ", "#image(\"图/a.svg\")"),
            ("图/a.svg", "<svg>a</svg>"),
            ("图/b.svg", "<svg>CHANGED</svg>"),
        ]);
        assert_ne!(page_files_digest(body, &base), page_files_digest(body, &touched));
        assert_eq!(page_files_digest(body, &base), page_files_digest(body, &untouched));
    }

    /// 被包含的源文件不在项目快照里时无法展开依赖，必须回退全项目摘要。
    #[test]
    fn disk_source_include_falls_back() {
        let body = "#include \"磁盘页.typ\"";
        let f = files(&[("图/a.svg", "1")]);
        assert_eq!(page_files_digest(body, &f), files_digest(&f));
    }

    #[test]
    fn digest_changes_only_for_referenced_files() {
        let body = "#image(\"/图/a.svg\")";
        let base = files(&[("图/a.svg", "<svg>a</svg>"), ("图/b.svg", "<svg>b</svg>")]);
        let untouched = files(&[("图/a.svg", "<svg>a</svg>"), ("图/b.svg", "<svg>CHANGED</svg>")]);
        let touched = files(&[("图/a.svg", "<svg>A2</svg>"), ("图/b.svg", "<svg>b</svg>")]);

        // 改没被引用的 b：本页摘要不变（缓存仍可命中）。
        assert_eq!(
            page_files_digest(body, &base),
            page_files_digest(body, &untouched)
        );
        // 改被引用的 a：摘要必须变化。
        assert_ne!(
            page_files_digest(body, &base),
            page_files_digest(body, &touched)
        );
    }

    #[test]
    fn no_refs_falls_back_to_whole_project() {
        let body = "= 纯文字，没有引用";
        let a = files(&[("x.svg", "1")]);
        let b = files(&[("x.svg", "2")]);
        assert_eq!(page_files_digest(body, &a), files_digest(&a));
        assert_ne!(page_files_digest(body, &a), page_files_digest(body, &b));
    }
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
    fn markdown_renders_inline_latex_as_mathml() {
        let out = render(ContentFormat::Markdown, "质能方程 $E = mc^2$ 很有名", Path::new(".")).unwrap();
        assert!(out.html.contains("<math"), "{}", out.html);
        assert!(out.html.contains("display=\"inline\""), "{}", out.html);
        // 公式两侧的普通文字不能被吞掉
        assert!(out.html.contains("质能方程"), "{}", out.html);
        assert!(out.html.contains("很有名"), "{}", out.html);
    }

    #[test]
    fn markdown_renders_block_latex_as_mathml() {
        let out = render(
            ContentFormat::Markdown,
            "行间公式：\n\n$$\n\\int_0^1 x^2 \\mathrm{d}x = \\frac{1}{3}\n$$\n\n后文",
            Path::new("."),
        )
        .unwrap();
        assert!(out.html.contains("display=\"block\""), "{}", out.html);
        assert!(out.html.contains("后文"), "{}", out.html);
    }

    #[test]
    fn markdown_renders_cases_environment() {
        // cases 环境 `latex2mathml` 不认，需归一化成带大括号的 matrix
        let body = r"$p = \begin{cases}1, & V > C \\ \dfrac{V}{C}, & V \le C\end{cases}$";
        let out = render(ContentFormat::Markdown, body, Path::new(".")).unwrap();
        assert!(out.html.contains("<math"), "{}", out.html);
        assert!(!out.html.contains("PARSE ERROR"), "{}", out.html);
        assert!(!out.html.contains("math-inline"), "{}", out.html);
    }

    #[test]
    fn markdown_failed_latex_keeps_original_text() {
        let body = r"坏公式 $\notacommand{xx}$ 结束";
        let out = render(ContentFormat::Markdown, body, Path::new(".")).unwrap();
        assert!(!out.html.contains("PARSE ERROR"), "{}", out.html);
        assert!(!out.html.contains("math-inline"), "{}", out.html);
        assert!(out.html.contains("结束"), "{}", out.html);
    }

    #[test]
    fn markdown_ignores_isolated_dollar_signs() {
        // 价格里的美元符号不能当公式，也不能让整篇渲染失败
        let out = render(ContentFormat::Markdown, "苹果 $5 一个，香蕉 $6 一根", Path::new(".")).unwrap();
        assert!(!out.html.contains("<math"), "{}", out.html);
        assert!(out.html.contains("$5"), "{}", out.html);
    }

    #[test]
    fn markdown_keeps_escaped_dollar_literal() {
        let out = render(ContentFormat::Markdown, r"路径是 \$DSH_HOME 目录", Path::new(".")).unwrap();
        assert!(!out.html.contains("<math"), "{}", out.html);
        assert!(out.html.contains("$DSH_HOME"), "{}", out.html);
    }

    #[test]
    fn markdown_invalid_latex_falls_back_to_text() {
        // 解析不了的公式保留原文，不让整页失败
        let out = render(ContentFormat::Markdown, "坏公式 $\\undefinedmacro{$ 结束", Path::new(".")).unwrap();
        assert!(out.html.contains("结束"), "{}", out.html);
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

        let world = TypstWorld::new("", &content, "main.typ", &Arc::new(HashMap::new()));
        // 普通文件仍然可以解析。
        let ok = world.path_for(FileId::new(RootedPath::new(VirtualRoot::Project, VirtualPath::new("normal.txt").unwrap())));
        assert!(ok.is_ok(), "普通文件应通过: {ok:?}");
        // 指向 root 外的符号链接必须被拒绝，而不是被 read 跟进去。
        let link = world.path_for(FileId::new(RootedPath::new(VirtualRoot::Project, VirtualPath::new("link.txt").unwrap())));
        assert!(
            matches!(link, Err(FileError::NotFound(_))),
            "符号链接应被拒绝: {link:?}"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}

/// 遍历 Typst 的 HTML DOM，把 `<img src="data:image/...">` 换回**项目里原有的图片路径**。
///
/// Typst 导出 HTML 时会把 `#image("图片/x.svg")` 内嵌成 Base64。这里的做法是：
/// 解码 data URI → 按内容哈希在项目文件里反查原始路径 → `src` 直接指向
/// `${asset_base}/图片/x.svg`。这样 HTML 引用的就是原文档引用的那张图，
/// 不需要额外落盘，也不会出现 media-* 这种重复副本。
///
/// 反查不到的（Typst 运行时绘制出来的图形）才落成 `media-<hash>.<ext>`，
/// 作为返回值交给调用方写盘。
fn rewrite_dom_images(
    root: &mut typst_html::HtmlElement,
    files: &ProjectFiles,
    asset_base: &str,
) {
    // 内容哈希 → 项目内路径。项目文件通常只有几十到几百个，一次建表足够便宜。
    let mut by_digest: HashMap<[u8; 32], String> = HashMap::with_capacity(files.len());
    for (path, data) in files.iter() {
        by_digest.entry(Sha256::digest(data).into()).or_insert_with(|| path.clone());
    }
    visit_html_element(root, &by_digest, asset_base);
}

fn visit_html_element(
    elem: &mut typst_html::HtmlElement,
    by_digest: &HashMap<[u8; 32], String>,
    asset_base: &str,
) {
    if elem.tag == typst_html::tag::img {
        if let Some(src) = elem.attrs.get(typst_html::attr::src).cloned() {
            if let Some((mime, bytes)) = decode_data_uri(&src) {
                let digest: [u8; 32] = Sha256::digest(&bytes).into();
                // 一律改回项目里的原路径；找不到就不动（保留原内嵌数据），
                // 不再另存 media-* 副本——那等于给项目复制一份图片。
                let Some(path) = by_digest.get(&digest) else {
                    return;
                };
                if path.is_empty() {
                    return;
                }
                let replacement = if asset_base.is_empty() {
                    path.clone()
                } else {
                    format!("{asset_base}/{path}")
                };
                if let Some(slot) = elem.attrs.get_mut(typst_html::attr::src) {
                    *slot = replacement.into();
                }
                return;
            }
        }
    }
    for child in elem.children.make_mut().iter_mut() {
        if let typst_html::HtmlNode::Element(inner) = child {
            visit_html_element(inner, by_digest, asset_base);
        }
    }
}

/// 解析 `data:image/<mime>;base64,<payload>`，返回 (mime, 解码后的字节)。
fn decode_data_uri(src: &str) -> Option<(String, Vec<u8>)> {
    use base64::Engine as _;

    let rest = src.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let mime = meta.strip_suffix(";base64")?.to_string();
    if !mime.starts_with("image/") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&rest[comma + 1..])
        .ok()?;
    Some((mime, bytes))
}

fn mime_ext(mime: &str) -> &'static str {
    match mime {
        "image/svg+xml" => "svg",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}


