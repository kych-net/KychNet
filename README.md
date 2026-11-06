# KychNet

新版个人网站。

## 架构

前后端分离,后端使用最高性能的方式.

前端部署在 edgeone 和 cloudflare 上,分为内容前端和管理前端:

- 内容前端是纯静态站,由后端编译输出 html.
- 管理前端是动态的,以 SPA 形式通过 JSON API 读写内容.

后端为单一二进制 `kychnet`,同时提供 HTTP 服务与命令行管理能力,两者共用同一套 service 层.

## 内容格式

每篇内容在数据库中记录一种主格式:

| 格式 | 渲染方式 |
| --- | --- |
| markdown | pulldown-cmark,保留 raw HTML 透传 |
| html | 原样注入模板,不做转义与过滤 |
| typst | typst 编译为 HTML,并同时输出 PDF 供下载 |

三种格式均不做内容限制.内容作者均为邀请注册的可信用户,因此允许存储型 XSS 是预期内的取舍;通过子域隔离降低影响面(内容站 `www`、管理端 `admin`、API `api`,Cookie 仅作用于 `admin` 与 `api`).

## 界面

- 内容端顶栏:左侧站点图标,中间为可编辑导航链接(管理端「站点设置」维护),右侧为主题切换与管理端入口.
- 管理端顶栏:左侧站点图标,中间为功能导航(内容/邀请码/用户/站点设置),右侧为头像菜单(用户设置/网站管理/前往内容端)与主题切换.
- 两端共用浅色、深色两套主题,基于 CSS 变量实现,选择持久化在 localStorage.
- 用户头像为上传的 SVG 文件,由服务端校验确为纯矢量:必须是合法 XML 且根元素为 `svg`,至少包含一个矢量图形,禁止内嵌/外链位图、`<script>`、`<foreignObject>`、事件属性与外部资源;上限 256 KB.


## 技术栈

| 层 | 选型 |
| --- | --- |
| 后端框架 | Rust + Axum + Tokio |
| 静态生成 | Askama 模板 + `build` 子命令 |
| 内容渲染 | pulldown-cmark / typst |
| 数据库 | PostgreSQL + SQLx |
| 认证 | webauthn-rs(Passkey) + totp-rs(TOTP) |
| 会话 | tower-sessions + HttpOnly/Secure/SameSite Cookie |
| 命令行 | clap |
| 管理前端 | React + TypeScript + Vite |
| 数据与表单 | 原生 fetch + React state |
| 样式 | 原生 CSS |
| 部署 | Cloudflare Pages + EdgeOne Pages |

## 目录结构

```text
.
├── src/
│   ├── main.rs          # kychnet 入口,子命令分发
│   ├── cli/             # 管理子命令实现
│   ├── auth/            # passkey / totp / 会话
│   ├── render/          # markdown / html / typst 渲染
│   ├── domain/          # 内容与用户模型
│   ├── templates/       # Askama 模板
│   └── db/              # SQLx 查询与迁移
└── admin/               # 管理前端(独立工程)
```

## 命令行

```text
kychnet serve                          # 启动 HTTP 服务
kychnet build --out dist               # 生成静态站
kychnet db migrate                     # 执行迁移

kychnet invite create --uses 1 --expires 7d
kychnet invite list
kychnet invite revoke <code>

kychnet user create --admin          # 创建管理员并输出 TOTP 密钥
kychnet user create                  # 创建普通成员
kychnet user list
kychnet user disable <user>
kychnet passkey reset <user>
```

## 注册与登录

- 注册必须持有有效邀请码.邀请码可由 CLI 或管理员在管理前端生成,支持设置使用次数与有效期.
- 邀请码由 CSPRNG 生成、Base32 编码,长度不少于 16 字符(约 80 bit 熵).
- 注册用户默认为普通成员;管理员通过 `kychnet user create --admin` 创建.
- 普通成员可管理内容与个人设置;邀请码、用户管理、站点设置仅管理员可见.
- 登录支持通行密钥(Passkey)与 TOTP 动态验证码,用户注册后可绑定任一或全部.
- 会话存放于 HttpOnly + Secure + SameSite Cookie,不落地到前端存储.

## 开发

```bash
# 后端
cp .env.example .env        # DATABASE_URL / ADMIN_ORIGIN / WEBAUTHN_RP_ID / WEBAUTHN_ORIGIN
cargo run -- db migrate
cargo run -- serve

# 生成静态站
cargo run -- build --out dist

# 管理前端
cd admin && pnpm install && pnpm dev
```

## 构建与部署

1. CI 执行 `kychnet build --out dist`,生成静态产物;Typst 页面同时产出 `.pdf`.
2. `dist/` 同时发布到 Cloudflare Pages 与 EdgeOne Pages.
3. 管理前端构建为静态产物,单独发布.
4. 媒体资源放在单一对象存储源,由两家 CDN 回源同一份.

## 约定

- 内容以数据库为唯一事实源,构建直接读库,不调用线上 API.
- 站点名称、图标与顶栏导航存于 `site_settings`,由管理端编辑,构建时读取.
- 模板与 API 共用同一套领域模型,避免两端口径漂移.
- Typst 编译在断网容器内执行,并用 `--root` 限定输入目录,避免内容作者访问构建机文件系统.
- Typst 产物按内容哈希缓存,避免每次构建全量重编译.
- 静态产物保持平台中立:只用相对路径、不写死域名,重定向与 404 分别在各平台配置.
- 静态资源使用哈希文件名并长缓存,HTML 短缓存;压缩交给 CDN.
