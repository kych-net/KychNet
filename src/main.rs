mod api;
mod auth;
mod build;
mod config;
mod db;
mod models;
mod render;
mod service;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use config::Config;

#[derive(Parser)]
#[command(name = "kychnet", version, about = "KychNet 站点后端与管理 CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 启动 HTTP API 服务
    Serve,
    /// 生成静态站点
    Build {
        #[arg(long, default_value = "dist")]
        out: PathBuf,
    },
    /// 数据库操作
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// 邀请码管理
    Invite {
        #[command(subcommand)]
        command: InviteCommand,
    },
    /// 用户管理
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// 通行密钥管理
    Passkey {
        #[command(subcommand)]
        command: PasskeyCommand,
    },
}

#[derive(Subcommand)]
enum DbCommand {
    Migrate,
}

#[derive(Subcommand)]
enum InviteCommand {
    Create {
        #[arg(long, default_value_t = 1)]
        uses: i32,
        /// 有效期,例如 7d、24h,或 never
        #[arg(long, default_value = "7d")]
        expires: String,
    },
    List,
    Revoke {
        code: String,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    Create {
        username: String,
        #[arg(long)]
        display: Option<String>,
        /// 授予管理员权限
        #[arg(long)]
        admin: bool,
    },
    List,
    Disable {
        username: String,
    },
    Enable {
        username: String,
    },
}

#[derive(Subcommand)]
enum PasskeyCommand {
    /// 清除某用户全部通行密钥
    Reset { username: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    let cfg = Config::from_env()?;

    match cli.command {
        Command::Serve => api::serve(cfg).await,
        Command::Build { out } => {
            let pool = db::connect(&cfg.database_url).await?;
            build::run(&pool, &cfg, &out).await
        }
        Command::Db { command } => {
            let pool = db::connect(&cfg.database_url).await?;
            match command {
                DbCommand::Migrate => db::migrate(&pool).await,
            }
        }
        Command::Invite { command } => {
            let pool = db::connect(&cfg.database_url).await?;
            match command {
                InviteCommand::Create { uses, expires } => {
                    let expires_at = service::parse_expiry(&expires)?;
                    let invite = service::create_invite(&pool, None, uses, expires_at).await?;
                    println!("{}", invite.code);
                    Ok(())
                }
                InviteCommand::List => {
                    for invite in service::list_invites(&pool).await? {
                        println!(
                            "{}\t{}/{}\t过期: {}\t作废: {}",
                            invite.code,
                            invite.used_count,
                            invite.max_uses,
                            invite
                                .expires_at
                                .map(|t| t.to_rfc3339())
                                .unwrap_or_else(|| "never".into()),
                            invite.revoked_at.is_some()
                        );
                    }
                    Ok(())
                }
                InviteCommand::Revoke { code } => service::revoke_invite(&pool, &code).await,
            }
        }
        Command::User { command } => {
            let pool = db::connect(&cfg.database_url).await?;
            match command {
                UserCommand::Create {
                    username,
                    display,
                    admin,
                } => {
                    let display = display.unwrap_or_else(|| username.clone());
                    let user = service::create_user(&pool, &username, &display, admin).await?;
                    let (url, secret) =
                        auth::provision_totp(&pool, user.id, &user.username).await?;
                    println!("{}", serde_json::to_string_pretty(&user)?);
                    println!("TOTP 密钥: {secret}");
                    println!("otpauth URL: {url}");
                    Ok(())
                }
                UserCommand::List => {
                    for user in service::list_users(&pool).await? {
                        println!(
                            "{}\t{}\t{}\t{}",
                            user.id, user.username, user.display_name, user.disabled
                        );
                    }
                    Ok(())
                }
                UserCommand::Disable { username } => {
                    let user = service::find_user_by_username(&pool, &username)
                        .await?
                        .context("用户不存在")?;
                    service::set_user_disabled(&pool, user.id, true).await
                }
                UserCommand::Enable { username } => {
                    let user = service::find_user_by_username(&pool, &username)
                        .await?
                        .context("用户不存在")?;
                    service::set_user_disabled(&pool, user.id, false).await
                }
            }
        }
        Command::Passkey { command } => {
            let pool = db::connect(&cfg.database_url).await?;
            match command {
                PasskeyCommand::Reset { username } => {
                    let user = service::find_user_by_username(&pool, &username)
                        .await?
                        .context("用户不存在")?;
                    service::delete_passkeys(&pool, user.id).await
                }
            }
        }
    }
}
