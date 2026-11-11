use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fmt;

/// Git 对象 ID（SHA-1，20 字节）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid([u8; 20]);

impl Oid {
    pub fn zero() -> Self {
        Self([0; 20])
    }

    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 20 {
            bail!("Git 对象 ID 必须是 20 字节");
        }
        let mut out = [0u8; 20];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    pub fn from_hex(value: &str) -> Result<Self> {
        let bytes = hex::decode(value.trim()).map_err(|_| anyhow::anyhow!("非法的 Git 对象 ID"))?;
        Self::from_slice(&bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn short(&self) -> String {
        self.to_hex()[..7].to_string()
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 20]
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl serde::Serialize for Oid {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for Oid {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Oid::from_hex(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i16)]
pub enum ObjectKind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
        }
    }

    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            1 => Some(Self::Commit),
            2 => Some(Self::Tree),
            3 => Some(Self::Blob),
            4 => Some(Self::Tag),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Object {
    pub kind: ObjectKind,
    pub data: Vec<u8>,
}

impl Object {
    pub fn blob(data: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: ObjectKind::Blob,
            data: data.into(),
        }
    }

    pub fn tree(data: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: ObjectKind::Tree,
            data: data.into(),
        }
    }

    pub fn commit(data: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: ObjectKind::Commit,
            data: data.into(),
        }
    }

    pub fn oid(&self) -> Oid {
        hash_object(self.kind, &self.data)
    }
}

pub fn hash_object(kind: ObjectKind, data: &[u8]) -> Oid {
    let mut hasher = Sha1::new();
    hasher.update(format!("{} {}\0", kind.as_str(), data.len()).as_bytes());
    hasher.update(data);
    Oid(hasher.finalize().into())
}

/// 判断内容能否作为 UTF-8 文本直接进 blob。
///
/// Git 自己的二进制判定主要看 NUL；这里再要求整体是合法 UTF-8。
/// 非文本内容走站内已实现的 LFS（`git/lfs.rs`），由 `project::write_file_bytes`
/// 分流；`ensure_text` 只用于管理端文本编辑器这个入口。
pub fn is_text(data: &[u8]) -> bool {
    !data.contains(&0) && std::str::from_utf8(data).is_ok()
}

pub fn ensure_text(data: &[u8]) -> Result<()> {
    if !is_text(data) {
        bail!("管理端编辑器只支持 UTF-8 文本；二进制请用 /git/<项目>/file 写入（LFS）");
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeEntry {
    Blob(Oid),
    Dir(Oid),
    /// 子模块 gitlink（mode `160000`）。
    ///
    /// 指向**外部仓库**的提交：这个 OID 在本仓库对象库里永远不存在，
    /// 任何遍历（clone/GC/模板复制/工作树统计）都必须跳过它，
    /// 否则会把一次普通的 clone 变成「请求的对象不存在」。
    Gitlink(Oid),
}

#[derive(Clone, Default, Debug)]
pub struct Tree {
    pub entries: BTreeMap<String, TreeEntry>,
}

/// Git tree 条目的名字必须是单个路径段，不能为空、`.`、`..` 或包含 `/`、NUL、控制字符。
pub(crate) fn valid_tree_entry_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\0')
        && !name.chars().any(|ch| ch.is_control())
}

impl Tree {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut entries = BTreeMap::new();
        let mut cursor = 0usize;
        while cursor < data.len() {
            let space = data[cursor..]
                .iter()
                .position(|byte| *byte == b' ')
                .map(|offset| cursor + offset)
                .ok_or_else(|| anyhow::anyhow!("tree 对象缺少 mode 分隔符"))?;
            let mode = std::str::from_utf8(&data[cursor..space])?;
            let name_start = space + 1;
            let nul = data[name_start..]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| name_start + offset)
                .ok_or_else(|| anyhow::anyhow!("tree 对象缺少名字终止符"))?;
            let name = std::str::from_utf8(&data[name_start..nul])?.to_string();
            if !valid_tree_entry_name(&name) {
                bail!("tree 对象包含非法条目名: {name:?}");
            }
            if nul + 21 > data.len() {
                bail!("tree 对象条目被截断");
            }
            let oid = Oid::from_slice(&data[nul + 1..nul + 21])?;
            let entry = if mode == "40000" || mode == "040000" {
                TreeEntry::Dir(oid)
            } else if mode == "160000" {
                // gitlink：子模块引用。OID 是外部仓库的提交，
                // 不是本仓库的对象，绝不能当作 blob/dir 展开。
                TreeEntry::Gitlink(oid)
            } else {
                TreeEntry::Blob(oid)
            };
            entries.insert(name, entry);
            cursor = nul + 21;
        }
        Ok(Self { entries })
    }

    pub fn encode(&self) -> Vec<u8> {
        // Git 的排序规则：目录名按末尾带 `/` 参与比较。
        let mut items: Vec<(&String, &TreeEntry)> = self.entries.iter().collect();
        items.sort_by(|(left_name, left), (right_name, right)| {
            let left_key = match left {
                TreeEntry::Dir(_) => format!("{left_name}/"),
                TreeEntry::Blob(_) | TreeEntry::Gitlink(_) => (*left_name).clone(),
            };
            let right_key = match right {
                TreeEntry::Dir(_) => format!("{right_name}/"),
                TreeEntry::Blob(_) | TreeEntry::Gitlink(_) => (*right_name).clone(),
            };
            left_key.as_bytes().cmp(right_key.as_bytes())
        });

        let mut out = Vec::new();
        for (name, entry) in items {
            let (mode, oid) = match entry {
                TreeEntry::Blob(oid) => (&b"100644 "[..], oid),
                // gitlink 原样回写 160000，保持与导入的仓库一致。
                TreeEntry::Gitlink(oid) => (&b"160000 "[..], oid),
                TreeEntry::Dir(oid) => (&b"40000 "[..], oid),
            };
            out.extend_from_slice(mode);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(oid.as_bytes());
        }
        out
    }

    pub fn get(&self, name: &str) -> Option<TreeEntry> {
        self.entries.get(name).copied()
    }
}

