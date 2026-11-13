# 前端部署（kych.net）

内容站、管理端与后端接口合并在**主域 `kych.net`** 下，由 EdgeOne 加速、源站 nginx 直接托管：

```
https://kych.net/        → /opt/kych-net/dist   内容站（后端 build 产出）
https://kych.net/管理/   → /opt/kych-admin      管理端 SPA（GitHub Actions 发布）
https://kych.net/编辑器/ → /opt/kych-editor     独立编辑器 SPA（GitHub Actions 发布）
https://kych.net/api/…   → 127.0.0.1:8080      后端接口（同源，不再是独立子域）
```

EdgeOne 只做加速回源，静态文件全部由源站 nginx 托管。旧入口
`eo.kych.net`、`c.kych.net`、`m.kych.net` 已停用：2026-11-13 实测公网 DNS
均为 NXDOMAIN，记录已删除。

## DNS 与子域现状

2026-11-13 通过 DoH 实测：

| 名称 | 记录 | 值 | 说明 |
| --- | --- | --- | --- |
| `kych.net` | CNAME | `kych.net.eo.dnse0.com` | 主域接入 EdgeOne，递归返回 EO 节点 A 记录 |
| `www.kych.net` | A | `49.232.71.240` | 直连源站，nginx 301 → `https://kych.net/` |
| `eo` / `c` / `m` / `api.kych.net` | — | 无记录（NXDOMAIN） | 历史入口，已删除 |

权威 DNS 仍在 DNSPod（`phecda.dnspod.net` / `hospital.dnspod.net`），
所以域名记录要在 DNSPod 改，EO 侧只管加速与回源配置。

## 回源与证书

主域直接接入 EO 后，不再需要为子域单独签证书，也不再需要拆 HOST/SNI：

| 配置 | 值 | 作用 |
| --- | --- | --- |
| 边缘证书 | EO 托管（免费证书自动申请/续期） | 浏览器 → EO 的 HTTPS |
| 回源地址 | `49.232.71.240:443` | EO → 源站 nginx |
| 回源 HOST / SNI | `kych.net` | nginx 按它选证书与 vhost，复用主域那张 Let's Encrypt 证书 |

实测源站行为（SNI 固定 `kych.net`）：

| 回源 Host | 结果 |
| --- | --- |
| `kych.net` | `200`，内容站首页，`Cache-Control: no-cache` |
| `eo.kych.net` | `301` → `https://kych.net/`（旧 vhost 仅作兼容跳转） |

也就是说源站现在由 `kych.net` vhost 同时托管内容站、管理端并反代后端路径，
回源 HOST 必须填 `kych.net`。请仍与 EO 控制台的回源配置核对一次——
旧方案曾用「SNI=`kych.net` + HOST=`eo.kych.net`」复用证书，那个组合现在会
命中 301，导致回源重定向循环。

自检命令（绕过 EO 直连源站）：

```bash
printf 'GET / HTTP/1.1\r\nHost: kych.net\r\nConnection: close\r\n\r\n' \
  | openssl s_client -quiet -connect 49.232.71.240:443 -servername kych.net
```

## 管理端的子路径

管理端挂在 `/管理/`（中文路径）。两点必须注意：

1. `admin/vite.config.ts` 的 `base` 必须是 `/管理/`，否则资源会去请求内容站的 `/assets/`
2. nginx 的 `location ^~ /管理/` 必须带 `^~`，否则 `/管理/assets/*.js` 会被内容站的
   `\.js$` 正则 location 抢走，管理端所有 chunk 404

浏览器把 `/管理/` 编码成 `/%E7%AE%A1%E7%90%86/` 发出，nginx 解码后再匹配 location，
所以配置里直接写中文即可。

## 独立编辑器的子路径

移动优先的独立编辑器挂在 `/编辑器/`，与管理端同构（`editor/vite.config.ts` 的
`base` 默认 `/编辑器/`，nginx `location ^~ /编辑器/` alias 到 `/opt/kych-editor/`，
深链接回退 `/编辑器/index.html`，assets 长缓存、入口 no-cache）。三点注意：

1. 编辑器通过 `@admin` 别名复用 `admin/src` 的 `api.ts` / `CodeEditor.tsx` /
   `editorCompletions.ts`，**不是**独立复制一份。这些共享文件改动会同时触发
   管理端与编辑器两条流水线。
