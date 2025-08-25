use crate::dag::Dag;
use crate::error::TaskError;
use crate::schema::AgentSpec;
use crate::schema::TaskFile;
use crate::schema::TaskSpec;
use crate::subagent::detect_supports_approval_flag;
use crate::subagent::run_subagent;
use crate::subagent::SubAgentOptions;
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;
use std::collections::BTreeMap;
use std::path::PathBuf;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tokio::time::Duration;
use uuid::Uuid;

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

        let semaphore = std::sync::Arc::new(Semaphore::new(self.opts.concurrency));
        for level in self.dag.levels() {
            println!("starting wave with tasks: {level:?}");
            let mut futs = FuturesUnordered::new();
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

                println!(
                    "launching task: {} (agent: {}) in {}",
                    id,
                    agent.id,
                    run_dir.display()
                );
                futs.push(tokio::spawn(async move {
                    // acquire a permit for the lifetime of the task
                    let _permit = match sem.acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => return Err(TaskError::Spawn("semaphore closed".into())),
                    };
                    execute_with_retries(run_dir, agent, task, &opts).await
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

            println!("completed wave\n");

            if !level_ok && !self.opts.continue_on_error {
                return Err(TaskError::InvalidTaskFile(
                    "one or more tasks failed".to_string(),
                ));
            }
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

async fn execute_with_retries(
    run_dir: PathBuf,
    agent: AgentSpec,
    task: TaskSpec,
    opts: &RunnerOptions,
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

        let result = run_subagent(&cwd, &agent, &task, &opts.subagent, &log_dir).await;
        match result {
            Ok(_ok) => {
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
                return Ok(());
            }
            Err(e) => {
                let now = OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "".into());
                let err_evt = serde_json::json!({
                    "ts": now,
                    "task_id": task.id,
                    "attempt": attempt,
                    "event": "error",
                    "error": format!("{e}"),
                })
                .to_string();
                write_event(&run_dir, &err_evt).await;

                if attempt > retries {
                    return Err(e);
                } else {
                    // backoff
                    sleep(Duration::from_millis(500 * attempt as u64)).await;
                }
            }
        }
    }
}
