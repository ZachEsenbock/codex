use crate::dag::Dag;
use crate::error::TaskError;
use crate::interagent::InterAgentParser;
use crate::interagent::RoutedMessage;
use crate::output::AgentStream;
use crate::output::StreamKind;
use crate::schema::AgentSpec;
use crate::schema::TaskFile;
use crate::schema::TaskSpec;
use crate::spool::RoutedSpool;
use crate::subagent::detect_supports_approval_flag;
use crate::subagent::SubAgentController;
use crate::subagent::SubAgentEventCtx;
use crate::subagent::SubAgentOptions;
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;
use std::collections::BTreeMap;
use std::path::PathBuf;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tokio::time::Duration;
use uuid::Uuid;

// Dashboard UI integration
use codex_tui::DashboardAgentStatus;
use codex_tui::MultiAgentCallbacks;
use codex_tui::MultiAgentDashboard;
use codex_tui::MultiAgentUpdate;
use ratatui::style::Stylize as _;
use ratatui::text::Line as RtLine;
use ratatui::text::Span as RtSpan;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Debug)]
pub struct RunnerOptions {
    pub run_id: String,
    pub base_run_dir: PathBuf,
    pub concurrency: usize,
    pub continue_on_error: bool,
    pub retries: u32,
    pub subagent: SubAgentOptions,
    pub dry_run: bool,
    pub default_cwd: Option<std::path::PathBuf>,
    /// When true, suppress regular stdout progress messages and prefer the
    /// interactive dashboard (wired by the caller). Non-UI mode retains the
    /// existing behavior.
    pub ui: bool,
    /// Preferred UI layout hint. Currently accepted values: "tabbed" (default)
    /// and "grid". The runner does not interpret it; it is forwarded to the
    /// dashboard implementation by the caller.
    pub ui_layout: String,
}

impl RunnerOptions {
    pub fn with_defaults_from_taskfile(tf: &TaskFile) -> Self {
        let run_id = Uuid::new_v4().to_string();
        let base_run_dir = default_base_run_dir().join(run_id.clone());
        let concurrency = tf.concurrency.unwrap_or(2);
        Self {
            run_id,
            base_run_dir,
            concurrency,
            continue_on_error: false,
            retries: 0,
            subagent: SubAgentOptions::default(),
            dry_run: false,
            default_cwd: Some(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
            ui: false,
            ui_layout: "tabbed".to_string(),
        }
    }
}

fn default_base_run_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".codex").join("task-runs"))
        .unwrap_or_else(|| PathBuf::from(".codex/task-runs"))
}

pub struct Runner<'a> {
    tf: &'a TaskFile,
    dag: Dag,
    opts: RunnerOptions,
    agents: BTreeMap<String, AgentSpec>,
}

impl<'a> Runner<'a> {
    pub fn new(tf: &'a TaskFile, dag: Dag, opts: RunnerOptions) -> Result<Self, TaskError> {
        let mut agents = BTreeMap::new();
        for a in &tf.agents {
            agents.insert(a.id.clone(), a.clone());
        }
        Ok(Self {
            tf,
            dag,
            opts,
            agents,
        })
    }

