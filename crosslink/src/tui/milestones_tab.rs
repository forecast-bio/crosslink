use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use std::cell::RefCell;
use std::path::PathBuf;

use crate::db::Database;

use super::{Tab, TabAction};

/// Background color for highlighted/selected rows (256-color palette, dark gray).
const HIGHLIGHT_BG: Color = Color::Indexed(236);

/// Which sub-view is active.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ViewMode {
    List,
    Detail,
}

/// Status filter for milestone list.
#[derive(Clone, Copy, Debug, PartialEq)]
enum StatusFilter {
    Open,
    Closed,
    All,
}

impl StatusFilter {
    fn next(self) -> Self {
        match self {
            StatusFilter::Open => StatusFilter::Closed,
            StatusFilter::Closed => StatusFilter::All,
            StatusFilter::All => StatusFilter::Open,
        }
    }

    fn label(self) -> &'static str {
        match self {
            StatusFilter::Open => "Open",
            StatusFilter::Closed => "Closed",
            StatusFilter::All => "All",
        }
    }

    fn db_arg(self) -> Option<&'static str> {
        match self {
            StatusFilter::Open => None, // default = open
            StatusFilter::Closed => Some("closed"),
            StatusFilter::All => Some("all"),
        }
    }
}

/// A row in the milestones list table.
struct MilestoneRow {
    id: i64,
    name: String,
    status: String,
    closed_count: usize,
    total_count: usize,
    description: Option<String>,
    created_at: String,
    closed_at: Option<String>,
}

/// Detail view for a selected milestone.
struct MilestoneDetail {
    id: i64,
    name: String,
    status: String,
    description: Option<String>,
    created_at: String,
    closed_at: Option<String>,
    open_count: usize,
    closed_count: usize,
    total_count: usize,
    issues: Vec<MilestoneIssue>,
}

/// An issue within a milestone detail.
struct MilestoneIssue {
    id: i64,
    title: String,
    status: String,
    priority: String,
}

/// The Milestones tab — progress tracking for grouped issues.
pub struct MilestonesTab {
    db_path: PathBuf,
    view_mode: ViewMode,
    milestones: Vec<MilestoneRow>,
    selected: usize,
    status_filter: StatusFilter,
    detail: Option<MilestoneDetail>,
    detail_scroll: u16,
    status_msg: String,
    error_msg: Option<String>,
    /// TableState for list view scroll-to-follow.
    list_table_state: RefCell<TableState>,
}

impl MilestonesTab {
    pub fn new(db: &Database, db_path: &std::path::Path) -> Self {
        let mut tab = MilestonesTab {
            db_path: db_path.to_path_buf(),
            view_mode: ViewMode::List,
            milestones: Vec::new(),
            selected: 0,
            status_filter: StatusFilter::Open,
            detail: None,
            detail_scroll: 0,
            status_msg: String::new(),
            error_msg: None,
            list_table_state: RefCell::new(TableState::default()),
        };
        tab.load_milestones(db);
        tab
    }

    fn open_db(&self) -> Option<Database> {
        Database::open(&self.db_path).ok()
    }

    fn load_milestones(&mut self, db: &Database) {
        self.error_msg = None;
        match db.list_milestones(self.status_filter.db_arg()) {
            Ok(milestones) => {
                self.milestones = milestones
                    .into_iter()
                    .map(|m| {
                        let issues = db.get_milestone_issues(m.id).unwrap_or_default();
                        let closed_count = issues.iter().filter(|i| i.status == "closed").count();
                        let total_count = issues.len();
                        MilestoneRow {
                            id: m.id,
                            name: m.name,
                            status: m.status,
                            closed_count,
                            total_count,
                            description: m.description,
                            created_at: m.created_at.format("%Y-%m-%d").to_string(),
                            closed_at: m.closed_at.map(|d| d.format("%Y-%m-%d").to_string()),
                        }
                    })
                    .collect();
                let total = self.milestones.len();
                self.status_msg = format!("{total} milestone{}", if total == 1 { "" } else { "s" });
            }
            Err(e) => {
                self.error_msg = Some(format!("Failed to load milestones: {e}"));
            }
        }
        // Clamp selection
        if self.selected >= self.milestones.len() && !self.milestones.is_empty() {
            self.selected = self.milestones.len() - 1;
        }
    }

