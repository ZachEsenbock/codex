use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;

/// Minimal per-agent status for key handling decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Paused,
    Done,
    Error,
}

#[derive(Debug, Clone)]
pub struct AgentPaneState {
    pub task_id: String,
    pub status: AgentStatus,
    /// Accumulated input while paused for this agent.
    pub compose: String,
    /// Rendered log lines for the agent (latest at the end).
    pub log: Vec<Line<'static>>,
    /// Scrollback from the bottom (0 = show newest lines).
    pub scrollback: usize,
    /// Optional one-line banner (e.g., routed message notice).
    pub banner: Option<Line<'static>>,
}

impl AgentPaneState {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: AgentStatus::Running,
            compose: String::new(),
            log: Vec::new(),
            scrollback: 0,
            banner: None,
        }
    }
}

/// Actions emitted by the dashboard input handler for the outer controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashboardAction {
    PauseAgent {
        index: usize,
    },
    /// Submit guidance and resume agent.
    ResumeWithGuidance {
        index: usize,
        text: String,
    },
    /// Change which agent has primary focus.
    SelectFocus {
        index: usize,
    },
    /// Toggle secondary watch pane visibility.
    ToggleWatch,
    /// Change the selected watch target.
    SelectWatch {
        index: usize,
    },
}

#[derive(Debug, Clone)]
pub struct DashboardState {
    pub agents: Vec<AgentPaneState>,
    pub focused: usize,
    pub watch_open: bool,
    /// Index of the watched agent when `watch_open` is true.
    pub watch_target: Option<usize>,
}

impl DashboardState {
    pub fn new(agents: Vec<AgentPaneState>) -> Self {
        Self {
            agents,
            focused: 0,
            watch_open: false,
            watch_target: None,
        }
    }

    fn focus_count(&self) -> usize {
        self.agents.len()
    }

    fn clamp_index(&self, idx: usize) -> usize {
        if self.agents.is_empty() {
            0
        } else {
            idx.min(self.agents.len() - 1)
        }
    }

    fn next_focus(&self) -> usize {
        if self.agents.is_empty() {
            0
        } else {
            (self.focused + 1) % self.agents.len()
        }
    }

    fn prev_focus(&self) -> usize {
        if self.agents.is_empty() {
            0
        } else {
            (self.focused + self.agents.len() - 1) % self.agents.len()
        }
    }

    fn default_watch_target(&self) -> Option<usize> {
        if self.agents.len() < 2 {
            None
        } else {
            // Prefer the next agent after focused, wrapping.
            let idx = (self.focused + 1) % self.agents.len();
            if idx == self.focused { None } else { Some(idx) }
        }
    }

    fn adjust_watch_target_after_focus_change(&mut self) {
        if !self.watch_open {
            return;
        }
        if let Some(w) = self.watch_target {
            if w == self.focused {
                // Move watch to a different agent if it collided with focus.
                self.watch_target = self.default_watch_target();
            }
        } else {
            self.watch_target = self.default_watch_target();
        }
    }

    fn cycle_watch(&mut self, dir: i32) {
        if !self.watch_open || self.agents.len() < 2 {
            return;
        }
        let len = self.agents.len();
        let mut idx = self
            .watch_target
            .unwrap_or_else(|| self.default_watch_target().unwrap_or(0));
        // Ensure we don't land on the focused agent.
        loop {
            let new = if dir > 0 {
                (idx + 1) % len
            } else {
                (idx + len - 1) % len
            };
            if new != self.focused {
                idx = new;
                break;
            }
            // If only two agents, and focused is the only other, keep as-is.
            if len <= 2 {
                break;
            }
            idx = new;
        }
        self.watch_target = Some(idx);
    }

    pub(crate) fn focused_is_composing(&self) -> bool {
        matches!(
            self.agents.get(self.focused).map(|a| a.status),
            Some(AgentStatus::Paused)
        )
    }

    /// Handle a single key event and return the list of emitted actions.
    ///
    /// Notes:
    /// - While the focused agent is paused, regular character keys are captured
    ///   into that agent's compose buffer. Only Enter (submit) and Ctrl+J or
    ///   modified-Enter (newline) are special-cased.
    /// - Navigation keys (Tab/Shift+Tab, number jumps, 'w', arrows for watch)
    ///   are ignored while composing to avoid stealing input.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Vec<DashboardAction> {
        let mut actions = Vec::new();
        if key.kind != KeyEventKind::Press {
            return actions;
        }

        let composing = self.focused_is_composing();

