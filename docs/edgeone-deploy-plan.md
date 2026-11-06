# KychNet 接入 EdgeOne 部署计划 · 换回源 HOST 方案

> 本文件是**部署计划**，不修改任何现有脚本。确认后按第 5 节落地改动，再执行第 7 节。
> 本文以 `docs/` 文档形式提交到 `kych-net/KychNet` 仓库，**仅新增文档，不含脚本改动**。
> 文中 `deploy.sh` / `rebuild.sh` 指本地部署交付包里的脚本（未纳入本仓库），第 5 节行号对应该包当前版本。
> 前置基础：三子域（内容站 / 管理端 / API）部署方案与代码评审结论，见同批交付文档。

## 0. 决策记录

| 项 | 结论 |
| --- | --- |
| 内容站 | `c.kych.net`，EO **站点加速 / CDN，有源站**，回源 Lighthouse 的 `dist/` |
| 管理端 | `m.kych.net`，**EdgeOne Pages 直传，无源站**，`admin/dist` 直接上传边缘 |
| 后端 | `uh.kych.net` 保持现状，不接 EO、不改动、不讨论 |
| 裸域 | `kych.net` 保持 A 指向本机，nginx 443 做 **301 → `https://c.kych.net`** |
| 回源 | 用**独立回源 HOST** `origin.kych.net`（A 指向本机），而非换端口或用 `@` |
| 接入方式 | **CNAME 接入，不改 NS**，整个 zone 的解析托管权留在原服务商 |
| NS / 端口 | 不改 NS、不开新端口、安全组仍只放行 80/443 |

「换 HOST」解决的是两个撞车：`@` 若同时做回源 HOST 和 301，nginx 无法区分回源请求与真人访问；改回源 HOST 后，同一个 443 靠 `server_name` 即可分开，且 `origin.kych.net` 指向本机使 certbot 能正常签发/续期。

---

## 1. 最终架构

```text
 用户 ── https://c.kych.net ──> EO 站点加速边缘（证书: c.kych.net）
                                    │ 未命中缓存，HTTPS 回源
                                    │ Host / SNI = origin.kych.net
                                    ▼
                          49.232.71.240:443  nginx
           ┌────────────────────────┼─────────────────────────┐
           │ server_name kych.net   │ server_name origin.kych.net│ server_name uh.kych.net
           │ → 301 https://c...     │  / c.kych.net              │ → 127.0.0.1:8080
           │                        │ → root /opt/kychnet/dist   │   （Axum API）
           └────────────────────────┴─────────────────────────┘

 用户 ── https://m.kych.net ──> EdgeOne Pages（静态托管，无源站，无回源）
                                    │ 浏览器 JS 跨源直连
                                    ▼ https://uh.kych.net/api/...  （直连 Lighthouse）
```

| 域名 | DNS | 谁提供内容 | 有无源站 |
| --- | --- | --- | --- |
| `c.kych.net` | CNAME → EO 加速域名 | EO 边缘缓存 + 回源 nginx `dist/` | **有** |
| `m.kych.net` | CNAME → Pages 域名 | EdgeOne Pages 静态托管 | **无** |
| `origin.kych.net` | A `49.232.71.240` | 仅 EO 回源用，用户不访问 | 回源别名 |
| `kych.net` | A `49.232.71.240` | nginx 301 跳转 | 否 |
| `uh.kych.net` | A `49.232.71.240` | nginx → Axum `:8080` | 保持现状 |

---

## 2. 前置条件

