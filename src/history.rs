//! 普通文章的精简版历史：单文件、单线版本、行级增量存储。
//!
//! 与项目 Git 完全独立：每篇内容一条版本链（`content_revisions` 表），
//! `full` 行存整段正文，`delta` 行存相对 `parent_seq` 版本的行级操作序列
//! （copy / delete / insert / replace，只引用父版本行号，不携带未变文本）。
//!
//! 重建开销被检查点间隔锁死：每累计 [`CHECKPOINT_INTERVAL`] 个增量或增量
//! 超过新正文 [`DELTA_MAX_RATIO_PERCENT`]% 时落一次全文检查点，因此任意
//! 版本的重建最多回放固定数量的增量。
//!
//! 回滚语义是「移动指针」：把 `contents.rev_seq` 直接指向目标版本并把该
//! 版本正文写回 `contents.body`，不产生新版本；下一次保存的 `parent_seq`
//! 取当时的指针，版本链仍严格单线。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use similar::{DiffOp, TextDiff};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::models::ContentRevision;

/// 连续 delta 数量达到该值就落一次全文检查点，
/// 任意版本的重建最多回放这么多个增量。
const CHECKPOINT_INTERVAL: i32 = 20;
/// 增量编码字节数超过新正文的这个百分比时，直接存全文更划算。
/// 全文重写、单超长行文件都会命中该阈值自动退化为全文存储。
const DELTA_MAX_RATIO_PERCENT: usize = 40;
/// 重建时沿 parent 链回溯的安全上限，防止脏数据造成死循环。
const MAX_CHAIN_LEN: usize = 10_000;

/// 行级增量操作。行号区间引用父版本（`from` 起始行、`len` 行数），
/// 新文本只出现在 `Insert` / `Replace` 里，因此未变的正文零存储。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
enum DeltaOp {
    /// 原样保留父版本 `[from, from+len)` 的行。
    Copy { from: usize, len: usize },
    /// 丢弃父版本 `[from, from+len)` 的行。
    Delete { from: usize, len: usize },
    /// 插入新行。
    Insert { lines: Vec<String> },
    /// 用新行替换父版本 `[from, from+len)` 的行。
    Replace {
        from: usize,
        len: usize,
        lines: Vec<String>,
    },
}

/// 按保留行尾换行符的方式切行，`concat` 切片可无损还原原文。
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    }
}

/// 用 `similar` 的行级 diff 生成增量操作序列，同时统计增删行数。
fn encode_delta(old: &str, new: &str) -> (Vec<DeltaOp>, i32, i32) {
    let diff = TextDiff::from_lines(old, new);
    let new_lines: Vec<&str> = split_lines(new);
    let mut ops = Vec::new();
    let mut added = 0i32;
    let mut removed = 0i32;
    // 直接按变体解构：similar 的 DiffOp::tag() 返回的是 DiffTag，
    // 且 Replace 是否存在取决于 capture 配置，用变体匹配最稳妥。
    for &op in diff.ops() {
        match op {
            DiffOp::Equal { old_index, len, .. } => ops.push(DeltaOp::Copy {
                from: old_index,
                len,
            }),
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                removed += old_len as i32;
                ops.push(DeltaOp::Delete {
                    from: old_index,
                    len: old_len,
                });
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                added += new_len as i32;
                ops.push(DeltaOp::Insert {
                    lines: new_lines[new_index..new_index + new_len]
                        .iter()
                        .map(|line| line.to_string())
                        .collect(),
                });
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                removed += old_len as i32;
                added += new_len as i32;
                ops.push(DeltaOp::Replace {
                    from: old_index,
                    len: old_len,
                    lines: new_lines[new_index..new_index + new_len]
                        .iter()
                        .map(|line| line.to_string())
                        .collect(),
                });
            }
        }
    }
    (ops, added, removed)
}

/// 把增量序列回放到父版本文本上，得到子版本文本。
fn apply_delta(parent: &str, ops: &[DeltaOp]) -> Result<String> {
    // 某些实现可能不为「完全无变化」产出任何 op，此时保持父版本不变。
    if ops.is_empty() {
        return Ok(parent.to_string());
    }
    let parent_lines: Vec<&str> = split_lines(parent);
    let mut out: Vec<std::borrow::Cow<str>> = Vec::new();
    for op in ops {
        match op {
            DeltaOp::Copy { from, len } => {
                let end = from
                    .checked_add(*len)
                    .filter(|end| *end <= parent_lines.len())
                    .ok_or_else(|| {
                        anyhow::anyhow!("增量越界：父版本只有 {} 行", parent_lines.len())
                    })?;
                out.extend(parent_lines[*from..end].iter().map(|line| (*line).into()));
            }
            DeltaOp::Delete { .. } => {}
            DeltaOp::Insert { lines } => out.extend(lines.iter().map(|line| line.as_str().into())),
            DeltaOp::Replace { lines, .. } => {
                out.extend(lines.iter().map(|line| line.as_str().into()))
            }
        }
    }
    Ok(out.concat())
}