        match key.code {
            // Ctrl+C pauses the focused agent (only when not already paused).
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !composing && self.focus_count() > 0 {
                    if let Some(agent) = self.agents.get_mut(self.focused) {
                        agent.status = AgentStatus::Paused;
                        // Start with an empty composer for clarity; keep any prior buffer.
                    }
                    actions.push(DashboardAction::PauseAgent {
                        index: self.focused,
                    });
                }
            }

            // Enter submits when composing; otherwise ignored here.
            KeyCode::Enter => {
                if composing {
                    // Submit only on unmodified Enter; modified Enter inserts newline.
                    if key.modifiers.is_empty() {
                        if let Some(agent) = self.agents.get_mut(self.focused) {
                            let text = std::mem::take(&mut agent.compose);
                            agent.status = AgentStatus::Running;
                            actions.push(DashboardAction::ResumeWithGuidance {
                                index: self.focused,
                                text,
                            });
                        }
                    } else {
                        if let Some(agent) = self.agents.get_mut(self.focused) {
                            agent.compose.push('\n');
                        }
                    }
                }
            }

            // Ctrl+J inserts a newline while composing.
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if composing {
                    if let Some(agent) = self.agents.get_mut(self.focused) {
                        agent.compose.push('\n');
                    }
                }
            }

            // Tab cycles focus; Shift+Tab (or BackTab) cycles backwards.
            KeyCode::Tab => {
                if !composing && self.focus_count() > 0 {
                    let next = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.prev_focus()
                    } else {
                        self.next_focus()
                    };
                    self.focused = next;
                    self.adjust_watch_target_after_focus_change();
                    actions.push(DashboardAction::SelectFocus { index: next });
                }
            }
            KeyCode::BackTab => {
                if !composing && self.focus_count() > 0 {
                    let prev = self.prev_focus();
                    self.focused = prev;
                    self.adjust_watch_target_after_focus_change();
                    actions.push(DashboardAction::SelectFocus { index: prev });
                }
            }

            // Number keys 1–9 jump to agent index (1-based).
            KeyCode::Char(d @ '1'..='9') if key.modifiers.is_empty() => {
                if self.focus_count() > 0 {
                    let idx = (d as u8 - b'1') as usize;
                    if self.watch_open && !composing {
                        // If watch is open and we're not composing, let numbers target watch.
                        if idx < self.agents.len() && idx != self.focused {
                            self.watch_target = Some(idx);
                            actions.push(DashboardAction::SelectWatch { index: idx });
                        }
                    } else if !composing {
                        let idx = self.clamp_index(idx);
                        self.focused = idx;
                        self.adjust_watch_target_after_focus_change();
                        actions.push(DashboardAction::SelectFocus { index: idx });
                    } else {
                        // composing: treat as input text
                        if let Some(agent) = self.agents.get_mut(self.focused) {
                            agent.compose.push(d);
                        }
                    }
                }
            }

            // 'w' toggles the watch pane when not composing; composing treats as text.
            KeyCode::Char('w') if key.modifiers.is_empty() => {
                if !composing {
                    self.watch_open = !self.watch_open;
                    if self.watch_open {
                        if self.watch_target.is_none() {
                            self.watch_target = self.default_watch_target();
                        }
                    }
                    actions.push(DashboardAction::ToggleWatch);
                } else {
                    if let Some(agent) = self.agents.get_mut(self.focused) {
                        agent.compose.push('w');
                    }
                }
            }

            // Arrow keys adjust watch target when watch is open and not composing.
            KeyCode::Left | KeyCode::Up => {
                if !composing && self.watch_open {
                    self.cycle_watch(-1);
                    if let Some(w) = self.watch_target {
                        actions.push(DashboardAction::SelectWatch { index: w });
                    }
                }
            }
            KeyCode::Right | KeyCode::Down => {
                if !composing && self.watch_open {
                    self.cycle_watch(1);
                    if let Some(w) = self.watch_target {
                        actions.push(DashboardAction::SelectWatch { index: w });
                    }
                }
            }

            // While composing, accept basic text editing.
            KeyCode::Char(c) => {
                if composing {
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                        if let Some(agent) = self.agents.get_mut(self.focused) {
                            agent.compose.push(c);
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if composing {
                    if let Some(agent) = self.agents.get_mut(self.focused) {
                        agent.compose.pop();
                    }
                }
            }
            KeyCode::Esc => {
                // Ignore for now; could cancel compose in a later iteration.
            }
            _ => {}
        }

        actions
    }
}