| # | 检查项 | 说明 |
| --- | --- | --- |
| 1 | DNS 现状 | 编写本计划时的快照：`kych.net` / `www` / `api` → `49.232.71.240`；`c` / `m` 无 A 记录；`uh` 有记录但指向别处（执行前请重新 `dig` 确认） |
| 2 | **确认 `uh.kych.net` 已解析到 `49.232.71.240`** | 否则 `deploy.sh` 里 certbot 的 `-d uh` 会 HTTP-01 失败；若它仍指向别处，把 `-d uh` 从 SAN 去掉（详见 5.3 注） |
| 3 | 安全组 | 仅放行 **80 / 443** 入站；ACME HTTP-01 与 EO 回源都走这两个端口 |
| 4 | 备案 | `kych.net` 大陆加速区需已备案（现网解析到国内服务器，应满足） |
| 5 | 源码 | 直接用 GitHub `main`：已合并 PR #5（第二轮修复）、PR #6（工作区更新）、PR #7（认证加固：CORS 补 `PUT`、登录限流、会话轮换、TOTP 确认后替换）。若沿用早前的离线部署包，需确认它**包含 PR #7 的 CORS `Method::PUT` 修复**，否则管理端 `saveSettings` 跨源预检会失败 |
| 6 | EO 凭据 | ① **Pages API Token**（Pages 控制台「设置 → API Token」，用于 `edgeone pages deploy`）；② **腾讯云 API 密钥 SecretId/SecretKey**（CAM，用于 `tccli teo CreatePurgeTask` 刷缓存）。**两者不是同一个东西，别混用** |
| 7 | CLI | 服务器已装 Node；另需 `npm i -g edgeone@latest`（≥1.2.30）与 `pip3 install tccli` |
| 8 | EO 套餐 | 站点加速为套餐制；确认可用区与额度，Pages 有免费额度 |

---

## 3. DNS 最终记录表（在原服务商改，NS 不动）

| 主机记录 | 类型 | 值 | 动作 |
| --- | --- | --- | --- |
| `@` | A | `49.232.71.240` | 保持 |
| `c` | **CNAME** | `<EO 站点加速分配的 CNAME>` | **新增** |
| `m` | **CNAME** | `<Pages 分配的 CNAME>` | **新增** |
| `origin` | A | `49.232.71.240` | **新增**（回源 HOST + 证书续期） |
| `uh` | A | `49.232.71.240` | 按前置条件 2 确认 |
| `www` / `api` / MX / TXT / SPF | 原样 | — | **不迁移、不改动** |

- `c` 是普通子域，CNAME 无 RFC 冲突；裸域 `@` 保留 A，因此**不需要 NS 接入 / CNAME 展平 / ALIAS**。
- Pages 绑定 `m.kych.net` 若要求归属权验证，按提示加的 **TXT 记录也加在原服务商**，不要迁 NS。
- 生效时间：CNAME 一般 5–30 分钟。

---

## 4. EdgeOne 控制台配置

### 4.1 内容站 —— 站点加速（有源站）

1. 添加站点 `kych.net`，接入方式选 **CNAME 接入**（域名托管在 DNSPod 时可选「DNSPod 托管接入」免验证），完成归属权验证（TXT 或文件）。
2. 「域名管理 → 添加域名」填 `c.kych.net`，拿到分配的 CNAME。
3. 源站配置：

| 配置项 | 值 |
| --- | --- |
| 源站地址 | `49.232.71.240`（填 IP，不要填 `c.kych.net`，避免循环解析） |
| 回源端口 | `443` |
| 回源协议 | **HTTPS** |
| **回源 HOST 头** | **自定义 → `origin.kych.net`**（SNI 随之） |
| 边缘证书 | 部署 `c.kych.net` 证书（EO 免费证书或上传现有证书） |

4. 缓存规则（文件名固定，不要指望 TTL 自动更新）：

| 路径 | 建议 |
| --- | --- |
| `/index.html` | 短 TTL 或 `no-cache` |
| `/posts/*.html` | 短–中 TTL |
| `/posts/*.pdf` | 长 TTL |
| `/.well-known/acme-challenge/` | **不缓存**（本方案不走 EO 验证，仅作兜底） |

5. 记录 **ZoneId**（`zone-xxxxxxxx`），供第 5.5 节刷新缓存使用。

### 4.2 管理端 —— Pages 直传（无源站）

