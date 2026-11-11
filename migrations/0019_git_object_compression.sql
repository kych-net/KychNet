-- Git 对象改为应用层 zlib 压缩存储（flate2），而不是只依赖 TOAST。
--
-- 为什么不靠 TOAST：PostgreSQL 的 pglz 对文本类 blob 只能压掉约 40%
-- （实测 1.6 MB -> 976 KB），而 zlib 能到 60-75%；而且 `pg_dump` 导出的是
-- 逻辑数据，TOAST 的压缩不会体现在备份体积里。应用层压缩对两者都有效。
--
-- size 仍记原始字节数（OID 由它算出来），compressed 标记 data 是否为 zlib 流。
-- 小于阈值的对象不压缩（zlib 头尾约 11 字节，小对象压了反而变大）。
ALTER TABLE git_objects ADD COLUMN IF NOT EXISTS compressed BOOLEAN NOT NULL DEFAULT false;

-- 旧行保持 compressed = false，由 `kych-net db compress` 分批压缩。
-- 不放在迁移里做：SQL 没有 zlib，而一次性全表改写会在迁移阶段卡住启动。
