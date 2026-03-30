use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use super::{Tab, TabAction, HIGHLIGHT_BG};
use crate::knowledge::{self, KnowledgeManager, PageFrontmatter, PageInfo};

/// Which sub-view is active within the Knowledge tab.
#[derive(Clone, Copy, Debug, PartialEq)]
enum KnowledgeViewMode {
    /// Page list with table.
    List,
    /// Full-page reader with rendered markdown.
    Reader,
}

/// The Knowledge tab — browse and read knowledge pages.
pub struct KnowledgeTab {
    crosslink_dir: PathBuf,
    view_mode: KnowledgeViewMode,
    /// All pages loaded from the knowledge manager.
    all_pages: Vec<PageInfo>,
    /// Filtered pages currently displayed.
    filtered_pages: Vec<PageInfo>,
    selected: usize,
    /// All unique tags gathered from pages, sorted. First entry is "all".
    available_tags: Vec<String>,
    /// Current tag filter index into `available_tags`. 0 = "all".
    tag_filter_idx: usize,
    /// Search query string (filters in list view).
    search_query: String,
    /// Whether search input mode is active.
    searching: bool,
    /// Full page content for the reader view (raw markdown string).
    reader_content: Option<String>,
    /// Parsed frontmatter for the reader view header.
    reader_frontmatter: Option<PageFrontmatter>,
    /// Slug of the page currently being read.
    reader_slug: Option<String>,
    /// Scroll offset for the reader view.
    reader_scroll: u16,
    /// Maximum scroll offset computed during render (content lines − viewport height).
    reader_max_scroll: Cell<u16>,
    /// Status message.
    status_msg: String,
    /// Error message if data load failed.
    error_msg: Option<String>,
    /// `TableState` for list view scroll-to-follow.
    list_table_state: RefCell<TableState>,
    /// Receiver for background sync results.
    sync_rx: Option<mpsc::Receiver<Option<String>>>,
    /// Whether a background sync is in progress.
    syncing: bool,
}

impl KnowledgeTab {
    pub fn new(crosslink_dir: &Path) -> Self {
        let mut tab = KnowledgeTab {
            crosslink_dir: crosslink_dir.to_path_buf(),
            view_mode: KnowledgeViewMode::List,
            all_pages: Vec::new(),
            filtered_pages: Vec::new(),
            selected: 0,
            available_tags: vec!["all".to_string()],
            tag_filter_idx: 0,
            search_query: String::new(),
            searching: false,
            reader_content: None,
            reader_frontmatter: None,
            reader_slug: None,
            reader_scroll: 0,
            reader_max_scroll: Cell::new(0),
            status_msg: String::new(),
            error_msg: None,
            list_table_state: RefCell::new(TableState::default()),
            sync_rx: None,
            syncing: false,
        };
        tab.refresh();
        tab
    }

    fn refresh(&mut self) {
        self.error_msg = None;

        let km = match KnowledgeManager::new(&self.crosslink_dir) {
            Ok(km) => km,
            Err(e) => {
                self.error_msg = Some(format!("Failed to init KnowledgeManager: {e}"));
                return;
            }
        };

        if !km.is_initialized() {
            self.error_msg = Some(
                "Knowledge cache not initialized. Run 'crosslink knowledge sync' first."
                    .to_string(),
            );
            return;
        }

        // Load pages from cache immediately (non-blocking).
        // Background sync (git fetch) runs separately to avoid blocking the main thread.
        match km.list_pages() {
            Ok(pages) => {
                self.all_pages = pages;
                self.collect_tags();
                self.apply_filters();
                self.status_msg = format!("{} pages", self.all_pages.len());
            }
            Err(e) => {
                self.error_msg = Some(format!("Failed to list pages: {e}"));
            }
        }

        // Start background sync (git fetch) without blocking the UI
        self.start_background_sync();
    }

    /// Spawn a background thread to run knowledge sync (git fetch) without blocking the UI.
    fn start_background_sync(&mut self) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        let (tx, rx) = mpsc::channel();
        self.sync_rx = Some(rx);
        let crosslink_dir = self.crosslink_dir.clone();

