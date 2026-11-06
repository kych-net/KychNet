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

三种格式均不做内容限制,内容作者均为邀请注册的可信用户,因此允许存储型 XSS 是项目明确的取舍。

必须注意:**子域隔离本身并不能阻止攻击**。`www` / `admin` / `api` 同属一个 eTLD+1(同站),`SameSite` Cookie 不会阻止同站跨源请求,CORS 也只阻止读取响应、不阻止带凭据的写请求被发出。因此后端对所有写接口增加了 Origin 白名单校验(仅放行管理端 origin),挡住内容站 XSS 冒用管理员会话发起写操作。这层防护仍以「内容作者可信」为前提;若要允许不可信作者,需要让 `api` 使用完全独立的站点(不同 eTLD+1)并对内容页加 CSP,而不是只靠子域划分。

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
| 会话 | tower-sessions + HttpOnly/Secure/SameSite=Strict Cookie,登录后轮换会话 ID,有效期 8 小时(当前为 MemoryStore,进程重启后登录态丢失,多实例不共享) |
| 命令行 | clap |
| 管理前端 | React + TypeScript + Vite |
| 数据与表单 | 原生 fetch + React state |
| 样式 | 原生 CSS |
| 部署 | Cloudflare Pages + EdgeOne Pages |

## 目录结构

```text
.
├── src/
│   ├── main.rs          # 入口与 clap 子命令分发
│   ├── config.rs        # 环境变量配置
│   ├── db.rs            # 连接池与迁移
│   ├── models.rs        # 领域模型(用户/邀请码/内容/站点设置)
│   ├── service.rs       # 数据库访问层,API 与 CLI 共用
│   ├── auth.rs          # Passkey / TOTP / 认证流程
│   ├── render.rs        # markdown / html / typst 渲染
│   ├── svg.rs           # 上传头像的 SVG 校验
│   ├── api.rs           # Axum 路由、会话、CORS、Origin 校验
│   └── build.rs         # 静态站生成
├── templates/           # Askama 模板(layout / index / page)
├── migrations/          # SQLx 迁移
└── admin/               # 管理前端(独立 Vite 工程)
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

## kych-net（中文命令行）

英文 CLI 叫 `kychnet`，中文入口的二进制叫 `kych-net`。中文命令按中文语序书写，且**连写、不加空格**：

```text
kych-net 启动服务
kych-net 生成静态站到public
kych-net 创建邀请码可用三次有效期七天
kych-net 作废邀请码ABCD1234
kych-net 创建管理员爱丽丝
kych-net 创建用户爱丽丝设为管理员
kych-net 重置爱丽丝的通行密钥
```

**用户名直接用中文，不需要显示名。** 后端 `validate_username` 接受 Unicode（单测里 `张三丰` 通过），英文 CLI 创建用户时若不传 `--display`，会用 `display.unwrap_or_else(|| username.clone())` 把显示名设成用户名。所以中文命令直接写 `创建用户爱丽丝` 即可，不提供「名为…」这类把英文 `--display` 硬译过来的写法；误写会给出中文提示。

中文不像英文靠空格分词，程序先去掉空白，再按中文短语切出命令名、数量、时长、所属与参数；支持中文数字（`三次`）、中文时长（`七天`、`永不过期`）和多种说法（`运行服务`、`撤销邀请码`、`解禁用户`）。解析后直接交给同一个 `kychnet` 执行，与英文 CLI 共用 service 层。

### 中文名「跨越晨昏」：在服务器环境变量里设置别名

中文命令名由**服务器环境变量/初始化文件里的别名**提供，不改动二进制与源码名。最直接的是 `alias`：

| 场景 | 写入位置 | 内容 |
| --- | --- | --- |
| 所有登录用户（推荐） | 新建 `/etc/profile.d/kychnet-alias.sh` | `alias 跨越晨昏='kych-net'` |
| 单个用户 | `~/.bashrc` / `~/.zshrc` / `~/.profile` | 同上 |
| 仅当前会话 | 直接执行 | `alias 跨越晨昏='kych-net'` |

前提是 `kych-net` 所在目录已在服务器环境变量 `PATH` 里（例如装到 `/usr/local/bin`；若放在自定义目录，在同一份脚本里先加 `export PATH=/opt/kychnet:$PATH`）。重新登录后：

```bash
alias 跨越晨昏                              # 确认别名已生效
跨越晨昏 创建邀请码可用三次有效期七天
```

**一处要注意**：`alias` 是 shell 内置命令，**不是环境变量**，不会出现在 `env` 或 `systemctl show -p Environment` 里；systemd 的 `Environment=`、Docker 的 `ENV`、`/etc/environment` 这些只能设置变量的地方写不了 `alias`。服务托管与容器场景改用 PATH 目录里的**同名链接**作为命令别名，效果一样且不依赖 shell：

```bash
ln -sfn /opt/kychnet/kych-net /usr/local/bin/跨越晨昏
export PATH=/usr/local/bin:$PATH            # 通常已默认包含
```

源码见 `src/bin/kych-net.rs`：零新增依赖，由 Cargo 自动发现为第二个二进制，内含单元测试（`cargo test --bin kych-net`）。运行时需能找到 `kychnet`（`KYCHNET_BIN` → 同目录 → PATH）。

## 注册与登录

- 注册必须持有有效邀请码.邀请码可由 CLI 或管理员在管理前端生成,支持设置使用次数与有效期.
- 邀请码由 CSPRNG 生成、Base32 编码,长度不少于 16 字符(约 80 bit 熵).
- 注册用户默认为普通成员;管理员通过 `kychnet user create --admin` 创建.
- 普通成员可管理内容与个人设置;邀请码、用户管理、站点设置仅管理员可见.
- 登录支持通行密钥(Passkey)与 TOTP 动态验证码,用户注册后可绑定任一或全部.
- 会话存放于 HttpOnly + Secure + SameSite Cookie,不落地到前端存储;登录或注册成功后会轮换会话 ID,避免会话固定.
- 登录相关接口有限流(按用户名与客户端 IP 双路计数):`login/start` 每用户名 10 次/分钟、`totp/login` 每账号 5 次/15 分钟(成功即清零)、注册每 IP 10 次/分钟,超额返回 429.限流是进程内的,重启清零、多实例各自计数,要跨实例统一得换集中存储.
- 客户端 IP 默认取 TCP 对端地址;只有确实只经可信 CDN/反代进来时才设 `TRUST_PROXY_HEADERS=1` 改读 `X-Forwarded-For`——这个头任何人都能伪造,信错了等于让攻击者自己挑额度.
- 绑定 TOTP 时新密钥先进 `pending_secret`,输入正确验证码确认后才替换旧密钥;中途放弃不会把原来能用的 TOTP 打死.
- `login/start` 与 `totp/login` 的失败响应统一成同一条消息:这两个接口匿名可调,"用户不存在""账号已禁用""没绑定通行密钥"如果是可区分的,就等于在向外枚举用户名和每个人的 2FA 配置.真实原因只进日志.
- 过期的 `auth_flows` 由 `serve` 内的后台任务每 60 秒清理一次;`login_start` 每次调用都会插一行,不清理就会被匿名请求撑爆.

## 开发

```bash
# 后端
cp .env.example .env        # DATABASE_URL / ADMIN_ORIGIN / WEBAUTHN_RP_ID / WEBAUTHN_ORIGIN
cargo run -- db migrate
cargo run -- serve

