use anyhow::{bail, Context, Result};
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

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub async fn run(pool: &PgPool, cfg: &Config, out: &Path) -> Result<()> {
    let settings = service::get_settings(pool).await?;
    let contents = service::list_published(pool).await?;
    std::fs::create_dir_all(out)?;
    let posts_dir = out.join("posts");
    std::fs::create_dir_all(&posts_dir)?;

    let mut links = Vec::new();
    for content in contents {
        if !valid_slug(&content.slug) {
            bail!("非法 slug: {}", content.slug);
        }

        let rendered = render::render(content.format(), &content.body, &cfg.content_root)
            .with_context(|| format!("渲染 {} 失败", content.slug))?;

        let pdf_url = match &rendered.pdf {
            Some(pdf) => {
                let path = posts_dir.join(format!("{}.pdf", content.slug));
                std::fs::write(&path, pdf)?;
                format!("/posts/{}.pdf", content.slug)
            }
            None => String::new(),
        };

        let date = content.updated_at.format("%Y-%m-%d").to_string();
        let page = PageTemplate {
            site: settings.clone(),
            admin_url: cfg.admin_origin.clone(),
            title: content.title.clone(),
            date: date.clone(),
            content: rendered.html,
            pdf_url,
        };
        let html = page.render()?;
        std::fs::write(posts_dir.join(format!("{}.html", content.slug)), html)?;

        links.push(PostLink {
            title: content.title,
            href: format!("/posts/{}.html", content.slug),
            date,
        });
    }

    links.sort_by(|a, b| b.date.cmp(&a.date));

    let index = IndexTemplate {
        site: settings,
        admin_url: cfg.admin_origin.clone(),
        posts: links,
    };
    std::fs::write(out.join("index.html"), index.render()?)?;
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
            admin_url: "https://admin.kych.net".into(),
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
            admin_url: "https://admin.kych.net".into(),
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
}
