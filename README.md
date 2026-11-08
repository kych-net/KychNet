# KychNet

新版个人网站。

## 架构

前后端分离,后端使用最高性能的方式.

前端分为内容前端和管理前端:

- 内容前端是纯静态站,由后端编译输出 html,通过 EdgeOne 发布.
- 管理前端是 React SPA,通过 EdgeOne Pages 发布,以 JSON API 读写内容.

后端为单一二进制 `kych-net`,同时提供 HTTP 服务、Git smart HTTP、MCP 和命令行管理能力,共用同一套 service 层.中英文命令行是同一个二进制的两个入口:中文命令先被翻译成英文参数,再交给同一个 clap 解析器.

## 域名与入口

| 地址 | 用途 |
| --- | --- |
| `kych.net` | 公网 API:`/api/*`、`/git/*`、`/mcp`;裸域其余路径 301 到内容站 |
| `api.kych.net` | **仅 EdgeOne 回源 Host / 静态源站**,不是给客户端使用的 API 地址 |
| `c.kych.net` | 内容站公开入口,EdgeOne 加速回源 |
| `m.kych.net` | 管理端,EdgeOne Pages |
| `origin.kych.net`、`src.kych.net` | 静态源站/实验入口,不作为 API 地址 |

管理端的 `VITE_API_BASE_URL` 必须是 `https://kych.net`,不能写 `api.kych.net`.

## 内容格式

每篇内容在数据库中记录一种主格式:

| 格式 | 渲染方式 |
| --- | --- |
| markdown | pulldown-cmark,保留 raw HTML 透传 |
| html | 原样注入模板,不做转义与过滤 |
| typst | typst 编译为 HTML,并同时输出 PDF 供下载 |
| wikidot | ftml 解析 Wikidot 语法并渲染 HTML,支持 `[[include]]`;项目因此按 AGPL-3.0 分发 |
| css | 原样写入 `/css/*.css`,供页面显式引用 |

这些格式均不做内容限制,内容作者均为邀请注册的可信用户,因此允许存储型 XSS 是项目明确的取舍.

## 编码与二进制

Git 仓库层按字节存储对象,与标准 git 一致:推送不校验 UTF-8,非 UTF-8 的提交信息(lossy 解析后头部照常解析)和二进制文件都能入库.必须在编辑器里打开或渲染成页面内容时,才要求内容是 UTF-8 文本;二进制文件请走 Git LFS.

内容不保存单独的标题字段:页面标题直接在正文里写,例如 HTML 的 `<h1>…</h1>`、Markdown 的 `# …`、Wikidot 的 `+ …`、Typst 的 `= …`.页面 `<title>`、目录树、首页回退列表、API/MCP 的展示名统一使用源文件名.

必须注意:**子域隔离本身并不能阻止攻击**。`c`(内容站) / `m`(管理端) / `kych.net`(API) 同属一个 eTLD+1(同站),`SameSite` Cookie 不会阻止同站跨源请求,CORS 也只阻止读取响应、不阻止带凭据的写请求被发出。因此后端对所有写接口增加了 Origin 白名单校验(仅放行管理端 origin),挡住内容站 XSS 冒用管理员会话发起写操作。这层防护仍以「内容作者可信」为前提;若要允许不可信作者,需要让 `api` 使用完全独立的站点(不同 eTLD+1)并对内容页加 CSP,而不是只靠子域划分。

## 界面

- 内容端顶栏:左侧站点图标,图标右侧为目录树按钮,中间为可编辑导航链接(管理端「站点设置」维护),右侧为主题切换与管理端入口.
- 管理端顶栏:左侧站点图标,图标右侧为目录树按钮,中间为功能导航(内容/邀请码/用户/站点设置),右侧为头像菜单(用户设置/网站管理/前往内容端)与主题切换.
- 两端共用浅色、深色两套主题,基于 CSS 变量实现,选择持久化在 localStorage.
- 内容站正文使用自托管的 `LXGW WenKai Screen`;管理端编辑器使用 CodeMirror 6,按格式高亮.
- 管理端 Markdown/HTML 预览在浏览器端渲染,Wikidot 预览走服务端 ftml,统一放进无脚本 sandbox iframe.
- 用户头像为上传的 SVG 文件,由服务端校验确为纯矢量:必须是合法 XML 且根元素为 `svg`,至少包含一个矢量图形,禁止内嵌/外链位图、`<script>`、`<foreignObject>`、事件属性、DTD 与实体声明、`<animate>`/`<set>` 等属性动画元素,`href` 只允许指向文档内图元(`#xxx`);内容上限 256 KB,传输上限 257 KB.