    pub async fn execute(&self) -> Result<(), TaskError> {
        if self.opts.dry_run {
            self.print_plan();
            return Ok(());
        }

        tokio::fs::create_dir_all(&self.opts.base_run_dir).await?;

        // Detect whether the installed codex supports the approval flag.
        let supports_approval = detect_supports_approval_flag(&self.opts.subagent.codex_bin).await;
        let mut base_opts = self.opts.clone();
        base_opts.subagent.supports_approval_flag = Some(supports_approval);

        // Shared context for the whole run: live controllers and streams for routing
        let controllers: std::sync::Arc<
            RwLock<BTreeMap<String, std::sync::Arc<Mutex<SubAgentController>>>>,
        > = std::sync::Arc::new(RwLock::new(BTreeMap::new()));
        let streams: std::sync::Arc<RwLock<BTreeMap<String, AgentStream>>> =
            std::sync::Arc::new(RwLock::new(BTreeMap::new()));
        let (route_tx, mut route_rx) = mpsc::unbounded_channel::<RouteEvent>();

        // Routed message spool: queues messages for tasks that haven't started yet.
        let spool = std::sync::Arc::new(RoutedSpool::new(self.opts.base_run_dir.clone()));

        // Optional dashboard across the entire run when --ui is enabled.
        let mut ui_tx_opt: Option<UnboundedSender<MultiAgentUpdate>> = None;
        let mut ui_task_opt: Option<tokio::task::JoinHandle<std::io::Result<()>>> = None;
        if self.opts.ui {
            let agent_ids: Vec<String> = self.tf.tasks.iter().map(|t| t.id.clone()).collect();
            let (ui_tx, ui_rx) = mpsc::unbounded_channel::<MultiAgentUpdate>();

            // Build control callbacks.
            let ctrl_for_pause = controllers.clone();
            let tx_for_pause = ui_tx.clone();
            let on_pause = move |task_id: String| {
                let ctrl_for_pause = ctrl_for_pause.clone();
                let tx_for_pause = tx_for_pause.clone();
                tokio::spawn(async move {
                    if let Some(ctrl) = ctrl_for_pause.read().await.get(&task_id).cloned() {
                        let _ = ctrl.lock().await.pause().await;
                    }
                    let _ = tx_for_pause.send(MultiAgentUpdate::SetStatus {
                        task_id: task_id.clone(),
                        status: DashboardAgentStatus::Paused,
                    });
                });
            };

            let ctrl_for_resume = controllers.clone();
            let tx_for_resume = ui_tx.clone();
            let on_resume = move |task_id: String| {
                let ctrl_for_resume = ctrl_for_resume.clone();
                let tx_for_resume = tx_for_resume.clone();
                tokio::spawn(async move {
                    if let Some(ctrl) = ctrl_for_resume.read().await.get(&task_id).cloned() {
                        let _ = ctrl.lock().await.resume().await;
                    }
                    let _ = tx_for_resume.send(MultiAgentUpdate::SetStatus {
                        task_id: task_id.clone(),
                        status: DashboardAgentStatus::Running,
                    });
                });
            };

            let ctrl_for_inject = controllers.clone();
            let on_inject = move |task_id: String, text: String| {
                let ctrl_for_inject = ctrl_for_inject.clone();
                tokio::spawn(async move {
                    if let Some(ctrl) = ctrl_for_inject.read().await.get(&task_id).cloned() {
                        let mut c = ctrl.lock().await;
                        let _ = c.inject(&text).await;
                    }
                });
            };

            // Abort: kill all active controllers and let runner unwind.
            let ctrl_for_abort = controllers.clone();
            let on_abort = move || {
                let ctrl_for_abort = ctrl_for_abort.clone();
                tokio::spawn(async move {
                    let ctrls = ctrl_for_abort.read().await;
                    for (_id, ctrl_arc) in ctrls.iter() {
                        let mut c = ctrl_arc.lock().await;
                        let _ = c.child_mut().kill().await;
                    }
                });
            };

            let callbacks = MultiAgentCallbacks::default()
                .with_pause(on_pause)
                .with_resume(on_resume)
                .with_inject(on_inject)
                .with_abort(on_abort);

            let ui_layout = self.opts.ui_layout.clone();
            let ui_handle = tokio::spawn(async move {
                let dash = MultiAgentDashboard::new(agent_ids, callbacks)?;
                let _ = ui_layout; // reserved for future grid implementation
                dash.run(ui_rx).await
            });

            ui_tx_opt = Some(ui_tx);
            ui_task_opt = Some(ui_handle);

            // Initialize all agents as Queued in the UI before any tasks start.
            if let Some(tx) = &ui_tx_opt {
                for t in &self.tf.tasks {
                    let _ = tx.send(MultiAgentUpdate::SetStatus {
                        task_id: t.id.clone(),
                        status: DashboardAgentStatus::Queued,
                    });
                }
            }
        }

        // Router task: consume routed messages and deliver to targets for the whole run
        let controllers_for_router = controllers.clone();
        let streams_for_router = streams.clone();
        let ui_for_router = ui_tx_opt.clone();
        let spool_for_router = spool.clone();
        let router_task = tokio::spawn(async move {
            while let Some(ev) = route_rx.recv().await {
                // Record a routed event in the source task's events.ndjson
                let now = OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "".into());
                let routed_evt = serde_json::json!({
                    "ts": now,
                    "task_id": ev.from_task_id,
                    "attempt": ev.from_attempt,
                    "event": "routed",
                    "to": ev.to_task_id,
                    "bytes": ev.body.len(),
                })
                .to_string();
                write_event(&ev.from_run_dir, &routed_evt).await;

                // Hint to UI by pushing a small banner-like line into the streams
                if let Some(src_stream) = streams_for_router.read().await.get(&ev.from_task_id) {
                    let _ = src_stream
                        .push_line(
                            StreamKind::Stdout,
                            &format!("» routed to {} ({} bytes)", ev.to_task_id, ev.body.len()),
                        )
                        .await;
                }
                if let Some(tx) = &ui_for_router {
                    let banner_src = RtLine::from(vec![
                        RtSpan::raw(" Routed to "),
                        ev.to_task_id.clone().cyan(),
                    ]);
                    let _ = tx.send(MultiAgentUpdate::SetBanner {
                        task_id: ev.from_task_id.clone(),
                        banner: Some(banner_src),
                    });
                    let _ = tx.send(MultiAgentUpdate::BumpRouteCount {
                        task_id: ev.from_task_id.clone(),
                    });
                }

                if let Some(tgt_ctrl) = controllers_for_router.read().await.get(&ev.to_task_id) {
                    if let Some(tgt_stream) = streams_for_router.read().await.get(&ev.to_task_id) {
                        let _ = tgt_stream
                            .push_line(
                                StreamKind::Stdout,
                                &format!(
                                    "» routed from {} ({} bytes)",
                                    ev.from_task_id,
                                    ev.body.len()
                                ),
                            )
                            .await;
                    }
                    if let Some(tx) = &ui_for_router {
                        let banner_tgt = RtLine::from(vec![
                            RtSpan::raw(" Routed from "),
                            ev.from_task_id.clone().cyan(),
                        ]);
                        let _ = tx.send(MultiAgentUpdate::SetBanner {
                            task_id: ev.to_task_id.clone(),
                            banner: Some(banner_tgt),
                        });
                        let _ = tx.send(MultiAgentUpdate::BumpRouteCount {
                            task_id: ev.to_task_id.clone(),
                        });
                    }
                    // Best-effort: pause, inject, and resume
                    let mut ctrl = tgt_ctrl.lock().await;
                    let _ = ctrl.pause().await;
                    let _ = ctrl.inject_then_resume(&ev.body).await;
                } else {
                    // Target not started yet: spool for future delivery.
                    let _ = spool_for_router.append(&ev.to_task_id, &ev.body).await;
                }
            }
        });

