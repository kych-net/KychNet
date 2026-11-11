#!/usr/bin/env bash
# 把站点入口从 c.kych.net / m.kych.net 切到 eo.kych.net。
#
# 前置条件（缺一不可，否则改完管理端登录会立刻失败）：
#   1. nginx 已加载 kychnet-eo.conf（eo.kych.net vhost）
#   2. EdgeOne 上 eo.kych.net 加速域名已生效，回源 HOST=eo.kych.net、SNI=kych.net
#   3. DNSPod 里 eo.kych.net 已指向 EO 分配的加速域名
#
# 改这三项是原子的：CORS 白名单和 Origin 校验都读 ADMIN_ORIGIN，
# 只能指向一个域，所以旧域名会在重启瞬间失去接口权限。
set -euo pipefail

ENV_FILE=/opt/kych-net/.env
BAK="${ENV_FILE}.bak-$(date +%Y%m%d%H%M%S)"

echo "==> 备份 $ENV_FILE -> $BAK"
cp -a "$ENV_FILE" "$BAK"

echo "==> 改写 SITE_URL / ADMIN_ORIGIN / WEBAUTHN_ORIGIN"
sed -i \
  -e 's|^SITE_URL=.*|SITE_URL=https://eo.kych.net|' \
  -e 's|^ADMIN_ORIGIN=.*|ADMIN_ORIGIN=https://eo.kych.net|' \
  -e 's|^WEBAUTHN_ORIGIN=.*|WEBAUTHN_ORIGIN=https://eo.kych.net|' \
  "$ENV_FILE"

grep -E '^(SITE_URL|ADMIN_ORIGIN|WEBAUTHN_RP_ID|WEBAUTHN_ORIGIN)=' "$ENV_FILE"

echo "==> 重启后端"
systemctl restart kych-net
sleep 2
systemctl is-active kych-net

echo "==> 重建内容站（SITE_URL 变了，链接与 canonical 都要跟着变）"
# 必须在 /opt/kych-net 下执行：配置靠 dotenvy 读当前目录的 .env，
# 直接 sudo -u 不加 cd 会报 "DATABASE_URL 未设置"。
sudo -u kych-net bash -c 'cd /opt/kych-net && ./kych-net build'

echo "==> 完成。回滚：cp -a $BAK $ENV_FILE && systemctl restart kych-net && sudo -u kych-net bash -c 'cd /opt/kych-net && ./kych-net build'"
