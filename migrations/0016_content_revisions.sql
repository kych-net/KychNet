-- 普通文章的精简版历史：单文件、单线版本，正文以行级增量存储，
-- 每 20 个增量或增量超过正文 40% 时落一次全文检查点。
--
-- contents.rev_seq 是「当前版本指针」：指向正在展示的版本。
-- 回滚 = 直接移动指针，不产生新版本；下一次保存的 parent_seq 取当前指针。

CREATE TABLE IF NOT EXISTS content_revisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    content_id UUID NOT NULL REFERENCES contents(id) ON DELETE CASCADE,
    seq INT NOT NULL,
    parent_seq INT,
    kind TEXT NOT NULL CHECK (kind IN ('full', 'delta')),
    slug TEXT NOT NULL,
    format TEXT NOT NULL,
    data TEXT NOT NULL,
    added INT NOT NULL DEFAULT 0,
    removed INT NOT NULL DEFAULT 0,
    author_id UUID REFERENCES users(id) ON DELETE SET NULL,
    message TEXT NOT NULL DEFAULT '',
    byte_size INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (content_id, seq)
);

CREATE INDEX IF NOT EXISTS content_revisions_content_idx
    ON content_revisions (content_id, seq DESC);

ALTER TABLE contents ADD COLUMN IF NOT EXISTS rev_seq INT NOT NULL DEFAULT 0;

-- 为现存内容补一条 seq = 1 的全文基线，迁移后所有文章都能从头比较。
INSERT INTO content_revisions
    (content_id, seq, parent_seq, kind, slug, format, data, byte_size, message, created_at)
SELECT id, 1, NULL, 'full', slug, format, body, octet_length(body),
       '基线版本（迁移生成）', created_at
FROM contents
WHERE NOT EXISTS (
    SELECT 1 FROM content_revisions r WHERE r.content_id = contents.id
);

UPDATE contents SET rev_seq = 1 WHERE rev_seq = 0;
