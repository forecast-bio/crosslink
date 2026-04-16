//! TUI walkthrough for `crosslink init` — registry-driven interactive setup.

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, disable_raw_mode, enable_raw_mode};
use crossterm::{cursor, execute};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph},
    Frame, TerminalOptions, Viewport,
};
use std::fs;
use std::io::{self, IsTerminal};

use super::{InitUI, TuiChoices, HOOK_CONFIG_JSON};
use crate::commands::config_registry::{WalkthroughCore, REGISTRY};

// ── Ratatui TUI walkthrough (registry-driven, REQ-5) ────────────────────────

struct InitWalkthroughApp {
    core: WalkthroughCore,
    /// Shell alias question
    alias_selected: usize, // 0=Yes, 1=No
    shell_config_file: String,
}

impl InitWalkthroughApp {
    fn new(existing: &serde_json::Value) -> Self {
        let core = WalkthroughCore::new(existing, 1); // 1 extra screen: alias

        // Detect shell for alias question
        let shell_env = std::env::var("SHELL").unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let (shell_name, shell_config_file) = if shell_env.ends_with("fish") {
            (
                "fish".to_string(),
                format!("{home}/.config/fish/config.fish"),
            )
        } else if shell_env.ends_with("zsh") {
            ("zsh".to_string(), format!("{home}/.zshrc"))
        } else if shell_env.ends_with("bash") {
            ("bash".to_string(), format!("{home}/.bashrc"))
        } else {
            ("unknown".to_string(), String::new())
        };

        // Check if alias already installed
        let alias_already = if shell_config_file.is_empty() {
            false
        } else {
            let alias_line = if shell_name == "fish" {
                "abbr -a xl crosslink"
            } else {
                "alias xl='crosslink'"
            };
            fs::read_to_string(&shell_config_file)
                .is_ok_and(|c| c.lines().any(|l| l.trim() == alias_line))
        };

        Self {
            core,
            alias_selected: usize::from(alias_already),
            shell_config_file,
        }
    }

    const fn is_alias_screen(&self) -> bool {
        self.core.extra_screen_idx().is_some()
    }

    const fn move_up(&mut self) {
        if self.is_alias_screen() {
            self.alias_selected = self.alias_selected.saturating_sub(1);
        } else {
            self.core.move_up();
        }
    }

    fn move_down(&mut self) {
        if self.is_alias_screen() {
            if self.alias_selected < 1 {
                self.alias_selected += 1;
            }
        } else {
            self.core.move_down();
        }
    }

    fn build_choices(&self) -> TuiChoices {
        TuiChoices {
            values: self.core.build_config(),
            install_alias: self.alias_selected == 0,
        }
    }
}

fn draw_init_walkthrough(frame: &mut Frame, app: &InitWalkthroughApp) {
    let area = frame.area();

    let outer = Block::default()
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
            Span::styled(" setup ", Style::default().fg(Color::DarkGray)),
        ]))
        .padding(Padding::new(2, 2, 1, 1));

    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Progress dots
    let total = app.core.total_screens();
    let progress_spans: Vec<Span> = (0..total)
        .map(|i| match i.cmp(&app.core.screen) {
            std::cmp::Ordering::Less => {
                Span::styled(" \u{25cf} ", Style::default().fg(Color::Green))
            }
            std::cmp::Ordering::Equal => Span::styled(
                " \u{25cf} ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            std::cmp::Ordering::Greater => {
                Span::styled(" \u{25cb} ", Style::default().fg(Color::DarkGray))
            }
        })
        .collect();

    if app.core.is_preset_screen() {
        draw_init_preset(frame, app, inner, progress_spans);
    } else if app.is_alias_screen() {
        draw_init_alias(frame, app, inner, progress_spans);
    } else if app.core.is_confirm_screen() {
        draw_init_confirm(frame, app, inner, progress_spans);
    } else if let Some(gi) = app.core.current_group_idx() {
        draw_init_group(frame, app, gi, inner, progress_spans);
    }
}

