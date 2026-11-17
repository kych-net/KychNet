# dsh-mcp-kychnet

KychNet 站点的 DeepSeek Harness 插件：安装后把 `https://kych.net/mcp` 登记为 MCP 服务，
由 [`dsh-mcp-oauth-client`](https://github.com/fangzaozao/dsh-mcp-oauth-client) 负责
OAuth 2.1 授权（401 发现 → 动态注册 → 授权码 + PKCE → 刷新令牌），首次调用时在浏览器里
用 KychNet 账号确认授权即可。

插件本身是纯配置 bundle：不含 Host/Client 代码，不保存凭据，也不发送静态
`Authorization` 头。

## 前置条件

- DSH 版本支持 bundle patch 与 Web 客户端扩展。
- 已安装 `dsh-mcp-oauth-client`（`>= 0.3.0`）。本插件通过 `peerDependencies` 声明该依赖，
  由 profile 提供同一份实例，避免同一个 MCP 客户端被装成两份。

## 安装

本地目录安装（开发时）：

```sh
dsh plugin --profile <profile> add /absolute/path/to/plugins/dsh-mcp-kychnet
```

或从插件市场 / npm 安装（发布后）：

```sh
dsh plugin --profile <profile> add dsh-mcp-kychnet
```

安装后在「设置 → 插件」里应看到 **KychNet 站点**，在「设置 → MCP」里应看到
`mcp-kychnet` 这一条已连接的服务。

## 首次授权

第一次调用任一 `mcp__kych-net__*` 工具时，客户端会收到 401 并按资源元数据自动完成发现与
动态注册，然后打开 KychNet 授权页。用现有账号登录、确认六项权限即可：

| 权限 | 覆盖的工具 |
| --- | --- |
| `content:read` | `列出站点页面`、`读取站点页面`、`搜索站点内容`、`站点信息` |
| `content:write` | `保存站点内容`、`删除站点内容` |
| `project:read` | `列出项目`、`列出项目文件`、`读取项目文件` |
| `project:write` | `写入项目文件`、`删除项目文件` |
| `pr:read` | `列出拉取请求`、`查看拉取请求` |
| `pr:write` | `创建拉取请求`、`评论拉取请求`、`合并拉取请求`、`关闭拉取请求` |

access token 一小时过期，refresh token 最长 30 天；令牌只保存在插件进程内存中，不写入
profile。站点侧撤销全部授权见仓库 README 的「撤销授权」一节。

## 工具

安装后工具以 `mcp__kych-net__<工具名>` 出现，工具名自 0.4.0 起为中文，与 README「MCP 服务器 → 工具」一节列出的
`列出站点页面`、`读取站点页面`、`搜索站点内容`、`站点信息`、`保存站点内容`、`删除站点内容`、
`列出项目`、`列出项目文件`、`读取项目文件`、`写入项目文件`、`删除项目文件`、
`列出拉取请求`、`查看拉取请求`、`创建拉取请求`、`评论拉取请求`、`合并拉取请求`、`关闭拉取请求` 一一对应。

旧客户端需要在「设置 → MCP」里重新连接，以刷新中文工具列表与新增的 PR 权限。

## 配置

需要换资源地址（例如指向自建实例）时，覆盖 `cordis.patch.yml` 里那一行的
`config.resourceUrl`；`scope` 也可按需收窄，例如只读场景：

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
        scope: content:read project:read
```

注意 `resourceUrl` 必须是站点 README 里给出的公网地址 `https://kych.net/mcp`；
`api.kych.net` 只是回源 Host，不是客户端地址。

## 故障排查

- `401`：尚未完成授权或 access token 过期，客户端会自行刷新；浏览器没弹出授权页时，
  在「设置 → MCP」里对该条目执行重新连接。
- `403`：请求的 `Host` 不是 `kych.net` / localhost，常见于反向代理没透传 Host。
- 工具名重复：profile 里如果还留着旧的手写 `kych-net` MCP 条目，会与本插件重复连接同一
  服务。删掉旧条目，只保留 `mcp-kychnet` 这一条。

## 开发与校验

```sh
# 校验 patch 与语言资源
node -e "require('js-yaml').load(require('fs').readFileSync('cordis.patch.yml','utf8'))"
node -e "JSON.parse(require('fs').readFileSync('locale/zh.json','utf8'))"
```

安装后重新组合 profile 检查：

```sh
dsh --profile <profile> --dump-config
```

## 许可证

与 KychNet 仓库一致：AGPL-3.0-or-later。