impl DashboardState {
    /// Render the dashboard with a left sidebar, a main pane for the focused
    /// agent, and an optional watch pane (read‑only).
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        // Clear the full render area each frame to avoid artifacts from
        // shorter lines leaving behind previous content.
        Clear.render_ref(area, buf);
        // Sidebar (fixed width), right content area.
        // Give the sidebar a bit more room. Compute a dynamic width based on the
        // longest task id, with sane bounds, so long names don't crowd the main pane.
        let mut sidebar_w: u16 = 22;
        if !self.agents.is_empty() {
            let max_id = self
                .agents
                .iter()
                .map(|a| a.task_id.len())
                .max()
                .unwrap_or(0);
            // Rough overhead for index + status + spacing + optional " (watch)"
            let needed = (max_id + 14) as u16;
            // Clamp between 24 and 40 cols
            sidebar_w = sidebar_w.max(needed).min(40);
        }

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_w), Constraint::Min(10)])
            .split(area);

        self.render_sidebar(chunks[0], buf);
        self.render_main_and_watch(chunks[1], buf);
    }

    fn render_sidebar(&self, area: Rect, buf: &mut Buffer) {
        // Title
        let title = " Agents ".bold();
        Paragraph::new(Line::from(vec![title]))
            .render_ref(Rect::new(area.x, area.y, area.width, 1), buf);

        if self.agents.is_empty() {
            let body = Paragraph::new("No agents".dim());
            body.render_ref(area.inner(Margin::new(1, 1)), buf);
            return;
        }

        // List agents with index and status. Highlight focused, show watch marker.
        let mut y = area.y + 1;
        for (i, a) in self.agents.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let idx = i + 1;
            let is_focus = i == self.focused;
            let is_watch = self.watch_open && self.watch_target == Some(i);

            // Follow styles.md: avoid yellow/blue; prefer cyan/green/red.
            let status_marker: Span<'static> = match a.status {
                AgentStatus::Running => Span::raw("R"),
                AgentStatus::Paused => "P".cyan(),
                AgentStatus::Done => "D".green(),
                AgentStatus::Error => "E".red(),
            };

            let mut spans: Vec<Span<'static>> = vec![
                Span::raw(format!("{:>2} ", idx)).dim(),
                Span::raw("["),
                status_marker,
                Span::raw("] "),
                Span::raw(a.task_id.clone()),
            ];
            if is_watch {
                spans.push(Span::raw(" "));
                spans.push("(watch)".dim());
            }

            let mut line = Line::from(spans);
            if is_focus {
                line = line.bold();
            }
            Paragraph::new(line).render_ref(
                Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1),
                buf,
            );
            y += 1;
        }
    }

    fn render_main_and_watch(&self, area: Rect, buf: &mut Buffer) {
        if self.agents.is_empty() {
            Paragraph::new("No agents to display".dim())
                .render_ref(area.inner(Margin::new(1, 1)), buf);
            return;
        }

        if self.watch_open {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(area);
            let main_rect = chunks[0];
            let watch_rect = chunks[1];
            self.render_main(main_rect, buf);
            self.render_watch(watch_rect, buf);
        } else {
            self.render_main(area, buf);
        }
    }

    fn render_main(&self, area: Rect, buf: &mut Buffer) {
        let Some(agent) = self.agents.get(self.focused) else {
            return;
        };

        // Header: Task ID and status
        // Follow styles.md: avoid yellow/blue; prefer cyan/green/red.
        let status_text = match agent.status {
            AgentStatus::Running => Span::raw("Running"),
            AgentStatus::Paused => "Paused".cyan(),
            AgentStatus::Done => "Done".green(),
            AgentStatus::Error => "Error".red(),
        };
        let header = Line::from(vec![
            Span::raw("Task "),
            agent.task_id.clone().bold(),
            Span::raw(" — "),
            status_text,
        ]);
        Paragraph::new(header).render_ref(Rect::new(area.x, area.y, area.width, 1), buf);

        // Optional banner just under header
        let mut y = area.y + 1;
        if let Some(line) = &agent.banner {
            Paragraph::new(line.clone()).render_ref(Rect::new(area.x, y, area.width, 1), buf);
            y += 1;
        }

        // Optional bottom composer height
        let composer_h = if matches!(agent.status, AgentStatus::Paused) {
            3
        } else {
            0
        };
        let logs_rect = Rect::new(
            area.x,
            y,
            area.width,
            area.height.saturating_sub(y - area.y + composer_h),
        );
        self.render_logs(agent, logs_rect, buf);

        if composer_h > 0 {
            self.render_composer(
                agent,
                Rect::new(area.x, logs_rect.bottom(), area.width, composer_h),
                buf,
            );
        }
    }

    fn render_watch(&self, area: Rect, buf: &mut Buffer) {
        let Some(watch_idx) = self.watch_target else {
            return;
        };
        if let Some(agent) = self.agents.get(watch_idx) {
            let header = Line::from(vec![Span::raw("Watch "), agent.task_id.clone().dim()]);
            Paragraph::new(header).render_ref(Rect::new(area.x, area.y, area.width, 1), buf);

            // Optional banner below header for the watch pane
            let mut y = area.y + 1;
            if let Some(line) = &agent.banner {
                Paragraph::new(line.clone()).render_ref(Rect::new(area.x, y, area.width, 1), buf);
                y += 1;
            }
            let logs_rect = Rect::new(
                area.x,
                y,
                area.width,
                area.height.saturating_sub(y - area.y),
            );
            self.render_logs(agent, logs_rect, buf);
        }
    }

    fn render_logs(&self, agent: &AgentPaneState, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let total = agent.log.len();
        let view_h = area.height as usize;
        let max_start = total.saturating_sub(view_h);
        let start = max_start.saturating_sub(agent.scrollback);
        let end = (start + view_h).min(total);
        let slice = &agent.log[start..end];
        Paragraph::new(slice.to_vec()).render_ref(area, buf);
    }

    fn render_composer(&self, agent: &AgentPaneState, area: Rect, buf: &mut Buffer) {
        // Simple header + content lines.
        // Label the composer with the Task ID for clarity.
        let header = Line::from(vec![
            "Guidance for ".cyan(),
            agent.task_id.clone().bold(),
            " (Enter to send)".cyan(),
        ]);
        Paragraph::new(header).render_ref(Rect::new(area.x, area.y, area.width, 1), buf);

        let body_rect = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
        if body_rect.height == 0 {
            return;
        }
        let lines: Vec<Line<'static>> = if agent.compose.is_empty() {
            vec![Line::from("")]
        } else {
            agent
                .compose
                .lines()
                .map(|l| Line::from(l.to_string()))
                .collect()
        };
        Paragraph::new(lines).render_ref(body_rect.inner(Margin::new(1, 0)), buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn ev(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn evm(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    fn make_dash(n: usize) -> DashboardState {
        let agents = (0..n)
            .map(|i| AgentPaneState::new(format!("A{i}")))
            .collect();
        DashboardState::new(agents)
    }

    #[test]
    fn cycles_focus_with_tab_and_backtab() {
        let mut d = make_dash(3);
        assert_eq!(d.focused, 0);
        let acts = d.handle_key_event(ev(KeyCode::Tab));
        assert_eq!(d.focused, 1);
        assert_eq!(acts, vec![DashboardAction::SelectFocus { index: 1 }]);
        let acts = d.handle_key_event(evm(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(d.focused, 0);
        assert_eq!(acts, vec![DashboardAction::SelectFocus { index: 0 }]);
        let acts = d.handle_key_event(ev(KeyCode::BackTab));
        assert_eq!(d.focused, 2);
        assert_eq!(acts, vec![DashboardAction::SelectFocus { index: 2 }]);
    }

    #[test]
    fn jump_by_number_keys() {
        let mut d = make_dash(4);
        let acts = d.handle_key_event(ev(KeyCode::Char('3')));
        assert_eq!(d.focused, 2);
        assert_eq!(acts, vec![DashboardAction::SelectFocus { index: 2 }]);
    }

    #[test]
    fn toggle_watch_and_change_target() {
        let mut d = make_dash(3);
        assert_eq!(d.watch_open, false);
        let acts = d.handle_key_event(ev(KeyCode::Char('w')));
        assert_eq!(d.watch_open, true);
        assert_eq!(acts, vec![DashboardAction::ToggleWatch]);
        assert!(d.watch_target.is_some());

        // Move watch with right/left
        let _ = d.handle_key_event(ev(KeyCode::Right));
        let after_right = d.watch_target;
        let _ = d.handle_key_event(ev(KeyCode::Left));
        assert_ne!(after_right, d.watch_target);
    }

    #[test]
    fn pause_compose_and_submit() {
        let mut d = make_dash(2);
        // Ctrl+C pauses
        let acts = d.handle_key_event(evm(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(d.agents[0].status, AgentStatus::Paused);
        assert_eq!(acts, vec![DashboardAction::PauseAgent { index: 0 }]);

        // Typing goes to compose, digits are not focus jumps
        let _ = d.handle_key_event(ev(KeyCode::Char('H')));
        let _ = d.handle_key_event(ev(KeyCode::Char('i')));
        let _ = d.handle_key_event(ev(KeyCode::Char('1')));
        let _ = d.handle_key_event(evm(KeyCode::Enter, KeyModifiers::CONTROL)); // newline
        assert_eq!(d.agents[0].compose, "Hi1\n");

        // Plain Enter submits and resumes
        let acts = d.handle_key_event(ev(KeyCode::Enter));
        assert_eq!(d.agents[0].status, AgentStatus::Running);
        assert_eq!(d.agents[0].compose, "");
        assert_eq!(
            acts,
            vec![DashboardAction::ResumeWithGuidance {
                index: 0,
                text: "Hi1\n".into()
            }]
        );
    }

    #[test]
    fn number_selects_watch_when_open_and_not_composing() {
        let mut d = make_dash(4);
        // open watch
        let _ = d.handle_key_event(ev(KeyCode::Char('w')));
        // choose 3rd as watch
        let acts = d.handle_key_event(ev(KeyCode::Char('3')));
        assert!(
            acts.iter()
                .any(|a| matches!(a, DashboardAction::SelectWatch { index: 2 }))
        );

        // Pause composing; numbers now go to input
        let _ = d.handle_key_event(evm(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let _ = d.handle_key_event(ev(KeyCode::Char('4')));
        assert_eq!(d.agents[d.focused].compose, "4");
    }

    fn buf_to_string(buf: &Buffer, area: Rect) -> String {
        let mut s = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                s.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            s.push('\n');
        }
        s
    }

    fn make_agent(id: &str, status: AgentStatus, lines: &[&str]) -> AgentPaneState {
        let mut a = AgentPaneState::new(id);
        a.status = status;
        a.log = lines.iter().map(|s| Line::from((*s).to_string())).collect();
        a
    }

    #[test]
    fn snapshot_idle_dashboard() {
        let d = DashboardState::new(vec![]);
        let area = Rect::new(0, 0, 52, 12);
        let mut buf = Buffer::empty(area);
        d.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }

    #[test]
    fn snapshot_single_running() {
        let lines = [
            "boot",
            "starting agent",
            "doing work",
            "producing output",
            "done",
        ];
        let a0 = make_agent("T-1", AgentStatus::Running, &lines);
        let d = DashboardState::new(vec![a0]);
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        d.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }

    #[test]
    fn snapshot_multiple_with_watch() {
        let mut d = DashboardState::new(vec![
            make_agent(
                "A1",
                AgentStatus::Running,
                &["a1 line1", "a1 line2", "a1 last"],
            ),
            make_agent("A2", AgentStatus::Running, &["a2: hello", "more text"]),
            make_agent("A3", AgentStatus::Done, &["finished"]),
        ]);
        // focus second agent
        d.focused = 1;
        // open watch on first agent
        d.watch_open = true;
        d.watch_target = Some(0);

        let area = Rect::new(0, 0, 64, 14);
        let mut buf = Buffer::empty(area);
        d.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }

    #[test]
    fn snapshot_paused_with_input() {
        let mut a0 = make_agent(
            "Agent-X",
            AgentStatus::Paused,
            &["log 1", "log 2", "log 3", "log 4"],
        );
        a0.compose = "Consider edge cases\nAdd tests".into();
        let d = DashboardState::new(vec![a0]);
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        d.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }

    #[test]
    fn snapshot_error_state() {
        let a0 = make_agent("A1", AgentStatus::Error, &["failed to start"]);
        let a1 = make_agent("A2", AgentStatus::Running, &["ok"]);
        let mut d = DashboardState::new(vec![a0, a1]);
        d.focused = 0;
        let area = Rect::new(0, 0, 56, 10);
        let mut buf = Buffer::empty(area);
        d.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }

    #[test]
    fn snapshot_route_banners() {
        let mut a0 = make_agent("A1", AgentStatus::Running, &["working", "log 2", "log 3"]);
        a0.banner = Some(Line::from(vec![" Routed to A2 ".to_string().cyan()]));
        let mut a1 = make_agent("A2", AgentStatus::Running, &["hello", "world"]);
        a1.banner = Some(Line::from(vec![" Routed from A1 ".to_string().cyan()]));
        let mut d = DashboardState::new(vec![a0, a1]);
        d.focused = 0;
        d.watch_open = true;
        d.watch_target = Some(1);

        let area = Rect::new(0, 0, 64, 12);
        let mut buf = Buffer::empty(area);
        d.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }
}
