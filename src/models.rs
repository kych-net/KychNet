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
    pub logo_url: String,
    pub nav: Vec<NavLink>,
}

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            site_name: "KychNet".into(),
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
}

impl ContentFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            "typst" => Some(Self::Typst),
            "wikidot" => Some(Self::Wikidot),
            "css" => Some(Self::Css),
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
        }
    }

    /// 源文件扩展名；三种格式最终都输出 `.html`。
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => ".md",
            Self::Html => ".html",
            Self::Typst => ".typ",
            Self::Wikidot => ".wj",
            Self::Css => ".css",
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
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Content {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub format: String,
    pub body: String,
    pub published: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Content {
    pub fn format(&self) -> ContentFormat {
        ContentFormat::parse(&self.format).unwrap_or(ContentFormat::Html)
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
