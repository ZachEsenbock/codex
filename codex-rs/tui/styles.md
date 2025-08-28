# Headers, primary, and secondary text

- **Headers:** Use `bold`. For markdown with various header levels, leave in the `#` signs.
- **Primary text:** Default.
- **Secondary text:** Use `dim`.

# Foreground colors

- **Default:** Most of the time, just use the default foreground color. `reset` can help get it back.
- **User input tips, selection, and status indicators:** Use ANSI `cyan`.
- **Success and additions:** Use ANSI `green`.
- **Errors, failures and deletions:** Use ANSI `red`.
- **Codex:** Use ANSI `magenta`.

# Dashboard conventions

- **Key hints and prompts:** Use `cyan` (e.g., "Ctrl+C to pause" -> `"Ctrl+C".cyan()` and surrounding text dimmed).
- **Statuses:**
  - Running: default or `green` for short labels (avoid over‑coloring long logs).
  - Paused: `yellow` to clearly differentiate the interactive state.
  - Done/Success: `green` (consider `.dim()` for reduced emphasis in lists).
  - Error: `red`.
- **Routed banners:** Keep subtle; prefer `magenta` for the "routed" keyword and `cyan` for task IDs.
- **Separators, timestamps, and secondary chrome:** Use `dim`.

Examples (using `Stylize` helpers):

- `vec!["» routed from ".into(), src_id.cyan(), " (".dim(), bytes.to_string().dim(), ")".dim()]`
- `"Paused".cyan()`; `"Done".green().dim()`; `"Error".red()`

## Components

- **Sidebar (agents list):**
  - Index digit: `cyan` (e.g., `"1".cyan()`).
  - Task ID: default; focus uses `bold`; watched agent adds `.dim()` "(watch)" tag.
  - Status chip: short word only, color by status above; avoid coloring the whole row.
  - Separators (`·`, `—`, `|`): `.dim()`.

- **Main header (focused pane):**
  - Format: `Task <id> — <status>` where `<id>` is `cyan` and `<status>` uses status colors; surrounding dashes and timestamps are `.dim()`.
  - When watch split is on, include a right‑aligned `Watching <id>` in `.dim()` with the `<id>` in `cyan`.

- **Scrollback (logs):**
  - Default color for content; avoid rainbow logs.
  - Secondary markers (timestamps, stream labels) `.dim()` only.

- **Input composer (paused):**
  - Prompt: `"Inject to ".into(), task_id.cyan(), ": ".dim()`; keep concise.
  - Key hints: concise cyan tokens, e.g., `vec!["Enter".cyan(), " send ".dim(), "Ctrl+J".cyan(), " newline".dim()]`.
  - User text: default. Placeholder or empty state: `.dim()`.

- **Key hints bar (footer):**
  - Use short cyan tokens for keys and `.dim()` bullets: `Ctrl+C`, `Enter`, `Ctrl+J`, `Tab`, `Shift+Tab`, `1-9`, `w`.
  - Example: `vec!["Ctrl+C".cyan(), " pause ".dim(), "• ".dim(), "Enter".cyan(), " send".dim()]`.

- **Routed message banners:**
  - Source pane: `vec!["» routed to ".magenta(), dst_id.cyan(), " (".dim(), bytes.to_string().dim(), ")".dim()]`.
  - Target pane: `vec!["» routed from ".magenta(), src_id.cyan(), " (".dim(), bytes.to_string().dim(), ")".dim()]`.

# Avoid

- Avoid custom colors because there's no guarantee that they'll contrast well or look good in various terminal color themes. (`shimmer.rs` is an exception that works well because we take the default colors and just adjust their levels.)
- Avoid ANSI `black` & `white` as foreground colors because the default terminal theme color will do a better job. (Use `reset` if you need to in order to get those.) The exception is if you need contrast rendering over a manually colored background.
- Avoid ANSI `blue`; and avoid `yellow` except for the explicit Paused status chip noted above. Prefer the foreground colors mentioned above.

(There are some rules to try to catch this in `clippy.toml`.)
