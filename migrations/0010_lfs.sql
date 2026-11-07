-- Git LFS 对象索引。二进制内容放在 LFS_ROOT 指向的文件系统里，
-- 这里只记录项目、OID 与大小，便于批量接口判断对象是否存在。
CREATE TABLE IF NOT EXISTS lfs_objects (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    oid text NOT NULL,
    size bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, oid)
);
