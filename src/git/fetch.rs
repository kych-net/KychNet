//! 从外部 Git 仓库拉取数据的客户端（Git Smart HTTP v0 协议）。
//!
//! 「从外部导入项目」用：给定公开仓库的 https URL，先做 refs 协商拿分支
//! 清单与默认分支，再以空仓库身份发起一次 want 协商拿全量 pack。
//! pack 的解析与 delta 解算复用站内已实现的 `proto`，无需引入 libgit2 或
//! 依赖系统 git CLI。
//!
//! 安全约束：
//! - 只允许 http/https，拒绝 URL 内嵌凭证（避免凭证进日志）。
//! - 目标主机必须是公网地址，阻断 SSRF 打内网；重定向直接拒绝，
//!   否则第一次请求的域名校验会被 302 绕过。
//! - DNS 解析结果与实际连接之间没有钉死（ureq 自己解析），理论上存在
//!   DNS rebinding 窗口；配合公网校验 + 限流，个人站场景下风险可接受。

use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;
use url::Url;

use super::object::Oid;
use super::proto;

/// refs 广告响应的上限。几百个分支的广告也在几十 KB 量级，1 MiB 很宽裕。
const REFS_MAX_BYTES: usize = 1024 * 1024;
/// pack 响应上限。推送通道放行到 256 MiB，导入是全量 clone，取一半。
const PACK_MAX_BYTES: usize = 128 * 1024 * 1024;
/// 整个拉取（连接 + 响应体读完）的总时限。
const FETCH_TIMEOUT: Duration = Duration::from_secs(600);
/// 一次最多导入的分支数。
pub const MAX_BRANCHES: usize = 30;
/// 导入的对象总数上限，防止把数据库塞爆。
pub const MAX_OBJECTS: usize = 100_000;

/// HTTP 头里的 User-Agent，可以带空格。
const USER_AGENT: &str = "kych-net git-import/0.3";
/// want 行里的 `agent=` 能力值必须是单个 token（不能有空格），否则严格
/// 实现的服务端会把它当成未知能力。
const AGENT_CAPABILITY: &str = "kych-net/0.3";

/// refs 协商的结果。
pub struct RemoteInfo {
    /// 远端 HEAD 指向的分支（来自 `symref=HEAD:` 能力，可能缺失）。
    pub default_branch: Option<String>,
    /// 分支名 → 分支顶点 OID，名字不带 `refs/heads/` 前缀。
    pub branches: Vec<(String, Oid)>,
    /// 第一条广告里宣告的能力名（去掉 `=值`），want 行只能请求这些。
    pub capabilities: Vec<String>,
}

/// 校验并规范化远端仓库 URL：只接受 http/https、不带凭证、无查询串。
pub fn normalize_remote_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|_| anyhow!("仓库地址不是合法 URL: {trimmed}"))?;
    if url.scheme() != "https" && url.scheme() != "http" {
        bail!("只支持 http/https 仓库地址");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("请在地址里去掉账号密码；站内只支持导入公开仓库");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("仓库地址不能带查询参数或锚点");
    }
    if url.host_str().unwrap_or_default().is_empty() {
        bail!("仓库地址缺少主机名");
    }
    // 末尾的 `/` 交给拼接时统一处理；git.example.com/repo 与 /repo.git 都合法。
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// 拒绝解析到私网/本机地址的目标，阻断借导入功能探测内网的 SSRF。
fn assert_public_host(url: &Url) -> Result<()> {
    let host = url.host_str().context("仓库地址缺少主机名")?;
    if allow_private_hosts() {
        return Ok(());
    }
    let port = url
        .port_or_known_default()
        .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
    // 主机本身就是 IP 字面量时直接判定；域名则看解析结果。
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(&ip) {
            bail!("不允许导入指向内网或本机的仓库地址");
        }
        return Ok(());
    }
    let addrs: Vec<std::net::SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| anyhow!("仓库主机解析失败: {error}"))?
        .collect();
    if addrs.is_empty() {
        bail!("仓库主机解析不到地址");
    }
    if addrs.iter().any(|addr| !is_public_ip(&addr.ip())) {
        bail!("不允许导入指向内网或本机的仓库地址");
    }
    Ok(())
}

/// 私有部署可以从内网 Git 服务器导入时显式打开；默认关闭，公网站点保持
/// 内网探测的防护。
const ALLOW_PRIVATE_ENV: &str = "KYCHNET_IMPORT_ALLOW_PRIVATE";