/// 单条版本行（重建链上需要的最小字段）。
#[derive(sqlx::FromRow)]
struct ChainRow {
    kind: String,
    parent_seq: Option<i32>,
    data: String,
}

/// 沿 parent 链从 `seq` 回溯到最近的 `full` 检查点并回放增量，
/// 返回该版本的完整正文。需要行级锁以外的任何锁：版本行是 append-only 的。
async fn reconstruct(conn: &mut PgConnection, content_id: Uuid, seq: i32) -> Result<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut base = String::new();
    let mut current = seq;
    loop {
        if chain.len() > MAX_CHAIN_LEN {
            bail!("版本链异常：回溯超过安全上限");
        }
        let Some(row): Option<ChainRow> = sqlx::query_as(
            "SELECT kind, parent_seq, data FROM content_revisions
             WHERE content_id = $1 AND seq = $2",
        )
        .bind(content_id)
        .bind(current)
        .fetch_optional(&mut *conn)
        .await?
        else {
            bail!("版本不存在");
        };
        if row.kind == "full" {
            base = row.data;
            break;
        }
        chain.push(row.data);
        current = row
            .parent_seq
            .ok_or_else(|| anyhow::anyhow!("增量版本缺少 parent_seq"))?;
    }
    // chain 按 seq 从新到旧收集，回放时从检查点向目标版本正向应用。
    let mut text = base;
    for delta in chain.iter().rev() {
        let ops: Vec<DeltaOp> = serde_json::from_str(delta)
            .map_err(|error| anyhow::anyhow!("增量数据损坏：{error}"))?;
        text = apply_delta(&text, &ops)?;
    }
    Ok(text)
}

/// 在保存事务内落一条版本行，返回新版本号。
///
/// 调用方必须先对 `contents` 行加锁（`FOR UPDATE`）串行化并发保存，
/// 且已确认正文或格式确实变化。`old_body` 是保存前的正文。
#[allow(clippy::too_many_arguments)]
pub async fn record_revision(
    conn: &mut PgConnection,
    content_id: Uuid,
    slug: &str,
    format: &str,
    old_body: &str,
    new_body: &str,
    author_id: Uuid,
    message: &str,
) -> Result<i32> {
    let seq: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM content_revisions WHERE content_id = $1",
    )
    .bind(content_id)
    .fetch_one(&mut *conn)
    .await?;
    // parent 取 contents 的当前指针：回滚移动指针后，下一次保存的增量
    // 基于指针指向的版本而不是版本号最大的那个。
    let parent_seq: i32 = sqlx::query_scalar("SELECT rev_seq FROM contents WHERE id = $1")
        .bind(content_id)
        .fetch_one(&mut *conn)
        .await?;

    let (ops, added, removed) = encode_delta(old_body, new_body);
    let delta_data = serde_json::to_string(&ops)?;

    // 检查点判定：当前指针沿 parent 链到最近全文检查点的实际深度达到上限，
    // 或增量编码本身比新正文的 40% 还大（全文重写、单超长行等），都直接存全文。
    // 不能用「seq 与最新 full 的差」：回滚移动指针后，最新 full 可能根本不在
    // 当前链上，seq 差会持续失真，链深无界增长直到重建触顶失败。
    let chain_depth: Option<i32> = sqlx::query_scalar(
        "WITH RECURSIVE chain AS (
             SELECT seq, parent_seq, kind, 1 AS depth
             FROM content_revisions WHERE content_id = $1 AND seq = $2
             UNION ALL
             SELECT r.seq, r.parent_seq, r.kind, c.depth + 1
             FROM content_revisions r
             JOIN chain c ON r.seq = c.parent_seq AND r.content_id = $1
             WHERE c.kind <> 'full' AND c.depth < $3
         )
         SELECT depth FROM chain WHERE kind = 'full' ORDER BY depth LIMIT 1",
    )
    .bind(content_id)
    .bind(parent_seq)
    .bind(MAX_CHAIN_LEN as i32)
    .fetch_optional(&mut *conn)
    .await?;
    let near_checkpoint = match chain_depth {
        Some(depth) => depth >= CHECKPOINT_INTERVAL,
        None => true,
    };
    let delta_too_big = delta_data.len() * 100 > new_body.len() * DELTA_MAX_RATIO_PERCENT;
    let (kind, data, parent) = if near_checkpoint || delta_too_big {
        ("full", new_body.to_string(), None::<i32>)
    } else {
        ("delta", delta_data, Some(parent_seq))
    };

    sqlx::query(
        "INSERT INTO content_revisions
             (content_id, seq, parent_seq, kind, slug, format, data,
              added, removed, author_id, message, byte_size)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(content_id)
    .bind(seq)
    .bind(parent)
    .bind(kind)
    .bind(slug)
    .bind(format)
    .bind(&data)
    .bind(added)
    .bind(removed)
    .bind(author_id)
    .bind(message)
    .bind(new_body.len() as i32)
    .execute(&mut *conn)
    .await?;
    Ok(seq)
}

