# `/git/` 工作树接口

## 为什么需要它

站内 Git 仓库**没有磁盘工作树，也没有 `.git`**。所谓「工作树」是把 PostgreSQL
`git_objects` 里的 commit / tree / blob 即时展开成 `BTreeMap<路径, 字节>`：

| 使用方 | 入口 | 读法 |
| --- | --- | --- |
| 管理后端 | `GET /api/projects/{id}/tree?ref=&path=`、`/file?ref=&path=` | `git::tree::list_dir()` 逐级展开 + `find_file()` 单文件，需 session 登录，只接受 UTF-8 文本 |
| 管理前端 | `admin/src/components/DirTree.tsx` / `views/ProjectsView.tsx` | 把接口返回的扁平 `Content[]` 在浏览器里拼成目录树，懒加载 |
| 静态站构建 | `project::snapshots()` → `BuildCtl::build_all()` | `git::tree::read_files()` 全量展开 |
| runner 快照 | `workflow::snapshot::build_snapshot()` | 同样是 `read_files()` 全量展开，再打包 tar.gz |

也就是说，全站共用 `src/git/tree.rs` 这一层。`git-upload-pack` 通道读的也是同一份对象，
只是输出 **packfile**——调用方必须解包、checkout 才能拿到文件，通常意味着一个 `.git`。

本次在 `/git/` 下**并列新增**工作树接口：不动 packfile 通道，直接把展开后的工作树
通过 HTTP 交出去，调用方不需要 git 二进制、不需要 `.git`、不需要解析 ofs-delta。

## 现有通道（不变）

```
GET  /git/<slug>.git/info/refs?service=git-upload-pack
POST /git/<slug>.git/git-upload-pack          → packfile
POST /git/<slug>.git/git-receive-pack         → 需 Git 令牌
GET  /git/<slug>.git/info/lfs/objects/<oid>   → LFS 对象
```

## 新增端点

三个端点都接受 `ref`：**分支名**或 **40 位 commit OID**。执行器从 claim 拿到的是
`commit_oid` 而不是分支名；按 OID 读可避免 run 排队期间分支被新 push 顶掉。

### `GET /git/<slug>.git/worktree/tree?ref=<ref>&path=<dir>`

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

### `GET /git/<slug>.git/worktree/file?ref=<ref>&path=<path>`

返回文件**原始字节** `application/octet-stream`。与管理端 `/file` 的区别：
不限制 UTF-8 文本，且 **LFS pointer 会还原成真实内容**。响应头：

| 头 | 含义 |
| --- | --- |
| `x-git-blob-oid` | tree 里记录的 blob OID（LFS 还原后仍是 pointer 的 OID） |
| `x-git-lfs` | 值为 `1` 表示内容来自 LFS，不是仓库里的 pointer 字节 |

文件不存在返回 404。

### `GET /git/<slug>.git/worktree/archive?ref=<ref>`

整棵工作树，一次返回 `application/gzip`（tar.gz）。tar 内是**仓库相对路径**，
不带 `code/` / `definitions/` 前缀——那是 runner 快照专用的布局。

| 头 | 含义 |
| --- | --- |
| `x-git-commit` | 实际展开的 commit OID |
| `x-git-files` | 文件数 |
| `x-git-bytes` | 原始字节数（未压缩） |

超过 `MAX_WORKTREE_BYTES`（默认 200 MiB）返回 413。打包复用
`workflow::snapshot::write_tar_gz()`，主服务只有一份 USTAR 实现。

## 认证与可见性

沿用 `/git/` 现有语义：`info/refs?service=git-upload-pack`、`git-upload-pack`、
LFS 下载本来就是**公开只读**（`git_project()` 只按 slug 查项目并排除已归档），
写操作才需要 `git_tokens`。packfile 已经包含仓库全部内容，所以工作树接口
**没有放大暴露面**。

如果将来要给 `/git/` 整体加认证，这三个端点会一并生效，不需要单独改动。
需要「只能读某个 run 绑定的 commit」这种更细的约束时，仍应走
`/api/runner/jobs/{id}/snapshot`——它带 `executor_id` / `in_progress` / 租约 /
commit 归属四重校验。

## 与执行器的关系

当前 KychRunner 仍走 `/api/runner/jobs/{id}/snapshot?kind=both`（tar 布局为
`code/` + `definitions/`，定义只取默认分支 head）。本接口是 `/git/` 的一等能力，
执行器若要切换，可用 `worktree/archive?ref=<commit_oid>` 取代码、再取
`ref=<def_commit_oid>` 取定义；切换需要同时调整契约与 `protocol_version`，
不在本次改动内。

## 实现位置

| 文件 | 内容 |
| --- | --- |
| `src/git/worktree.rs` | `resolve_ref` / `list` / `read` / `collect` / `total_bytes`，复用 `git::tree` 与 `git::lfs` |
| `src/api.rs::worktree_get` | 路由分发与响应组装 |
| `src/git/mod.rs` | 注册 `pub mod worktree` |
| `src/workflow/snapshot.rs` | `write_tar_gz` 放开为 `pub(crate)` 供 archive 复用 |