1. 创建项目时选 **「直接上传」**（不要选 Git 导入，产物是 `vite build` 出来的，仓库里没有）。
2. 绑定自定义域名 `m.kych.net`，按提示加 CNAME（及归属权 TXT）。HTTPS 由 Pages 自动签发。
3. **不需要 `edgeone.json` 的 SPA fallback**：已确认 `admin/src/App.tsx` 与 `Shell.tsx` 用 `useState` 切换视图、无客户端路由，全站只有 `/`，刷新不会 404。若将来引入 `BrowserRouter` 再补 rewrite。
4. 直接上传限制：单文件 ≤25MB、≤20000 文件、总容量约 5GB；本项目远低于上限。**注意：选「直接上传」后不能切回 Git 集成**，要 Git 集成需另建项目。
5. 生成 **Pages API Token**，存到服务器 `/opt/kychnet/.edgeone/token`（`chmod 600`）。

---

## 5. 服务器端改动清单（对应 `deploy.sh` 行号）

> 推荐路径：**先把以下改动落进 `deploy.sh`，重新打包后一次性部署**（第 7 节 Phase 2），避免部署完再手工打补丁。

### 5.1 变量（`deploy.sh` 第 38 行附近）

| 变量 | 值 | 变化 |
| --- | --- | --- |
| `SITE_DOMAIN` | `c.kych.net` | **默认值，不动** |
| `ADMIN_DOMAIN` | `m.kych.net` | 不动 |
| `API_DOMAIN` | `uh.kych.net` | 不动 |
| `ORIGIN_DOMAIN` | `origin.kych.net` | **新增** |
| `SITE_URL` / `SITE_ORIGIN` | `https://c.kych.net` | 不动（`SITE_URL` 同时决定管理端「查看站点」链接，取自 `getSettings()`） |
| `ADMIN_ORIGIN` | `https://m.kych.net` | 不动（首页「管理端」链接、CORS、`guard_origin`） |
| `VITE_API_BASE_URL` | `https://uh.kych.net` | 不动 |
| `WEBAUTHN_ORIGIN` / `WEBAUTHN_RP_ID` | `https://m.kych.net` / `m.kych.net` | 不动 |
| `BIND` | `127.0.0.1:8080` | 不动 |

### 5.2 DNS 预检（第 112–131 行）

现状要求 `c`/`m`/`uh` 三个域名都解析到 `49.232.71.240`，而 `c`/`m` 改 CNAME 后会解析到 EO/Pages 边缘 IP，预检必然失败。改为**按角色校验**：

```python
import os, socket
E = os.environ
expect = E["DNS_EXPECT_IP"]
# 必须指向本机：源站回源与 certbot HTTP-01 依赖它们
must_local = [E["ORIGIN_DOMAIN"], "kych.net", E["API_DOMAIN"]]
# 只需能解析：已交给 EO / Pages
must_resolve = [E["SITE_DOMAIN"], E["ADMIN_DOMAIN"]]

def addrs(name):
    try:
        return sorted({i[4][0] for i in socket.getaddrinfo(name, None)})
    except Exception as e:
        raise SystemExit("[失败] %s 无法解析（%s）" % (name, e))

for n in must_resolve:
    print("  %s -> %s（交由 EO/Pages，仅校验可解析）" % (n, ", ".join(addrs(n))))
for n in must_local:
    a = addrs(n)
    print("  %s -> %s" % (n, ", ".join(a)))
    if expect.lower() != "skip" and expect not in a:
        raise SystemExit("[失败] %s 未解析到 %s（当前 %s）" % (n, expect, ", ".join(a)))
```

不落脚本时的应急开关：`DNS_EXPECT_IP=skip`。

### 5.3 certbot（第 447–457 行）

`c` / `m` 的 HTTP-01 挑战分别落到 EO 边缘和 Pages（后者没有 challenge 文件），**留着任一 `-d` 都会让整个签发失败、`deploy.sh` 直接 `die`**：

```bash
certbot certonly --webroot -w "$CERTBOT_WEBROOT" \
  -d "kych.net" -d "$ORIGIN_DOMAIN" -d "$API_DOMAIN" \
  --non-interactive --agree-tos -m "admin@kych.net" --keep-until-expiring
```

