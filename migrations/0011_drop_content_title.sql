-- 页面标题改由正文自己提供，内容不再保存单独的 title 字段。
ALTER TABLE contents DROP COLUMN IF EXISTS title;
