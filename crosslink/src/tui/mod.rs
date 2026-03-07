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
use std::path::Path;
use std::time::Duration;

use crate::db::Database;

/// Action returned by a tab's key handler to communicate with the App.
pub enum TabAction {
    /// Key was consumed by the tab.
    Consumed,
    /// Key was not handled; App should process it.
    NotHandled,
    /// Request the app to quit (available for future tab use).
    #[allow(dead_code)]
    Quit,
    /// Show a flash message to the user.
    Flash(String),
}

/// Trait that each tab panel must implement.
pub trait Tab {
    fn title(&self) -> &str;
    fn render(&self, frame: &mut Frame, area: Rect);
    fn handle_key(&mut self, key: KeyEvent) -> TabAction;
    /// Called when this tab becomes the active tab.
    fn on_enter(&mut self);
    /// Called when this tab loses focus.
    fn on_leave(&mut self);
    /// Poll for async data updates (called each event-loop tick).
    fn poll_updates(&mut self) {}
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
    let result = std::process::Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait()
        });
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result: Result<std::process::ExitStatus, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    result.map(|s| s.success()).unwrap_or(false)
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
}

impl App {
    pub fn new(db: &Database, crosslink_dir: &Path) -> anyhow::Result<Self> {
        let db_path = crosslink_dir.join("issues.db");
        let issues_tab = issues_tab::IssuesTab::new(db, &db_path)?;
        let agents_tab = agents_tab::AgentsTab::new(crosslink_dir);
        let knowledge_tab = knowledge_tab::KnowledgeTab::new(crosslink_dir);
        let milestones_tab = milestones_tab::MilestonesTab::new(db, &db_path);
        let config_tab = config_tab::ConfigTab::new(db, &db_path, crosslink_dir);
        let tabs: Vec<Box<dyn Tab>> = vec![
            Box::new(issues_tab),
            Box::new(agents_tab),
            Box::new(knowledge_tab),
            Box::new(milestones_tab),
            Box::new(config_tab),
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
                self.should_quit = true
            }
            KeyCode::Tab => self.next_tab(),
            KeyCode::BackTab => self.prev_tab(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(':') => {
                self.command_mode = true;
                self.command_input.clear();
            }
            // Number keys 1-5 for direct tab selection
            KeyCode::Char(c @ '1'..='5') => {
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
                    self.flash_message = Some("Usage: :tab <1-5>".to_string());
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
                let _ = self.tabs[self.active_tab].handle_key(up);
            }
            MouseEventKind::ScrollDown => {
                // Forward scroll as Down key to active tab
                let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
                let _ = self.tabs[self.active_tab].handle_key(down);
            }
            _ => {}
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
            self.render_status_bar(frame, chunks[2]);
        }

        if self.show_help {
            self.render_help_overlay(frame);
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

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        let keys = vec![
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(":Quit  "),
            Span::styled("Tab", Style::default().fg(Color::Cyan)),
            Span::raw(":Next  "),
            Span::styled("S-Tab", Style::default().fg(Color::Cyan)),
            Span::raw(":Prev  "),
            Span::styled("1-5", Style::default().fg(Color::Cyan)),
            Span::raw(":Jump  "),
            Span::styled("?", Style::default().fg(Color::Cyan)),
            Span::raw(":Help  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(":Refresh"),
        ];

        let status = Paragraph::new(Line::from(keys))
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        frame.render_widget(status, area);
    }

    fn render_help_overlay(&self, frame: &mut Frame) {
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
            Line::from("  1-5           Jump to tab"),
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
            Line::from("  r             Refresh data"),
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
            Line::from("  r             Refresh data"),
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
            Line::from("  r             Refresh"),
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
            Line::from("  r             Refresh"),
            Line::from("  Esc           Back to list"),
            Line::from(""),
            Line::from(Span::styled(
                "Config Tab",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Up/Down / j/k Scroll"),
            Line::from("  e             Full event log"),
            Line::from("  r             Refresh"),
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
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

struct TerminalGuard {
    original_hook: Option<PanicHook>,
}

impl TerminalGuard {
    fn new() -> Self {
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|panic_info| {
            let _ = io::stdout().execute(DisableMouseCapture);
            let _ = disable_raw_mode();
            let _ = io::stdout().execute(LeaveAlternateScreen);
            // Print the panic info manually since we can't call the original hook here
            eprintln!("{panic_info}");
        }));
        TerminalGuard {
            original_hook: Some(original_hook),
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        // Restore the original panic hook (fixes H4: hook chaining)
        if let Some(hook) = self.original_hook.take() {
            std::panic::set_hook(hook);
        }
    }
}

/// Run the TUI application. Sets up terminal, runs event loop, cleans up on exit.
pub fn run(db: &Database, crosslink_dir: &Path) -> anyhow::Result<()> {
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

        // Poll all tabs for async data that may have arrived
        for tab in &mut app.tabs {
            tab.poll_updates();
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
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
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
        assert_eq!(app.tabs.len(), 5);
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
        for _ in 0..4 {
            app.handle_key(make_key(KeyCode::Tab));
        }
        assert_eq!(app.active_tab, 4);
        // Should wrap to 0
        app.handle_key(make_key(KeyCode::Tab));
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn test_tab_navigation_backward() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::BackTab));
        assert_eq!(app.active_tab, 4);
        app.handle_key(make_key(KeyCode::BackTab));
        assert_eq!(app.active_tab, 3);
    }

    #[test]
    fn test_direct_tab_selection() {
        let (mut app, _dir) = setup_test_app();
        app.handle_key(make_key(KeyCode::Char('3')));
        assert_eq!(app.active_tab, 2);
        app.handle_key(make_key(KeyCode::Char('1')));
        assert_eq!(app.active_tab, 0);
        app.handle_key(make_key(KeyCode::Char('5')));
        assert_eq!(app.active_tab, 4);
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

        for i in 0..5 {
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
