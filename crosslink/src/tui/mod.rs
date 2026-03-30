pub mod agents_tab;
pub mod config_tab;
pub mod issues_tab;
pub mod knowledge_tab;
pub mod milestones_tab;
pub mod tabs;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs as TabsWidget, Wrap},
    Frame,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;
use crate::sync::SyncManager;

/// Background color for highlighted/selected rows. Uses a dark gray from the
/// 256-color palette that is distinct enough to show selection without
/// overriding cell-level foreground colors.
pub const HIGHLIGHT_BG: Color = Color::Indexed(236);

/// Format a UTC datetime as a human-readable relative time string.
/// Used across multiple TUI tabs (agents, issues, config).
pub fn format_relative_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
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
    } else if diff.num_days() < 30 {
        format!("{}d ago", diff.num_days())
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}

/// Status filter options shared by Issues and Milestones tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusFilter {
    Open,
    Closed,
    All,
}

impl StatusFilter {
    pub const fn next(self) -> Self {
        match self {
            Self::Open => Self::Closed,
            Self::Closed => Self::All,
            Self::All => Self::Open,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Closed => "Closed",
            Self::All => "All",
        }
    }
}

/// Create a `KeyEvent` for testing purposes. Shared across TUI tab test modules.
#[cfg(test)]
pub fn make_test_key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    KeyEvent {
        code,
        modifiers: KeyModifiers::empty(),
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    }
}

/// Truncate a string to a maximum character length, appending "..." if truncated.
/// Used across multiple TUI tabs (agents, config, issues).
pub fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let end = max_len.saturating_sub(3);
        let truncated: String = s.chars().take(end).collect();
        format!("{truncated}...")
    }
}

/// Format an event into a human-readable summary string.
/// Shared between `agents_tab` and `config_tab` for event display.
pub fn format_event_description(event: &crate::events::Event) -> String {
    use crate::events::Event;
    match event {
        Event::IssueCreated { title, .. } => {
            format!("IssueCreated: {}", truncate_str(title, 40))
        }
        Event::LockClaimed {
            issue_display_id, ..
        } => format!("LockClaimed #{issue_display_id}"),
        Event::LockReleased {
            issue_display_id, ..
        } => format!("LockReleased #{issue_display_id}"),
        Event::IssueUpdated { title, .. } => {
            let t = title.as_deref().unwrap_or("(untitled)");
            format!("IssueUpdated: {}", truncate_str(t, 40))
        }
        Event::StatusChanged { new_status, .. } => {
            format!("StatusChanged \u{2192} {new_status}")
        }
        Event::DependencyAdded { .. } => "DependencyAdded".to_string(),
        Event::DependencyRemoved { .. } => "DependencyRemoved".to_string(),
        Event::RelationAdded { .. } => "RelationAdded".to_string(),
        Event::RelationRemoved { .. } => "RelationRemoved".to_string(),
        Event::MilestoneAssigned { .. } => "MilestoneAssigned".to_string(),
        Event::LabelAdded { label, .. } => format!("LabelAdded: {label}"),
        Event::LabelRemoved { label, .. } => format!("LabelRemoved: {label}"),
        Event::ParentChanged { .. } => "ParentChanged".to_string(),
    }
}

/// Action returned by a tab's key handler to communicate with the App.
pub enum TabAction {
    /// Key was consumed by the tab.
    Consumed,
    /// Key was not handled; App should process it.
    NotHandled,
    /// Request the app to quit.
    Quit,
    /// Show a flash message to the user.
    Flash(String),
}

/// Trait that each tab panel must implement.
pub trait Tab {
    fn title(&self) -> &'static str;
    fn render(&self, frame: &mut Frame, area: Rect);
    fn handle_key(&mut self, key: KeyEvent) -> TabAction;
    /// Called when this tab becomes the active tab.
    fn on_enter(&mut self);
    /// Called when this tab loses focus.
    fn on_leave(&mut self);
    /// Poll for async data updates (called each event-loop tick).
    fn poll_updates(&mut self) {}
    /// Force a data reload (called after sync completes). Default cycles `on_leave`/`on_enter`.
    fn force_refresh(&mut self) {
        self.on_leave();
        self.on_enter();
    }
}

