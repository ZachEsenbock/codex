use crate::error::TaskError;
use crate::output::AgentStream;
use crate::output::StreamKind;
use crate::schema::AgentSpec;
use crate::schema::TaskSpec;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tokio::time::Duration;

#[derive(Clone, Debug)]
pub struct SubAgentOptions {
    pub codex_bin: String,                // default: "codex"
    pub model: Option<String>,            // global override
    pub profile: Option<String>,          // global override
    pub sandbox: Option<String>,          // global override
    pub ask_for_approval: Option<String>, // global override
    pub extra_args: Vec<String>,
    pub time_budget_secs: Option<u64>,        // global fallback
    pub supports_approval_flag: Option<bool>, // detected once per run
    pub show_output: bool,                    // print sub-agent stdout/stderr live
    pub task_md_full_text: Option<String>,    // full task.md provided to sub-agents
}

impl Default for SubAgentOptions {
    fn default() -> Self {
        // Gather any extra args from env, then ensure we always skip the git repo check
        // so sub-agents can operate outside a trusted git workspace by default.
        let mut extra_args: Vec<String> = match std::env::var("CODEX_TASK_EXTRA_ARGS") {
            Ok(val) => val.split_whitespace().map(|v| v.to_string()).collect(),
            Err(_) => Vec::new(),
        };
        extra_args.push("--skip-git-repo-check".into());
        // Default to enabling network access for the workspace-write sandbox
        // used by sub-agents (and the planner). This keeps disk writes scoped
        // while allowing HTTP requests when needed.
        extra_args.push("-c".into());
        extra_args.push("sandbox_workspace_write.network_access=true".into());

        Self {
            codex_bin: std::env::var("CODEX_TASK_SUBAGENT_BIN").unwrap_or_else(|_| "codex".into()),
            model: None,
            profile: None,
            sandbox: Some("workspace-write".into()),
            ask_for_approval: Some("never".into()),
            extra_args,
            time_budget_secs: None,
            supports_approval_flag: None,
            show_output: std::env::var("CODEX_TASK_SHOW_OUTPUT")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            task_md_full_text: None,
        }
    }
}

pub struct SubAgentResult {
    pub status_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Platform-agnostic control wrapper for a running sub-agent process.
/// Provides best-effort pause/resume via signals and stdin injection.
pub struct SubAgentController {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    /// On Windows we create a new process group; we keep the PID to target CTRL events.
    pid: u32,
    event: Option<SubAgentEventCtx>,
    // Fields to support restart-with-guidance
    workdir: std::path::PathBuf,
    agent: AgentSpec,
    task: TaskSpec,
    opts: SubAgentOptions,
    appended_guidance: Vec<String>,
}

impl SubAgentController {
    /// Spawn a controllable sub-agent with piped stdin/stdout/stderr.
    /// This does not wait for completion; callers can read child output via `child.stdout/stderr`.
    pub async fn spawn(
        workdir: &Path,
        agent: &AgentSpec,
        task: &TaskSpec,
        opts: &SubAgentOptions,
        event: Option<SubAgentEventCtx>,
    ) -> Result<Self, TaskError> {
        let envs = merge_envs(agent, task);

        // global overrides win over agent/task
        let model = opts.model.clone().or_else(|| agent.model.clone());
        let profile = opts.profile.clone().or_else(|| agent.profile.clone());
        let sandbox = opts.sandbox.clone().or_else(|| agent.sandbox.clone());
        let ask_for_approval = opts
            .ask_for_approval
            .clone()
            .or_else(|| agent.ask_for_approval.clone());

        let mut cmd = Command::new(&opts.codex_bin);
        cmd.arg("exec").arg("--full-auto");
        if let Some(m) = model {
            cmd.arg("--model").arg(m);
        }
        if let Some(p) = profile {
            cmd.arg("--profile").arg(p);
        }
        if let Some(s) = sandbox {
            cmd.arg("--sandbox").arg(s);
        }
        if let Some(a) = ask_for_approval {
            if opts.supports_approval_flag.unwrap_or(false) {
                cmd.arg("--ask-for-approval").arg(a);
            }
        }
        if !opts.extra_args.is_empty() {
            cmd.args(opts.extra_args.clone());
        }

        // Build final instruction: role primer + task.md context + specific instructions
        let primer = agent_primer(&agent.id);
        let final_instructions = if let Some(md) = opts.task_md_full_text.as_ref() {
            let task_instructions = &task.instructions;
            format!(
                "{primer}\n\nProject context (task.md):\n```md\n{md}\n```\n\nYour task:\n{task_instructions}"
            )
        } else {
            let task_instructions = &task.instructions;
            format!("{primer}\n\nYour task:\n{task_instructions}")
        };
        cmd.arg(&final_instructions);

        cmd.current_dir(workdir);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        for (k, v) in envs.iter() {
            cmd.env(k, v);
        }

        // On Windows, create a new process group so we can send CTRL events.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NEW_PROCESS_GROUP = 0x00000200
            cmd.creation_flags(0x0000_0200);
        }

