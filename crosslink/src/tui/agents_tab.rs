use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use super::{Tab, TabAction};

/// Background color for highlighted/selected rows (256-color palette, dark gray).
const HIGHLIGHT_BG: Color = Color::Indexed(236);

use crate::events;
use crate::locks::{Heartbeat, Lock, LocksFile};
use crate::signing::AllowedSignerEntry;
use crate::sync::SyncManager;

/// Which sub-view is active within the Agents tab.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ViewMode {
    /// Main view: merged agent activity table.
    Agents,
    /// Lock-focused view: all locks + stale detection.
    Locks,
    /// Trust store view: allowed signers list.
    Trust,
    /// Detail view for a specific agent.
    Detail,
}

/// A row in the merged agents table.
struct AgentRow {
    agent_id: String,
    active_issue: Option<i64>,
    lock_issue: Option<i64>,
    branch: Option<String>,
    heartbeat_ago: Option<String>,
    is_stale: bool,
    machine_id: Option<String>,
}

/// A row in the locks table.
struct LockRow {
    issue_id: i64,
    agent_id: String,
    branch: Option<String>,
    claimed_ago: String,
    is_stale: bool,
}

/// Detail for a selected agent.
struct AgentDetail {
    agent_id: String,
    machine_id: Option<String>,
    heartbeat: Option<Heartbeat>,
    locks: Vec<(i64, Lock)>,
    recent_events: Vec<events::EventEnvelope>,
    is_stale: bool,
}

/// Data payload sent from background refresh thread.
struct AgentsLoadResult {
    agents: Vec<AgentRow>,
    lock_rows: Vec<LockRow>,
    trust_entries: Vec<AllowedSignerEntry>,
    status_msg: String,
    error_msg: Option<String>,
}

/// The Agents tab — live coordination dashboard.
pub struct AgentsTab {
    crosslink_dir: PathBuf,
    view_mode: ViewMode,
    /// Merged agent rows (agents view).
    agents: Vec<AgentRow>,
    selected: usize,
    /// Lock rows (locks view).
    lock_rows: Vec<LockRow>,
    lock_selected: usize,
    /// Trust entries (trust view).
    trust_entries: Vec<AllowedSignerEntry>,
    trust_selected: usize,
    /// Detail for a specific agent.
    detail: Option<AgentDetail>,
    detail_scroll: usize,
    /// Status message (e.g. "Last sync: 12s ago").
    status_msg: String,
    /// Error message if data load failed.
    error_msg: Option<String>,
    /// Whether a background load is in progress.
    loading: bool,
    /// Receiver for background load results.
    load_rx: Option<mpsc::Receiver<AgentsLoadResult>>,
    /// TableState for agents view scroll-to-follow.
    agents_table_state: RefCell<TableState>,
    /// TableState for locks view scroll-to-follow.
    locks_table_state: RefCell<TableState>,
    /// TableState for trust view scroll-to-follow.
    trust_table_state: RefCell<TableState>,
}

impl AgentsTab {
    pub fn new(crosslink_dir: &Path) -> Self {
        let mut tab = AgentsTab {
            crosslink_dir: crosslink_dir.to_path_buf(),
            view_mode: ViewMode::Agents,
            agents: Vec::new(),
            selected: 0,
            lock_rows: Vec::new(),
            lock_selected: 0,
            trust_entries: Vec::new(),
            trust_selected: 0,
            detail: None,
            detail_scroll: 0,
            status_msg: String::new(),
            error_msg: None,
            loading: false,
            load_rx: None,
            agents_table_state: RefCell::new(TableState::default()),
            locks_table_state: RefCell::new(TableState::default()),
            trust_table_state: RefCell::new(TableState::default()),
        };
        tab.start_background_refresh();
        tab
    }

    /// Spawn a background thread to load agents/locks/trust data without blocking the UI.
    fn start_background_refresh(&mut self) {
        self.loading = true;
        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        let crosslink_dir = self.crosslink_dir.clone();

        std::thread::spawn(move || {
            let result = load_agents_data(&crosslink_dir);
            let _ = tx.send(result);
        });
    }