2. 编辑器构建（`tsc --noEmit`）会编译上述 admin 源文件，它们的 bare import 按
   node 解析落到 `admin/node_modules`。因此 [deploy-editor.yml](../.github/workflows/deploy-editor.yml)
   在构建前**先装 admin 依赖、再装 editor 依赖**，只装 editor 会报「Cannot find module」。
3. 编辑器与管理端共用同一部署账号与 `DEPLOY_HOST` / `DEPLOY_USER` / `DEPLOY_SSH_KEY`，
   目标目录固定写死 `/opt/kych-editor/`（非敏感，无需新增 secret）。

## 服务器侧一次性准备

```bash
# 部署专用账号：无 sudo，只对 /opt/kych-admin 有写权限
useradd -m -s /bin/bash deploy-admin
install -d -m 700 -o deploy-admin -g deploy-admin /home/deploy-admin/.ssh
install -d -m 755 -o deploy-admin -g deploy-admin /opt/kych-admin
# 编辑器与管理端共用该账号，另建编辑器产物目录并授予写权限
install -d -m 755 -o deploy-admin -g deploy-admin /opt/kych-editor
# 把 CI 公钥写入 authorized_keys（chmod 600）

# nginx（本目录 kychnet-eo.conf）
install -m 644 deploy/nginx/kychnet-eo.conf /etc/nginx/conf.d/kychnet-eo.conf
nginx -t && systemctl reload nginx
```

> ⚠️ 仓库内的 `deploy/nginx/kychnet-eo.conf` 是旧入口时期的参考配置，
> `server_name` 仍写 `eo.kych.net`（80 端口的 301 目标也是它）。部署到当前主域前
> 必须改成 `kych.net`，否则 vhost 不命中。本次文档更新**未改动该配置文件**，
> 请以服务器上的实际配置为准。

## 历史：域名切换脚本

`deploy/switch-to-eo.sh` 是当初从 `c.kych.net` / `m.kych.net` 切到
`eo.kych.net` 的一次性脚本，会把 `.env` 的 `SITE_URL` / `ADMIN_ORIGIN` /
`WEBAUTHN_ORIGIN` 写成 `https://eo.kych.net`，然后重启后端、重建内容站。

**该域名已停用，不要再执行此脚本**：`ADMIN_ORIGIN` 是 CORS 与写接口
Origin 白名单的等值比较基准，写成已停用的域名会让管理端登录与所有写请求
立刻失败。当前正确值是 `https://kych.net`。脚本仅作历史记录保留。

重建内容站必须在 `/opt/kych-net` 目录下执行，配置靠 `dotenvy` 读当前目录的 `.env`。

## 环境变量（后端）

| 变量 | 值 | 说明 |
| --- | --- | --- |
| `SITE_URL` | `https://kych.net` | 内容站的对外地址，决定 canonical 与「前往内容端」链接 |
| `ADMIN_ORIGIN` | `https://kych.net` | 管理端的 **origin**。CORS 与写接口的 Origin 白名单拿它做等值比较，**必须纯 scheme://host，不能带路径** |
| `ADMIN_URL` | `https://kych.net/管理/`（可省略） | 页面里「管理端」按钮的地址，允许带子路径；缺省由 `ADMIN_ORIGIN` 拼出 `/管理/` |
| `WEBAUTHN_ORIGIN` | `https://kych.net` | WebAuthn 校验的 origin |
| `WEBAUTHN_RP_ID` | `kych.net` | 通行密钥的绑定域，不要写成子域 |

`ADMIN_ORIGIN` 与 `ADMIN_URL` 分开是必要的：前者参与等值比较，一旦带上 `/管理/`
就再也匹配不上浏览器发来的 `Origin: https://kych.net`，会导致所有写请求被拒。

> `.env.example` 已同步为 `kych.net`，但 `src/config.rs` 中 `SITE_URL` /
> `ADMIN_ORIGIN` / `WEBAUTHN_ORIGIN` 的内置默认值仍是 `https://eo.kych.net`。
> 部署时必须在 `.env` 显式覆盖，不要依赖默认值。

## 后端二进制发布

前端产物走上面的 workflow，后端 `kych-net` 二进制则由
[build.yml](../.github/workflows/build.yml) 在 push 到 `main` 时编译并上传 artifact。

