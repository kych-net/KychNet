use anyhow::{bail, Context, Result};

const MAX_BYTES: usize = 256 * 1024;

const FORBIDDEN_ELEMENTS: &[&str] = &[
    "script",
    "foreignobject",
    "iframe",
    "embed",
    "object",
    "image",
];

const VECTOR_ELEMENTS: &[&str] = &[
    "path", "circle", "ellipse", "rect", "line", "polyline", "polygon", "text", "textpath",
];

/// 校验上传内容确实是矢量 SVG:
/// - 是合法 XML,根元素为 svg
/// - 不含脚本、foreignObject 等可执行内容
/// - 不含内嵌或外链位图
/// - 至少包含一个矢量图形元素
pub fn validate_svg(input: &[u8]) -> Result<()> {
    if input.len() > MAX_BYTES {
        bail!("SVG 不能超过 256 KB");
    }
    let text = std::str::from_utf8(input).context("SVG 必须是 UTF-8 文本")?;

    if text.contains("data:image/") {
        bail!("不允许内嵌位图,头像必须是纯矢量");
    }

    let doc = roxmltree::Document::parse(text).context("内容不是合法的 SVG/XML")?;
    let root = doc.root_element();
    if root.tag_name().name().to_ascii_lowercase() != "svg" {
        bail!("根元素必须是 svg");
    }

    let mut has_vector = false;
    for node in doc.descendants().filter(|node| node.is_element()) {
        let tag = node.tag_name().name().to_ascii_lowercase();
        if FORBIDDEN_ELEMENTS.contains(&tag.as_str()) {
            bail!("SVG 不允许包含 <{tag}> 元素");
        }
        if VECTOR_ELEMENTS.contains(&tag.as_str()) {
            has_vector = true;
        }

        for attr in node.attributes() {
            let name = attr.name().to_ascii_lowercase();
            if name.starts_with("on") {
                bail!("SVG 不允许包含事件属性");
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
            if tag == "use" && name == "href" && !value.starts_with('#') {
                bail!("<use> 只能引用文档内的图元");
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
}