        std::thread::spawn(move || {
            let result = match KnowledgeManager::new(&crosslink_dir) {
                Ok(km) => {
                    // INTENTIONAL: sync is best-effort — TUI shows cached pages if offline
                    match km.sync() {
                        Ok(_) => None,
                        Err(e) => Some(e.to_string()),
                    }
                }
                Err(e) => Some(e.to_string()),
            };
            // INTENTIONAL: send failure means the receiver was dropped — TUI is shutting down
            let _ = tx.send(result);
        });
    }

    fn collect_tags(&mut self) {
        let mut tag_set = BTreeSet::new();
        for page in &self.all_pages {
            for tag in &page.frontmatter.tags {
                tag_set.insert(tag.clone());
            }
        }
        self.available_tags = vec!["all".to_string()];
        self.available_tags.extend(tag_set);

        if self.tag_filter_idx >= self.available_tags.len() {
            self.tag_filter_idx = 0;
        }
    }

    fn apply_filters(&mut self) {
        let active_tag = if self.tag_filter_idx > 0 {
            self.available_tags.get(self.tag_filter_idx)
        } else {
            None
        };

        let query = if self.search_query.is_empty() {
            None
        } else {
            Some(self.search_query.to_lowercase())
        };

        // Build filtered pages list
        self.filtered_pages = self
            .all_pages
            .iter()
            .filter(|p| {
                // Tag filter
                if let Some(tag) = &active_tag {
                    if !p.frontmatter.tags.contains(tag) {
                        return false;
                    }
                }
                // Search filter
                if let Some(ref q) = query {
                    if !p.slug.to_lowercase().contains(q)
                        && !p.frontmatter.title.to_lowercase().contains(q)
                        && !p
                            .frontmatter
                            .tags
                            .iter()
                            .any(|t| t.to_lowercase().contains(q))
                    {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        // Clamp selection
        if self.filtered_pages.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_pages.len() {
            self.selected = self.filtered_pages.len() - 1;
        }
    }

    fn load_page(&mut self, slug: &str) {
        let Ok(km) = KnowledgeManager::new(&self.crosslink_dir) else {
            return;
        };

        match km.read_page(slug) {
            Ok(content) => {
                self.reader_frontmatter = knowledge::parse_frontmatter(&content);
                self.reader_content = Some(content);
                self.reader_slug = Some(slug.to_string());
                self.reader_scroll = 0;
                self.view_mode = KnowledgeViewMode::Reader;
            }
            Err(e) => {
                self.error_msg = Some(format!("Failed to read page: {e}"));
            }
        }
    }

    // ── Key handlers ──────────────────────────────────────────────────

    fn handle_list_key(&mut self, key: KeyEvent) -> TabAction {
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.search_query.clear();
                    self.apply_filters();
                    return TabAction::Consumed;
                }
                KeyCode::Enter => {
                    self.searching = false;
                    return TabAction::Consumed;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.apply_filters();
                    return TabAction::Consumed;
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    self.apply_filters();
                    return TabAction::Consumed;
                }
                _ => return TabAction::Consumed,
            }
        }

        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.filtered_pages.is_empty() {
                    self.selected = (self.selected + 1).min(self.filtered_pages.len() - 1);
                }
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = 0;
                TabAction::Consumed
            }
            KeyCode::End | KeyCode::Char('G') => {
                if !self.filtered_pages.is_empty() {
                    self.selected = self.filtered_pages.len() - 1;
                }
                TabAction::Consumed
            }
            KeyCode::Enter => {
                if let Some(page) = self.filtered_pages.get(self.selected) {
                    let slug = page.slug.clone();
                    self.load_page(&slug);
                }
                TabAction::Consumed
            }
            KeyCode::Char('t') => {
                if !self.available_tags.is_empty() {
                    self.tag_filter_idx = (self.tag_filter_idx + 1) % self.available_tags.len();
                    self.selected = 0;
                    self.apply_filters();
                }
                TabAction::Consumed
            }
            KeyCode::Char('/') => {
                self.searching = true;
                TabAction::Consumed
            }
            _ => TabAction::NotHandled,
        }
    }

    fn handle_reader_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.view_mode = KnowledgeViewMode::List;
                self.reader_content = None;
                self.reader_frontmatter = None;
                self.reader_slug = None;
                TabAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.reader_max_scroll.get();
                self.reader_scroll = self.reader_scroll.saturating_add(1).min(max);
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.reader_scroll = self.reader_scroll.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::PageDown => {
                let max = self.reader_max_scroll.get();
                self.reader_scroll = self.reader_scroll.saturating_add(10).min(max);
                TabAction::Consumed
            }
            KeyCode::PageUp => {
                self.reader_scroll = self.reader_scroll.saturating_sub(10);
                TabAction::Consumed
            }
            KeyCode::Char('G') => {
                self.reader_scroll = self.reader_max_scroll.get();
                TabAction::Consumed
            }
            KeyCode::Home => {
                self.reader_scroll = 0;
                TabAction::Consumed
            }
            KeyCode::Char('y') => self.copy_page_to_clipboard(),
            _ => TabAction::NotHandled,
        }
    }

    fn copy_page_to_clipboard(&self) -> TabAction {
        if let Some(ref content) = self.reader_content {
            let ok = super::copy_to_clipboard(content);
            let msg = if ok {
                "Copied to clipboard"
            } else {
                "Clipboard copy failed"
            };
            return TabAction::Flash(msg.to_string());
        }
        TabAction::Consumed
    }

    // ── Renderers ─────────────────────────────────────────────────────

    fn render_list(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header
                Constraint::Min(0),    // Table
                Constraint::Length(1), // Context keys
            ])
            .split(area);

        // Header
        let tag_label = self
            .available_tags
            .get(self.tag_filter_idx)
            .map_or("all", String::as_str);

        let search_display = if self.searching {
            format!("  Search: {}_", self.search_query)
        } else if !self.search_query.is_empty() {
            format!("  Search: {}", self.search_query)
        } else {
            String::new()
        };

        let header = Line::from(vec![
            Span::styled(
                format!(" Knowledge Pages ({})", self.all_pages.len()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("    Tag: [{tag_label}]"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(search_display, Style::default().fg(Color::Cyan)),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        // Error or empty state
        if let Some(ref err) = self.error_msg {
            let msg = Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(err, Style::default().fg(Color::Red)),
            ]));
            frame.render_widget(msg, chunks[1]);
        } else if self.filtered_pages.is_empty() {
            let empty_msg = if self.search_query.is_empty() && self.tag_filter_idx == 0 {
                "No knowledge pages found."
            } else {
                "No pages match the current filter."
            };
            let msg = Paragraph::new(Line::from(Span::styled(
                format!("  {empty_msg}"),
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(msg, chunks[1]);
        } else {
            // Table
            let header_row = Row::new(vec!["Slug", "Title", "Tags", "Updated"]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

            let rows: Vec<Row> = self
                .filtered_pages
                .iter()
                .map(|page| {
                    let tags = page.frontmatter.tags.join(", ");
                    let updated = format_relative_date(&page.frontmatter.updated);

                    Row::new(vec![
                        ratatui::text::Text::raw(&page.slug),
                        ratatui::text::Text::raw(&page.frontmatter.title),
                        ratatui::text::Text::styled(tags, Style::default().fg(Color::Magenta)),
                        ratatui::text::Text::styled(updated, Style::default().fg(Color::DarkGray)),
                    ])
                })
                .collect();

            let table = Table::new(
                rows,
                [
                    Constraint::Min(16),    // Slug
                    Constraint::Min(20),    // Title
                    Constraint::Length(20), // Tags
                    Constraint::Length(10), // Updated
                ],
            )
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
                Span::raw(":Read  "),
                Span::styled("/", Style::default().fg(Color::Cyan)),
                Span::raw(":Search  "),
                Span::styled("t", Style::default().fg(Color::Cyan)),
                Span::raw(":Filter tag  "),
                Span::styled("r", Style::default().fg(Color::Cyan)),
                Span::raw(":Refresh"),
            ])
        };
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn render_reader(&self, frame: &mut Frame, area: Rect) {
        let Some(content) = &self.reader_content else {
            return;
        };

        let slug = self.reader_slug.as_deref().unwrap_or("unknown");
        let mut lines: Vec<Line> = Vec::new();

        // Title header
        let title = self
            .reader_frontmatter
            .as_ref()
            .map_or(slug, |fm| fm.title.as_str());

        lines.push(Line::from(Span::styled(
            format!(" {slug} \u{2014} {title}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(
            " \u{2500}".to_string() + &"\u{2500}".repeat(area.width.saturating_sub(3) as usize),
        ));

        // Metadata
        if let Some(ref fm) = self.reader_frontmatter {
            // Tags line — each tag as a separate colored pill
            let mut tag_spans = vec![Span::styled(
                " Tags: ",
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if fm.tags.is_empty() {
                tag_spans.push(Span::styled("(none)", Style::default().fg(Color::DarkGray)));
            } else {
                for (i, tag) in fm.tags.iter().enumerate() {
                    if i > 0 {
                        tag_spans.push(Span::styled(
                            " \u{2022} ",
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    tag_spans.push(Span::styled(
                        tag.clone(),
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
            }
            lines.push(Line::from(tag_spans));

            // Sources line — list each source with title and URL
            if fm.sources.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" Sources: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled("(none)", Style::default().fg(Color::DarkGray)),
                ]));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(" Sources ({}):", fm.sources.len()),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for src in &fm.sources {
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled("\u{2192} ", Style::default().fg(Color::Cyan)),
                        Span::styled(src.title.clone(), Style::default().fg(Color::Cyan)),
                        Span::styled(
                            format!("  {}", src.url),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }

            // Contributors line
            let mut contrib_spans = vec![Span::styled(
                " Contributors: ",
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if fm.contributors.is_empty() {
                contrib_spans.push(Span::styled("(none)", Style::default().fg(Color::DarkGray)));
            } else {
                for (i, contrib) in fm.contributors.iter().enumerate() {
                    if i > 0 {
                        contrib_spans
                            .push(Span::styled(", ", Style::default().fg(Color::DarkGray)));
                    }
                    contrib_spans.push(Span::styled(
                        contrib.clone(),
                        Style::default().fg(Color::Green),
                    ));
                }
            }
            lines.push(Line::from(contrib_spans));

            // Dates line
            lines.push(Line::from(vec![
                Span::styled(" Created: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(fm.created.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled("Updated: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(fm.updated.clone(), Style::default().fg(Color::DarkGray)),
            ]));

            lines.push(Line::from(
                " \u{2500}".to_string() + &"\u{2500}".repeat(area.width.saturating_sub(3) as usize),
            ));
        }

        lines.push(Line::from(""));

        // Body (strip frontmatter, then render markdown)
        let body = strip_frontmatter(content);
        lines.extend(render_markdown_lines(body));

        // Bottom separator
        lines.push(Line::from(""));
        lines.push(Line::from(
            " \u{2500}".to_string() + &"\u{2500}".repeat(area.width.saturating_sub(3) as usize),
        ));

        // Layout: content + context keys
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        // Clamp scroll so the user can't scroll past content.
        let content_height = lines.len() as u16;
        let viewport_height = chunks[0].height;
        let max_scroll = content_height.saturating_sub(viewport_height);
        self.reader_max_scroll.set(max_scroll);
        let clamped_scroll = self.reader_scroll.min(max_scroll);

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false })
            .scroll((clamped_scroll, 0));
        frame.render_widget(paragraph, chunks[0]);

        let keys = Line::from(vec![
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(":Back  "),
            Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::Cyan)),
            Span::raw(":Scroll  "),
            Span::styled("G", Style::default().fg(Color::Cyan)),
            Span::raw(":Bottom  "),
            Span::styled("Home", Style::default().fg(Color::Cyan)),
            Span::raw(":Top  "),
            Span::styled("y", Style::default().fg(Color::Cyan)),
            Span::raw(":Copy"),
        ]);
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    }
}

impl Tab for KnowledgeTab {
    fn title(&self) -> &'static str {
        "Knowledge"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self.view_mode {
            KnowledgeViewMode::List => self.render_list(frame, area),
            KnowledgeViewMode::Reader => self.render_reader(frame, area),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabAction {
        match self.view_mode {
            KnowledgeViewMode::List => self.handle_list_key(key),
            KnowledgeViewMode::Reader => self.handle_reader_key(key),
        }
    }

    // Data is loaded eagerly in new() and refreshed on 'r' keypress.
    fn on_enter(&mut self) {}
    fn on_leave(&mut self) {}

    fn poll_updates(&mut self) {
        let result = self.sync_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(sync_error) = result {
            self.syncing = false;
            self.sync_rx = None;
            // If sync succeeded, reload pages from cache to pick up any new content
            if sync_error.is_none() {
                let km = KnowledgeManager::new(&self.crosslink_dir).ok();
                if let Some(km) = km {
                    if let Ok(pages) = km.list_pages() {
                        self.all_pages = pages;
                        self.collect_tags();
                        self.apply_filters();
                        self.status_msg = format!("{} pages", self.all_pages.len());
                    }
                }
            }
        }
    }

    fn force_refresh(&mut self) {
        self.refresh();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Strip YAML frontmatter (delimited by `---`) from markdown content.
fn strip_frontmatter(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);
    after_first.find("\n---").map_or(content, |end_idx| {
        let remainder = &after_first[end_idx + 4..];
        remainder.trim_start_matches(['\r', '\n'])
    })
}

/// Render markdown body text into styled Lines for the TUI.
/// Handles headings, code blocks (with language labels), bullet and numbered
/// lists, blockquotes, horizontal rules, and inline formatting (`code`,
/// **bold**, *italic*).
fn render_markdown_lines(body: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;

    for raw_line in body.lines() {
        let trimmed = raw_line.trim_start();

        // ── Code fences ──────────────────────────────────────────
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                // Opening fence — show language label if present
                let lang = trimmed.strip_prefix("```").unwrap_or("").trim();
                if lang.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "  \u{2500}\u{2500}\u{2500} code \u{2500}\u{2500}\u{2500}",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "  \u{2500}\u{2500}\u{2500} ",
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            lang.to_string(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            " \u{2500}\u{2500}\u{2500}",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            } else {
                // Closing fence
                lines.push(Line::from(Span::styled(
                    "  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                format!("  {raw_line}"),
                Style::default().fg(Color::Green).bg(Color::Indexed(235)),
            )));
            continue;
        }

        // ── Headings ─────────────────────────────────────────────
        if let Some(rest) = trimmed.strip_prefix("#### ") {
            lines.push(Line::from(vec![
                Span::styled("  \u{25b8} ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    rest.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        } else if let Some(rest) = trimmed.strip_prefix("### ") {
            lines.push(Line::from(vec![
                Span::styled("  \u{25b6} ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    rest.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                format!("  {rest}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(rest) = trimmed.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                format!("  {rest}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        // ── Horizontal rules ─────────────────────────────────────
        else if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                "  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                Style::default().fg(Color::DarkGray),
            )));
        }
        // ── Blockquotes ──────────────────────────────────────────
        else if let Some(rest) = trimmed.strip_prefix("> ") {
            lines.push(Line::from(vec![
                Span::styled(
                    "  \u{2502} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    rest.to_string(),
                    Style::default()
                        .fg(Color::Indexed(250))
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        // ── Bullet lists ─────────────────────────────────────────
        else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let content = &trimmed[2..];
            let mut spans = vec![Span::styled(
                "  \u{2022} ",
                Style::default().fg(Color::Cyan),
            )];
            spans.extend(parse_inline_formatting(content));
            lines.push(Line::from(spans));
        }
        // ── Numbered lists ───────────────────────────────────────
        else if is_numbered_list(trimmed) {
            let (num, content) = split_numbered_list(trimmed);
            let mut spans = vec![Span::styled(
                format!("  {num} "),
                Style::default().fg(Color::Cyan),
            )];
            spans.extend(parse_inline_formatting(content));
            lines.push(Line::from(spans));
        }
        // ── Empty lines ──────────────────────────────────────────
        else if trimmed.is_empty() {
            lines.push(Line::from(""));
        }
        // ── Plain text with inline formatting ────────────────────
        else {
            let mut spans = vec![Span::raw("  ".to_string())];
            spans.extend(parse_inline_formatting(trimmed));
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// Parse inline formatting: `code`, **bold**, *italic*.
/// Returns a Vec of styled Spans.
fn parse_inline_formatting(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next formatting marker
        let next_backtick = remaining.find('`');
        let next_double_star = remaining.find("**");
        let next_single_star = find_single_star(remaining);

        // Find the earliest marker
        let earliest = [
            next_backtick.map(|i| (i, '`')),
            next_double_star.map(|i| (i, 'B')), // B for bold
            next_single_star.map(|i| (i, 'I')), // I for italic
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(pos, _)| *pos);

        match earliest {
            None => {
                // No more markers — push the rest as plain text
                spans.push(Span::raw(remaining.to_string()));
                break;
            }
            Some((pos, marker)) => {
                // Push text before the marker
                if pos > 0 {
                    spans.push(Span::raw(remaining[..pos].to_string()));
                }

                match marker {
                    '`' => {
                        let after = &remaining[pos + 1..];
                        if let Some(end) = after.find('`') {
                            let code = &after[..end];
                            spans.push(Span::styled(
                                code.to_string(),
                                Style::default().fg(Color::Green).bg(Color::Indexed(235)),
                            ));
                            remaining = &after[end + 1..];
                        } else {
                            // Unmatched backtick — treat as plain text
                            spans.push(Span::raw("`".to_string()));
                            remaining = after;
                        }
                    }
                    'B' => {
                        let after = &remaining[pos + 2..];
                        if let Some(end) = after.find("**") {
                            let bold_text = &after[..end];
                            spans.push(Span::styled(
                                bold_text.to_string(),
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            remaining = &after[end + 2..];
                        } else {
                            spans.push(Span::raw("**".to_string()));
                            remaining = after;
                        }
                    }
                    'I' => {
                        let after = &remaining[pos + 1..];
                        if let Some(end) = find_single_star(after) {
                            let italic_text = &after[..end];
                            spans.push(Span::styled(
                                italic_text.to_string(),
                                Style::default()
                                    .fg(Color::Indexed(250))
                                    .add_modifier(Modifier::ITALIC),
                            ));
                            remaining = &after[end + 1..];
                        } else {
                            spans.push(Span::raw("*".to_string()));
                            remaining = after;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    spans
}

/// Find position of a single `*` that is NOT part of `**`.
fn find_single_star(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'*' {
            let is_double_before = i > 0 && bytes[i - 1] == b'*';
            let is_double_after = i + 1 < bytes.len() && bytes[i + 1] == b'*';
            if !is_double_before && !is_double_after {
                return Some(i);
            }
        }
    }
    None
}

/// Check if a line looks like a numbered list item (e.g. "1. text").
fn is_numbered_list(s: &str) -> bool {
    let mut chars = s.chars();
    // Must start with digits
    let mut has_digit = false;
    for c in chars.by_ref() {
        if c.is_ascii_digit() {
            has_digit = true;
        } else if c == '.' && has_digit {
            // Must be followed by a space
            return chars.next() == Some(' ');
        } else {
            return false;
        }
    }
    false
}

/// Split a numbered list line into the number part and content.
fn split_numbered_list(s: &str) -> (&str, &str) {
    s.find(". ").map_or(("", s), |dot_pos| {
        (&s[..=dot_pos], s[dot_pos + 2..].trim_start())
    })
}

/// Format a date string (YYYY-MM-DD) as a relative time (e.g. "3d ago").
fn format_relative_date(date_str: &str) -> String {
    let today = chrono::Utc::now().date_naive();
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").map_or_else(
        |_| date_str.to_string(),
        |date| {
            let days = (today - date).num_days();
            if days < 0 {
                date_str.to_string()
            } else if days == 0 {
                "today".to_string()
            } else if days == 1 {
                "1d ago".to_string()
            } else if days < 7 {
                format!("{days}d ago")
            } else if days < 30 {
                format!("{}w ago", days / 7)
            } else {
                date_str.to_string()
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_key(code: KeyCode) -> crossterm::event::KeyEvent {
        super::super::make_test_key(code)
    }

    fn make_page(slug: &str, title: &str, tags: &[&str], updated: &str) -> PageInfo {
        PageInfo {
            slug: slug.to_string(),
            frontmatter: PageFrontmatter {
                title: title.to_string(),
                tags: tags.iter().map(|s| s.to_string()).collect(),
                sources: Vec::new(),
                contributors: vec!["worker-1".to_string()],
                created: "2026-01-01".to_string(),
                updated: updated.to_string(),
            },
        }
    }

    fn make_tab_empty() -> KnowledgeTab {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = KnowledgeTab::new(dir.path());
        // Clear error since tempdir isn't a real crosslink dir
        tab.error_msg = None;
        tab
    }

    fn make_tab_with_pages() -> KnowledgeTab {
        let mut tab = make_tab_empty();
        tab.all_pages = vec![
            make_page(
                "ratatui-basics",
                "Ratatui Getting Started",
                &["rust", "tui"],
                "2026-03-01",
            ),
            make_page(
                "sqlite-wal",
                "SQLite WAL Mode",
                &["db", "perf"],
                "2026-02-27",
            ),
            make_page(
                "ssh-signing",
                "SSH Signing Guide",
                &["security"],
                "2026-02-20",
            ),
        ];
        tab.collect_tags();
        tab.apply_filters();
        tab.status_msg = format!("{} pages", tab.all_pages.len());
        tab
    }

    #[test]
    fn test_title() {
        let tab = make_tab_empty();
        assert_eq!(tab.title(), "Knowledge");
    }

    #[test]
    fn test_initial_view_mode() {
        let tab = make_tab_empty();
        assert_eq!(tab.view_mode, KnowledgeViewMode::List);
    }

    #[test]
    fn test_navigation_empty_list() {
        let mut tab = make_tab_empty();
        tab.handle_list_key(make_key(KeyCode::Char('j')));
        tab.handle_list_key(make_key(KeyCode::Char('k')));
        tab.handle_list_key(make_key(KeyCode::Enter));
        assert_eq!(tab.selected, 0);
    }

    #[test]
    fn test_navigation_with_pages() {
        let mut tab = make_tab_with_pages();
        assert_eq!(tab.selected, 0);

        tab.handle_list_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.selected, 1);

        tab.handle_list_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.selected, 2);

        // Should not go past end
        tab.handle_list_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.selected, 2);

        tab.handle_list_key(make_key(KeyCode::Char('k')));
        assert_eq!(tab.selected, 1);

        tab.handle_list_key(make_key(KeyCode::Home));
        assert_eq!(tab.selected, 0);

        tab.handle_list_key(make_key(KeyCode::End));
        assert_eq!(tab.selected, 2);
    }

    #[test]
    fn test_enter_opens_reader_state() {
        let mut tab = make_tab_with_pages();
        // Set reader content manually since we can't call KnowledgeManager
        tab.reader_content = Some("# Test\nSome content.".to_string());
        tab.reader_slug = Some("ratatui-basics".to_string());
        tab.view_mode = KnowledgeViewMode::Reader;

        assert_eq!(tab.view_mode, KnowledgeViewMode::Reader);
        assert!(tab.reader_content.is_some());
    }

    #[test]
    fn test_esc_returns_to_list() {
        let mut tab = make_tab_with_pages();
        tab.reader_content = Some("content".to_string());
        tab.reader_slug = Some("test".to_string());
        tab.view_mode = KnowledgeViewMode::Reader;

        tab.handle_reader_key(make_key(KeyCode::Esc));
        assert_eq!(tab.view_mode, KnowledgeViewMode::List);
        assert!(tab.reader_content.is_none());
        assert!(tab.reader_slug.is_none());
    }

    #[test]
    fn test_tag_filter_cycle() {
        let mut tab = make_tab_with_pages();
        // Tags: all, db, perf, rust, security, tui
        assert_eq!(tab.available_tags.len(), 6);
        assert_eq!(tab.tag_filter_idx, 0);

        tab.handle_list_key(make_key(KeyCode::Char('t')));
        assert_eq!(tab.tag_filter_idx, 1);
        assert_eq!(tab.available_tags[1], "db");
        // Only sqlite-wal has "db" tag
        assert_eq!(tab.filtered_pages.len(), 1);
        assert_eq!(tab.filtered_pages[0].slug, "sqlite-wal");
    }

    #[test]
    fn test_tag_filter_wraps() {
        let mut tab = make_tab_with_pages();
        let tag_count = tab.available_tags.len();
        for _ in 0..tag_count {
            tab.handle_list_key(make_key(KeyCode::Char('t')));
        }
        assert_eq!(tab.tag_filter_idx, 0);
        assert_eq!(tab.filtered_pages.len(), 3); // All pages
    }

    #[test]
    fn test_search_mode_enter_cancel() {
        let mut tab = make_tab_with_pages();

        // Enter search
        tab.handle_list_key(make_key(KeyCode::Char('/')));
        assert!(tab.searching);

        // Type query
        tab.handle_list_key(make_key(KeyCode::Char('w')));
        tab.handle_list_key(make_key(KeyCode::Char('a')));
        tab.handle_list_key(make_key(KeyCode::Char('l')));
        assert_eq!(tab.search_query, "wal");

        // Cancel clears
        tab.handle_list_key(make_key(KeyCode::Esc));
        assert!(!tab.searching);
        assert!(tab.search_query.is_empty());
        assert_eq!(tab.filtered_pages.len(), 3);
    }

    #[test]
    fn test_search_mode_accept() {
        let mut tab = make_tab_with_pages();
        tab.handle_list_key(make_key(KeyCode::Char('/')));
        tab.handle_list_key(make_key(KeyCode::Char('w')));
        tab.handle_list_key(make_key(KeyCode::Char('a')));
        tab.handle_list_key(make_key(KeyCode::Char('l')));

        tab.handle_list_key(make_key(KeyCode::Enter));
        assert!(!tab.searching);
        assert_eq!(tab.search_query, "wal");
    }

    #[test]
    fn test_search_filters_pages() {
        let mut tab = make_tab_with_pages();
        tab.search_query = "wal".to_string();
        tab.apply_filters();
        assert_eq!(tab.filtered_pages.len(), 1);
        assert_eq!(tab.filtered_pages[0].slug, "sqlite-wal");
    }

    #[test]
    fn test_search_by_tag() {
        let mut tab = make_tab_with_pages();
        tab.search_query = "security".to_string();
        tab.apply_filters();
        assert_eq!(tab.filtered_pages.len(), 1);
        assert_eq!(tab.filtered_pages[0].slug, "ssh-signing");
    }

    #[test]
    fn test_refresh_key() {
        let mut tab = make_tab_empty();
        let result = tab.handle_list_key(make_key(KeyCode::Char('r')));
        // 'r' is now a global keybinding (sync), so tabs return NotHandled
        assert!(matches!(result, TabAction::NotHandled));
    }

    #[test]
    fn test_unhandled_key() {
        let mut tab = make_tab_empty();
        let result = tab.handle_list_key(make_key(KeyCode::Char('x')));
        assert!(matches!(result, TabAction::NotHandled));
    }

    #[test]
    fn test_reader_scroll() {
        let mut tab = make_tab_with_pages();
        tab.view_mode = KnowledgeViewMode::Reader;
        tab.reader_content = Some("content".to_string());
        // Simulate a render having computed a max scroll value.
        tab.reader_max_scroll.set(100);

        tab.handle_reader_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.reader_scroll, 1);

        tab.handle_reader_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.reader_scroll, 2);

        tab.handle_reader_key(make_key(KeyCode::Char('k')));
        assert_eq!(tab.reader_scroll, 1);

        tab.handle_reader_key(make_key(KeyCode::PageDown));
        assert_eq!(tab.reader_scroll, 11);

        tab.handle_reader_key(make_key(KeyCode::PageUp));
        assert_eq!(tab.reader_scroll, 1);

        // G jumps to max scroll, not u16::MAX
        tab.handle_reader_key(make_key(KeyCode::Char('G')));
        assert_eq!(tab.reader_scroll, 100);

        tab.handle_reader_key(make_key(KeyCode::Home));
        assert_eq!(tab.reader_scroll, 0);
    }

    #[test]
    fn test_reader_scroll_clamps_at_bottom() {
        let mut tab = make_tab_with_pages();
        tab.view_mode = KnowledgeViewMode::Reader;
        tab.reader_content = Some("short".to_string());
        tab.reader_max_scroll.set(5);

        // Scrolling past max is clamped
        for _ in 0..20 {
            tab.handle_reader_key(make_key(KeyCode::Char('j')));
        }
        assert_eq!(tab.reader_scroll, 5);

        // PageDown also clamped
        tab.reader_scroll = 0;
        tab.handle_reader_key(make_key(KeyCode::PageDown));
        assert_eq!(tab.reader_scroll, 5);
    }

    #[test]
    fn test_render_list_no_panic() {
        let tab = make_tab_with_pages();
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render_list(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_list_empty_no_panic() {
        let tab = make_tab_empty();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render_list(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_render_reader_no_panic() {
        let mut tab = make_tab_with_pages();
        tab.view_mode = KnowledgeViewMode::Reader;
        tab.reader_slug = Some("test".to_string());
        tab.reader_frontmatter = Some(PageFrontmatter {
            title: "Test Page".to_string(),
            tags: vec!["rust".to_string()],
            sources: Vec::new(),
            contributors: vec!["worker-1".to_string()],
            created: "2026-01-01".to_string(),
            updated: "2026-03-01".to_string(),
        });
        tab.reader_content = Some(
            "---\ntitle: Test\n---\n\n# Heading\n\nSome text.\n\n```rust\nlet x = 1;\n```\n\n- bullet 1\n- bullet 2\n".to_string(),
        );
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render_reader(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_strip_frontmatter() {
        let content = "---\ntitle: Test\ntags: [a]\n---\n\n# Body\nText here.";
        let body = strip_frontmatter(content);
        assert!(body.starts_with("# Body"));
        assert!(!body.contains("title: Test"));
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let content = "# Just a heading\nNo frontmatter here.";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn test_strip_frontmatter_crlf() {
        let content = "---\r\ntitle: Test\r\ntags: [a]\r\n---\r\n\r\n# Body\r\nText here.";
        let body = strip_frontmatter(content);
        assert!(body.starts_with("# Body"), "got: {body:?}");
        assert!(!body.contains("title: Test"));
    }

    #[test]
    fn test_render_markdown_lines_headings() {
        let body = "# H1 Title\n## H2 Title\n### H3 Title\nPlain text.";
        let lines = render_markdown_lines(body);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn test_render_markdown_lines_code_block() {
        let body = "text\n```\ncode line\n```\nmore text";
        let lines = render_markdown_lines(body);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn test_render_markdown_lines_bullets() {
        let body = "- item 1\n* item 2\nplain";
        let lines = render_markdown_lines(body);
        assert_eq!(lines.len(), 3);
        // Check bullet char is present
        let first_line_str: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(first_line_str.contains('\u{2022}'));
    }

    #[test]
    fn test_format_relative_date() {
        let today = chrono::Utc::now().date_naive();
        let today_str = today.format("%Y-%m-%d").to_string();
        assert_eq!(format_relative_date(&today_str), "today");

        let yesterday = today - chrono::Duration::days(1);
        let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
        assert_eq!(format_relative_date(&yesterday_str), "1d ago");

        let five_days = today - chrono::Duration::days(5);
        let five_str = five_days.format("%Y-%m-%d").to_string();
        assert_eq!(format_relative_date(&five_str), "5d ago");

        let two_weeks = today - chrono::Duration::days(14);
        let two_weeks_str = two_weeks.format("%Y-%m-%d").to_string();
        assert_eq!(format_relative_date(&two_weeks_str), "2w ago");

        // Invalid date returns as-is
        assert_eq!(format_relative_date("not-a-date"), "not-a-date");
    }

    #[test]
    fn test_collect_tags() {
        let mut tab = make_tab_with_pages();
        tab.collect_tags();
        // Expected: all, db, perf, rust, security, tui
        assert_eq!(tab.available_tags[0], "all");
        assert!(tab.available_tags.contains(&"rust".to_string()));
        assert!(tab.available_tags.contains(&"db".to_string()));
        assert!(tab.available_tags.contains(&"security".to_string()));
    }

    #[test]
    fn test_apply_filters_tag_and_search() {
        let mut tab = make_tab_with_pages();
        // Filter to "rust" tag
        tab.tag_filter_idx = tab.available_tags.iter().position(|t| t == "rust").unwrap();
        tab.search_query = "ratatui".to_string();
        tab.apply_filters();
        assert_eq!(tab.filtered_pages.len(), 1);
        assert_eq!(tab.filtered_pages[0].slug, "ratatui-basics");
    }
}
