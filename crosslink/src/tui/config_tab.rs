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
use crate::commands::config::{self, ConfigGroup, ConfigType, Source, WriteScope, REGISTRY};
use crate::db::Database;
use crate::events;
use crate::identity::AgentConfig;
use crate::sync::SyncManager;

/// Which sub-view is active.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ViewMode {
    /// Main diagnostics overview with inline config editing.
    Main,
    /// Full event log browser.
    EventLog,
    /// Sub-list editor for array keys.
    EditArray,
    /// Confirmation prompt before writing a config change.
    ConfirmWrite,
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

/// Per-key display entry driven by the registry.
struct ConfigEntry {
    key: String,
    value: String,
    source: Source,
    team_value: Option<String>,
    is_default: bool,
    hot_swappable: bool,
    _group: ConfigGroup,
    config_type: ConfigType,
    description: String,
}

/// Pending config change waiting for scope confirmation.
struct PendingChange {
    key: String,
    old_value: String,
    new_value: String,
    scope: WriteScope,
}

/// The Config tab — configuration and diagnostics dashboard with inline editing.
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

    // Configuration — registry-driven (REQ-1)
    config_entries: Vec<ConfigEntry>,
    config_cursor: usize,

    // Shell alias status (REQ-11)
    alias_installed: bool,
    alias_file: String,

    // Inline editing state
    _edit_text: String,
    pending_change: Option<PendingChange>,

    // Array editing
    array_items: Vec<String>,
    array_cursor: usize,
    array_key: String,

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
            config_cursor: 0,
            alias_installed: false,
            alias_file: String::new(),
            _edit_text: String::new(),
            pending_change: None,
            array_items: Vec::new(),
            array_cursor: 0,
            array_key: String::new(),
            recent_events: Vec::new(),
            all_events: Vec::new(),
            event_scroll: 0,
            error_msg: None,
            loading_sync: false,
            sync_rx: None,
        };
        tab.load_identity();
        tab.load_db_info(db);
        tab.load_config();
        tab.load_alias_status();
        tab.start_background_sync();
        tab
    }

    fn open_db(&self) -> Option<Database> {
        Database::open(&self.db_path).ok()
    }

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
            Ok(Some(cfg)) => {
                self.agent_id = cfg.agent_id;
                self.machine_id = cfg.machine_id;
                self.ssh_fingerprint = cfg.ssh_fingerprint.unwrap_or_else(|| "(none)".to_string());
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

        let resolved = match config::read_config_layered(&self.crosslink_dir) {
            Ok(r) => r,
            Err(_) => {
                // Fallback: no config
                return;
            }
        };

        let defaults = match serde_json::from_str::<serde_json::Value>(
            crate::commands::init::HOOK_CONFIG_JSON,
        ) {
            Ok(d) => d,
            Err(_) => return,
        };

        for entry in REGISTRY.iter() {
            let current = resolved.merged.get(entry.key);
            let default = defaults.get(entry.key);
            let source = resolved
                .provenance
                .get(entry.key)
                .copied()
                .unwrap_or(Source::Default);
            let is_default = current == default;

            let value_str = current
                .map(format_json_value)
                .unwrap_or_else(|| "(unset)".into());

            let team_value = if source == Source::Local {
                resolved.team.get(entry.key).map(format_json_value)
            } else {
                None
            };

            self.config_entries.push(ConfigEntry {
                key: entry.key.to_string(),
                value: value_str,
                source,
                team_value,
                is_default,
                hot_swappable: entry.hot_swappable,
                _group: entry.group,
                config_type: entry.config_type,
                description: entry.description.to_string(),
            });
        }
    }

    fn load_alias_status(&mut self) {
        let (installed, file) = config::detect_alias_status();
        self.alias_installed = installed;
        self.alias_file = file;
    }

    fn refresh(&mut self) {
        self.error_msg = None;
        if let Some(db) = self.open_db() {
            self.load_identity();
            self.load_db_info(&db);
            self.load_config();
            self.load_alias_status();
        }
        self.start_background_sync();
    }

    // ── Config editing helpers ──────────────────────────────────────

    fn current_config_entry(&self) -> Option<&ConfigEntry> {
        self.config_entries.get(self.config_cursor)
    }

    fn cycle_enum_or_bool(&mut self) {
        if let Some(entry) = self.config_entries.get(self.config_cursor) {
            let new_value = match entry.config_type {
                ConfigType::Bool => {
                    if entry.value == "true" {
                        "false".to_string()
                    } else {
                        "true".to_string()
                    }
                }
                ConfigType::Enum(options) => {
                    let current_idx = options.iter().position(|o| *o == entry.value);
                    let next = match current_idx {
                        Some(i) => (i + 1) % options.len(),
                        None => 0,
                    };
                    options[next].to_string()
                }
                _ => return,
            };

            self.pending_change = Some(PendingChange {
                key: entry.key.clone(),
                old_value: entry.value.clone(),
                new_value,
                scope: WriteScope::Team,
            });
            self.view_mode = ViewMode::ConfirmWrite;
        }
    }

    fn open_array_editor(&mut self) {
        if let Some(entry) = self.config_entries.get(self.config_cursor) {
            if !matches!(entry.config_type, ConfigType::StringArray) {
                return;
            }
            self.array_key = entry.key.clone();

            // Load current array items from merged config
            if let Ok(resolved) = config::read_config_layered(&self.crosslink_dir) {
                if let Some(serde_json::Value::Array(arr)) = resolved.merged.get(&entry.key) {
                    self.array_items = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                } else {
                    self.array_items = Vec::new();
                }
            }
            self.array_cursor = 0;
            self.view_mode = ViewMode::EditArray;
        }
    }

    fn apply_pending_change(&mut self) {
        if let Some(ref change) = self.pending_change {
            let entry = config::find_registry_key(&change.key);
            if let Some(reg) = entry {
                let json_val = match reg.config_type {
                    ConfigType::Bool => match change.new_value.as_str() {
                        "true" => serde_json::Value::Bool(true),
                        _ => serde_json::Value::Bool(false),
                    },
                    _ => serde_json::Value::String(change.new_value.clone()),
                };

                let scope = change.scope;
                let key = change.key.clone();

                // Read the appropriate config file
                let mut cfg = match scope {
                    WriteScope::Team => {
                        let path = self.crosslink_dir.join("hook-config.json");
                        std::fs::read_to_string(&path)
                            .ok()
                            .and_then(|c| serde_json::from_str(&c).ok())
                            .unwrap_or_else(|| serde_json::json!({}))
                    }
                    WriteScope::Local => {
                        let path = self.crosslink_dir.join("hook-config.local.json");
                        std::fs::read_to_string(&path)
                            .ok()
                            .and_then(|c| serde_json::from_str(&c).ok())
                            .unwrap_or_else(|| serde_json::json!({}))
                    }
                };

                cfg[&key] = json_val;
                let _ = config::write_config_scoped(&self.crosslink_dir, &cfg, scope);
            }
        }
        self.pending_change = None;
        self.view_mode = ViewMode::Main;
        self.load_config();
    }

    fn reset_current_key(&mut self) {
        if let Some(entry) = self.config_entries.get(self.config_cursor) {
            if entry.is_default {
                return;
            }
            let defaults: serde_json::Value =
                serde_json::from_str(crate::commands::init::HOOK_CONFIG_JSON).unwrap_or_default();
            if let Some(default_val) = defaults.get(&entry.key) {
                self.pending_change = Some(PendingChange {
                    key: entry.key.clone(),
                    old_value: entry.value.clone(),
                    new_value: format_json_value(default_val),
                    scope: WriteScope::Team,
                });
                self.view_mode = ViewMode::ConfirmWrite;
            }
        }
    }

    // ── Rendering ────────────────────────────────────────────────────

    fn render_main(&self, frame: &mut Frame, area: Rect) {
        // Split: main content area + help/description pane at bottom
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(4)])
            .split(area);

        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                " Configuration & Diagnostics",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            section_header("Agent Identity"),
            kv_line("Agent ID", &self.agent_id, Color::White),
            kv_line("Machine", &self.machine_id, Color::White),
            kv_line("SSH Key", &self.ssh_fingerprint, Color::DarkGray),
        ];

        // Shell alias status (REQ-11)
        let alias_str = if self.alias_installed {
            format!("installed ({})", self.alias_file)
        } else {
            "not installed".to_string()
        };
        lines.push(kv_line(
            "xl alias",
            &alias_str,
            if self.alias_installed {
                Color::Green
            } else {
                Color::DarkGray
            },
        ));
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

        // ── Configuration (hot-swappable first, then setup-time) (REQ-9) ──
        lines.push(section_header("Configuration (hot-swappable)"));
        let mut config_line_to_entry: Vec<usize> = Vec::new();
        let mut entry_idx = 0;

        // Hot-swappable keys first
        for (i, ce) in self.config_entries.iter().enumerate() {
            if !ce.hot_swappable {
                continue;
            }
            let is_focused = entry_idx == self.config_cursor;
            lines.push(self.render_config_entry(ce, is_focused));
            config_line_to_entry.push(i);
            entry_idx += 1;
        }

        lines.push(Line::from(""));
        lines.push(section_header("Configuration (setup-time)"));

        // Setup-time keys
        for (i, ce) in self.config_entries.iter().enumerate() {
            if ce.hot_swappable {
                continue;
            }
            let is_focused = entry_idx == self.config_cursor;
            lines.push(self.render_config_entry(ce, is_focused));
            config_line_to_entry.push(i);
            entry_idx += 1;
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
            " r:Reset  e:Events  Enter:Edit  \u{2191}\u{2193}/j/k:Navigate",
            Style::default().fg(Color::DarkGray),
        )));

        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL))
            .scroll((self.main_scroll as u16, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(para, chunks[0]);

        // Help pane — description of focused key (REQ-8)
        let help_lines = if let Some(entry) = self.current_config_entry() {
            let valid = match entry.config_type {
                ConfigType::Bool => "Valid: true, false".to_string(),
                ConfigType::Enum(opts) => format!("Valid: {}", opts.join(", ")),
                ConfigType::StringArray => "Type: string array (Enter to edit list)".to_string(),
                ConfigType::Map => "Type: map (use CLI to edit)".to_string(),
                ConfigType::String => "Type: string".to_string(),
                ConfigType::Integer => "Type: integer".to_string(),
            };
            vec![
                Line::from(Span::styled(
                    format!(" {} — {}", entry.key, entry.description),
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    format!(" {}", valid),
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        } else {
            vec![Line::from(Span::styled(
                " Select a config key to see details",
                Style::default().fg(Color::DarkGray),
            ))]
        };

        let help_para = Paragraph::new(help_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Span::styled(
                    " Key Info ",
                    Style::default().fg(Color::DarkGray),
                )),
        );
        frame.render_widget(help_para, chunks[1]);
    }

    fn render_config_entry(&self, ce: &ConfigEntry, focused: bool) -> Line<'static> {
        let marker = if !ce.is_default { "*" } else { " " };
        let source_badge = match ce.source {
            Source::Default => "[default]",
            Source::Team => "[team]",
            Source::Local => "[local]",
        };

        let key_style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let val_color = match ce.value.as_str() {
            "true" => Color::Green,
            "false" => Color::Red,
            "strict" | "enforced" | "required" => Color::Yellow,
            "relaxed" | "disabled" | "none" => Color::DarkGray,
            _ => Color::White,
        };

        let cursor_marker = if focused { "\u{276f}" } else { " " };

        let mut spans = vec![
            Span::styled(format!(" {cursor_marker}{marker}"), key_style),
            Span::styled(format!("{:<28}", ce.key), key_style),
            Span::styled(format!("{:<16}", ce.value), Style::default().fg(val_color)),
            Span::styled(
                source_badge.to_string(),
                Style::default().fg(match ce.source {
                    Source::Default => Color::DarkGray,
                    Source::Team => Color::Blue,
                    Source::Local => Color::Magenta,
                }),
            ),
        ];

        // Show override info (REQ-7)
        if let Some(ref team_val) = ce.team_value {
            spans.push(Span::styled(
                format!(" (overrides: {})", team_val),
                Style::default().fg(Color::DarkGray),
            ));
        }

        Line::from(spans)
    }

    fn render_edit_array(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);

        let header = Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" Editing: {} ", self.array_key),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("({} items)", self.array_items.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        frame.render_widget(header, chunks[0]);

        if self.array_items.is_empty() {
            let empty = Paragraph::new(Line::from(Span::styled(
                " (empty)",
                Style::default().fg(Color::DarkGray),
            )))
            .block(Block::default().borders(Borders::ALL));
            frame.render_widget(empty, chunks[1]);
        } else {
            let rows: Vec<Row> = self
                .array_items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let marker = if i == self.array_cursor {
                        "\u{276f} "
                    } else {
                        "  "
                    };
                    let style = if i == self.array_cursor {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    };
                    Row::new(vec![
                        ratatui::widgets::Cell::from(marker).style(style),
                        ratatui::widgets::Cell::from(item.clone()).style(style),
                    ])
                })
                .collect();

            let widths = [Constraint::Length(3), Constraint::Min(20)];
            let table = Table::new(rows, widths).block(Block::default().borders(Borders::ALL));
            frame.render_widget(table, chunks[1]);
        }

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " d:Delete  Esc:Back",
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[2],
        );
    }

    fn render_confirm_write(&self, frame: &mut Frame, area: Rect) {
        if let Some(ref change) = self.pending_change {
            let scope_str = match change.scope {
                WriteScope::Team => "team (hook-config.json)",
                WriteScope::Local => "local (hook-config.local.json)",
            };

            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    " Confirm config change",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("   Key:   ", Style::default().fg(Color::DarkGray)),
                    Span::styled(change.key.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("   From:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(change.old_value.clone(), Style::default().fg(Color::Red)),
                ]),
                Line::from(vec![
                    Span::styled("   To:    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(change.new_value.clone(), Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::styled("   Scope: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        scope_str,
                        Style::default().fg(match change.scope {
                            WriteScope::Team => Color::Blue,
                            WriteScope::Local => Color::Magenta,
                        }),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    " Enter:Apply  t:Team  l:Local  Esc:Cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
            frame.render_widget(para, area);
        }
    }

    fn render_event_log(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);

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
                if !self.config_entries.is_empty()
                    && self.config_cursor < self.config_entries.len() - 1
                {
                    self.config_cursor += 1;
                }
                // Also scroll the view
                self.main_scroll = self.main_scroll.saturating_add(1);
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.config_cursor = self.config_cursor.saturating_sub(1);
                self.main_scroll = self.main_scroll.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::PageDown => {
                let jump = 10.min(
                    self.config_entries
                        .len()
                        .saturating_sub(1)
                        .saturating_sub(self.config_cursor),
                );
                self.config_cursor += jump;
                self.main_scroll = self.main_scroll.saturating_add(10);
                TabAction::Consumed
            }
            KeyCode::PageUp => {
                let jump = 10.min(self.config_cursor);
                self.config_cursor -= jump;
                self.main_scroll = self.main_scroll.saturating_sub(10);
                TabAction::Consumed
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.config_cursor = 0;
                self.main_scroll = 0;
                TabAction::Consumed
            }
            KeyCode::Enter => {
                // Edit focused key based on type (REQ-6)
                if let Some(entry) = self.config_entries.get(self.config_cursor) {
                    match entry.config_type {
                        ConfigType::Bool | ConfigType::Enum(_) => self.cycle_enum_or_bool(),
                        ConfigType::StringArray => self.open_array_editor(),
                        _ => {} // String/Integer/Map — use CLI
                    }
                }
                TabAction::Consumed
            }
            KeyCode::Char('r') => {
                // Reset current key to default (REQ-9)
                self.reset_current_key();
                TabAction::Consumed
            }
            KeyCode::Char('e') => {
                self.view_mode = ViewMode::EventLog;
                self.event_scroll = 0;
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
            _ => TabAction::NotHandled,
        }
    }

    fn handle_edit_array_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.view_mode = ViewMode::Main;
                TabAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.array_items.is_empty() && self.array_cursor < self.array_items.len() - 1 {
                    self.array_cursor += 1;
                }
                TabAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.array_cursor = self.array_cursor.saturating_sub(1);
                TabAction::Consumed
            }
            KeyCode::Char('d') => {
                // Delete selected item
                if !self.array_items.is_empty() {
                    self.array_items.remove(self.array_cursor);
                    if self.array_cursor >= self.array_items.len() && self.array_cursor > 0 {
                        self.array_cursor -= 1;
                    }
                    // Write back
                    self.save_array_items();
                }
                TabAction::Consumed
            }
            _ => TabAction::Consumed,
        }
    }

    fn save_array_items(&mut self) {
        let items: Vec<serde_json::Value> = self
            .array_items
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect();
        let path = self.crosslink_dir.join("hook-config.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(mut cfg) = serde_json::from_str::<serde_json::Value>(&content) {
                cfg[&self.array_key] = serde_json::Value::Array(items);
                if let Ok(pretty) = serde_json::to_string_pretty(&cfg) {
                    let _ = std::fs::write(&path, format!("{pretty}\n"));
                }
            }
        }
        self.load_config();
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            KeyCode::Esc => {
                self.pending_change = None;
                self.view_mode = ViewMode::Main;
                TabAction::Consumed
            }
            KeyCode::Enter => {
                self.apply_pending_change();
                TabAction::Consumed
            }
            KeyCode::Char('t') => {
                if let Some(ref mut change) = self.pending_change {
                    change.scope = WriteScope::Team;
                }
                TabAction::Consumed
            }
            KeyCode::Char('l') => {
                if let Some(ref mut change) = self.pending_change {
                    change.scope = WriteScope::Local;
                }
                TabAction::Consumed
            }
            _ => TabAction::Consumed,
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
            ViewMode::EditArray => self.render_edit_array(frame, area),
            ViewMode::ConfirmWrite => self.render_confirm_write(frame, area),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabAction {
        match self.view_mode {
            ViewMode::Main => self.handle_main_key(key),
            ViewMode::EventLog => self.handle_event_log_key(key),
            ViewMode::EditArray => self.handle_edit_array_key(key),
            ViewMode::ConfirmWrite => self.handle_confirm_key(key),
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
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            format!("[{}]", items.join(", "))
        }
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
            format!("StatusChanged \u{2192} {new_status}")
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
        // Write a minimal hook-config.json so load_config can parse it
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            crate::commands::init::HOOK_CONFIG_JSON,
        )
        .unwrap();
        let db_path = crosslink_dir.join("issues.db");
        let db = Database::open(&db_path).unwrap();
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
        // Config entries should be populated from registry
        assert!(!tab.config_entries.is_empty());
    }

    #[test]
    fn test_config_entries_from_registry() {
        let (tab, _dir) = setup_tab();
        // Should have entries for all registry keys
        assert!(tab.config_entries.len() >= 11);
        // Check that provenance is set
        assert!(tab.config_entries.iter().any(|e| e.key == "tracking_mode"));
    }

    #[test]
    fn test_main_scroll() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(tab.config_cursor, 1);
        tab.handle_key(make_key(KeyCode::Down));
        assert_eq!(tab.config_cursor, 2);
        tab.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(tab.config_cursor, 1);
        tab.handle_key(make_key(KeyCode::Char('g')));
        assert_eq!(tab.config_cursor, 0);
    }

    #[test]
    fn test_page_scroll() {
        let (mut tab, _dir) = setup_tab();
        tab.handle_key(make_key(KeyCode::PageDown));
        assert!(tab.config_cursor > 0);
        tab.handle_key(make_key(KeyCode::PageUp));
        assert_eq!(tab.config_cursor, 0);
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
    fn test_enter_cycles_enum() {
        let (mut tab, _dir) = setup_tab();
        // First entry should be tracking_mode (hot-swappable, displayed first)
        tab.config_cursor = 0;
        tab.handle_key(make_key(KeyCode::Enter));
        // Should enter ConfirmWrite mode
        assert_eq!(tab.view_mode, ViewMode::ConfirmWrite);
        assert!(tab.pending_change.is_some());
    }

    #[test]
    fn test_confirm_scope_toggle() {
        let (mut tab, _dir) = setup_tab();
        tab.config_cursor = 0;
        tab.handle_key(make_key(KeyCode::Enter));
        assert_eq!(tab.view_mode, ViewMode::ConfirmWrite);
        // Toggle to local
        tab.handle_key(make_key(KeyCode::Char('l')));
        assert!(matches!(
            tab.pending_change.as_ref().unwrap().scope,
            WriteScope::Local
        ));
        // Toggle back to team
        tab.handle_key(make_key(KeyCode::Char('t')));
        assert!(matches!(
            tab.pending_change.as_ref().unwrap().scope,
            WriteScope::Team
        ));
    }

    #[test]
    fn test_confirm_cancel() {
        let (mut tab, _dir) = setup_tab();
        tab.config_cursor = 0;
        tab.handle_key(make_key(KeyCode::Enter));
        assert_eq!(tab.view_mode, ViewMode::ConfirmWrite);
        tab.handle_key(make_key(KeyCode::Esc));
        assert_eq!(tab.view_mode, ViewMode::Main);
        assert!(tab.pending_change.is_none());
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
    fn test_render_confirm_no_panic() {
        let (mut tab, _dir) = setup_tab();
        tab.pending_change = Some(PendingChange {
            key: "tracking_mode".to_string(),
            old_value: "strict".to_string(),
            new_value: "normal".to_string(),
            scope: WriteScope::Team,
        });
        tab.view_mode = ViewMode::ConfirmWrite;
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
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            crate::commands::init::HOOK_CONFIG_JSON,
        )
        .unwrap();
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
        assert!(!tab.config_entries.is_empty());
        assert!(tab
            .config_entries
            .iter()
            .any(|e| e.key == "tracking_mode" && e.value == "strict"));
    }

    #[test]
    fn test_with_local_override() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"tracking_mode": "strict", "intervention_tracking": true}"#,
        )
        .unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.local.json"),
            r#"{"tracking_mode": "relaxed"}"#,
        )
        .unwrap();
        let db_path = crosslink_dir.join("issues.db");
        let db = Database::open(&db_path).unwrap();
        let tab = ConfigTab::new(&db, &db_path, &crosslink_dir);
        let tm = tab
            .config_entries
            .iter()
            .find(|e| e.key == "tracking_mode")
            .unwrap();
        assert_eq!(tm.value, "relaxed");
        assert_eq!(tm.source, Source::Local);
        assert_eq!(tm.team_value.as_deref(), Some("strict"));
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
            "StatusChanged \u{2192} closed"
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
    fn test_format_json_value() {
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
        tab.handle_key(make_key(KeyCode::Down));
        assert_eq!(tab.event_scroll, 0);
        tab.handle_key(make_key(KeyCode::PageDown));
        assert_eq!(tab.event_scroll, 0);
    }

    #[test]
    fn test_new_starts_with_loading_sync() {
        let (tab, _dir) = setup_tab();
        assert!(tab.schema_version > 0);
        assert_eq!(tab.issue_count, 1);
    }

    #[test]
    fn test_new_returns_instantly() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            crate::commands::init::HOOK_CONFIG_JSON,
        )
        .unwrap();
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
        std::thread::sleep(std::time::Duration::from_millis(500));
        tab.poll_updates();
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