/// 生成 commit 对象内容。`identity` 形如 `名字 <noreply@kych.net>`。
pub fn encode_commit(
    tree: Oid,
    parents: &[Oid],
    identity: &str,
    message: &str,
    timestamp: i64,
) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("tree {}\n", tree.to_hex()));
    for parent in parents {
        out.push_str(&format!("parent {}\n", parent.to_hex()));
    }
    out.push_str(&format!("author {identity} {timestamp} +0000\n"));
    out.push_str(&format!("committer {identity} {timestamp} +0000\n"));
    out.push('\n');
    out.push_str(message.trim_end());
    out.push('\n');
    out.into_bytes()
}

/// 从 commit 对象里取出根 tree。
pub fn commit_tree(data: &[u8]) -> Result<Oid> {
    let text = std::str::from_utf8(data).map_err(|_| anyhow::anyhow!("commit 对象不是 UTF-8"))?;
    let line = text
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("commit 对象为空"))?;
    let value = line
        .strip_prefix("tree ")
        .ok_or_else(|| anyhow::anyhow!("commit 对象缺少 tree 行"))?;
    Oid::from_hex(value)
}

pub fn commit_parents(data: &[u8]) -> Result<Vec<Oid>> {
    let text = std::str::from_utf8(data).map_err(|_| anyhow::anyhow!("commit 对象不是 UTF-8"))?;
    let mut parents = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("parent ") {
            parents.push(Oid::from_hex(value)?);
        }
        if line.is_empty() {
            break;
        }
    }
    Ok(parents)
}

/// 从 commit 对象里解析出的字段。
pub struct CommitInfo {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    pub author: String,
    pub committed_at: DateTime<Utc>,
    pub message: String,
}

