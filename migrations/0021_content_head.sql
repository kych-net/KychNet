-- 页面自定义 <head> 内容：管理员可为单篇内容追加 meta / link / script 等。
--
-- 只对能渲染成 HTML 的格式有意义（markdown / html / wikidot / typst）；
-- css 与 svg 不产出页面，构建时会忽略这个字段。
--
-- 内容原样注入模板，不做转义——这是管理员专属能力，等价于直接改页面源码。
-- 注入点固定在 </head> 之前，并拒绝出现闭合标签，避免把整页结构撑破。
ALTER TABLE contents ADD COLUMN IF NOT EXISTS head TEXT NOT NULL DEFAULT '';
