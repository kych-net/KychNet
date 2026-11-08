# `/git/` 文件树接口

## 为什么需要它

站内 Git 仓库**没有磁盘工作树，也没有 `.git`**。所谓「文件树」是把 PostgreSQL
`git_objects` 里的 commit / tree / blob 即时展开成 `BTreeMap<路径, 字节>`：

| 使用方 | 入口 | 读法 |
| --- | --- | --- |
| 管理后端 | `GET /api/projects/{id}/tree?ref=&path=`、`/file?ref=&path=` | `git::tree::list_dir()` 逐级展开 + `find_file()` 单文件，需 session 登录，文本编辑器只收 UTF-8 |
| 管理前端 | `admin/src/components/DirTree.tsx` / `views/ProjectsView.tsx` | 把接口返回的扁平 `Content[]` 在浏览器里拼成目录树，懒加载 |
| 静态站构建 | `project::snapshots()` → `BuildCtl::build_all()` | `git::tree::read_files()` 全量展开 |
| runner 快照 | `workflow::snapshot::build_snapshot()` | 同样是 `read_files()` 全量展开，再打包 tar.gz |

全站共用 `src/git/tree.rs` 这一层。`git-upload-pack` 读的也是同一份对象，
只是输出 **packfile**——调用方必须解包、checkout 才能拿到文件，通常意味着一个 `.git`。

**LFS 已经实现**：`src/git/lfs.rs` 有 pointer 解析、`store` / `load` / `batch` / `remove`，
`/git/<项目>.git/info/lfs/objects/<oid>` 上传下载齐全。`git::worktree::read()` /
`collect()` 会还原 pointer，`project::write_file_bytes()` 会把二进制写成 LFS pointer。
管理端 `/api/projects/{id}/file` 的文本编辑器仍只收 UTF-8（返回值是 `String`），
二进制请走下面 `/git/<项目>/file`。

## 两类通道并列

`/git/{*path}` 一条通配路由，由 `git::http::split_git_path()` 按第一段是否以
`.git` 结尾分流：

| 通道 | 路径 | 用途 |
| --- | --- | --- |
| Smart HTTP | `/git/<项目>.git/...` | Git 客户端 clone / push / LFS，`info/refs`、`git-upload-pack`、`git-receive-pack` **行为不变** |
| 文件树 | `/git/<项目>/...` | 不装 git 客户端、不建 `.git`、不解析 packfile，直接读写展开后的文件树 |

## 读（公开只读）

与 `git-upload-pack` 同等可见性：站内仓库内容本来就能匿名 fetch，`git_project()`
只按 slug 查项目并排除已归档。packfile 已包含全部内容，文件树接口**没有放大暴露面**。

`ref` 支持**分支名或 40 位 commit OID**。执行器从 claim 拿到的是 `commit_oid`
而不是分支名；按 OID 读可避免 run 排队期间分支被新 push 顶掉。

### `GET /git/<项目>/tree?ref=<ref>&path=<dir>`

单层目录列表，与管理端 `project_tree` 的 `items` 同构（`git::tree::DirItem`）。
`path` 省略或为空表示仓库根。

```json
{
  "commit": "1111111111111111111111111111111111111111",
  "path": "docs",
  "items": [
    { "name": "README.md", "path": "docs/README.md", "is_dir": false,
      "size": 128, "oid": "2222…" }
  ]
}
```

### `GET /git/<项目>/file?ref=<ref>&path=<path>`

返回文件**原始字节** `application/octet-stream`，**不限 UTF-8、还原 LFS pointer**。
响应头：

| 头 | 含义 |
| --- | --- |
| `x-git-blob-oid` | tree 里记录的 blob OID（LFS 还原后仍是 pointer 的 OID） |
| `x-git-lfs` | 值为 `1` 表示内容来自 LFS，不是仓库里的 pointer 字节 |

文件不存在返回 404。

### `GET /git/<项目>/archive?ref=<ref>`

