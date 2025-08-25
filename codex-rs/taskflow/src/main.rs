use clap::ArgAction;
use clap::Parser;
use clap::Subcommand;
use codex_task::extract_yaml_front_matter;
use codex_task::planner::auto_plan_tasks;
use codex_task::subagent::SubAgentOptions;
use codex_task::Dag;
use codex_task::Runner;
use codex_task::RunnerOptions;
use codex_task::TaskError;
use codex_task::TaskFile;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn ensure_default_specialized_agents(tf: &mut TaskFile) {
    if tf.agents.is_empty() {
        return;
    }
    // Use 'primary' as the template if present; otherwise use the first agent.
    let template = tf
        .agents
        .iter()
        .find(|a| a.id == "primary")
        .cloned()
        .or_else(|| tf.agents.first().cloned());
    let Some(base) = template else { return };

    let existing: BTreeSet<String> = tf.agents.iter().map(|a| a.id.clone()).collect();
    let defaults = [
        "backend", "frontend", "docs", "qa", "research", "devops", "tooling",
    ];
    for id in defaults {
        if !existing.contains(id) {
            let mut a = base.clone();
            a.id = id.to_string();
            tf.agents.push(a);
        }
    }
}

/// codex-task: Multi-agent orchestration for Codex CLI driven by task.md
#[derive(Parser, Debug)]
#[command(name = "codex-task")]
#[command(about = "Multi-agent orchestration for Codex CLI via task.md", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Execute a task.md file
    Run {
        /// Path to task.md
        file: PathBuf,

        /// Max concurrent tasks
        #[arg(long, default_value_t = 2)]
        concurrency: usize,

        /// Continue executing remaining tasks if one fails
        #[arg(long, action = ArgAction::SetTrue)]
        continue_on_error: bool,

        /// Default retries for tasks that don't specify it
        #[arg(long, default_value_t = 0)]
        retries: u32,

        /// Global model override (e.g. gpt-5)
        #[arg(long)]
        model: Option<String>,

        /// Global profile override (matches codex profiles)
        #[arg(long)]
        profile: Option<String>,

        /// Global sandbox override (read-only|workspace-write|danger-full-access)
        #[arg(long)]
        sandbox: Option<String>,

        /// Global approval mode override (never|on-request|always)
        #[arg(long)]
        ask_for_approval: Option<String>,

        /// Pass-through arguments to sub-agent codex exec
        #[arg(long, num_args=1.., value_delimiter=' ', allow_hyphen_values = true)]
        extra_args: Vec<String>,

        /// Dry-run mode (print plan and exit)
        #[arg(long, action = ArgAction::SetTrue)]
        dry_run: bool,

        /// Override base run dir (default: ~/.codex/task-runs/<run-id>)
        #[arg(long)]
        run_dir: Option<PathBuf>,

        /// Per-subagent default timeout (seconds)
        #[arg(long)]
        time_budget_secs: Option<u64>,

        /// Default cwd to use when task.cwd is not set
        #[arg(long)]
        default_cwd: Option<PathBuf>,

        /// Print live sub-agent stdout/stderr to the console
        #[arg(long, action = ArgAction::SetTrue)]
        show_output: bool,

        /// Convenience: always pass --skip-git-repo-check to sub-agents
        #[arg(long, action = clap::ArgAction::SetTrue)]
        skip_git_repo_check: bool,
    },
}

fn parse_task_file(path: &PathBuf) -> Result<TaskFile, TaskError> {
    let raw = fs::read_to_string(path)?;
    let yaml = extract_yaml_front_matter(&raw)?;
    let tf: TaskFile = serde_yaml::from_str(yaml)?;
    tf.validate().map_err(TaskError::InvalidTaskFile)?;
    Ok(tf)
}

fn read_task_body(path: &PathBuf) -> std::io::Result<String> {
    let raw = fs::read_to_string(path)?;
    let trimmed = raw.trim_start();
    let mut lines = trimmed.lines();
    if lines.next() != Some("---") {
        return Ok(String::new());
    }
    for line in lines.by_ref() {
        if line.trim() == "---" {
            break;
        }
    }
    let body: String = lines.collect::<Vec<_>>().join("\n");
    Ok(body)
}

#[tokio::main]
async fn main() -> Result<(), TaskError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            file,
            concurrency,
            continue_on_error,
            retries,
            model,
            profile,
            sandbox,
            ask_for_approval,
            extra_args,
            dry_run,
            run_dir,
            time_budget_secs,
            default_cwd,
            show_output,
            skip_git_repo_check,
        } => {
            let mut tf = parse_task_file(&file)?;
            let full_task_md = fs::read_to_string(&file).unwrap_or_default();
            // If the taskfile defines only a generic agent, add a default set of
            // specialized agents (cloned from the generic one) so the planner can
            // assign work to role-specific agents.
            ensure_default_specialized_agents(&mut tf);
            // Prepare sub-agent options for planning using CLI overrides
            let mut planning_sub = SubAgentOptions::default();
            planning_sub.model = model.clone();
            planning_sub.profile = profile.clone();
            planning_sub.sandbox = sandbox.clone();
            planning_sub.ask_for_approval = ask_for_approval.clone();
            planning_sub.extra_args.extend(extra_args.clone());
            if skip_git_repo_check {
                planning_sub.extra_args.push("--skip-git-repo-check".into());
            }
            planning_sub.time_budget_secs = time_budget_secs;
            if show_output {
                planning_sub.show_output = true;
            }
            planning_sub.task_md_full_text = Some(full_task_md.clone());
            if tf.tasks.is_empty() {
                // If we're auto-planning and the user supplied a concurrency on the CLI,
                // use it as the max task count hint for the planner when the taskfile
                // doesn't specify one.
                if tf.concurrency.is_none() {
                    tf.concurrency = Some(concurrency);
                }
                let body = read_task_body(&file).unwrap_or_default();
                let default_cwd_effective = default_cwd
                    .clone()
                    .or_else(|| std::env::var("CODEX_TASK_DEFAULT_CWD").ok().map(Into::into))
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let tasks =
                    auto_plan_tasks(&tf, &body, &default_cwd_effective, &planning_sub).await?;
                tf.tasks = tasks;
            }
            let dag = Dag::build(&tf)?;

            // bootstrap runner options
            let mut opts = RunnerOptions::with_defaults_from_taskfile(&tf);
            opts.concurrency = concurrency;
            opts.continue_on_error = continue_on_error;
            opts.retries = retries;
            opts.dry_run = dry_run;

            if let Some(rd) = run_dir {
                opts.base_run_dir = rd.join(opts.run_id.clone());
            }

            // Set default cwd only if explicitly provided via flag or env; otherwise
            // keep the runner's default (current working directory), avoiding the
            // fallback to per-run work directories.
            if let Some(dc) =
                default_cwd.or_else(|| std::env::var("CODEX_TASK_DEFAULT_CWD").ok().map(Into::into))
            {
                opts.default_cwd = Some(dc);
            }

            let mut sub = opts.subagent.clone();
            sub.model = model;
            sub.profile = profile;
            sub.sandbox = sandbox;
            sub.ask_for_approval = ask_for_approval;
            sub.extra_args.extend(extra_args);
            if skip_git_repo_check {
                sub.extra_args.push("--skip-git-repo-check".into());
            }
            sub.time_budget_secs = time_budget_secs;
            if show_output {
                sub.show_output = true;
            }
            sub.task_md_full_text = Some(full_task_md);
            opts.subagent = sub;

            let runner = Runner::new(&tf, dag, opts)?;
            runner.execute().await?;
        }
    }

    Ok(())
}
