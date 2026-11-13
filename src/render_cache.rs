//! 构建期渲染缓存：Typst / Wikidot 的编译产物按内容哈希落盘缓存。
//!
//! 整站重建不再重复编译未变化的页面：缓存键覆盖正文、格式、Typst 可见的
//! 全部文件（项目快照 + 内容目录磁盘快照）与 Wikidot include 集合，
//! 任何输入变化都得到新键，不存在陈旧命中。Markdown/HTML/SVG 渲染足够快，
//! 不走缓存。
//!
//! 缓存目录放在站点输出目录**之外**（同父目录的隐藏目录），不会被 nginx
//! 当成站点内容伺服。每轮整站构建结束后清掉本轮未使用的条目。

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::render::Rendered;

/// 渲染逻辑或依赖引擎（typst/ftml）行为变化时手动递增，使旧缓存整体失效。
/// 包版本也参与键计算，常规依赖升级随 Cargo.lock 自动失效。
const CACHE_VERSION: &str = "1";

pub struct RenderCache {
    dir: PathBuf,
}

/// 缓存目录：out 的同父隐藏目录，保证与产物同盘且不被站点伺服。
pub fn cache_dir(out: &Path) -> PathBuf {
    out.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".kychnet-render-cache")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl RenderCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Typst 键：正文 + 主文件路径 + 项目文件快照 + 内容目录磁盘快照。
    pub fn key_typst(
        &self,
        body: &str,
        main_path: &str,
        files: &HashMap<String, Vec<u8>>,
        root_snapshot: &str,
    ) -> String {
        let mut h = Sha256::new();
        h.update(b"typst\0");
        h.update(CACHE_VERSION.as_bytes());
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        h.update(body.as_bytes());
        h.update(b"\0");
        h.update(main_path.as_bytes());
        let mut paths: Vec<&String> = files.keys().collect();
        paths.sort();
        for path in paths {
            h.update(path.as_bytes());
            h.update(b"\0");
            h.update(&files[path]);
            h.update(b"\0");
        }
        h.update(root_snapshot.as_bytes());
        hex(&h.finalize())
    }

    /// Wikidot 键：正文 + 全部 include 页面（ftml 的 [[include]] 在构建期解析）。
    pub fn key_wikidot(&self, body: &str, includes: &HashMap<String, String>) -> String {
        let mut h = Sha256::new();
        h.update(b"wikidot\0");
        h.update(CACHE_VERSION.as_bytes());
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        h.update(body.as_bytes());
        let mut pages: Vec<&String> = includes.keys().collect();
        pages.sort();
        for page in pages {
            h.update(page.as_bytes());
            h.update(b"\0");
            h.update(includes[page].as_bytes());
            h.update(b"\0");
        }
        hex(&h.finalize())
    }

    pub fn get(&self, key: &str) -> Option<Rendered> {
        let html = std::fs::read_to_string(self.dir.join(format!("{key}.html"))).ok()?;
        let pdf = std::fs::read(self.dir.join(format!("{key}.pdf"))).ok();
        Some(Rendered { html, pdf })
    }

    /// 写缓存失败只告警：缓存是优化，不能让它阻断构建。
    pub fn put(&self, key: &str, rendered: &Rendered) {
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            std::fs::write(self.dir.join(format!("{key}.html")), &rendered.html)?;
            if let Some(pdf) = &rendered.pdf {
                std::fs::write(self.dir.join(format!("{key}.pdf")), pdf)?;
            }
            Ok(())
        };
        if let Err(error) = write() {
            tracing::warn!(%error, "写入渲染缓存失败");
        }
    }

    /// 清掉本轮构建未使用的条目，缓存目录不会随内容演化无限增长。
    pub fn sweep(&self, used: &HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".html")
                .or_else(|| name.strip_suffix(".pdf"))
                .unwrap_or("");
            if !stem.is_empty() && !used.contains(stem) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// 内容目录的磁盘快照哈希：Typst 的 include 在项目快照读不到时会回退
/// 读取 CONTENT_ROOT 下的文件，这些文件变化也必须使缓存失效。
pub fn root_snapshot(root: &Path) -> String {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                walk(root, &path, out);
            } else if file_type.is_file() {
                if let Ok(data) = std::fs::read(&path) {
                    if let Ok(rel) = path.strip_prefix(root) {
                        out.push((rel.to_string_lossy().into_owned(), data));
                    }
                }
            }
        }
    }
    walk(root, root, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (path, data) in &entries {
        h.update(path.as_bytes());
        h.update(b"\0");
        h.update(data);
        h.update(b"\0");
    }
    hex(&h.finalize())
}