整棵工作树，一次返回 `application/gzip`（tar.gz）。tar 内是**仓库相对路径**，
不带 `code/` / `definitions/` 前缀——那是 runner 快照专用的布局。

| 头 | 含义 |
| --- | --- |
| `x-git-commit` | 实际展开的 commit OID |
| `x-git-files` | 文件数 |
| `x-git-bytes` | 原始字节数（未压缩） |

超过 `MAX_WORKTREE_BYTES`（默认 200 MiB）返回 413。打包复用
`workflow::snapshot::write_tar_gz()`，主服务只有一份 USTAR 实现。

## 写（登录用户）

写操作统一由 `git_write_user()` 认证：**Git 令牌优先，登录会话兜底**；
禁用账号两条路径都不放行，匿名返回 401。也就是说登录用户不必另建 Git 令牌
即可编辑文件树，Git 客户端继续用原有令牌，互不影响。

同一套认证也用于 smart HTTP 的写路径，因此**登录会话现在也能 push**：
`info/refs?service=git-receive-pack`、`git-receive-pack`、`info/lfs/objects/batch`
（upload）、LFS 对象 PUT 全部接受 session。读路径（upload-pack、LFS 下载）仍匿名。

> 权限模型与管理端一致：`/api/projects/{id}/*` 本来就是「登录用户 + 有效会话」
> 即可操作，不做项目级 ACL。如果将来要给项目加成员权限，需同时收紧这两处。

### `PUT /git/<项目>/file?ref=<分支>&path=<路径>[&message=..][&base_oid=..]`

请求体就是文件**原始字节**，直接生成一个新的 commit：

- UTF-8 文本 → 直接写进 blob。
- 二进制 → `lfs::store()` 落盘并登记 `lfs_objects`，仓库里存 LFS pointer；
  读取时 `/file` 与 `/archive` 都会还原成真实字节。
- `base_oid` 是乐观锁（文件当前 blob OID），不匹配返回 400，避免并发覆盖。
- 复用 `project::write_file_bytes()`：项目锁、`ensure_no_output_conflict()`、
  `commit_files()` 与管理端 `write_file()` 完全一致。
- 写在默认分支时触发一次静态站构建（与管理端写文件一致）。

返回 `{"item": {"commit", "path", "oid"}}`。写只接受**分支名**（要更新 ref），
不接受 commit OID。

### `DELETE /git/<项目>/file?ref=<分支>&path=<路径>[&message=..]`

复用 `project::delete_file()`，默认分支同样触发构建，成功返回 204。
项目主页 `index` 不允许删除（与现有规则一致）。

## 与执行器的关系

KychRunner 仍走 `/api/runner/jobs/{id}/snapshot?kind=both`（tar 布局为
`code/` + `definitions/`，定义只取默认分支 head），它带 `executor_id` /
`in_progress` / 租约 / commit 归属四重校验。**文件树接口是公开只读 + 登录可写**，
没有「只能读某个 run 绑定的 commit」这种约束，因此执行器不应直接改用它。
若要切换，需同步调整契约、`protocol_version` 与鉴权模型，不在本次改动内。

## 实现位置

| 文件 | 内容 |
| --- | --- |
| `src/git/http.rs` | `GitChannel` 与 `split_git_path()`：按 `.git` 后缀分流两类通道 |
| `src/git/worktree.rs` | `resolve_ref` / `list` / `read` / `collect` / `total_bytes`，复用 `git::tree` 与 `git::lfs` |
| `src/api.rs::git_write_user` | Git 令牌 + 登录会话的双通道写认证 |
| `src/api.rs::worktree_get` / `worktree_put` / `worktree_delete` | 读写 handler |
| `src/project.rs::write_file_bytes` | 字节写入：文本直存 blob，二进制走 LFS，生成 commit |
| `src/workflow/snapshot.rs` | `write_tar_gz` 放开为 `pub(crate)` 供 `archive` 复用 |
