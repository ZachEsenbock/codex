use std::collections::HashMap;

use crate::custom_terminal::Frame;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crate::tui::{self};
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::dashboard::AgentPaneState;
use crate::dashboard::AgentStatus;
use crate::dashboard::DashboardAction;
use crate::dashboard::DashboardState;

/// Public re-exports for consumers to avoid reaching into the private `dashboard` module.
pub use crate::dashboard::AgentStatus as DashboardAgentStatus;

/// Update messages that the dashboard consumes to keep per-agent state in sync with
/// the runner streams. These are intentionally minimal and incremental.
#[derive(Debug, Clone)]
pub enum MultiAgentUpdate {
    /// Replace the full snapshot for an agent (log, status, optional banner).
    Snapshot {
        task_id: String,
        log: Vec<Line<'static>>,
        status: AgentStatus,
        banner: Option<Line<'static>>,
    },
    /// Append new log lines for an agent.
    AppendLines {
        task_id: String,
        lines: Vec<Line<'static>>,
    },
    /// Update the status for an agent.
    SetStatus {
        task_id: String,
        status: AgentStatus,
    },
    /// Set or clear a one-line banner for an agent.
    SetBanner {
        task_id: String,
        banner: Option<Line<'static>>,
    },
}

/// Callback hooks for controlling agents. All hooks are optional; if not provided,
/// the dashboard operates as a read-only viewer.
#[derive(Default, Clone)]
pub struct MultiAgentCallbacks {
    pub on_pause: Option<std::sync::Arc<dyn Fn(String) + Send + Sync + 'static>>,
    pub on_resume: Option<std::sync::Arc<dyn Fn(String) + Send + Sync + 'static>>,
    pub on_inject: Option<std::sync::Arc<dyn Fn(String, String) + Send + Sync + 'static>>,
    pub on_abort_all: Option<std::sync::Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl MultiAgentCallbacks {
    pub fn with_pause(mut self, f: impl Fn(String) + Send + Sync + 'static) -> Self {
        self.on_pause = Some(std::sync::Arc::new(f));
        self
    }
    pub fn with_resume(mut self, f: impl Fn(String) + Send + Sync + 'static) -> Self {
        self.on_resume = Some(std::sync::Arc::new(f));
        self
    }
    pub fn with_inject(mut self, f: impl Fn(String, String) + Send + Sync + 'static) -> Self {
        self.on_inject = Some(std::sync::Arc::new(f));
        self
    }
    pub fn with_abort(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_abort_all = Some(std::sync::Arc::new(f));
        self
    }
}

/// A ready-to-use Multi‑Agent dashboard that integrates input handling, focus,
/// watch split, and streaming updates.
pub struct MultiAgentDashboard {
    state: DashboardState,
    id_to_index: HashMap<String, usize>,
    tui: Tui,
    frame: FrameRequester,
    callbacks: MultiAgentCallbacks,
    exit_requested: bool,
}

impl MultiAgentDashboard {
    /// Construct a dashboard with a fixed list of agent Task IDs, pre-populated in Running state.
    pub fn new(agent_ids: Vec<String>, callbacks: MultiAgentCallbacks) -> std::io::Result<Self> {
        let mut agents: Vec<AgentPaneState> = Vec::with_capacity(agent_ids.len());
        let mut id_to_index: HashMap<String, usize> = HashMap::new();
        for (idx, id) in agent_ids.into_iter().enumerate() {
            id_to_index.insert(id.clone(), idx);
            agents.push(AgentPaneState::new(id));
        }
        let state = DashboardState::new(agents);

        let terminal = tui::init()?;
        let mut tui = Tui::new(terminal);
        // Use alt screen to avoid mixing with the taskflow stdout.
        let _ = tui.enter_alt_screen();
        let frame = tui.frame_requester();

        Ok(Self {
            state,
            id_to_index,
            tui,
            frame,
            callbacks,
            exit_requested: false,
        })
    }