    fn refresh(&mut self) {
        if let Some(db) = self.open_db() {
            self.load_milestones(&db);
        }
    }

    fn open_detail(&mut self) {
        if self.milestones.is_empty() {
            return;
        }
        let row = &self.milestones[self.selected];
        let milestone_id = row.id;

        if let Some(db) = self.open_db() {
            let issues: Vec<MilestoneIssue> = db
                .get_milestone_issues(milestone_id)
                .unwrap_or_default()
                .into_iter()
                .map(|i| MilestoneIssue {
                    id: i.id,
                    title: i.title,
                    status: i.status,
                    priority: i.priority,
                })
                .collect();

            let closed_count = issues.iter().filter(|i| i.status == "closed").count();
            let total_count = issues.len();
            let open_count = total_count - closed_count;

            self.detail = Some(MilestoneDetail {
                id: row.id,
                name: row.name.clone(),
                status: row.status.clone(),
                description: row.description.clone(),
                created_at: row.created_at.clone(),
                closed_at: row.closed_at.clone(),
                open_count,
                closed_count,
                total_count,
                issues,
            });
            self.detail_scroll = 0;
            self.view_mode = ViewMode::Detail;
        }
    }

    // ── Rendering ────────────────────────────────────────────────────

    fn render_list(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);

        // Header
        let header_spans = vec![
            Span::styled(
                " Milestones",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({}) ", self.status_msg),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("Filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.status_filter.label(),
                Style::default().fg(Color::Yellow),
            ),
        ];
        frame.render_widget(Paragraph::new(Line::from(header_spans)), chunks[0]);

        if let Some(ref err) = self.error_msg {
            let error = Paragraph::new(Line::from(Span::styled(
                err.as_str(),
                Style::default().fg(Color::Red),
            )))
            .block(Block::default().borders(Borders::ALL));
            frame.render_widget(error, chunks[1]);
            return;
        }

        if self.milestones.is_empty() {
            let empty = Paragraph::new(Line::from(Span::styled(
                " No milestones found",
                Style::default().fg(Color::DarkGray),
            )))
            .block(Block::default().borders(Borders::ALL));
            frame.render_widget(empty, chunks[1]);
            return;
        }

        // Table
        let header = Row::new(vec!["ID", "Name", "Status", "Issues", "Progress", ""])
            .style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        let rows: Vec<Row> = self
            .milestones
            .iter()
            .map(|m| {
                let pct = if m.total_count > 0 {
                    (m.closed_count * 100) / m.total_count
                } else {
                    0
                };
                let bar = progress_bar(m.closed_count, m.total_count, 12);
                let issues_str = format!("{}/{}", m.closed_count, m.total_count);
                let pct_str = format!("{pct}%");

                let status_style = if m.status == "closed" {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Yellow)
                };

                Row::new(vec![
                    ratatui::widgets::Cell::from(format!("#{}", m.id)),
                    ratatui::widgets::Cell::from(m.name.clone()),
                    ratatui::widgets::Cell::from(m.status.clone()).style(status_style),
                    ratatui::widgets::Cell::from(issues_str),
                    ratatui::widgets::Cell::from(bar).style(Style::default().fg(Color::Cyan)),
                    ratatui::widgets::Cell::from(pct_str)
                        .style(Style::default().fg(Color::DarkGray)),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(6),
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(14),
            Constraint::Length(5),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL))
            .row_highlight_style(Style::default().bg(HIGHLIGHT_BG));

        let mut state = self.list_table_state.borrow_mut();
        state.select(Some(self.selected));
        frame.render_stateful_widget(table, chunks[1], &mut state);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let detail = match &self.detail {
            Some(d) => d,
            None => return,
        };

        let mut lines: Vec<Line> = Vec::new();

        // Title
        lines.push(Line::from(vec![
            Span::styled(
                format!(" #{} — ", detail.id),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                detail.name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(
            " ─────────────────────────────────────────────────",
        ));

        // Metadata
        let status_color = if detail.status == "closed" {
            Color::Green
        } else {
            Color::Yellow
        };
        lines.push(Line::from(vec![
            Span::styled(" Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(detail.status.clone(), Style::default().fg(status_color)),
            Span::styled("    Created: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&detail.created_at),
        ]));
        if let Some(ref closed) = detail.closed_at {
            lines.push(Line::from(vec![
                Span::styled(" Closed: ", Style::default().fg(Color::DarkGray)),
                Span::raw(closed.as_str()),
            ]));
        }

        // Progress
        let pct = if detail.total_count > 0 {
            (detail.closed_count * 100) / detail.total_count
        } else {
            0
        };
        let bar = progress_bar(detail.closed_count, detail.total_count, 20);
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" Progress: ", Style::default().fg(Color::DarkGray)),
            Span::styled(bar, Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("  {}/{} ({pct}%)", detail.closed_count, detail.total_count),
                Style::default().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} open", detail.open_count),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("  •  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} closed", detail.closed_count),
                Style::default().fg(Color::Green),
            ),
        ]));

