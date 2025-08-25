# codex-task

Multi-agent task orchestration for Codex CLI driven by a `task.md` file with YAML front-matter.

- Parses `task.md` -> schema -> DAG
- Spawns sub-agents via `codex exec --full-auto` with safe defaults
- Concurrency, retries, timeouts, logs, dry-run

See `codex-rs/examples/task.simple.md` for a minimal example.

Build:

```bash
cargo build --release --bin codex-task
```

Run:

```bash
target/release/codex-task run ./examples/task.simple.md --concurrency 3
```

