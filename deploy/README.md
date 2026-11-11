# 前端部署（eo.kych.net）

内容站与管理端合并在同一域名下，由 EdgeOne 加速、源站 nginx 直接托管：

```
eo.kych.net/        → /opt/kych-net/dist   内容站（后端 build 产出）
eo.kych.net/管理/   → /opt/kych-admin      管理端 SPA（GitHub Actions 发布）
接口               → https://kych.net      主域 API，不进本机代理
```

旧入口 `c.kych.net`、`m.kych.net` 一律 301 到新地址。

## 为什么 eo.kych.net 不需要单独签证书

EdgeOne 的**回源 HOST**与**回源 SNI**是两个独立配置：

| 配置 | 值 | 作用 |
| --- | --- | --- |
| 回源地址 | `49.232.71.240:443` | 连哪台机器 |
| 回源 SNI | `kych.net` | nginx 按它选证书，复用主域那张 |
| 回源 HOST | `eo.kych.net` | nginx 按它选 vhost |

nginx 先按 SNI 选证书、再按 Host 选站点，两者互不干扰，所以主域证书
（SAN: kych.net/api/origin/www）可直接复用。填错 Host 会命中别的 vhost：
填 `kych.net` 会被主域 API vhost 301 走，填 IP 会落到 `default_server` 返回内容站。

自检命令：

```bash
printf 'GET / HTTP/1.1\r\nHost: eo.kych.net\r\nConnection: close\r\n\r\n' \
  | openssl s_client -quiet -connect 49.232.71.240:443 -servername kych.net
```

## 管理端的子路径

管理端挂在 `/管理/`（中文路径）。两点必须注意：

1. `admin/vite.config.ts` 的 `base` 必须是 `/管理/`，否则资源会去请求内容站的 `/assets/`
2. nginx 的 `location ^~ /管理/` 必须带 `^~`，否则 `/管理/assets/*.js` 会被内容站的
   `\.js$` 正则 location 抢走，管理端所有 chunk 404

浏览器把 `/管理/` 编码成 `/%E7%AE%A1%E7%90%86/` 发出，nginx 解码后再匹配 location，
所以配置里直接写中文即可。

## 服务器侧一次性准备

```bash
# 部署专用账号：无 sudo，只对 /opt/kych-admin 有写权限
useradd -m -s /bin/bash deploy-admin
install -d -m 700 -o deploy-admin -g deploy-admin /home/deploy-admin/.ssh
install -d -m 755 -o deploy-admin -g deploy-admin /opt/kych-admin
# 把 CI 公钥写入 authorized_keys（chmod 600）

# nginx（本目录 kychnet-eo.conf）
install -m 644 deploy/nginx/kychnet-eo.conf /etc/nginx/conf.d/kychnet-eo.conf
nginx -t && systemctl reload nginx
```

## 域名切换

`deploy/switch-to-eo.sh` 做三件事：备份 `.env` → 改写 `SITE_URL`/`ADMIN_ORIGIN`/
`WEBAUTHN_ORIGIN` → 重启后端并重建内容站。

执行前必须确认：nginx 已加载 `kychnet-eo.conf`、EO 上 eo.kych.net 已生效、
DNSPod 已指向 EO。三项缺一，管理端登录会立刻失败（`ADMIN_ORIGIN` 只能指向一个域）。

重建内容站必须在 `/opt/kych-net` 目录下执行，配置靠 `dotenvy` 读当前目录的 `.env`。

## GitHub Secrets

| Secret | 值 |
| --- | --- |
| `DEPLOY_HOST` | `49.232.71.240` |
| `DEPLOY_USER` | `deploy-admin` |
| `DEPLOY_PATH` | `/opt/kych-admin/` |
| `DEPLOY_SSH_KEY` | 部署私钥全文 |

## 手工发版

```bash
cd admin && VITE_API_BASE_URL=https://kych.net pnpm build
rsync -az --delete admin/dist/ deploy-admin@49.232.71.240:/opt/kych-admin/ \
  -e "ssh -i ~/.ssh/kych-deploy"
```

## 回滚

| 场景 | 做法 |
| --- | --- |
| 前端发版有问题 | `git revert` 后重新发布 |
| 域名要退回 | `cp -a /opt/kych-net/.env.bak-<时间戳> /opt/kych-net/.env`，重启后端并重建内容站 |
| nginx 配置要退回 | 配置备份在服务器 `/root/nginx-backup-<时间戳>/` |
| EO 出问题 | 控制台暂停加速域名，或 DNS 直指 `49.232.71.240` |

## 已验证响应

| 请求 | 结果 |
| --- | --- |
| `GET /` | `200`，`Cache-Control: no-cache` |
| `GET /管理/` | `200`，`no-cache` |
| `GET /管理/assets/<hash>.js` | `200`，`public, max-age=2592000, immutable` |
| `GET /管理` | `301` → `/管理/` |
| `GET /管理/任意深链接` | `200`，回退到管理端 index.html |
| `c.kych.net` / `m.kych.net` | `301` → eo 对应路径 |
| `http://` 任意已知域名 | `301` → `https://` |
| `http://` 未知 Host | `444` 直接断开 |

## 只走 HTTPS

源站 80 端口只放行 `/.well-known/acme-challenge/`，其余一律 301 到 HTTPS，
未知 Host 返回 444。**不能直接不监听 80**：证书靠 webroot 续期，
`certbot.timer` 每天要访问该路径，关掉端口会让续期失效。

浏览器侧访问 `http://eo.kych.net/` 由 EdgeOne 处理，源站管不到——
需要在 EO 控制台开启 **HTTPS 配置 → 强制 HTTPS 跳转** 才会 301。
