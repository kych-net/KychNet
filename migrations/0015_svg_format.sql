-- 原始 SVG 文件作为一等内容格式，不再套 HTML 模板。
ALTER TABLE contents DROP CONSTRAINT IF EXISTS contents_format_check;
ALTER TABLE contents ADD CONSTRAINT contents_format_check
    CHECK (format IN ('markdown', 'html', 'typst', 'wikidot', 'css', 'svg'));
