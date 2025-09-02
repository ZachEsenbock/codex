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

## UI Dashboard

Run with an interactive dashboard by passing `--ui`:

```bash
codex-task run --ui ./examples/task.simple.md
```

 - Default layout: tabbed primary view with an optional two‑pane watch split.
 - Toggle layout with `--ui-layout=tabbed|grid` (tabbed is the default). The `grid` layout may be a no‑op placeholder initially.
 - Without `--ui`, non‑UI mode remains unchanged.

Planner overlay (when auto‑planning):

- If your task definition uses auto‑planning, enabling `--ui` shows a lightweight, full‑screen planner overlay while the planner runs.
- Visuals: a single pane with header "Planning…" (cyan), optional banner for errors/warnings, and a scrollable log area.
- Keys: PageUp/PageDown/Home/End scroll the planner log; `Esc` is ignored; there is no input composer.
- On success, the overlay closes and the normal dashboard starts. On failure, the overlay shows an error banner and exits non‑zero.
- Planner logs continue to be written under `~/.codex/planner-logs/` (`stdout.log`, `stderr.log`).

Flags summary:

- `--ui`: enable the interactive dashboard.
- `--ui-layout=tabbed|grid`: select layout (grid may be a placeholder).
 - Network access: sub‑agents (including the planner) run with the workspace‑write sandbox and network enabled by default. This keeps writes scoped to the repo/TMP while allowing HTTP. Override with `-c sandbox_workspace_write.network_access=false` or `--sandbox read-only|danger-full-access`.

Keybinds (minimal set, consistent with codex‑cli):

- Ctrl+C: pause the focused agent (dashboard intercepts; host process keeps running).
- Enter (while paused): send composed guidance to the focused agent and resume.
- Ctrl+J (while paused): insert a newline in the input box.
- Tab / Shift+Tab: cycle focus across agents.
- 1–9: jump directly to an agent by index.
- `w`: toggle the watch split; when open, arrows or 1–9 select the watch target.
 - PageUp / PageDown / Home / End: scroll logs. When scrollback extends beyond in‑memory buffers, older content is paged from on‑disk attempt logs.
 - q (after completion): when all sub‑agents are Done/Error, the dashboard shows “All agents complete — press q to quit”; press `q` to close the UI.
 - Ctrl+Q: aborts immediately (kills active sub‑agents) and exits the UI.

Styling follows `codex-rs/tui/styles.md` (use `Stylize` helpers; cyan key hints; status chips: Queued default/plain, Paused cyan, Done green, Error red).

### Quick walkthrough

1. Start two agents with UI enabled:

   ```bash
   codex-task run --ui ./examples/task.simple.md --concurrency 2
   ```

2. Switch focus with Tab or numbers (`1`, `2`). The focused agent’s pane shows a header with its Task ID and status; the sidebar lists all agents.

3. Press Ctrl+C to pause the focused agent. An input composer appears labeled with the Task ID and key hints (Enter to send, Ctrl+J for newline).

4. Type guidance and press Enter. The runner injects the text to the agent’s stdin, resumes it, and logs `inject` + `resume` events. The composer closes.

5. Toggle the watch split with `w` to tail another agent in a secondary read‑only pane. Use 1–9 or arrows to change the watched agent. Only the focused agent accepts input.

Statuses:

- Agents initialize as `Queued` before they start running; the sidebar shows a concise `Q` chip.
- When a task starts, it transitions to `Running` (`R` in the sidebar). `Paused` shows `P`, `Done` shows `D`, and `Error` shows `E`.

Scrolling and full history:

- Use PageUp/PageDown/Home/End to scroll within a pane. Both the primary and watch panes support scrolling.
- Each agent view maintains a live tail plus full‑history scrollback by paging from the current attempt’s log files (`stdout.log` and `stderr.log`). Older content is loaded incrementally from disk when you scroll beyond the in‑memory head; the entire file is not loaded at once.

## Inter‑agent routing protocol

Agents can send guidance to a peer by printing a routed block to stdout:

```
@@to: <task_id>
<message>
@@/to
```

The runner parses these blocks and delivers the `<message>` to the target agent identified by `<task_id>`. Messages may span multiple lines. Blocks cannot be nested.

When routing occurs, the runner records a `routed` event (see Events below) and pushes a small banner line into both source and target streams (visible in the dashboard scrollback).

Routing to future‑wave agents:

- If a routed message targets an agent that has not started yet, the runner spools the message for delivery when that agent begins.
- Spool persistence: messages are appended under the run directory so they survive long runs and restarts:

  `~/.codex/task-runs/<run_id>/spool/<target_task_id>.txt`