    /// Apply a completed background load result to the tab state.
    fn apply_load_result(&mut self, result: AgentsLoadResult) {
        self.loading = false;
        self.agents = result.agents;
        self.lock_rows = result.lock_rows;
        self.trust_entries = result.trust_entries;
        self.status_msg = result.status_msg;
        self.error_msg = result.error_msg;

        // Clamp selections
        if self.selected >= self.agents.len() && !self.agents.is_empty() {
            self.selected = self.agents.len() - 1;
        }
        if self.lock_selected >= self.lock_rows.len() && !self.lock_rows.is_empty() {
            self.lock_selected = self.lock_rows.len() - 1;
        }
        if self.trust_selected >= self.trust_entries.len() && !self.trust_entries.is_empty() {
            self.trust_selected = self.trust_entries.len() - 1;
        }
    }

    fn load_detail(&mut self, agent_id: &str) {
        let sync = match SyncManager::new(&self.crosslink_dir) {
            Ok(s) => s,
            Err(_) => return,
        };

        // Find heartbeat for this agent
        let heartbeats = sync.read_heartbeats().unwrap_or_default();
        let heartbeat = heartbeats.into_iter().find(|h| h.agent_id == agent_id);

        // Find locks held by this agent
        let locks = sync
            .read_locks_auto()
            .unwrap_or_else(|_| LocksFile::empty());
        let agent_locks: Vec<(i64, Lock)> = locks
            .locks
            .into_iter()
            .filter(|(_, lock)| lock.agent_id == agent_id)
            .map(|(id_str, lock)| (id_str.parse::<i64>().unwrap_or(0), lock))
            .collect();

        // Read recent events for this agent
        let events_path = sync
            .cache_path()
            .join("agents")
            .join(agent_id)
            .join("events.log");
        let all_events = events::read_events(&events_path).unwrap_or_default();
        // Take last 20 events
        let recent_events: Vec<events::EventEnvelope> =
            all_events.into_iter().rev().take(20).collect();

        // Check stale status
        let stale = sync.find_stale_locks().unwrap_or_default();
        let is_stale = stale.iter().any(|(_, a)| a == agent_id);

        let machine_id = heartbeat.as_ref().map(|h| h.machine_id.clone());

        self.detail = Some(AgentDetail {
            agent_id: agent_id.to_string(),
            machine_id,
            heartbeat,
            locks: agent_locks,
            recent_events,
            is_stale,
        });
        self.detail_scroll = 0;
    }

    // ── Key handlers ──────────────────────────────────────────────────

