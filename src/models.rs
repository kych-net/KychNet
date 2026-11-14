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
    /// ICP 备案号，如「京ICP备12345678号-1」；留空则页脚不显示备案信息。
    #[serde(default)]
    pub icp_beian: String,
    /// 公安机关互联网站安全管理服务平台备案号显示文本，
    /// 如「京公网安备 11010802012345号」；留空则不显示。
    #[serde(default)]
    pub police_beian: String,
    /// 公安备案编号（纯数字），用于拼接备案查询链接。
    /// 留空时自动从 `police_beian` 中取最长的一段连续数字。
    #[serde(default)]
    pub police_beian_code: String,
}

impl SiteSettings {
    /// 页脚是否需要展示备案信息。
    pub fn has_beian(&self) -> bool {
        !self.icp_beian.trim().is_empty() || !self.police_beian.trim().is_empty()
    }

    /// 公安备案查询链接。编号只取数字，避免把任意文本拼进 URL。
    /// 取不到编号时回落到平台首页，保证链接始终可用。
    pub fn police_beian_href(&self) -> String {
        let code = if self
            .police_beian_code
            .chars()
            .all(|c| c.is_ascii_digit())
            && !self.police_beian_code.is_empty()
        {
            self.police_beian_code.trim().to_string()
        } else {
            // 备案号形如「京公网安备 11010802012345号」，取其中最长数字段。
            let mut best = String::new();
            let mut current = String::new();
            for c in self.police_beian.chars() {
                if c.is_ascii_digit() {
                    current.push(c);
                    if current.len() > best.len() {
                        best.clone_from(&current);
                    }
                } else {
                    current.clear();
                }
            }
            best
        };
        if code.is_empty() {
            "https://beian.gov.cn/".to_string()
        } else {
            format!("https://beian.gov.cn/portal/registerSystemInfo?recordcode={code}")
        }
    }
}

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            // 首次建站、还没有保存过设置时的兜底值；可被 SITE_NAME /
            // SITE_DESCRIPTION env 覆盖，避免品牌名写死在代码里。
            site_name: std::env::var("SITE_NAME")
                .unwrap_or_else(|_| "个人站点".to_string())
                .into(),
            description: std::env::var("SITE_DESCRIPTION")
                .unwrap_or_else(|_| "个人站点".to_string())
                .into(),
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
            icp_beian: String::new(),
            police_beian: String::new(),
            police_beian_code: String::new(),
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
    /// 这种格式是否会产出一个带 <head> 的 HTML 页面。
    ///
    /// css / svg 作为静态资源原样输出，没有页面结构，因此不支持自定义 head。
    pub fn renders_html(self) -> bool {
        matches!(
            self,
            Self::Markdown | Self::Html | Self::Typst | Self::Wikidot
        )
    }

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
    /// 页面自定义 <head> 内容（原样注入，仅管理员可写；不参与版本历史）。
    #[serde(default)]
    pub head: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn beian(icp: &str, police: &str, code: &str) -> SiteSettings {
        SiteSettings {
            icp_beian: icp.into(),
            police_beian: police.into(),
            police_beian_code: code.into(),
            ..SiteSettings::default()
        }
    }

    #[test]
    fn beian_footer_hidden_when_not_configured() {
        assert!(!SiteSettings::default().has_beian());
        assert!(!beian("", "  ", "").has_beian());
        assert!(beian("京ICP备12345678号-1", "", "").has_beian());
        assert!(beian("", "京公网安备 11010802012345号", "").has_beian());
    }

    #[test]
    fn police_href_prefers_explicit_numeric_code() {
        let site = beian("", "京公网安备 11010802012345号", "11010802012345");
        assert_eq!(
            site.police_beian_href(),
            "https://beian.gov.cn/portal/registerSystemInfo?recordcode=11010802012345"
        );
    }

    #[test]
    fn police_href_extracts_longest_digits_from_text() {
        let site = beian("", "京公网安备 11010802012345号", "");
        assert_eq!(
            site.police_beian_href(),
            "https://beian.gov.cn/portal/registerSystemInfo?recordcode=11010802012345"
        );
    }

    #[test]
    fn police_href_falls_back_when_code_is_not_numeric() {
        // 有人在编号框里粘了整段文本或脚本，一律忽略，避免拼进 URL。
        let site = beian("", "京公网安备 11010802012345号", "javascript:alert(1)");
        assert_eq!(
            site.police_beian_href(),
            "https://beian.gov.cn/portal/registerSystemInfo?recordcode=11010802012345"
        );
        let empty = beian("", "备案号待申请", "");
        assert_eq!(empty.police_beian_href(), "https://beian.gov.cn/");
    }

    #[test]
    fn site_settings_deserialize_legacy_json_without_beian() {
        // 升级前存进 site_settings 的 JSON 没有这三个字段，必须能读出来。
        let old = r#"{"site_name":"KychNet","description":"站点","logo_url":"","nav":[]}"#;
        let site: SiteSettings = serde_json::from_str(old).unwrap();
        assert_eq!(site.site_name, "KychNet");
        assert!(!site.has_beian());
        assert!(site.icp_beian.is_empty());
    }
}
