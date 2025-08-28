use crate::error::TaskError;
use crate::schema::AgentSpec;
use crate::schema::TaskFile;
use crate::schema::TaskSpec;
use crate::subagent::run_subagent;
use crate::subagent::SubAgentOptions;
use regex::Regex;
use std::path::PathBuf;

/// Select a primary agent: prefer id == "primary", otherwise the first agent.
fn select_primary_agent(agents: &[AgentSpec]) -> Option<&AgentSpec> {
    if agents.is_empty() {
        return None;
    }
    agents
        .iter()
        .find(|a| a.id == "primary")
        .or_else(|| agents.first())
}

/// Generate a task plan by asking the primary agent to propose a tasks list.
/// Returns a new TaskFile with the same agents and generated tasks.
pub async fn auto_plan_tasks(
    tf: &TaskFile,
    task_body: &str,
    default_cwd: &PathBuf,
    subagent: &SubAgentOptions,
) -> Result<Vec<TaskSpec>, TaskError> {
    let primary = select_primary_agent(&tf.agents)
        .ok_or_else(|| TaskError::InvalidTaskFile("no agents defined".to_string()))?;

    // Build a strict instruction to output YAML only.
    let max_tasks = tf.concurrency.unwrap_or(6);
    let agent_ids: Vec<String> = tf.agents.iter().map(|a| a.id.clone()).collect();
    let instruction = format!(
        r#"
        "You are the lead orchestrator for a multi-agent team.

Objective:
{}

Context (from task.md body):
{}

Please propose a task plan as pure YAML wrapped in a fenced code block labeled yaml, with this exact shape (no prose):

```yaml
tasks:
  - id: <kebab-case-id>
    agent: <one-of: {:?}>
    instructions: <fully scoped instruction with role-specific primer and explicit scope>
    depends_on: [<ids>]
```

Agent roles and priming:
- Prefer specialized agents when available (e.g., backend, frontend, docs, qa, research, devops, tooling).
- If a specialized agent is not present in the allowed set, select 'primary' and include a short role primer inside instructions to prime the subagent (e.g., "You are a backend engineer ...").

Instruction quality bar for each task:
- Provide a brief role primer appropriate to the selected agent.
- Explicitly outline IN-SCOPE and OUT-OF-SCOPE boundaries; avoid vague goals.
- State concrete deliverables and success/acceptance criteria.
- Include any key constraints (files/dirs to touch, dependency boundaries, style or test requirements).
- Keep wording concise but complete; use imperative voice.

Constraints:
- Use at most {} tasks, but optimize total task number to the problem at hand.
- Avoid cycles.
        - Only output the YAML block with 'tasks:' as shown. No extra commentary."#,
        tf.objective, task_body, agent_ids, max_tasks
    );

    // Use the default cwd so the agent can operate in the repo.
    let workdir = default_cwd.clone();
    let log_dir = workdir.join(".codex").join("planner-logs");
    let task = TaskSpec {
        id: "auto-plan".to_string(),
        title: Some("Auto plan tasks".to_string()),
        agent: primary.id.clone(),
        instructions: instruction,
        depends_on: vec![],
        env: Default::default(),
        retries: Some(0),
        timeout_secs: Some(1200),
        cwd: Some(workdir.display().to_string()),
    };

    // Run the planner agent.
    let _ = tokio::fs::create_dir_all(&log_dir).await;
    let res = run_subagent(&workdir, primary, &task, subagent, &log_dir).await?;

    // Extract candidate YAML snippets from stdout and try to parse them.
    // When multiple candidates exist (e.g., due to retries/partial outputs),
    // prefer the LAST valid candidate which most closely reflects the final intent.
    let stdout = res.stdout;
    if let Some(tasks) = parse_tasks_from_output(&stdout) {
        return Ok(tasks);
    }

    Err(TaskError::InvalidTaskFile(format!(
        "planner did not return a valid YAML tasks list (see {} for output)",
        log_dir.join("stdout.log").display()
    )))
}

/// Parse the planner stdout and return the last valid tasks list found, if any.
pub(crate) fn parse_tasks_from_output(output: &str) -> Option<Vec<TaskSpec>> {
    let candidates = extract_yaml_candidates(output);

    // Try parsing all candidates and return the LAST one that passes a basic
    // sanity check (no missing dependencies, no duplicates). Prefer top-level `tasks:` docs.
    for candidate in candidates.iter().rev() {
        let clean = sanitize_yaml_candidate(candidate);
        if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&clean) {
            if let Some(tasks_val) = doc.get("tasks") {
                if let Ok(tasks) = serde_yaml::from_value::<Vec<TaskSpec>>(tasks_val.clone()) {
                    if validate_tasks(&tasks).is_ok() {
                        return Some(tasks);
                    }
                }
            }
        }
    }
    for candidate in candidates.iter().rev() {
        let clean = sanitize_yaml_candidate(candidate);
        if let Ok(gen_tf) = serde_yaml::from_str::<TaskFile>(&clean) {
            if validate_tasks(&gen_tf.tasks).is_ok() {
                return Some(gen_tf.tasks);
            }
        }
    }
    for candidate in candidates.iter().rev() {
        let clean = sanitize_yaml_candidate(candidate);
        if let Ok(list) = serde_yaml::from_str::<Vec<TaskSpec>>(&clean) {
            if validate_tasks(&list).is_ok() {
                return Some(list);
            }
        }
    }
    None
}

