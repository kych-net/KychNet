use anyhow::{Context, Result};
use askama::Template;
use sqlx::PgPool;
use std::path::Path;

use crate::models::SiteSettings;
use crate::{config::Config, render, service};

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    site: SiteSettings,
    admin_url: String,
    posts: Vec<PostLink>,
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
}

/// 一篇已经渲染完成的页面，先留在内存里，等全部成功后再统一落盘。
struct BuiltPage {
    slug: String,
    title: String,
    date: String,
    html: String,
    pdf: Option<Vec<u8>>,
}

/// 把 slug 编码成 URL 路径段：非 ASCII 字节转成 `%XX`，ASCII 字母数字与 `-` `_` 原样保留。
///
/// slug 现在允许中文等 Unicode 字符，磁盘上的文件名保持原始 UTF-8，
/// 但 HTML 里的链接必须是合法 URL；静态服务器收到请求后会解码再按原始文件名查找，
/// 两端因此一致。不做编码的话，部分 CDN 会把原始多字节路径当成找不到资源。
fn url_path_segment(slug: &str) -> String {
    let mut out = String::with_capacity(slug.len());
    for &byte in slug.as_bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub async fn run(pool: &PgPool, cfg: &Config, out: &Path) -> Result<()> {
    let settings = service::get_settings(pool).await?;
    let contents = service::list_published(pool).await?;

    // 先把所有页面渲染到内存。旧实现边渲染边写盘，一旦某篇 Typst 编译失败，
    // 就会留下半套站点；现在任何一篇失败都不会触碰磁盘。
    let mut pages = Vec::new();
    for content in contents {
        if !service::valid_slug(&content.slug) {
            // 历史脏数据不该让整站构建永久失败。新增数据已在 save_content 拦住，
            // 这里只作兜底：跳过并告警，索引里也不会出现它。
            tracing::warn!("跳过非法 slug: {:?}", content.slug);
            continue;
        }

        let rendered = render::render(content.format(), &content.body, &cfg.content_root)
            .with_context(|| format!("渲染 {} 失败", content.slug))?;

        let date = content.updated_at.format("%Y-%m-%d").to_string();
        let encoded_slug = url_path_segment(&content.slug);
        let pdf_url = if rendered.pdf.is_some() {
            format!("/posts/{encoded_slug}.pdf")
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
        };
        let html = page.render()?;

        pages.push(BuiltPage {
            slug: content.slug,
            title: content.title,
            date,
            html,
            pdf: rendered.pdf,
        });
    }

    let mut links: Vec<PostLink> = pages
        .iter()
        .map(|page| PostLink {
            title: page.title.clone(),
            href: format!("/posts/{}.html", url_path_segment(&page.slug)),
            date: page.date.clone(),
        })
        .collect();
    links.sort_by(|a, b| b.date.cmp(&a.date));

    let index = IndexTemplate {
        site: settings,
        admin_url: cfg.admin_origin.clone(),
        posts: links,
    };
    let index_html = index.render()?;

    // 全部渲染成功后才清理旧产物：删除内容或改 slug 后，旧 .html / .pdf 不再残留。
    // 只清理本程序生成的 posts/ 与 index.html，不动目录里可能存在的其它资源。
    std::fs::create_dir_all(out)?;
    let posts_dir = out.join("posts");
    if posts_dir.exists() {
        std::fs::remove_dir_all(&posts_dir)?;
    }
    std::fs::create_dir_all(&posts_dir)?;

    for page in &pages {
        std::fs::write(posts_dir.join(format!("{}.html", page.slug)), &page.html)?;
        if let Some(pdf) = &page.pdf {
            std::fs::write(posts_dir.join(format!("{}.pdf", page.slug)), pdf)?;
        }
    }
    std::fs::write(out.join("index.html"), index_html)?;
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
        let index = IndexTemplate {
            site: settings(),
            admin_url: "https://m.kych.net".into(),
            posts: vec![PostLink {
                title: "第一篇".into(),
                href: "/posts/hello.html".into(),
                date: "2026-11-06".into(),
            }],
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
            pdf_url: "/posts/demo.pdf".into(),
        };
        let html = page.render().unwrap();
        assert!(html.contains("<p>正文</p>"));
        assert!(html.contains("下载 PDF"));
        assert!(html.contains("theme-toggle"));
    }

    #[test]
    fn encodes_unicode_slug_for_url() {
        assert_eq!(url_path_segment("你好"), "%E4%BD%A0%E5%A5%BD");
        assert_eq!(url_path_segment("café"), "caf%C3%A9");
        assert_eq!(url_path_segment("a-b_1"), "a-b_1");
    }
}
