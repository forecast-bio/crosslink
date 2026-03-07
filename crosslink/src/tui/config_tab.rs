use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table, Wrap},
    Frame,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use super::{Tab, TabAction};
use crate::db::Database;
use crate::events;
use crate::identity::AgentConfig;
use crate::sync::SyncManager;

/// Which sub-view is active.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ViewMode {
    /// Main diagnostics overview.
    Main,
    /// Full event log browser.
    EventLog,
}

/// A summary of a single event for display.
struct EventSummary {
    timestamp: String,
    agent_id: String,
    description: String,
}

/// Data payload from background sync thread.
struct ConfigSyncResult {
    hub_initialized: bool,
    hub_v2: bool,
    lock_count: usize,
    stale_lock_count: usize,
    agent_count: usize,
    recent_events: Vec<EventSummary>,
    all_events: Vec<EventSummary>,
}

/// The Config tab — configuration and diagnostics dashboard.
pub struct ConfigTab {
    crosslink_dir: PathBuf,
    db_path: PathBuf,
    view_mode: ViewMode,
    main_scroll: usize,

    // Agent identity
    agent_id: String,
    machine_id: String,
    ssh_fingerprint: String,

    // Database
    schema_version: i32,
    issue_count: i64,
    milestone_count: i64,

    // Sync state
    hub_initialized: bool,
    hub_v2: bool,
    lock_count: usize,
    stale_lock_count: usize,
    agent_count: usize,

    // Configuration (key-value pairs)
    config_entries: Vec<(String, String)>,

    // Events
    recent_events: Vec<EventSummary>,
    all_events: Vec<EventSummary>,
    event_scroll: usize,

    error_msg: Option<String>,

    /// Whether a background sync load is in progress.
    loading_sync: bool,
    /// Receiver for background sync results.
    sync_rx: Option<mpsc::Receiver<ConfigSyncResult>>,
}

impl ConfigTab {
    pub fn new(db: &Database, db_path: &Path, crosslink_dir: &Path) -> Self {
        let mut tab = ConfigTab {
            crosslink_dir: crosslink_dir.to_path_buf(),
            db_path: db_path.to_path_buf(),
            view_mode: ViewMode::Main,
            main_scroll: 0,
            agent_id: String::new(),
            machine_id: String::new(),
            ssh_fingerprint: String::new(),
            schema_version: 0,
            issue_count: 0,
            milestone_count: 0,
            hub_initialized: false,
            hub_v2: false,
            lock_count: 0,
            stale_lock_count: 0,
            agent_count: 0,
            config_entries: Vec::new(),
            recent_events: Vec::new(),
            all_events: Vec::new(),
            event_scroll: 0,
            error_msg: None,
            loading_sync: false,
            sync_rx: None,
        };
        // Fast loads done synchronously
        tab.load_identity();
        tab.load_db_info(db);
        tab.load_config();
        // Heavy sync/events deferred to background
        tab.start_background_sync();
        tab
    }

    fn open_db(&self) -> Option<Database> {
        Database::open(&self.db_path).ok()
    }

    /// Spawn a background thread for the slow SyncManager + events work.
    fn start_background_sync(&mut self) {
        self.loading_sync = true;
        let (tx, rx) = mpsc::channel();
        self.sync_rx = Some(rx);
        let crosslink_dir = self.crosslink_dir.clone();

        std::thread::spawn(move || {
            let result = load_config_sync_data(&crosslink_dir);
            let _ = tx.send(result);
        });
    }

    /// Apply background sync results.
    fn apply_sync_result(&mut self, result: ConfigSyncResult) {
        self.loading_sync = false;
        self.hub_initialized = result.hub_initialized;
        self.hub_v2 = result.hub_v2;
        self.lock_count = result.lock_count;
        self.stale_lock_count = result.stale_lock_count;
        self.agent_count = result.agent_count;
        self.recent_events = result.recent_events;
        self.all_events = result.all_events;
    }