        let mut child = cmd.spawn().map_err(|e| TaskError::Spawn(e.to_string()))?;
        let stdin = child.stdin.take();
        let pid = child.id().unwrap_or(0);
        Ok(Self {
            child,
            stdin,
            pid,
            event,
            workdir: workdir.to_path_buf(),
            agent: agent.clone(),
            task: task.clone(),
            opts: opts.clone(),
            appended_guidance: Vec::new(),
        })
    }

    /// Send a pause request: prefer SIGINT/CTRL_BREAK to trigger codex pause.
    pub async fn pause(&self) -> Result<(), TaskError> {
        let r = send_pause_signal(self.pid);
        if r.is_ok() {
            if let Some(ctx) = &self.event {
                write_control_event(ctx, "pause", None).await;
            }
        }
        r
    }

    /// Resume a stopped process if we had to SIGSTOP; otherwise no-op.
    pub async fn resume(&self) -> Result<(), TaskError> {
        let r = send_resume_signal(self.pid);
        if r.is_ok() {
            if let Some(ctx) = &self.event {
                write_control_event(ctx, "resume", None).await;
            }
        }
        r
    }

    /// Inject guidance into the child's stdin. Appends a newline.
    pub async fn inject(&mut self, guidance: &str) -> Result<(), TaskError> {
        match self.stdin.as_mut() {
            Some(stdin) => {
                let mut buf = guidance.as_bytes().to_vec();
                buf.push(b'\n');
                let r = tokio::io::AsyncWriteExt::write_all(stdin, &buf)
                    .await
                    .map_err(|e| TaskError::Injection(format!("{e}")));
                if r.is_ok() {
                    if let Some(ctx) = &self.event {
                        let extra = serde_json::json!({ "guidance": guidance });
                        write_control_event(ctx, "inject", Some(extra)).await;
                    }
                }
                r
            }
            None => Err(TaskError::StdinClosed),
        }
    }

    /// Convenience: inject guidance while paused, then best-effort resume (SIGCONT on Unix).
    pub async fn inject_then_resume(&mut self, guidance: &str) -> Result<(), TaskError> {
        self.inject(guidance).await?;
        // Resume even if not stopped; SIGCONT is harmless when already running (Unix).
        let _ = self.resume().await;
        Ok(())
    }