fn draw_init_preset(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Quick-start presets",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Choose a preset or configure each setting individually",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[3],
    );

    let presets = [
        ("Team", "strict tracking, CI verification, signing enforced"),
        ("Solo", "relaxed tracking, local verification, no signing"),
        ("Custom", "configure each setting individually"),
    ];
    let items: Vec<ListItem> = presets
        .iter()
        .enumerate()
        .map(|(i, (label, desc))| {
            let (marker, style) = if i == app.core.preset_selected {
                (
                    "\u{276f} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(*label, style),
                Span::raw("  "),
                Span::styled(*desc, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items);
    let mut state = ListState::default().with_selected(Some(app.core.preset_selected));
    frame.render_stateful_widget(list, chunks[5], &mut state);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} select  Enter confirm  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

fn draw_init_group(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    group_idx: usize,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let keys = &app.core.group_keys[group_idx];
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(keys.len() as u16 + 1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            app.core.group_names[group_idx],
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    let items: Vec<ListItem> = keys
        .iter()
        .enumerate()
        .map(|(ki, &reg_idx)| {
            let entry = &REGISTRY[reg_idx];
            let options = WalkthroughCore::options_for_key(reg_idx);
            let selected = app.core.group_selections[group_idx][ki];
            let val_str = if selected < options.len() {
                options[selected]
            } else {
                "?"
            };
            let (marker, style) = if ki == app.core.group_cursor {
                (
                    "\u{276f} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(format!("{:<30}", entry.key), style),
                Span::styled(
                    format!("[{val_str}]"),
                    if ki == app.core.group_cursor {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
            ]))
        })
        .collect();
    let list = List::new(items);
    let mut state = ListState::default().with_selected(Some(app.core.group_cursor));
    frame.render_stateful_widget(list, chunks[4], &mut state);

    // Description pane
    if app.core.group_cursor < keys.len() {
        let reg_idx = keys[app.core.group_cursor];
        let entry = &REGISTRY[reg_idx];
        let options = WalkthroughCore::options_for_key(reg_idx);
        let valid = if options.len() > 1 {
            format!("  Valid: {}", options.join(", "))
        } else {
            String::new()
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("  {}", entry.description),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(valid, Style::default().fg(Color::DarkGray))),
            ]),
            chunks[5],
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} navigate  \u{2192}/\u{2190} cycle  Enter next  Backspace back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

fn draw_init_alias(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Shell Alias",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    let desc = if app.shell_config_file.is_empty() {
        "Could not detect shell config file".to_string()
    } else {
        format!(
            "Install `xl` alias for `crosslink` in {}?",
            app.shell_config_file
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            desc,
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[3],
    );

    let options = [
        ("Yes", "Add alias to shell config"),
        ("No", "Skip alias setup"),
    ];
    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, (label, desc))| {
            let (marker, style) = if i == app.alias_selected {
                (
                    "\u{276f} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(*label, style),
                Span::raw("  "),
                Span::styled(*desc, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items);
    let mut state = ListState::default().with_selected(Some(app.alias_selected));
    frame.render_stateful_widget(list, chunks[5], &mut state);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} select  Enter confirm  Backspace back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

fn draw_init_confirm(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let total_keys: usize = app.core.group_keys.iter().map(Vec::len).sum();
    let summary_height = total_keys as u16 + app.core.group_names.len() as u16 + 3;

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(summary_height),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Review your choices",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    let mut lines: Vec<Line> = Vec::new();
    for (gi, keys) in app.core.group_keys.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!("  {}", app.core.group_names[gi]),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for (ki, &reg_idx) in keys.iter().enumerate() {
            let entry = &REGISTRY[reg_idx];
            let options = WalkthroughCore::options_for_key(reg_idx);
            let selected = app.core.group_selections[gi][ki];
            let val_str = if selected < options.len() {
                options[selected]
            } else {
                "?"
            };
            lines.push(Line::from(vec![
                Span::styled("    \u{2713} ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{}: ", entry.key),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    val_str,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    let alias_text = if app.alias_selected == 0 {
        format!(
            "    \u{2713} xl alias: will install ({})",
            app.shell_config_file
        )
    } else {
        "    \u{2713} xl alias: skip".to_string()
    };
    lines.push(Line::from(Span::styled(
        alias_text,
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines), chunks[4]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter apply  Backspace go back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[5],
    );
}

/// Run the interactive TUI walkthrough using ratatui, returning user choices.
/// Falls back to defaults if stdin is not a TTY.
pub(super) fn run_tui_walkthrough(existing: Option<&serde_json::Value>) -> Result<TuiChoices> {
    if !std::io::stdin().is_terminal() {
        println!("Non-interactive environment detected, using defaults.");
        return Ok(TuiChoices::default());
    }

    let base = existing
        .cloned()
        .unwrap_or_else(|| serde_json::from_str(HOOK_CONFIG_JSON).unwrap_or_default());

    let mut app = InitWalkthroughApp::new(&base);

    const WALKTHROUGH_HEIGHT: u16 = 24;
    enable_raw_mode().context("Failed to enable raw mode")?;
    let stdout = io::stdout();
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(WALKTHROUGH_HEIGHT),
        },
    )
    .context("Failed to create terminal")?;

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| draw_init_walkthrough(f, &app))?;

            if let Event::Key(key) = event::read().context("Failed to read terminal event")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') if !app.core.is_confirm_screen() => {
                        app.move_up();
                    }
                    KeyCode::Down | KeyCode::Char('j') if !app.core.is_confirm_screen() => {
                        app.move_down();
                    }
                    KeyCode::Right
                        if !app.core.is_preset_screen()
                            && !app.core.is_confirm_screen()
                            && !app.is_alias_screen() =>
                    {
                        app.core.cycle_value();
                    }
                    KeyCode::Left
                        if !app.core.is_preset_screen()
                            && !app.core.is_confirm_screen()
                            && !app.is_alias_screen() =>
                    {
                        // Cycle backwards
                        if let Some(gi) = app.core.current_group_idx() {
                            if app.core.group_cursor < app.core.group_keys[gi].len() {
                                let reg_idx = app.core.group_keys[gi][app.core.group_cursor];
                                let options = WalkthroughCore::options_for_key(reg_idx);
                                if !options.is_empty() {
                                    let current =
                                        app.core.group_selections[gi][app.core.group_cursor];
                                    app.core.group_selections[gi][app.core.group_cursor] =
                                        if current == 0 {
                                            options.len() - 1
                                        } else {
                                            current - 1
                                        };
                                }
                            }
                        }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if !app.core.is_preset_screen()
                            && !app.core.is_confirm_screen()
                            && !app.is_alias_screen()
                        {
                            // On group screens, Enter advances to next key or next screen
                            if let Some(gi) = app.core.current_group_idx() {
                                if app.core.group_cursor + 1 < app.core.group_keys[gi].len() {
                                    app.core.group_cursor += 1;
                                } else {
                                    app.core.screen += 1;
                                    app.core.group_cursor = 0;
                                }
                            }
                        } else {
                            app.core.confirm();
                        }
                        if app.core.finished {
                            break;
                        }
                    }
                    KeyCode::Tab if !app.core.is_confirm_screen() => {
                        if app.core.is_preset_screen() {
                            app.core.confirm();
                        } else {
                            app.core.screen += 1;
                            app.core.group_cursor = 0;
                        }
                    }
                    KeyCode::Backspace => app.core.go_back(),
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.core.cancelled = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    // Clear inline viewport
    {
        let area = terminal.get_frame().area();
        let backend = terminal.backend_mut();
        for row in area.y..area.y + area.height {
            execute!(
                backend,
                cursor::MoveTo(0, row),
                terminal::Clear(terminal::ClearType::CurrentLine)
            )
            .ok();
        }
        execute!(backend, cursor::MoveTo(0, area.y)).ok();
    }

    disable_raw_mode().ok();
    terminal.show_cursor().ok();

    result?;

    if app.core.cancelled {
        anyhow::bail!("Setup cancelled");
    }

    Ok(app.build_choices())
}

/// Apply TUI choices onto a config JSON value, preserving fields not covered by the TUI.
pub(super) fn apply_tui_choices(
    config: &mut serde_json::Value,
    choices: &TuiChoices,
) -> Result<()> {
    let obj = config
        .as_object_mut()
        .context("hook-config.json is not a JSON object")?;
    for (k, v) in &choices.values {
        obj.insert(k.clone(), v.clone());
    }
    Ok(())
}

/// Install the `xl` shell alias if requested by the user during init.
pub(super) fn setup_shell_alias(ui: &InitUI, choices: &TuiChoices) {
    if !choices.install_alias {
        return;
    }

    let shell_env = std::env::var("SHELL").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();

    let (config_file, alias_line) = if shell_env.ends_with("fish") {
        (
            format!("{home}/.config/fish/config.fish"),
            "abbr -a xl crosslink",
        )
    } else if shell_env.ends_with("zsh") {
        (format!("{home}/.zshrc"), "alias xl='crosslink'")
    } else if shell_env.ends_with("bash") {
        (format!("{home}/.bashrc"), "alias xl='crosslink'")
    } else {
        ui.warn("Could not detect shell for alias installation");
        return;
    };

    let path = std::path::Path::new(&config_file);

    // Idempotent: check if already present
    if let Ok(content) = fs::read_to_string(path) {
        if content.lines().any(|line| line.trim() == alias_line) {
            ui.step_skip("xl alias already installed");
            return;
        }
    }

    // Append the alias
    ui.step_start("Installing xl alias");
    let line_to_append = format!("\n# crosslink shortcut\n{alias_line}\n");
    match fs::OpenOptions::new().append(true).open(path) {
        Ok(mut file) => {
            use std::io::Write;
            if let Err(e) = file.write_all(line_to_append.as_bytes()) {
                ui.warn(&format!("Failed to write alias: {e}"));
            } else {
                ui.step_ok(Some(&config_file));
                ui.detail(&format!(
                    "Run `source {config_file}` or open a new terminal to activate"
                ));
            }
        }
        Err(e) => {
            ui.warn(&format!("Could not open {config_file}: {e}"));
        }
    }
}
