---
version: "1"
objective: "Design and implement a multi‑agent TUI for codex-task with per‑agent pause (Ctrl+C), user guidance injection, and optional cross‑agent communication. Deliver a dashboard that runs multiple sub‑agents concurrently, each with its own labeled pane and input while paused."
agents:
  - id: tooling
    model: gpt-5
    sandbox: workspace-write
    ask_for_approval: on-request
  - id: frontend
    model: gpt-5
    sandbox: workspace-write
    ask_for_approval: on-request
  - id: docs
    model: gpt-5
    sandbox: workspace-write
    ask_for_approval: on-request
tasks:
  # Remaining work: TUI + UI integration + docs
  - id: tui.dashboard_scaffold
    title: "Scaffold multi‑agent dashboard views"
    agent: frontend
    instructions: |
      Build a multi‑agent dashboard in `codex-rs/tui`:
      - Sidebar: list all agents with Task ID, status (Running/Paused/Done/Error), and a brief activity indicator. Use concise stylings per styles.md.
      - Main pane (focused agent): streaming transcript (stdout/stderr), scrollback, header with Task ID and status.
      - Watch split: optional second pane showing another agent read‑only; toggle via 'w'; selection via Tab/Shift+Tab or 1–9.
      - Focus management: track which agent is focused; maintain per‑agent scroll offsets and input state.
      - Data sources: consume `AgentStream` snapshots and subscribe for live updates; render lines with ratatui Stylize helpers.
      Deliver a `MultiAgentDashboard` entry in the `codex-tui` crate exposing an event loop that can be invoked by codex-task.
    cwd: codex-rs/tui
    depends_on: []

  - id: tui.key_input
    title: "Implement key handling and input composer"
    agent: frontend
    instructions: |
      Implement keybinds and paused input:
      - Ctrl+C pauses the focused agent (the UI should not exit); show an input box labeled with Task ID.
      - Enter sends the current input to the agent and resumes; Ctrl+J inserts a newline.
      - Tab/Shift+Tab cycles focus; 1–9 jumps to an agent; 'w' toggles watch split.
      - Display small banners when routed messages are received/sent.
      Connect the input composer to a callback hook `on_inject(task_id, text)` that codex-task will provide.
    cwd: codex-rs/tui
    depends_on: ["tui.dashboard_scaffold"]

  - id: tooling.ui_integration
    title: "Integrate codex-task with dashboard UI"
    agent: tooling
    instructions: |
      Wire codex-task to launch the dashboard when `--ui` is set:
      - Add a dependency on `codex-tui`.
      - Export live `AgentStream`s and control handles (pause/resume/inject) to the dashboard.
      - Implement `on_pause(task_id)`, `on_resume(task_id)`, and `on_inject(task_id, text)` using `SubAgentController`.
      - Honor `--ui-layout` (tabbed default; grid can be a no-op placeholder initially).
      - Ensure non‑UI mode stays unchanged.
    cwd: codex-rs/taskflow
    depends_on: ["tui.dashboard_scaffold", "tui.key_input"]

  - id: tui.snapshots
    title: "Add snapshot tests for dashboard states"
    agent: frontend
    instructions: |
      Add insta snapshot tests in `codex-rs/tui` for:
      - Idle (no agents), running with one agent, running with multiple agents.
      - Paused state with input composer visible.
      - Watch split on/off.
      - Routed message banners visible in source/target panes.
      Follow the repo conventions for cargo-insta usage.
    cwd: codex-rs/tui
    depends_on: ["tui.dashboard_scaffold"]

  - id: docs.usage
    title: "Update docs with usage, keybinds, and examples"
    agent: docs
    instructions: |
      Update documentation:
      - `codex-rs/taskflow/README.md`: describe `--ui`, `--ui-layout`, pausing (Ctrl+C), injecting (Enter, Ctrl+J), switching focus, watch split, and routed messages (@@to: blocks).
      - `codex-rs/tui/styles.md`: add guidance for the new dashboard components and key hint styles.
      Include short walkthroughs and troubleshooting (signals/platform differences, closed stdin, etc.).
    cwd: codex-rs
    depends_on: ["tooling.ui_integration"]
---
Title: Multi‑agent TUI for codex‑task (Tabbed + Watch Split)

Summary
- Build a TUI dashboard for codex‑task where multiple sub‑agents run concurrently, each with its own clearly labeled pane (by Task ID), scrollback, and status. The focused agent can be paused with Ctrl+C at any time; while paused, the user can type guidance (like codex‑cli) and press Enter to resume. Provide an optional watch split to monitor a second agent.

Status Board
- Backend control-plane: Completed — SubAgentController (pause/resume/inject/restart), AgentStream, routed message parser and router, start/success/error/routed events, `--ui` and `--ui-layout` flags.
- tui.dashboard_scaffold: Completed — sidebar + focused pane + optional watch split; per‑agent scrollback; banner line; rendering via ratatui WidgetRef.
- tui.key_input: Completed — Ctrl+C pause, Enter send/resume, Ctrl+J newline, Tab/Shift+Tab cycle focus, 1–9 jump, 'w' toggle watch, arrows cycle watch target.
- tooling.ui_integration: Completed — `codex-task` launches `MultiAgentDashboard` under `--ui`, streams updates, and wires pause/resume/inject callbacks.
- tui.snapshots: Pending — snapshot tests implemented in `dashboard.rs` (insta assertions); snapshot files need to be recorded/accepted.
- docs.usage: Not started — docs updates for UI usage/keybinds/routing still outstanding.

