// E-ana tablet — kickoff wizard: interactive ratatui TUI for design→plan→run pipeline
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph};
use ratatui::Frame;
use std::path::{Path, PathBuf};

use super::pipeline::{self, PipelineState};

// ── Data types ──────────────────────────────────────────────────────────────

/// Source for the kickoff pipeline.
#[derive(Debug, Clone)]
pub enum WizardSource {
    /// A `.design/*.md` file with optional pipeline state.
    DesignDoc(PathBuf),
    /// Free-text feature description (no design doc).
    QuickDescription(String),
}

/// Stage selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStage {
    Plan,
    Run,
}

/// Configuration for a plan run.
#[derive(Debug, Clone)]
pub struct PlanConfig {
    pub model: String,
    pub timeout: String,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            model: "opus".to_string(),
            timeout: "30m".to_string(),
        }
    }
}

/// Configuration for an implementation run.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub verify: String,
    pub model: String,
    pub timeout: String,
    pub container: String,
    pub issue: Option<i64>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            verify: "local".to_string(),
            model: "opus".to_string(),
            timeout: "1h".to_string(),
            container: "none".to_string(),
            issue: None,
        }
    }
}

/// Final wizard choices ready for dispatch.
#[derive(Debug, Clone)]
pub struct WizardChoices {
    pub source: WizardSource,
    pub stage: WizardStage,
    pub plan_config: Option<PlanConfig>,
    pub run_config: Option<RunConfig>,
}

// ── Design doc entry for source selection ───────────────────────────────────

struct DesignDocEntry {
    path: PathBuf,
    filename: String,
    pipeline: Option<PipelineState>,
    stage_display: String,
    gaps_display: String,
}

// ── Wizard screens ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Screen {
    Source,
    Stage,
    Configure,
    Launch,
}

struct WizardApp {
    screen: Screen,
    // Source screen
    design_docs: Vec<DesignDocEntry>,
    source_selected: usize,
    quick_description: String,
    editing_quick: bool,
    // Stage screen
    stage_selected: usize, // 0 = Plan, 1 = Run
    // Configure screen
    config_options: Vec<ConfigOption>,
    config_selected: usize,
    // State
    source: Option<WizardSource>,
    stage: Option<WizardStage>,
    plan_config: PlanConfig,
    run_config: RunConfig,
    // Control
    finished: bool,
    cancelled: bool,
}

struct ConfigOption {
    label: &'static str,
    values: Vec<&'static str>,
    selected: usize,
}

impl WizardApp {
    fn new(design_docs: Vec<DesignDocEntry>) -> Self {
        Self {
            screen: Screen::Source,
            design_docs,
            source_selected: 0,
            quick_description: String::new(),
            editing_quick: false,
            stage_selected: 1, // Default to Run
            config_options: Vec::new(),
            config_selected: 0,
            source: None,
            stage: None,
            plan_config: PlanConfig::default(),
            run_config: RunConfig::default(),
            finished: false,
            cancelled: false,
        }
    }

    const fn total_source_items(&self) -> usize {
        self.design_docs.len() + 1 // +1 for quick description
    }

    fn confirm_source(&mut self) {
        if self.source_selected < self.design_docs.len() {
            let entry = &self.design_docs[self.source_selected];
            self.source = Some(WizardSource::DesignDoc(entry.path.clone()));
        } else if !self.quick_description.trim().is_empty() {
            self.source = Some(WizardSource::QuickDescription(
                self.quick_description.trim().to_string(),
            ));
        } else {
            return; // Don't advance with empty description
        }
        self.screen = Screen::Stage;
        self.build_stage_defaults();
    }

    fn build_stage_defaults(&mut self) {
        // If source is a design doc with no plan yet, default to Plan
        if let Some(WizardSource::DesignDoc(ref path)) = self.source {
            if let Some(entry) = self.design_docs.iter().find(|e| e.path == *path) {
                if let Some(ref pipeline) = entry.pipeline {
                    match pipeline.stage.as_str() {
                        "designed" => self.stage_selected = 0, // Plan first
                        // "planned" and all other stages are ready to run
                        _ => self.stage_selected = 1,
                    }
                } else {
                    self.stage_selected = 0; // No pipeline — plan first
                }
            }
        } else {
            // Quick description — default to Run
            self.stage_selected = 1;
        }
    }

    fn confirm_stage(&mut self) {
        self.stage = Some(if self.stage_selected == 0 {
            WizardStage::Plan
        } else {
            WizardStage::Run
        });
        self.build_config_options();
        self.screen = Screen::Configure;
    }