fn extract_yaml_candidates(output: &str) -> Vec<&str> {
    let mut candidates: Vec<&str> = Vec::new();

    // 1) fenced YAML code blocks: ```yaml ... ``` and ```yml ... ``` (most explicit, highest priority)
    if let Ok(re) = Regex::new(r"(?s)```ya?ml\s*(.*?)\s*```") {
        // Collect fenced blocks in encounter order; we will prefer the last valid
        // one during parsing to capture the final intended plan when multiple
        // blocks are present.
        for caps in re.captures_iter(output) {
            if let Some(m) = caps.get(1) {
                candidates.push(m.as_str());
            }
        }
    }

    // 2) front-matter style blocks starting and ending with --- at line starts.
    // Prefer blocks that contain a top-level 'tasks:' key.
    if let Ok(re) = Regex::new(r"(?ms)^---\s*\r?$\n(.*?)\n^---\s*\r?$") {
        for caps in re.captures_iter(output) {
            if let Some(m) = caps.get(1) {
                let s = m.as_str();
                if s.contains("tasks:") {
                    candidates.push(s);
                }
            }
        }
    }

    // 2b) generic fenced code blocks ```...``` (no language tag). If they contain
    // 'tasks:' treat as candidate. This helps when the planner forgets to mark
    // the fence as yaml.
    if let Ok(re) = Regex::new(r"(?s)```\s*(.*?)\s*```") {
        for caps in re.captures_iter(output) {
            if let Some(m) = caps.get(1) {
                let s = m.as_str();
                if s.contains("tasks:") {
                    candidates.push(s);
                }
            }
        }
    }

    // 3) from each occurrence of 'tasks:' to a likely boundary (closing '---', logs, fences) or EOF
    let mut search = output;
    let mut base_off = 0usize;
    while let Some(rel) = search.find("tasks:") {
        let start = base_off + rel;
        let rest = &output[start..];
        // Determine an end boundary conservatively.
        let mut end_idx = rest.len();
        // Next closing front-matter fence on its own line
        if let Some(mat) = Regex::new(r"(?m)\n---\s*$")
            .ok()
            .and_then(|re| re.find(rest))
        {
            end_idx = end_idx.min(mat.start());
        }
        // Any subsequent log/separator/code-fence line
        if let Some(mat) = Regex::new(r"(?m)^\[.*").ok().and_then(|re| re.find(rest)) {
            end_idx = end_idx.min(mat.start());
        }
        if let Some(mat) = Regex::new(r"(?m)^-{4,}\s*$")
            .ok()
            .and_then(|re| re.find(rest))
        {
            end_idx = end_idx.min(mat.start());
        }
        if let Some(mat) = Regex::new(r"(?m)^```\s*").ok().and_then(|re| re.find(rest)) {
            end_idx = end_idx.min(mat.start());
        }
        if let Some(mat) = Regex::new(r"(?mi)^Error:\s*")
            .ok()
            .and_then(|re| re.find(rest))
        {
            end_idx = end_idx.min(mat.start());
        }
        candidates.push(&rest[..end_idx]);
        // Advance search past this occurrence to find later ones
        let advance = rel + "tasks:".len();
        search = &search[advance..];
        base_off += advance;
    }

    // 4) As a last resort, try whole output
    candidates.push(output);

    // De-duplicate while preserving order
    let mut seen_ptrs = std::collections::BTreeSet::new();
    candidates
        .into_iter()
        .filter(|s| seen_ptrs.insert((*s).as_ptr() as usize))
        .collect()
}

fn sanitize_yaml_candidate(s: &str) -> String {
    let mut out = s.replace('\t', "  ");
    if out.starts_with('\u{feff}') {
        out = out.trim_start_matches('\u{feff}').to_string();
    }
    // Trim whitespace that sometimes trails after fenced blocks/logs
    out.trim().to_string()
}

/// Validate that a list of tasks has unique ids and no missing dependencies.
fn validate_tasks(tasks: &[TaskSpec]) -> Result<(), TaskError> {
    use std::collections::BTreeSet;

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for t in tasks {
        if !seen.insert(&t.id) {
            return Err(TaskError::DuplicateTask(t.id.clone()));
        }
    }
    for t in tasks {
        for dep in &t.depends_on {
            if !seen.contains(dep.as_str()) {
                return Err(TaskError::MissingDependency(dep.clone()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_last_valid_yaml_block() {
        // Simulate a noisy stream with an early, minimal YAML and a later, final YAML.
        let output = r#"
[2025-08-28T15:23:28] codex
```yaml
tasks:
  - id: a
    agent: primary
    instructions: "do a"
    depends_on: []
  - id: b
    agent: primary
    instructions: "do b"
    depends_on: [a]
```
[2025-08-28T15:23:30] stream error: stream disconnected before completion: ...
[2025-08-28T15:23:32] recovering
```yaml
tasks:
  - id: research
    agent: research
    instructions: "r"
    depends_on: []
  - id: build
    agent: backend
    instructions: "b"
    depends_on: [research]
  - id: test
    agent: qa
    instructions: "t"
    depends_on: [build]
```
misc trailing text
"#;

        let tasks = parse_tasks_from_output(output).expect("should parse tasks");
        let ids: Vec<_> = tasks.into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec!["research", "build", "test"]);
    }
}
