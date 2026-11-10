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
//!   `UploadRequest` 再交给 `http::upload_pack_request()`。
//! - v0 push 读命令段后分两路：非删除推送会跟着 pack 且客户端发完会关闭
//!   写端（git send-pack 非 stateless 模式下由 pack 生成器收尾关闭，
//!   见 send-pack.c 对 `fd[1]` 的交接），所以可以读到 EOF 再整体解析；
//!   纯删除推送没有 pack 也不关闭写端，必须立即回 report，不能等 EOF。
//! - v2 fetch 本身就是无状态请求，逐条转发给 `upload_pack_v2` 即可。

use anyhow::{bail, Context, Result};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use sqlx::PgPool;
use std::path::PathBuf;
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

    let mut server = SshServer { pool };
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
}

impl russh::server::Server for SshServer {
    type Handler = GitHandler;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
        GitHandler {
            pool: self.pool.clone(),
            channel: None,
            protocol_v2: false,
            user: None,
        }
    }
}

/// 单个 SSH 连接的处理器。
///
/// russh 会在签名验证通过后回调 `auth_publickey`，此时才能把用户身份
/// 存进 handler；`exec_request` 里据此决定放行还是拒绝。
struct GitHandler {
    pool: PgPool,
    /// `channel_open_session` 存下来的通道；exec 时取走交给 git 会话任务。
    channel: Option<Channel<Msg>>,
    /// 客户端通过 `GIT_PROTOCOL=version=2` env 请求协商出的协议版本。
    protocol_v2: bool,
    user: Option<User>,
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
        match ssh_key::authenticate(&self.pool, key.public_key).await {
            Ok(Some(user)) => {
                tracing::info!(username = %user.username, "SSH 公钥认证通过");
                self.user = Some(user);
                Ok(Auth::Accept)
            }
            Ok(None) => Ok(Auth::reject()),
            Err(error) => {
                tracing::warn!(%error, "SSH 公钥认证查询失败");
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
        reply.accept().await;
        self.channel = Some(channel);
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
            self.protocol_v2 = true;
            session.channel_success(channel)?;
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
                session.channel_failure(channel)?;
                return Ok(());
            }
        };
        let Some((service, slug)) = parse_exec(command) else {
            session.data(
                channel,
                error_pkt(&format!(
                    "kych-net git: 只支持 git-upload-pack / git-receive-pack，收到: {command}"
                )),
            )?;
            session.close(channel)?;
            return Ok(());
        };
        let Some(user) = self.user.clone() else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let Some(channel_obj) = self.channel.take() else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        if channel_obj.id() != channel {
            session.channel_failure(channel)?;
            return Ok(());
        }

        // 项目解析与可见性检查在 exec 回调里完成，失败直接拒绝命令。
        let project = match resolve_project(&self.pool, &slug).await {
            Ok(project) => project,
            Err(error) => {
                tracing::info!(slug = %slug, %error, "SSH git 请求被拒绝");
                session.data(channel, error_pkt(&error.to_string()))?;
                session.close(channel)?;
                return Ok(());
            }
        };

        session.channel_success(channel)?;
        let pool = self.pool.clone();
        let protocol_v2 = self.protocol_v2;
        // git 客户端在读完 report 后还会检查 ssh 退出码：没有 exit-status
        // 请求时 ssh 以 -1 退出，推送结果会被误报成失败，所以必须显式回状态。
        let handle = session.handle();
        tokio::spawn(async move {
            let channel_id = channel_obj.id();
            let result =
                run_service(pool, project, user, service, protocol_v2, channel_obj).await;
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
}

/// 解析 exec 命令，支持可选的 `--protocol=version=2` 与引号包裹的路径。
fn parse_exec(command: &str) -> Option<(Service, String)> {
    let command = command.trim();
    let (service, rest) = if let Some(rest) = command.strip_prefix("git-upload-pack") {
        (Service::Upload, rest)
    } else if let Some(rest) = command.strip_prefix("git-receive-pack") {
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
            value[1..value.len() - 1].to_string()
        }
        value => value.trim().to_string(),
    };

    // 接受 `/slug.git`、`slug.git`、`~/slug.git` 等形式，统一成 slug。
    let slug = path.trim_start_matches(['/', '~']).trim_end_matches('/');
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    if slug.is_empty() {
        return None;
    }
    Some((service, slug.to_string()))
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
    channel: Channel<Msg>,
) -> Result<()> {
    let mut stream = channel.into_stream();
    let result = match (service, protocol_v2) {
        (Service::Upload, true) => session_upload_pack_v2(&pool, &project, &mut stream).await,
        (Service::Upload, false) => session_upload_pack_v0(&pool, &project, &mut stream).await,
        (Service::Receive, _) => session_receive_pack(&pool, &project, &user, &mut stream).await,
    };
    if let Err(error) = &result {
        // 协议层错误尽量以 pkt-line ERR 送达，客户端能看到中文原因。
        let _ = stream.write_all(&error_pkt(&format!("{error}"))).await;
    }
    // shutdown 会给客户端发 EOF；ChannelStream 被 drop 时自动 close 通道。
    let _ = stream.shutdown().await;
    result
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
    let mut shallow_sent = false;
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
            let out = crate::git::http::upload_pack_request(pool, project, &acc).await?;
            stream.write_all(&out).await?;
            stream.flush().await?;
            return Ok(());
        }

        // 带 deepen 的第一轮必须先回浅边界段（以 flush 收尾），
        // 客户端读完这段才进入 ACK/NAK 协商。
        if !shallow_sent && acc.deepen.is_some() {
            let mut conn = pool.acquire().await?;
            let (boundaries, unshallow, _) =
                crate::git::http::compute_shallow(&mut conn, project.id, &acc.wants, &acc).await?;
            let mut out = crate::git::http::shallow_response(&boundaries, &unshallow);
            out.extend(proto::flush());
            stream.write_all(&out).await?;
            shallow_sent = true;
        }

        // 协商轮：只回 ACK/NAK，绝不提前发 pack。
        let ack = if acc.haves.is_empty() {
            proto::pkt_str("NAK\n")
        } else {
            let mut conn = pool.acquire().await?;
            match crate::git::http::have_reachable(&mut conn, project.id, &acc.haves, &acc.shallow)
                .await?
            {
                Some((common, _)) => match common.last() {
                    Some(oid) => proto::pkt_str(&format!("ACK {}\n", oid.to_hex())),
                    None => proto::pkt_str("NAK\n"),
                },
                None => proto::pkt_str("NAK\n"),
            }
        };
        stream.write_all(&ack).await?;
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
        stream.write_all(&response).await?;
        stream.flush().await?;
    }
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
        // 非 stateless 模式下客户端发完 pack 会关闭写端（send-pack.c 把
        // 写端交给 pack 生成器收尾），所以可以安全读到 EOF。
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await?;
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
}
