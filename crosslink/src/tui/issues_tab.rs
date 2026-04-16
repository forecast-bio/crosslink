use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::db::Database;
use crate::models::{Comment, Issue};

use super::{StatusFilter, TabAction, HIGHLIGHT_BG};

/// Sort options for the issue list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    IdDesc,
    IdAsc,
    Priority,
    Updated,
}

impl SortOrder {
    const fn next(self) -> Self {
        match self {
            Self::IdDesc => Self::IdAsc,
            Self::IdAsc => Self::Priority,
            Self::Priority => Self::Updated,
            Self::Updated => Self::IdDesc,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::IdDesc => "ID (newest)",
            Self::IdAsc => "ID (oldest)",
            Self::Priority => "Priority",
            Self::Updated => "Updated",
        }
    }
}

/// View mode for the issues tab.
#[derive(Clone, Copy, PartialEq)]
enum IssueViewMode {
    List,
    Detail,
    Tree,
}

/// Data for the detail view.
struct IssueDetail {
    issue: Issue,
    labels: Vec<String>,
    comments: Vec<Comment>,
    blocked_by: Vec<i64>,
    blocking: Vec<i64>,
    subissues: Vec<Issue>,
    related: Vec<Issue>,
    milestone: Option<crate::models::Milestone>,
}

/// A node in the tree view.
struct TreeNode {
    issue: Issue,
    labels: Vec<String>,
    depth: usize,
}

/// The Issues tab implementation.
pub struct IssuesTab {
    /// Path to the database file, used to reopen for operations.
    db_path: PathBuf,
    issues: Vec<Issue>,
    /// Labels cached per issue id for display in list view.
    issue_labels: std::collections::HashMap<i64, Vec<String>>,
    selected: usize,
    status_filter: StatusFilter,
    sort_order: SortOrder,
    search_query: String,
    searching: bool,
    view_mode: IssueViewMode,
    /// View mode to return to when leaving detail view.
    prev_view_mode: IssueViewMode,
    detail: Option<IssueDetail>,
    detail_scroll: u16,
    /// Maximum detail scroll offset computed during render.
    detail_max_scroll: Cell<u16>,
    open_count: usize,
    closed_count: usize,
    /// Flattened tree nodes for tree view.
    tree_nodes: Vec<TreeNode>,
    tree_selected: usize,
    /// `TableState` for list view scroll-to-follow (interior mutability for render).
    list_table_state: RefCell<TableState>,
    /// `TableState` for tree view scroll-to-follow.
    tree_table_state: RefCell<TableState>,
}

impl IssuesTab {
    pub fn new(db: &Database, db_path: &std::path::Path) -> anyhow::Result<Self> {
        let mut tab = IssuesTab {
            db_path: db_path.to_path_buf(),
            issues: Vec::new(),
            issue_labels: std::collections::HashMap::new(),
            selected: 0,
            status_filter: StatusFilter::Open,
            sort_order: SortOrder::IdDesc,
            search_query: String::new(),
            searching: false,
            view_mode: IssueViewMode::List,
            prev_view_mode: IssueViewMode::List,
            detail: None,
            detail_scroll: 0,
            detail_max_scroll: Cell::new(0),
            open_count: 0,
            closed_count: 0,
            tree_nodes: Vec::new(),
            tree_selected: 0,
            list_table_state: RefCell::new(TableState::default()),
            tree_table_state: RefCell::new(TableState::default()),
        };
        tab.refresh(db)?;
        Ok(tab)
    }

    /// Open a fresh database connection from the stored path.
    fn open_db(&self) -> anyhow::Result<Database> {
        Database::open(&self.db_path)
    }

    /// Reload issues from the database with current filters applied.
    pub fn refresh(&mut self, db: &Database) -> anyhow::Result<()> {
        // Get counts before filtering
        let all_issues = db.list_issues(Some("all"), None, None)?;
        self.open_count = all_issues
            .iter()
            .filter(|i| i.status == crate::models::IssueStatus::Open)
            .count();
        self.closed_count = all_issues
            .iter()
            .filter(|i| i.status == crate::models::IssueStatus::Closed)
            .count();

        // Fetch with status filter
        let status_arg = match self.status_filter {
            StatusFilter::Open => Some("open"),
            StatusFilter::Closed => Some("closed"),
            StatusFilter::All => Some("all"),
        };
        let mut issues = db.list_issues(status_arg, None, None)?;

        // Cache labels for list display
        self.issue_labels.clear();
        for issue in &issues {
            if let Ok(labels) = db.get_labels(issue.id) {
                if !labels.is_empty() {
                    self.issue_labels.insert(issue.id, labels);
                }
            }
        }

        // Apply search filter
        if !self.search_query.is_empty() {
            let query = self.search_query.to_lowercase();
            issues.retain(|i| {
                i.title.to_lowercase().contains(&query)
                    || i.id.to_string().contains(&query)
                    || self.issue_labels.get(&i.id).is_some_and(|labels| {
                        labels.iter().any(|l| l.to_lowercase().contains(&query))
                    })
            });
        }

        // Apply sort
        match self.sort_order {
            SortOrder::IdDesc => issues.sort_by_key(|b| std::cmp::Reverse(b.id)),
            SortOrder::IdAsc => issues.sort_by_key(|a| a.id),
            SortOrder::Priority => {
                issues.sort_by_key(|a| priority_rank(a.priority));
            }
            SortOrder::Updated => issues.sort_by_key(|b| std::cmp::Reverse(b.updated_at)),
        }

        self.issues = issues;

        // Clamp selection
        if self.issues.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.issues.len() {
            self.selected = self.issues.len() - 1;
        }

        Ok(())
    }

