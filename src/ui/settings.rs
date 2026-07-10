use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::App;
use crate::config;

/// Modal overlay drawn on top of whatever view is active.
pub fn render(f: &mut Frame, app: &App) {
    let s = &app.settings;
    let area = centered(f.area(), 74, if s.first_run { 22 } else { 17 });
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
            Span::styled("https://alphai.io", Style::new().fg(Color::Cyan)),
            Span::raw(" (Account -> API keys) and paste it below."),
        ]));
        lines.push(Line::from(""));
    }

    let rows: [(&str, String, String); 6] = [
        (
            "Price source",
            format!("‹ {} ›", s.source_choice),
            source_hint(s.source_choice.as_str()),
        ),
        (
            "Finnhub key",
            field_value(s, 1, &s.finnhub_key),
            env_hint("FINNHUB_API_KEY"),
        ),
        (
            "AlphaAI key",
            field_value(s, 2, &s.alphai_key),
            env_hint("ALPHAI_API_KEY"),
        ),
        (
            "Alpaca key ID",
            field_value(s, 3, &s.alpaca_key_id),
            env_hint("APCA_API_KEY_ID"),
        ),
        (
            "Alpaca secret",
            field_value(s, 4, &s.alpaca_secret),
            env_hint("APCA_API_SECRET_KEY"),
        ),
        (
            "News opens",
            format!("‹ {} ›", s.news_open_choice),
            news_open_hint(s.news_open_choice.as_str()),
        ),
    ];
    for (i, (label, value, hint)) in rows.into_iter().enumerate() {
        let selected = s.cursor == i;
        let editing = selected && s.editing;
        let marker = if selected { "▶ " } else { "  " };
        let value_style = if editing {
            Style::new().fg(Color::Yellow)
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
    let save_style = if s.cursor == 6 {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    lines.push(Line::from(vec![
        Span::raw(if s.cursor == 6 { "▶ " } else { "  " }),
        Span::styled("[ Save and close ]", save_style),
    ]));

    lines.push(Line::from(""));
    if let Some(msg) = &s.message {
        lines.push(Line::from(Span::styled(
            format!("  {msg}"),
            Style::new().fg(Color::Red),
        )));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(Line::from("  ↑↓ move · enter edit / toggle / save · esc close").dim());
    if let Some(p) = config::path() {
        lines.push(Line::from(format!("  config: {}", tilde(&p.display().to_string()))).dim());
    }

    let block = Block::bordered()
        .title(" Settings ")
        .border_style(Style::new().fg(Color::Cyan));
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
    match choice {
        "finnhub" => "real-time quotes, needs a key (free at finnhub.io)".into(),
        "alpaca" => "realtime IEX quotes and real candle history, free keys at alpaca.markets".into(),
        _ => "no key needed, ~15 min delayed".into(),
    }
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

fn centered(r: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(r.width.saturating_sub(2));
    let h = height.min(r.height.saturating_sub(2));
    Rect {
        x: r.x + (r.width.saturating_sub(w)) / 2,
        y: r.y + (r.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
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