/// 新建内容时的第一条全文基线版本（seq = 1）。
pub async fn record_full_baseline(
    conn: &mut PgConnection,
    content_id: Uuid,
    slug: &str,
    format: &str,
    body: &str,
    author_id: Uuid,
    message: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO content_revisions
             (content_id, seq, parent_seq, kind, slug, format, data,
              added, removed, author_id, message, byte_size)
         VALUES ($1, 1, NULL, 'full', $2, $3, $4, 0, 0, $5, $6, $7)",
    )
    .bind(content_id)
    .bind(slug)
    .bind(format)
    .bind(body)
    .bind(author_id)
    .bind(message)
    .bind(body.len() as i32)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// 版本列表（不含 data，避免把大正文拖出来）。
pub async fn list_revisions(
    pool: &sqlx::PgPool,
    content_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<ContentRevision>> {
    Ok(sqlx::query_as::<_, ContentRevision>(
        "SELECT r.id, r.content_id, r.seq, r.parent_seq, r.kind, r.slug, r.format, ''::text AS data,
                r.added, r.removed, r.author_id, COALESCE(u.username, '已注销用户') AS author_name,
                r.message, r.byte_size, r.created_at
         FROM content_revisions r
         LEFT JOIN users u ON u.id = r.author_id
         WHERE r.content_id = $1
         ORDER BY r.seq DESC LIMIT $2 OFFSET $3",
    )
    .bind(content_id)
    .bind(limit.clamp(1, 500))
    .bind(offset.max(0))
    .fetch_all(pool)
    .await?)
}

/// 读取某个版本保存时的格式与完整正文。
pub async fn read_revision(
    pool: &sqlx::PgPool,
    content_id: Uuid,
    seq: i32,
) -> Result<(String, String)> {
    let mut conn = pool.acquire().await?;
    let format: String = sqlx::query_scalar(
        "SELECT format FROM content_revisions WHERE content_id = $1 AND seq = $2",
    )
    .bind(content_id)
    .bind(seq)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| anyhow::anyhow!("版本不存在"))?;
    let body = reconstruct(&mut conn, content_id, seq).await?;
    Ok((format, body))
}

/// 两个版本间的 unified diff（基于源文本行级比较）。
/// `against` 缺省取当前指针版本。
pub async fn diff_revisions(
    pool: &sqlx::PgPool,
    content_id: Uuid,
    seq: i32,
    against: Option<i32>,
) -> Result<String> {
    let mut conn = pool.acquire().await?;
    let against = match against {
        Some(value) => value,
        None => sqlx::query_scalar("SELECT rev_seq FROM contents WHERE id = $1")
            .bind(content_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| anyhow::anyhow!("内容不存在"))?,
    };
    let old_body = reconstruct(&mut conn, content_id, against).await?;
    let new_body = reconstruct(&mut conn, content_id, seq).await?;
    if old_body == new_body {
        return Ok(String::new());
    }
    let diff = TextDiff::from_lines(&old_body, &new_body);
    let old_header = format!("版本 {against}");
    let new_header = format!("版本 {seq}");
    let mut formatter = diff.unified_diff();
    formatter.context_radius(3);
    formatter.header(&old_header, &new_header);
    Ok(formatter.to_string())
}