        let semaphore = std::sync::Arc::new(Semaphore::new(self.opts.concurrency));
        for level in self.dag.levels() {
            if !self.opts.ui {
                println!("starting wave with tasks: {level:?}");
            }
            let mut futs = FuturesUnordered::new();
            let ui_for_tasks = ui_tx_opt.clone();
            for id in level {
                let sem = semaphore.clone();
                let run_dir = self.task_run_dir(&id);
                let task = match self.dag.task_by_id(self.tf, &id) {
                    Some(t) => t.clone(),
                    None => return Err(TaskError::InvalidTaskFile(format!("missing task: {id}"))),
                };
                let agent = match self.agents.get(&task.agent) {
                    Some(a) => a.clone(),
                    None => return Err(TaskError::UnknownAgent(task.agent.clone())),
                };
                let opts = base_opts.clone();
                let controllers = controllers.clone();
                let streams = streams.clone();
                let route_tx = route_tx.clone();
                let spool_for_task = spool.clone();
                let ui_tx_for_task = ui_for_tasks.clone();

                if !self.opts.ui {
                    println!(
                        "launching task: {} (agent: {}) in {}",
                        id,
                        agent.id,
                        run_dir.display()
                    );
                }
                futs.push(tokio::spawn(async move {
                    // acquire a permit for the lifetime of the task
                    let _permit = match sem.acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => return Err(TaskError::Spawn("semaphore closed".into())),
                    };
                    execute_with_retries_routed(
                        run_dir,
                        agent,
                        task,
                        &opts,
                        controllers,
                        streams,
                        route_tx,
                        spool_for_task,
                        ui_tx_for_task,
                    )
                    .await
                }));
            }