**同时必须改第 447–448 行的证书路径变量**：certbot 以第一个 `-d` 命名证书目录，现在会落在 `live/kych.net/`，而原脚本写的是 `live/$SITE_DOMAIN/`（即 `live/c.kych.net/`），不改会让 `nginx -t` 因找不到证书失败：

```bash
CERT="/etc/letsencrypt/live/kych.net/fullchain.pem"
KEY="/etc/letsencrypt/live/kych.net/privkey.pem"
```

> 注 1：`SITE_DOMAIN` 仍是 `c.kych.net`（决定内容站地址），但**证书目录名变成 `kych.net`**，两者不再相同，这是本方案最容易漏改的一处。
> 注 2：`-d "$API_DOMAIN"` 保留的前提是前置条件 2 已确认 `uh` 解析到本机；若 `uh` 仍指向别处，**去掉这一项**，否则同样会失败。`origin` 与 `@` 都 A 指向本机，HTTP-01 走 80 可正常续期，**不再依赖 EO 放行 ACME 路径**。

### 5.4 nginx（第 459–508 行）

最终 `/etc/nginx/conf.d/kychnet.conf` 应为（`$CERT` / `$KEY` 沿用第 5.3 节签出的证书）：

```nginx
# 80：ACME 校验 + 直接跳到内容站（避免 80→443→301 的二次跳转）
server {
    listen 80;
    listen [::]:80;
    server_name kych.net origin.kych.net c.kych.net uh.kych.net;
    location /.well-known/acme-challenge/ { root /var/www/certbot; }
    location / { return 301 https://c.kych.net$request_uri; }
}

# 443：裸域跳转
server {
    listen 443 ssl;
    listen [::]:443 ssl;
    server_name kych.net;
    ssl_certificate     /etc/letsencrypt/live/kych.net/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/kych.net/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    return 301 https://c.kych.net$request_uri;
}

# 443：内容站（EO 回源命中这里）
server {
    listen 443 ssl;
    listen [::]:443 ssl;
    server_name origin.kych.net c.kych.net;
    ssl_certificate     /etc/letsencrypt/live/kych.net/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/kych.net/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    client_max_body_size 10m;
    location / {
        root /opt/kychnet/dist;
        try_files $uri $uri/ =404;
    }
}

# 443：API（不动，保留原始 Origin）
server {
    listen 443 ssl;
    listen [::]:443 ssl;
    server_name uh.kych.net;
    ssl_certificate     /etc/letsencrypt/live/kych.net/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/kych.net/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    client_max_body_size 10m;
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        # 不覆盖 Origin：后端 guard_origin 依赖浏览器原始 Origin
    }
}
```

要点：**删掉 `m` 的 server 块**（管理端已由 Pages 提供）；`origin.kych.net` 与 `kych.net` 共用同一张证书，靠 `server_name` 区分；`nginx -t` 后 reload。

### 5.5 `rebuild.sh`（第 378–387 行）

末尾加 EO 缓存刷新。刷新用 **腾讯云 API 密钥**（不是 Pages Token）：

```bash
#!/usr/bin/env bash
# 重新生成静态站并刷新 EO 缓存（管理端保存/发布内容后执行）
set -Eeuo pipefail
cd /opt/kychnet
set -a; . ./.env; set +a
./target/release/kychnet build --out /opt/kychnet/dist
echo "静态站已重建：/opt/kychnet/dist"

# 刷新 EO 站点加速缓存：文件名固定，不刷则用户看到的仍是旧内容
tccli teo CreatePurgeTask \
  --ZoneId "$(cat /opt/kychnet/.edgeone/zone-id)" \
  --Type purge_host \
  --Targets '["c.kych.net"]' \
  --Method delete
echo "EO 缓存刷新已提交（约 30s–2min 生效）"
```

- `/opt/kychnet/.edgeone/zone-id` 存 ZoneId，`chmod 600`；`tccli` 凭据用 `tccli configure` 配置或环境变量，勿写入脚本。
- 更精细可用 `purge_prefix` 只刷 `/posts/`。控制台手动刷新是免配置的兜底方案。

