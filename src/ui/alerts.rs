//! Alert presentation shares the existing view geometry and modal input.
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::hits::Modal;
use super::{centered, ellipsize, time_axis};
use crate::alerts::{Level, Rule, Sample};
use crate::app::App;
use crate::market;

pub fn style(app: &App, rule: &Rule) -> Style {
    if rule.fired.is_some() {
        app.theme.subtle()
    } else {
        Style::new().fg(app.theme.warn)
    }
}

pub fn symbol(app: &App, symbol: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(symbol.to_string(), Style::new().bold())];
    if let Some(rule) = app.alerts.get(symbol) {
        let mark_style = if rule.fired.is_some() {
            Style::new().fg(app.theme.warn).bold()
        } else {
            style(app, rule)
        };
        spans.push(Span::styled(format!(" {}", rule.marker()), mark_style));
    }
    Line::from(spans)
}

pub fn rail_zone(app: &App, symbol: &str) -> Vec<Vec<Span<'static>>> {
    let Some(rule) = app.alerts.get(symbol) else {
        return Vec::new();
    };
    let label = if rule.fired.is_some() {
        "alert fired".to_string()
    } else if rule.currency_paused {
        "alert paused".to_string()
    } else if rule.previous.is_none() {
        "alert waiting".to_string()
    } else {
        format!("alert {}", rule.level.label)
    };
    [label, format!("alert {}", rule.marker())]
        .into_iter()
        .map(|s| vec![Span::styled(format!("  {s}"), style(app, rule))])
        .collect()
}

fn stamp(app: &App, symbol: &str, timestamp: i64) -> String {
    let us = market::is_us_equity(symbol);
    format!(
        "{} {}",
        time_axis::label(timestamp, "%d %b %H:%M:%S", app.chart.timezone, us),
        time_axis::zone_name(app.chart.timezone, us)
    )
}

fn sample_text(app: &App, symbol: &str, sample: &Sample) -> String {
    let session = if market::is_us_equity(symbol) {
        match market::window_at(sample.timestamp).map(|w| w.session) {
            Some(market::Session::Pre) => "PRE",
            Some(market::Session::Post) => "AH",
            _ => "quote",
        }
    } else {
        "quote"
    };
    let currency = sample
        .currency
        .as_ref()
        .map(|c| format!(" {c}"))
        .unwrap_or_default();
    let feed = if sample.feed.label().is_empty() {
        "feed unknown"
    } else {
        sample.feed.label()
    };
    format!(
        "{}{currency} · {feed} · {session} {}",
        sample.price,
        stamp(app, symbol, sample.timestamp)
    )
}

pub fn render_banner(f: &mut Frame, app: &mut App) {
    let Some(notice) = app.alerts.notification() else {
        return;
    };
    let screen = f.area();
    if screen.is_empty() {
        return;
    }
    let area = Rect::new(screen.x, screen.bottom() - 1, screen.width, 1);
    let more = app.alerts.waiting_count();
    let queued = if more > 0 {
        format!(" (+{more})")
    } else {
        String::new()
    };
    let text = format!(
        " ALERT {} {}{queued} · {}",
        notice.symbol,
        notice.level,
        sample_text(app, &notice.symbol, &notice.trigger.sample)
    );
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(ellipsize(&text, area.width as usize))
            .style(Style::new().bg(app.theme.surface).fg(app.theme.warn).bold()),
        area,
    );
    // The overlay must not leave the hidden bottom row clickable in bare mode.
    app.hits.alert_banner = Some(area);
}

pub fn render_prompt(f: &mut Frame, app: &mut App) {
    let prompt = &app.prompt;
    let rule = app.alerts.get(&prompt.target);
    let sample = app.alert_sample(&prompt.target);
    let screen = f.area();
    let compact = screen.height < 6;
    let area = if compact {
        Rect {
            height: screen
                .height
                .saturating_sub(u16::from(app.alerts.notification().is_some())),
            ..screen
        }
    } else {
        let rows = 7
            + u16::from(prompt.error.is_some())
            + 2 * u16::from(rule.is_some_and(|r| r.fired.is_some()));
        centered(screen, 88, rows)
    };
    if area.is_empty() {
        return;
    }
    let block = app.theme.modal().title(
        app.theme
            .heading(format!(" Price alert · {} ", prompt.target)),
    );
    let inner = if compact { area } else { block.inner(area) };
    let budget = inner.height as usize;
    let width = inner.width as usize;

    let mut input = prompt.input.as_str();
    while input.width() > width.saturating_sub(2) {
        let Some(first) = input.graphemes(true).next() else {
            break;
        };
        input = &input[first.len()..];
    }
    let typed = Line::from(vec![
        Span::raw(" "),
        Span::styled(input.to_string(), Style::new().bold()),
        Span::styled("▏", Style::new().fg(app.theme.accent)),
    ]);
    let hint = if width >= 72 {
        " >= 150 / <= 140 · enter arm · ctrl-u, enter clear · esc cancel"
    } else if width >= 47 {
        " enter arm · empty clears · esc cancel"
    } else {
        " enter arm · esc cancel"
    };
    let mut detail = Vec::new();
    if let Some(error) = &prompt.error {
        detail.push(Line::styled(
            format!(" {error}"),
            Style::new().fg(app.theme.error),
        ));
    }
    let preview = Level::parse(&prompt.input)
        .ok()
        .map(|l| Rule::new(l, sample.clone()));
    let displayed = rule
        .filter(|r| r.level.label == prompt.input)
        .or(preview.as_ref())
        .or(rule);
    if prompt.input.trim().is_empty() && rule.is_some() {
        detail.push(Line::styled(
            " Empty input clears this alert on Enter",
            Style::new().fg(app.theme.warn),
        ));
    } else if let Some(rule) = displayed {
        detail.push(Line::styled(
            format!(" {}", rule.status()),
            Style::new().fg(app.theme.warn),
        ));
    } else {
        detail.push(Line::styled(
            " Reach a price from below (>=) or above (<=)",
            app.theme.subtle(),
        ));
    }
    detail.push(Line::from(match &sample {
        Some(sample) => format!(" Watching: {}", sample_text(app, &prompt.target, sample)),
        None => " Waiting for a timestamped quote; cached prices cannot arm it".into(),
    }));
    if let Some(fired) = rule.and_then(|r| r.fired.as_ref()) {
        detail.push(Line::from(format!(
            " Fired: {}",
            sample_text(app, &prompt.target, &fired.sample)
        )));
        detail.push(Line::styled(
            format!(
                " Detected: {}",
                stamp(app, &prompt.target, fired.detected_at.timestamp())
            ),
            app.theme.subtle(),
        ));
    }
    let mut lines = vec![typed];
    // Input and the action hint survive first. An error takes priority over
    // the session reminder when only one other row fits.
    let show_lifetime = budget >= 3 && (prompt.error.is_none() || budget >= 4);
    let details = budget.saturating_sub(2 + usize::from(show_lifetime));
    lines.extend(detail.into_iter().take(details));
    if show_lifetime {
        lines.push(Line::styled(
            " This session only · one signal until rearmed",
            app.theme.subtle(),
        ));
    }
    if budget >= 2 {
        lines.push(Line::styled(hint, app.theme.subtle()));
    }
    f.render_widget(Clear, area);
    let paragraph = Paragraph::new(lines).style(app.theme.fill());
    if compact {
        f.render_widget(paragraph, area);
    } else {
        f.render_widget(paragraph.block(block), area);
    }
    app.hits.modal = Some((area, Modal::Prompt));
}
