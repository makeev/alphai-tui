use ratatui::Frame;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::{App, SettingsRow, settings_rows};
use crate::source::registry;
use crate::ui::centered;

/// Modal overlay drawn on top of whatever view is active.
pub fn render(f: &mut Frame, app: &App) {
    let s = &app.settings;
    let rows = settings_rows();
    // Height scales with the registry (rows plus the fixed chrome: borders,
    // blank lines, message, help and config-path footer), so a new source's
    // key rows never clip.
    let height = rows.len() as u16 + 10 + if s.first_run { 5 } else { 0 };
    let area = centered(f.area(), 74, height);
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    if s.first_run {
        lines.push(Line::from(" Welcome to alphai-tui.").bold());
        lines.push(Line::from(" Prices work out of the box via Yahoo, no key needed."));
        lines.push(Line::from(
            " The News and Insider views use the AlphaAI API: get a free key at",
        ));
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled("https://alphai.io", Style::new().fg(app.theme.accent)),
            Span::raw(" (Account -> API keys) and paste it below."),
        ]));
        lines.push(Line::from(""));
    }

    for (i, row) in rows.iter().enumerate() {
        let selected = s.cursor == i;
        let marker = if selected { "▶ " } else { "  " };
        if let SettingsRow::Save = row {
            lines.push(Line::from(""));
            let save_style = if selected {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            lines.push(Line::from(vec![
                Span::raw(marker),
                Span::styled("[ Save and close ]", save_style),
            ]));
            continue;
        }
        let (label, value, hint) = match row {
            SettingsRow::SourceChoice => (
                "Price source",
                format!("‹ {} ›", s.source_choice),
                source_hint(s.source_choice.as_str()),
            ),
            SettingsRow::Key(field) => (
                field.label,
                field_value(s, i, s.key_values.get(field.config_name).map_or("", String::as_str)),
                env_hint(field.env_var),
            ),
            SettingsRow::NewsOpen => (
                "News opens",
                format!("‹ {} ›", s.news_open_choice),
                news_open_hint(s.news_open_choice.as_str()),
            ),
            SettingsRow::Save => unreachable!(),
        };
        let editing = selected && s.editing;
        let value_style = if editing {
            Style::new().fg(app.theme.warn)
        } else if selected {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::styled(format!("{label:<14}"), Style::new().bold()),
            Span::styled(value, value_style),
            Span::raw(" "),
            Span::raw(hint).dim(),
        ]));
    }

    lines.push(Line::from(""));
    if let Some(msg) = &s.message {
        lines.push(Line::from(Span::styled(
            format!("  {msg}"),
            Style::new().fg(app.theme.error),
        )));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(Line::from("  ↑↓ move · enter edit / toggle / save · esc close").dim());
    if let Some(p) = &app.config_path {
        lines.push(Line::from(format!("  config: {}", tilde(&p.display().to_string()))).dim());
    }

    let block = Block::bordered()
        .title(" Settings ")
        .border_style(Style::new().fg(app.theme.accent));
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(block),
        area,
    );
}

/// While editing show the raw buffer with a cursor mark; otherwise mask.
fn field_value(s: &crate::app::SettingsState, row: usize, stored: &str) -> String {
    if s.cursor == row && s.editing {
        format!("{}▏", s.input)
    } else {
        mask(stored)
    }
}

pub fn mask(key: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        return "(not set)".into();
    }
    if key.chars().count() <= 10 {
        return "••••••".into();
    }
    let start: String = key.chars().take(6).collect();
    let end: String = key.chars().skip(key.chars().count() - 4).collect();
    format!("{start}…{end}")
}

fn source_hint(choice: &str) -> String {
    registry::find(choice)
        .unwrap_or(&registry::SOURCES[0])
        .hint
        .to_string()
}

fn news_open_hint(choice: &str) -> String {
    match choice {
        "original" => "enter opens the source site".into(),
        _ => "enter opens the article page on alphai.io".into(),
    }
}

fn env_hint(var: &str) -> String {
    if std::env::var(var).is_ok_and(|v| !v.trim().is_empty()) {
        format!("(env {var} overrides this)")
    } else {
        String::new()
    }
}

fn tilde(path: &str) -> String {
    match dirs::home_dir() {
        Some(h) => path.replacen(&h.display().to_string(), "~", 1),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::mask;

    #[test]
    fn masks_keys() {
        assert_eq!(mask(""), "(not set)");
        assert_eq!(mask("short"), "••••••");
        assert_eq!(mask("ak_live_abcdefgh1234"), "ak_liv…1234");
    }
}