/// Copy text to the system clipboard using platform-native commands.
pub fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait()
        });
    #[cfg(target_os = "linux")]
    let result = {
        // Try clipboard tools in order of preference:
        // 1. wl-copy (Wayland)
        // 2. xclip (X11)
        // 3. xsel (X11 fallback)
        // 4. clip.exe (WSL2)
        let tools: &[(&str, &[&str])] = &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("clip.exe", &[]),
        ];

        let mut last_result: Result<std::process::ExitStatus, std::io::Error> = Err(
            std::io::Error::new(std::io::ErrorKind::NotFound, "no clipboard tool found"),
        );

        for &(cmd, args) in tools {
            let attempt = std::process::Command::new(cmd)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    if let Some(ref mut stdin) = child.stdin {
                        stdin.write_all(text.as_bytes())?;
                    }
                    child.wait()
                });

            match &attempt {
                Ok(status) if status.success() => {
                    last_result = attempt;
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    last_result = attempt;
                }
            }
        }

        last_result
    };
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("clip.exe")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait()
        });
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let result: Result<std::process::ExitStatus, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    result.map(|s| s.success()).unwrap_or(false)
}

/// Result from a background sync operation.
struct SyncResult {
    cache_path: PathBuf,
    error: Option<String>,
}

/// Top-level TUI application state.
pub struct App {
    tabs: Vec<Box<dyn Tab>>,
    active_tab: usize,
    show_help: bool,
    should_quit: bool,
    /// Command palette state.
    command_mode: bool,
    command_input: String,
    /// Transient status message (e.g. "Copied!", "Unknown command").
    flash_message: Option<String>,
    /// Tracks the tab bar area for mouse click detection.
    tab_bar_area: Rect,
    /// Path to the .crosslink directory (for sync operations).
    crosslink_dir: PathBuf,
    /// Path to the issues database (for hydration after sync).
    db_path: PathBuf,
    /// When the last successful sync completed.
    last_sync: Instant,
    /// Receiver for background sync results.
    sync_rx: Option<mpsc::Receiver<SyncResult>>,
    /// Whether a background sync is in progress.
    syncing: bool,
}

impl App {
    pub fn new(db: &Database, crosslink_dir: &Path) -> anyhow::Result<Self> {
        let db_path = crosslink_dir.join("issues.db");
        let issues_tab = issues_tab::IssuesTab::new(db, &db_path)?;
        let agents_tab = agents_tab::AgentsTab::new(crosslink_dir);
        let knowledge_tab = knowledge_tab::KnowledgeTab::new(crosslink_dir);
        let milestones_tab = milestones_tab::MilestonesTab::new(db, &db_path);
        let config_tab = config_tab::ConfigTab::new(db, &db_path, crosslink_dir);
        let pipelines_tab = tabs::PlaceholderTab::new("Pipelines", 6);
        let tabs: Vec<Box<dyn Tab>> = vec![
            Box::new(issues_tab),
            Box::new(agents_tab),
            Box::new(knowledge_tab),
            Box::new(milestones_tab),
            Box::new(config_tab),
            Box::new(pipelines_tab),
        ];

        // Activate the first tab
        let mut app = App {
            tabs,
            active_tab: 0,
            show_help: false,
            should_quit: false,
            command_mode: false,
            command_input: String::new(),
            flash_message: None,
            tab_bar_area: Rect::default(),
            crosslink_dir: crosslink_dir.to_path_buf(),
            db_path,
            last_sync: Instant::now(),
            sync_rx: None,
            syncing: false,
        };
        app.tabs[0].on_enter();
        Ok(app)
    }

