use crossterm::event::KeyEvent;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::TabAction;

/// A placeholder tab for features not yet implemented.
pub struct PlaceholderTab {
    title: String,
    phase: u8,
}

impl PlaceholderTab {
    pub fn new(title: &str, phase: u8) -> Self {
        PlaceholderTab {
            title: title.to_string(),
            phase,
        }
    }
}

impl super::Tab for PlaceholderTab {
    fn title(&self) -> &str {
        &self.title
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(""),
            Line::from(""),
            Line::from(Span::styled(
                format!("  {} Tab", self.title),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(format!("  Coming soon \u{2014} Phase {}", self.phase)),
            Line::from(""),
            Line::from(Span::styled(
                "  This tab will be implemented in a future update.",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabAction {
        match key.code {
            crossterm::event::KeyCode::Char('q') => TabAction::Quit,
            _ => TabAction::NotHandled,
        }
    }

    // Placeholder tabs have no state to initialize or tear down on focus changes.
    fn on_enter(&mut self) {}
    fn on_leave(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::super::Tab;
    use super::*;

    #[test]
    fn test_placeholder_title() {
        let tab = PlaceholderTab::new("Agents", 2);
        assert_eq!(tab.title(), "Agents");
    }

    #[test]
    fn test_placeholder_phase() {
        let tab = PlaceholderTab::new("Knowledge", 3);
        assert_eq!(tab.phase, 3);
    }

    #[test]
    fn test_placeholder_render_no_panic() {
        let tab = PlaceholderTab::new("Config", 5);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tab.render(frame, frame.area()))
            .unwrap();
    }

    #[test]
    fn test_placeholder_key_not_handled() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut tab = PlaceholderTab::new("Test", 1);
        let key = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert!(matches!(tab.handle_key(key), TabAction::NotHandled));
    }

    #[test]
    fn test_placeholder_key_quit() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut tab = PlaceholderTab::new("Test", 1);
        let key = KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert!(matches!(tab.handle_key(key), TabAction::Quit));
    }
}