fn allow_private_hosts() -> bool {
    matches!(
        std::env::var(ALLOW_PRIVATE_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// 只放行真正的公网地址；`IpAddr::is_global` 语义太松（放行了共享地址段
/// 与保留段），这里按网段显式排除。
fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            !matches!(
                octets,
                [0, ..]                        // 本网络
                    | [10, ..]                 // 私网
                    | [127, ..]                // 回环
                    | [169, 254, ..]           // 链路本地（含云元数据）
                    | [100, 64..=127, ..]       // 运营商级 NAT（CGNAT）
                    | [172, 16..=31, ..]        // 私网
                    | [192, 0, 0, ..]          // IETF 协议保留
                    | [192, 0, 2, ..]          // TEST-NET-1
                    | [192, 168, ..]           // 私网
                    | [198, 18 | 19, ..]       // 基准测试
                    | [198, 51, 100, ..]       // TEST-NET-2
                    | [203, 0, 113, ..]        // TEST-NET-3
                    | [224..=255, ..]          // 组播 / 广播
            )
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            let first = segments[0];
            // ::/128（未指定）与 ::1（回环）：首段全零。
            if first == 0 {
                return false;
            }
            // IPv4 兼容 / IPv4 转换段。
            if matches!(first, 0x0064 | 0x0065 | 0x00f0..=0x00ff) {
                return false;
            }
            // 链路本地 fe80::/10 与 ULA 私网 fc00::/7。
            if matches!(first, 0xfe80..=0xfebf | 0xfc00..=0xfdff) {
                return false;
            }
            // 组播 ff00::/8。
            if first >= 0xff00 {
                return false;
            }
            // 文档示例段 2001:db8::/32。
            if first == 0x2001 && segments[1] == 0x0db8 {
                return false;
            }
            true
        }
    }
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout(FETCH_TIMEOUT)
        // 重定向会把第一次请求没检查过的目标直接交给 ureq 连接，等于绕过
        // 公网校验，因此禁掉；主流托管平台的 /info/refs 不需要重定向。
        .redirects(0)
        .user_agent(USER_AGENT)
        .build()
}

/// 把 ureq 的错误翻译成面向用户的中文提示。
fn http_error(context: &str, error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::Status(code, _) => {
            if matches!(code, 401 | 403 | 404) {
                anyhow!("{context}失败：仓库不存在或不是公开仓库（HTTP {code}）")
            } else {
                anyhow!("{context}失败：远端返回 HTTP {code}")
            }
        }
        ureq::Error::Transport(transport) => {
            anyhow!("{context}失败：{}（{}）", transport, transport.kind())
        }
    }
}

fn read_capped(reader: impl Read, cap: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    reader
        .take((cap + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| anyhow!("读取远端响应失败: {error}"))?;
    if body.len() > cap {
        bail!("远端响应超过 {} MiB，超出导入上限", cap / (1024 * 1024));
    }
    Ok(body)
}

/// 读取 refs 广告，解析分支清单与默认分支。
fn parse_refs_advertisement(data: &[u8]) -> Result<RemoteInfo> {
    let (service_lines, service_end) =
        proto::parse_pkt_lines(data).context("远端响应不是合法的 pkt-line 流")?;
    let service = service_lines
        .first()
        .map(|line| String::from_utf8_lossy(line).trim().to_string())
        .unwrap_or_default();
    if !service.starts_with("# service=git-upload-pack") {
        bail!("远端地址不是 Git smart HTTP 服务");
    }
    let (lines, _) = proto::parse_pkt_lines(&data[service_end..])?;

    let mut default_branch = None;
    let mut capabilities = Vec::new();
    let mut branches = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\n');
        let (head, caps) = match text.split_once('\0') {
            Some((head, caps)) => (head, Some(caps)),
            None => (text, None),
        };
        if index == 0 {
            if let Some(caps) = caps {
                for capability in caps.split_whitespace() {
                    if let Some(target) = capability.strip_prefix("symref=HEAD:refs/heads/") {
                        default_branch = Some(target.to_string());
                    }
                    // 只记能力名：want 行里的 `ofs-delta` 之类不带取值，`agent=x`
                    // 这种要按原样重新拼，统一按名字判断后再拼。
                    capabilities.push(
                        capability
                            .split('=')
                            .next()
                            .unwrap_or(capability)
                            .to_string(),
                    );
                }
            }
        }
        let Some((oid_hex, refname)) = head.split_once(' ') else {
            continue;
        };
        let oid = match Oid::from_hex(oid_hex) {
            Ok(oid) => oid,
            // 空仓库的广告里是 capabilities^{} 行，OID 为全零，跳过即可。
            Err(_) => continue,
        };
        if let Some(branch) = refname.strip_prefix("refs/heads/") {
            branches.push((branch.to_string(), oid));
        }
    }
    Ok(RemoteInfo {
        default_branch,
        branches,
        capabilities,
    })
}