    fn next_tab(&mut self) {
        self.tabs[self.active_tab].on_leave();
        self.active_tab = (self.active_tab + 1) % self.tabs.len();
        self.tabs[self.active_tab].on_enter();
    }

    fn prev_tab(&mut self) {
        self.tabs[self.active_tab].on_leave();
        if self.active_tab == 0 {
            self.active_tab = self.tabs.len() - 1;
        } else {
            self.active_tab -= 1;
        }
        self.tabs[self.active_tab].on_enter();
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Clear flash message on any keypress
        self.flash_message = None;

        // Help overlay consumes all keys except ? and Esc to dismiss
        if self.show_help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.show_help = false,
                _ => {}
            }
            return;
        }

        // Command palette mode
        if self.command_mode {
            self.handle_command_key(key);
            return;
        }

        // Let the active tab handle the key first
        match self.tabs[self.active_tab].handle_key(key) {
            TabAction::Consumed => return,
            TabAction::Quit => {
                self.should_quit = true;
                return;
            }
            TabAction::Flash(msg) => {
                self.flash_message = Some(msg);
                return;
            }
            TabAction::NotHandled => {}
        }

        // Global key bindings
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Tab => self.next_tab(),
            KeyCode::BackTab => self.prev_tab(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(':') => {
                self.command_mode = true;
                self.command_input.clear();
            }
            KeyCode::Char('r') => {
                self.start_background_sync();
            }
            // Number keys 1-6 for direct tab selection
            KeyCode::Char(c @ '1'..='6') => {
                let idx = (c as usize) - ('1' as usize);
                if idx < self.tabs.len() && idx != self.active_tab {
                    self.tabs[self.active_tab].on_leave();
                    self.active_tab = idx;
                    self.tabs[self.active_tab].on_enter();
                }
            }
            _ => {}
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.command_mode = false;
                self.command_input.clear();
            }
            KeyCode::Enter => {
                let cmd = self.command_input.trim().to_string();
                self.command_mode = false;
                self.command_input.clear();
                self.execute_command(&cmd);
            }
            KeyCode::Backspace => {
                self.command_input.pop();
            }
            KeyCode::Char(c) => {
                self.command_input.push(c);
            }
            _ => {}
        }
    }

    fn execute_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        match parts.first().copied() {
            Some("q" | "quit") => self.should_quit = true,
            Some("help" | "?") => self.show_help = true,
            Some("r" | "refresh") => {
                self.tabs[self.active_tab].on_leave();
                self.tabs[self.active_tab].on_enter();
                self.flash_message = Some("Refreshed".to_string());
            }
            Some("tab" | "t") => {
                if let Some(n) = parts.get(1).and_then(|s| s.parse::<usize>().ok()) {
                    if n >= 1 && n <= self.tabs.len() && (n - 1) != self.active_tab {
                        self.tabs[self.active_tab].on_leave();
                        self.active_tab = n - 1;
                        self.tabs[self.active_tab].on_enter();
                    }
                } else {
                    self.flash_message = Some(format!("Usage: :tab <1-{}>", self.tabs.len()));
                }
            }
            Some(other) => {
                self.flash_message = Some(format!("Unknown command: {other}"));
            }
            None => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.flash_message = None;
                // Click on tab bar → switch tabs
                if mouse.row >= self.tab_bar_area.y
                    && mouse.row < self.tab_bar_area.y + self.tab_bar_area.height
                {
                    self.click_tab_bar(mouse.column);
                }
            }
            MouseEventKind::ScrollUp => {
                // Forward scroll as Up key to active tab
                let up = KeyEvent::new(KeyCode::Up, KeyModifiers::empty());
                // INTENTIONAL: handle_key return value is unused — scroll is best-effort UI interaction
                let _ = self.tabs[self.active_tab].handle_key(up);
            }
            MouseEventKind::ScrollDown => {
                // Forward scroll as Down key to active tab
                let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
                // INTENTIONAL: handle_key return value is unused — scroll is best-effort UI interaction
                let _ = self.tabs[self.active_tab].handle_key(down);
            }
            _ => {}
        }
    }

    /// Start a background sync (fetch from coordination branch).
    fn start_background_sync(&mut self) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        self.flash_message = Some("Syncing...".to_string());
        let (tx, rx) = mpsc::channel();
        self.sync_rx = Some(rx);
        let crosslink_dir = self.crosslink_dir.clone();

        std::thread::spawn(move || {
            let result = match SyncManager::new(&crosslink_dir) {
                Ok(sync_mgr) => {
                    // INTENTIONAL: cache init is best-effort — fetch below will report the real error
                    let _ = sync_mgr.init_cache();
                    match sync_mgr.fetch() {
                        Ok(()) => SyncResult {
                            cache_path: sync_mgr.cache_path().to_path_buf(),
                            error: None,
                        },
                        Err(e) => SyncResult {
                            cache_path: sync_mgr.cache_path().to_path_buf(),
                            error: Some(e.to_string()),
                        },
                    }
                }
                Err(e) => SyncResult {
                    cache_path: PathBuf::new(),
                    error: Some(e.to_string()),
                },
            };
            // INTENTIONAL: send failure means the receiver was dropped — TUI is shutting down
            let _ = tx.send(result);
        });
    }

    /// Poll for a completed background sync and apply results.
    fn poll_sync(&mut self) {
        let result = self.sync_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(result) = result {
            self.syncing = false;
            self.sync_rx = None;
            self.last_sync = Instant::now();

            if let Some(err) = result.error {
                self.flash_message = Some(format!("Sync error: {err}"));
            } else {
                // Hydrate local DB from the fetched coordination branch data
                if let Ok(db) = Database::open(&self.db_path) {
                    // INTENTIONAL: hydration failure is non-fatal — TUI shows stale data until next sync
                    let _ = hydrate_to_sqlite(&result.cache_path, &db);
                }
                // Refresh the active tab to show updated data
                self.tabs[self.active_tab].force_refresh();
                self.flash_message = Some("Synced".to_string());
            }
        }
    }

    fn click_tab_bar(&mut self, col: u16) {
        // Tab bar has borders (1 col each side) and tabs are rendered as
        // " Title " with dividers. Approximate positions by measuring tab titles.
        let inner_x = self.tab_bar_area.x + 1; // skip left border
        if col < inner_x {
            return;
        }
        let rel_col = col - inner_x;

        // Each tab is rendered as " <title> " with a divider character between them.
        // ratatui::TabsWidget uses " <title> │" for each tab.
        let mut offset: u16 = 0;
        for (idx, tab) in self.tabs.iter().enumerate() {
            let tab_width = tab.title().chars().count() as u16 + 2; // " title " padding
            let with_divider = tab_width + 1; // + "│"
            if rel_col >= offset && rel_col < offset + with_divider {
                if idx != self.active_tab {
                    self.tabs[self.active_tab].on_leave();
                    self.active_tab = idx;
                    self.tabs[self.active_tab].on_enter();
                }
                return;
            }
            offset += with_divider;
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Tab bar
                Constraint::Min(0),    // Content area
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        // Store areas for mouse hit detection
        self.tab_bar_area = chunks[0];

        self.render_tab_bar(frame, chunks[0]);
        self.tabs[self.active_tab].render(frame, chunks[1]);

        if self.command_mode {
            self.render_command_bar(frame, chunks[2]);
        } else if let Some(ref msg) = self.flash_message {
            let flash = Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(msg.as_str(), Style::default().fg(Color::Yellow)),
            ]))
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));
            frame.render_widget(flash, chunks[2]);
        } else {
            Self::render_status_bar(frame, chunks[2]);
        }

        if self.show_help {
            Self::render_help_overlay(frame);
        }
    }

    fn render_tab_bar(&self, frame: &mut Frame, area: Rect) {
        let titles: Vec<Line> = self.tabs.iter().map(|t| Line::from(t.title())).collect();

        let tabs = TabsWidget::new(titles)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(
                            "crosslink",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" tui ", Style::default().fg(Color::DarkGray)),
                    ])),
            )
            .select(self.active_tab)
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_widget(tabs, area);
    }

    fn render_command_bar(&self, frame: &mut Frame, area: Rect) {
        let input_spans = vec![
            Span::styled(":", Style::default().fg(Color::Cyan)),
            Span::raw(&self.command_input),
            Span::styled("█", Style::default().fg(Color::White)),
        ];
        let bar = Paragraph::new(Line::from(input_spans))
            .style(Style::default().bg(Color::Black).fg(Color::White));
        frame.render_widget(bar, area);
    }

    fn render_status_bar(frame: &mut Frame, area: Rect) {
        let keys = vec![
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(":Quit  "),
            Span::styled("Tab", Style::default().fg(Color::Cyan)),
            Span::raw(":Next  "),
            Span::styled("S-Tab", Style::default().fg(Color::Cyan)),
            Span::raw(":Prev  "),
            Span::styled("1-6", Style::default().fg(Color::Cyan)),
            Span::raw(":Jump  "),
            Span::styled("?", Style::default().fg(Color::Cyan)),
            Span::raw(":Help  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(":Sync"),
        ];

        let status = Paragraph::new(Line::from(keys))
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        frame.render_widget(status, area);
    }

    fn render_help_overlay(frame: &mut Frame) {
        let area = centered_rect(60, 70, frame.area());

        // Clear the background
        frame.render_widget(ratatui::widgets::Clear, area);

        let help_text = vec![
            Line::from(Span::styled(
                "Keyboard Shortcuts",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Global",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  q / Ctrl-c    Quit"),
            Line::from("  Tab           Next tab"),
            Line::from("  Shift-Tab     Previous tab"),
            Line::from("  1-6           Jump to tab"),
            Line::from("  :             Command palette"),
            Line::from("  ?             Toggle this help"),
            Line::from("  Mouse         Click tabs, scroll wheel"),
            Line::from(""),
            Line::from(Span::styled(
                "Issues List",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Up/Down / j/k Navigate issues"),
            Line::from("  Enter         View issue details"),
            Line::from("  f             Cycle status filter"),
            Line::from("  s             Cycle sort order"),
            Line::from("  r             Sync & refresh"),
            Line::from("  /             Search (type to filter)"),
            Line::from("  Esc           Clear search"),
            Line::from("  t             Tree view"),
            Line::from(""),
            Line::from(Span::styled(
                "Issue Detail",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Esc           Back to list"),
            Line::from("  Up/Down / j/k Scroll"),
            Line::from("  y             Copy to clipboard"),
            Line::from(""),
            Line::from(Span::styled(
                "Agents Tab",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Up/Down / j/k Navigate agents"),
            Line::from("  Enter         View agent details"),
            Line::from("  v             Cycle view (Agents/Locks/Trust)"),
            Line::from("  r             Sync & refresh"),
            Line::from("  Esc           Back to list"),
            Line::from(""),
            Line::from(Span::styled(
                "Knowledge Tab",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Up/Down / j/k Navigate pages"),
            Line::from("  Enter         Read page"),
            Line::from("  /             Search pages"),
            Line::from("  t             Cycle tag filter"),
            Line::from("  y             Copy page to clipboard"),
            Line::from("  r             Sync & refresh"),
            Line::from("  Esc           Back to list"),
            Line::from(""),
            Line::from(Span::styled(
                "Milestones Tab",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Up/Down / j/k Navigate milestones"),
            Line::from("  Enter         View milestone details"),
            Line::from("  f             Cycle status filter"),
            Line::from("  y             Copy to clipboard"),
            Line::from("  r             Sync & refresh"),
            Line::from("  Esc           Back to list"),
            Line::from(""),
            Line::from(Span::styled(
                "Config Tab",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Up/Down / j/k Scroll"),
            Line::from("  e             Full event log"),
            Line::from("  r             Sync & refresh"),
            Line::from("  Esc           Back to main"),
            Line::from(""),
            Line::from(Span::styled(
                "Command Palette (:)",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  :q / :quit    Quit"),
            Line::from("  :r / :refresh Refresh current tab"),
            Line::from("  :tab N        Jump to tab N"),
            Line::from("  :help         Show this help"),
            Line::from(""),
            Line::from(Span::styled(
                "Press ? or Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let help = Paragraph::new(help_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(
                            "Help",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                    ])),
            )
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::White).bg(Color::Black));

        frame.render_widget(help, area);
    }
}

/// RAII guard that restores the terminal on drop — ensures cleanup even on `?` errors.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Self {
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            // INTENTIONAL: terminal cleanup in panic hook must not itself panic — best-effort restore
            let _ = io::stdout().execute(DisableMouseCapture);
            let _ = disable_raw_mode();
            let _ = io::stdout().execute(LeaveAlternateScreen);
            // Invoke the original panic hook to preserve existing behavior (e.g. backtrace printing)
            original_hook(panic_info);
        }));
        TerminalGuard
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // INTENTIONAL: terminal cleanup in Drop must not panic — best-effort restore
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        // Restore the default panic hook (the TUI-specific hook is no longer needed)
        let _ = std::panic::take_hook();
    }
}

/// Interval between automatic background syncs.
const PERIODIC_SYNC_INTERVAL: Duration = Duration::from_secs(30);

/// Run the TUI application. Sets up terminal, runs event loop, cleans up on exit.
pub fn run(db: &Database, crosslink_dir: &Path) -> anyhow::Result<()> {
    // Startup sync — pull latest from coordination branch before entering TUI
    eprint!("Syncing...");
    if let Ok(sync_mgr) = SyncManager::new(crosslink_dir) {
        // INTENTIONAL: startup sync is best-effort — TUI works with stale local data if offline
        let _ = sync_mgr.init_cache();
        let _ = sync_mgr.fetch();
        let _ = hydrate_to_sqlite(sync_mgr.cache_path(), db);
    }
    eprintln!(" done.");

    let _guard = TerminalGuard::new();

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(EnableMouseCapture)?;

    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(db, crosslink_dir)?;

    // Main loop — non-blocking so we can poll for async data updates
    loop {
        terminal.draw(|frame| app.render(frame))?;

        // Poll only the active tab for async data that may have arrived
        app.tabs[app.active_tab].poll_updates();

        // Poll for background sync completion
        app.poll_sync();

        // Periodic background sync
        if app.last_sync.elapsed() > PERIODIC_SYNC_INTERVAL && !app.syncing {
            app.start_background_sync();
        }

        // Non-blocking event poll (50ms timeout keeps UI responsive for async updates)
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    // Ignore key release events (crossterm sends press + release on some platforms)
                    if key.kind != event::KeyEventKind::Press {
                        continue;
                    }
                    app.handle_key(key);
                }
                Event::Mouse(mouse) => {
                    app.handle_mouse(mouse);
                }
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }

    // _guard dropped here → cleanup runs automatically
    Ok(())
}

/// Helper to create a centered rectangle for overlays.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use tempfile::tempdir;

    fn make_key(code: KeyCode) -> KeyEvent {
        super::make_test_key(code)
    }

    fn make_key_with_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn setup_test_app() -> (App, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        // Simulate a .crosslink directory
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db_path = crosslink_dir.join("issues.db");
        let db = Database::open(&db_path).unwrap();
        db.create_issue("Test issue 1", Some("Description"), "high")
            .unwrap();
        db.create_issue("Test issue 2", None, "medium").unwrap();
        let app = App::new(&db, &crosslink_dir).unwrap();
        (app, dir)
    }

    #[test]
    fn test_app_initial_state() {
        let (app, _dir) = setup_test_app();
        assert_eq!(app.active_tab, 0);
        assert!(!app.show_help);
        assert!(!app.should_quit);
        assert_eq!(app.tabs.len(), 6);
    }

    #[test]
    fn test_tab_navigation_forward() {
        let (mut app, _dir) = setup_test_app();
        assert_eq!(app.active_tab, 0);
        app.handle_key(make_key(KeyCode::Tab));
        assert_eq!(app.active_tab, 1);
        app.handle_key(make_key(KeyCode::Tab));
        assert_eq!(app.active_tab, 2);
    }

    #[test]
    fn test_tab_navigation_wraps() {
        let (mut app, _dir) = setup_test_app();
        // Go to last tab
        for _ in 0..5 {
            app.handle_key(make_key(KeyCode::Tab));
        }
        assert_eq!(app.active_tab, 5);
        // Should wrap to 0
        app.handle_key(make_key(KeyCode::Tab));
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn test_tab_navigation_backward() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::BackTab));
        assert_eq!(app.active_tab, 5);
        app.handle_key(make_key(KeyCode::BackTab));
        assert_eq!(app.active_tab, 4);
    }

    #[test]
    fn test_direct_tab_selection() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char('3')));
        assert_eq!(app.active_tab, 2);
        app.handle_key(make_key(KeyCode::Char('1')));
        assert_eq!(app.active_tab, 0);
        app.handle_key(make_key(KeyCode::Char('6')));
        assert_eq!(app.active_tab, 5);
    }

    #[test]
    fn test_quit_with_q() {
        let (mut app, _dir) = setup_test_app();
        assert!(!app.should_quit);
        app.handle_key(make_key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_quit_with_ctrl_c() {
        let (mut app, _dir) = setup_test_app();
        assert!(!app.should_quit);
        app.handle_key(make_key_with_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn test_help_toggle() {
        let (mut app, _dir) = setup_test_app();
        assert!(!app.show_help);
        app.handle_key(make_key(KeyCode::Char('?')));
        assert!(app.show_help);
        // While help is shown, other keys should not change tabs
        app.handle_key(make_key(KeyCode::Tab));
        assert_eq!(app.active_tab, 0);
        // ? dismisses help
        app.handle_key(make_key(KeyCode::Char('?')));
        assert!(!app.show_help);
    }

    #[test]
    fn test_help_dismiss_with_esc() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char('?')));
        assert!(app.show_help);
        app.handle_key(make_key(KeyCode::Esc));
        assert!(!app.show_help);
    }

    #[test]
    fn test_render_does_not_panic() {
        let (mut app, _dir) = setup_test_app();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
    }

    #[test]
    fn test_render_with_help_overlay() {
        let (mut app, _dir) = setup_test_app();
        app.show_help = true;
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
    }

    #[test]
    fn test_render_each_tab() {
        let (mut app, _dir) = setup_test_app();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        for i in 0..6 {
            app.tabs[app.active_tab].on_leave();
            app.active_tab = i;
            app.tabs[app.active_tab].on_enter();
            terminal.draw(|frame| app.render(frame)).unwrap();
        }
    }

    #[test]
    fn test_centered_rect() {
        let area = Rect::new(0, 0, 100, 50);
        let centered = centered_rect(60, 70, area);
        // Should be roughly centered
        assert!(centered.x > 0);
        assert!(centered.y > 0);
        assert!(centered.width < area.width);
        assert!(centered.height < area.height);
    }

    // ── Command palette tests ────────────────────────────────────────

    #[test]
    fn test_command_mode_enter_exit() {
        let (mut app, _dir) = setup_test_app();
        assert!(!app.command_mode);
        // ':' enters command mode
        app.handle_key(make_key(KeyCode::Char(':')));
        assert!(app.command_mode);
        assert!(app.command_input.is_empty());
        // Esc exits command mode
        app.handle_key(make_key(KeyCode::Esc));
        assert!(!app.command_mode);
    }

    #[test]
    fn test_command_mode_typing() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char(':')));
        app.handle_key(make_key(KeyCode::Char('t')));
        app.handle_key(make_key(KeyCode::Char('a')));
        app.handle_key(make_key(KeyCode::Char('b')));
        assert_eq!(app.command_input, "tab");
        app.handle_key(make_key(KeyCode::Backspace));
        assert_eq!(app.command_input, "ta");
    }

    #[test]
    fn test_command_quit() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char(':')));
        app.handle_key(make_key(KeyCode::Char('q')));
        app.handle_key(make_key(KeyCode::Enter));
        assert!(app.should_quit);
    }

    #[test]
    fn test_command_tab_switch() {
        let (mut app, _dir) = setup_test_app();
        assert_eq!(app.active_tab, 0);
        app.handle_key(make_key(KeyCode::Char(':')));
        for c in "tab 3".chars() {
            app.handle_key(make_key(KeyCode::Char(c)));
        }
        app.handle_key(make_key(KeyCode::Enter));
        assert!(!app.command_mode);
        assert_eq!(app.active_tab, 2);
    }

    #[test]
    fn test_command_refresh() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char(':')));
        app.handle_key(make_key(KeyCode::Char('r')));
        app.handle_key(make_key(KeyCode::Enter));
        assert!(!app.command_mode);
        assert_eq!(app.flash_message.as_deref(), Some("Refreshed"));
    }

    #[test]
    fn test_command_unknown() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char(':')));
        for c in "foo".chars() {
            app.handle_key(make_key(KeyCode::Char(c)));
        }
        app.handle_key(make_key(KeyCode::Enter));
        assert_eq!(app.flash_message.as_deref(), Some("Unknown command: foo"));
    }

    #[test]
    fn test_command_help() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char(':')));
        for c in "help".chars() {
            app.handle_key(make_key(KeyCode::Char(c)));
        }
        app.handle_key(make_key(KeyCode::Enter));
        assert!(app.show_help);
    }

    #[test]
    fn test_flash_cleared_on_keypress() {
        let (mut app, _dir) = setup_test_app();
        app.flash_message = Some("test".to_string());
        app.handle_key(make_key(KeyCode::Char('j')));
        assert!(app.flash_message.is_none());
    }

    // ── Mouse tests ──────────────────────────────────────────────────

    #[test]
    fn test_mouse_scroll_down() {
        let (mut app, _dir) = setup_test_app();
        // First render to populate areas
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::empty(),
        };
        app.handle_mouse(mouse);
        // Should not panic — scroll event forwarded to active tab
    }

    #[test]
    fn test_mouse_scroll_up() {
        let (mut app, _dir) = setup_test_app();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::empty(),
        };
        app.handle_mouse(mouse);
    }

    #[test]
    fn test_mouse_click_tab_bar() {
        let (mut app, _dir) = setup_test_app();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        assert_eq!(app.active_tab, 0);
        // Click roughly where tab 2 (Agents) would be — after "Issues" tab
        // "Issues" = 6 chars + 2 padding + 1 divider = 9 cols from inner_x
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: app.tab_bar_area.x + 1 + 10, // past first tab
            row: app.tab_bar_area.y + 1,
            modifiers: KeyModifiers::empty(),
        };
        app.handle_mouse(mouse);
        assert_eq!(app.active_tab, 1);
    }

    #[test]
    fn test_render_command_bar() {
        let (mut app, _dir) = setup_test_app();
        app.command_mode = true;
        app.command_input = "tab 3".to_string();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
    }

    #[test]
    fn test_render_flash_message() {
        let (mut app, _dir) = setup_test_app();
        app.flash_message = Some("Copied!".to_string());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
    }
}