    fn build_config_options(&mut self) {
        self.config_options.clear();
        self.config_selected = 0;

        match self.stage {
            Some(WizardStage::Plan) => {
                self.config_options.push(ConfigOption {
                    label: "Model",
                    values: vec!["opus", "sonnet"],
                    selected: 0,
                });
                self.config_options.push(ConfigOption {
                    label: "Timeout",
                    values: vec!["30m", "1h", "2h"],
                    selected: 0,
                });
            }
            Some(WizardStage::Run) => {
                self.config_options.push(ConfigOption {
                    label: "Verify",
                    values: vec!["local", "ci", "thorough"],
                    selected: 0,
                });
                self.config_options.push(ConfigOption {
                    label: "Model",
                    values: vec!["opus", "sonnet"],
                    selected: 0,
                });
                self.config_options.push(ConfigOption {
                    label: "Timeout",
                    values: vec!["1h", "2h", "4h"],
                    selected: 0,
                });
                self.config_options.push(ConfigOption {
                    label: "Container",
                    values: vec!["none", "docker", "podman"],
                    selected: 0,
                });
            }
            None => {}
        }
    }

    fn confirm_config(&mut self) {
        // Apply config selections
        match self.stage {
            Some(WizardStage::Plan) => {
                if let Some(opt) = self.config_options.first() {
                    self.plan_config.model = opt.values[opt.selected].to_string();
                }
                if let Some(opt) = self.config_options.get(1) {
                    self.plan_config.timeout = opt.values[opt.selected].to_string();
                }
            }
            Some(WizardStage::Run) => {
                if let Some(opt) = self.config_options.first() {
                    self.run_config.verify = opt.values[opt.selected].to_string();
                }
                if let Some(opt) = self.config_options.get(1) {
                    self.run_config.model = opt.values[opt.selected].to_string();
                }
                if let Some(opt) = self.config_options.get(2) {
                    self.run_config.timeout = opt.values[opt.selected].to_string();
                }
                if let Some(opt) = self.config_options.get(3) {
                    self.run_config.container = opt.values[opt.selected].to_string();
                }
            }
            None => {}
        }
        self.screen = Screen::Launch;
    }

    const fn confirm_launch(&mut self) {
        self.finished = true;
    }

    const fn go_back(&mut self) {
        match self.screen {
            Screen::Source => {} // Can't go back from first screen
            Screen::Stage => self.screen = Screen::Source,
            Screen::Configure => self.screen = Screen::Stage,
            Screen::Launch => self.screen = Screen::Configure,
        }
    }

    fn into_choices(self) -> Option<WizardChoices> {
        let source = self.source?;
        let stage = self.stage?;
        Some(WizardChoices {
            source,
            stage,
            plan_config: if stage == WizardStage::Plan {
                Some(self.plan_config)
            } else {
                None
            },
            run_config: if stage == WizardStage::Run {
                Some(self.run_config)
            } else {
                None
            },
        })
    }
}

// ── Drawing ─────────────────────────────────────────────────────────────────

fn draw_wizard(frame: &mut Frame, app: &WizardApp) {
    let area = frame.area();

    match app.screen {
        Screen::Source => draw_source_screen(frame, app, area),
        Screen::Stage => draw_stage_screen(frame, app, area),
        Screen::Configure => draw_configure_screen(frame, app, area),
        Screen::Launch => draw_launch_screen(frame, app, area),
    }
}

fn progress_line(current: Screen) -> Line<'static> {
    let screens = [
        ("Source", Screen::Source),
        ("Stage", Screen::Stage),
        ("Configure", Screen::Configure),
        ("Launch", Screen::Launch),
    ];
    let spans: Vec<Span> = screens
        .iter()
        .enumerate()
        .flat_map(|(i, (name, screen))| {
            let mut parts = Vec::new();
            if i > 0 {
                parts.push(Span::styled("  ", Style::default()));
            }
            let (marker, style) = if *screen == current {
                (
                    "\u{25cf} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else if (*screen as u8) < (current as u8) {
                ("\u{25cf} ", Style::default().fg(Color::Green))
            } else {
                ("\u{25cb} ", Style::default().fg(Color::DarkGray))
            };
            parts.push(Span::styled(marker, style));
            parts.push(Span::styled(*name, style));
            parts
        })
        .collect();
    Line::from(spans)
}

fn outer_block() -> Block<'static> {
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
            Span::styled(" kickoff ", Style::default().fg(Color::DarkGray)),
        ]))
        .padding(Padding::new(2, 2, 1, 1))
}