Known Issues
- Provider/network errors (e.g., “stream disconnected before completion”) surface as red stderr lines in the dashboard; ensure credentials and network connectivity are configured for the chosen provider.
- Terminal compatibility: the dashboard enters the terminal alternate screen. If nothing appears, try a different terminal emulator or ensure the process has a real TTY (WSL/ConPTY quirks can hide alt‑screen UIs). The UI now persists across the entire run, not just a single DAG wave.

Next Steps
- Accept snapshots once you’re satisfied: `cargo test -p codex-tui`; then `cargo insta accept -p codex-tui`.
- Document usage and keybinds in `codex-rs/taskflow/README.md` and `codex-rs/tui/styles.md`.

Explicit Scope (intentionally limited)
- Pause/resume for the focused agent: Ctrl+C to pause, Enter to send guidance and resume, Ctrl+J for newline in input.
- Default layout: tabbed primary view with optional two‑pane watch split; future grid view is a follow‑up toggle.
- Per‑agent scrollback, status (Running/Paused/Done/Error), and task label (Task ID).
- Streaming logs wired to both the TUI buffers and persisted logs; structured events in `events.ndjson`.
- Cross‑agent communication via a lightweight stdout convention parsed by the runner, routed as injections to target agents, with UI surfacing.
- In‑memory state only (no persistence beyond logs); rely on Tokio for concurrency.

Out of Scope
- Remote control, networking, or multi‑host orchestration.
- Session restore of interactive state (beyond existing on‑disk logs/events).
- Full tiled grid with N arbitrary panes (future enhancement).

Notes
- Layout choice: Tabbed primary with an optional watch split is recommended over a full grid for readability and complexity. It scales with many agents (sidebar list + jump keys) while enabling focused interaction. A grid toggle can come later for power users.
- Signals: Prefer sending SIGINT to the sub‑agent to trigger codex’s pause behavior. Provide fallbacks (SIGSTOP/SIGCONT on Unix; CTRL_BREAK on Windows). If pause/inject isn’t acknowledged, restart with appended guidance.
- Keybinds (initial): Ctrl+C pause; Enter send/resume; Ctrl+J newline; Tab/Shift+Tab cycle focus; 1–9 jump; 'w' toggle watch split.
- Styling: Follow TUI style conventions; use ratatui’s Stylize helpers (e.g., "Paused".yellow(), dim separators, cyan key hints).

Prerequisites
- Linux: ensure OpenSSL dev packages are installed for workspace crates that use native-tls (e.g., `sudo apt-get install -y pkg-config libssl-dev`). On macOS: `brew install openssl@3` and ensure pkg-config picks it up (`export PKG_CONFIG_PATH="$(brew --prefix)/opt/openssl@3/lib/pkgconfig:$PKG_CONFIG_PATH"`). This is only to satisfy workspace dependencies; the codex-task crate itself does not directly depend on OpenSSL.

Validation and Errors
- If sending a signal fails or is unsupported, show a non‑blocking toast in the pane and offer to retry or fall back to restart‑with‑guidance.
- If the child exits while paused, mark the agent Done/Error and disable input.
- Guard against writing to a closed stdin; detect EPIPE and present a clear message.
- Ensure Ctrl+C is intercepted by the app and not the host process; only the focused agent is paused.
- UI should never freeze on a blocked write; use bounded channels and backpressure.

Testing Checklist
- Unit tests in taskflow for: signal wrapper on Unix (mocked), stdin injection logic, events.ndjson emission, routing parser for inter‑agent messages.
- Snapshot tests in codex‑tui for: idle, running, paused with input, error state, and watch split.
- Manual smoke: run two trivial tasks concurrently, pause one, inject a line, resume, verify logs and status; route a message between them using @@to:.

Acceptance Criteria
- Can run `codex-task run --ui <file>` and see a dashboard with a sidebar of agents and a main pane for the focused agent.
- Pressing Ctrl+C pauses only the focused agent; an input composer appears with a Task‑ID‑labeled prompt.
- Typing guidance and pressing Enter injects the message and resumes; Ctrl+J inserts a newline.
- Switching focus with Tab/Shift+Tab or 1–9 is responsive; the watch split toggles with 'w'.
- Logs stream live, persist under the run directory, and events.ndjson records pause/resume/inject and routed messages.
- Agents can emit inter‑agent messages using the @@to: convention; the target receives the guidance, and the UI shows a routed banner in both panes.

Exact Files To Edit (More if you identify them along the way)
- `codex-rs/taskflow/src/subagent.rs`: stdin piping; signal control wrapper; stream/log fan‑out; inter‑agent message parser hook.
- `codex-rs/taskflow/src/runner.rs`: track per‑agent state; events.ndjson for pause/resume/inject; expose channels to the TUI; wire --ui flags.
- `codex-rs/taskflow/src/main.rs`: CLI flags `--ui`, `--ui-layout` and initialization for dashboard mode.
- `codex-rs/tui/src/`: new dashboard module (multi‑agent panes, input composer, focus, watch split) and snapshot tests.
- `codex-rs/taskflow/README.md` and `codex-rs/tui/styles.md`: usage and style updates.

Coding Notes
- Keep changes minimal and consistent with existing style; prefer adding focused modules over invasive refactors.
- Use concise Stylize helpers for TUI text (e.g., "M".red(), "Paused".yellow(), dim separators).
- Crate names are prefixed with `codex-`; reuse `codex-tui` rather than creating a separate TUI crate unless necessary.
- Never modify code related to CODEX_SANDBOX_* env vars.
- Tokio concurrency is sufficient; avoid adding a custom scheduler unless profiling proves otherwise.