    /// Fallback: terminate and restart the sub-agent with additional guidance appended
    /// to the original instructions. The restart preserves all prior appended guidance
    /// and writes a control event with the appended text.
    pub async fn restart_with_appended_guidance(
        &mut self,
        guidance: &str,
    ) -> Result<(), TaskError> {
        // Kill current child if still running
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;

        self.appended_guidance.push(guidance.to_string());

        let envs = merge_envs(&self.agent, &self.task);

        // Rebuild command
        let mut cmd = Command::new(&self.opts.codex_bin);
        cmd.arg("exec").arg("--full-auto");
        if let Some(m) = self.opts.model.clone().or_else(|| self.agent.model.clone()) {
            cmd.arg("--model").arg(m);
        }
        if let Some(p) = self
            .opts
            .profile
            .clone()
            .or_else(|| self.agent.profile.clone())
        {
            cmd.arg("--profile").arg(p);
        }
        if let Some(s) = self
            .opts
            .sandbox
            .clone()
            .or_else(|| self.agent.sandbox.clone())
        {
            cmd.arg("--sandbox").arg(s);
        }
        if let Some(a) = self
            .opts
            .ask_for_approval
            .clone()
            .or_else(|| self.agent.ask_for_approval.clone())
        {
            if self.opts.supports_approval_flag.unwrap_or(false) {
                cmd.arg("--ask-for-approval").arg(a);
            }
        }
        if !self.opts.extra_args.is_empty() {
            cmd.args(self.opts.extra_args.clone());
        }

        // Build final instruction with appended guidance
        let primer = agent_primer(&self.agent.id);
        let base = if let Some(md) = self.opts.task_md_full_text.as_ref() {
            let task_instructions = &self.task.instructions;
            format!(
                "{primer}\n\nProject context (task.md):\n```md\n{md}\n```\n\nYour task:\n{task_instructions}"
            )
        } else {
            let task_instructions = &self.task.instructions;
            format!("{primer}\n\nYour task:\n{task_instructions}")
        };
        let final_instructions = if self.appended_guidance.is_empty() {
            base
        } else {
            let extra = self.appended_guidance.join("\n\n");
            format!("{base}\n\nAdditional guidance (prior attempts preserved):\n{extra}")
        };
        cmd.arg(&final_instructions);

        cmd.current_dir(&self.workdir);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        for (k, v) in envs.iter() {
            cmd.env(k, v);
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0000_0200);
        }

        let mut child = cmd.spawn().map_err(|e| TaskError::Spawn(e.to_string()))?;
        self.stdin = child.stdin.take();
        self.pid = child.id().unwrap_or(0);
        self.child = child;

