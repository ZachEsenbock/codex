---
version: "1"
objective: "End-to-end skeleton demo for codex-task"
agents:
  - id: primary
    model: gpt-5
    sandbox: workspace-write
    ask_for_approval: never
  - id: backend
    model: o4-mini
    sandbox: workspace-write
tasks:
  - id: setup
    title: "Initialize project scaffolding"
    agent: primary
    instructions: |
      Create a minimal hello-world program in this directory and print "hello".
    depends_on: []
    retries: 2

  - id: tests
    title: "Add a smoke test"
    agent: backend
    instructions: |
      Add one simple automated test that validates the hello program prints "hello".
    depends_on: ["setup"]
    retries: 2
    timeout_secs: 120
---
# Project notes

You can place any additional notes here for humans. The orchestrator only reads the YAML block above.

Notes for maintainers (belongs in PR body)

Workspace wiring: This PR adds a new member under codex-rs/taskflow producing a codex-task binary. No existing files are modified; release packaging (if desired) can be updated later to publish codex-task alongside codex. Build instructions remain the same (cargo build --release --bin codex-task). The README already documents codex exec and approvals/sandbox flags used by the orchestrator. 

Why process‑level sub‑agents? Keeps coupling minimal and leverages stable codex CLI behavior (exec --full-auto) today; avoids internal API churn while Rust CLI continues to evolve. Later, we can add a thin glue layer so codex task run delegates to this crate or rehost the runner in core if desired.

Safety: Defaults to --ask-for-approval never and --sandbox workspace-write; callers can change via CLI flags or per‑agent settings. The current_dir is set per task, reducing the write surface; logs are stored under ~/.codex/task-runs/<run-id>/<task-id>/....

Extensibility: Schema leaves room for budgets and acceptance criteria; those can be enforced in a follow‑up once codex exec exposes stable usage telemetry.