    /// Drive the event loop until the sender end of `updates_rx` is dropped and all
    /// agents reach a terminal state (Done or Error), or until the process is externally
    /// terminated by the caller.
    pub async fn run(
        mut self,
        mut updates_rx: tokio::sync::mpsc::UnboundedReceiver<MultiAgentUpdate>,
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
                        TuiEvent::Paste(_) => { /* ignore paste in this dashboard */ }
                        TuiEvent::AttachImage { .. } => { /* ignore */ }
                        TuiEvent::Draw => self.draw()?,
                    }
                    if self.exit_requested {
                        break;
                    }
                }
                update = updates_rx.recv() => {
                    match update {
                        Some(msg) => {
                            self.apply_update(msg);
                            self.frame.schedule_frame();
                        }
                        None => {
                            // Channel closed — if everyone is done (or error) and not composing, we can exit.
                            if self.all_agents_terminal() && !self.state.focused_is_composing() {
                                break;
                            } else {
                                // Still allow user to navigate/view until all done; schedule redraws on input only.
                            }
                        }
                    }
                }
            }
        }

        // Leave alt screen before returning.
        let _ = self.tui.leave_alt_screen();
        tui::restore()?;
        Ok(())
    }

    fn draw(&mut self) -> std::io::Result<()> {
        // Compute target height from current terminal size.
        let height = self.tui.terminal.size()?.height;
        self.tui.draw(height, |f: &mut Frame| {
            let area: Rect = f.area();
            // Render into the underlying buffer directly using our dashboard renderer.
            let buf: &mut Buffer = f.buffer_mut();
            self.state.render(area, buf);
        })
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl+Q aborts the entire run.
        if key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
            && matches!(key.code, crossterm::event::KeyCode::Char('q'))
        {
            if let Some(cb) = &self.callbacks.on_abort_all {
                cb();
            }
            self.exit_requested = true;
            return;
        }
        // Route to dashboard handler and translate emitted actions into callbacks.
        let actions = self.state.handle_key_event(key);
        for a in actions {
            match a {
                DashboardAction::PauseAgent { index } => {
                    if let Some(cb) = &self.callbacks.on_pause
                        && let Some(agent) = self.state.agents.get(index) {
                            cb(agent.task_id.clone());
                        }
                }
                DashboardAction::ResumeWithGuidance { index, text } => {
                    // Send guidance first, then resume.
                    if let Some(agent) = self.state.agents.get(index) {
                        if let Some(on_inject) = &self.callbacks.on_inject {
                            on_inject(agent.task_id.clone(), text.clone());
                        }
                        if let Some(on_resume) = &self.callbacks.on_resume {
                            on_resume(agent.task_id.clone());
                        }
                    }
                }
                DashboardAction::SelectFocus { .. }
                | DashboardAction::ToggleWatch
                | DashboardAction::SelectWatch { .. } => {
                    // No external callbacks needed.
                }
            }
        }

        // Schedule a frame after handling input.
        self.frame.schedule_frame();
    }

    fn apply_update(&mut self, update: MultiAgentUpdate) {
        match update {
            MultiAgentUpdate::Snapshot {
                task_id,
                log,
                status,
                banner,
            } => {
                if let Some(&idx) = self.id_to_index.get(&task_id)
                    && let Some(a) = self.state.agents.get_mut(idx) {
                        a.log = log;
                        a.status = status;
                        a.banner = banner;
                    }
            }
            MultiAgentUpdate::AppendLines { task_id, lines } => {
                if let Some(&idx) = self.id_to_index.get(&task_id)
                    && let Some(a) = self.state.agents.get_mut(idx) {
                        a.log.extend(lines);
                    }
            }
            MultiAgentUpdate::SetStatus { task_id, status } => {
                if let Some(&idx) = self.id_to_index.get(&task_id)
                    && let Some(a) = self.state.agents.get_mut(idx) {
                        a.status = status;
                    }
            }
            MultiAgentUpdate::SetBanner { task_id, banner } => {
                if let Some(&idx) = self.id_to_index.get(&task_id)
                    && let Some(a) = self.state.agents.get_mut(idx) {
                        a.banner = banner;
                    }
            }
        }
    }

    fn all_agents_terminal(&self) -> bool {
        self.state.agents.iter().all(|a| match a.status {
            AgentStatus::Done | AgentStatus::Error => true,
            AgentStatus::Running | AgentStatus::Paused => false,
        })
    }
}