## 技术栈

| 层 | 选型 |
| --- | --- |
| 后端框架 | Rust + Axum + Tokio |
| 静态生成 | Askama 模板 + `build` 子命令 |
| 内容渲染 | pulldown-cmark / typst / ftml / CSS |
| 数据库 | PostgreSQL + SQLx |
| 认证 | webauthn-rs(Passkey) + totp-rs(TOTP) |
| 会话 | tower-sessions + Postgres store(schema `tower_sessions`,表 `session`) + HttpOnly/Secure/SameSite=Strict Cookie,登录后轮换会话 ID,有效期 8 小时 |
| 命令行 | clap |
| 管理前端 | React + TypeScript + Vite |
| 数据与表单 | 原生 fetch + React state |
| 样式 | 原生 CSS |
| 部署 | EdgeOne（内容 CDN）+ EdgeOne Pages（管理端） |
| 项目/Git | PostgreSQL Git 对象 + Git smart HTTP + LFS |

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
│   ├── rate.rs          # 登录/注册接口的有界内存限流
│   ├── api.rs           # Axum 路由、会话、CORS、Origin 校验
│   ├── build.rs         # 静态站生成
│   ├── mcp.rs           # MCP Streamable HTTP 只读服务器
│   ├── project.rs       # 项目管理
│   ├── collab.rs        # 分支/PR/讨论协作
│   └── git/             # PostgreSQL Git、LFS、smart HTTP
├── templates/           # Askama 模板(layout / index / page)
├── migrations/          # SQLx 迁移(当前 0001–0011)
└── admin/               # 管理前端(独立 Vite 工程)
```

## 命令行

```text
kych-net serve                          # 启动 HTTP 服务
kych-net build --out dist               # 生成静态站
kych-net build --only xef/index.html    # 只编译一页(管理端保存时调用)
kych-net db migrate                     # 执行迁移

kych-net invite create --uses 1 --expires 7d
kych-net invite list
kych-net invite revoke <code>

kych-net user create --admin          # 创建管理员并输出 TOTP 密钥
kych-net user create                  # 创建普通成员
kych-net user list
kych-net user disable <user>
kych-net user enable <user>
kych-net passkey reset <user>

