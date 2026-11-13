//! 从 `api.rs` 拆出的子模块。路由注册与公共类型见 `super`（`api/mod.rs`）。
use super::*;

#[derive(Clone, Serialize)]
pub(crate) struct BuildRecord {
    started_at: i64,
    finished_at: Option<i64>,
    /// None = 仍在运行；Some = 退出码。
    exit_code: Option<i32>,
    /// 输出尾部（stdout + stderr 合并截断），失败时用来定位原因。
    tail: String,
}

#[derive(Serialize)]
pub(crate) struct BuildStatus {
    running: bool,
    last: Option<BuildRecord>,
}

/// 管理端触发的整站重建。
///
/// serve 进程不直接调 `build::run`，而是用「当前二进制 + build 子命令」拉起
/// 独立进程：Typst 及其内嵌字体是重依赖，进程隔离让渲染崩溃、内存尖峰只损失
/// 一次构建，不会波及常驻的 API 服务。可执行文件取 `current_exe()`，
/// 与 serve 是同一个二进制，不存在路径配置问题。
pub(crate) struct BuildCtl {
    /// 同一时刻只允许一个构建。用原子量而不是锁，避免排队语义——
    /// 并发触发直接 409，让前端提示「已在构建」。
    running: AtomicBool,
    last: tokio::sync::Mutex<Option<BuildRecord>>,
    out_dir: PathBuf,
    exe: PathBuf,
}

/// 输出尾部保留长度，超过即从头部截断。
pub(crate) const BUILD_TAIL_BYTES: usize = 4096;

impl BuildCtl {
    pub(crate) fn new(out_dir: PathBuf) -> Self {
        Self {
            running: AtomicBool::new(false),
            last: tokio::sync::Mutex::new(None),
            out_dir,
            // 兜底名与二进制名一致：拿不到自身路径时，重建子进程仍能起得来。
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kych-net")),
        }
    }

    /// 产物目录路径，资源监控用来读磁盘占用。
    pub(crate) fn out_dir(&self) -> &PathBuf {
        &self.out_dir
    }

    /// 管理端保存内容后自动编译这一页。
    ///
    /// 仍走独立子进程，避免 Typst 渲染污染常驻进程；`old` 是改路径前的旧 slug，
    /// 子进程会顺手删掉旧产物。返回失败时内容已经落库，前端会提示重新整站构建。
    pub(crate) async fn build_page(&self, slug: &str, old: Option<&str>) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            bail!("已有构建在进行，请稍后重试或执行整站重建");
        }

        let mut command = tokio::process::Command::new(&self.exe);
        command
            .arg("build")
            .arg("--out")
            .arg(&self.out_dir)
            .arg("--only")
            .arg(slug)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(old) = old {
            if old != slug {
                command.arg("--remove").arg(old);
            }
        }

        let output = command.output().await;
        let (exit_code, tail) = match output {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                let cut = text.len().saturating_sub(BUILD_TAIL_BYTES);
                let tail = if cut > 0 {
                    let start = (cut..=text.len())
                        .find(|&i| text.is_char_boundary(i))
                        .unwrap_or(text.len());
                    format!("…{}", &text[start..])
                } else {
                    text
                };
                (Some(out.status.code().unwrap_or(-1)), tail)
            }
            Err(error) => (None, format!("编译进程启动失败: {error}")),
        };

        {
            let mut last = self.last.lock().await;
            *last = Some(BuildRecord {
                started_at: Utc::now().timestamp(),
                finished_at: Some(Utc::now().timestamp()),
                exit_code,
                tail: tail.clone(),
            });
        }
        self.running.store(false, Ordering::SeqCst);

        if exit_code != Some(0) {
            bail!("{}", if tail.is_empty() { "自动编译失败".to_string() } else { tail });
        }
        Ok(())
    }

    /// 项目文件可能影响多个页面（目录树、项目索引），保存后直接整站重建。
    pub(crate) async fn build_all(&self) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            bail!("已有构建在进行，请稍后重试或执行整站重建");
        }

        let output = tokio::process::Command::new(&self.exe)
            .arg("build")
            .arg("--out")
            .arg(&self.out_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        let (exit_code, tail) = match output {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                let cut = text.len().saturating_sub(BUILD_TAIL_BYTES);
                let tail = if cut > 0 {
                    let start = (cut..=text.len())
                        .find(|&i| text.is_char_boundary(i))
                        .unwrap_or(text.len());
                    format!("…{}", &text[start..])
                } else {
                    text
                };
                (Some(out.status.code().unwrap_or(-1)), tail)
            }
            Err(error) => (None, format!("编译进程启动失败: {error}")),
        };

        {
            let mut last = self.last.lock().await;
            *last = Some(BuildRecord {
                started_at: Utc::now().timestamp(),
                finished_at: Some(Utc::now().timestamp()),
                exit_code,
                tail: tail.clone(),
            });
        }
        self.running.store(false, Ordering::SeqCst);

        if exit_code != Some(0) {
            bail!("{}", if tail.is_empty() { "自动编译失败".to_string() } else { tail });
        }
        Ok(())
    }
}

/// 触发一次整站重建。立即返回 200 并异步执行，前端轮询 GET 拿进度与结果。
pub(crate) async fn rebuild_site(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<BuildStatus>> {
    current_admin(&state, &session).await?;
    require_step_up(&session).await?;
    let ctl = &state.build_ctl;
    if ctl.running.swap(true, Ordering::SeqCst) {
        return Err(ApiError::with_status(
            StatusCode::CONFLICT,
            "已有一次构建在进行，请等待其结束",
        ));
    }
    let record = BuildRecord {
        started_at: Utc::now().timestamp(),
        finished_at: None,
        exit_code: None,
        tail: String::new(),
    };
    *ctl.last.lock().await = Some(record);

    let ctl = Arc::clone(&state.build_ctl);
    tokio::spawn(async move {
        let output = tokio::process::Command::new(ctl.exe.clone())
            .arg("build")
            .arg("--out")
            .arg(&ctl.out_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        let (exit_code, tail) = match output {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                let cut = text.len().saturating_sub(BUILD_TAIL_BYTES);
                let tail = if cut > 0 {
                    // 按字符边界截，避免中文被腰斩成乱码。
                    let start = (cut..=text.len())
                        .find(|&i| text.is_char_boundary(i))
                        .unwrap_or(text.len());
                    format!("…{}", &text[start..])
                } else {
                    text
                };
                (Some(out.status.code().unwrap_or(-1)), tail)
            }
            Err(e) => (None, format!("构建进程启动失败: {e}")),
        };
        let mut last = ctl.last.lock().await;
        if let Some(rec) = last.as_mut() {
            rec.finished_at = Some(Utc::now().timestamp());
            rec.exit_code = exit_code;
            rec.tail = tail;
        }
        drop(last);
        ctl.running.store(false, Ordering::SeqCst);
    });

    let last = state.build_ctl.last.lock().await.clone();
    Ok(Json(BuildStatus {
        running: true,
        last,
    }))
}

/// 查询最近一次构建的状态。
pub(crate) async fn build_status(
    State(state): State<AppState>,
    session: Session,
) -> ApiResult<Json<BuildStatus>> {
    // WHY: 构建日志尾部可能含路径、报错等敏感信息，旧实现无鉴权，匿名即可读；
    // 与 rebuild_site 保持同一套管理员 + step-up 校验。
    current_admin(&state, &session).await?;
    require_step_up(&session).await?;
    Ok(Json(BuildStatus {
        running: state.build_ctl.running.load(Ordering::SeqCst),
        last: state.build_ctl.last.lock().await.clone(),
    }))
}
