use crate::error::TaskError;
use crate::schema::AgentSpec;
use crate::schema::TaskSpec;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
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
    cmd.stdin(Stdio::null());
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

/// Merge environment variables with precedence: task.env > agent.env.
pub fn merge_envs(agent: &AgentSpec, task: &TaskSpec) -> BTreeMap<String, String> {
    let mut envs: BTreeMap<String, String> = agent.env.clone();
    for (k, v) in &task.env {
        envs.insert(k.clone(), v.clone());
    }
    envs
}

fn agent_primer(agent_id: &str) -> &'static str {
    match agent_id {
        "backend" => "You are an expert backend engineer. Design, implement, and test server-side logic and APIs. Follow repo conventions, keep changes minimally scoped, and respect boundaries defined in the task. Prefer reliability, tests, and clear docs.",
        "frontend" => "You are an expert frontend engineer. Implement UI with accessibility, responsiveness, and clean state management. Follow the code style and component patterns in this repo. Keep changes scoped to the task.",
        "docs" => "You are a technical writer. Produce clear, concise documentation with accurate examples. Maintain consistent tone and structure. Update any relevant README or docs sections coherently.",
        "qa" => "You are a quality engineer. Design and implement tests that validate behavior and guard against regressions. Prefer fast, deterministic tests and meaningful assertions.",
        "research" => "You are a research engineer. Investigate, compare options, and summarize trade-offs concisely. Provide actionable recommendations and next steps grounded in evidence.",
        "devops" => "You are a DevOps/SRE engineer. Improve build, CI/CD, and runtime reliability with minimal, safe changes. Favor observability, repeatability, and principle of least privilege.",
        "tooling" => "You are a tooling engineer. Enhance developer experience with safe, incremental changes to build scripts, linters, and CLIs. Keep the toolchain simple and well-documented.",
        "primary" => "You are a lead full-stack engineer. Break down work, make pragmatic choices, and deliver incremental value. Keep changes scoped and well-tested.",
        _ => "You are an expert software engineer. Work pragmatically, keep changes scoped, and adhere to repo conventions and tests.",
    }
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