        // Description
        if let Some(ref desc) = detail.description {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Description",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for line in desc.lines() {
                lines.push(Line::from(format!("   {line}")));
            }
        }

        // Issues list
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(" Issues ({})", detail.issues.len()),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(
            " ─────────────────────────────────────────────────",
        ));

        if detail.issues.is_empty() {
            lines.push(Line::from(Span::styled(
                "   No issues assigned",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for issue in &detail.issues {
                let status_icon = if issue.status == "closed" {
                    "✓"
                } else {
                    "○"
                };
                let status_color = if issue.status == "closed" {
                    Color::Green
                } else {
                    Color::White
                };
                let priority_color = match issue.priority.as_str() {
                    "high" => Color::Red,
                    "medium" => Color::Yellow,
                    _ => Color::DarkGray,
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("   {status_icon} "),
                        Style::default().fg(status_color),
                    ),
                    Span::styled(
                        format!("#{:<4} ", issue.id),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("●{:<7}", issue.priority),
                        Style::default().fg(priority_color),
                    ),
                    Span::styled(issue.title.clone(), Style::default().fg(status_color)),
                ]));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Esc:Back  ↑↓/j/k:Scroll  y:Copy",
            Style::default().fg(Color::DarkGray),
        )));

        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL))
            .scroll((self.detail_scroll, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(para, area);
    }

    // ── Key handling ─────────────────────────────────────────────────

    fn handle_list_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.milestones.is_empty() {
                    self.selected = (self.selected + 1).min(self.milestones.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                TabAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = 0;
                TabAction::Consumed
            }
            KeyCode::End | KeyCode::Char('G') => {
                if !self.milestones.is_empty() {
                    self.selected = self.milestones.len() - 1;
                }
                TabAction::Consumed
            }
            KeyCode::Enter => {
                self.open_detail();
                TabAction::Consumed
            }
            KeyCode::Char('f') => {
                self.status_filter = self.status_filter.next();
                self.refresh();
                TabAction::Consumed
            }
            KeyCode::Char('r') => {
                self.refresh();
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.view_mode = ViewMode::List;
                self.detail = None;
                TabAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
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
            KeyCode::Home | KeyCode::Char('g') => {
                self.detail_scroll = 0;
                TabAction::Consumed
            }
            KeyCode::Char('y') => self.copy_detail_to_clipboard(),
            _ => TabAction::NotHandled,
        }
    }

    fn copy_detail_to_clipboard(&self) -> TabAction {
        if let Some(ref d) = self.detail {
            let mut text = format!(
                "Milestone #{} — {}\nStatus: {}  Progress: {}/{}\n",
                d.id, d.name, d.status, d.closed_count, d.total_count
            );
            if let Some(ref desc) = d.description {
                text.push_str(&format!("\n{desc}\n"));
            }
            if !d.issues.is_empty() {
                text.push_str(&format!("\nIssues ({}):\n", d.issues.len()));
                for issue in &d.issues {
                    let marker = if issue.status == "closed" {
                        "✓"
                    } else {
                        "○"
                    };
                    text.push_str(&format!(
                        "  {marker} #{} [{}] {}\n",
                        issue.id, issue.priority, issue.title
                    ));
                }
            }
            let ok = super::copy_to_clipboard(&text);
            let msg = if ok {
                "Copied to clipboard"
            } else {
                "Clipboard copy failed"
            };
            return TabAction::Flash(msg.to_string());
        }
        TabAction::Consumed
    }
}