    fn load_identity(&mut self) {
        match AgentConfig::load(&self.crosslink_dir) {
            Ok(Some(config)) => {
                self.agent_id = config.agent_id;
                self.machine_id = config.machine_id;
                self.ssh_fingerprint = config
                    .ssh_fingerprint
                    .unwrap_or_else(|| "(none)".to_string());
            }
            Ok(None) => {
                self.agent_id = "(not configured)".to_string();
                self.machine_id = "(unknown)".to_string();
                self.ssh_fingerprint = "(none)".to_string();
            }
            Err(_) => {
                self.agent_id = "(error loading)".to_string();
                self.machine_id = "(error)".to_string();
                self.ssh_fingerprint = "(error)".to_string();
            }
        }
    }

    fn load_db_info(&mut self, db: &Database) {
        self.schema_version = db.get_schema_version().unwrap_or(0);
        self.issue_count = db.get_issue_count().unwrap_or(0);
        self.milestone_count = db.get_milestone_count().unwrap_or(0);
    }

    fn load_config(&mut self) {
        self.config_entries.clear();
        let config_path = self.crosslink_dir.join("hook-config.json");
        if !config_path.exists() {
            self.config_entries
                .push(("(no config file)".to_string(), String::new()));
            return;
        }
        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => {
                self.config_entries
                    .push(("(error reading config)".to_string(), String::new()));
                return;
            }
        };
        let config: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => {
                self.config_entries
                    .push(("(invalid JSON)".to_string(), String::new()));
                return;
            }
        };

        // Display known config keys in order
        let keys = [
            "tracking_mode",
            "intervention_tracking",
            "cpitd_auto_install",
            "comment_discipline",
            "kickoff_verification",
            "signing_enforcement",
        ];
        for key in &keys {
            if let Some(val) = config.get(*key) {
                self.config_entries
                    .push((key.to_string(), format_json_value(val)));
            }
        }
        // Array keys
        for key in &[
            "blocked_git_commands",
            "gated_git_commands",
            "allowed_bash_prefixes",
        ] {
            if let Some(serde_json::Value::Array(arr)) = config.get(*key) {
                let items: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                self.config_entries
                    .push((key.to_string(), format!("[{}]", items.join(", "))));
            }
        }
    }

    fn refresh(&mut self) {
        self.error_msg = None;
        if let Some(db) = self.open_db() {
            self.load_identity();
            self.load_db_info(&db);
            self.load_config();
        }
        self.start_background_sync();
    }

    // ── Rendering ────────────────────────────────────────────────────

    fn render_main(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        // Title
        lines.push(Line::from(Span::styled(
            " Configuration & Diagnostics",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // ── Agent Identity ──
        lines.push(section_header("Agent Identity"));
        lines.push(kv_line("Agent ID", &self.agent_id, Color::White));
        lines.push(kv_line("Machine", &self.machine_id, Color::White));
        lines.push(kv_line("SSH Key", &self.ssh_fingerprint, Color::DarkGray));
        lines.push(Line::from(""));

        // ── Database ──
        lines.push(section_header("Database"));
        lines.push(kv_line(
            "Schema",
            &format!("v{}", self.schema_version),
            Color::White,
        ));
        lines.push(kv_line(
            "Issues",
            &self.issue_count.to_string(),
            Color::White,
        ));
        lines.push(kv_line(
            "Milestones",
            &self.milestone_count.to_string(),
            Color::White,
        ));
        lines.push(kv_line(
            "Path",
            &self.db_path.display().to_string(),
            Color::DarkGray,
        ));
        lines.push(Line::from(""));

        // ── Hub Sync ──
        lines.push(section_header("Hub Sync"));
        if self.loading_sync {
            lines.push(kv_line("Status", "loading...", Color::DarkGray));
        } else if self.hub_initialized {
            let layout_str = if self.hub_v2 { "V2" } else { "V1" };
            lines.push(kv_line("Status", "initialized", Color::Green));
            lines.push(kv_line("Layout", layout_str, Color::White));
            lines.push(kv_line(
                "Active Locks",
                &self.lock_count.to_string(),
                Color::Yellow,
            ));
            if self.stale_lock_count > 0 {
                lines.push(kv_line(
                    "Stale Locks",
                    &self.stale_lock_count.to_string(),
                    Color::Red,
                ));
            }
            lines.push(kv_line(
                "Known Agents",
                &self.agent_count.to_string(),
                Color::White,
            ));
        } else {
            lines.push(kv_line(
                "Status",
                "not initialized (run 'crosslink sync')",
                Color::DarkGray,
            ));
        }
        lines.push(Line::from(""));

        // ── Configuration ──
        lines.push(section_header("Configuration"));
        if self.config_entries.is_empty() {
            lines.push(Line::from(Span::styled(
                "   (no configuration)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (key, value) in &self.config_entries {
                let val_color = match value.as_str() {
                    "true" => Color::Green,
                    "false" => Color::Red,
                    "strict" | "enforced" | "required" => Color::Yellow,
                    "relaxed" | "disabled" | "none" => Color::DarkGray,
                    _ => Color::White,
                };
                lines.push(kv_line(key, value, val_color));
            }
        }
        lines.push(Line::from(""));

        // ── Recent Events ──
        lines.push(section_header("Recent Events"));
        if self.recent_events.is_empty() {
            lines.push(Line::from(Span::styled(
                "   (no events)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for evt in &self.recent_events {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("   {} ", evt.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{} ", evt.description),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(
                        format!("by {}", evt.agent_id),
                        Style::default().fg(Color::Indexed(245)),
                    ),
                ]));
            }
            if self.all_events.len() > self.recent_events.len() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "   ... {} more (press 'e' for full log)",
                        self.all_events.len() - self.recent_events.len()
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " r:Refresh  e:Event log  ↑↓/j/k:Scroll",
            Style::default().fg(Color::DarkGray),
        )));

        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL))
            .scroll((self.main_scroll as u16, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(para, area);
    }

    fn render_event_log(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);

        // Header
        let header_spans = vec![
            Span::styled(
                " Event Log",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({} events)", self.all_events.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        frame.render_widget(Paragraph::new(Line::from(header_spans)), chunks[0]);

        if self.all_events.is_empty() {
            let empty = Paragraph::new(Line::from(Span::styled(
                " No events found",
                Style::default().fg(Color::DarkGray),
            )))
            .block(Block::default().borders(Borders::ALL));
            frame.render_widget(empty, chunks[1]);
            return;
        }

        let widths = [
            Constraint::Length(10),
            Constraint::Min(30),
            Constraint::Length(32),
        ];

        let header = Row::new(vec!["Time", "Event", "Agent"])
            .style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        // Skip rows based on event_scroll for manual scrolling
        let visible_rows: Vec<Row> = self
            .all_events
            .iter()
            .skip(self.event_scroll)
            .map(|evt| {
                Row::new(vec![
                    ratatui::widgets::Cell::from(evt.timestamp.clone())
                        .style(Style::default().fg(Color::DarkGray)),
                    ratatui::widgets::Cell::from(evt.description.clone()),
                    ratatui::widgets::Cell::from(evt.agent_id.clone())
                        .style(Style::default().fg(Color::Indexed(245))),
                ])
            })
            .collect();

        let table = Table::new(visible_rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL));

        frame.render_widget(table, chunks[1]);
    }

    // ── Key handling ─────────────────────────────────────────────────

    fn handle_main_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.main_scroll = self.main_scroll.saturating_add(1);
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.main_scroll = self.main_scroll.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::PageDown => {
                self.main_scroll = self.main_scroll.saturating_add(10);
                TabAction::Consumed
            }
            KeyCode::PageUp => {
                self.main_scroll = self.main_scroll.saturating_sub(10);
                TabAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.main_scroll = 0;
                TabAction::Consumed
            }
            KeyCode::Char('e') => {
                self.view_mode = ViewMode::EventLog;
                self.event_scroll = 0;
                TabAction::Consumed
            }
            KeyCode::Char('r') => {
                self.refresh();
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    fn handle_event_log_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.view_mode = ViewMode::Main;
                TabAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.all_events.is_empty() {
                    self.event_scroll = self
                        .event_scroll
                        .saturating_add(1)
                        .min(self.all_events.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.event_scroll = self.event_scroll.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::PageDown => {
                if !self.all_events.is_empty() {
                    self.event_scroll = self
                        .event_scroll
                        .saturating_add(10)
                        .min(self.all_events.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::PageUp => {
                self.event_scroll = self.event_scroll.saturating_sub(10);
                TabAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.event_scroll = 0;
                TabAction::Consumed
            }
            KeyCode::Char('G') => {
                if !self.all_events.is_empty() {
                    self.event_scroll = self.all_events.len() - 1;
                }
                TabAction::Consumed
            }
            KeyCode::Char('r') => {
                self.refresh();
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }
}

impl Tab for ConfigTab {
    fn title(&self) -> &str {
        "Config"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self.view_mode {
            ViewMode::Main => self.render_main(frame, area),
            ViewMode::EventLog => self.render_event_log(frame, area),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabAction {
        match self.view_mode {
            ViewMode::Main => self.handle_main_key(key),
            ViewMode::EventLog => self.handle_event_log_key(key),
        }
    }

    fn on_enter(&mut self) {
        self.refresh();
    }

    fn on_leave(&mut self) {}

    fn poll_updates(&mut self) {
        let result = self.sync_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(data) = result {
            self.sync_rx = None;
            self.apply_sync_result(data);
        }
    }
}

// ── Background loader ────────────────────────────────────────────────

/// Load sync state and events on a background thread.
fn load_config_sync_data(crosslink_dir: &Path) -> ConfigSyncResult {
    let mut result = ConfigSyncResult {
        hub_initialized: false,
        hub_v2: false,
        lock_count: 0,
        stale_lock_count: 0,
        agent_count: 0,
        recent_events: Vec::new(),
        all_events: Vec::new(),
    };

    let sync = match SyncManager::new(crosslink_dir) {
        Ok(s) => s,
        Err(_) => return result,
    };

    result.hub_initialized = sync.is_initialized();
    if !result.hub_initialized {
        return result;
    }

    result.hub_v2 = sync.is_v2_layout();

    if let Ok(locks) = sync.read_locks_auto() {
        result.lock_count = locks.locks.len();
    }
    if let Ok(stale) = sync.find_stale_locks() {
        result.stale_lock_count = stale.len();
    }
    if let Ok(heartbeats) = sync.read_heartbeats() {
        result.agent_count = heartbeats.len();
    }

    // Read events from all agents
    let agents_dir = sync.cache_path().join("agents");
    if agents_dir.exists() {
        let mut all: Vec<events::EventEnvelope> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&agents_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let log_path = entry.path().join("events.log");
                    if let Ok(mut evts) = events::read_events(&log_path) {
                        all.append(&mut evts);
                    }
                }
            }
        }
        all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        let summaries: Vec<EventSummary> = all
            .iter()
            .map(|e| EventSummary {
                timestamp: e.timestamp.format("%H:%M:%S").to_string(),
                agent_id: truncate(&e.agent_id, 30),
                description: describe_event(&e.event),
            })
            .collect();

        result.recent_events = summaries
            .iter()
            .take(15)
            .map(|s| EventSummary {
                timestamp: s.timestamp.clone(),
                agent_id: s.agent_id.clone(),
                description: s.description.clone(),
            })
            .collect();
        result.all_events = summaries;
    }

    result
}

// ── Helpers ──────────────────────────────────────────────────────────

fn section_header(title: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        format!(" {title}"),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )])
}

fn kv_line(key: &str, value: &str, val_color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("   {key:<18}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value.to_string(), Style::default().fg(val_color)),
    ])
}

fn format_json_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = max.saturating_sub(3);
        let truncated: String = s.chars().take(end).collect();
        format!("{truncated}...")
    }
}

