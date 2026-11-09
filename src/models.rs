use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub disabled: bool,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NavLink {
    pub label: String,
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteSettings {
    pub site_name: String,
    #[serde(default)]
    pub description: String,
    pub logo_url: String,
    pub nav: Vec<NavLink>,
}

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            site_name: "KychNet".into(),
            description: "KychNet 个人站点".into(),
            logo_url: String::new(),
            nav: vec![
                NavLink {
                    label: "文章".into(),
                    href: "/".into(),
                },
                NavLink {
                    label: "关于".into(),
                    href: "/about".into(),
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Invite {
    pub code: String,
    pub created_by: Option<Uuid>,
    pub max_uses: i32,
    pub used_count: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentFormat {
    Markdown,
    Html,
    Typst,
    Wikidot,
    Css,
    Svg,
}

impl ContentFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            "typst" => Some(Self::Typst),
            "wikidot" => Some(Self::Wikidot),
            "css" => Some(Self::Css),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }

    /// 数据库与 API 使用的格式名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Typst => "typst",
            Self::Wikidot => "wikidot",
            Self::Css => "css",
            Self::Svg => "svg",
        }
    }

    /// 源文件扩展名。页面类格式最终输出 `.html`，CSS / SVG 作为静态资源原样输出。
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => ".md",
            Self::Html => ".html",
            Self::Typst => ".typ",
            Self::Wikidot => ".wj",
            Self::Css => ".css",
            Self::Svg => ".svg",
        }
    }

    /// 从源文件扩展名推断格式，扩展名不区分大小写。
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "md" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            "typ" => Some(Self::Typst),
            "wj" => Some(Self::Wikidot),
            "css" => Some(Self::Css),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Content {
    pub id: Uuid,
    pub slug: String,
    pub format: String,
    pub body: String,
    pub published: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 当前版本指针：指向 `content_revisions` 里正在展示的版本。
    /// 回滚直接移动这个指针，不产生新版本；0 表示尚未落任何版本。
    pub rev_seq: i32,
}

/// 单篇内容的单线历史版本。`kind = full` 时 `data` 是整段正文，
/// `kind = delta` 时 `data` 是相对 `parent_seq` 版本的行级增量（JSON 操作序列）。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ContentRevision {
    pub id: Uuid,
    pub content_id: Uuid,
    pub seq: i32,
    pub parent_seq: Option<i32>,
    pub kind: String,
    pub slug: String,
    pub format: String,
    pub data: String,
    pub added: i32,
    pub removed: i32,
    pub author_id: Option<Uuid>,
    pub author_name: Option<String>,
    pub message: String,
    pub byte_size: i32,
    pub created_at: DateTime<Utc>,
}

impl Content {
    pub fn format(&self) -> ContentFormat {
        ContentFormat::parse(&self.format).unwrap_or(ContentFormat::Html)
    }

    /// 展示名统一用源文件名，不再从标题字段推导。
    pub fn filename(&self) -> String {
        let name = self.slug.rsplit('/').next().unwrap_or(&self.slug);
        match name.strip_suffix(".html") {
            Some(stem) => format!("{stem}{}", self.format().extension()),
            None => name.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PasskeyRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub credential_id: String,
    pub passkey: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
