# Dashboard Layout: Tabbed Primary + Optional Watch Split

This document records the layout decision for the upcoming multi‑agent dashboard used by `codex-task`.

## Summary

- Default layout: a tabbed view where one focused agent occupies the main pane.
- Optional watch split: toggle a second read‑only pane to monitor another agent concurrently.
- Future: a grid toggle (tiled N‑pane view) can be added later for power users.

## Rationale

- Readability first: the focused agent gets the full width/height, which keeps transcripts and logs legible and reduces cognitive load.
- Scales with many agents: the sidebar + number keys (1–9) and Tab/Shift+Tab cycling make it easy to navigate, without cramming many panes.
- Pragmatic complexity: a full grid adds non‑trivial layout, focus, and scroll state management overhead; the watch split delivers most value at a lower cost.

## Pros / Cons

Pros:
- Clear focus: primary agent is always readable at full size.
- Lightweight to implement: simpler focus, scrollback, and input handling than a full grid.
- Useful monitoring: the watch pane shows another agent’s logs live without interaction.

Cons:
- Only two agents visible at once (primary + watch). Others are accessible via tabs but not simultaneously visible.
- Power users may still want a dense grid to glance at many agents at once.

## Key Interactions (initial set)

- Ctrl+C: pause the focused agent.
- Enter (while paused): send composed guidance and resume.
- Ctrl+J: insert newline into the input (while paused).
- Tab / Shift+Tab: cycle focus across agents.
- 1–9: jump directly to an agent by index.
- `w`: toggle the watch split; when open, arrows or numbers select the watch target.

## Future Path to Grid Toggle

- Add a layout mode enum (Tabbed | Grid) and a `--ui-layout` flag. Keep Tabbed as default.
- Grid semantics: tiled panes (2×2, 3×2, etc.) with reduced per‑pane chrome; focus ring + per‑pane scrollback.
- Implementation approach: reuse the same per‑agent ring buffers and status, but render multiple panes with shared input focus logic. Introduce a lightweight viewport manager to track visible agents and scroll states.
- Trade‑offs: grid mode reduces per‑pane readability; keep it opt‑in and remember the last selection.

## Implementation Notes

- Rendering: use ratatui layout splits. Primary pane takes remaining space after sidebar; watch pane, when enabled, splits the primary region vertically.
- State: maintain per‑agent scrollback buffers and status (Running/Paused/Done/Error). The watch pane is strictly read‑only.
- Styling: follow `styles.md` and prefer `Stylize` helpers (e.g., "Paused".yellow(), dim separators, cyan key hints).

This layout balances readability and complexity now, while leaving a clear path to an optional grid view later.