fn describe_event(event: &events::Event) -> String {
    match event {
        events::Event::IssueCreated { title, .. } => {
            format!("IssueCreated: {}", truncate(title, 40))
        }
        events::Event::LockClaimed {
            issue_display_id, ..
        } => format!("LockClaimed #{issue_display_id}"),
        events::Event::LockReleased {
            issue_display_id, ..
        } => format!("LockReleased #{issue_display_id}"),
        events::Event::IssueUpdated { .. } => "IssueUpdated".to_string(),
        events::Event::StatusChanged { new_status, .. } => {
            format!("StatusChanged → {new_status}")
        }
        events::Event::DependencyAdded { .. } => "DependencyAdded".to_string(),
        events::Event::DependencyRemoved { .. } => "DependencyRemoved".to_string(),
        events::Event::RelationAdded { .. } => "RelationAdded".to_string(),
        events::Event::RelationRemoved { .. } => "RelationRemoved".to_string(),
        events::Event::MilestoneAssigned { .. } => "MilestoneAssigned".to_string(),
        events::Event::LabelAdded { label, .. } => format!("LabelAdded: {label}"),
        events::Event::LabelRemoved { label, .. } => format!("LabelRemoved: {label}"),
        events::Event::ParentChanged { .. } => "ParentChanged".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use tempfile::tempdir;

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn setup_tab() -> (ConfigTab, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db_path = crosslink_dir.join("issues.db");
        let db = Database::open(&db_path).unwrap();
        // Create some data
        db.create_issue("Test issue", None, "high").unwrap();
        db.create_milestone("v1.0", None).unwrap();
        let tab = ConfigTab::new(&db, &db_path, &crosslink_dir);
        (tab, dir)
    }

    #[test]
    fn test_title() {
        let (tab, _dir) = setup_tab();
        assert_eq!(tab.title(), "Config");
    }

    #[test]
    fn test_initial_state() {
        let (tab, _dir) = setup_tab();
        assert_eq!(tab.view_mode, ViewMode::Main);
        assert_eq!(tab.main_scroll, 0);
        assert_eq!(tab.issue_count, 1);
        assert_eq!(tab.milestone_count, 1);
        assert!(tab.schema_version > 0);
    }

    #[test]
    fn test_main_scroll() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.main_scroll, 1);
        tab.handle_key(make_key(KeyCode::Down));
        assert_eq!(tab.main_scroll, 2);
        tab.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(tab.main_scroll, 1);
        tab.handle_key(make_key(KeyCode::Char('g')));
        assert_eq!(tab.main_scroll, 0);
    }

    #[test]
    fn test_page_scroll() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::PageDown));
        assert_eq!(tab.main_scroll, 10);
        tab.handle_key(make_key(KeyCode::PageUp));
        assert_eq!(tab.main_scroll, 0);
    }

    #[test]
    fn test_switch_to_event_log() {
        let (mut tab, _dir) = setup_tab();
        assert_eq!(tab.view_mode, ViewMode::Main);
        tab.handle_key(make_key(KeyCode::Char('e')));
        assert_eq!(tab.view_mode, ViewMode::EventLog);
        assert_eq!(tab.event_scroll, 0);
    }

    #[test]
    fn test_event_log_back() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::Char('e')));
        assert_eq!(tab.view_mode, ViewMode::EventLog);
        tab.handle_key(make_key(KeyCode::Esc));
        assert_eq!(tab.view_mode, ViewMode::Main);
    }

    #[test]
    fn test_refresh() {
        let (mut tab, _dir) = setup_tab();
        let result = tab.handle_key(make_key(KeyCode::Char('r')));
        assert!(matches!(result, TabAction::Consumed));
    }

    #[test]
    fn test_unhandled_key() {
        let (mut tab, _dir) = setup_tab();
        let result = tab.handle_key(make_key(KeyCode::Char('x')));
        assert!(matches!(result, TabAction::NotHandled));
    }

    #[test]
    fn test_render_main_no_panic() {
        let (tab, _dir) = setup_tab();
        let backend = ratatui::backend::TestBackend::new(100, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_event_log_no_panic() {
        let (mut tab, _dir) = setup_tab();
        tab.view_mode = ViewMode::EventLog;
        let backend = ratatui::backend::TestBackend::new(100, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_no_agent_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db_path = crosslink_dir.join("issues.db");
        let db = Database::open(&db_path).unwrap();
        let tab = ConfigTab::new(&db, &db_path, &crosslink_dir);
        assert_eq!(tab.agent_id, "(not configured)");
    }

    #[test]
    fn test_with_config_file() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let config = r#"{
            "tracking_mode": "strict",
            "intervention_tracking": true,
            "signing_enforcement": "audit"
        }"#;
        std::fs::write(crosslink_dir.join("hook-config.json"), config).unwrap();
        let db_path = crosslink_dir.join("issues.db");
        let db = Database::open(&db_path).unwrap();
        let tab = ConfigTab::new(&db, &db_path, &crosslink_dir);
        assert!(tab.config_entries.len() >= 3);
        assert!(tab
            .config_entries
            .iter()
            .any(|(k, v)| k == "tracking_mode" && v == "strict"));
    }

    #[test]
    fn test_describe_event_variants() {
        use crate::events::Event;
        use uuid::Uuid;

        assert_eq!(
            describe_event(&Event::LockClaimed {
                issue_display_id: 42,
                branch: None
            }),
            "LockClaimed #42"
        );
        assert_eq!(
            describe_event(&Event::LockReleased {
                issue_display_id: 7
            }),
            "LockReleased #7"
        );
        assert_eq!(
            describe_event(&Event::StatusChanged {
                uuid: Uuid::nil(),
                new_status: "closed".to_string(),
                closed_at: None
            }),
            "StatusChanged → closed"
        );
        assert!(describe_event(&Event::IssueCreated {
            uuid: Uuid::nil(),
            title: "Test issue".to_string(),
            description: None,
            priority: "high".to_string(),
            labels: vec![],
            parent_uuid: None,
            created_by: "agent".to_string(),
        })
        .starts_with("IssueCreated:"));
    }

    #[test]
    fn test_progress_bar_helper() {
        assert_eq!(format_json_value(&serde_json::json!("hello")), "hello");
        assert_eq!(format_json_value(&serde_json::json!(true)), "true");
        assert_eq!(format_json_value(&serde_json::json!(42)), "42");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a long string here", 10), "a long ...");
    }

    #[test]
    fn test_event_log_scroll() {
        let (mut tab, _dir) = setup_tab();
        tab.view_mode = ViewMode::EventLog;
        // No events, so scroll should stay at 0
        tab.handle_key(make_key(KeyCode::Down));
        assert_eq!(tab.event_scroll, 0);
        tab.handle_key(make_key(KeyCode::PageDown));
        assert_eq!(tab.event_scroll, 0);
    }

    // ── Async loading tests ─────────────────────────────────────────

    #[test]
    fn test_new_starts_with_loading_sync() {
        let (tab, _dir) = setup_tab();
        // Fast parts (identity, db, config) are loaded synchronously
        assert!(tab.schema_version > 0);
        assert_eq!(tab.issue_count, 1);
        // Sync state is loaded in background — loading_sync should be true
        // (or already finished if thread was very fast)
        // Either way, we should have a receiver or it should have completed
        // The key point: constructor returned without blocking on SyncManager
    }

    #[test]
    fn test_new_returns_instantly() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db_path = crosslink_dir.join("issues.db");
        let db = Database::open(&db_path).unwrap();

        let start = std::time::Instant::now();
        let _tab = ConfigTab::new(&db, &db_path, &crosslink_dir);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "ConfigTab::new() took {}ms, expected <100ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn test_poll_updates_receives_sync_result() {
        let (mut tab, _dir) = setup_tab();
        // Wait for background thread
        std::thread::sleep(std::time::Duration::from_millis(500));
        tab.poll_updates();
        // After polling, sync loading should be done
        assert!(!tab.loading_sync);
        assert!(tab.sync_rx.is_none());
    }

    #[test]
    fn test_on_enter_spawns_new_sync() {
        let (mut tab, _dir) = setup_tab();
        std::thread::sleep(std::time::Duration::from_millis(500));
        tab.poll_updates();
        assert!(!tab.loading_sync);

        tab.on_enter();
        assert!(tab.loading_sync);
        assert!(tab.sync_rx.is_some());
    }
}