kych-net gc --project <项目名>   # 清理不可达 Git 对象与孤立 LFS 文件
```

## 中文命令行

同一个二进制 `kych-net` 同时提供中英文两套命令。英文命令直接给 clap；中文命令先被翻译成英文参数，再交给同一个 clap 解析器——因此两者走完全相同的执行路径和同一个 service 层，不需要第二个二进制，也不需要在 PATH 里另找程序。

中文命令按中文语序书写，且**连写、不加空格**：

```text
kych-net 启动服务
kych-net 生成静态站到public
kych-net 创建邀请码可用三次有效期七天
kych-net 作废邀请码ABCD1234
kych-net 创建管理员爱丽丝
kych-net 创建用户爱丽丝设为管理员
kych-net 重置爱丽丝的通行密钥
```

项目只保留**用户名**这一个名字（`display_name` 已移除）；用户名支持中文、日文与 emoji，注册、用户列表和通行密钥的展示名都用它。中文不像英文靠空格分词，程序先去掉空白，再按中文短语切出命令名、数量、时长、所属与参数；支持中文数字（`三次`）、中文时长（`七天`、`永不过期`）和多种说法（`运行服务`、`撤销邀请码`、`解禁用户`）。

`kych-net --help` 打印完整帮助：上半是 clap 的英文子命令列表，下半是中文命令一览，两边共用同一份子命令定义。

### 中文名「跨越晨昏」：在服务器环境变量里设置别名

中文命令名由**服务器环境变量/初始化文件里的别名**提供，不改动二进制与源码名。最直接的是 `alias`：

| 场景 | 写入位置 | 内容 |
| --- | --- | --- |
| 所有登录用户（推荐） | 新建 `/etc/profile.d/kych-net-alias.sh` | `alias 跨越晨昏='kych-net'` |
| 单个用户 | `~/.bashrc` / `~/.zshrc` / `~/.profile` | 同上 |
| 仅当前会话 | 直接执行 | `alias 跨越晨昏='kych-net'` |

前提是 `kych-net` 所在目录已在服务器环境变量 `PATH` 里（例如装到 `/usr/local/bin`；若放在自定义目录，在同一份脚本里先加 `export PATH=/opt/kych-net:$PATH`）。重新登录后：

```bash
alias 跨越晨昏                              # 确认别名已生效
跨越晨昏 创建邀请码可用三次有效期七天
```

**一处要注意**：`alias` 是 shell 内置命令，**不是环境变量**，不会出现在 `env` 或 `systemctl show -p Environment` 里；systemd 的 `Environment=`、Docker 的 `ENV`、`/etc/environment` 这些只能设置变量的地方写不了 `alias`。服务托管与容器场景改用 PATH 目录里的**同名链接**作为命令别名，效果一样且不依赖 shell：

```bash
ln -sfn /opt/kych-net/kych-net /usr/local/bin/跨越晨昏
export PATH=/usr/local/bin:$PATH            # 通常已默认包含
```

中文解析实现在 `src/cli_cn.rs`：只依赖标准库，不新增任何依赖，内含单元测试（`cargo test cli_cn`）。它把原始参数翻译成英文参数交给 clap，不 fork 子进程，因此中文命令的启动开销与英文命令完全一致。

## 注册与登录

- 注册必须持有有效邀请码.邀请码可由 CLI 或管理员在管理前端生成,支持设置使用次数与有效期.
- 邀请码由 CSPRNG 生成、Base32 编码,长度不少于 16 字符(约 80 bit 熵).
- 注册用户默认为普通成员;管理员通过 `kych-net user create --admin` 创建.
- 普通成员可管理内容与个人设置;邀请码、用户管理、站点设置仅管理员可见.
- 登录支持通行密钥(Passkey)与 TOTP 动态验证码,用户注册后可绑定任一或全部.
- 会话存放于 HttpOnly + Secure + SameSite Cookie,不落地到前端存储;登录或注册成功后会轮换会话 ID,避免会话固定.
- 会话记录存在 Postgres(schema `tower_sessions`,表 `session`),由 `serve` 启动时自动建表,每 5 分钟清理过期行;`kych-net db migrate` 不涉及它,但数据库角色需要有建 schema 的权限.换成集中存储后,进程重启不再让所有人掉登录,多实例也能共享会话,内存也不再随会话数无上限增长.代价是每个带 Cookie 的请求多一次按主键的会话读.
- 登录相关接口有限流(按用户名与客户端 IP 双路计数):`login/start` 每用户名 10 次/分钟、`totp/login` 每账号 5 次/15 分钟(成功即清零)、注册每 IP 10 次/分钟,超额返回 429.限流是进程内的,重启清零、多实例各自计数,要跨实例统一得换集中存储.
- 客户端 IP 默认取 TCP 对端地址;只有确实只经可信 CDN/反代进来时才设 `TRUST_PROXY_HEADERS=1` 改读 `X-Forwarded-For`——这个头任何人都能伪造,信错了等于让攻击者自己挑额度.
- 绑定 TOTP 时新密钥先进 `pending_secret`,输入正确验证码确认后才替换旧密钥;中途放弃不会把原来能用的 TOTP 打死.
- `login/start` 与 `totp/login` 的失败响应统一成同一条消息:这两个接口匿名可调,"用户不存在""账号已禁用""没绑定通行密钥"如果是可区分的,就等于在向外枚举用户名和每个人的 2FA 配置.真实原因只进日志.
- 过期的 `auth_flows` 由 `serve` 内的后台任务每 60 秒清理一次;`login_start` 每次调用都会插一行,不清理就会被匿名请求撑爆.
- 注册改为「凭证通过才落库」:`/api/auth/register` 只读校验邀请码并把用户资料暂存在认证流程里,`/api/auth/register/finish` 校验 Passkey 成功后才在一个事务内扣邀请码、建用户、存凭证.浏览器取消仪式不再消耗邀请名额,也不会留下没有凭证的孤立用户;撞用户名由唯一约束兜底.
- 修改凭证需要 step-up:`totp/setup`、`passkey/setup`、`passkey/setup/finish` 要求会话在最近一次真实认证的 5 分钟内;过期后管理端会弹窗要求输入 TOTP 或使用通行密钥重新确认,验证成功后自动重试原操作.只拿到旧 cookie 或只读到 `/api/me` 的攻击者无法把受害者的二次验证换绑到自己设备上.`totp/confirm` 不要求 step-up(pending 密钥只能由已 step-up 的 setup 产生),但每账号限 5 次/15 分钟.
- 管理员启停账号有护栏:不能禁用自己的账号,也不能禁用最后一个可用管理员(同一事务内锁行判断,并发互关时后提交者失败).`kych-net user` CLI 不受此限制,保留为应急入口.
- 请求体上限显式声明:全局 2 MiB,头像 257 KB,不再依赖框架默认值.

## MCP 服务器

MCP 使用标准 **OAuth 2.1**：MCP 客户端通过动态客户端注册、授权码 + PKCE 登录 KychNet 账号，拿到短期 access token 和 refresh token。不再使用静态 Bearer token。

公网地址是 **`https://kych.net/mcp`**；`api.kych.net` 只是回源 Host，不能作为 MCP 或 Git 的客户端地址。