        if let Some(ctx) = &self.event {
            let extra = serde_json::json!({ "guidance": guidance });
            write_control_event(ctx, "restart", Some(extra)).await;
        }
        Ok(())
    }

    pub fn child_mut(&mut self) -> &mut tokio::process::Child {
        &mut self.child
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

#[derive(Clone, Debug)]
pub struct SubAgentEventCtx {
    pub run_dir: std::path::PathBuf,
    pub task_id: String,
    pub attempt: u32,
}

async fn write_control_event(
    ctx: &SubAgentEventCtx,
    event: &str,
    extra: Option<serde_json::Value>,
) {
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "".into());
    let mut obj = serde_json::json!({
        "ts": now,
        "task_id": ctx.task_id,
        "attempt": ctx.attempt,
        "event": event,
    });
    if let Some(extra) = extra {
        if let Some(map) = obj.as_object_mut() {
            for (k, v) in extra.as_object().into_iter().flatten() {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    let line = obj.to_string();
    let path = ctx.run_dir.join("events.ndjson");
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

/// Pause a process by PID using best-effort platform behavior.
pub fn pause_process(pid: u32) -> Result<(), TaskError> {
    send_pause_signal(pid)
}

/// Resume a process by PID if previously stopped (Unix SIGCONT). No-op on Windows.
pub fn resume_process(pid: u32) -> Result<(), TaskError> {
    send_resume_signal(pid)
}

#[cfg(unix)]
fn send_pause_signal(pid: u32) -> Result<(), TaskError> {
    use nix::sys::signal::kill;
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;
    let p = Pid::from_raw(pid as i32);
    // Use SIGSTOP to reliably pause the child without risking an exit.
    // This avoids cases where SIGINT is treated as a graceful termination by
    // the sub-agent binary, which would end the task instead of pausing it.
    kill(p, Signal::SIGSTOP).map_err(|e| TaskError::Signal(format!("SIGSTOP failed: {e}")))
}

#[cfg(unix)]
fn send_resume_signal(pid: u32) -> Result<(), TaskError> {
    use nix::sys::signal::kill;
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;
    let p = Pid::from_raw(pid as i32);
    kill(p, Signal::SIGCONT).map_err(|e| TaskError::Signal(format!("SIGCONT failed: {e}")))
}

#[cfg(windows)]
fn send_pause_signal(pid: u32) -> Result<(), TaskError> {
    use windows_sys::Win32::Foundation::BOOL;
    use windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent;
    use windows_sys::Win32::System::Console::CTRL_BREAK_EVENT;
    unsafe {
        let ok: BOOL = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
        if ok == 0 {
            // Fallback to CTRL_C_EVENT if CTRL_BREAK fails
            use windows_sys::Win32::System::Console::CTRL_C_EVENT;
            let ok2: BOOL = GenerateConsoleCtrlEvent(CTRL_C_EVENT, pid);
            if ok2 == 0 {
                return Err(TaskError::Signal("failed to send CTRL event".into()));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn send_resume_signal(_pid: u32) -> Result<(), TaskError> {
    // No direct resume equivalent on Windows for CTRL events.
    Ok(())
}

pub async fn run_subagent(
    workdir: &Path,
    agent: &AgentSpec,
    task: &TaskSpec,
    opts: &SubAgentOptions,
    log_dir: &Path,
) -> Result<SubAgentResult, TaskError> {
    tokio::fs::create_dir_all(log_dir).await?;

    let envs = merge_envs(agent, task);

    // global overrides win over agent/task
    let model = opts.model.clone().or_else(|| agent.model.clone());
    let profile = opts.profile.clone().or_else(|| agent.profile.clone());
    let sandbox = opts.sandbox.clone().or_else(|| agent.sandbox.clone());
    let ask_for_approval = opts
        .ask_for_approval
        .clone()
        .or_else(|| agent.ask_for_approval.clone());

    let mut cmd = Command::new(&opts.codex_bin);

    // non-interactive CI mode
    cmd.arg("exec").arg("--full-auto");

    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    if let Some(p) = profile {
        cmd.arg("--profile").arg(p);
    }
    if let Some(s) = sandbox {
        cmd.arg("--sandbox").arg(s);
    }
    if let Some(a) = ask_for_approval {
        if opts.supports_approval_flag.unwrap_or(false) {
            cmd.arg("--ask-for-approval").arg(a);
        }
    }

    // pass through any extra args
    if !opts.extra_args.is_empty() {
        cmd.args(opts.extra_args.clone());
    }

    // Build final instruction: role primer + task.md context + specific instructions
    let primer = agent_primer(&agent.id);
    let final_instructions = if let Some(md) = opts.task_md_full_text.as_ref() {
        let task_instructions = &task.instructions;
        format!(
            "{primer}\n\nProject context (task.md):\n```md\n{md}\n```\n\nYour task:\n{task_instructions}"
        )
    } else {
        let task_instructions = &task.instructions;
        format!("{primer}\n\nYour task:\n{task_instructions}")
    };

    // final argument: instructions
    cmd.arg(&final_instructions);

    cmd.current_dir(workdir);
    // Pipe stdin to enable guidance injection
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    for (k, v) in envs.iter() {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|e| TaskError::Spawn(e.to_string()))?;

    // capture output with optional timeout
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return Err(TaskError::Spawn("failed to capture stdout".into())),
    };
    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => return Err(TaskError::Spawn("failed to capture stderr".into())),
    };

    let show_output = opts.show_output;

    let join = async move {
        let out_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if show_output {
                            let _ = tokio::io::stdout().write_all(&chunk[..n]).await;
                        }
                    }
                    Err(_) => break,
                }
            }
            buf
        });

        let err_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut reader = tokio::io::BufReader::new(stderr);
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if show_output {
                            let _ = tokio::io::stderr().write_all(&chunk[..n]).await;
                        }
                    }
                    Err(_) => break,
                }
            }
            buf
        });

        let out = out_task.await.unwrap_or_default();
        let err = err_task.await.unwrap_or_default();
        (out, err)
    };

    let (out_buf, err_buf) = if let Some(secs) = task.timeout_secs.or(opts.time_budget_secs) {
        match timeout(Duration::from_secs(secs), join).await {
            Ok(pair) => pair,
            Err(_) => {
                let _ = child.kill().await;
                return Err(TaskError::Timeout(secs));
            }
        }
    } else {
        join.await
    };

    let status = child
        .wait()
        .await
        .map_err(|e| TaskError::Spawn(e.to_string()))?;
    let code = status.code().unwrap_or(-1);

    // write logs
    let mut out_file = tokio::fs::File::create(log_dir.join("stdout.log")).await?;
    let mut err_file = tokio::fs::File::create(log_dir.join("stderr.log")).await?;
    tokio::io::AsyncWriteExt::write_all(&mut out_file, &out_buf).await?;
    tokio::io::AsyncWriteExt::write_all(&mut err_file, &err_buf).await?;

    let res = SubAgentResult {
        status_code: code,
        stdout: String::from_utf8_lossy(&out_buf).into_owned(),
        stderr: String::from_utf8_lossy(&err_buf).into_owned(),
    };

    if code != 0 {
        return Err(TaskError::SubAgentFailed {
            code,
            message: res.stderr.clone(),
        });
    }

    Ok(res)
}

