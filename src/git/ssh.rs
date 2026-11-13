//! Git over SSH：内建 SSH 服务，把 smart HTTP 的协议实现搬到 SSH 通道上。
//!
//! 架构说明：
//!
//! - 认证只认公钥（`ssh_key::authenticate`），按指纹反查用户，不依赖
//!   系统账号，也不需要系统 sshd 参与——本模块自己监听独立端口
//!   （`SSH_BIND`，如 `0.0.0.0:2222`），与服务器登录用的 22 端口互不相干。
//! - 客户端 exec 的命令是 `git-upload-pack '/slug.git'` 或
//!   `git-receive-pack '/slug.git'`；协议版本由客户端通过 SSH env 请求
//!   `GIT_PROTOCOL=version=2` 协商（与真实 sshd + git 的机制一致）。
//! - v0 fetch 是有状态会话（客户端分多轮发 want/have/done），而 HTTP 的
//!   upload-pack 是无状态 RPC，所以这里把多轮请求累积成一个
//!   `UploadRequest` 再交给 `http::upload_pack_request()`。协商轮直接复用
//!   它的 `!done` 输出，浅边界段只发一次；done 轮按「浅边界是否已发过」
//!   决定原样回完整响应还是只回 packfile，避免与 HTTP 的顺序出现偏差。
//! - v0 push 读命令段后分两路：非删除推送会跟着 pack，正常客户端发完会
//!   关闭写端，但 libgit2 等实现可能保持写端打开，所以读取带空闲超时，
//!   超时后按 packfile 自身的长度判定是否已收齐，不依赖 EOF；
//!   纯删除推送没有 pack 也不关闭写端，必须立即回 report，不能等 EOF。
//! - v2 fetch 是 stateless 请求，逐条转发给 `upload_pack_v2`；一旦响应里
//!   带上 `packfile` 段就立即收尾，不等客户端关闭写端。
//! - `git-lfs-authenticate` 是 LFS 客户端在 SSH 远程上执行的命令，返回
//!   basic 传输用的 batch 端点与短期令牌，使 LFS 上传不必另配 HTTP 凭据。
//! - 通道与协议版本按 `ChannelId` 分别保存，避免同一连接开多个 session
//!   时互相覆盖（russh 允许一个连接承载多个通道）。

use anyhow::{bail, Context, Result};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use serde_json::json;
use sha1::{Digest, Sha1};
use sqlx::PgPool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::git::proto;
use crate::git::ssh_key;
use crate::models::User;
use crate::project::{find_project_by_slug, Project};

/// SSH 服务配置。
#[derive(Debug, Clone)]
pub struct SshConfig {
    /// 监听地址，如 `0.0.0.0:2222`。
    pub bind: String,
    /// 主机私钥文件（OpenSSH 格式）；不存在时自动生成 ed25519 并写入。
    pub host_key: PathBuf,
    /// Git HTTP 服务的对外根地址，用于拼 LFS 的 batch 端点。
    pub git_http_base: String,
}

/// 服务器入口：加载主机密钥、绑定端口并进入 accept 循环。
///
/// 返回的 future 长驻；由 `api::serve` 按配置决定是否 spawn。
pub async fn serve(cfg: SshConfig, pool: PgPool) -> Result<()> {
    let host_key = load_or_create_host_key(&cfg.host_key)?;
    let config = std::sync::Arc::new(russh::server::Config {
        keys: vec![host_key],
        // 会话只跑 git 命令，闲置 10 分钟主动断开，避免僵死连接占着 PG 连接池。
        inactivity_timeout: Some(std::time::Duration::from_secs(600)),
        ..Default::default()
    });

    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .with_context(|| format!("SSH 端口绑定失败: {}", cfg.bind))?;
    tracing::info!(bind = %cfg.bind, "Git SSH 服务已启动");

    let mut server = SshServer {
        pool,
        git_http_base: cfg.git_http_base.clone(),
    };
    server
        .run_on_socket(config, &listener)
        .await
        .map_err(|error| anyhow::anyhow!("SSH 服务退出: {error}"))
}