- On agent start, any spooled lines for that task are injected before consuming child output. After successful delivery, the in‑memory and on‑disk spools are cleared.

Example banner text (source/target panes):

- Source: `» routed to <task_id> (<N> bytes)`
- Target: `» routed from <task_id> (<N> bytes)`

UI visibility:

- The sidebar and pane headers show a `↔ <count>` indicator per agent reflecting peer messages routed to or from that agent during the run.

Tip: You can paste a routed block directly into a paused composer to send a message on behalf of the focused agent.

## Pause, inject, and resume

While viewing the focused agent in the dashboard:

- Press Ctrl+C to pause it. A composer appears at the bottom of the pane.
- Type guidance. Press Ctrl+J to insert newlines without submitting.
- Press Enter to send the guidance to the agent and resume.

Behavior and safeguards:

- Injection is written to the child’s stdin followed by a newline. If stdin is closed, the write fails (EPIPE) and the UI surfaces a non‑blocking message.
- Best‑effort resume is attempted after injection. If the agent does not acknowledge, the runner may kill and restart it with the additional guidance appended to its instructions (previous appended guidance is preserved).

Composer UX:

- Prompt shows `Inject to <task_id>` with concise key hints in cyan.
- Ctrl+J inserts a newline without submitting.
- Enter sends and resumes; the composer clears on success.

## Signals: platform behavior

To pause, the runner sends a best‑effort signal to the child process:

- Unix: prefer `SIGINT` to trigger codex’s pause prompt. If that fails, fall back to `SIGSTOP`. Resume uses `SIGCONT` when needed.
- Windows: send a console CTRL event to the process group using `CTRL_BREAK_EVENT` and fall back to `CTRL_C_EVENT` if needed. There is no direct resume equivalent; resuming proceeds by continuing normal execution after injection.

Failures to signal are logged and surfaced in the UI as a non‑blocking message; guidance injection and restart‑with‑guidance remain available as alternatives.

## Watch split

Press `w` to toggle a secondary read‑only pane that tails another agent’s output.

- Use arrow keys or 1–9 to select which agent to watch.
- Only the focused (primary) agent accepts input and can be paused/injected.
- Each agent maintains independent scrollback and status (Queued/Running/Paused/Done/Error).

Hints:

- The watch split is read‑only. Pausing always targets the primary (focused) pane.
- The sidebar highlights both the focused and watched agents to keep context clear.

## Logs and events

Each run is assigned a unique Run ID and stored on disk under:

```
~/.codex/task-runs/<run_id>/<task_id>/
```

Per‑attempt logs are written under `attempt-<N>/`:

- `attempt-1/stdout.log`
- `attempt-1/stderr.log`

Spool files (for inter‑wave routing) are stored per run under:

- `~/.codex/task-runs/<run_id>/spool/<task_id>.txt`

Structured events for the task are appended to `events.ndjson` at the task’s run directory (one line of JSON per event):

- `start`: task attempt began (includes `cwd`).
- `success`: task finished with exit code 0.
- `error`: task finished non‑zero (includes `code` or `error`).
- `pause`, `resume`, `inject`, `restart`: control actions taken while running.
- `routed`: an inter‑agent message was emitted from this task (includes `to` and `bytes`).

These events power the dashboard UI and can be inspected post‑run for auditing.

## Troubleshooting

- Ctrl+C exits my terminal instead of pausing: Ensure you launched with `--ui`. In the dashboard, Ctrl+C is intercepted and routed to the focused agent. If your terminal sends a different code for BackTab, Shift+Tab may not work; use numbers to jump.
- Injection reports EPIPE (stdin closed): The agent may have already exited or closed stdin. The UI shows a non‑blocking message. Re‑run the task or use restart‑with‑guidance if offered.
- No pause on Windows: Console CTRL events require the child to share a console. If pause is not acknowledged, use inject (Enter) and restart‑with‑guidance fallback.
- Nothing appears in watch split: Verify you selected a different agent (1–9 or arrows). The watch pane is read‑only and collapses when `w` is pressed again.
- Colors are hard to read: The TUI avoids hardcoded backgrounds; foreground color hints follow terminal themes. See `codex-rs/tui/styles.md` for guidance.
- UI doesn’t appear / stuck on a blank screen: The dashboard uses the terminal alternate screen and requires a real TTY. If using WSL/ConPTY or piping output, try a different terminal emulator (e.g., Windows Terminal) or run directly in a local terminal. Press `q` to exit if your terminal captured an alternate screen unexpectedly.
