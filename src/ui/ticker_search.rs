//! Watchlist search uses the quotes already on screen, including their
//! cache/error labels. The active view stays untouched behind the modal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::domain::fmt_price;
use crate::ui::{centered, ellipsize, pad_right};

pub fn render(f: &mut Frame, app: &App) {
    let matches = app.ticker_matches();
    let cursor = app.ticker_search.cursor(&matches);
    let theme = &app.theme;
    let area = centered(f.area(), 84, app.symbols.len().clamp(1, 10) as u16 + 5);
    f.render_widget(Clear, area);
    let block = theme.modal().title(theme.heading(format!(
        " Find ticker · {}/{} ",
        matches.len(),
        app.symbols.len()
    )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [query, description, list, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(inner.height >= 4)),
        Constraint::Min(1),
        Constraint::Length(u16::from(inner.height >= 5)),
    ])
    .areas(inner);

    // Show the end of a long query, keeping graphemes and its cursor in
    // the box even after the terminal shrinks.
    let mut input = app.ticker_search.input.as_str();
    let input_width = query.width.saturating_sub(4) as usize;
    while input.width() > input_width {
        let Some(first) = input.graphemes(true).next() else {
            break;
        };
        input = &input[first.len()..];
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" / "),
            Span::styled(input, Style::new().bold()),
            Span::styled("▏", Style::new().fg(theme.accent)),
        ])),
        query,
    );
    f.render_widget(
        Paragraph::new(" Symbol or company name · regular quotes").style(theme.subtle()),
        description,
    );

    let visible = list.height.max(1) as usize;
    let skip = cursor.unwrap_or(0).saturating_sub(visible - 1);
    let rows: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(skip)
        .take(visible)
        .map(|(row, &idx)| {
            let symbol = &app.symbols[idx];
            let data = app.data.get(symbol);
            let status = if app.from_cache.contains_key(symbol) {
                "cached"
            } else if app.errors.contains_key(symbol) {
                if data.is_some() { "stale" } else { "error" }
            } else if data.is_none() {
                "loading"
            } else if data.is_some_and(|d| {
                d.quote.timing.regular_feed == crate::domain::PriceFeed::DelayedSip
            }) {
                "delayed"
            } else {
                ""
            };
            let selected = cursor == Some(row);
            let mut spans = vec![
                Span::raw(if selected { "▶ " } else { "  " }),
                Span::styled(pad_right(symbol, 10), Style::new().bold()),
            ];
            let remaining = list.width as usize - 2.min(list.width as usize);
            let remaining = remaining.saturating_sub(symbol.width().max(10));
            // Drop optional columns whole: neither a price nor its currency
            // should look like a smaller number because the pane is narrow.
            if remaining >= 25 {
                let show_status = remaining >= 33;
                let name_width = remaining.saturating_sub(34);
                if name_width >= 16 {
                    let name = data.and_then(|d| d.quote.name.as_deref()).unwrap_or("");
                    spans.push(Span::styled(
                        format!(" {}", pad_right(&ellipsize(name, name_width), name_width)),
                        theme.subtle(),
                    ));
                }
                let price = data.map(|d| {
                    let currency = d.quote.currency.as_deref().unwrap_or("");
                    format!("{} {currency}", fmt_price(d.quote.price))
                        .trim()
                        .to_string()
                });
                let price = if !show_status && !status.is_empty() {
                    status.to_string()
                } else {
                    price.unwrap_or_else(|| "—".into())
                };
                // A value wider than its column is explicitly omitted;
                // clipping its last digits would show a different price.
                let price = if price.width() <= 14 { &price } else { "…" };
                spans.push(Span::raw(format!(" {price:>14}")));
                let change = data.and_then(|d| d.quote.change_pct());
                let pct = change
                    .map(|p| format!("{p:+.2}%"))
                    .unwrap_or_else(|| "—".into());
                let pct = if pct.width() <= 9 { &pct } else { "…" };
                spans.push(Span::styled(
                    format!(" {pct:>9}"),
                    Style::new().fg(crate::ui::chart::move_color(change, theme)),
                ));
                if show_status {
                    spans.push(Span::styled(format!(" {status:<7}"), theme.subtle()));
                }
            } else if !status.is_empty() {
                spans.push(Span::styled(format!(" {status}"), theme.subtle()));
            }
            let line = Line::from(spans);
            if selected {
                line.style(theme.selected())
            } else {
                line
            }
        })
        .collect();
    let rows = if matches.is_empty() {
        vec![Line::from(" No matches in your watchlist").style(theme.subtle())]
    } else {
        rows
    };
    f.render_widget(Paragraph::new(rows), list);
    let hint = if keys.width >= 61 {
        " ↑↓ move · enter select · esc cancel · ctrl-u clear"
    } else {
        " ↑↓ move · enter select · esc cancel"
    };
    f.render_widget(Paragraph::new(hint).style(theme.subtle()), keys);
}