### 端点

- 资源元数据：`https://kych.net/.well-known/oauth-protected-resource`
- 授权服务器元数据：`https://kych.net/.well-known/oauth-authorization-server`
- 动态注册：`POST https://kych.net/oauth/register`
- 授权：`GET https://kych.net/oauth/authorize`
- Token：`POST https://kych.net/oauth/token`
- MCP：`https://kych.net/mcp`

### 登录

客户端第一次连 `/mcp` 会收到 `401` 和 `WWW-Authenticate: Bearer resource_metadata=...`，然后自动完成发现和注册，并打开 KychNet 授权页。授权页使用现有账号 + 动态验证码登录；登录后确认权限即可。

普通成员和管理员都可以读写 MCP；项目 Git 提交使用当前登录用户作为提交身份。授权页会明确列出读取内容、写入内容、读取项目、写入项目四项权限。

### 工具

- 只读：`list_pages`、`get_page`、`search_content`、`site_info`
- 内容写入：`save_content`、`delete_content`
- 项目读取：`list_projects`、`list_project_files`、`read_project_file`
- 项目写入：`write_project_file`、`delete_project_file`

### 客户端安装

MCP 客户端需要支持 **Streamable HTTP + OAuth**。不要给客户端配置静态 `Authorization` 头。

#### Claude Desktop

编辑：