/// refs 协商：GET /info/refs?service=git-upload-pack（v0 广告）。
pub fn fetch_refs_blocking(url: &str) -> Result<RemoteInfo> {
    let url = Url::parse(url)?;
    assert_public_host(&url)?;
    let endpoint = format!(
        "{}/info/refs?service=git-upload-pack",
        url.as_str().trim_end_matches('/')
    );
    let response = http_agent()
        .get(&endpoint)
        // pack 与广告都是二进制/紧凑文本，显式关掉压缩，避免 ureq 的 gzip
        // 解压只作用于字符串入口、拿到的字节流却不是原始 pack。
        .set("Accept-Encoding", "identity")
        .call()
        .map_err(|error| http_error("获取远端仓库信息", error))?;
    let body = read_capped(response.into_reader(), REFS_MAX_BYTES)?;
    if body.is_empty() {
        bail!("远端仓库信息为空，地址可能不是 Git 仓库");
    }
    parse_refs_advertisement(&body).map_err(|error| {
        // 个别托管平台会无视未带 Git-Protocol 头的请求，直接回 protocol v2
        // 广告；这种形态解析不出 service 行，点名提示方便排查。
        if std::str::from_utf8(&body).unwrap_or("").contains("version 2") {
            anyhow!("远端仓库返回了不支持的协议版本（protocol v2）：{error:#}")
        } else {
            error
        }
    })
}

/// 拼接 want 请求体：第一个 want 携带能力，末尾 done 结束协商。
///
/// 只请求服务端宣告过的 `ofs-delta`：能力必须来自对面的广告，凭空宣告
/// 未支持的能力在严格实现里会被当成协议错误。
fn build_want_body(wants: &[Oid], capabilities: &[String]) -> Vec<u8> {
    let mut first = String::new();
    if capabilities.iter().any(|name| name == "ofs-delta") {
        first.push_str(" ofs-delta");
    }
    first.push_str(&format!(" agent={AGENT_CAPABILITY}"));
    let mut body = Vec::new();
    for (index, oid) in wants.iter().enumerate() {
        let line = if index == 0 {
            format!("want {oid}{first}\n")
        } else {
            format!("want {oid}\n")
        };
        body.extend(proto::pkt_str(&line));
    }
    body.extend(proto::flush());
    body.extend(proto::pkt_str("done\n"));
    body
}

/// 从 upload-pack 响应里剥掉 pkt-line（NAK / ACK / shallow 等），取出 pack。
fn extract_pack(data: &[u8]) -> Result<Vec<u8>> {
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        if &data[pos..pos + 4] == b"PACK" {
            return Ok(data[pos..].to_vec());
        }
        let header = std::str::from_utf8(&data[pos..pos + 4])
            .map_err(|_| anyhow!("远端 pack 响应格式非法"))?;
        let length = usize::from_str_radix(header, 16)
            .map_err(|_| anyhow!("远端 pack 响应格式非法"))?;
        match length {
            0 | 1 | 2 => {
                pos += 4;
                continue;
            }
            3 => bail!("远端 pack 响应格式非法"),
            _ => {}
        }
        if pos + length > data.len() {
            bail!("远端 pack 响应被截断");
        }
        let payload = String::from_utf8_lossy(&data[pos + 4..pos + length]);
        let text = payload.trim_end_matches('\n');
        if text.starts_with("ERR ") {
            bail!("远端拒绝本次导入: {text}");
        }
        // NAK / ACK / shallow / unshallow 都不是 pack，跳过继续找。
        pos += length;
    }
    bail!("远端响应里没有 pack 数据");
}

/// 全量拉取：以空仓库身份 want 全部分支顶点，一轮协商直接收 pack。
pub fn fetch_pack_blocking(url: &str, wants: &[Oid], capabilities: &[String]) -> Result<Vec<u8>> {
    let url = Url::parse(url)?;
    assert_public_host(&url)?;
    if wants.is_empty() {
        bail!("没有要导入的分支");
    }
    let endpoint = format!("{}/git-upload-pack", url.as_str().trim_end_matches('/'));
    let response = http_agent()
        .post(&endpoint)
        .set("Content-Type", "application/x-git-upload-pack-request")
        .set("Accept", "application/x-git-upload-pack-result")
        .set("Accept-Encoding", "identity")
        // ureq 2 的 `send` 收的是 `Read`，整个请求体已在内存里，走字节入口。
        .send_bytes(&build_want_body(wants, capabilities))
        .map_err(|error| http_error("拉取远端仓库数据", error))?;
    let data = read_capped(response.into_reader(), PACK_MAX_BYTES)?;
    extract_pack(&data)
}