/// Run a subagent while streaming stdout/stderr into an optional in-memory ring buffer
/// for consumption by a TUI, and simultaneously write to per-attempt log files.
/// Returns the full captured stdout/stderr as strings for callers that need them.
pub async fn run_subagent_streaming(
    workdir: &Path,
    agent: &AgentSpec,
    task: &TaskSpec,
    opts: &SubAgentOptions,
    log_dir: &Path,
    stream: Option<AgentStream>,
) -> Result<SubAgentResult, TaskError> {
    tokio::fs::create_dir_all(log_dir).await?;

    let envs = merge_envs(agent, task);

    // global overrides win over agent/task
    let model = opts.model.clone().or_else(|| agent.model.clone());
    let profile = opts.profile.clone().or_else(|| agent.profile.clone());
    let sandbox = opts.sandbox.clone().or_else(|| agent.sandbox.clone());
    let ask_for_approval = opts
        .ask_for_approval
        .clone()
        .or_else(|| agent.ask_for_approval.clone());

    let mut cmd = Command::new(&opts.codex_bin);

    // non-interactive CI mode
    cmd.arg("exec").arg("--full-auto");

    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    if let Some(p) = profile {
        cmd.arg("--profile").arg(p);
    }
    if let Some(s) = sandbox {
        cmd.arg("--sandbox").arg(s);
    }
    if let Some(a) = ask_for_approval {
        if opts.supports_approval_flag.unwrap_or(false) {
            cmd.arg("--ask-for-approval").arg(a);
        }
    }

    // pass through any extra args
    if !opts.extra_args.is_empty() {
        cmd.args(opts.extra_args.clone());
    }

    // Build final instruction: role primer + task.md context + specific instructions
    let primer = agent_primer(&agent.id);
    let final_instructions = if let Some(md) = opts.task_md_full_text.as_ref() {
        let task_instructions = &task.instructions;
        format!(
            "{primer}\n\nProject context (task.md):\n```md\n{md}\n```\n\nYour task:\n{task_instructions}"
        )
    } else {
        let task_instructions = &task.instructions;
        format!("{primer}\n\nYour task:\n{task_instructions}")
    };

    // final argument: instructions
    cmd.arg(&final_instructions);

    cmd.current_dir(workdir);
    // Pipe stdin to enable guidance injection during streaming runs
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    for (k, v) in envs.iter() {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|e| TaskError::Spawn(e.to_string()))?;

    // capture pipes
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return Err(TaskError::Spawn("failed to capture stdout".into())),
    };
    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => return Err(TaskError::Spawn("failed to capture stderr".into())),
    };

    // open log files for streaming writes
    let out_log_path = log_dir.join("stdout.log");
    let err_log_path = log_dir.join("stderr.log");
    let mut out_file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(out_log_path)
        .await?;
    let mut err_file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(err_log_path)
        .await?;

    let show_output = opts.show_output;
    let stream_clone_a = stream.clone();
    let stream_clone_b = stream.clone();

    // Read stdout lines
    let out_task = tokio::spawn(async move {
        let mut buf_accum: Vec<u8> = Vec::new();
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_n) => {
                    // remove trailing newline for ring buffer readability
                    let line_str = if line.ends_with(&[b'\n']) {
                        String::from_utf8_lossy(&line[..line.len() - 1]).into_owned()
                    } else {
                        String::from_utf8_lossy(&line).into_owned()
                    };
                    // write to log with newline restored
                    if !line_str.is_empty() || !line.is_empty() {
                        let mut to_write = line_str.as_bytes().to_vec();
                        to_write.push(b'\n');
                        let _ = tokio::io::AsyncWriteExt::write_all(&mut out_file, &to_write).await;
                        buf_accum.extend_from_slice(&to_write);
                    }
                    if show_output {
                        let mut to_write = line_str.as_bytes().to_vec();
                        to_write.push(b'\n');
                        let _ = tokio::io::stdout().write_all(&to_write).await;
                    }
                    if let Some(ref s) = stream_clone_a {
                        let _ = s.push_line(StreamKind::Stdout, &line_str).await;
                    }
                }
                Err(_) => break,
            }
        }
        buf_accum
    });

    // Read stderr lines
    let err_task = tokio::spawn(async move {
        let mut buf_accum: Vec<u8> = Vec::new();
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
                        let _ = tokio::io::AsyncWriteExt::write_all(&mut err_file, &to_write).await;
                        buf_accum.extend_from_slice(&to_write);
                    }
                    if show_output {
                        let mut to_write = line_str.as_bytes().to_vec();
                        to_write.push(b'\n');
                        let _ = tokio::io::stderr().write_all(&to_write).await;
                    }
                    if let Some(ref s) = stream_clone_b {
                        let _ = s.push_line(StreamKind::Stderr, &line_str).await;
                    }
                }
                Err(_) => break,
            }
        }
        buf_accum
    });

    let (out_buf, err_buf) = tokio::join!(out_task, err_task);
    let out_buf = out_buf.unwrap_or_default();
    let err_buf = err_buf.unwrap_or_default();

    let status = child
        .wait()
        .await
        .map_err(|e| TaskError::Spawn(e.to_string()))?;
    let code = status.code().unwrap_or(-1);

    let res = SubAgentResult {
        status_code: code,
        stdout: String::from_utf8_lossy(&out_buf).into_owned(),
        stderr: String::from_utf8_lossy(&err_buf).into_owned(),
    };

    if code != 0 {
        return Err(TaskError::SubAgentFailed {
            code,
            message: res.stderr.clone(),
        });
    }

    Ok(res)
}