/// 主机私钥：已有文件直接加载，否则生成 ed25519 并以 0600 落盘。
///
/// 换主机密钥会让老客户端报 host key 变更警告，所以生成一次后固定复用。
fn load_or_create_host_key(path: &PathBuf) -> Result<russh::keys::PrivateKey> {
    if path.exists() {
        return russh::keys::load_secret_key(path, None)
            .with_context(|| format!("SSH 主机密钥加载失败: {}", path.display()));
    }
    let key = russh::keys::PrivateKey::random(&mut russh_rand::rng(), russh::keys::Algorithm::Ed25519)
        .context("生成 SSH 主机密钥失败")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let text = key
        .to_openssh(::ssh_key::LineEnding::LF)
        .map_err(|error| anyhow::anyhow!("SSH 主机密钥编码失败: {error}"))?;
    std::fs::write(path, text.as_bytes())
        .with_context(|| format!("SSH 主机密钥写入失败: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    tracing::info!(path = %path.display(), "已生成新的 SSH 主机密钥");
    Ok(key)
}

struct SshServer {
    pool: PgPool,
    git_http_base: String,
}

impl russh::server::Server for SshServer {
    type Handler = GitHandler;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
        GitHandler {
            pool: self.pool.clone(),
            channels: HashMap::new(),
            protocols: HashMap::new(),
            git_http_base: self.git_http_base.clone(),
            auth_failures: 0,
            user: None,
        }
    }
}

/// 单个 SSH 连接允许同时持有的会话通道数上限。
///
/// 客户端可能反复开 session 却从不发 exec，也不能让它无限占用
/// `Channel<Msg>`（每个都持有发送端）。超过上限直接 drop reply，
/// russh 会回 `AdministrativelyProhibited` 拒绝这次开通道请求。
const MAX_CHANNELS: usize = 32;

/// 公钥认证失败的退避时长：按失败次数线性增长，封顶 1 秒。
///
/// 封顶是关键——失败次数只能用来拖慢爆破，不能变成「达到阈值就
/// 永久拒绝」的开关。否则 ssh-agent 里 key 较多的正常用户，在同一条
/// 连接上试完前 10 个 key 后连正确的 key 也认证不了，只能断开重连。
fn auth_backoff(failures: u32) -> Duration {
    let millis = 100u64.saturating_mul(failures as u64).min(1_000);
    Duration::from_millis(millis)
}

/// 单个 SSH 连接的处理器。
///
/// russh 会在签名验证通过后回调 `auth_publickey`，此时才能把用户身份
/// 存进 handler；`exec_request` 里据此决定放行还是拒绝。
struct GitHandler {
    pool: PgPool,
    /// 已打开的会话通道，按 id 索引：一个连接可以承载多个通道，
    /// 用单个 `Option<Channel>` 存会让后开的通道覆盖先开的那个。
    channels: HashMap<ChannelId, Channel<Msg>>,
    /// 每个通道单独记录 `GIT_PROTOCOL=version=2` 的协商结果，
    /// 否则一个通道协商 v2 会把整条连接的其它通道也带成 v2。
    protocols: HashMap<ChannelId, bool>,
    git_http_base: String,
    /// 公钥认证连续失败次数，只用于退避限速，成功一次即清零。
    auth_failures: u32,
    user: Option<User>,
}

impl GitHandler {
    /// 释放一个通道的槽位与协商标记。
    ///
    /// exec 成功时会把 `Channel` 移交给会话任务，这里再 remove 只是 no-op；
    /// 真正要清的是「开了 session 但没发 exec / 命令被拒绝 / 通道关闭」
    /// 这些残留。
    fn release_channel(&mut self, id: ChannelId) {
        self.channels.remove(&id);
        self.protocols.remove(&id);
    }
}

impl russh::server::Handler for GitHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<Auth, Self::Error> {
        // 密码登录一律拒绝：本服务只服务 git，凭据只有 SSH 公钥。
        Ok(Auth::reject())
    }

    async fn auth_publickey(
        &mut self,
        _: &str,
        key: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        // 始终验证公钥。旧实现在失败 10 次后直接 reject 且不再查库，
        // `auth_failures = 0` 只在成功分支里，于是那条连接被永久锁死：
        // key 较多的正常用户即使随后提供正确的 key 也认证不了。
        // 现在失败次数只决定退避时长（有上限），成功一次即清零。
        match ssh_key::authenticate(&self.pool, key.public_key).await {
            Ok(Some(user)) => {
                tracing::info!(username = %user.username, "SSH 公钥认证通过");
                self.user = Some(user);
                self.auth_failures = 0;
                Ok(Auth::Accept)
            }
            Ok(None) => {
                self.auth_failures = self.auth_failures.saturating_add(1);
                tokio::time::sleep(auth_backoff(self.auth_failures)).await;
                Ok(Auth::reject())
            }
            Err(error) => {
                tracing::warn!(%error, "SSH 公钥认证查询失败");
                self.auth_failures = self.auth_failures.saturating_add(1);
                tokio::time::sleep(auth_backoff(self.auth_failures)).await;
                Ok(Auth::reject())
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channels.len() >= MAX_CHANNELS {
            drop(reply); // russh 的 Drop 会回 AdministrativelyProhibited
            return Ok(());
        }
        reply.accept().await;
        // 先记 id 再存通道；同一连接后续再开 session 不会覆盖已有通道。
        let id = channel.id();
        self.channels.insert(id, channel);
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.release_channel(channel);
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.release_channel(channel);
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // 与 sshd 的 AcceptEnv GIT_PROTOCOL 等价：只认协议版本这一项。
        // 注意 `&mut Session` 上的应答方法是同步的（Handle 上才有 async 版本）。
        if name == "GIT_PROTOCOL" && value.trim() == "version=2" {
            // 只影响当前通道；channel_open_session 里已注册的通道才接受。
            if self.channels.contains_key(&channel) {
                self.protocols.insert(channel, true);
                session.channel_success(channel)?;
            } else {
                session.channel_failure(channel)?;
            }
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = match std::str::from_utf8(data) {
            Ok(value) => value,
            Err(_) => {
                self.release_channel(channel);
                session.channel_failure(channel)?;
                return Ok(());
            }
        };
        let Some((service, slug)) = parse_exec(command) else {
            session.data(
                channel,
                error_pkt(&format!(
                    "kych-net git: 只支持 git-upload-pack / git-receive-pack / \
                     git-lfs-authenticate，收到: {command}"
                )),
            )?;
            session.close(channel)?;
            self.release_channel(channel);
            return Ok(());
        };
        let Some(user) = self.user.clone() else {
            self.release_channel(channel);
            session.channel_failure(channel)?;
            return Ok(());
        };
        // 按 id 取通道：取不到就只拒绝本次请求，不会像旧实现那样先 take()
        // 掉唯一的槽位、再发现 id 不符，导致整条连接上的其它通道也失效。
        let Some(channel_obj) = self.channels.remove(&channel) else {
            self.release_channel(channel);
            session.channel_failure(channel)?;
            return Ok(());
        };

        // 项目解析与可见性检查在 exec 回调里完成，失败直接拒绝命令。
        let project = match resolve_project(&self.pool, &slug).await {
            Ok(project) => project,
            Err(error) => {
                tracing::info!(slug = %slug, %error, "SSH git 请求被拒绝");
                session.data(channel, error_pkt(&error.to_string()))?;
                session.close(channel)?;
                self.release_channel(channel);
                return Ok(());
            }
        };

        session.channel_success(channel)?;
        let pool = self.pool.clone();
        let protocol_v2 = self.protocols.remove(&channel).unwrap_or(false);
        let git_http_base = self.git_http_base.clone();
        // git 客户端在读完 report 后还会检查 ssh 退出码：没有 exit-status
        // 请求时 ssh 以 -1 退出，推送结果会被误报成失败，所以必须显式回状态。
        let handle = session.handle();
        tokio::spawn(async move {
            let channel_id = channel_obj.id();
            let result = run_service(
                pool,
                project,
                user,
                service,
                protocol_v2,
                &git_http_base,
                channel_obj,
            )
            .await;
            let status = if result.is_ok() { 0 } else { 1 };
            if let Err(error) = &result {
                tracing::warn!(%error, "SSH git 会话异常结束");
            }
            let _ = handle.exit_status_request(channel_id, status).await;
        });
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    Upload,
    Receive,
    LfsAuth(LfsOperation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LfsOperation {
    Download,
    Upload,
}

/// 匹配命令名，要求后面是空白或结束，避免把 `git-upload-pack-evil` 当成命令。
fn strip_command<'a>(command: &'a str, name: &str) -> Option<&'a str> {
    let rest = command.strip_prefix(name)?;
    match rest.chars().next() {
        None => Some(""),
        Some(ch) if ch.is_whitespace() => Some(rest),
        _ => None,
    }
}

/// 解析 exec 命令，支持可选的 `--protocol=version=2` 与引号包裹的路径。
fn parse_exec(command: &str) -> Option<(Service, String)> {
    let command = command.trim();
    if let Some(result) = parse_lfs_auth(command) {
        return Some(result);
    }
    let (service, rest) = if let Some(rest) = strip_command(command, "git-upload-pack") {
        (Service::Upload, rest)
    } else if let Some(rest) = strip_command(command, "git-receive-pack") {
        (Service::Receive, rest)
    } else {
        return None;
    };

    // v0/v2 的版本协商走 env 请求，命令行上的 flag 只做兼容记录。
    let rest = rest.trim_start();
    let rest = match rest.split_once(' ') {
        Some((flag, tail)) if flag == "--protocol=version=2" => tail,
        _ => rest,
    };

    // git 客户端总是给路径加引号；裸路径（手工测试）也接受。
    let path = match rest.trim() {
        value if value.len() >= 2 && (value.starts_with('\'') || value.starts_with('"')) => {
            let quote = value.chars().next().expect("已检查非空");
            // 闭引号缺失时不能按字节切尾：末尾若是多字节字符会切在
            // 字符中间直接 panic。
            if !value.ends_with(quote) {
                return None;
            }
            value[1..value.len() - 1].to_string()
        }
        value => value.trim().to_string(),
    };

    Some((service, normalize_slug(&path)?))
}

/// 接受 `/slug.git`、`slug.git`、`~/slug.git` 等形式，统一成 slug。
fn normalize_slug(path: &str) -> Option<String> {
    let slug = path.trim_start_matches(['/', '~']).trim_end_matches('/');
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    if slug.is_empty() {
        None
    } else {
        Some(slug.to_string())
    }
}

/// 把命令参数按空白切开，允许单/双引号包裹含空格的仓库路径。
fn shell_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in text.chars() {
        match quote {
            Some(end) if ch == end => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// `git-lfs-authenticate <repo> <download|upload>`。
///
/// LFS 客户端通过 SSH 远程拿 basic 传输端点时执行这条命令；旧实现只认
/// upload-pack / receive-pack，会把 LFS 请求当成非法命令直接拒绝。
fn parse_lfs_auth(command: &str) -> Option<(Service, String)> {
    let rest = strip_command(command, "git-lfs-authenticate")?;
    let tokens = shell_tokens(rest);
    // 只取前两个 token：`git-lfs-authenticate <path> <operation>`，
    // 某些实现还会在末尾附加额外参数，忽略即可。
    let (path, operation) = match tokens.as_slice() {
        [path, op, ..] => (path, op),
        _ => return None,
    };
    let operation = match operation.as_str() {
        "download" => LfsOperation::Download,
        "upload" => LfsOperation::Upload,
        _ => return None,
    };
    Some((Service::LfsAuth(operation), normalize_slug(path)?))
}

/// SSH 侧的项目可见性与 HTTP 一致：未归档项目即可访问。
/// （SSH 本身要求认证，所以 fetch 比 HTTP 的匿名可见更严格。）
async fn resolve_project(pool: &PgPool, slug: &str) -> Result<Project> {
    match find_project_by_slug(pool, slug).await? {
        Some(project) if project.archived_at.is_none() => Ok(project),
        _ => bail!("项目不存在"),
    }
}

/// 把一个 exec 会话跑完：写广告 → 按协议循环 → 收尾。
async fn run_service(
    pool: PgPool,
    project: Project,
    user: User,
    service: Service,
    protocol_v2: bool,
    git_http_base: &str,
    channel: Channel<Msg>,
) -> Result<()> {
    let mut stream = channel.into_stream();
    let result = match service {
        Service::Upload if protocol_v2 => {
            session_upload_pack_v2(&pool, &project, &mut stream).await
        }
        Service::Upload => session_upload_pack_v0(&pool, &project, &mut stream).await,
        Service::Receive => session_receive_pack(&pool, &project, &user, &mut stream).await,
        Service::LfsAuth(operation) => {
            session_lfs_authenticate(
                &pool,
                &project,
                &user,
                operation,
                git_http_base,
                &mut stream,
            )
            .await
        }
    };
    if let Err(error) = &result {
        // 协议层错误尽量以 pkt-line ERR 送达；LFS 客户端期待 JSON，
        // 混发 pkt-line 会被当成非法响应，所以单独回一条 JSON message。
        let payload = match service {
            Service::LfsAuth(_) => json!({ "message": error.to_string() }).to_string().into_bytes(),
            _ => error_pkt(&error.to_string()),
        };
        let _ = stream.write_all(&payload).await;
    }
    // shutdown 会给客户端发 EOF；ChannelStream 被 drop 时自动 close 通道。
    let _ = stream.shutdown().await;
    result
}

/// LFS 的 SSH 认证响应：给出 basic 传输的 batch 端点与短期 Git 令牌。
///
/// `git-lfs-authenticate` 走 SSH，但 LFS 对象本身走 HTTP basic 传输；
/// 上传时 batch 端点要求 `git_write_user()`，所以为已通过公钥认证的用户
/// 签一个一小时过期、随用随换的令牌，客户端不必另配 HTTP 凭据。
/// 下载端点匿名可读，不写库、不发令牌。
async fn session_lfs_authenticate<S>(
    pool: &PgPool,
    project: &Project,
    user: &User,
    operation: LfsOperation,
    git_http_base: &str,
    stream: &mut S,
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    let base = git_http_base.trim_end_matches('/');
    // 注意这里给的是 **LFS 端点基址**，不是 batch 完整 URL：
    // git-lfs 的 NewRequest 会把 ssh 认证响应里的 href 当 prefix，再用
    // joinURL(prefix, "objects/batch") 拼出请求地址。写成
    // `.../info/lfs/objects/batch` 会变成 `.../objects/batch/objects/batch`。
    // 站点实际处理的是 `/git/<slug>.git/info/lfs/objects/batch`。
    let href = format!(
        "{base}/git/{}.git/info/lfs",
        percent_encode_segment(&project.slug)
    );
    let mut response = json!({ "href": href });
    if operation == LfsOperation::Upload {
        const TTL_SECS: i64 = 3600;
        let token = crate::git::token::create_ephemeral(
            pool,
            user.id,
            crate::git::token::LFS_EPHEMERAL_NAME,
            TTL_SECS,
        )
        .await?;
        response["header"] = json!({ "Authorization": format!("Bearer {token}") });
        response["expires_in"] = json!(TTL_SECS);
    }
    stream.write_all(&serde_json::to_vec(&response)?).await?;
    stream.flush().await?;
    Ok(())
}

/// 只编码 URL 路径段里必须编码的字节（与 HTTP 侧保持一致）。
fn percent_encode_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 把服务端错误包装成 pkt-line `ERR ...`，按 UTF-8 字符截断。
fn error_pkt(message: &str) -> Vec<u8> {
    let one_line = message.lines().collect::<Vec<_>>().join("; ");
    let truncated: String = one_line.chars().take(512).collect();
    let mut out = proto::pkt_str(&format!("ERR {truncated}\n"));
    out.extend(proto::flush());
    out
}

/// 从通道读取一串 pkt-line，返回**原始字节**（含结尾的 flush-pkt）。
///
/// 停止条件：flush-pkt、v0 协商的裸 `done` 行（它后面没有 flush）、EOF。
/// 返回空表示对端已关闭。原始字节直接喂给解析器，delim/flush 语义不丢。
async fn read_pkt_lines<R: AsyncRead + Unpin>(
    reader: &mut R,
    stop_on_done: bool,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let mut header = [0u8; 4];
        match reader.read_exact(&mut header).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                // 客户端关写端：如果没有已读内容就当作会话结束。
                if out.is_empty() {
                    return Ok(Vec::new());
                }
                bail!("pkt-line 头部被截断");
            }
            Err(error) => return Err(error.into()),
        }
        let text = std::str::from_utf8(&header)
            .map_err(|_| anyhow::anyhow!("pkt-line 长度不是 ASCII"))?;
        let length = usize::from_str_radix(text, 16)
            .map_err(|_| anyhow::anyhow!("pkt-line 长度非法: {text}"))?;
        if length == 0 {
            // flush-pkt：一轮请求结束。
            out.extend_from_slice(&header);
            return Ok(out);
        }
        if length == 1 {
            // delim-pkt（v2 的命令/参数分隔标记），原样保留。
            out.extend_from_slice(&header);
            continue;
        }
        if length < 4 || length > 65524 {
            bail!("pkt-line 长度越界: {length}");
        }
        let mut payload = vec![0u8; length - 4];
        reader.read_exact(&mut payload).await?;

        if stop_on_done && payload.trim_ascii_end() == b"done" {
            out.extend_from_slice(proto::pkt_str("done\n").as_slice());
            return Ok(out);
        }
        out.extend(proto::pkt(&payload));
        if out.len() > MAX_PKT_SESSION_BYTES {
            bail!("pkt-line 会话数据超过上限 {MAX_PKT_SESSION_BYTES}");
        }
    }
}

/// 从 `upload_pack_request` 的响应里剥掉开头的协商/浅边界控制行，
/// 只留下 packfile。
///
/// SSH 的 v0 fetch 是有状态会话，ACK/NAK（以及 deepen 时的 shallow 段）
/// 已在 `session_upload_pack_v0` 的协商轮发给客户端；`upload_pack_request`
/// 却是按无状态 HTTP RPC 设计的，会把这些控制行连同 pack 一起返回。
/// 若在 done 轮原样再发一遍，客户端会多读一行，把 `0008` 当作 PACK 头。
///
/// packfile 以字面量 `PACK` 开头，不是 pkt-line，循环会在那里停下。
fn packfile_only(out: &[u8]) -> &[u8] {
    let mut pos = 0;
    while pos + 4 <= out.len() {
        let Some(length) = std::str::from_utf8(&out[pos..pos + 4])
            .ok()
            .and_then(|text| usize::from_str_radix(text, 16).ok())
        else {
            break;
        };
        if length == 0 {
            pos += 4; // flush-pkt
            continue;
        }
        if length < 4 || pos + length > out.len() {
            break;
        }
        let payload = &out[pos + 4..pos + length];
        let is_control = payload.starts_with(b"ACK ")
            || payload.starts_with(b"NAK")
            || payload.starts_with(b"shallow ")
            || payload.starts_with(b"unshallow ")
            || payload.starts_with(b"ERR ");
        if !is_control {
            break;
        }
        pos += length;
    }
    &out[pos..]
}

/// 把累积的协商状态还原成「协商轮」请求，交给 `upload_pack_request` 的
/// `!done` 分支处理。
///
/// 会话层不再自己维护一份 shallow / ACK 状态机，直接复用 smart HTTP 的
/// 输出，两边的浅边界段、flush、ACK … ready + NAK 顺序就完全一致。
/// `send_shallow` 为 false 时清掉 deepen：客户端只在第一轮期待
/// shallow 段 + flush，后续协商轮不能重复回这一段。
fn negotiation_request(
    acc: &proto::UploadRequest,
    send_shallow: bool,
) -> proto::UploadRequest {
    proto::UploadRequest {
        wants: acc.wants.clone(),
        haves: acc.haves.clone(),
        done: false,
        deepen: if send_shallow { acc.deepen } else { None },
        deepen_relative: acc.deepen_relative,
        shallow: acc.shallow.clone(),
        filter_blob_none: acc.filter_blob_none,
    }
}

/// done 轮该发给客户端的响应。
///
/// 客户端把 deepen 与 done 放在同一轮时（HTTP 的 stateless 请求也是这种
/// 形态），浅边界段还没发过：此时 `upload_pack_request` 的输出本身就是
/// 「shallow 段 + flush + ACK/NAK + PACK」，必须原样发回才与 HTTP 顺序
/// 一致。旧实现无条件调 `packfile_only`，把这 65 字节的 shallow 段剥掉，
/// SSH 浅克隆就收不到边界信息。
///
/// 反之协商轮已经发过浅边界与 ACK/NAK，只发 packfile（PR #33 的修复，
/// 避免 libgit2 把重复的 `0008NAK\n` 当成 PACK 头）。
fn done_response<'a>(out: &'a [u8], preserve_shallow: bool) -> &'a [u8] {
    if preserve_shallow {
        out
    } else {
        packfile_only(out)
    }
}

/// v0 fetch 会话：广告 → 多轮 want/have 协商 → done 后发 pack。
async fn session_upload_pack_v0<S>(
    pool: &PgPool,
    project: &Project,
    stream: &mut S,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let advertisement = crate::git::http::advertise_refs(pool, project, "git-upload-pack").await?;
    stream.write_all(&advertisement).await?;
    stream.flush().await?;

    // 客户端在第一轮把 wants/shallow/deepen 都发出来，之后各轮只有 haves
    // 和最后的 done；累积成一个请求，语义与真实 git-upload-pack 一致。
    let mut acc = proto::UploadRequest::default();
    // 浅边界段只发一次：客户端第一轮读完 shallow 段 + flush 后，才进入
    // ACK/NAK 协商。
    let mut shallow_pending = true;
    loop {
        let chunk = read_pkt_lines(stream, true).await?;
        if chunk.is_empty() {
            bail!("客户端在协商完成前断开");
        }
        let request = proto::parse_upload_request(&chunk)?;
        for oid in request.wants {
            if !acc.wants.contains(&oid) {
                acc.wants.push(oid);
            }
        }
        acc.haves.extend(request.haves);
        if !request.shallow.is_empty() {
            acc.shallow = request.shallow.clone();
        }
        if request.deepen.is_some() {
            acc.deepen = request.deepen;
        }
        acc.deepen_relative |= request.deepen_relative;
        acc.filter_blob_none |= request.filter_blob_none;

        if request.done {
            // 关键：把 done 传下去。upload_pack_request 按 request.done 决定
            // 是回协商响应还是生成 packfile；漏掉这一行会让它一直走 !done
            // 分支，SSH v0 克隆根本收不到 pack（客户端只看到 EOF）。
            acc.done = true;
            let out = crate::git::http::upload_pack_request(pool, project, &acc).await?;
            // deepen 与 done 同轮时浅边界还没发过，原样发回完整响应；
            // 否则只发 packfile（协商轮的控制行已经发过了）。
            let preserve_shallow = shallow_pending && acc.deepen.is_some();
            stream.write_all(done_response(&out, preserve_shallow)).await?;
            stream.flush().await?;
            return Ok(());
        }

        // 协商轮直接复用 upload_pack_request 的 !done 输出，不再在会话层
        // 另写一套 compute_shallow / have_reachable 分支：旧实现首轮发完
        // shallow 段后还无条件补 NAK，客户端会报
        // "expected shallow/unshallow, got NAK"；HTTP 侧同场景返回空段。
        let send_shallow = shallow_pending && acc.deepen.is_some();
        let negotiation = negotiation_request(&acc, send_shallow);
        let out = crate::git::http::upload_pack_request(pool, project, &negotiation).await?;
        if send_shallow {
            shallow_pending = false;
        }
        if out.is_empty() {
            // HTTP 是无状态 RPC，回空就表示这一轮没有共同提交；SSH 是
            // 有状态会话，客户端发完一轮会阻塞等应答，补一个 NAK 作为
            // 轮次分隔。deepen 首轮的 out 含 shallow 段 + flush，不会
            // 走到这里，因此不会触发上面那个 "got NAK" 错误。
            let nak = proto::pkt_str("NAK\n");
            stream.write_all(&nak).await?;
        } else {
            stream.write_all(&out).await?;
        }
        stream.flush().await?;
    }
}

/// v2 fetch 会话：能力广告 → 逐条处理 stateless 请求（ls-refs / fetch）。
async fn session_upload_pack_v2<S>(
    pool: &PgPool,
    project: &Project,
    stream: &mut S,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(&crate::git::http::v2_capabilities()).await?;
    stream.flush().await?;
    loop {
        let chunk = read_pkt_lines(stream, false).await?;
        if chunk.is_empty() {
            return Ok(());
        }
        let response = crate::git::http::upload_pack_v2(pool, project, &chunk).await?;
        let finished = response
            .windows(9)
            .any(|part| part == &b"packfile\n"[..]);
        stream.write_all(&response).await?;
        stream.flush().await?;
        // fetch 的 packfile 段发完就代表这个 exec 会话结束。客户端读完 pack
        // 未必会关闭写端，继续等 read() 会让 exit-status 一直拖到 russh 的
        // 600 秒 inactivity_timeout（表现为 push/fetch 卡住）。ls-refs 等不带
        // packfile 的响应仍允许在同一通道里继续发请求。
        if finished {
            return Ok(());
        }
    }
}

/// 判定 pack 是否收齐时，允许的单次空闲时长与连续空闲轮数。
const PUSH_IDLE_TIMEOUT_SECS: u64 = 3;
const PUSH_MAX_IDLE_ROUNDS: u32 = 10;

/// SSH push 的 pack 总量上限，与 HTTP 侧 GIT_MAX_BODY_BYTES 对齐。
/// 没有它，已认证客户端持续发送就能把接收缓冲撑到无限大。
const MAX_PUSH_BYTES: usize = 256 * 1024 * 1024;

/// 单次 pkt-line 协商段（命令/want/have）的总量上限。
/// 正常协商远小于此；haves 上限 65536 条 × 49 字节也才 ~3 MiB。
const MAX_PKT_SESSION_BYTES: usize = 8 * 1024 * 1024;

/// 检查数据末尾是不是一个完整 packfile 的 SHA-1 trailer。
///
/// Git pack 的最后 20 字节是前面所有字节的 SHA-1。客户端不关闭写端时，
/// 没法靠 EOF 判断 pack 边界，但 trailer 本身是自校验的：一次哈希就能
/// 确认是否已收齐，比逐个解压缩对象便宜得多，也不会误判正在传输中的
/// 数据。`http::receive_pack` 随后会用 `parse_pack` 再完整校验一遍。
fn pack_trailer_valid(data: &[u8]) -> bool {
    let Some(start) = data.windows(4).position(|part| part == &b"PACK"[..]) else {
        return false;
    };
    let pack = &data[start..];
    if pack.len() < 32 || &pack[..4] != b"PACK" {
        return false;
    }
    let split = pack.len() - 20;
    let digest = Sha1::digest(&pack[..split]);
    digest[..] == pack[split..]
}

/// v0 push 会话：广告 → 命令段 →（视情况）pack → report。
async fn session_receive_pack<S>(
    pool: &PgPool,
    project: &Project,
    user: &User,
    stream: &mut S,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let advertisement = crate::git::http::advertise_refs(pool, project, "git-receive-pack").await?;
    stream.write_all(&advertisement).await?;
    stream.flush().await?;

    let commands = read_pkt_lines(stream, false).await?;
    if commands.is_empty() {
        bail!("客户端未发送推送命令");
    }
    let request = proto::parse_receive_request(&commands)?;

    // 纯删除推送：客户端没有 pack 也不会关闭写端，等 EOF 会死锁，
    // 必须立即回 report（与真实 receive-pack 的 delete_only 分支一致）。
    let delete_only = request
        .commands
        .iter()
        .all(|(_, new, _)| new.is_zero());

    let mut body = commands;
    if delete_only {
        if request.capabilities.iter().any(|item| item == "push-options") {
            let options = read_pkt_lines(stream, false).await?;
            body.extend(options);
        }
    } else {
        // 正常 git send-pack 发完 pack 会关闭写端，读到 EOF 即可；但 libgit2
        // 等客户端会保持写端打开，`read_to_end()` 会一直挂到 russh 的 600 秒
        // inactivity_timeout。改成「有数据就一直读，空闲超时后用 packfile
        // 自带的 SHA-1 trailer 判断是否已收齐」，不再依赖对端关闭写端。
        let mut rest = Vec::new();
        let mut buf = [0u8; 8192];
        let mut idle_rounds = 0u32;
        loop {
            match tokio::time::timeout(
                Duration::from_secs(PUSH_IDLE_TIMEOUT_SECS),
                stream.read(&mut buf),
            )
            .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(count)) => {
                    rest.extend_from_slice(&buf[..count]);
                    if rest.len() > MAX_PUSH_BYTES {
                        bail!("推送 pack 超过上限 {MAX_PUSH_BYTES} 字节");
                    }
                    idle_rounds = 0;
                }
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => {
                    if pack_trailer_valid(&rest) {
                        break;
                    }
                    // 大 pack 在客户端本地生成时可能长时间没有数据流出：
                    // 多给几轮机会，只有确实一直没凑齐 trailer 才报错。
                    idle_rounds += 1;
                    if idle_rounds >= PUSH_MAX_IDLE_ROUNDS {
                        bail!("等待推送 pack 数据超时：客户端未关闭写端且 pack 不完整");
                    }
                }
            }
        }
        body.extend(rest);
    }

    let outcome = crate::git::http::receive_pack(pool, project, user, &body).await?;
    stream.write_all(&outcome.response).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_and_flagged_commands() {
        assert_eq!(
            parse_exec("git-upload-pack '/我的项目.git'"),
            Some((Service::Upload, "我的项目".into()))
        );
        assert_eq!(
            parse_exec("git-receive-pack '/a/b.git'"),
            Some((Service::Receive, "a/b".into()))
        );
        assert_eq!(
            parse_exec("git-upload-pack --protocol=version=2 '/x.git'"),
            Some((Service::Upload, "x".into()))
        );
        assert_eq!(
            parse_exec("git-upload-pack bare.git"),
            Some((Service::Upload, "bare".into()))
        );
        assert_eq!(parse_exec("ls -la"), None);
        assert_eq!(parse_exec("git-upload-pack ''"), None);
    }

    #[test]
    fn slug_keeps_inner_paths_intact() {
        // slug 本身允许出现路径分隔符（多级项目名），只剥头尾修饰。
        assert_eq!(
            parse_exec("git-upload-pack '/docs/site.git'"),
            Some((Service::Upload, "docs/site".into()))
        );
    }

    #[test]
    fn packfile_only_strips_final_nak() {
        // libgit2 首次 clone 会把 wants + flush + done 一次发完：协商轮的
        // NAK 已被 wait_while_ack() 消费，done 轮若再带一个 NAK，客户端
        // 就会把 `0008` 当成 PACK 头，报 "Incomplete pack header"。
        let mut response = proto::pkt_str("NAK\n");
        response.extend(b"PACK\x00\x00\x00\x02rest");
        assert_eq!(
            packfile_only(&response),
            &b"PACK\x00\x00\x00\x02rest"[..]
        );
    }

    #[test]
    fn packfile_only_strips_shallow_section() {
        let mut response = proto::pkt_str("shallow 8d523ed47d6dba3062ef9728b49adc41f3d6ac49\n");
        response.extend(proto::flush());
        response.extend(proto::pkt_str("NAK\n"));
        response.extend(b"PACKdata");
        assert_eq!(packfile_only(&response), &b"PACKdata"[..]);
    }

    #[test]
    fn packfile_only_keeps_bare_pack() {
        let response = b"PACK\x00\x00\x00\x02rest".to_vec();
        assert_eq!(packfile_only(&response), response.as_slice());
    }

    #[test]
    fn negotiation_request_sends_shallow_only_once() {
        let mut acc = proto::UploadRequest::default();
        acc.deepen = Some(3);
        acc.deepen_relative = true;
        acc.filter_blob_none = true;
        acc.done = true; // 累积状态里带了 done，协商轮也必须强制 false

        let first = negotiation_request(&acc, true);
        assert_eq!(first.deepen, Some(3));
        assert!(!first.done);
        assert!(first.deepen_relative);
        assert!(first.filter_blob_none);

        // 浅边界段发过之后，后续协商轮不能再带 deepen，
        // 否则 upload_pack_request 会每轮重复回 shallow 段 + flush。
        let next = negotiation_request(&acc, false);
        assert_eq!(next.deepen, None);
        assert!(!next.done);
    }

    #[test]
    fn done_response_keeps_shallow_section_for_same_round_deepen() {
        // 客户端把 deepen 与 done 同轮发出时，浅边界段还没发过：
        // 响应必须保留「shallow 段 + flush + NAK + PACK」的完整顺序。
        let mut response = proto::pkt_str("shallow 8d523ed47d6dba3062ef9728b49adc41f3d6ac49\n");
        response.extend(proto::flush());
        response.extend(proto::pkt_str("NAK\n"));
        response.extend(b"PACKdata");

        assert_eq!(done_response(&response, true), response.as_slice());
        // 协商轮已发过浅边界 / ACK 时仍只发 packfile（PR #33）。
        assert_eq!(done_response(&response, false), &b"PACKdata"[..]);
    }

    #[test]
    fn auth_backoff_is_bounded() {
        assert_eq!(auth_backoff(0), Duration::from_millis(0));
        assert_eq!(auth_backoff(3), Duration::from_millis(300));
        assert_eq!(auth_backoff(10), Duration::from_millis(1_000));
        // 失败次数再多也不无限增长；阈值不再变成「永久拒绝」的开关。
        assert_eq!(auth_backoff(u32::MAX), Duration::from_millis(1_000));
    }

    #[test]
    fn parses_lfs_authenticate_commands() {
        assert_eq!(
            parse_exec("git-lfs-authenticate '/地狱之下.git' download"),
            Some((
                Service::LfsAuth(LfsOperation::Download),
                "地狱之下".into()
            ))
        );
        assert_eq!(
            parse_exec("git-lfs-authenticate \"/docs/a b.git\" upload"),
            Some((
                Service::LfsAuth(LfsOperation::Upload),
                "docs/a b".into()
            ))
        );
        assert_eq!(parse_exec("git-lfs-authenticate '/x.git' push"), None);
        assert_eq!(parse_exec("git-lfs-authenticate '/x.git'"), None);
        assert_eq!(
            parse_exec("git-lfs-authenticate-evil '/x.git' download"),
            None
        );
        assert_eq!(parse_exec("git-upload-pack-evil '/x.git'"), None);
    }

    #[test]
    fn shell_tokens_keep_quoted_spaces() {
        assert_eq!(
            shell_tokens("'/a b.git' download"),
            vec!["/a b.git".to_string(), "download".to_string()]
        );
    }

    /// 一个合法的 v2 packfile：1 个内容为 `hello` 的 blob + SHA-1 trailer。
    const SAMPLE_PACK: &[u8] = &[
        0x50, 0x41, 0x43, 0x4b, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01,
        0x35, 0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c,
        0x02, 0x15, 0x77, 0x20, 0xd4, 0x1b, 0xf3, 0xa7, 0x16, 0xde, 0xc8, 0x04,
        0x1e, 0xcc, 0x7e, 0x0a, 0x74, 0x84, 0xea, 0x53, 0x44, 0xdc,
    ];

    #[test]
    fn pack_trailer_detects_complete_pack() {
        assert!(pack_trailer_valid(SAMPLE_PACK));
        // push-options 段在 pack 之前时也能定位到 pack 起点。
        let mut data = proto::pkt_str("push-option foo\n");
        data.extend(proto::flush());
        data.extend(SAMPLE_PACK);
        assert!(pack_trailer_valid(&data));
    }

    #[test]
    fn pack_trailer_rejects_incomplete_or_trailing_garbage() {
        let truncated = &SAMPLE_PACK[..SAMPLE_PACK.len() - 1];
        assert!(!pack_trailer_valid(truncated));
        let mut extra = SAMPLE_PACK.to_vec();
        extra.extend(b"junk");
        assert!(!pack_trailer_valid(&extra));
        assert!(!pack_trailer_valid(b"no pack here"));
    }
}
