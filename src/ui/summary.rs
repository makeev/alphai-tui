//! Summary view: the whole watchlist as small charts at once.
//!
//! The Table view already lists every ticker, but a sparkline squeezed
//! into one row answers "up or down" and nothing else. This view spends
//! real rows on each ticker instead, so a glance covers the shape of the
//! session across the watchlist rather than one symbol at a time. It is
//! the layout tickrs is built around, and the one thing this app had no
//! answer to.
//!
//! Cards are laid out in a grid sized to the terminal, and the page
//! follows the watchlist cursor rather than carrying a scroll offset of
//! its own: ↑↓ already move that cursor everywhere else.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph};

use crate::app::App;
use crate::domain::{TickerData, fmt_price};
use crate::keymap::Action;
use crate::ui::chart::{dir_color, flash_style, visible_from};
use crate::ui::{Hint, View, ViewId};

/// Smallest card that still reads: a border, a title and two plot rows.
const MIN_CARD_W: u16 = 30;
const MIN_CARD_H: u16 = 6;
/// And the tallest. A card is about 40 columns wide, so past this the
/// session's own jitter gets stretched vertically until the line reads as
/// noise rather than as a path. Slack goes to the bottom of the view.
const MAX_CARD_H: u16 = 12;
/// Past this many columns the eye stops tracking a row, so the grid stops
/// widening even on very large terminals.
const MAX_COLS: usize = 3;

static HINTS: &[Hint] = &[
    Hint::act(&[Action::Quit], "quit"),
    Hint::fixed("tab/1-9", "view"),
    Hint::act(&[Action::Up, Action::Down], "select"),
    Hint::act(&[Action::AddTicker], "add"),
    Hint::act(&[Action::NextPreset], "interval"),
    Hint::act(&[Action::Refresh], "refresh"),
    Hint::act(&[Action::Help], "help"),
];

pub struct SummaryView;

impl View for SummaryView {
    fn id(&self) -> ViewId {
        ViewId::Summary
    }

    fn title(&self) -> &'static str {
        "Summary"
    }

    fn hints(&self) -> &'static [Hint] {
        HINTS
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        render_summary(f, area, app);
    }
}

/// Columns for `count` tickers in `area`. Capped by the terminal, by
/// `MAX_COLS`, and by the number of tickers themselves, so two symbols
/// never render as two narrow cards beside an empty slot.
fn columns(area: Rect, count: usize) -> usize {
    ((area.width / MIN_CARD_W) as usize)
        .clamp(1, MAX_COLS)
        .min(count.max(1))
}

/// Card rows a page may use: what the height allows, at least one.
fn max_rows(area: Rect) -> usize {
    ((area.height / MIN_CARD_H) as usize).max(1)
}

fn render_summary(f: &mut Frame, area: Rect, app: &mut App) {
    let cols = columns(area, app.symbols.len());
    let per_page = cols * max_rows(area);
    // The cursor drives the page, so ↑↓ walk off the bottom into the next
    // one without this view holding a scroll position of its own.
    let start = (app.selected / per_page) * per_page;
    let shown: Vec<String> = app
        .symbols
        .iter()
        .skip(start)
        .take(per_page)
        .cloned()
        .collect();
    // Rows come from what this page actually holds, not from what the
    // height would allow: four tickers on a tall terminal are four tall
    // cards, not four short ones above an empty half-screen.
    let rows = shown.len().div_ceil(cols).max(1);

    let card_h = (area.height / rows as u16).min(MAX_CARD_H);
    let mut heights = vec![Constraint::Length(card_h); rows];
    // Absorbs the remainder so the cards keep their height instead of
    // stretching to fill a tall terminal.
    heights.push(Constraint::Min(0));
    let row_areas = Layout::vertical(heights).split(area);
    for (r, row_area) in row_areas.iter().take(rows).enumerate() {
        let col_areas =
            Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols]).split(*row_area);
        for (c, cell) in col_areas.iter().enumerate() {
            let Some(symbol) = shown.get(r * cols + c) else {
                continue;
            };
            card(f, *cell, app, symbol, start + r * cols + c == app.selected);
        }
    }
}

/// One ticker: its quote in the border title, its recent closes inside.
fn card(f: &mut Frame, area: Rect, app: &App, symbol: &str, selected: bool) {
    let theme = &app.theme;
    let block = theme.panel().title(title_line(app, symbol, selected));

    let Some(data) = app.data.get(symbol) else {
        let msg = match app.errors.get(symbol) {
            Some(e) => Line::from(e.clone()).style(Style::new().fg(theme.error)),
            None => Line::from("…").dim(),
        };
        f.render_widget(Paragraph::new(msg).block(block), area);
        return;
    };
    plot(f, area, app, data, block);
}

/// `▶ AAPL 315.42 -0.27%`, the same numbers the rail and the table show.
///
/// The cursor is the same `▶` the watchlist table marks its row with, not
/// a colored border: borders carry the theme's border slot everywhere in
/// the app, and a card is too small to reverse without burying the chart.
fn title_line(app: &App, symbol: &str, selected: bool) -> Line<'static> {
    let theme = &app.theme;
    let mut spans = vec![Span::styled(
        if selected {
            format!(" ▶ {symbol} ")
        } else {
            format!(" {symbol} ")
        },
        Style::new().bold().fg(theme.accent),
    )];
    match app.data.get(symbol) {
        Some(data) => {
            let q = &data.quote;
            let style = app
                .price_flash_dir(symbol)
                .map_or(Style::new(), |up| flash_style(up, theme));
            spans.push(Span::styled(fmt_price(q.price), style));
            if let Some(pct) = q.change_pct() {
                spans.push(Span::styled(
                    format!(" {pct:+.2}% "),
                    Style::new().fg(dir_color(q, theme)),
                ));
            } else {
                spans.push(Span::raw(" "));
            }
        }
        None => spans.push(Span::raw(" ").dim()),
    }
    Line::from(spans)
}

/// The close line alone: no axes, no averages, no previous-close
/// reference. A card is 4 rows of plot at its smallest, and anything past
/// the price itself turns that into noise. The full chart is one key away.
///
/// Renders rather than returns: the widget borrows its point buffer, and
/// that buffer cannot outlive this call.
fn plot(f: &mut Frame, area: Rect, app: &App, data: &TickerData, block: Block) {
    let q = &data.quote;
    let cut = visible_from(&data.candles, app.range);
    let points: Vec<(f64, f64)> = data.candles[cut..]
        .iter()
        .enumerate()
        .map(|(i, c)| (i as f64, c.close))
        .collect();
    if points.len() < 2 {
        f.render_widget(Paragraph::new(Line::from("…").dim()).block(block), area);
        return;
    }

    let (lo, hi) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(_, y)| {
            (lo.min(y), hi.max(y))
        });
    let pad = ((hi - lo) * 0.05).max(hi.abs() * 0.0005).max(1e-9);
    let x_hi = (points.len() - 1) as f64;

    let chart = Chart::new(vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(dir_color(q, &app.theme)))
            .data(&points),
    ])
    .block(block)
    .x_axis(Axis::default().bounds([0.0, x_hi]))
    .y_axis(Axis::default().bounds([lo - pad, hi + pad]));
    f.render_widget(chart, area);
}
