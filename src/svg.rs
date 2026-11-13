use anyhow::{bail, Context, Result};

/// 内容上限。api.rs 的传输层上限要对它取齐，所以公开出去。
pub const MAX_BYTES: usize = 256 * 1024;

const FORBIDDEN_ELEMENTS: &[&str] = &[
    "script",
    "foreignobject",
    "iframe",
    "embed",
    "object",
    "image",
    "style",
];

/// 动画类元素。它们能把属性（包括 `href`）改成任意值，是 SVG 里最典型的
/// XSS 跳板：`<animate attributeName="href" to="javascript:...">` 不需要任何
/// 事件属性就能触发。头像这种静态图形用不到动画，一律拒绝。
const ANIMATION_ELEMENTS: &[&str] = &[
    "animate",
    "animatetransform",
    "animatecolor",
    "animatemotion",
    "discard",
    "set",
];

const VECTOR_ELEMENTS: &[&str] = &[
    "path", "circle", "ellipse", "rect", "line", "polyline", "polygon", "text", "textpath",
];

/// 校验上传内容确实是矢量 SVG:
/// - 是合法 XML,根元素为 svg
/// - 不含脚本、foreignObject 等可执行内容
/// - 不含内嵌或外链位图
/// - 不含 DTD(实体展开与外部实体)
/// - 不含会改写属性的动画元素
/// - 至少包含一个矢量图形元素
fn contains_dangerous_css(value: &str) -> bool {
    let compact: String = value
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    // WHY: CSS 转义（如 `\75rl(` 解码后就是 `url(`）能绕过下面的字面匹配；
    // 头像 CSS 没有合法转义需求，见到反斜杠直接拒绝。
    if compact.contains('\\') {
        return true;
    }
    if compact.contains("@import") || compact.contains("expression(") {
        return true;
    }
    let mut rest = compact.as_str();
    while let Some(index) = rest.find("url(") {
        let after = &rest[index + 4..];
        // 头像只允许引用文档内 fragment；其它 url() 一律拒绝。
        if !after.starts_with('#') {
            return true;
        }
        rest = &after[1..];
    }
    false
}

fn reject_processing_instructions(text: &str) -> Result<()> {
    let mut rest = text.trim_start();
    if rest.starts_with("<?xml") {
        let Some(end) = rest.find("?>") else {
            bail!("SVG XML 声明未闭合");
        };
        rest = &rest[end + 2..];
    }
    if rest.contains("<?") {
        bail!("SVG 不允许包含处理指令");
    }
    Ok(())
}

