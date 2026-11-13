mod api;
mod auth;
mod build;
mod cli_cn;
mod collab;
mod config;
mod db;
mod git;
mod history;
mod mcp;
mod models;
mod oauth;
mod project;
mod rate;
mod render;
mod render_cache;
mod service;
mod svg;
mod workflow;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use config::Config;

/// 站点后端与管理命令行的唯一入口。
///
/// 中文命令先经 `cli_cn` 翻译成英文参数，再交给同一个 clap 解析器，
/// 因此中英文共用同一条执行路径与同一个 service 层。
#[derive(Parser)]
#[command(name = "kych-net", version, about = "站点后端与管理 CLI")]
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
        /// 只编译这一页（输出路径，例如 xef/index.html）
        #[arg(long)]
        only: Option<String>,
        /// 同时删除这个旧路径的产物（改路径时使用）
        #[arg(long)]
        remove: Option<String>,
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
    /// 清理项目仓库中的不可达对象与孤立 LFS 文件
    Gc {
        /// 项目名（slug）
        #[arg(long)]
        project: String,
    },
}

#[derive(Subcommand)]
enum DbCommand {
    Migrate,
    /// 把历史遗留的未压缩 Git 对象压成 zlib（可重复执行）
    Compress,
    /// 把项目对象导出成磁盘全量 pack（需 GIT_OBJECT_BACKEND=disk）
    Offload {
        /// 项目名（slug）
        #[arg(long)]
        project: String,
        /// 删掉冷行，让 PG 只留默认分支 tip 快照作热缓存
        #[arg(long)]
        drop_cold: bool,
    },
    /// 把项目的磁盘 pack 合并成一个，重做全局 delta
    Repack {
        /// 项目名（slug）
        #[arg(long)]
        project: String,
        /// 只保留 refs 可达的对象
        #[arg(long)]
        prune: bool,
    },
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

/// 把原始命令行参数翻译成 clap 能吃的参数列表。
///
/// 纯英文调用原样透传（保留 clap 自己的 `--help` / `--version` 行为）；
/// 带中文的调用交给 `cli_cn` 翻译。返回值第一个元素是程序名，clap 靠它拼 usage。
fn translate_args(raw: &[String]) -> Result<cli_cn::Dispatch, String> {
    // 彩色输出在任何分支之前定好，避免英文路径下帮助文本没有颜色。
    cli_cn::setup_color(raw);

    // clap 的 parse_from 把第一个元素当程序名，所以参数表必须以它开头。
    let mut argv = Vec::with_capacity(raw.len() + 1);
    argv.push("kych-net".to_string());

    // 没有任何参数：打印中英合并的完整帮助。
    if raw.is_empty() {
        return Ok(cli_cn::Dispatch::FullHelp);
    }

    // 英文命令即使带中文参数（用户名、输出目录等）也必须原样交给 clap，
    // 不能被 has_cjk 误判成中文命令。
    if cli_cn::is_english_command(&raw[0]) {
        argv.extend(raw.iter().cloned());
        return Ok(cli_cn::Dispatch::Argv(argv));
    }

    // 纯英文调用原样透传，保留 clap 自己的子命令帮助与报错行为。
    // 顶层 `--help` / `-h` 走合并帮助，子命令级帮助仍交给 clap。
    if !cli_cn::has_cjk(raw) {
        let is_top_help = raw.len() == 1 && matches!(raw[0].as_str(), "--help" | "-h");
        if is_top_help {
            return Ok(cli_cn::Dispatch::FullHelp);
        }
        argv.extend(raw.iter().cloned());
        return Ok(cli_cn::Dispatch::Argv(argv));
    }

    match cli_cn::dispatch(raw)? {
        // 翻译结果不含程序名，补上再交给 clap。
        cli_cn::Dispatch::Argv(translated) => {
            argv.extend(translated);
            Ok(cli_cn::Dispatch::Argv(argv))
        }
        other => Ok(other),
    }
}

/// 完整帮助：clap 的英文子命令列表 + 中文命令一览。
///
/// 英文部分直接取自 clap 自己的命令树，中文部分由 `cli_cn` 提供，
/// 两边共用同一份子命令定义，不会各说各话。
fn print_full_help() {
    let mut cmd = Cli::command();
    cmd = cmd.after_long_help(cli_cn::help_text());
    print!("{}", cmd.render_long_help());
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // 中文命令连写、不加空格，先翻译成英文参数再交给 clap。
    // 纯英文调用原样透传，行为与改动前一致。
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let cli = match translate_args(&raw) {
        Ok(cli_cn::Dispatch::Argv(argv)) => Cli::parse_from(argv),
        Ok(cli_cn::Dispatch::Print(text)) => {
            println!("{text}");
            return Ok(());
        }
        Ok(cli_cn::Dispatch::FullHelp) => {
            print_full_help();
            return Ok(());
        }
        Err(msg) => {
            eprintln!("{}", cli_cn::error_text(&msg));
            std::process::exit(1);
        }
    };

    let cfg = Config::from_env()?;
    // 磁盘对象存储是显式开关：不设置时全局 OnceLock 为空，所有读写路径
    // 都与升级前完全一致，线上可以随时回退到纯 PG。
    if cfg.git_object_backend.eq_ignore_ascii_case("disk") {
        let store = git::store::init_disk_store(cfg.git_root.clone());
        tracing::info!(
            root = %store.root().display(),
            "已启用磁盘 Git 对象存储（packfile + idx v2）"
        );
    } else if !cfg.git_object_backend.eq_ignore_ascii_case("pg") {
        anyhow::bail!(
            "GIT_OBJECT_BACKEND 只支持 pg 或 disk，当前为 {}",
            cfg.git_object_backend
        );
    }

    match cli.command {
        Command::Serve => api::serve(cfg).await,
        Command::Build { out, only, remove } => {
            let pool = db::connect(&cfg.database_url).await?;
            match only {
                Some(only) => build::run_one(&pool, &cfg, &out, &only, remove.as_deref()).await,
                None => build::run(&pool, &cfg, &out).await,
            }
        }
        Command::Db { command } => {
            let pool = db::connect(&cfg.database_url).await?;
            match command {
                DbCommand::Migrate => db::migrate(&pool).await,
                DbCommand::Compress => {
                    let (done, skipped) =
                        crate::git::store::compress_existing_objects(&pool).await?;
                    println!("已压缩 {done} 个 Git 对象，{skipped} 个压不动，保持原样");
                    if done > 0 {
                        println!(
                            "提示：批量 UPDATE 会留下 TOAST 死元组，磁盘空间需在运维窗口执行 \
                             VACUUM FULL git_objects 后才会回收（该命令会锁表）"
                        );
                    }
                    Ok(())
                }
                DbCommand::Offload { project, drop_cold } => {
                    let disk = git::store::disk_store()
                        .context("需设置 GIT_OBJECT_BACKEND=disk 才能 offload")?;
                    let project = project::find_project_by_slug(&pool, &project)
                        .await?
                        .context("项目不存在")?;
                    let stats = git::disk::offload(&pool, &project, disk, drop_cold).await?;
                    println!(
                        "已导出 {} 个对象到磁盘全量 pack；PG 保留热集 {} 个，删除冷行 {} 个",
                        stats.objects_written, stats.hot_kept, stats.cold_dropped
                    );
                    if !drop_cold {
                        println!(
                            "提示：PG 仍保留全量字节（双写观察期）。确认稳定后加 --drop-cold 才真正切到 S3"
                        );
                    }
                    Ok(())
                }
                DbCommand::Repack { project, prune } => {
                    let disk = git::store::disk_store()
                        .context("需设置 GIT_OBJECT_BACKEND=disk 才能 repack")?;
                    let project = project::find_project_by_slug(&pool, &project)
                        .await?
                        .context("项目不存在")?;
                    // prune 时也保护排队中的工作流快照，与 gc 的口径一致。
                    let keep = if prune {
                        let pending = workflow::dispatch::pending_commits(&pool)
                            .await?
                            .into_iter()
                            .filter_map(|bytes| git::object::Oid::from_slice(&bytes).ok())
                            .collect();
                        Some({
                            let mut conn = pool.acquire().await?;
                            git::gc::mark_reachable(&mut conn, project.id, pending)
                                .await?
                                .0
                        })
                    } else {
                        None
                    };
                    let project_id = project.id;
                    let count =
                        tokio::task::spawn_blocking(move || disk.repack(project_id, keep.as_ref()))
                            .await
                            .context("repack 任务失败")??;
                    println!("已合并为 1 个 pack，保留 {count} 个对象");
                    Ok(())
                }
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
                UserCommand::Create { username, admin } => {
                    let user = service::create_user(&pool, &username, admin).await?;
                    let (url, secret) =
                        auth::provision_totp(&pool, user.id, &user.username).await?;
                    println!("{}", serde_json::to_string_pretty(&user)?);
                    println!("TOTP 密钥: {secret}");
                    println!("otpauth URL: {url}");
                    Ok(())
                }
                UserCommand::List => {
                    for user in service::list_users(&pool).await? {
                        println!("{}\t{}\t{}", user.id, user.username, user.disabled);
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
        Command::Gc { project } => {
            let pool = db::connect(&cfg.database_url).await?;
            let project = project::find_project_by_slug(&pool, &project)
                .await?
                .context("项目不存在")?;
            let stats = git::gc::run(&pool, &cfg.lfs_root, &project).await?;
            println!(
                "已清理项目 {}：对象 {}、提交 {}、路径记录 {}、LFS {}",
                project.slug,
                stats.objects_removed,
                stats.commits_removed,
                stats.paths_removed,
                stats.lfs_removed
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_command_with_chinese_args_passes_through() {
        let raw = vec!["user".to_string(), "create".to_string(), "爱丽丝".to_string()];
        match translate_args(&raw).unwrap() {
            cli_cn::Dispatch::Argv(argv) => {
                assert_eq!(argv, ["kych-net", "user", "create", "爱丽丝"]);
            }
            _ => panic!("英文命令带中文参数应原样透传"),
        }
    }
}