    /// Load detail for the currently selected issue.
    fn load_detail(&mut self, db: &Database) -> anyhow::Result<()> {
        if let Some(issue) = self.issues.get(self.selected) {
            let id = issue.id;
            let detail = IssueDetail {
                issue: issue.clone(),
                labels: db.get_labels(id)?,
                comments: db.get_comments(id)?,
                blocked_by: db.get_blockers(id)?,
                blocking: db.get_blocking(id)?,
                subissues: db.get_subissues(id)?,
                related: db.get_related_issues(id)?,
                milestone: db.get_issue_milestone(id)?,
            };
            self.detail = Some(detail);
            self.detail_scroll = 0;
            self.prev_view_mode = self.view_mode;
            self.view_mode = IssueViewMode::Detail;
        }
        Ok(())
    }

    /// Handle key events in list view mode. Returns true if consumed.
    ///
    /// INTENTIONAL: `let _ =` on refresh/`build_tree` calls throughout this handler --
    /// TUI event handlers cannot propagate errors, so DB failures are silently ignored
    /// and the UI shows stale data until the next successful refresh.
    fn handle_list_key(&mut self, key: KeyEvent, db: Option<&Database>) -> TabAction {
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.search_query.clear();
                    if let Some(db) = db {
                        let _ = self.refresh(db);
                    }
                    return TabAction::Consumed;
                }
                KeyCode::Enter => {
                    self.searching = false;
                    return TabAction::Consumed;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    if let Some(db) = db {
                        let _ = self.refresh(db);
                    }
                    return TabAction::Consumed;
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    if let Some(db) = db {
                        let _ = self.refresh(db);
                    }
                    return TabAction::Consumed;
                }
                _ => return TabAction::Consumed,
            }
        }

        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.issues.is_empty() {
                    self.selected = (self.selected + 1).min(self.issues.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !self.issues.is_empty() {
                    self.selected = self.selected.saturating_sub(1);
                }
                TabAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = 0;
                TabAction::Consumed
            }
            KeyCode::End | KeyCode::Char('G') => {
                if !self.issues.is_empty() {
                    self.selected = self.issues.len() - 1;
                }
                TabAction::Consumed
            }
            KeyCode::Enter => {
                if let Some(db) = db {
                    let _ = self.load_detail(db);
                }
                TabAction::Consumed
            }
            KeyCode::Char('f') => {
                self.status_filter = self.status_filter.next();
                self.selected = 0;
                if let Some(db) = db {
                    let _ = self.refresh(db);
                }
                TabAction::Consumed
            }
            KeyCode::Char('s') => {
                self.sort_order = self.sort_order.next();
                if let Some(db) = db {
                    let _ = self.refresh(db);
                }
                TabAction::Consumed
            }
            KeyCode::Char('/') => {
                self.searching = true;
                TabAction::Consumed
            }
            KeyCode::Char('t') => {
                if let Some(db) = db {
                    let _ = self.build_tree(db);
                    self.view_mode = IssueViewMode::Tree;
                }
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    /// Handle key events in detail view mode.
    fn handle_detail_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.view_mode = self.prev_view_mode;
                self.detail = None;
                TabAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.detail_max_scroll.get();
                self.detail_scroll = self.detail_scroll.saturating_add(1).min(max);
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::PageDown => {
                let max = self.detail_max_scroll.get();
                self.detail_scroll = self.detail_scroll.saturating_add(10).min(max);
                TabAction::Consumed
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
                TabAction::Consumed
            }
            KeyCode::Char('y') => self.copy_detail_to_clipboard(),
            _ => TabAction::NotHandled,
        }
    }

    fn copy_detail_to_clipboard(&self) -> TabAction {
        if let Some(ref d) = self.detail {
            let mut text = format!(
                "#{} — {}\nStatus: {}  Priority: {}  Labels: {}\n",
                d.issue.id,
                d.issue.title,
                d.issue.status,
                d.issue.priority,
                if d.labels.is_empty() {
                    "(none)".to_string()
                } else {
                    d.labels.join(", ")
                }
            );
            if let Some(ref desc) = d.issue.description {
                let _ = write!(text, "\n{desc}\n");
            }
            if !d.comments.is_empty() {
                let _ = write!(text, "\nComments ({}):\n", d.comments.len());
                for c in &d.comments {
                    let kind = if c.kind == "note" {
                        String::new()
                    } else {
                        format!("[{}] ", c.kind)
                    };
                    let _ = write!(
                        text,
                        "  {}{}\n  {}\n\n",
                        kind,
                        c.created_at.format("%Y-%m-%d %H:%M"),
                        c.content
                    );
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

    /// Build flattened tree from issue hierarchy.
    fn build_tree(&mut self, db: &Database) -> anyhow::Result<()> {
        let status_arg = match self.status_filter {
            StatusFilter::Open => Some("open"),
            StatusFilter::Closed => Some("closed"),
            StatusFilter::All => Some("all"),
        };
        let all_issues = db.list_issues(status_arg, None, None)?;
        let top_level: Vec<_> = all_issues
            .into_iter()
            .filter(|i| i.parent_id.is_none())
            .collect();

        self.tree_nodes.clear();
        for issue in top_level {
            self.build_tree_recursive(db, issue, 0)?;
        }
        if self.tree_nodes.is_empty() {
            self.tree_selected = 0;
        } else if self.tree_selected >= self.tree_nodes.len() {
            self.tree_selected = self.tree_nodes.len() - 1;
        }
        Ok(())
    }

    fn build_tree_recursive(
        &mut self,
        db: &Database,
        issue: Issue,
        depth: usize,
    ) -> anyhow::Result<()> {
        // Guard against cycles or extremely deep hierarchies
        const MAX_DEPTH: usize = 32;
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let labels = db.get_labels(issue.id).unwrap_or_default();
        let id = issue.id;
        self.tree_nodes.push(TreeNode {
            issue,
            labels,
            depth,
        });
        let children = db.get_subissues(id)?;
        for child in children {
            // Respect status filter
            let excluded_by_filter = match self.status_filter {
                StatusFilter::All => false,
                StatusFilter::Open => child.status != crate::models::IssueStatus::Open,
                StatusFilter::Closed => child.status != crate::models::IssueStatus::Closed,
            };
            if !excluded_by_filter {
                self.build_tree_recursive(db, child, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Handle key events in tree view mode.
    /// INTENTIONAL: `let _ =` on `build_tree` calls -- TUI event handlers cannot propagate errors.
    fn handle_tree_key(&mut self, key: KeyEvent, db: Option<&Database>) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.view_mode = IssueViewMode::List;
                TabAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.tree_nodes.is_empty() {
                    self.tree_selected = (self.tree_selected + 1).min(self.tree_nodes.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.tree_selected = self.tree_selected.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.tree_selected = 0;
                TabAction::Consumed
            }
            KeyCode::End | KeyCode::Char('G') => {
                if !self.tree_nodes.is_empty() {
                    self.tree_selected = self.tree_nodes.len() - 1;
                }
                TabAction::Consumed
            }
            KeyCode::Enter => {
                // Open detail for the selected tree node
                if let Some(node) = self.tree_nodes.get(self.tree_selected) {
                    if let Some(db) = db {
                        let id = node.issue.id;
                        if let Ok(Some(issue)) = db.get_issue(id) {
                            let detail = IssueDetail {
                                issue,
                                labels: db.get_labels(id).unwrap_or_default(),
                                comments: db.get_comments(id).unwrap_or_default(),
                                blocked_by: db.get_blockers(id).unwrap_or_default(),
                                blocking: db.get_blocking(id).unwrap_or_default(),
                                subissues: db.get_subissues(id).unwrap_or_default(),
                                related: db.get_related_issues(id).unwrap_or_default(),
                                milestone: db.get_issue_milestone(id).ok().flatten(),
                            };
                            self.detail = Some(detail);
                            self.detail_scroll = 0;
                            self.prev_view_mode = self.view_mode;
                            self.view_mode = IssueViewMode::Detail;
                        }
                    }
                }
                TabAction::Consumed
            }
            KeyCode::Char('f') => {
                self.status_filter = self.status_filter.next();
                self.tree_selected = 0;
                if let Some(db) = db {
                    let _ = self.build_tree(db);
                }
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    fn render_tree(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(0),    // Tree
                Constraint::Length(1), // Context keys
            ])
            .split(area);

        // Header
        let header = Line::from(vec![
            Span::styled(" Issue Tree", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("    Filter: [{}]", self.status_filter.label())),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        if self.tree_nodes.is_empty() {
            let p = Paragraph::new("No issues found.")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::TOP));
            frame.render_widget(p, chunks[1]);
        } else {
            let rows: Vec<Row> = self
                .tree_nodes
                .iter()
                .map(|node| {
                    let indent = "  ".repeat(node.depth);
                    let connector = if node.depth > 0 {
                        "\u{251c}\u{2500} "
                    } else {
                        ""
                    };

                    let status_marker = if node.issue.status == crate::models::IssueStatus::Closed {
                        Span::styled("\u{2713} ", Style::default().fg(Color::DarkGray))
                    } else {
                        Span::styled("\u{25cf} ", priority_color(node.issue.priority))
                    };

                    let id_str = format_issue_id(node.issue.id);
                    let labels_str = if node.labels.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", node.labels.join(", "))
                    };

                    let title_style = if node.issue.status == crate::models::IssueStatus::Closed {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                    };

                    Row::new(vec![ratatui::text::Text::from(Line::from(vec![
                        Span::raw(format!("{indent}{connector}")),
                        status_marker,
                        Span::styled(format!("{id_str} "), Style::default().fg(Color::DarkGray)),
                        Span::styled(node.issue.title.clone(), title_style),
                        Span::styled(labels_str, Style::default().fg(Color::Magenta)),
                    ]))])
                })
                .collect();

            let table = Table::new(rows, [Constraint::Min(0)])
                .block(Block::default().borders(Borders::TOP))
                .row_highlight_style(Style::default().bg(HIGHLIGHT_BG));

            let mut state = self.tree_table_state.borrow_mut();
            state.select(Some(self.tree_selected));
            frame.render_stateful_widget(table, chunks[1], &mut state);
        }

        // Context keys
        let keys = Line::from(vec![
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(":Back  "),
            Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::Cyan)),
            Span::raw(":Navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(":Details  "),
            Span::styled("f", Style::default().fg(Color::Cyan)),
            Span::raw(":Filter  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(":Refresh"),
        ]);
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn render_list(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header with counts and filters
                Constraint::Min(0),    // Table
                Constraint::Length(1), // Context keys
            ])
            .split(area);

        // Header
        let search_display = if self.searching {
            format!("  Search: {}_", self.search_query)
        } else if !self.search_query.is_empty() {
            format!("  Search: {}", self.search_query)
        } else {
            String::new()
        };

        let header = Line::from(vec![
            Span::styled(
                format!(
                    " Issues ({} open, {} closed)",
                    self.open_count, self.closed_count
                ),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "    Filter: [{}]  Sort: [{}]",
                self.status_filter.label(),
                self.sort_order.label()
            )),
            Span::styled(search_display, Style::default().fg(Color::Cyan)),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        // Issue table
        if self.issues.is_empty() {
            let empty_msg = if self.search_query.is_empty() {
                "No issues found."
            } else {
                "No issues match the search."
            };
            let p = Paragraph::new(empty_msg)
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::TOP));
            frame.render_widget(p, chunks[1]);
        } else {
            let header_row = Row::new(vec![
                "ID", "Priority", "Status", "Labels", "Title", "Updated",
            ])
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

            let rows: Vec<Row> = self
                .issues
                .iter()
                .map(|issue| {
                    let priority_style = match issue.priority.as_str() {
                        "critical" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        "high" => Style::default().fg(Color::Red),
                        "medium" => Style::default().fg(Color::Cyan),
                        "low" => Style::default().fg(Color::Green),
                        _ => Style::default(),
                    };

                    let status_style = if issue.status == crate::models::IssueStatus::Closed {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::Green)
                    };

                    let labels = self
                        .issue_labels
                        .get(&issue.id)
                        .map(|l| l.join(", "))
                        .unwrap_or_default();

                    let updated = super::format_relative_time(&issue.updated_at);

                    let id_str = format_issue_id(issue.id);

                    Row::new(vec![
                        ratatui::text::Text::styled(id_str, Style::default()),
                        ratatui::text::Text::styled(issue.priority.to_string(), priority_style),
                        ratatui::text::Text::styled(issue.status.to_string(), status_style),
                        ratatui::text::Text::styled(labels, Style::default().fg(Color::Magenta)),
                        ratatui::text::Text::raw(&issue.title),
                        ratatui::text::Text::styled(updated, Style::default().fg(Color::DarkGray)),
                    ])
                })
                .collect();

            let widths = [
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Length(16),
                Constraint::Min(20),
                Constraint::Length(10),
            ];

            let table = Table::new(rows, widths)
                .header(header_row)
                .block(Block::default().borders(Borders::TOP))
                .row_highlight_style(Style::default().bg(HIGHLIGHT_BG));

            let mut state = self.list_table_state.borrow_mut();
            state.select(Some(self.selected));
            frame.render_stateful_widget(table, chunks[1], &mut state);
        }

        // Context keys
        let keys = if self.searching {
            Line::from(vec![
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(":Cancel  "),
                Span::styled("Enter", Style::default().fg(Color::Cyan)),
                Span::raw(":Accept  "),
                Span::raw("Type to search..."),
            ])
        } else {
            Line::from(vec![
                Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::Cyan)),
                Span::raw(":Navigate  "),
                Span::styled("Enter", Style::default().fg(Color::Cyan)),
                Span::raw(":Details  "),
                Span::styled("f", Style::default().fg(Color::Cyan)),
                Span::raw(":Filter  "),
                Span::styled("s", Style::default().fg(Color::Cyan)),
                Span::raw(":Sort  "),
                Span::styled("/", Style::default().fg(Color::Cyan)),
                Span::raw(":Search  "),
                Span::styled("r", Style::default().fg(Color::Cyan)),
                Span::raw(":Refresh  "),
                Span::styled("t", Style::default().fg(Color::Cyan)),
                Span::raw(":Tree"),
            ])
        };
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let Some(detail) = &self.detail else {
            return;
        };

        let issue = &detail.issue;
        let mut lines: Vec<Line> = Vec::new();

        // Title
        lines.push(Line::from(Span::styled(
            format!(" {} \u{2014} {}", format_issue_id(issue.id), issue.title),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(
            "\u{2500}".repeat(area.width.saturating_sub(2) as usize),
        ));

        // Metadata
        let labels_str = if detail.labels.is_empty() {
            "(none)".to_string()
        } else {
            detail.labels.join(", ")
        };

        lines.push(Line::from(vec![
            Span::styled(" Status: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(issue.status.as_str(), status_color(issue.status)),
            Span::raw("       "),
            Span::styled("Priority: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(issue.priority.as_str(), priority_color(issue.priority)),
            Span::raw("       "),
            Span::styled("Labels: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(labels_str, Style::default().fg(Color::Magenta)),
        ]));

        let milestone_str = detail
            .milestone
            .as_ref()
            .map_or_else(|| "(none)".to_string(), |m| format!("#{} {}", m.id, m.name));

        lines.push(Line::from(vec![
            Span::styled(" Parent: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(
                issue
                    .parent_id
                    .map_or_else(|| "(none)".to_string(), format_issue_id),
            ),
            Span::raw("     "),
            Span::styled("Milestone: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(milestone_str),
        ]));
        // Show timestamps with local time when terminal is wide enough (#402)
        let created_utc = issue.created_at.format("%Y-%m-%d %H:%M UTC").to_string();
        let updated_utc = issue.updated_at.format("%Y-%m-%d %H:%M UTC").to_string();
        let wide_enough = area.width >= 90;

        let mut ts_spans = vec![
            Span::styled(" Created: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(created_utc),
        ];
        if wide_enough {
            let local_created = issue
                .created_at
                .with_timezone(&chrono::Local)
                .format("(%I:%M %p %Z)")
                .to_string();
            ts_spans.push(Span::styled(
                format!("  {local_created}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        ts_spans.push(Span::raw("  "));
        ts_spans.push(Span::styled(
            "Updated: ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        ts_spans.push(Span::raw(updated_utc));
        if wide_enough {
            let local_updated = issue
                .updated_at
                .with_timezone(&chrono::Local)
                .format("(%I:%M %p %Z)")
                .to_string();
            ts_spans.push(Span::styled(
                format!("  {local_updated}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(ts_spans));

        // Dependencies
        let blocked_by_str = if detail.blocked_by.is_empty() {
            "(none)".to_string()
        } else {
            detail
                .blocked_by
                .iter()
                .map(|id| format_issue_id(*id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let blocking_str = if detail.blocking.is_empty() {
            "(none)".to_string()
        } else {
            detail
                .blocking
                .iter()
                .map(|id| format_issue_id(*id))
                .collect::<Vec<_>>()
                .join(", ")
        };

        lines.push(Line::from(vec![
            Span::styled(
                " Blocked by: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(&blocked_by_str),
            Span::raw("   "),
            Span::styled("Blocks: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&blocking_str),
        ]));

        // Description
        lines.push(Line::from(""));
        if let Some(desc) = &issue.description {
            if !desc.is_empty() {
                lines.push(Line::from(Span::styled(
                    " Description:",
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for line in desc.lines() {
                    lines.push(Line::from(format!("   {line}")));
                }
            }
        }

        // Subissues
        if !detail.subissues.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" Subissues ({}):", detail.subissues.len()),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for sub in &detail.subissues {
                let status_marker = if sub.status == crate::models::IssueStatus::Closed {
                    Span::styled("  \u{2713} ", Style::default().fg(Color::DarkGray))
                } else {
                    Span::styled("  \u{25cf} ", priority_color(sub.priority))
                };
                let title_style = if sub.status == crate::models::IssueStatus::Closed {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    status_marker,
                    Span::styled(
                        format!("{} ", format_issue_id(sub.id)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(&sub.title, title_style),
                    Span::styled(format!("  {}", sub.priority), priority_color(sub.priority)),
                ]));
            }
        }

        // Related issues
        if !detail.related.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" Related ({}):", detail.related.len()),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for rel in &detail.related {
                let status_marker = if rel.status == crate::models::IssueStatus::Closed {
                    Span::styled("  \u{2713} ", Style::default().fg(Color::DarkGray))
                } else {
                    Span::styled("  \u{25cb} ", Style::default().fg(Color::Cyan))
                };
                let title_style = if rel.status == crate::models::IssueStatus::Closed {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    status_marker,
                    Span::styled(
                        format!("{} ", format_issue_id(rel.id)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(&rel.title, title_style),
                ]));
            }
        }

        // Comments
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(" Comments ({}):", detail.comments.len()),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(
            " \u{2500}".to_string() + &"\u{2500}".repeat(area.width.saturating_sub(4) as usize),
        ));

        if detail.comments.is_empty() {
            lines.push(Line::from(Span::styled(
                "   No comments.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for comment in &detail.comments {
                let kind_badge = if comment.kind == "note" {
                    String::new()
                } else {
                    format!("[{}] ", comment.kind)
                };

                let time = comment.created_at.format("%Y-%m-%d %H:%M");

                lines.push(Line::from(vec![
                    Span::styled(format!(" {kind_badge}"), Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{time}"), Style::default().fg(Color::DarkGray)),
                ]));

                for line in comment.content.lines() {
                    lines.push(Line::from(format!("   {line}")));
                }
                lines.push(Line::from(""));
            }
        }

        // Footer with context keys
        lines.push(Line::from(
            " \u{2500}".to_string() + &"\u{2500}".repeat(area.width.saturating_sub(4) as usize),
        ));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        // Clamp scroll so the user can't scroll past content.
        let content_height = lines.len() as u16;
        let viewport_height = chunks[0].height;
        let max_scroll = content_height.saturating_sub(viewport_height);
        self.detail_max_scroll.set(max_scroll);
        let clamped_scroll = self.detail_scroll.min(max_scroll);

        let detail_widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false })
            .scroll((clamped_scroll, 0));

        frame.render_widget(detail_widget, chunks[0]);

        let keys = Line::from(vec![
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(":Back  "),
            Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::Cyan)),
            Span::raw(":Scroll  "),
            Span::styled("y", Style::default().fg(Color::Cyan)),
            Span::raw(":Copy"),
        ]);
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    }
}

impl super::Tab for IssuesTab {
    fn title(&self) -> &'static str {
        "Issues"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self.view_mode {
            IssueViewMode::List => self.render_list(frame, area),
            IssueViewMode::Detail => self.render_detail(frame, area),
            IssueViewMode::Tree => self.render_tree(frame, area),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabAction {
        match self.view_mode {
            IssueViewMode::List => {
                let db = self.open_db().ok();
                self.handle_list_key(key, db.as_ref())
            }
            IssueViewMode::Detail => self.handle_detail_key(key),
            IssueViewMode::Tree => {
                let db = self.open_db().ok();
                self.handle_tree_key(key, db.as_ref())
            }
        }
    }

    // IssuesTab loads data eagerly in new() and on refresh, so no work needed on focus change.
    fn on_enter(&mut self) {}
    fn on_leave(&mut self) {}

    /// INTENTIONAL: `let _ =` on `refresh`/`build_tree` -- `force_refresh` is best-effort, TUI shows stale data on failure.
    fn force_refresh(&mut self) {
        if let Ok(db) = self.open_db() {
            match self.view_mode {
                IssueViewMode::Tree => {
                    let _ = self.build_tree(&db);
                }
                _ => {
                    let _ = self.refresh(&db);
                }
            }
        }
    }
}

// === Helper functions ===

fn format_issue_id(id: i64) -> String {
    if id < 0 {
        format!("L{}", id.unsigned_abs())
    } else {
        format!("#{id}")
    }
}

const fn priority_rank(priority: crate::models::Priority) -> u8 {
    use crate::models::Priority;
    match priority {
        Priority::Critical => 0,
        Priority::High => 1,
        Priority::Medium => 2,
        Priority::Low => 3,
    }
}

fn priority_color(priority: crate::models::Priority) -> Style {
    use crate::models::Priority;
    match priority {
        Priority::Critical => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Priority::High => Style::default().fg(Color::Red),
        Priority::Medium => Style::default().fg(Color::Cyan),
        Priority::Low => Style::default().fg(Color::Green),
    }
}

fn status_color(status: crate::models::IssueStatus) -> Style {
    use crate::models::IssueStatus;
    match status {
        IssueStatus::Open => Style::default().fg(Color::Green),
        IssueStatus::Closed => Style::default().fg(Color::DarkGray),
        IssueStatus::Archived => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::tempdir;

    fn setup_test_db() -> (Database, PathBuf, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, db_path, dir)
    }

    fn setup_populated_db() -> (Database, PathBuf, tempfile::TempDir) {
        let (db, db_path, dir) = setup_test_db();
        let id1 = db
            .create_issue("High priority bug", Some("Fix ASAP"), "high")
            .unwrap();
        let id2 = db
            .create_issue("Medium feature", Some("Add thing"), "medium")
            .unwrap();
        let id3 = db.create_issue("Low docs fix", None, "low").unwrap();
        db.add_label(id1, "bug").unwrap();
        db.add_label(id2, "feature").unwrap();
        db.add_label(id3, "docs").unwrap();
        db.add_comment(id1, "Plan: fix the bug", "plan").unwrap();
        db.add_comment(id1, "Found the root cause", "observation")
            .unwrap();
        (db, db_path, dir)
    }

    #[test]
    fn test_issues_tab_new_empty() {
        let (db, db_path, _dir) = setup_test_db();
        let tab = IssuesTab::new(&db, &db_path).unwrap();
        assert!(tab.issues.is_empty());
        assert_eq!(tab.selected, 0);
        assert_eq!(tab.open_count, 0);
        assert_eq!(tab.closed_count, 0);
    }

    #[test]
    fn test_issues_tab_new_with_issues() {
        let (db, db_path, _dir) = setup_populated_db();
        let tab = IssuesTab::new(&db, &db_path).unwrap();
        assert_eq!(tab.issues.len(), 3);
        assert_eq!(tab.open_count, 3);
        assert_eq!(tab.closed_count, 0);
    }

    #[test]
    fn test_status_filter_cycle() {
        assert_eq!(StatusFilter::Open.next(), StatusFilter::Closed);
        assert_eq!(StatusFilter::Closed.next(), StatusFilter::All);
        assert_eq!(StatusFilter::All.next(), StatusFilter::Open);
    }

    #[test]
    fn test_sort_order_cycle() {
        assert_eq!(SortOrder::IdDesc.next(), SortOrder::IdAsc);
        assert_eq!(SortOrder::IdAsc.next(), SortOrder::Priority);
        assert_eq!(SortOrder::Priority.next(), SortOrder::Updated);
        assert_eq!(SortOrder::Updated.next(), SortOrder::IdDesc);
    }

    #[test]
    fn test_status_filter_labels() {
        assert_eq!(StatusFilter::Open.label(), "Open");
        assert_eq!(StatusFilter::Closed.label(), "Closed");
        assert_eq!(StatusFilter::All.label(), "All");
    }

    #[test]
    fn test_priority_rank_ordering() {
        assert!(
            priority_rank(crate::models::Priority::Critical)
                < priority_rank(crate::models::Priority::High)
        );
        assert!(
            priority_rank(crate::models::Priority::High)
                < priority_rank(crate::models::Priority::Medium)
        );
        assert!(
            priority_rank(crate::models::Priority::Medium)
                < priority_rank(crate::models::Priority::Low)
        );
        assert!(priority_rank(crate::models::Priority::Low) < 4);
    }

    #[test]
    fn test_format_issue_id_positive() {
        assert_eq!(format_issue_id(42), "#42");
        assert_eq!(format_issue_id(1), "#1");
    }

    #[test]
    fn test_format_issue_id_negative() {
        assert_eq!(format_issue_id(-1), "L1");
        assert_eq!(format_issue_id(-42), "L42");
    }

    #[test]
    fn test_format_relative_time() {
        let now = Utc::now();
        assert_eq!(super::super::format_relative_time(&now), "0s ago");

        let five_min_ago = now - chrono::Duration::minutes(5);
        assert_eq!(super::super::format_relative_time(&five_min_ago), "5m ago");

        let two_hours_ago = now - chrono::Duration::hours(2);
        assert_eq!(super::super::format_relative_time(&two_hours_ago), "2h ago");

        let three_days_ago = now - chrono::Duration::days(3);
        assert_eq!(
            super::super::format_relative_time(&three_days_ago),
            "3d ago"
        );
    }

    #[test]
    fn test_refresh_with_status_filter() {
        let (db, db_path, _dir) = setup_populated_db();
        let id = db.create_issue("Closed one", None, "medium").unwrap();
        db.close_issue(id).unwrap();

        let mut tab = IssuesTab::new(&db, &db_path).unwrap();
        assert_eq!(tab.issues.len(), 3); // Only open

        tab.status_filter = StatusFilter::All;
        tab.refresh(&db).unwrap();
        assert_eq!(tab.issues.len(), 4);

        tab.status_filter = StatusFilter::Closed;
        tab.refresh(&db).unwrap();
        assert_eq!(tab.issues.len(), 1);
    }

    #[test]
    fn test_refresh_with_sort() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();

        tab.sort_order = SortOrder::Priority;
        tab.refresh(&db).unwrap();
        assert_eq!(tab.issues[0].priority, "high");
        assert_eq!(tab.issues[2].priority, "low");

        tab.sort_order = SortOrder::IdAsc;
        tab.refresh(&db).unwrap();
        assert!(tab.issues[0].id < tab.issues[1].id);
    }

    #[test]
    fn test_refresh_with_search() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();

        tab.search_query = "bug".to_string();
        tab.refresh(&db).unwrap();
        // Should match "High priority bug" (title) and any issue with "bug" label
        assert!(tab.issues.iter().any(|i| i.title.contains("bug")));
    }

    #[test]
    fn test_selection_navigation() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();
        assert_eq!(tab.selected, 0);

        // Down
        tab.handle_list_key(make_key(KeyCode::Down), None);
        assert_eq!(tab.selected, 1);

        tab.handle_list_key(make_key(KeyCode::Down), None);
        assert_eq!(tab.selected, 2);

        // Should not go past end
        tab.handle_list_key(make_key(KeyCode::Down), None);
        assert_eq!(tab.selected, 2);

        // Up
        tab.handle_list_key(make_key(KeyCode::Up), None);
        assert_eq!(tab.selected, 1);

        // Home
        tab.handle_list_key(make_key(KeyCode::Home), None);
        assert_eq!(tab.selected, 0);

        // End
        tab.handle_list_key(make_key(KeyCode::End), None);
        assert_eq!(tab.selected, 2);
    }

    #[test]
    fn test_vim_navigation() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();
        assert_eq!(tab.selected, 0);

        tab.handle_list_key(make_key(KeyCode::Char('j')), None);
        assert_eq!(tab.selected, 1);

        tab.handle_list_key(make_key(KeyCode::Char('k')), None);
        assert_eq!(tab.selected, 0);
    }

    #[test]
    fn test_detail_view_and_back() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();

        // Enter detail
        tab.handle_list_key(make_key(KeyCode::Enter), Some(&db));
        assert!(matches!(tab.view_mode, IssueViewMode::Detail));
        assert!(tab.detail.is_some());

        // Simulate a render having computed max scroll.
        tab.detail_max_scroll.set(100);

        // Scroll
        tab.handle_detail_key(make_key(KeyCode::Down));
        assert_eq!(tab.detail_scroll, 1);

        // Back
        tab.handle_detail_key(make_key(KeyCode::Esc));
        assert!(matches!(tab.view_mode, IssueViewMode::List));
        assert!(tab.detail.is_none());
    }

    #[test]
    fn test_detail_has_comments() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();

        // Select the first issue (highest ID, which has comments)
        // Issues are sorted IdDesc by default, so first issue has id=3 (Low docs fix).
        // We need to find the one with comments (id=1, "High priority bug")
        let idx = tab
            .issues
            .iter()
            .position(|i| i.title == "High priority bug")
            .unwrap();
        tab.selected = idx;

        tab.handle_list_key(make_key(KeyCode::Enter), Some(&db));
        let detail = tab.detail.as_ref().unwrap();
        assert_eq!(detail.comments.len(), 2);
        assert_eq!(detail.labels, vec!["bug"]);
    }

    #[test]
    fn test_search_mode() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();

        // Enter search
        tab.handle_list_key(make_key(KeyCode::Char('/')), None);
        assert!(tab.searching);

        // Type query
        tab.handle_list_key(make_key(KeyCode::Char('b')), Some(&db));
        tab.handle_list_key(make_key(KeyCode::Char('u')), Some(&db));
        tab.handle_list_key(make_key(KeyCode::Char('g')), Some(&db));
        assert_eq!(tab.search_query, "bug");

        // Accept search
        tab.handle_list_key(make_key(KeyCode::Enter), None);
        assert!(!tab.searching);
        assert_eq!(tab.search_query, "bug");

        // Cancel clears search
        tab.handle_list_key(make_key(KeyCode::Char('/')), None);
        tab.handle_list_key(make_key(KeyCode::Esc), Some(&db));
        assert!(tab.search_query.is_empty());
    }

    #[test]
    fn test_render_list_no_panic() {
        let (db, db_path, _dir) = setup_populated_db();
        let tab = IssuesTab::new(&db, &db_path).unwrap();
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render_list(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_detail_no_panic() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();
        tab.load_detail(&db).unwrap();
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render_detail(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_empty_list_no_panic() {
        let (db, db_path, _dir) = setup_test_db();
        let tab = IssuesTab::new(&db, &db_path).unwrap();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render_list(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_selection_clamp_on_filter_change() {
        let (db, db_path, _dir) = setup_populated_db();
        let mut tab = IssuesTab::new(&db, &db_path).unwrap();
        tab.selected = 2; // Last issue

        // Close all issues, then filter to closed
        for issue in &tab.issues {
            db.close_issue(issue.id).unwrap();
        }

        // Filter to open (empty)
        tab.status_filter = StatusFilter::Open;
        tab.refresh(&db).unwrap();
        assert_eq!(tab.selected, 0); // Clamped
    }

    fn make_key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        super::super::make_test_key(code)
    }
}