- macOS：`~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows：`%APPDATA%\\Claude\\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "kych-net": {
      "url": "https://kych.net/mcp"
    }
  }
}
```

保存后完全退出并重启 Claude Desktop，首次连接会在浏览器打开授权页。

#### Claude Code

```bash
claude mcp add --transport http kych-net https://kych.net/mcp
```

#### Cursor

项目级 `.cursor/mcp.json`，或全局 `~/.cursor/mcp.json`：

```json
{
  "mcpServers": {
    "kych-net": {
      "url": "https://kych.net/mcp"
    }
  }
}
```

#### VS Code / GitHub Copilot MCP

`.vscode/mcp.json`：

```json
{
  "servers": {
    "kych-net": {
      "type": "http",
      "url": "https://kych.net/mcp"
    }
  }
}
```

#### Trae

Trae 目前不原生执行 MCP OAuth，直接填远程 `url` 会得到 401。用 `mcp-remote` 作为本地 stdio 桥接，由它完成浏览器 OAuth，再把工具转给 Trae。

先确认本机有 Node.js 22+ 和 `npx`。在 Trae 的 **设置 → MCP → 手动配置** 中加入：

```json
{
  "mcpServers": {
    "kych-net": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "https://kych.net/mcp"]
    }
  }
}
```

也可以直接编辑全局配置：

- macOS：`~/Library/Application Support/Trae/User/mcp.json`
- Windows：`%APPDATA%\Trae\User\mcp.json`
- Linux：`~/.config/Trae/User/mcp.json`

保存后首次连接会在浏览器打开 KychNet 授权页。`mcp-remote` 会在本地缓存 token；不要额外配置 `Authorization` 头。

#### DSH / DeepSeek Harness

DSH 直接用本仓库自带的插件 `dsh-mcp-kychnet`（见 `plugins/dsh-mcp-kychnet/`）接入。
插件本身只是配置 bundle，OAuth 2.1 的全部流程交给
[`dsh-mcp-oauth-client`](https://github.com/fangzaozao/dsh-mcp-oauth-client) 完成，
因此是原生的 Streamable HTTP 连接，不需要 `mcp-remote` 之类的 stdio 桥接。

```bash
dsh plugin --profile <profile> add /absolute/path/to/plugins/dsh-mcp-kychnet
```

重启 `dsh web` 后，**设置 → 插件** 里会出现「KychNet 站点」，**设置 → MCP** 里会出现
`mcp-kychnet`。首次调用 `mcp__kych-net__*` 工具时会打开授权页；工具与上面的「工具」
一节一一对应。

不使用插件、直接写 Cordis overlay 时，把下面的 `insert` 合并到
`$DSH_HOME/profiles/<name>/cordis.patch.yml`（只对该 profile 生效），或
`$DSH_HOME/cordis.patch.yml`（本机所有 profile 生效）：

```yaml
- insert:
    - id: mcp-kychnet
      name: dsh-mcp-oauth-client
      config:
        entryId: mcp-kychnet
        manager: false
        serverName: kych-net
        transport: streamable-http
        resourceUrl: https://kych.net/mcp
        authType: oauth
        scope: content:read content:write project:read project:write
```

> 老版本 DSH 如果既没有 bundle 支持也没有带 OAuth 的 MCP 客户端，才需要退回
> `npx -y mcp-remote https://kych.net/mcp` 的 stdio 桥接。桥接只解决 OAuth，不改变
> KychNet 的权限模型；登录用户仍然是 Git 提交身份。

### 自检

未带 token 请求 `/mcp`，应得到 401 和发现地址：

```bash
curl -i https://kych.net/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream"
```

也可以直接检查元数据：

```bash
curl -sS https://kych.net/.well-known/oauth-protected-resource
curl -sS https://kych.net/.well-known/oauth-authorization-server
```

### 故障排查

- `401`：还没有完成 OAuth，或 access token 过期；客户端应自动刷新；
- `403`：请求 `Host` 不是 `kych.net`/localhost，或反向代理未正确传 Host；
- `404`：路径不是 `/mcp`，或 nginx 未转发；
- 客户端只支持 stdio：当前服务是 Streamable HTTP，需要支持远程 HTTP MCP 的客户端。

### 撤销授权

撤销某个用户的全部 MCP 授权：

```sql
DELETE FROM oauth_tokens WHERE user_id = '<用户 UUID>';
```

撤销所有 MCP 授权：

```sql
DELETE FROM oauth_tokens;
```

access token 一小时内自然过期；refresh token 最长 30 天。数据库迁移 `0012_oauth` 创建客户端、授权码和 token 表。

## 项目与 Git

- 项目管理在管理端「项目」页,数据落在 PostgreSQL 的 Git 表和 `LFS_ROOT` 文件目录;
- 仓库地址:`https://kych.net/git/<项目 slug>.git`;
- Git smart HTTP 和 LFS 都通过 `kych.net` 的 nginx `/git/` 反代;
- 匿名可以 clone 公开仓库,写操作使用 Git token;
- 迁移 `0009_projects` / `0010_lfs` 创建项目、对象、refs、commits、PR、讨论与 LFS 索引;
- `kych-net gc --project <slug>` 清理不可达对象。

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

