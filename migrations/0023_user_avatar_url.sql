-- 头像支持直接填链接：外部 URL 或以 / 开头的站内路径。
-- 与历史 avatar_svg 并存：avatar_url 非空时优先使用，为空则回退到上传的 SVG。
ALTER TABLE users ADD COLUMN IF NOT EXISTS avatar_url TEXT NOT NULL DEFAULT '';
