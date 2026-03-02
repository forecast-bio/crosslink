pub mod agents_tab;
pub mod issues_tab;
pub mod tabs;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
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
}

/// Top-level TUI application state.
pub struct App {
    tabs: Vec<Box<dyn Tab>>,
    active_tab: usize,
    show_help: bool,
    should_quit: bool,
}

impl App {
    pub fn new(db: &Database, crosslink_dir: &Path) -> anyhow::Result<Self> {
        let db_path = crosslink_dir.join("issues.db");
        let issues_tab = issues_tab::IssuesTab::new(db, &db_path)?;
        let agents_tab = agents_tab::AgentsTab::new(crosslink_dir);
        let mut tabs: Vec<Box<dyn Tab>> = vec![Box::new(issues_tab), Box::new(agents_tab)];

        // Placeholder tabs for future phases
        tabs.push(Box::new(tabs::PlaceholderTab::new("Knowledge", 3)));
        tabs.push(Box::new(tabs::PlaceholderTab::new("Milestones", 4)));
        tabs.push(Box::new(tabs::PlaceholderTab::new("Config", 5)));

        // Activate the first tab
        let mut app = App {
            tabs,
            active_tab: 0,
            show_help: false,
            should_quit: false,
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
        // Help overlay consumes all keys except ? and Esc to dismiss
        if self.show_help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.show_help = false,
                _ => {}
            }
            return;
        }

        // Let the active tab handle the key first
        match self.tabs[self.active_tab].handle_key(key) {
            TabAction::Consumed => return,
            TabAction::Quit => {
                self.should_quit = true;
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

    fn render(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Tab bar
                Constraint::Min(0),    // Content area
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        self.render_tab_bar(frame, chunks[0]);
        self.tabs[self.active_tab].render(frame, chunks[1]);
        self.render_status_bar(frame, chunks[2]);

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
            Line::from("  ?             Toggle this help"),
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

/// Run the TUI application. Sets up terminal, runs event loop, cleans up on exit.
pub fn run(db: &Database, crosslink_dir: &Path) -> anyhow::Result<()> {
    // Install panic hook that restores terminal before printing panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(db, crosslink_dir)?;

    // Main loop
    loop {
        terminal.draw(|frame| app.render(frame))?;

        if let Event::Key(key) = event::read()? {
            // Ignore key release events (crossterm sends press + release on some platforms)
            if key.kind != event::KeyEventKind::Press {
                continue;
            }
            app.handle_key(key);
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

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
        let (app, _dir) = setup_test_app();
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
}
