ALTER TABLE users
    ADD COLUMN IF NOT EXISTS avatar_svg TEXT CHECK (length(avatar_svg) <= 262144);