开发时 `VITE_API_BASE_URL` 留空,`/api` 由 vite 代理转发到后端。生产构建前必须把它设为 API 服务的完整 origin(例如 `https://kych.net`),因为管理前端是独立静态站,没有同源 `/api` 可转发;后端 CORS 与写接口 Origin 白名单均以 `ADMIN_ORIGIN` 为准。

## 构建与部署

1. [.github/workflows/build.yml](.github/workflows/build.yml) 在 push、PR 和手动触发时运行测试,并编译 Linux 二进制;推送 `v*` 标签时自动创建 Release.
   - `x86_64-unknown-linux-gnu`:依赖系统 glibc,在 Ubuntu 26.04 x86-64 runner 上构建并 strip 符号表.
2. 管理端保存内容后会自动编译当前页(改路径时同时删除旧产物);「站点设置 → 整站重建」或 `kych-net build --out dist` 重建全站.Typst 页面同时产出 `.pdf`.构建会先渲染到内存,全部成功后才清理旧的 `.html/.pdf` 并写入新路径(不再统一放 `posts/`),中途失败不会破坏现有站点.
3. 内容站 `dist/` 由 EdgeOne 加速;管理前端构建为独立静态产物,由 EdgeOne Pages 发布.
4. 服务器 nginx 只把 `/api/`、`/git/`、`/mcp` 等后端路径反代到 `127.0.0.1:8080`;`api.kych.net` 只作为回源 Host.
5. 项目 Git 对象存 PostgreSQL,LFS 文件存 `LFS_ROOT`;systemd 需要该目录的写权限.

## 约定

- 内容以数据库为唯一事实源,构建直接读库,不调用线上 API.
- 站点名称、图标与顶栏导航存于 `site_settings`,由管理端编辑,构建时读取.
- 内容路径允许嵌套目录,路径段只允许 Unicode 字母/数字、`-` `_`;目录主页必须显式写出文件名,例如 `/xef/index` 归一化为 `xef/index.html`;不再接受 `/xef/`.页面可写 `/reff/93df/l.html`,也可写 `.md/.typ/.wj` 由扩展名决定格式,但最终都输出 `.html`,同一路径只保留一条.`/api` 与 `/css` 保留;`index.html` 固定为网站主页,可编辑不可删除.路径总长上限 512 字节、单段上限 200 字节;链接中的非 ASCII 路径段按 `%XX` 编码,磁盘文件名保持原始 UTF-8.
- 自定义 CSS 使用 CSS 格式,路径必须写成 `/css/theme` 或 `/css/theme.css`,输出 `css/theme.css`;页面要在正文最前面显式声明才会加载:
  ```text
  ---
  css: /css/theme.css, /css/wikidot.css
  ---
  ```
  只有声明了 `css:` 的页面才会生成对应的 `<link rel="stylesheet">`;CSS 文件本身不会出现在内容站目录树里.
- 技术栈中的内容渲染包含 pulldown-cmark、Typst、ftml;管理端和技术栈表不再单独维护 CSS 项.
- 模板与 API 共用同一套领域模型,避免两端口径漂移.
- Typst 在当前进程内编译,由 `VirtualPath::resolve(root)` 加 canonicalize 双重校验把文件读取限制在 `CONTENT_ROOT` 内,避免内容作者通过 `#include` 读到构建机上的任意文件;父目录校验之外还会对目标文件本身做 `symlink_metadata` 判断,内容目录里的符号链接一律拒绝(否则 `link.png -> /etc/passwd` 会被 `fs::read` 跟进去).生产仍建议放进断网容器并加 CPU/内存限额,构建机上的 `.env` 与云凭据不要落在可读目录.
- Wikidot 由 [ftml](https://docs.rs/ftml) 渲染,`[[include]]` 在构建时从数据库预取 Wikidot 页面并限制 8 层递归;`ftml` 是 AGPL-3.0,因此本项目也按 AGPL-3.0 分发,见 `LICENSE`.
- 编译缓存尚未实现;目前每篇 Typst 内容会分别编译 `PagedDocument`(出 PDF)与 `HtmlDocument`(出 HTML)各一次.
- 静态产物保持平台中立:只用相对路径、不写死域名,重定向与 404 分别在各平台配置.
- 静态资源使用哈希文件名并长缓存,HTML 短缓存;压缩交给 CDN.
