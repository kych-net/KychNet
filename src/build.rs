use anyhow::{bail, Context, Result};
use askama::Template;
use sqlx::PgPool;
use std::path::Path;

use crate::{config::Config, render, service};

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    posts: Vec<PostLink>,
}

struct PostLink {
    title: String,
    href: String,
}

#[derive(Template)]
#[template(path = "page.html")]
struct PageTemplate {
    title: String,
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

        let page = PageTemplate {
            title: content.title.clone(),
            content: rendered.html,
            pdf_url,
        };
        let html = page.render()?;
        std::fs::write(posts_dir.join(format!("{}.html", content.slug)), html)?;

        links.push(PostLink {
            title: content.title,
            href: format!("/posts/{}.html", content.slug),
        });
    }

    let index = IndexTemplate { posts: links };
    std::fs::write(out.join("index.html"), index.render()?)?;
    Ok(())
}