pub fn validate_svg(input: &[u8]) -> Result<()> {
    if input.len() > MAX_BYTES {
        bail!("SVG 不能超过 256 KB");
    }
    let text = std::str::from_utf8(input).context("SVG 必须是 UTF-8 文本")?;

    if text.to_ascii_lowercase().contains("data:image/") {
        bail!("不允许内嵌位图,头像必须是纯矢量");
    }

    // roxmltree 不解析 DTD,但 `<!DOCTYPE`/`<!ENTITY` 一出现就说明对方想走的
    // 不是「画个图形」这条路;而且换一家 XML 解析器(或将来加实体支持)就会
    // 变成 XXE。在解析前直接按文本拒掉,成本为零。
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("<!doctype") || lowered.contains("<!entity") {
        bail!("SVG 不允许包含 DTD 或实体声明");
    }
    reject_processing_instructions(text)?;

    let doc = roxmltree::Document::parse(text).context("内容不是合法的 SVG/XML")?;
    let root = doc.root_element();
    if root.tag_name().name().to_ascii_lowercase() != "svg" {
        bail!("根元素必须是 svg");
    }

    let mut has_vector = false;
    for node in doc.descendants() {
        if node.is_pi() {
            bail!("SVG 不允许包含处理指令");
        }
        if !node.is_element() {
            continue;
        }
        let tag = node.tag_name().name().to_ascii_lowercase();
        if FORBIDDEN_ELEMENTS.contains(&tag.as_str()) {
            bail!("SVG 不允许包含 <{tag}> 元素");
        }
        if ANIMATION_ELEMENTS.contains(&tag.as_str()) {
            bail!("SVG 不允许包含 <{tag}> 动画元素");
        }
        if VECTOR_ELEMENTS.contains(&tag.as_str()) {
            has_vector = true;
        }

        for attr in node.attributes() {
            let name = attr.name().to_ascii_lowercase();
            if name.starts_with("on") {
                bail!("SVG 不允许包含事件属性");
            }
            if name == "style" {
                bail!("SVG 不允许包含 style 属性");
            }
            if contains_dangerous_css(attr.value()) {
                bail!("SVG 不允许包含危险 CSS");
            }
            // 浏览器解析 URL 时会忽略 tab / 换行 / 回车等控制字符，
            // 例如 `java&#10;script:alert(1)` 解码后仍会被当作 javascript: 执行。
            // 必须先剔除这些字符再做协议判断，否则前缀检查可被绕过。
            let value: String = attr
                .value()
                .trim()
                .to_ascii_lowercase()
                .chars()
                .filter(|c| !matches!(c, '\t' | '\n' | '\r' | '\0' | ' '))
                .collect();
            if value.starts_with("javascript:") || value.starts_with("data:") {
                bail!("SVG 不允许引用脚本或内嵌数据");
            }
            if value.starts_with("http:") || value.starts_with("https:") {
                bail!("SVG 不允许引用外部资源");
            }
            // 只按协议判断会漏掉 `//evil.com`（协议相对）和相对路径；头像里
            // href 唯一合理的用途是指向本文档内的图元，直接收紧到 fragment。
            if name == "href" && !value.starts_with('#') {
                bail!("SVG 的 href 只能引用文档内的图元");
            }
        }
    }

    if !has_vector {
        bail!("SVG 不包含任何矢量图形");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_vector() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><circle cx="10" cy="10" r="8"/></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_raster_disguised_as_svg() {
        let png = b"\x89PNG\r\n\x1a\nfake";
        assert!(validate_svg(png).is_err());
    }

    #[test]
    fn rejects_embedded_base64_image() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><image href="data:image/png;base64,AAAA"/><rect width="10" height="10"/></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
    }

    #[test]
    fn rejects_script_and_events() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><rect width="10" height="10"/></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script><rect width="10" height="10"/></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
    }

    #[test]
    fn rejects_style_pi_and_dangerous_css() {
        let cases = [
            r#"<?xml-stylesheet href="https://evil.com/x.css"?><svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><style>*{fill:url(https://evil.com/x)}</style><rect width="10" height="10"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10" style="fill:red"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10" fill="url(https://evil.com/x)"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10" fill="url(#ok)"/><animateMotion dur="1s"/></svg>"#,
        ];
        for svg in cases {
            assert!(validate_svg(svg.as_bytes()).is_err(), "应拒绝 {svg}");
        }
    }

    #[test]
    fn rejects_css_escape_bypass() {
        // `\75` 是 'u' 的 CSS 十六进制转义，浏览器解码后 `\75rl(` 就是 url(，
        // 只按字面 "url(" 匹配会被绕过。
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10" fill="\75rl(https://evil.com/x)"/></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
    }

    #[test]
    fn rejects_empty_svg() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
    }

    #[test]
    fn rejects_javascript_url_with_control_chars() {
        // XML 字符实体解码后是 "java\nscript:alert(1)"，浏览器仍会执行；
        // 单纯 starts_with("javascript:") 会被绕过。
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><a href="java&#10;script:alert(1)"><rect width="10" height="10"/></a></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
    }

    #[test]
    fn rejects_doctype_and_entities() {
        let svg = r#"<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd"><svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10"/></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
        let svg = r#"<!DOCTYPE svg [<!ENTITY x "y">]<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10"/></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
    }

    #[test]
    fn rejects_animation_elements() {
        // 不需要任何事件属性，动画本身就能把 href 改成 javascript: URL。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"><a href="#x"><rect width="10" height="10"/><animate attributeName="href" to="javascript:alert(1)"/></a></svg>"##;
        assert!(validate_svg(svg.as_bytes()).is_err());
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10"><set attributeName="onclick" to="alert(1)"/></rect></svg>"#;
        assert!(validate_svg(svg.as_bytes()).is_err());
    }

    #[test]
    fn accepts_internal_use_reference() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><defs><circle id="c" r="5"/></defs><use xlink:href="#c"/></svg>"##;
        assert!(validate_svg(svg.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_protocol_relative_href() {
        // 没有协议前缀，`starts_with("http:")` 之类的判断全都放行，
        // 但浏览器会按当前协议去请求 evil.com。
        for href in ["//evil.com/x.svg", "evil.com/x.svg", "/redirect?to=1"] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg"><a href="{href}"><rect width="10" height="10"/></a></svg>"#
            );
            assert!(validate_svg(svg.as_bytes()).is_err(), "应拒绝 {href}");
        }
    }
}