    fn handle_agents_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.agents.is_empty() {
                    self.selected = (self.selected + 1).min(self.agents.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::Enter => {
                if let Some(agent) = self.agents.get(self.selected) {
                    let agent_id = agent.agent_id.clone();
                    self.load_detail(&agent_id);
                    self.view_mode = ViewMode::Detail;
                }
                TabAction::Consumed
            }
            KeyCode::Char('v') => {
                self.view_mode = ViewMode::Locks;
                TabAction::Consumed
            }
            KeyCode::Char('r') => {
                self.start_background_refresh();
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    fn handle_locks_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.lock_rows.is_empty() {
                    self.lock_selected = (self.lock_selected + 1).min(self.lock_rows.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.lock_selected = self.lock_selected.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::Char('v') => {
                self.view_mode = ViewMode::Trust;
                TabAction::Consumed
            }
            KeyCode::Char('r') => {
                self.start_background_refresh();
                TabAction::Consumed
            }
            KeyCode::Esc => {
                self.view_mode = ViewMode::Agents;
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    fn handle_trust_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.trust_entries.is_empty() {
                    self.trust_selected =
                        (self.trust_selected + 1).min(self.trust_entries.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.trust_selected = self.trust_selected.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::Char('v') => {
                self.view_mode = ViewMode::Agents;
                TabAction::Consumed
            }
            KeyCode::Char('r') => {
                self.start_background_refresh();
                TabAction::Consumed
            }
            KeyCode::Esc => {
                self.view_mode = ViewMode::Agents;
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.view_mode = ViewMode::Agents;
                self.detail = None;
                TabAction::Consumed
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
                TabAction::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(10);
                TabAction::Consumed
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    // ── Renderers ─────────────────────────────────────────────────────

    fn render_agents(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Min(0),    // Table
                Constraint::Length(1), // Context keys
            ])
            .split(area);

        // Header
        let header = Line::from(vec![
            Span::styled(
                " Agents & Locks",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", self.status_msg),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        // Error or empty state
        if let Some(ref err) = self.error_msg {
            let msg = Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(err, Style::default().fg(Color::Red)),
            ]));
            frame.render_widget(msg, chunks[1]);
            return;
        }

        if self.loading && self.agents.is_empty() {
            let msg = Paragraph::new(Line::from(Span::styled(
                "  Loading agents...",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(msg, chunks[1]);
        } else if self.agents.is_empty() {
            let msg = Paragraph::new(Line::from(Span::styled(
                "  No agents detected. Hub may not be initialized.",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(msg, chunks[1]);
        } else {
            // Table
            let header_row = Row::new(vec!["Agent", "Active", "Lock", "Branch", "Heartbeat"])
                .style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );

            let rows: Vec<Row> = self
                .agents
                .iter()
                .map(|agent| {
                    let style = if agent.is_stale {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default()
                    };

                    let active = agent
                        .active_issue
                        .map(|id| format!("#{id}"))
                        .unwrap_or_else(|| "—".to_string());

                    let lock = agent
                        .lock_issue
                        .map(|id| format!("● #{id}"))
                        .unwrap_or_else(|| "—".to_string());

                    let branch = truncate_str(agent.branch.as_deref().unwrap_or("—"), 22);

                    let heartbeat = agent.heartbeat_ago.as_deref().unwrap_or("—").to_string();

                    Row::new(vec![
                        truncate_str(&agent.agent_id, 35),
                        active,
                        lock,
                        branch,
                        heartbeat,
                    ])
                    .style(style)
                })
                .collect();

            let table = Table::new(
                rows,
                [
                    Constraint::Min(20),    // Agent
                    Constraint::Length(8),  // Active
                    Constraint::Length(10), // Lock
                    Constraint::Length(24), // Branch
                    Constraint::Length(12), // Heartbeat
                ],
            )
            .header(header_row)
            .block(Block::default().borders(Borders::NONE))
            .row_highlight_style(Style::default().bg(HIGHLIGHT_BG));

            let mut state = self.agents_table_state.borrow_mut();
            state.select(Some(self.selected));
            frame.render_stateful_widget(table, chunks[1], &mut state);
        }

        // Context keys
        let keys = Line::from(vec![
            Span::styled("↑↓", Style::default().fg(Color::Cyan)),
            Span::raw(":Navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(":Details  "),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw(":Locks view  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(":Refresh"),
        ]);
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn render_locks(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Min(0),    // Table
                Constraint::Length(1), // Context keys
            ])
            .split(area);

        // Header
        let stale_count = self.lock_rows.iter().filter(|r| r.is_stale).count();
        let header = Line::from(vec![
            Span::styled(
                " Locks",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "  {} active, {} stale",
                    self.lock_rows.len() - stale_count,
                    stale_count
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        if self.lock_rows.is_empty() {
            let msg = Paragraph::new(Line::from(Span::styled(
                "  No locks held.",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(msg, chunks[1]);
        } else {
            let header_row = Row::new(vec!["Issue", "Agent", "Branch", "Claimed", "Status"]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

            let rows: Vec<Row> = self
                .lock_rows
                .iter()
                .map(|lock| {
                    let style = if lock.is_stale {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default()
                    };

                    let status = if lock.is_stale {
                        "⚠ stale"
                    } else {
                        "● active"
                    };

                    Row::new(vec![
                        format!("#{}", lock.issue_id),
                        truncate_str(&lock.agent_id, 35),
                        truncate_str(lock.branch.as_deref().unwrap_or("—"), 22),
                        lock.claimed_ago.clone(),
                        status.to_string(),
                    ])
                    .style(style)
                })
                .collect();

            let table = Table::new(
                rows,
                [
                    Constraint::Length(8),  // Issue
                    Constraint::Min(20),    // Agent
                    Constraint::Length(24), // Branch
                    Constraint::Length(12), // Claimed
                    Constraint::Length(10), // Status
                ],
            )
            .header(header_row)
            .block(Block::default().borders(Borders::NONE))
            .row_highlight_style(Style::default().bg(HIGHLIGHT_BG));

            let mut state = self.locks_table_state.borrow_mut();
            state.select(Some(self.lock_selected));
            frame.render_stateful_widget(table, chunks[1], &mut state);
        }

        // Context keys
        let keys = Line::from(vec![
            Span::styled("↑↓", Style::default().fg(Color::Cyan)),
            Span::raw(":Navigate  "),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw(":Trust view  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(":Refresh  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(":Agents"),
        ]);
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn render_trust(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Min(0),    // List
                Constraint::Length(1), // Context keys
            ])
            .split(area);

        // Header
        let header = Line::from(vec![
            Span::styled(
                " Trust Store",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {} approved signers", self.trust_entries.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        if self.trust_entries.is_empty() {
            let msg = Paragraph::new(Line::from(Span::styled(
                "  No trusted signers configured.",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(msg, chunks[1]);
        } else {
            let header_row = Row::new(vec!["Principal", "Key Type", "Approved"]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

            let rows: Vec<Row> = self
                .trust_entries
                .iter()
                .map(|entry| {
                    // Extract key type from public key (e.g. "ssh-ed25519 AAAA...")
                    let key_type = entry
                        .public_key
                        .split_whitespace()
                        .next()
                        .unwrap_or("unknown");

                    let approved = entry.metadata_comment.as_deref().unwrap_or("—");

                    Row::new(vec![
                        truncate_str(&entry.principal, 40),
                        key_type.to_string(),
                        truncate_str(approved, 30),
                    ])
                })
                .collect();

            let table = Table::new(
                rows,
                [
                    Constraint::Min(20),    // Principal
                    Constraint::Length(16), // Key Type
                    Constraint::Length(32), // Approved
                ],
            )
            .header(header_row)
            .block(Block::default().borders(Borders::NONE))
            .row_highlight_style(Style::default().bg(HIGHLIGHT_BG));

            let mut state = self.trust_table_state.borrow_mut();
            state.select(Some(self.trust_selected));
            frame.render_stateful_widget(table, chunks[1], &mut state);
        }

        // Context keys
        let keys = Line::from(vec![
            Span::styled("↑↓", Style::default().fg(Color::Cyan)),
            Span::raw(":Navigate  "),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw(":Agents view  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(":Refresh  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(":Agents"),
        ]);
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let detail = match &self.detail {
            Some(d) => d,
            None => return,
        };

        let mut lines: Vec<Line> = Vec::new();

        // Title
        lines.push(Line::from(Span::styled(
            format!(" {}", detail.agent_id),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // Metadata
        let status_style = if detail.is_stale {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        let status_text = if detail.is_stale { "stale" } else { "active" };

        lines.push(Line::from(vec![
            Span::styled("  Status: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(status_text, status_style),
        ]));

        if let Some(ref machine) = detail.machine_id {
            lines.push(Line::from(vec![
                Span::styled("  Machine: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(machine),
            ]));
        }

        if let Some(ref hb) = detail.heartbeat {
            lines.push(Line::from(vec![
                Span::styled(
                    "  Last heartbeat: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format_relative_time(&hb.last_heartbeat)),
            ]));
            if let Some(issue_id) = hb.active_issue_id {
                lines.push(Line::from(vec![
                    Span::styled(
                        "  Active issue: ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("#{issue_id}"), Style::default().fg(Color::Cyan)),
                ]));
            }
        }

        // Locks section
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  Locks ({})", detail.locks.len()),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));

        if detail.locks.is_empty() {
            lines.push(Line::from(Span::styled(
                "    No locks held",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (issue_id, lock) in &detail.locks {
                let branch_str = lock
                    .branch
                    .as_deref()
                    .map(|b| format!(" ({b})"))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled("    ● ", Style::default().fg(Color::Green)),
                    Span::styled(format!("#{issue_id}"), Style::default().fg(Color::Cyan)),
                    Span::raw(branch_str),
                    Span::styled(
                        format!("  claimed {}", format_relative_time(&lock.claimed_at)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }

        // Recent events section
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  Recent Events ({})", detail.recent_events.len()),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));

        if detail.recent_events.is_empty() {
            lines.push(Line::from(Span::styled(
                "    No events recorded",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for ev in &detail.recent_events {
                let time_str = ev.timestamp.format("%H:%M").to_string();
                let event_desc = format_event_summary(&ev.event);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("    {time_str}  "),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(event_desc),
                ]));
            }
        }

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll as u16, 0));

        frame.render_widget(paragraph, area);

        // Render context keys at bottom — overlay a 1-line area
        if area.height > 1 {
            let keys_area = Rect {
                x: area.x,
                y: area.y + area.height - 1,
                width: area.width,
                height: 1,
            };
            let keys = Line::from(vec![
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(":Back  "),
                Span::styled("↑↓", Style::default().fg(Color::Cyan)),
                Span::raw(":Scroll"),
            ]);
            frame.render_widget(
                Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
                keys_area,
            );
        }
    }
}

impl Tab for AgentsTab {
    fn title(&self) -> &str {
        "Agents"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self.view_mode {
            ViewMode::Agents => self.render_agents(frame, area),
            ViewMode::Locks => self.render_locks(frame, area),
            ViewMode::Trust => self.render_trust(frame, area),
            ViewMode::Detail => self.render_detail(frame, area),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabAction {
        match self.view_mode {
            ViewMode::Agents => self.handle_agents_key(key),
            ViewMode::Locks => self.handle_locks_key(key),
            ViewMode::Trust => self.handle_trust_key(key),
            ViewMode::Detail => self.handle_detail_key(key),
        }
    }

    fn on_enter(&mut self) {
        self.start_background_refresh();
    }

    fn on_leave(&mut self) {}

    fn poll_updates(&mut self) {
        let result = self.load_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(data) = result {
            self.load_rx = None;
            self.apply_load_result(data);
        }
    }
}

// ── Background loader ─────────────────────────────────────────────────

/// Load all agents/locks/trust data synchronously (runs on a background thread).
fn load_agents_data(crosslink_dir: &Path) -> AgentsLoadResult {
    let sync = match SyncManager::new(crosslink_dir) {
        Ok(s) => s,
        Err(e) => {
            return AgentsLoadResult {
                agents: Vec::new(),
                lock_rows: Vec::new(),
                trust_entries: Vec::new(),
                status_msg: String::new(),
                error_msg: Some(format!("Failed to init SyncManager: {e}")),
            };
        }
    };

    if !sync.is_initialized() {
        return AgentsLoadResult {
            agents: Vec::new(),
            lock_rows: Vec::new(),
            trust_entries: Vec::new(),
            status_msg: String::new(),
            error_msg: Some("Hub cache not initialized. Run 'crosslink sync' first.".to_string()),
        };
    }

    // Fetch latest state (ignore fetch errors — may be offline)
    let _ = sync.fetch();

    // Read locks
    let (locks, lock_error) = match sync.read_locks_auto() {
        Ok(l) => (l, None),
        Err(e) => (
            LocksFile::empty(),
            Some(format!("Failed to read locks: {e}")),
        ),
    };

    // Read heartbeats (auto-dispatches V1/V2)
    let heartbeats = sync.read_heartbeats_auto().unwrap_or_default();

    // Find stale locks
    let stale = sync.find_stale_locks().unwrap_or_default();
    let stale_agents: std::collections::HashSet<String> =
        stale.iter().map(|(_, a)| a.clone()).collect();
    let stale_issues: std::collections::HashSet<i64> = stale.iter().map(|(id, _)| *id).collect();

    // Read trust store
    let trust = sync.read_allowed_signers().unwrap_or_default();

    // Build merged agent rows
    let agents = build_agent_rows_static(&locks, &heartbeats, &stale_agents);

    // Build lock rows
    let lock_rows = build_lock_rows_static(&locks, &stale_issues);

    let status_msg = format!(
        "{} agents, {} locks, {} trusted signers",
        agents.len(),
        lock_rows.len(),
        trust.entries.len()
    );

    AgentsLoadResult {
        agents,
        lock_rows,
        trust_entries: trust.entries,
        status_msg,
        error_msg: lock_error,
    }
}

/// Build merged agent rows from locks and heartbeats (free function for thread use).
fn build_agent_rows_static(
    locks: &LocksFile,
    heartbeats: &[Heartbeat],
    stale_agents: &std::collections::HashSet<String>,
) -> Vec<AgentRow> {
    use std::collections::HashMap;

    let mut agents: HashMap<String, AgentRow> = HashMap::new();

    for hb in heartbeats {
        agents
            .entry(hb.agent_id.clone())
            .or_insert_with(|| AgentRow {
                agent_id: hb.agent_id.clone(),
                active_issue: None,
                lock_issue: None,
                branch: None,
                heartbeat_ago: None,
                is_stale: false,
                machine_id: None,
            })
            .heartbeat_ago = Some(format_relative_time(&hb.last_heartbeat));

        if let Some(row) = agents.get_mut(&hb.agent_id) {
            row.active_issue = hb.active_issue_id;
            row.machine_id = Some(hb.machine_id.clone());
        }
    }

    for (issue_str, lock) in &locks.locks {
        let issue_id: i64 = issue_str.parse().unwrap_or(0);
        let row = agents
            .entry(lock.agent_id.clone())
            .or_insert_with(|| AgentRow {
                agent_id: lock.agent_id.clone(),
                active_issue: None,
                lock_issue: None,
                branch: None,
                heartbeat_ago: None,
                is_stale: false,
                machine_id: None,
            });
        row.lock_issue = Some(issue_id);
        row.branch = lock.branch.clone();
    }

    for row in agents.values_mut() {
        row.is_stale = stale_agents.contains(&row.agent_id);
    }

    let mut rows: Vec<AgentRow> = agents.into_values().collect();
    rows.sort_by(|a, b| {
        a.is_stale
            .cmp(&b.is_stale)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });
    rows
}

/// Build lock rows from locks file (free function for thread use).
fn build_lock_rows_static(
    locks: &LocksFile,
    stale_issues: &std::collections::HashSet<i64>,
) -> Vec<LockRow> {
    let mut rows: Vec<LockRow> = locks
        .locks
        .iter()
        .map(|(issue_str, lock)| {
            let issue_id: i64 = issue_str.parse().unwrap_or(0);
            LockRow {
                issue_id,
                agent_id: lock.agent_id.clone(),
                branch: lock.branch.clone(),
                claimed_ago: format_relative_time(&lock.claimed_at),
                is_stale: stale_issues.contains(&issue_id),
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        a.is_stale
            .cmp(&b.is_stale)
            .then_with(|| a.issue_id.cmp(&b.issue_id))
    });
    rows
}

// ── Helpers ───────────────────────────────────────────────────────────

fn format_relative_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let diff = now.signed_duration_since(*dt);

    if diff.num_seconds() < 0 {
        "just now".to_string()
    } else if diff.num_seconds() < 60 {
        format!("{}s ago", diff.num_seconds())
    } else if diff.num_minutes() < 60 {
        format!("{}m ago", diff.num_minutes())
    } else if diff.num_hours() < 24 {
        format!("{}h ago", diff.num_hours())
    } else {
        format!("{}d ago", diff.num_days())
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let end = max_len.saturating_sub(3);
        let truncated: String = s.chars().take(end).collect();
        format!("{truncated}...")
    }
}

fn format_event_summary(event: &events::Event) -> String {
    match event {
        events::Event::IssueCreated { title, .. } => {
            format!("IssueCreated: {}", truncate_str(title, 40))
        }
        events::Event::LockClaimed {
            issue_display_id, ..
        } => format!("LockClaimed #{issue_display_id}"),
        events::Event::LockReleased {
            issue_display_id, ..
        } => format!("LockReleased #{issue_display_id}"),
        events::Event::IssueUpdated { title, .. } => {
            let t = title.as_deref().unwrap_or("(untitled)");
            format!("IssueUpdated: {}", truncate_str(t, 40))
        }
        events::Event::StatusChanged { new_status, .. } => {
            format!("StatusChanged → {new_status}")
        }
        _ => format!("{event:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn make_tab() -> AgentsTab {
        // Use a temp dir — no real hub cache, so it will show error/empty state
        let dir = tempfile::tempdir().unwrap();
        AgentsTab::new(dir.path())
    }

    #[test]
    fn test_title() {
        let tab = make_tab();
        assert_eq!(tab.title(), "Agents");
    }

    #[test]
    fn test_initial_view_mode() {
        let tab = make_tab();
        assert_eq!(tab.view_mode, ViewMode::Agents);
    }

    #[test]
    fn test_view_cycle() {
        let mut tab = make_tab();
        assert_eq!(tab.view_mode, ViewMode::Agents);

        tab.handle_key(make_key(KeyCode::Char('v')));
        assert_eq!(tab.view_mode, ViewMode::Locks);

        tab.handle_key(make_key(KeyCode::Char('v')));
        assert_eq!(tab.view_mode, ViewMode::Trust);

        tab.handle_key(make_key(KeyCode::Char('v')));
        assert_eq!(tab.view_mode, ViewMode::Agents);
    }

    #[test]
    fn test_esc_returns_to_agents() {
        let mut tab = make_tab();
        tab.handle_key(make_key(KeyCode::Char('v'))); // → Locks
        assert_eq!(tab.view_mode, ViewMode::Locks);

        tab.handle_key(make_key(KeyCode::Esc));
        assert_eq!(tab.view_mode, ViewMode::Agents);
    }

    #[test]
    fn test_refresh_key() {
        let mut tab = make_tab();
        let result = tab.handle_key(make_key(KeyCode::Char('r')));
        assert!(matches!(result, TabAction::Consumed));
    }

    #[test]
    fn test_unhandled_key() {
        let mut tab = make_tab();
        let result = tab.handle_key(make_key(KeyCode::Char('x')));
        assert!(matches!(result, TabAction::NotHandled));
    }

    #[test]
    fn test_render_agents_no_panic() {
        let tab = make_tab();
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_locks_no_panic() {
        let mut tab = make_tab();
        tab.view_mode = ViewMode::Locks;
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_trust_no_panic() {
        let mut tab = make_tab();
        tab.view_mode = ViewMode::Trust;
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_format_relative_time() {
        let now = chrono::Utc::now();
        assert_eq!(format_relative_time(&now), "0s ago");

        let past = now - chrono::Duration::minutes(5);
        assert_eq!(format_relative_time(&past), "5m ago");

        let hours_ago = now - chrono::Duration::hours(3);
        assert_eq!(format_relative_time(&hours_ago), "3h ago");

        let days_ago = now - chrono::Duration::days(2);
        assert_eq!(format_relative_time(&days_ago), "2d ago");
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
        assert_eq!(truncate_str("ab", 2), "ab");
    }

    #[test]
    fn test_navigation_empty_list() {
        let mut tab = make_tab();
        // Should not panic on empty list
        tab.handle_key(make_key(KeyCode::Char('j')));
        tab.handle_key(make_key(KeyCode::Char('k')));
        tab.handle_key(make_key(KeyCode::Enter));
    }

    // ── Async loading tests ─────────────────────────────────────────

    #[test]
    fn test_new_starts_with_loading_state() {
        let dir = tempfile::tempdir().unwrap();
        let tab = AgentsTab::new(dir.path());
        // Should be loading (background thread spawned) and have a receiver
        assert!(tab.loading);
        assert!(tab.load_rx.is_some());
        // Data should be empty initially (not loaded yet synchronously)
        assert!(tab.agents.is_empty());
        assert!(tab.lock_rows.is_empty());
    }

    #[test]
    fn test_new_returns_instantly() {
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let _tab = AgentsTab::new(dir.path());
        let elapsed = start.elapsed();
        // Constructor should return in under 100ms (no blocking I/O)
        assert!(
            elapsed.as_millis() < 100,
            "AgentsTab::new() took {}ms, expected <100ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn test_poll_updates_receives_result() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = AgentsTab::new(dir.path());
        assert!(tab.loading);

        // Wait for background thread to complete (should be fast with no hub)
        std::thread::sleep(std::time::Duration::from_millis(500));

        tab.poll_updates();
        // After polling, loading should be false and receiver consumed
        assert!(!tab.loading);
        assert!(tab.load_rx.is_none());
    }

    #[test]
    fn test_render_shows_loading_indicator() {
        let dir = tempfile::tempdir().unwrap();
        let tab = AgentsTab::new(dir.path());
        // Render immediately before background thread completes
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        // Should not panic — renders "Loading agents..." or error state
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_on_enter_spawns_new_background_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = AgentsTab::new(dir.path());
        // Wait for first load to complete
        std::thread::sleep(std::time::Duration::from_millis(500));
        tab.poll_updates();
        assert!(!tab.loading);

        // on_enter should spawn a new background load
        tab.on_enter();
        assert!(tab.loading);
        assert!(tab.load_rx.is_some());
    }
}
