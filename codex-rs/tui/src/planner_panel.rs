use crate::custom_terminal::Frame;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;

/// Minimal planner status for header coloring and terminal checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerStatus {
    Running,
    Done,
    Error,
}

/// Updates to drive the planner overlay, mirroring the multi‑agent API shape.
#[derive(Debug, Clone)]
pub enum PlannerUpdate {
    /// Replace the full snapshot: log, status, and optional banner.
    Snapshot {
        log: Vec<Line<'static>>,
        status: PlannerStatus,
        banner: Option<Line<'static>>,
    },
    /// Append new log lines.
    AppendLines { lines: Vec<Line<'static>> },
    /// Update the planner status.
    SetStatus { status: PlannerStatus },
    /// Set or clear a one-line banner (error/warning/info).
    SetBanner { banner: Option<Line<'static>> },
}

/// Internal state for the single-pane planner overlay.
#[derive(Debug, Clone)]
pub struct PlannerPanelState {
    pub log: Vec<Line<'static>>,
    pub scrollback: usize,
    pub banner: Option<Line<'static>>,
    pub status: PlannerStatus,
}

impl Default for PlannerPanelState {
    fn default() -> Self {
        Self {
            log: Vec::new(),
            scrollback: 0,
            banner: None,
            status: PlannerStatus::Running,
        }
    }
}

impl PlannerPanelState {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        // Clear the full render area each frame to avoid artifacts from shorter content.
        Clear.render_ref(area, buf);

        // Header line: "Planning…" with cyan color (or red on error, green on done).
        let status_span: Span = match self.status {
            PlannerStatus::Running => "Planning…".cyan(),
            PlannerStatus::Done => "Planning complete".green(),
            PlannerStatus::Error => "Planning error".red(),
        };
        Paragraph::new(Line::from(status_span))
            .render_ref(Rect::new(area.x, area.y, area.width, 1), buf);

        // Optional banner line directly under header.
        let mut y = area.y + 1;
        if let Some(line) = &self.banner {
            Paragraph::new(line.clone()).render_ref(Rect::new(area.x, y, area.width, 1), buf);
            y += 1;
        }

        // Log area fills the remainder; show a scrollback view from the bottom.
        let logs_rect = Rect::new(
            area.x,
            y,
            area.width,
            area.height.saturating_sub(y - area.y),
        );
        if logs_rect.height == 0 {
            return;
        }
        let total = self.log.len();
        let view_h = logs_rect.height as usize;
        let max_start = total.saturating_sub(view_h);
        let start = max_start.saturating_sub(self.scrollback);
        let end = (start + view_h).min(total);
        let slice = &self.log[start..end];
        Paragraph::new(slice.to_vec()).render_ref(logs_rect, buf);
    }

    fn apply_update(&mut self, update: PlannerUpdate) {
        match update {
            PlannerUpdate::Snapshot {
                log,
                status,
                banner,
            } => {
                self.log = log;
                self.status = status;
                self.banner = banner;
            }
            PlannerUpdate::AppendLines { lines } => {
                self.log.extend(lines);
            }
            PlannerUpdate::SetStatus { status } => {
                self.status = status;
            }
            PlannerUpdate::SetBanner { banner } => {
                self.banner = banner;
            }
        }
    }
}

/// Minimal wrapper to integrate with terminal events and streaming updates, similar to
/// MultiAgentDashboard but with a single pane and no composer.
pub struct PlannerDashboard {
    state: PlannerPanelState,
    tui: Tui,
    frame: FrameRequester,
}

impl PlannerDashboard {
    pub fn new() -> std::io::Result<Self> {
        let state = PlannerPanelState::default();
        let terminal = crate::tui::init()?;
        let mut tui = Tui::new(terminal);
        // Use alt screen to avoid mixing with stdout.
        let _ = tui.enter_alt_screen();
        let frame = tui.frame_requester();
        Ok(Self { state, tui, frame })
    }

