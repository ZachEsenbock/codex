use codex_task::extract_yaml_front_matter;
use codex_task::subagent::merge_envs;
use codex_task::Dag;
use codex_task::Runner;
use codex_task::RunnerOptions;
use codex_task::TaskFile;

const SIMPLE: &str = r#"---
version: "1"
objective: "Ship a thing"
agents:
  - id: "primary"
    model: "o4-mini"
  - id: "backend"
    model: "gpt-5"
tasks:
  - id: "design"
    agent: "primary"
    instructions: "Do the high-level design."
    depends_on: []
  - id: "api"
    agent: "backend"
    instructions: "Implement API."
    depends_on: ["design"]
  - id: "tests"
    agent: "backend"
    instructions: "Write tests."
    depends_on: ["api"]
  - id: "docs"
    agent: "primary"
    instructions: "Draft docs."
    depends_on: ["api"]
concurrency: 2
---
Body not used
"#;

#[test]
fn test_frontmatter_parse() {
    let yaml = match extract_yaml_front_matter(SIMPLE) {
        Ok(y) => y,
        Err(e) => panic!("frontmatter parse failed: {e}"),
    };
    let tf: TaskFile = match serde_yaml::from_str(yaml) {
        Ok(v) => v,
        Err(e) => panic!("yaml parse failed: {e}"),
    };
    assert_eq!(tf.version, "1");
    assert_eq!(tf.objective, "Ship a thing");
    assert_eq!(tf.tasks.len(), 4);
}

#[test]
fn test_dag_levels() {
    let yaml = match extract_yaml_front_matter(SIMPLE) {
        Ok(y) => y,
        Err(e) => panic!("frontmatter parse failed: {e}"),
    };
    let tf: TaskFile = match serde_yaml::from_str(yaml) {
        Ok(v) => v,
        Err(e) => panic!("yaml parse failed: {e}"),
    };
    let dag = match Dag::build(&tf) {
        Ok(d) => d,
        Err(e) => panic!("dag build failed: {e}"),
    };
    let levels = dag.levels();
    // Expected waves: [design], [api], [tests, docs] (third level order deterministic by sort)
    assert_eq!(levels.len(), 3);
    assert_eq!(levels[0], vec!["design".to_string()]);
    assert_eq!(levels[1], vec!["api".to_string()]);
    assert_eq!(levels[2], vec!["docs".to_string(), "tests".to_string()]);
}

#[test]
fn test_cyclic_dag_rejected() {
    const CYCLIC: &str = r#"---
version: "1"
objective: "Cycle"
agents:
  - id: a
tasks:
  - id: t1
    agent: a
    instructions: ""
    depends_on: ["t2"]
  - id: t2
    agent: a
    instructions: ""
    depends_on: ["t1"]
---
"#;
    let yaml = match extract_yaml_front_matter(CYCLIC) {
        Ok(y) => y,
        Err(e) => panic!("frontmatter parse failed: {e}"),
    };
    let tf: TaskFile = match serde_yaml::from_str(yaml) {
        Ok(v) => v,
        Err(e) => panic!("yaml parse failed: {e}"),
    };
    if Dag::build(&tf).is_ok() {
        panic!("should be cyclic")
    }
}

#[test]
fn test_env_merge_task_overrides_agent() {
    use std::collections::BTreeMap;
    let agent = codex_task::AgentSpec {
        id: "a".into(),
        model: None,
        profile: None,
        env: BTreeMap::from([
            ("FOO".into(), "from_agent".into()),
            ("BAR".into(), "agent_bar".into()),
        ]),
        sandbox: None,
        ask_for_approval: None,
    };
    let task = codex_task::TaskSpec {
        id: "t".into(),
        title: None,
        agent: "a".into(),
        instructions: "hi".into(),
        depends_on: vec![],
        env: BTreeMap::from([
            ("FOO".into(), "from_task".into()),
            ("BAZ".into(), "task_baz".into()),
        ]),
        retries: None,
        timeout_secs: None,
        cwd: None,
    };
    let merged = merge_envs(&agent, &task);
    assert_eq!(merged.get("FOO").cloned(), Some("from_task".into()));
    assert_eq!(merged.get("BAR").cloned(), Some("agent_bar".into()));
    assert_eq!(merged.get("BAZ").cloned(), Some("task_baz".into()));
}

#[test]
fn test_dry_run_renders_plan() {
    let yaml = match extract_yaml_front_matter(SIMPLE) {
        Ok(y) => y,
        Err(e) => panic!("frontmatter parse failed: {e}"),
    };
    let tf: TaskFile = match serde_yaml::from_str(yaml) {
        Ok(v) => v,
        Err(e) => panic!("yaml parse failed: {e}"),
    };
    let dag = match Dag::build(&tf) {
        Ok(d) => d,
        Err(e) => panic!("dag build failed: {e}"),
    };
    let mut opts = RunnerOptions::with_defaults_from_taskfile(&tf);
    opts.dry_run = true;
    let runner = match Runner::new(&tf, dag, opts) {
        Ok(r) => r,
        Err(e) => panic!("runner failed: {e}"),
    };
    let plan = runner.render_plan();
    assert!(plan.contains("# Run Plan"));
    assert!(plan.contains("objective:"));
}