### 5.6 管理端构建与上传（新增步骤）

```bash
cd /opt/kychnet/admin
VITE_API_BASE_URL=https://uh.kych.net pnpm build          # 产物含 API 绝对地址
edgeone pages deploy ./dist -n <Pages项目名> \
  -t "$(cat /opt/kychnet/.edgeone/token)" -e production
```

- CLI 带目录参数时按「直接上传」处理该目录；若因目录里没有 `package.json` 而试图自动构建报错，改传 ZIP：`(cd dist && zip -qr /tmp/admin.zip .)` 后部署 `/tmp/admin.zip`。
- 也可在 Pages 控制台「创建部署」拖拽上传，不依赖服务器 CLI。

### 5.7 保留不动

`main` 已由 PR #7 补上 CORS `Method::PUT`（`saveSettings` 用 PUT，三子域跨源预检需要）；若部署包早于该修复，仍需 `deploy.sh` 里的幂等补丁。`db migrate`、`kychnet build`、systemd 单元、首页「管理端」链接校验（指向 `m` 域），全部保持。

---

## 6. 执行顺序

| Phase | 步骤 | 验收 |
| --- | --- | --- |
| **0 准备** | ① 按第 3 节配 DNS（`c`/`m` CNAME、`origin` A、确认 `uh`）② 安全组 80/443 ③ 生成 Pages Token 与 CAM 密钥、记录 ZoneId | `dig +short c.kych.net CNAME`、`dig +short origin.kych.net A` 正确 |
| **1 EO 控制台** | ① 建站点 `kych.net`（CNAME 接入）+ 归属权验证 ② 加加速域名 `c.kych.net`，按 4.1 配源站/回源 HOST/证书/缓存 ③ 建 Pages「直接上传」项目，绑定 `m.kych.net` | 控制台加速域名状态「已生效」，Pages 项目 HTTPS 已开启 |
| **2 服务器部署** | ① 按第 5 节改 `deploy.sh` 并重新打包 ② 在 WebTerminal 还原命令块、校验 md5 ③ 启动后台部署，`tail -f /var/log/kychnet-deploy.log` | 日志出现部署完成；`nginx -t` 通过；certbot 签出 `kych.net` + `origin.kych.net`（+ `uh`） |
| **3 管理端上传** | `VITE_API_BASE_URL=https://uh.kych.net pnpm build` → `edgeone pages deploy ./dist` | `https://m.kych.net/` 加载，资源 `/assets/*` 200 |
| **4 联调验证** | 按第 8 节逐项 | 全绿 |

改造后的启动命令（新增 `ORIGIN_DOMAIN`，预检已按角色校验故无需 `skip`）：

```bash
sudo bash -c 'nohup env SITE_DOMAIN=c.kych.net ADMIN_DOMAIN=m.kych.net API_DOMAIN=uh.kych.net ORIGIN_DOMAIN=origin.kych.net PURGE_CONFIRM=yes bash /tmp/kychnet-deploy.sh > /var/log/kychnet-deploy.log 2>&1 &'
sleep 2; tail -n 20 /var/log/kychnet-deploy.log
```

---

## 7. 验证清单

```bash
# DNS
dig +short kych.net A                 # 49.232.71.240
dig +short origin.kych.net A          # 49.232.71.240
dig +short c.kych.net CNAME           # EO 分配域名
dig +short m.kych.net CNAME           # Pages 分配域名

# 源站直连（绕过 EO，验证回源 HOST 分流）
curl -sI --resolve origin.kych.net:443:49.232.71.240 https://origin.kych.net/      # 200 内容站
curl -sI --resolve kych.net:443:49.232.71.240        https://kych.net/             # 301 → https://c.kych.net

# 经 EO 边缘
curl -sI https://c.kych.net/          # 200；看 EO-Cache-Status（再次请求应 HIT）
curl -sI https://c.kych.net/posts/%E4%BD%A0%E5%A5%BD.html   # 中文 slug 必须 200
curl -sI https://c.kych.net/posts/%E4%BD%A0%E5%A5%BD.pdf    # PDF 200

# 管理端与跨域
curl -sI https://m.kych.net/          # 200，/assets/* 可达
curl -s -X OPTIONS https://uh.kych.net/api/settings \
  -H 'Origin: https://m.kych.net' -H 'Access-Control-Request-Method: PUT' -i
# 响应需放行 PUT（CORS 幂等补丁生效）

# 证书续期演练
sudo certbot renew --dry-run          # 三个 SAN 域名都能通过 HTTP-01

# 发布闭环
sudo /opt/kychnet/rebuild.sh          # 重建 + 提交缓存刷新
curl -s https://c.kych.net/ | grep '管理端'   # 应指向 https://m.kych.net
```

