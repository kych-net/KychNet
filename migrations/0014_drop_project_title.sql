-- 项目名就是唯一显示名，不再单独保存标题。
ALTER TABLE projects DROP COLUMN IF EXISTS title;