```bash
# 1. 找到 main 上最新一次 build
gh run list --repo kych-net/KychNet --branch main --workflow build --limit 3

# 2. 下载产物（international 带宽差时可用 gh api 断点续传）
gh run download <run-id> --name kych-net-x86_64-unknown-linux-gnu -D /tmp/kych-build
cd /tmp/kych-build && tar -xzf kych-net-x86_64-unknown-linux-gnu.tar.gz

# 3. 备份旧二进制后替换
scp kych-net root@49.232.71.240:/tmp/kych-net.new
ssh root@49.232.71.240 '
  set -e
  systemctl stop kych-net
  cp -a /opt/kych-net/kych-net /opt/kych-net/kych-net.pre-$(date +%Y%m%d%H%M%S)
  install -m 755 -o root -g kych-net /tmp/kych-net.new /opt/kych-net/kych-net
  systemctl start kych-net
  systemctl is-active kych-net
'

# 4. 模板或 SITE_URL 有变动时重建内容站
ssh root@49.232.71.240 "sudo -u kych-net bash -c 'cd /opt/kych-net && ./kych-net build'"
```

替换二进制不会动 `.env`；若本次改了配置项，需同步修改 `/opt/kych-net/.env` 并重启。

## GitHub Secrets

| Secret | 值 |
| --- | --- |
| `DEPLOY_HOST` | `49.232.71.240` |
| `DEPLOY_USER` | `deploy-admin` |
| `DEPLOY_PATH` | `/opt/kych-admin/` |
| `DEPLOY_SSH_KEY` | 部署私钥全文 |

编辑器流水线复用 `DEPLOY_HOST` / `DEPLOY_USER` / `DEPLOY_SSH_KEY`，目标目录
`/opt/kych-editor/` 直接写死在 workflow 里（非敏感），**不需要新增 secret**；
`DEPLOY_PATH` 仍只用于管理端。

## 手工发版

管理端：

```bash
cd admin && VITE_API_BASE_URL=https://kych.net pnpm build
rsync -az --delete admin/dist/ deploy-admin@49.232.71.240:/opt/kych-admin/ \
  -e "ssh -i ~/.ssh/kych-deploy"
```

编辑器（复用 admin 源码，本地两个工程的依赖都需已安装）：

```bash
cd editor && VITE_API_BASE_URL=https://kych.net pnpm build
rsync -az --delete editor/dist/ deploy-admin@49.232.71.240:/opt/kych-editor/ \
  -e "ssh -i ~/.ssh/kych-deploy"
```

发版后如果浏览器仍看到旧页面，检查 EO 是否开了「强制缓存」：`/管理/`、`/编辑器/`
与各自的 `index.html` 必须保持 `no-cache`，只有带 hash 的 `assets/*` 才能长缓存。
必要时刷新 `https://kych.net/管理/` 或 `https://kych.net/编辑器/` 的缓存。

## 回滚

| 场景 | 做法 |
| --- | --- |
| 前端发版有问题 | `git revert` 后重新发布 |
| 域名配置要退回 | `cp -a /opt/kych-net/.env.bak-<时间戳> /opt/kych-net/.env`，重启后端并重建内容站 |
| nginx 配置要退回 | 配置备份在服务器 `/root/nginx-backup-<时间戳>/` |
| EO 出问题 | 控制台暂停加速域名，或 DNS 直指 `49.232.71.240` |

## 已验证响应

2026-11-13 实测（经 EO 与直连源站）：

| 请求 | 结果 |
| --- | --- |
| `GET https://kych.net/` | `200`，`Cache-Control: no-cache`，内容站首页 |
| `GET https://kych.net/管理/` | `200`，`no-cache`，管理端 SPA |
| `GET https://kych.net/管理/assets/<hash>.js` | `200`，`public, max-age=2592000, immutable` |
| `GET https://kych.net/项目/地狱之下/` | `200`，内容页 |
| `GET http://www.kych.net/` | `301` → `https://kych.net/` |
| `eo` / `c` / `m` / `api.kych.net` | DNS NXDOMAIN |

以下为 nginx 配置约定，部署后应自行复核：`GET /管理` → `301 /管理/`、
`/管理/` 下任意深链接回退到管理端 `index.html`、未知 Host 的 80 请求返回 `444`。

## 只走 HTTPS

源站 80 端口只放行 `/.well-known/acme-challenge/`，其余一律 301 到 HTTPS，
未知 Host 返回 444。**不能直接不监听 80**：证书靠 webroot 续期，
`certbot.timer` 每天要访问该路径，关掉端口会让续期失效。

浏览器侧访问 `http://kych.net/` 由 EdgeOne 处理，源站管不到——
需要在 EO 控制台开启 **HTTPS 配置 → 强制 HTTPS 跳转** 才会 301。
