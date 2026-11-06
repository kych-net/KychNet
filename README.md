# KychNet

新版个人网站。

## 架构

前后端分离,后端使用最高性能的方式.

前端部署在 edgeone 和 cloudflare 上,分为内容前端和管理前端:

- 内容前端是纯静态站,由后端编译输出 html.
- 管理前端是动态的,以 SPA 形式通过 JSON API 读写内容.

## 技术栈

| 层 | 选型 |
| --- | --- |
| 后端框架 | Rust + Axum + Tokio |
| 静态生成 | Askama 模板 + `build` 子命令 |
| Markdown | pulldown-cmark |
| 数据库 | PostgreSQL + SQLx |
| 鉴权 | Argon2 + HttpOnly Cookie 会话 |
| 管理前端 | React + TypeScript + Vite |
| 数据与表单 | TanStack Query + React Hook Form + Zod |
| 样式 | Tailwind CSS + shadcn/ui |
| 部署 | Cloudflare Pages + EdgeOne Pages |

## 目录结构

```text
.
├── src/
│   ├── main.rs          # bin: api —— 管理 API
│   ├── bin/build.rs     # bin: build —— 读库、渲染、输出 dist/
│   ├── domain/          # 内容模型,两个 bin 共享
│   ├── templates/       # Askama 模板
│   └── db/              # SQLx 查询与迁移
└── admin/               # 管理前端(独立工程)
```

当前仓库尚未包含实现代码,以上为规划结构.

## 开发

```bash
# 后端 API
cp .env.example .env        # DATABASE_URL / SESSION_SECRET
cargo run --bin api

# 生成静态站
cargo run --bin build -- --out dist

# 管理前端
cd admin && pnpm install && pnpm dev
```

## 构建与部署

1. CI 执行 `cargo run --bin build -- --out dist`,生成静态产物.
2. `dist/` 同时发布到 Cloudflare Pages 与 EdgeOne Pages.
3. 管理前端构建为静态产物,单独发布.
4. 媒体资源放在单一对象存储源,由两家 CDN 回源同一份.

## 约定

- 内容以数据库为唯一事实源,构建直接读库,不调用线上 API.
- 模板与 API 共用同一套领域模型,避免两端口径漂移.
- 静态产物保持平台中立:只用相对路径、不写死域名,重定向与 404 分别在各平台配置.
- 静态资源使用哈希文件名并长缓存,HTML 短缓存;压缩交给 CDN.