impl Tab for MilestonesTab {
    fn title(&self) -> &str {
        "Milestones"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self.view_mode {
            ViewMode::List => self.render_list(frame, area),
            ViewMode::Detail => self.render_detail(frame, area),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabAction {
        match self.view_mode {
            ViewMode::List => self.handle_list_key(key),
            ViewMode::Detail => self.handle_detail_key(key),
        }
    }

    fn on_enter(&mut self) {
        self.refresh();
    }

    fn on_leave(&mut self) {}
}

/// Build a text progress bar: `████████░░░░` style.
fn progress_bar(done: usize, total: usize, width: usize) -> String {
    if total == 0 {
        return "░".repeat(width);
    }
    let filled = (done * width) / total;
    let empty = width - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
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

    fn setup_tab() -> (MilestonesTab, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("issues.db");
        let db = Database::open(&db_path).unwrap();

        // Create milestones with issues
        db.create_milestone("v1.0 Release", Some("First stable release"))
            .unwrap();
        db.create_milestone("Knowledge MVP", None).unwrap();
        db.create_milestone("Multi-Agent GA", Some("General availability"))
            .unwrap();

        // Create some issues and assign to milestones
        let i1 = db.create_issue("Setup CI", None, "high").unwrap();
        let i2 = db.create_issue("Add tests", None, "medium").unwrap();
        let i3 = db
            .create_issue("Write docs", Some("Documentation"), "low")
            .unwrap();
        let i4 = db.create_issue("Fix bug", None, "high").unwrap();

        db.add_issue_to_milestone(1, i1).unwrap();
        db.add_issue_to_milestone(1, i2).unwrap();
        db.add_issue_to_milestone(1, i3).unwrap();
        db.add_issue_to_milestone(2, i4).unwrap();

        // Close one issue
        db.close_issue(i1).unwrap();

        let tab = MilestonesTab::new(&db, &db_path);
        (tab, dir)
    }

    #[test]
    fn test_title() {
        let (tab, _dir) = setup_tab();
        assert_eq!(tab.title(), "Milestones");
    }

    #[test]
    fn test_initial_state() {
        let (tab, _dir) = setup_tab();
        assert_eq!(tab.view_mode, ViewMode::List);
        assert_eq!(tab.selected, 0);
        assert_eq!(tab.status_filter, StatusFilter::Open);
        assert_eq!(tab.milestones.len(), 3);
    }

    #[test]
    fn test_milestone_issue_counts() {
        let (tab, _dir) = setup_tab();
        // Milestones ordered by ID DESC, so index 0 = Multi-Agent GA (#3),
        // index 2 = v1.0 Release (#1)
        let m_v1 = tab
            .milestones
            .iter()
            .find(|m| m.name == "v1.0 Release")
            .unwrap();
        assert_eq!(m_v1.total_count, 3);
        assert_eq!(m_v1.closed_count, 1);
        let m_k = tab
            .milestones
            .iter()
            .find(|m| m.name == "Knowledge MVP")
            .unwrap();
        assert_eq!(m_k.total_count, 1);
        assert_eq!(m_k.closed_count, 0);
    }

    #[test]
    fn test_navigate_down() {
        let (mut tab, _dir) = setup_tab();
        assert_eq!(tab.selected, 0);
        tab.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.selected, 1);
        tab.handle_key(make_key(KeyCode::Down));
        assert_eq!(tab.selected, 2);
        // Should not go past last
        tab.handle_key(make_key(KeyCode::Down));
        assert_eq!(tab.selected, 2);
    }

    #[test]
    fn test_navigate_up() {
        let (mut tab, _dir) = setup_tab();
        tab.selected = 2;
        tab.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(tab.selected, 1);
        tab.handle_key(make_key(KeyCode::Up));
        assert_eq!(tab.selected, 0);
        // Should not go below 0
        tab.handle_key(make_key(KeyCode::Up));
        assert_eq!(tab.selected, 0);
    }

    #[test]
    fn test_home_end() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::End));
        assert_eq!(tab.selected, 2);
        tab.handle_key(make_key(KeyCode::Home));
        assert_eq!(tab.selected, 0);
    }

    #[test]
    fn test_status_filter_cycle() {
        let (mut tab, _dir) = setup_tab();
        assert_eq!(tab.status_filter, StatusFilter::Open);
        tab.handle_key(make_key(KeyCode::Char('f')));
        assert_eq!(tab.status_filter, StatusFilter::Closed);
        tab.handle_key(make_key(KeyCode::Char('f')));
        assert_eq!(tab.status_filter, StatusFilter::All);
        tab.handle_key(make_key(KeyCode::Char('f')));
        assert_eq!(tab.status_filter, StatusFilter::Open);
    }

    #[test]
    fn test_open_detail() {
        let (mut tab, _dir) = setup_tab();
        // First entry is the last-created milestone (ID DESC order)
        let first_name = tab.milestones[0].name.clone();
        tab.handle_key(make_key(KeyCode::Enter));
        assert_eq!(tab.view_mode, ViewMode::Detail);
        assert!(tab.detail.is_some());
        let detail = tab.detail.as_ref().unwrap();
        assert_eq!(detail.name, first_name);
    }

    #[test]
    fn test_detail_scroll() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::Enter));
        assert_eq!(tab.detail_scroll, 0);
        tab.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.detail_scroll, 1);
        tab.handle_key(make_key(KeyCode::PageDown));
        assert_eq!(tab.detail_scroll, 11);
        tab.handle_key(make_key(KeyCode::PageUp));
        assert_eq!(tab.detail_scroll, 1);
        tab.handle_key(make_key(KeyCode::Char('g')));
        assert_eq!(tab.detail_scroll, 0);
    }

    #[test]
    fn test_detail_back() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::Enter));
        assert_eq!(tab.view_mode, ViewMode::Detail);
        tab.handle_key(make_key(KeyCode::Esc));
        assert_eq!(tab.view_mode, ViewMode::List);
        assert!(tab.detail.is_none());
    }

    #[test]
    fn test_unhandled_key() {
        let (mut tab, _dir) = setup_tab();
        let result = tab.handle_key(make_key(KeyCode::Char('x')));
        assert!(matches!(result, TabAction::NotHandled));
    }

    #[test]
    fn test_empty_list_safety() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("issues.db");
        let db = Database::open(&db_path).unwrap();
        let mut tab = MilestonesTab::new(&db, &db_path);
        assert!(tab.milestones.is_empty());
        // Navigation on empty list should not panic
        tab.handle_key(make_key(KeyCode::Down));
        tab.handle_key(make_key(KeyCode::Up));
        tab.handle_key(make_key(KeyCode::Enter));
        assert_eq!(tab.view_mode, ViewMode::List);
    }

    #[test]
    fn test_render_list_no_panic() {
        let (tab, _dir) = setup_tab();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_detail_no_panic() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::Enter));
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_empty_list_no_panic() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("issues.db");
        let db = Database::open(&db_path).unwrap();
        let tab = MilestonesTab::new(&db, &db_path);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_progress_bar() {
        assert_eq!(progress_bar(0, 10, 10), "░░░░░░░░░░");
        assert_eq!(progress_bar(5, 10, 10), "█████░░░░░");
        assert_eq!(progress_bar(10, 10, 10), "██████████");
        assert_eq!(progress_bar(0, 0, 10), "░░░░░░░░░░");
        assert_eq!(progress_bar(1, 3, 12), "████░░░░░░░░");
    }

    #[test]
    fn test_refresh_key() {
        let (mut tab, _dir) = setup_tab();
        let result = tab.handle_key(make_key(KeyCode::Char('r')));
        assert!(matches!(result, TabAction::Consumed));
    }

    #[test]
    fn test_closed_filter() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("issues.db");
        let db = Database::open(&db_path).unwrap();
        db.create_milestone("Open one", None).unwrap();
        db.create_milestone("Closed one", None).unwrap();
        db.close_milestone(2).unwrap();

        let mut tab = MilestonesTab::new(&db, &db_path);
        // Default: open only
        assert_eq!(tab.milestones.len(), 1);
        assert_eq!(tab.milestones[0].name, "Open one");

        // Switch to closed
        tab.status_filter = StatusFilter::Closed;
        tab.refresh();
        assert_eq!(tab.milestones.len(), 1);
        assert_eq!(tab.milestones[0].name, "Closed one");

        // Switch to all
        tab.status_filter = StatusFilter::All;
        tab.refresh();
        assert_eq!(tab.milestones.len(), 2);
    }
}