            // execute level and gather results
            let mut level_ok = true;
            while let Some(res) = futs.next().await {
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        level_ok = false;
                        eprintln!("task failed: {e:?}");
                    }
                    Err(join_err) => {
                        level_ok = false;
                        eprintln!("task join error: {join_err:?}");
                    }
                }
            }

            if !self.opts.ui {
                println!("completed wave\n");
            }
            if !level_ok && !self.opts.continue_on_error {
                return Err(TaskError::InvalidTaskFile(
                    "one or more tasks failed".to_string(),
                ));
            }
        }

        // Close router and wait for it to finish
        drop(route_tx);
        let _ = router_task.await;

        // Close UI updates channel and wait for dashboard to exit
        if let Some(ui_tx) = ui_tx_opt.take() {
            drop(ui_tx);
        }
        if let Some(ui_task) = ui_task_opt.take() {
            let _ = ui_task.await;
        }

        Ok(())
    }

    fn task_run_dir(&self, task_id: &str) -> PathBuf {
        self.opts.base_run_dir.join(task_id)
    }

    fn print_plan(&self) {
        let s = self.render_plan();
        println!("{s}");
    }

    pub fn render_plan(&self) -> String {
        let mut out = String::new();
        out.push_str("# Run Plan\n");
        out.push_str(&format!("run_id: {}\n", self.opts.run_id));
        out.push_str(&format!("objective: {}\n", self.tf.objective));
        out.push_str(&format!("concurrency: {}\n", self.opts.concurrency));
        out.push_str("\n## DAG levels\n");
        for (i, level) in self.dag.levels().iter().enumerate() {
            out.push_str(&format!("level {i}: {level:?}\n"));
        }
        out.push_str("\n## Tasks\n");
        for t in &self.tf.tasks {
            out.push_str(&format!(
                "- id: {:<12} agent: {:<12} depends_on: {:?}\n",
                t.id, t.agent, t.depends_on
            ));
        }
        out
    }
}

async fn write_event(run_dir: &PathBuf, line: &str) {
    let path = run_dir.join("events.ndjson");
    if let Ok(mut f) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut f, line.as_bytes()).await;
        let _ = tokio::io::AsyncWriteExt::write_all(&mut f, b"\n").await;
    }
}

// Old `execute_with_retries` removed in favor of `execute_with_retries_routed`.

/// Routed message emitted by stdout parsers.
#[derive(Debug, Clone)]
struct RouteEvent {
    from_task_id: String,
    from_run_dir: PathBuf,
    from_attempt: u32,
    to_task_id: String,
    body: String,
}