/// 一次性解析 commit 对象：tree、父提交、committer 身份与提交信息。
///
/// 身份取的 committer 而不是 author，与 git 的 `%cn` 一致；站内所有提交
/// 由同一账号生成，两者本来相同，导入的外部历史才可能不同。
pub fn parse_commit(data: &[u8]) -> Result<CommitInfo> {
    let text = std::str::from_utf8(data).map_err(|_| anyhow::anyhow!("commit 对象不是 UTF-8"))?;
    let (head, message) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = "KychNet".to_string();
    let mut committed_at = Utc::now();
    for line in head.lines() {
        if let Some(value) = line.strip_prefix("tree ") {
            tree = Some(Oid::from_hex(value)?);
        } else if let Some(value) = line.strip_prefix("parent ") {
            parents.push(Oid::from_hex(value)?);
        } else if let Some(value) = line.strip_prefix("committer ") {
            if let Some((name, timestamp)) = split_identity(value) {
                author = name;
                committed_at = DateTime::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now);
            }
        }
    }
    Ok(CommitInfo {
        tree: tree.ok_or_else(|| anyhow::anyhow!("commit 对象缺少 tree 行"))?,
        parents,
        author,
        committed_at,
        message: message.trim().to_string(),
    })
}

/// 解析 `名字 <email> 1700000000 +0800` 这一段，返回清理过的名字与时间戳。
fn split_identity(value: &str) -> Option<(String, i64)> {
    let close = value.rfind('>')?;
    let name = value[..close].split('<').next().unwrap_or_default().trim();
    let rest = value[close + 1..].trim();
    let timestamp = rest.split_whitespace().next()?.parse().ok()?;
    Some((identity_name(name), timestamp))
}

/// 清理用户名中会破坏 commit 身份行的字符。
pub fn identity_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|ch| !matches!(ch, '<' | '>' | '\n' | '\r' | '\0'))
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "KychNet".to_string()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_the_empty_blob_like_git() {
        assert_eq!(
            hash_object(ObjectKind::Blob, b"").to_hex(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn hashes_a_known_blob_like_git() {
        // printf 'hello\n' | git hash-object --stdin
        assert_eq!(
            hash_object(ObjectKind::Blob, b"hello\n").to_hex(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn tree_rejects_path_traversal_names() {
        for name in ["", ".", "..", "a/b", "a\0b"] {
            let mut data = format!("100644 {name}\0").into_bytes();
            data.extend_from_slice(&[0u8; 20]);
            assert!(Tree::parse(&data).is_err(), "应拒绝 tree 条目名 {name:?}");
        }
    }

    #[test]
    fn tree_round_trips_and_sorts_directories_by_slash() {
        let mut tree = Tree::default();
        tree.entries.insert(
            "a.txt".into(),
            TreeEntry::Blob(Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap()),
        );
        tree.entries.insert(
            "a".into(),
            TreeEntry::Dir(Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap()),
        );
        let parsed = Tree::parse(&tree.encode()).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert!(matches!(parsed.get("a"), Some(TreeEntry::Dir(_))));
        assert!(matches!(parsed.get("a.txt"), Some(TreeEntry::Blob(_))));

        // gitlink（子模块）：160000 解析为独立变体，编码时原样回写。
        let submodule = Oid::from_hex("1107741ce227fbd500ef201b1ef8140518f0eb44").unwrap();
        let mut data = format!("160000 地狱之下\0").into_bytes();
        data.extend_from_slice(submodule.as_bytes());
        let parsed = Tree::parse(&data).unwrap();
        assert!(matches!(
            parsed.get("地狱之下"),
            Some(TreeEntry::Gitlink(_))
        ));

        // gitlink 排序与 blob 相同（目录名才带 `/`），编码往返不失真。
        let mut tree = Tree::default();
        tree.entries
            .insert("mod".into(), TreeEntry::Gitlink(submodule));
        tree.entries.insert(
            "mod.txt".into(),
            TreeEntry::Blob(Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap()),
        );
        tree.entries.insert(
            "mod.d".into(),
            TreeEntry::Dir(Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap()),
        );
        let parsed = Tree::parse(&tree.encode()).unwrap();
        assert!(matches!(parsed.get("mod"), Some(TreeEntry::Gitlink(_))));
        let raw = tree.encode();
        let prefix = format!("160000 mod\0");
        assert!(
            raw.windows(prefix.len() + 20)
                .any(|w| w.starts_with(prefix.as_bytes())),
            "gitlink 应以 160000 模式编码"
        );
    }

    #[test]
    fn rejects_binary_content() {
        assert!(is_text("普通文本\n".as_bytes()));
        assert!(!is_text(&[0xff, 0xfe, 0x00]));
        assert!(!is_text(b"a\0b"));
    }
}