**中文 slug 是重点回归项**：`build.rs` 磁盘文件名是原始 UTF-8、HTML 链接是 percent-encoded，靠服务端解码后匹配；nginx 会解码，但需确认 EO 边缘与缓存键不改变这一行为。若 404，改 `build.rs` 让文件名与 URL 采用同一套字节，或给中文 slug 加 ASCII 别名。

---

## 8. 发布运维（两条独立链路）

| 触发 | 链路 |
| --- | --- |
| 内容变更 | 管理端保存 → `uh` API 写 PostgreSQL → `sudo /opt/kychnet/rebuild.sh`（build + 刷 EO 缓存）→ `c.kych.net` 可见 |
| 管理端发版 | `VITE_API_BASE_URL=https://uh.kych.net pnpm build` → `edgeone pages deploy ./dist` → Pages 整版替换，即时生效，无需刷缓存 |
| 证书 | certbot 自动续期，源站证书与 EO 边缘证书各自独立 |

两条链路互不影响；`origin.kych.net` 不对外提供入口，用户不会访问它。

---

## 9. 风险与回滚

| 风险 | 处置 |
| --- | --- |
| `c`/`m` CNAME 后 certbot 签发失败 | 已按 5.3 从 SAN 去掉；`certbot renew --dry-run` 提前验证 |
| EO 回源 HOST 自定义后 nginx 未命中 | 5.4 的 `server_name origin.kych.net`；用 `--resolve` 直连验证 |
| 内容更新后边缘仍是旧页 | `rebuild.sh` 必须带缓存刷新；文件名固定，不能靠 TTL |
| 回滚 | 把 `c`/`m` 的 CNAME 改回 A `49.232.71.240`，nginx 恢复 `c`/`m` 两个 server 块即可；**不动数据库、不动后端** |
| 绕过 EO 直连源站 | `origin.kych.net:443` 与 `kych.net` 均可直取内容站；内容为公开静态文件，无实质危害，需要时按 EO 回源 IP 段做白名单 |
| cookie tossing | 内容站是子域 `c`，仍可写 `.kych.net` 域同名 Cookie 干扰 `uh` 会话；会话 Cookie 保持 host-only，后续建议改 `__Host-` 前缀 |
| 凭据 | Pages Token 与 CAM 密钥均落盘 `chmod 600`、勿入仓库；服务器初始密码与部署用 API Token 应尽快轮换 |

其余已知项：PR #7 已修登录限流、会话轮换、TOTP 确认后替换；仍存 `MemoryStore` 重启掉登录、Typst 进程内编译无资源限额、`{{ content|safe }}` 的存储型 XSS 面。

---

## 10. 开放项（执行前确认）

1. EO 站点加速的**套餐与加速区域**（大陆区域需备案确认）；Pages 项目名。
2. `uh.kych.net` 当前解析是否已指到 `49.232.71.240` —— 决定 certbot 是否保留 `-d uh`。
3. `www.kych.net` / `api.kych.net` 旧 A 记录的处置（保留、还是统一 301 到 `c`）。
4. 是否把第 5 节改动**落进 `deploy.sh` 并重新打包**（推荐），或先按原脚本部署再手工打补丁。
5. 刷新缓存用 `tccli`（需 CAM 密钥）还是控制台手动刷新。