    pub async fn run(
        mut self,
        mut updates_rx: tokio::sync::mpsc::UnboundedReceiver<PlannerUpdate>,
    ) -> std::io::Result<()> {
        use tokio_stream::StreamExt as _;
        let mut events = self.tui.event_stream();

        // Initial draw
        self.frame.schedule_frame();

        loop {
            tokio::select! {
                biased;
                Some(evt) = events.next() => {
                    match evt {
                        TuiEvent::Key(key) => self.on_key(key),
                        TuiEvent::Paste(_) => { /* ignore */ }
                        TuiEvent::AttachImage { .. } => { /* ignore */ }
                        TuiEvent::Mouse(_) => { /* ignore */ }
                        TuiEvent::Draw => self.draw()?,
                    }
                }
                update = updates_rx.recv() => {
                    match update {
                        Some(msg) => {
                            self.apply_update(msg);
                            self.frame.schedule_frame();
                        }
                        None => {
                            // Channel closed; planner is done. Exit overlay.
                            break;
                        }
                    }
                }
            }
        }

        // Leave alt screen before returning.
        let _ = self.tui.leave_alt_screen();
        crate::tui::restore()?;
        Ok(())
    }

    fn draw(&mut self) -> std::io::Result<()> {
        let height = self.tui.terminal.size()?.height;
        self.tui.draw(height, |f: &mut Frame| {
            let area: Rect = f.area();
            let buf: &mut Buffer = f.buffer_mut();
            self.state.render(area, buf);
        })
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return;
        }
        match key.code {
            KeyCode::PageUp => {
                let h = self.tui.terminal.viewport_area.height.saturating_sub(1) as usize; // approximate content height
                self.state.scrollback = self.state.scrollback.saturating_add(h);
            }
            KeyCode::PageDown => {
                let h = self.tui.terminal.viewport_area.height.saturating_sub(1) as usize;
                self.state.scrollback = self.state.scrollback.saturating_sub(h);
            }
            KeyCode::Home => {
                // Jump to top of the buffer.
                let total = self.state.log.len();
                let view_h = self.tui.terminal.viewport_area.height.saturating_sub(1) as usize;
                let max_start = total.saturating_sub(view_h);
                self.state.scrollback = max_start;
            }
            KeyCode::End => {
                // Jump to bottom (no scrollback).
                self.state.scrollback = 0;
            }
            KeyCode::Esc => {
                // Explicitly ignored.
            }
            _ => {}
        }
        self.frame.schedule_frame();
    }

    fn apply_update(&mut self, update: PlannerUpdate) {
        self.state.apply_update(update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

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

    #[test]
    fn snapshot_empty_planner() {
        let state = PlannerPanelState::default();
        let area = Rect::new(0, 0, 48, 8);
        let mut buf = Buffer::empty(area);
        state.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }

    #[test]
    fn snapshot_streaming_lines() {
        let mut state = PlannerPanelState::default();
        state.banner = Some(Line::from("Auto-planning…".to_string().cyan()));
        // Simulate streaming by appending lines in chunks
        state.apply_update(PlannerUpdate::AppendLines {
            lines: vec![
                Line::from("boot"),
                Line::from("resolving repo context"),
                Line::from("parsing task file"),
            ],
        });
        state.apply_update(PlannerUpdate::AppendLines {
            lines: vec![
                Line::from("building plan"),
                Line::from("…step 1"),
                Line::from("…step 2"),
            ],
        });

        let area = Rect::new(0, 0, 52, 10);
        let mut buf = Buffer::empty(area);
        state.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }

    #[test]
    fn snapshot_error_banner() {
        let mut state = PlannerPanelState::default();
        state.status = PlannerStatus::Error;
        state.banner = Some(Line::from(vec![
            " Error: missing prompt file ".to_string().red(),
        ]));
        state.log = vec![
            Line::from("starting planner"),
            Line::from("failed to read input"),
        ];

        let area = Rect::new(0, 0, 56, 8);
        let mut buf = Buffer::empty(area);
        state.render(area, &mut buf);
        assert_snapshot!(buf_to_string(&buf, area));
    }
}
