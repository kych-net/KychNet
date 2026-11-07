-- 增加 CSS 内容格式，用于 /css/ 下的自定义样式文件。
DO $$
DECLARE
    constraint_name text;
BEGIN
    FOR constraint_name IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'contents'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%format%'
    LOOP
        EXECUTE format('ALTER TABLE contents DROP CONSTRAINT %I', constraint_name);
    END LOOP;
END $$;

ALTER TABLE contents
    ADD CONSTRAINT contents_format_check
    CHECK (format IN ('markdown', 'html', 'typst', 'wikidot', 'css'));