/// 阻塞拉取放到独立线程池，避免占死 Tokio worker。
pub async fn fetch_refs(url: String) -> Result<RemoteInfo> {
    tokio::task::spawn_blocking(move || fetch_refs_blocking(&url))
        .await
        .map_err(|error| anyhow!("导入任务异常退出: {error}"))?
}

pub async fn fetch_pack(
    url: String,
    wants: Vec<Oid>,
    capabilities: Vec<String>,
) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || fetch_pack_blocking(&url, &wants, &capabilities))
        .await
        .map_err(|error| anyhow!("导入任务异常退出: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_credentials_and_schemes() {
        assert!(normalize_remote_url("https://github.com/a/b.git").is_ok());
        assert!(normalize_remote_url("  https://github.com/a/b.git/  ").is_ok());
        assert!(normalize_remote_url("https://user:pass@github.com/a/b").is_err());
        assert!(normalize_remote_url("git@github.com:a/b.git").is_err());
        assert!(normalize_remote_url("file:///tmp/repo").is_err());
        assert!(normalize_remote_url("https://github.com/a/b?x=1").is_err());
        assert!(normalize_remote_url("ftp://github.com/a/b").is_err());
        assert!(normalize_remote_url("not a url").is_err());
    }

    #[test]
    fn private_and_reserved_addresses_are_rejected() {
        assert!(is_public_ip(&"93.184.216.34".parse().unwrap()));
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "172.31.255.255",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
        ] {
            assert!(!is_public_ip(&ip.parse().unwrap()), "{ip} 不应算公网");
        }
        assert!(is_public_ip(&"2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()));
        for ip in ["::1", "fe80::1", "fc00::1", "ff02::1", "::ffff:127.0.0.1"] {
            assert!(!is_public_ip(&ip.parse().unwrap()), "{ip} 不应算公网");
        }
    }

    fn pkt(data: &[u8]) -> Vec<u8> {
        proto::pkt(data)
    }

    fn sample_advertisement() -> Vec<u8> {
        let mut data = pkt(b"# service=git-upload-pack\n");
        data.extend(proto::flush());
        let oid = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        data.extend(pkt(
            format!("{oid} HEAD\\0multi_ack symref=HEAD:refs/heads/main\n")
                .replace("\\0", "\u{0}")
                .as_bytes(),
        ));
        data.extend(pkt(format!("{oid} refs/heads/main\n").as_bytes()));
        data.extend(pkt(format!("{oid} refs/heads/dev\n").as_bytes()));
        data.extend(pkt(format!("{oid} refs/tags/v1\n").as_bytes()));
        data.extend(proto::flush());
        data
    }

    #[test]
    fn parses_refs_advertisement_with_symref() {
        let info = parse_refs_advertisement(&sample_advertisement()).unwrap();
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert!(info.capabilities.iter().any(|name| name == "multi_ack"));
        assert!(!info.capabilities.iter().any(|name| name == "ofs-delta"));
        let names: Vec<&str> = info.branches.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["main", "dev"]);
    }

    #[test]
    fn extracts_pack_after_nak() {
        let mut data = pkt(b"NAK\n");
        data.extend_from_slice(b"PACKpayload");
        assert_eq!(extract_pack(&data).unwrap(), b"PACKpayload");
    }

    #[test]
    fn extracts_pack_after_flush_and_shallow() {
        let mut data = pkt(b"shallow e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n");
        data.extend(proto::flush());
        data.extend(pkt(b"NAK\n"));
        data.extend_from_slice(b"PACKrest");
        assert_eq!(extract_pack(&data).unwrap(), b"PACKrest");
    }

    #[test]
    fn surfaces_remote_errors() {
        let mut data = pkt(b"NAK\n");
        data.extend(pkt(b"ERR pack too large\n"));
        assert!(extract_pack(&data).is_err());
        let data = pkt(b"NAK\n");
        assert!(extract_pack(&data).is_err());
    }

    #[test]
    fn want_body_carries_capabilities_once() {
        let oid = Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let capabilities = vec!["multi_ack".to_string(), "ofs-delta".to_string()];
        let body = build_want_body(&[oid, oid], &capabilities);
        let text = String::from_utf8(body).unwrap();
        assert_eq!(text.matches("want ").count(), 2);
        assert!(text.contains("ofs-delta"));
        assert!(text.ends_with("0009done\n"));
        // 服务端没宣告 ofs-delta 时不能凭空请求，否则严格实现会拒绝。
        let body = build_want_body(&[oid], &["multi_ack".to_string()]);
        let text = String::from_utf8(body).unwrap();
        assert!(!text.contains("ofs-delta"));
        assert!(text.contains("agent=kych-net/0.3"));
    }
}