/// Execute a task with retries using SubAgentController while streaming
/// output, parsing inter-agent routed messages, and delivering them.
async fn execute_with_retries_routed(
    run_dir: PathBuf,
    agent: AgentSpec,
    task: TaskSpec,
    opts: &RunnerOptions,
    controllers: std::sync::Arc<
        RwLock<BTreeMap<String, std::sync::Arc<Mutex<SubAgentController>>>>,
    >,
    streams: std::sync::Arc<RwLock<BTreeMap<String, AgentStream>>>,
    route_tx: mpsc::UnboundedSender<RouteEvent>,
    spool: std::sync::Arc<RoutedSpool>,
    ui_tx: Option<UnboundedSender<MultiAgentUpdate>>,
) -> Result<(), TaskError> {
    let retries = task.retries.unwrap_or(opts.retries);
    let mut attempt = 0u32;
    loop {
        attempt += 1;

        // Resolve per-task working directory
        let cwd = if let Some(ref user_cwd) = task.cwd {
            PathBuf::from(user_cwd)
        } else if let Some(ref dc) = opts.default_cwd {
            dc.clone()
        } else {
            run_dir.join("work")
        };
        if let Err(e) = tokio::fs::create_dir_all(&cwd).await {
            return Err(TaskError::Io(e));
        }

        let log_dir = run_dir.join(format!("attempt-{attempt}"));
        if let Err(e) = tokio::fs::create_dir_all(&log_dir).await {
            return Err(TaskError::Io(e));
        }

        // write start event
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "".into());
        let start_evt = serde_json::json!({
            "ts": now,
            "task_id": task.id,
            "attempt": attempt,
            "event": "start",
            "cwd": cwd.display().to_string(),
        })
        .to_string();
        write_event(&run_dir, &start_evt).await;

        // Create or replace AgentStream for this attempt
        let stream = AgentStream::new(5000);
        streams
            .write()
            .await
            .insert(task.id.clone(), stream.clone());
        // Transition status to Running before we start streaming
        if let Some(tx) = &ui_tx {
            let _ = tx.send(MultiAgentUpdate::SetStatus {
                task_id: task.id.clone(),
                status: DashboardAgentStatus::Running,
            });
        }
        // Send initial snapshot to UI
        if let Some(tx) = &ui_tx {
            let snap = stream.snapshot().await;
            let mut lines: Vec<RtLine<'static>> = Vec::with_capacity(snap.len());
            for item in snap {
                let l = match item.kind {
                    StreamKind::Stdout => RtLine::from(item.text.clone()),
                    StreamKind::Stderr => RtLine::from(vec![RtSpan::raw(item.text.clone()).red()]),
                };
                lines.push(l);
            }
            let _ = tx.send(MultiAgentUpdate::Snapshot {
                task_id: task.id.clone(),
                log: lines,
                status: DashboardAgentStatus::Running,
                banner: None,
            });
        }

        // Spawn a controller with an event context for control events
        let event_ctx = SubAgentEventCtx {
            run_dir: run_dir.clone(),
            task_id: task.id.clone(),
            attempt,
        };
        let mut controller =
            SubAgentController::spawn(&cwd, &agent, &task, &opts.subagent, Some(event_ctx)).await?;

        // Take child pipes for streaming
        let stdout = match controller.child_mut().stdout.take() {
            Some(s) => s,
            None => return Err(TaskError::Spawn("failed to capture stdout".into())),
        };
        let stderr = match controller.child_mut().stderr.take() {
            Some(s) => s,
            None => return Err(TaskError::Spawn("failed to capture stderr".into())),
        };

        // Insert controller for routing
        let ctrl_arc = std::sync::Arc::new(Mutex::new(controller));
        controllers
            .write()
            .await
            .insert(task.id.clone(), ctrl_arc.clone());

        // open log files for streaming writes
        let out_log_path = log_dir.join("stdout.log");
        let err_log_path = log_dir.join("stderr.log");
        let mut out_file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(out_log_path.clone())
            .await?;
        let mut err_file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(err_log_path.clone())
            .await?;

        // Provide log file paths to the UI so it can page older content from disk
        if let Some(tx) = &ui_tx {
            let _ = tx.send(MultiAgentUpdate::SetLogFiles {
                task_id: task.id.clone(),
                stdout: out_log_path.clone(),
                stderr: err_log_path.clone(),
            });
        }

        // Before we start consuming child output, deliver any spooled routed messages
        // that were sent to this task before it started. Messages are newline-delimited.
        if let Ok(queued) = spool.take_all(&task.id).await {
            if !queued.is_empty() {
                let combined = queued.join("\n");
                let mut ctrl = ctrl_arc.lock().await;
                let _ = ctrl.inject_then_resume(&combined).await;

                // Optional: show a banner in the UI indicating delivery
                if let Some(tx) = &ui_tx {
                    let banner = RtLine::from(vec![RtSpan::raw(format!(
                        " Delivered {} spooled message(s)",
                        queued.len()
                    ))]);
                    let _ = tx.send(MultiAgentUpdate::SetBanner {
                        task_id: task.id.clone(),
                        banner: Some(banner),
                    });
                }
            }
        }

        let show_output = opts.subagent.show_output;
        let stream_out = stream.clone();
        let stream_err = stream.clone();
        let ui_for_out = ui_tx.clone();
        let ui_for_err = ui_tx.clone();

        // Clone per-closure to avoid moving the same String twice into async blocks.
        let from_task_id_out = task.id.clone();
        let from_task_id_err = task.id.clone();
        let from_run_dir = run_dir.clone();
        let route_tx_clone = route_tx.clone();
        // Read stdout lines and parse routed blocks
        let out_task = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut line = Vec::new();
            let mut parser = InterAgentParser::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) => break,
                    Ok(_n) => {
                        let line_str = if line.ends_with(&[b'\n']) {
                            String::from_utf8_lossy(&line[..line.len() - 1]).into_owned()
                        } else {
                            String::from_utf8_lossy(&line).into_owned()
                        };
                        // write to log with newline restored
                        if !line_str.is_empty() || !line.is_empty() {
                            let mut to_write = line_str.as_bytes().to_vec();
                            to_write.push(b'\n');
                            let _ =
                                tokio::io::AsyncWriteExt::write_all(&mut out_file, &to_write).await;
                        }
                        if show_output {
                            let mut to_write = line_str.as_bytes().to_vec();
                            to_write.push(b'\n');
                            let _ = tokio::io::stdout().write_all(&to_write).await;
                        }
                        let _ = stream_out.push_line(StreamKind::Stdout, &line_str).await;

                        // Forward to UI as an append
                        if let Some(tx) = &ui_for_out {
                            let _ = tx.send(MultiAgentUpdate::AppendLines {
                                task_id: from_task_id_out.clone(),
                                lines: vec![RtLine::from(line_str.clone())],
                            });
                        }

                        // Feed parser for routed message detection
                        if let Some(RoutedMessage { to, body }) = parser.feed_line(&line_str) {
                            let _ = route_tx_clone.send(RouteEvent {
                                from_task_id: from_task_id_out.clone(),
                                from_run_dir: from_run_dir.clone(),
                                from_attempt: attempt,
                                to_task_id: to,
                                body,
                            });
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Read stderr lines
        let err_task = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stderr);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) => break,
                    Ok(_n) => {
                        let line_str = if line.ends_with(&[b'\n']) {
                            String::from_utf8_lossy(&line[..line.len() - 1]).into_owned()
                        } else {
                            String::from_utf8_lossy(&line).into_owned()
                        };
                        if !line_str.is_empty() || !line.is_empty() {
                            let mut to_write = line_str.as_bytes().to_vec();
                            to_write.push(b'\n');
                            let _ =
                                tokio::io::AsyncWriteExt::write_all(&mut err_file, &to_write).await;
                        }
                        if show_output {
                            let mut to_write = line_str.as_bytes().to_vec();
                            to_write.push(b'\n');
                            let _ = tokio::io::stderr().write_all(&to_write).await;
                        }
                        let _ = stream_err.push_line(StreamKind::Stderr, &line_str).await;

                        // Forward to UI as an append (styled red)
                        if let Some(tx) = &ui_for_err {
                            let _ = tx.send(MultiAgentUpdate::AppendLines {
                                task_id: from_task_id_err.clone(),
                                lines: vec![RtLine::from(
                                    vec![RtSpan::raw(line_str.clone()).red()],
                                )],
                            });
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let _ = tokio::join!(out_task, err_task);

        // Wait for child to exit
        let code = {
            let mut ctrl = ctrl_arc.lock().await;
            let status = ctrl
                .child_mut()
                .wait()
                .await
                .map_err(|e| TaskError::Spawn(e.to_string()))?;
            status.code().unwrap_or(-1)
        };

        // Remove controller from routing map
        controllers.write().await.remove(&task.id);

        if code == 0 {
            let now = OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "".into());
            let ok_evt = serde_json::json!({
                "ts": now,
                "task_id": task.id,
                "attempt": attempt,
                "event": "success",
            })
            .to_string();
            write_event(&run_dir, &ok_evt).await;
            if let Some(tx) = &ui_tx {
                let _ = tx.send(MultiAgentUpdate::SetStatus {
                    task_id: task.id.clone(),
                    status: DashboardAgentStatus::Done,
                });
                let _ = tx.send(MultiAgentUpdate::SetBanner {
                    task_id: task.id.clone(),
                    banner: None,
                });
            }
            return Ok(());
        } else {
            let now = OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "".into());
            let err_evt = serde_json::json!({
                "ts": now,
                "task_id": task.id,
                "attempt": attempt,
                "event": "error",
                "code": code,
            })
            .to_string();
            write_event(&run_dir, &err_evt).await;

            if let Some(tx) = &ui_tx {
                let _ = tx.send(MultiAgentUpdate::SetStatus {
                    task_id: task.id.clone(),
                    status: DashboardAgentStatus::Error,
                });
            }

            if attempt > retries {
                return Err(TaskError::SubAgentFailed {
                    code,
                    message: String::new(),
                });
            } else {
                // Backoff between retries
                sleep(Duration::from_millis(500 * attempt as u64)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_tui::MultiAgentUpdate;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_set_logfiles_update_paths() {
        // Create a unique temp run dir
        let base = std::env::temp_dir()
            .join(format!("codex_task_test_{}", Uuid::new_v4().to_string()));
        let run_dir = base.join("attempt-1");
        let _ = tokio::fs::create_dir_all(&run_dir).await;

        // Create stdout/stderr log files like the runner does
        let stdout_path = run_dir.join("stdout.log");
        let stderr_path = run_dir.join("stderr.log");
        let _ = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&stdout_path)
            .await
            .expect("create stdout.log");
        let _ = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&stderr_path)
            .await
            .expect("create stderr.log");

        // Simulate notifying the UI
        let (tx, mut rx) = mpsc::unbounded_channel::<MultiAgentUpdate>();
        let task_id = "t1".to_string();
        tx.send(MultiAgentUpdate::SetLogFiles {
            task_id: task_id.clone(),
            stdout: stdout_path.clone(),
            stderr: stderr_path.clone(),
        })
        .expect("send update");

        // Verify the update content matches the created paths
        match rx.try_recv() {
            Ok(MultiAgentUpdate::SetLogFiles { task_id: id, stdout, stderr }) => {
                assert_eq!(id, task_id);
                assert_eq!(stdout, stdout_path);
                assert_eq!(stderr, stderr_path);
            }
            other => panic!("unexpected update: {other:?}"),
        }
    }
}