# 生成静态站
cargo run -- build --out dist

# 管理前端
cd admin && pnpm install && cp .env.example .env && pnpm dev
```

开发时 `VITE_API_BASE_URL` 留空,`/api` 由 vite 代理转发到后端。生产构建前必须把它设为 API 服务的完整 origin(例如 `https://api.kych.net`),因为管理前端是独立静态站,没有同源 `/api` 可转发;后端 CORS 与写接口 Origin 白名单均以 `ADMIN_ORIGIN` 为准。

## 构建与部署

1. [.github/workflows/build.yml](.github/workflows/build.yml) 在 push、PR 和手动触发时运行测试,并编译 Linux 二进制;推送 `v*` 标签时自动创建 Release.
   - `x86_64-unknown-linux-gnu`:依赖系统 glibc,在 Ubuntu 22.04 上构建.
   - `x86_64-unknown-linux-musl`:静态链接,启用 `vendored-openssl` 特性,适配 Alpine 与老版本发行版.
2. 先执行 `kychnet db migrate`,再执行 `kychnet build --out dist`,生成静态产物;Typst 页面同时产出 `.pdf`.构建会先渲染到内存,全部成功后才清理并重写 `posts/` 与 `index.html`,中途失败不会破坏现有站点.
3. `dist/` 同时发布到 Cloudflare Pages 与 EdgeOne Pages.
4. 管理前端构建为静态产物,单独发布.
5. 媒体资源放在单一对象存储源,由两家 CDN 回源同一份.

## 约定

- 内容以数据库为唯一事实源,构建直接读库,不调用线上 API.
- 站点名称、图标与顶栏导航存于 `site_settings`,由管理端编辑,构建时读取.
- 内容 slug 允许任意 Unicode 字母与数字(中文、日文等)以及 `-` `_`;禁止空格、标点、路径分隔符 `/` `\`、点号 `.`、URL 保留字符(`?` `#` `%` 等)与控制字符,UTF-8 长度上限 180 字节.写入时即校验,避免脏数据卡死构建.链接中的非 ASCII slug 会按 `%XX` 编码,磁盘文件名仍是原始 UTF-8;带变音符号的文字请使用 NFC 预组合形式.
- 模板与 API 共用同一套领域模型,避免两端口径漂移.
- Typst 在当前进程内编译,由 `VirtualPath::resolve(root)` 加 canonicalize 双重校验把文件读取限制在 `CONTENT_ROOT` 内,避免内容作者通过 `#include` 读到构建机上的任意文件;生产仍建议放进断网容器并加 CPU/内存限额,构建机上的 `.env` 与云凭据不要落在可读目录.
- 编译缓存尚未实现;目前每篇 Typst 内容会分别编译 `PagedDocument`(出 PDF)与 `HtmlDocument`(出 HTML)各一次.
- 静态产物保持平台中立:只用相对路径、不写死域名,重定向与 404 分别在各平台配置.
- 静态资源使用哈希文件名并长缓存,HTML 短缓存;压缩交给 CDN.