/// Merge environment variables with precedence: task.env > agent.env.
pub fn merge_envs(agent: &AgentSpec, task: &TaskSpec) -> BTreeMap<String, String> {
    let mut envs: BTreeMap<String, String> = agent.env.clone();
    for (k, v) in &task.env {
        envs.insert(k.clone(), v.clone());
    }
    envs
}

fn agent_primer(agent_id: &str) -> String {
    use crate::interagent::PROTOCOL_PRIMER;
    let role = match agent_id {
        "backend" => "You are an expert backend engineer. Design, implement, and test server-side logic and APIs. Follow repo conventions, keep changes minimally scoped, and respect boundaries defined in the task. Prefer reliability, tests, and clear docs.",
        "frontend" => "You are an expert frontend engineer. Implement UI with accessibility, responsiveness, and clean state management. Follow the code style and component patterns in this repo. Keep changes scoped to the task.",
        "docs" => "You are a technical writer. Produce clear, concise documentation with accurate examples. Maintain consistent tone and structure. Update any relevant README or docs sections coherently.",
        "qa" => "You are a quality engineer. Design and implement tests that validate behavior and guard against regressions. Prefer fast, deterministic tests and meaningful assertions.",
        "research" => "You are a research engineer. Investigate, compare options, and summarize trade-offs concisely. Provide actionable recommendations and next steps grounded in evidence.",
        "devops" => "You are a DevOps/SRE engineer. Improve build, CI/CD, and runtime reliability with minimal, safe changes. Favor observability, repeatability, and principle of least privilege.",
        "tooling" => "You are a tooling engineer. Enhance developer experience with safe, incremental changes to build scripts, linters, and CLIs. Keep the toolchain simple and well-documented.",
        "primary" => "You are a lead full-stack engineer. Break down work, make pragmatic choices, and deliver incremental value. Keep changes scoped and well-tested.",
        _ => "You are an expert software engineer. Work pragmatically, keep changes scoped, and adhere to repo conventions and tests.",
    };
    format!("{role}\n\n{PROTOCOL_PRIMER}")
}

/// Detect if the installed codex binary supports the `--ask-for-approval` flag.
pub async fn detect_supports_approval_flag(bin: &str) -> bool {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("exec").arg("--help");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    match cmd.spawn() {
        Ok(child) => match child.wait_with_output().await {
            Ok(out) => {
                let s = String::from_utf8_lossy(&out.stdout);
                s.contains("ask-for-approval")
            }
            Err(_) => false,
        },
        Err(_) => false,
    }
}
