-- 内容 slug 从「无扩展名的短名」改为「输出路径」：
--   index.html / xef/index.html / reff/93df/l.html
-- 旧的 posts/<slug>.html 由构建层统一处理，这里只补 .html 后缀。
UPDATE contents
SET slug = slug || '.html'
WHERE slug NOT LIKE '%.html';

-- 网站主页固定为 index.html：可编辑，但 API 禁止删除。
-- 默认用 HTML，编辑器里显示为 /index.html；也可以改成 /index.md 或 /index.typ。
INSERT INTO contents (id, slug, title, format, body, published)
SELECT
    gen_random_uuid(),
    'index.html',
    'KychNet',
    'html',
    '<h1>KychNet</h1><p>欢迎来到 KychNet。</p>',
    TRUE
WHERE NOT EXISTS (
    SELECT 1 FROM contents WHERE slug = 'index.html'
);