/// 回滚：直接移动指针。`contents.rev_seq` 指向目标版本，正文、格式与路径
/// 同步取该版本，不产生新版本；下一次保存基于移动后的指针继续单线延伸。
/// 返回 (更新后的内容, 回滚前的路径)：路径变化时调用方要删旧产物。
pub async fn revert(
    pool: &sqlx::PgPool,
    content_id: Uuid,
    seq: i32,
) -> Result<(crate::models::Content, String)> {
    let mut tx = pool.begin().await?;
    // 锁住内容行：与保存流程同一把锁，回滚期间不会有并发保存插入增量。
    let locked: Option<(i32, String)> =
        sqlx::query_as("SELECT rev_seq, slug FROM contents WHERE id = $1 FOR UPDATE")
            .bind(content_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((_current, old_slug)) = locked else {
        bail!("内容不存在");
    };
    let (format, slug): (String, String) = sqlx::query_as(
        "SELECT format, slug FROM content_revisions WHERE content_id = $1 AND seq = $2",
    )
    .bind(content_id)
    .bind(seq)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow::anyhow!("版本不存在"))?;
    // 主页路径固定：回滚不能把 index.html 挪走，否则站点失去主页。
    if old_slug == crate::service::HOME_SLUG && slug != crate::service::HOME_SLUG {
        bail!("主页路径不能通过回滚改变");
    }
    let body = reconstruct(&mut tx, content_id, seq).await?;
    let content = sqlx::query_as::<_, crate::models::Content>(
        "UPDATE contents
         SET slug = $2, format = $3, body = $4, rev_seq = $5, updated_at = now()
         WHERE id = $1
         RETURNING *",
    )
    .bind(content_id)
    .bind(&slug)
    .bind(&format)
    .bind(&body)
    .bind(seq)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| match &error {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            anyhow::anyhow!("回滚失败：该版本的路径已被其它内容占用")
        }
        _ => error.into(),
    })?;
    tx.commit().await?;
    Ok((content, old_slug))
}

#[cfg(test)]
mod tests {
    use super::{apply_delta, encode_delta, split_lines, DiffOp};
    use similar::TextDiff;

    /// 保存→重建的往返不变量：apply(encode(old, new), old) == new。
    fn roundtrip(old: &str, new: &str) {
        let (ops, added, removed) = encode_delta(old, new);
        let rebuilt = apply_delta(old, &ops).expect("回放失败");
        assert_eq!(rebuilt, new, "重建结果不一致");
        // 增删行数与按变体统计的口径一致。
        let diff = TextDiff::from_lines(old, new);
        let mut expect_added = 0i32;
        let mut expect_removed = 0i32;
        for &op in diff.ops() {
            match op {
                DiffOp::Insert { new_len, .. } => expect_added += new_len as i32,
                DiffOp::Delete { old_len, .. } => expect_removed += old_len as i32,
                DiffOp::Replace {
                    old_len, new_len, ..
                } => {
                    expect_removed += old_len as i32;
                    expect_added += new_len as i32;
                }
                DiffOp::Equal { .. } => {}
            }
        }
        assert_eq!(added, expect_added);
        assert_eq!(removed, expect_removed);
    }

    #[test]
    fn 普通编辑往返() {
        roundtrip(
            "第一段\n第二段\n第三段\n",
            "第一段\n改过的第二段\n第三段\n第四段\n",
        );
    }

    #[test]
    fn 空文本往返() {
        roundtrip("", "新内容\n");
        roundtrip("有内容\n", "");
        roundtrip("", "");
    }

    #[test]
    fn 无尾换行往返() {
        roundtrip("a\nb\nc", "a\nb2\nc\nd");
        roundtrip("a\nb\nc\n", "a\nb\nc");
    }

    #[test]
    fn 单超长行退化往返() {
        // 行级 diff 对单超长行没有增量收益，但往返正确性必须保持。
        let long_line: String = "x".repeat(200_000);
        roundtrip(&long_line, &format!("{long_line}尾部追加"));
    }

    #[test]
    fn 全文重写往返() {
        roundtrip("旧全文\n旧全文\n旧全文\n", "全新全文\n完全不同\n");
    }

    #[test]
    fn 未变化为纯拷贝() {
        let (ops, added, removed) = encode_delta("一样\n一样\n", "一样\n一样\n");
        assert_eq!(added + removed, 0);
        assert_eq!(apply_delta("一样\n一样\n", &ops).unwrap(), "一样\n一样\n");
    }

    #[test]
    fn 序列化往返() {
        let (ops, _, _) = encode_delta("a\nb\nc\n", "a\nB\nc\nd\n");
        let json = serde_json::to_string(&ops).unwrap();
        let back: Vec<super::DeltaOp> = serde_json::from_str(&json).unwrap();
        assert_eq!(apply_delta("a\nb\nc\n", &back).unwrap(), "a\nB\nc\nd\n");
    }

    #[test]
    fn 增量越界报错() {
        let ops = vec![super::DeltaOp::Copy { from: 0, len: 10 }];
        assert!(apply_delta("只有一行\n", &ops).is_err());
    }

    #[test]
    fn 切行保留行尾() {
        assert_eq!(split_lines("a\nb\nc"), vec!["a\n", "b\n", "c"]);
        assert_eq!(split_lines("").len(), 0);
        assert_eq!(split_lines("\n").len(), 1);
    }
}
