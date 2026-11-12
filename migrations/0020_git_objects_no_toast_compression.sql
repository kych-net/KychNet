-- 应用层已对 git_objects.data 做 zlib 压缩（见 0019），PostgreSQL 再对
-- bytea 做 pglz 几乎压不动，却仍要付压缩与 detoast 的 CPU。
-- EXTERNAL 保留行外存储，只关掉这一层重复压缩。
--
-- 只影响新写入或被重写的行；历史行保持原样，
-- 需要一次性重写可执行 VACUUM FULL git_objects。
ALTER TABLE git_objects ALTER COLUMN data SET STORAGE EXTERNAL;