fn draw_source_screen(frame: &mut Frame, app: &WizardApp, area: Rect) {
    let block = outer_block();
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let doc_list_height = (app.design_docs.len() as u16).max(1);
    let chunks = Layout::vertical([
        Constraint::Length(1),               // progress
        Constraint::Length(1),               // spacer
        Constraint::Length(1),               // title
        Constraint::Length(1),               // description
        Constraint::Length(1),               // spacer
        Constraint::Length(doc_list_height), // design docs
        Constraint::Length(1),               // separator
        Constraint::Length(1),               // quick description
        Constraint::Min(1),                  // fill
        Constraint::Length(1),               // help
    ])
    .split(inner);

    // Progress
    frame.render_widget(Paragraph::new(progress_line(Screen::Source)), chunks[0]);

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Select a source",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    // Description
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Choose a design document or enter a quick feature description",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[3],
    );

    // Design doc list
    if app.design_docs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  (no .design/*.md files found)",
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[5],
        );
    } else {
        let items: Vec<ListItem> = app
            .design_docs
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let is_selected = i == app.source_selected;
                let (marker, name_style) = if is_selected {
                    (
                        "\u{276f} ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ("  ", Style::default().fg(Color::Gray))
                };

                let stage_style = match entry.pipeline.as_ref().map(|p| p.stage.as_str()) {
                    Some("planned" | "complete") => Style::default().fg(Color::Green),
                    Some("planning" | "running") => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::DarkGray),
                };

                ListItem::new(Line::from(vec![
                    Span::styled(marker, name_style),
                    Span::styled(format!("{:<36}", entry.filename), name_style),
                    Span::styled(format!("{:<18}", entry.stage_display), stage_style),
                    Span::styled(&entry.gaps_display, Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect();

        let list = List::new(items);
        let mut state =
            ListState::default().with_selected(if app.source_selected < app.design_docs.len() {
                Some(app.source_selected)
            } else {
                None
            });
        frame.render_stateful_widget(list, chunks[5], &mut state);
    }

    // Separator
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  \u{2014} or \u{2014}",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );

    // Quick description input
    let is_quick_selected = app.source_selected >= app.design_docs.len();
    let (marker, input_style) = if is_quick_selected {
        (
            "\u{276f} ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("  ", Style::default().fg(Color::Gray))
    };

    let display_text = if app.quick_description.is_empty() {
        "Quick feature description: _".to_string()
    } else if app.editing_quick {
        format!("Quick feature description: {}_", app.quick_description)
    } else {
        format!("Quick feature description: {}", app.quick_description)
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(marker, input_style),
            Span::styled(display_text, input_style),
        ])),
        chunks[7],
    );

    // Help bar
    let help = if app.editing_quick {
        "Type description  Enter confirm  Esc cancel"
    } else {
        "\u{2191}\u{2193} navigate  Enter select  Esc cancel"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[9],
    );
}

fn draw_stage_screen(frame: &mut Frame, app: &WizardApp, area: Rect) {
    let block = outer_block();
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // progress
        Constraint::Length(1), // spacer
        Constraint::Length(1), // source summary
        Constraint::Length(1), // spacer
        Constraint::Length(1), // title
        Constraint::Length(1), // spacer
        Constraint::Length(3), // plan option
        Constraint::Length(1), // spacer
        Constraint::Length(3), // run option
        Constraint::Min(1),    // fill
        Constraint::Length(1), // help
    ])
    .split(inner);

    // Progress
    frame.render_widget(Paragraph::new(progress_line(Screen::Stage)), chunks[0]);

    // Source summary
    let source_text = match &app.source {
        Some(WizardSource::DesignDoc(path)) => {
            format!(
                "\u{2713} Source: {}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
            )
        }
        Some(WizardSource::QuickDescription(desc)) => {
            format!("\u{2713} Source: \"{}\"", truncate(desc, 50))
        }
        None => "Source: (none)".to_string(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            source_text,
            Style::default().fg(Color::Green),
        ))),
        chunks[2],
    );

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Select stage",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[4],
    );

    // Plan option
    let plan_selected = app.stage_selected == 0;
    let plan_style = if plan_selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let plan_marker = if plan_selected { "\u{276f} " } else { "  " };

    // Get plan status from pipeline
    let plan_status = get_plan_status_text(app);

    let plan_lines = vec![
        Line::from(vec![
            Span::styled(plan_marker, plan_style),
            Span::styled("Plan", plan_style),
            Span::styled(
                "  \u{2014} Gap analysis (read-only)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::raw("    "),
            Span::styled(plan_status, Style::default().fg(Color::DarkGray)),
        ]),
    ];
    frame.render_widget(Paragraph::new(plan_lines), chunks[6]);

    // Run option
    let run_selected = app.stage_selected == 1;
    let run_style = if run_selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let run_marker = if run_selected { "\u{276f} " } else { "  " };

    let run_status = get_run_status_text(app);

    let run_lines = vec![
        Line::from(vec![
            Span::styled(run_marker, run_style),
            Span::styled("Run", run_style),
            Span::styled(
                "  \u{2014} Implementation",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::raw("    "),
            Span::styled(run_status, Style::default().fg(Color::DarkGray)),
        ]),
    ];
    frame.render_widget(Paragraph::new(run_lines), chunks[8]);

    // Help
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} select  Enter confirm  Backspace back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[10],
    );
}

fn get_plan_status_text(app: &WizardApp) -> String {
    if let Some(WizardSource::DesignDoc(ref path)) = app.source {
        if let Some(entry) = app.design_docs.iter().find(|e| e.path == *path) {
            if let Some(ref pipeline) = entry.pipeline {
                if let Some(plan) = pipeline.plans.last() {
                    let stale = if pipeline::is_plan_stale(pipeline, path) {
                        " (stale \u{26a0})"
                    } else {
                        ""
                    };
                    return format!(
                        "Status: {} — {} blocking, {} advisory{}",
                        plan.status, plan.blocking_gaps, plan.advisory_gaps, stale
                    );
                }
            }
        }
        "Status: not yet run".to_string()
    } else {
        "Status: not applicable (no design doc)".to_string()
    }
}

fn get_run_status_text(app: &WizardApp) -> String {
    if let Some(WizardSource::DesignDoc(ref path)) = app.source {
        if let Some(entry) = app.design_docs.iter().find(|e| e.path == *path) {
            if let Some(ref pipeline) = entry.pipeline {
                if pipeline.stage == "planned" {
                    let stale = if pipeline::is_plan_stale(pipeline, path) {
                        "Plan: \u{26a0} stale (doc modified) \u{2014} re-plan recommended"
                    } else if let Some(plan) = pipeline.plans.last() {
                        if plan.blocking_gaps > 0 {
                            "Plan: has blocking gaps \u{2014} resolve before running"
                        } else {
                            "Plan: \u{2713} ready (0 blocking gaps)"
                        }
                    } else {
                        "Plan: \u{2713} ready"
                    };
                    return stale.to_string();
                }
                if !pipeline.runs.is_empty() {
                    if let Some(run) = pipeline.runs.last() {
                        return format!("Last run: {} ({})", run.status, run.agent_id);
                    }
                }
            }
        }
        "Status: not started".to_string()
    } else {
        "Ready to launch".to_string()
    }
}

fn draw_configure_screen(frame: &mut Frame, app: &WizardApp, area: Rect) {
    let block = outer_block();
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let opt_count = app.config_options.len() as u16;
    let chunks = Layout::vertical([
        Constraint::Length(1),             // progress
        Constraint::Length(1),             // spacer
        Constraint::Length(2),             // source + stage summary
        Constraint::Length(1),             // spacer
        Constraint::Length(1),             // title
        Constraint::Length(1),             // spacer
        Constraint::Length(opt_count * 2), // config options (2 lines each)
        Constraint::Min(1),                // fill
        Constraint::Length(1),             // help
    ])
    .split(inner);

    // Progress
    frame.render_widget(Paragraph::new(progress_line(Screen::Configure)), chunks[0]);

    // Summaries
    let source_name = match &app.source {
        Some(WizardSource::DesignDoc(p)) => p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string(),
        Some(WizardSource::QuickDescription(d)) => format!("\"{}\"", truncate(d, 40)),
        None => "(none)".to_string(),
    };
    let stage_name = match app.stage {
        Some(WizardStage::Plan) => "Plan (gap analysis)",
        Some(WizardStage::Run) => "Run (implementation)",
        None => "(none)",
    };
    let summary_lines = vec![
        Line::from(vec![
            Span::styled("\u{2713} Source: ", Style::default().fg(Color::Green)),
            Span::styled(source_name, Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::styled("\u{2713} Stage:  ", Style::default().fg(Color::Green)),
            Span::styled(stage_name, Style::default().fg(Color::Green)),
        ]),
    ];
    frame.render_widget(Paragraph::new(summary_lines), chunks[2]);

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Configure",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[4],
    );

    // Config options
    let mut option_lines: Vec<Line> = Vec::new();
    for (i, opt) in app.config_options.iter().enumerate() {
        let is_selected = i == app.config_selected;
        let label_style = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let marker = if is_selected { "\u{276f} " } else { "  " };

        // Label line
        option_lines.push(Line::from(vec![
            Span::styled(marker, label_style),
            Span::styled(opt.label, label_style),
        ]));

        // Values line
        let value_spans: Vec<Span> = opt
            .values
            .iter()
            .enumerate()
            .flat_map(|(j, val)| {
                let mut spans = Vec::new();
                if j > 0 {
                    spans.push(Span::raw("  "));
                }
                let style = if j == opt.selected && is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if j == opt.selected {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let prefix = if j == opt.selected {
                    "[\u{2713}] "
                } else {
                    "[ ] "
                };
                spans.push(Span::styled(format!("{prefix}{val}"), style));
                spans
            })
            .collect();

        let mut line_spans = vec![Span::raw("    ")];
        line_spans.extend(value_spans);
        option_lines.push(Line::from(line_spans));
    }
    frame.render_widget(Paragraph::new(option_lines), chunks[6]);

    // Help
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} navigate  \u{2190}\u{2192} change value  Enter confirm  Backspace back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[8],
    );
}

fn draw_launch_screen(frame: &mut Frame, app: &WizardApp, area: Rect) {
    let block = outer_block();
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // progress
        Constraint::Length(1), // spacer
        Constraint::Length(1), // title
        Constraint::Length(1), // spacer
        Constraint::Length(5), // summary
        Constraint::Min(1),    // fill
        Constraint::Length(1), // help
    ])
    .split(inner);

    // Progress (all green)
    let all_green: Vec<Span> = ["Source", "Stage", "Configure", "Launch"]
        .iter()
        .enumerate()
        .flat_map(|(i, name)| {
            let mut parts = Vec::new();
            if i > 0 {
                parts.push(Span::styled("  ", Style::default()));
            }
            parts.push(Span::styled("\u{25cf} ", Style::default().fg(Color::Green)));
            parts.push(Span::styled(*name, Style::default().fg(Color::Green)));
            parts
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(all_green)), chunks[0]);

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Ready to launch?",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    // Summary
    let source_name = match &app.source {
        Some(WizardSource::DesignDoc(p)) => p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string(),
        Some(WizardSource::QuickDescription(d)) => format!("\"{}\"", truncate(d, 40)),
        None => "(none)".to_string(),
    };
    let stage_name = match app.stage {
        Some(WizardStage::Plan) => "Plan (gap analysis)",
        Some(WizardStage::Run) => "Run (implementation)",
        None => "(none)",
    };
    let config_summary = match app.stage {
        Some(WizardStage::Plan) => {
            format!(
                "model={}, timeout={}",
                app.plan_config.model, app.plan_config.timeout
            )
        }
        Some(WizardStage::Run) => {
            format!(
                "verify={}, model={}, timeout={}, container={}",
                app.run_config.verify,
                app.run_config.model,
                app.run_config.timeout,
                app.run_config.container
            )
        }
        None => String::new(),
    };

    let summary_lines = vec![
        Line::from(vec![
            Span::styled("  \u{2713} Source:  ", Style::default().fg(Color::Green)),
            Span::styled(
                source_name,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  \u{2713} Stage:   ", Style::default().fg(Color::Green)),
            Span::styled(
                stage_name,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  \u{2713} Config:  ", Style::default().fg(Color::Green)),
            Span::styled(
                config_summary,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(summary_lines), chunks[4]);

    // Help
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter confirm  Backspace go back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

// ── Event loop ──────────────────────────────────────────────────────────────

/// Launch the interactive wizard and return user choices.
///
/// Returns `None` if the user cancels.
pub fn launch_wizard(crosslink_dir: &Path) -> Result<Option<WizardChoices>> {
    use ratatui::TerminalOptions;
    use ratatui::Viewport;

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        anyhow::bail!(
            "Non-interactive environment. Use: crosslink kickoff .design/<slug>.md --plan|--run"
        );
    }

    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    // Discover design docs and their pipeline state
    let design_docs = build_design_doc_entries(root, crosslink_dir);

    let mut app = WizardApp::new(design_docs);

    // Inline viewport (20 lines below cursor)
    const WIZARD_HEIGHT: u16 = 22;
    enable_raw_mode().context("Failed to enable raw mode")?;
    let stdout = std::io::stdout();
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(WIZARD_HEIGHT),
        },
    )
    .context("Failed to create terminal")?;

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| draw_wizard(f, &app))?;

            if let Event::Key(key) = event::read().context("Failed to read terminal event")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Handle text editing mode for quick description
                if app.editing_quick {
                    match key.code {
                        KeyCode::Enter => {
                            app.editing_quick = false;
                            app.confirm_source();
                        }
                        KeyCode::Esc => {
                            app.editing_quick = false;
                        }
                        KeyCode::Backspace => {
                            app.quick_description.pop();
                        }
                        KeyCode::Char(c) => {
                            app.quick_description.push(c);
                        }
                        _ => {}
                    }
                    continue;
                }

                match app.screen {
                    Screen::Source => match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.source_selected > 0 {
                                app.source_selected -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if app.source_selected < app.total_source_items() - 1 {
                                app.source_selected += 1;
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            if app.source_selected >= app.design_docs.len() {
                                // Quick description — enter edit mode
                                app.editing_quick = true;
                            } else {
                                app.confirm_source();
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('q') => {
                            app.cancelled = true;
                            break;
                        }
                        _ => {}
                    },
                    Screen::Stage => match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.stage_selected > 0 {
                                app.stage_selected -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if app.stage_selected < 1 {
                                app.stage_selected += 1;
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            app.confirm_stage();
                        }
                        KeyCode::Backspace | KeyCode::Left => {
                            app.go_back();
                        }
                        KeyCode::Esc | KeyCode::Char('q') => {
                            app.cancelled = true;
                            break;
                        }
                        _ => {}
                    },
                    Screen::Configure => match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.config_selected > 0 {
                                app.config_selected -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if app.config_selected < app.config_options.len().saturating_sub(1) {
                                app.config_selected += 1;
                            }
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            if let Some(opt) = app.config_options.get_mut(app.config_selected) {
                                if opt.selected > 0 {
                                    opt.selected -= 1;
                                }
                            }
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            if let Some(opt) = app.config_options.get_mut(app.config_selected) {
                                if opt.selected < opt.values.len() - 1 {
                                    opt.selected += 1;
                                }
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            app.confirm_config();
                        }
                        KeyCode::Backspace => {
                            app.go_back();
                        }
                        KeyCode::Esc | KeyCode::Char('q') => {
                            app.cancelled = true;
                            break;
                        }
                        _ => {}
                    },
                    Screen::Launch => match key.code {
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            app.confirm_launch();
                            break;
                        }
                        KeyCode::Backspace | KeyCode::Left => {
                            app.go_back();
                        }
                        KeyCode::Esc | KeyCode::Char('q') => {
                            app.cancelled = true;
                            break;
                        }
                        _ => {}
                    },
                }
            }
        }
        Ok(())
    })();

    // Clear the inline viewport
    {
        let area = terminal.get_frame().area();
        let backend = terminal.backend_mut();
        for row in area.y..area.y + area.height {
            crossterm::execute!(
                backend,
                crossterm::cursor::MoveTo(0, row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
            )
            .ok();
        }
        crossterm::execute!(backend, crossterm::cursor::MoveTo(0, area.y)).ok();
    }

    // Restore terminal
    disable_raw_mode().ok();
    terminal.show_cursor().ok();

    result?;

    if app.cancelled {
        return Ok(None);
    }

    Ok(app.into_choices())
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn build_design_doc_entries(repo_root: &Path, _crosslink_dir: &Path) -> Vec<DesignDocEntry> {
    let docs = pipeline::scan_design_docs(repo_root);

    docs.into_iter()
        .map(|path| {
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            let pipeline = pipeline::read_pipeline_state(&path);

            let stage_display = pipeline.as_ref().map_or_else(
                || "\u{2014}".to_string(),
                |p| pipeline::stage_display(p, &path),
            );

            let gaps_display = pipeline.as_ref().map_or_else(
                || "\u{2014}".to_string(),
                |p| {
                    p.plans.last().map_or_else(
                        || "\u{2014}".to_string(),
                        |plan| {
                            if plan.status == "done" {
                                format!("{} blocking", plan.blocking_gaps)
                            } else {
                                String::new()
                            }
                        },
                    )
                },
            );

            DesignDocEntry {
                path,
                filename,
                pipeline,
                stage_display,
                gaps_display,
            }
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{truncated}...")
    } else {
        s.to_string()
    }
}
