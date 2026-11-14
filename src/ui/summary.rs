//! Summary view: the whole watchlist at a glance, in one of three layouts.
//!
//! A line over the layout sums up the day: the session, how many tickers
//! are up and down, the portfolio's day, the next macro release and the
//! watchlist's reports due within the week. `x` picks the layout:
//!
//! - cards: a card per ticker with its path through the window, the
//!   previous close, the extended sessions shaded, its news marks, the day's
//!   range, the window's move, the position and the alert;
//! - heatmap: a tile per ticker tinted by the day's move, the move in big
//!   figures;
//! - compare: every ticker on one chart, in percent of the window's start.
//!
//! `f` orders the tickers as the watchlist does or by the day's move. The
//! page follows the cursor rather than carrying a scroll offset of its own:
//! ↑↓ walk the tickers in the order on screen. Everything here comes from
//! data the app already holds, so the view costs no request.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, SummaryLayout, SummaryOrder};
use crate::domain::{Candle, Interval, TickerData, fmt_price};
use crate::keymap::Action;
use crate::market::{self, Session};
use crate::portfolio::{self, fmt_signed};
use crate::theme::Theme;
use crate::ui::chart::{BrailleOverlay, dir_color, move_color, visible_from};
use crate::ui::hits::Button;
use crate::ui::keycap::{self, Pills};
use crate::ui::{Hint, View, ViewId, bigtext, news_marks};

/// Smallest card that still reads: a border, four plot rows and the line
/// of facts under them.
const CARD_MIN_W: u16 = 34;
const CARD_MIN_H: u16 = 7;
/// And the tallest: past this the session's jitter stretches until the
/// path reads as noise. Slack goes to the bottom of the view.
const CARD_MAX_H: u16 = 24;
/// Past this many columns the eye stops tracking a row.
const MAX_COLS: usize = 3;
/// A heat tile's smallest size, and the most tiles in a row.
const TILE_MIN_W: u16 = 20;
const TILE_MIN_H: u16 = 4;
const MAX_TILE_COLS: usize = 6;
/// The move that tints a heat tile fully, in percent.
const HEAT_FULL: f64 = 5.0;
/// The strongest tint, in percent of the way from the panel to the color:
/// past it the figures on the tile stop reading.
const HEAT_MAX_TINT: f64 = 60.0;
/// Width of the day-range track in a card's line of facts.
const TRACK: usize = 10;
/// The most news marks a card draws.
const CARD_MARKS: usize = 6;

static HINTS: &[Hint] = &[
    Hint::act(&[Action::Quit], "quit"),
    Hint::fixed("tab/1-9", "view"),
    Hint::act(&[Action::Up, Action::Down], "select"),
    Hint::act(&[Action::PriceAlert], "alert"),
    Hint::act(&[Action::CycleLayout], "layout"),
    Hint::act(&[Action::CycleScope], "order"),
    Hint::act(&[Action::AddTicker], "add"),
    Hint::act(&[Action::NextPreset], "preset"),
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
        render_summary(f, area, app, Utc::now());
    }
}

/// The watchlist's indices in the order the view shows them.
pub(crate) fn order(app: &App) -> Vec<usize> {
    let mut order: Vec<usize> = (0..app.symbols.len()).collect();
    if app.summary_order == SummaryOrder::Change {
        let pct = |i: usize| {
            app.data
                .get(&app.symbols[i])
                .and_then(|d| d.quote.change_pct())
        };
        // Biggest gain first; tickers without a move yet go last, in
        // watchlist order.
        order.sort_by(|&a, &b| match (pct(a), pct(b)) {
            (Some(x), Some(y)) => y.total_cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(&b),
        });
    }
    order
}

pub(crate) fn render_summary(f: &mut Frame, area: Rect, app: &mut App, now: DateTime<Utc>) {
    let body = if area.height >= 8 {
        let [strip, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let buttons = render_strip(f, strip, app, now);
        app.hits.buttons.extend(buttons);
        body
    } else {
        area
    };
    let order = order(app);
    if order.is_empty() {
        let key = keycap::label(&app.keymap.labels(&[Action::AddTicker]));
        f.render_widget(
            Paragraph::new(
                Line::from(format!("the watchlist is empty, {key} adds a ticker"))
                    .style(app.theme.subtle()),
            )
            .centered(),
            body,
        );
        return;
    }
    match app.summary_layout {
        SummaryLayout::Cards => render_cards(f, body, app, &order, now),
        SummaryLayout::Heatmap => render_heatmap(f, body, app, &order),
        SummaryLayout::Compare => render_compare(f, body, app, &order),
    }
}

// -- the day's line -----------------------------------------------------------

/// The line over the layout: what the day looks like on the left, the
/// layout and the order as pills on the right. The pills win the room;
/// the facts drop from the end, least needed first.
fn render_strip(f: &mut Frame, area: Rect, app: &App, now: DateTime<Utc>) -> Vec<(Rect, Button)> {
    let theme = &app.theme;
    let width = usize::from(area.width);
    let switch = |all: bool| {
        let mut spans = Vec::new();
        let mut pills = Pills::new();
        for (action, key) in [
            (
                Action::CycleLayout,
                app.keymap.labels(&[Action::CycleLayout]),
            ),
            (Action::CycleScope, app.keymap.labels(&[Action::CycleScope])),
        ] {
            spans.push(Span::raw(" "));
            let cycle = Some(Button::Key(action));
            if !key.is_empty() {
                keycap::push_keycap(&mut spans, &mut pills, &key, cycle, theme);
            }
            let choices: Vec<(String, Button, bool)> = if action == Action::CycleLayout {
                SummaryLayout::ALL
                    .iter()
                    .filter(|l| all || **l == app.summary_layout)
                    .map(|l| {
                        let button = if all {
                            Button::SummaryLayout(*l)
                        } else {
                            Button::Key(action)
                        };
                        (l.name().to_string(), button, *l == app.summary_layout)
                    })
                    .collect()
            } else {
                SummaryOrder::ALL
                    .iter()
                    .filter(|o| all || **o == app.summary_order)
                    .map(|o| {
                        let button = if all {
                            Button::SummaryOrder(*o)
                        } else {
                            Button::Key(action)
                        };
                        (o.name().to_string(), button, *o == app.summary_order)
                    })
                    .collect()
            };
            keycap::push_choices(&mut spans, &mut pills, choices, theme);
        }
        spans.push(Span::raw(" "));
        (spans, pills)
    };
    let zones = day_zones(app, now);
    let width_of = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    // The full set of pills only where the session and the day's breadth
    // still fit beside it; otherwise the current choices alone.
    let first: usize = zones.iter().take(2).map(|z| width_of(z)).sum();
    let (right, right_pills) = {
        let full = switch(true);
        if first + width_of(&full.0) <= width {
            full
        } else {
            switch(false)
        }
    };
    let right_w = width_of(&right);
    let mut spans = Vec::new();
    for zone in zones {
        if width_of(&spans) + width_of(&zone) + right_w <= width {
            spans.extend(zone);
        }
    }
    let used = width_of(&spans);
    if used + right_w > width {
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return Vec::new();
    }
    spans.push(Span::raw(" ".repeat(width - used - right_w)));
    spans.extend(right);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let x0 = area.x + (width - right_w) as u16;
    keycap::place(right_pills, x0, area.y, area.right())
}

/// The day's facts, each a zone, most needed first.
fn day_zones(app: &App, now: DateTime<Utc>) -> Vec<Vec<Span<'static>>> {
    let theme = &app.theme;
    let mut zones = Vec::new();

    let clock = market::clock_at(now);
    let style = match clock.session {
        Session::Open => Style::new().fg(theme.pos),
        Session::Pre | Session::Post => Style::new().fg(theme.warn),
        Session::Closed => theme.subtle(),
    };
    zones.push(vec![
        Span::styled(format!(" {}", clock.session.label()), style),
        Span::styled(format!(" {}", clock.countdown()), theme.subtle()),
    ]);

    let moves: Vec<f64> = app
        .symbols
        .iter()
        .filter_map(|s| app.data.get(s).and_then(|d| d.quote.change_pct()))
        .collect();
    if !moves.is_empty() {
        let up = moves.iter().filter(|p| **p > 0.0).count();
        let down = moves.iter().filter(|p| **p < 0.0).count();
        let avg = moves.iter().sum::<f64>() / moves.len() as f64;
        zones.push(vec![
            Span::raw("   "),
            Span::styled("▲", Style::new().fg(theme.pos)),
            Span::styled(format!(" {up}  "), Style::new().fg(theme.text)),
            Span::styled("▼", Style::new().fg(theme.neg)),
            Span::styled(format!(" {down}  "), Style::new().fg(theme.text)),
            Span::styled("avg ", theme.subtle()),
            Span::styled(
                format!("{avg:+.2}%"),
                Style::new().fg(move_color(Some(avg), theme)),
            ),
        ]);
    }

    if !app.positions.is_empty() {
        let totals = portfolio::totals(
            app.positions
                .iter()
                .map(|p| (p, app.data.get(&p.symbol).map(|d| &d.quote))),
            now,
        );
        if let Some(day) = totals.day_pnl {
            let start = totals.value - day;
            let pct = (start.abs() > 0.0).then(|| day / start.abs() * 100.0);
            let mut zone = vec![
                Span::styled("   portfolio today ", theme.subtle()),
                Span::styled(
                    fmt_signed(day),
                    Style::new().fg(move_color(Some(day), theme)),
                ),
            ];
            if let Some(pct) = pct {
                zone.push(Span::styled(
                    format!(" ({pct:+.2}%)"),
                    Style::new().fg(move_color(Some(day), theme)),
                ));
            }
            zones.push(zone);
        }
    }

    if let Some(slot) = &app.calendar {
        let ahead = crate::ui::earnings::ahead_line(&slot.events, now);
        if let Some(next) = ahead.split("  ·  ").next().filter(|s| !s.is_empty()) {
            zones.push(vec![
                Span::styled("   next ", theme.subtle()),
                Span::styled(next.to_string(), Style::new().fg(theme.text)),
            ]);
        }
    }

    let today = market::et_date(now.timestamp());
    let mut reports: Vec<(i64, &str)> = app
        .symbols
        .iter()
        .filter_map(|s| {
            let day = crate::ui::calendar::next_report_day(app, s, now)?;
            let days = (day - today?).num_days();
            (0..=7).contains(&days).then_some((days, s.as_str()))
        })
        .collect();
    reports.sort();
    if !reports.is_empty() {
        let list = reports
            .iter()
            .take(3)
            .map(|(days, s)| format!("{s} {}", when(*days)))
            .collect::<Vec<_>>()
            .join(" · ");
        zones.push(vec![
            Span::styled("   earnings ", theme.subtle()),
            Span::styled(list, Style::new().fg(theme.accent)),
        ]);
    }
    zones
}

/// A report `days` away, as the strip and the cards say it.
fn when(days: i64) -> String {
    match days {
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        n => format!("in {n}d"),
    }
}

// -- cards --------------------------------------------------------------------

/// Columns for `count` cards in `area`, never wider than the tickers need.
fn columns(area: Rect, count: usize, min_w: u16, max: usize) -> usize {
    ((area.width / min_w) as usize)
        .clamp(1, max)
        .min(count.max(1))
}

fn render_cards(f: &mut Frame, area: Rect, app: &mut App, order: &[usize], now: DateTime<Utc>) {
    let cols = columns(area, order.len(), CARD_MIN_W, MAX_COLS);
    let fit_rows = (area.height / CARD_MIN_H).max(1) as usize;
    // More tickers than cards fit: one line each instead of paging cards.
    if order.len() > cols * fit_rows {
        render_dense(f, area, app, order, now);
        return;
    }
    let rows = order.len().div_ceil(cols);
    let card_h = (area.height / rows as u16).min(CARD_MAX_H);
    let mut heights = vec![Constraint::Length(card_h); rows];
    heights.push(Constraint::Min(0));
    let row_areas = Layout::vertical(heights).split(area);
    for (r, row_area) in row_areas.iter().take(rows).enumerate() {
        let col_areas =
            Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols]).split(*row_area);
        for (c, cell) in col_areas.iter().enumerate() {
            let Some(&idx) = order.get(r * cols + c) else {
                continue;
            };
            app.hits.cards.push((*cell, idx));
            card(f, *cell, app, idx, now);
        }
    }
}

/// One ticker: its quote across the top border, its path through the
/// window inside with the facts under it, its latest story and its next
/// report along the bottom.
fn card(f: &mut Frame, area: Rect, app: &App, idx: usize, now: DateTime<Utc>) {
    let theme = &app.theme;
    let symbol = app.symbols[idx].as_str();
    let selected = idx == app.selected;
    let (left, right) = card_titles(app, symbol, selected, now);
    let room = usize::from(area.width.saturating_sub(2));
    let mut block = theme.panel().title(left.clone());
    if let Some(right) = right.filter(|r| left.width() + 1 + r.width() <= room) {
        block = block.title(right.right_aligned());
    }

    let Some(data) = app.data.get(symbol) else {
        let msg = match app.errors.get(symbol) {
            Some(e) => Line::from(e.clone()).style(Style::new().fg(theme.error)),
            None => Line::from("…").style(theme.subtle()),
        };
        f.render_widget(Paragraph::new(msg).block(block), area);
        return;
    };
    let ((range, interval, _), _) = app.chart_window(symbol);
    let cut = visible_from(&data.candles, range);
    let visible = &data.candles[cut..];
    let marks = if app.show_news_markers {
        news_marks::place(app.ticker_articles(symbol), visible, interval, None, symbol)
    } else {
        Vec::new()
    };

    // Along the bottom: the latest story, and the next report if it is due
    // within two weeks.
    let badge = report_badge(app, symbol, now);
    let badge_w = badge.as_ref().map_or(0, |b| b.width() as u16 + 1);
    if let Some(story) = news_marks::latest(&marks)
        .and_then(|m| news_marks::headline(m, symbol, area.width.saturating_sub(badge_w), theme))
    {
        block = block.title_bottom(story);
    }
    if let Some(badge) = badge.filter(|b| b.width() + 2 <= room) {
        block = block.title_bottom(badge.right_aligned());
    }

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 4 {
        return;
    }
    let (plot, facts) = if inner.height >= 4 {
        let [plot, facts] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        (plot, Some(facts))
    } else {
        (inner, None)
    };
    draw_path(
        f.buffer_mut(),
        plot,
        app,
        symbol,
        data,
        visible,
        interval,
        &marks,
    );
    if let Some(facts) = facts {
        let line = facts_line(app, symbol, data, visible, range.as_str(), facts.width);
        f.render_widget(Paragraph::new(line), facts);
    }
}

/// `▶ NBIS  115.88  ▼ -12.76  -9.92%` on the left, the extended print on
/// the right (`AH 112.80 -2.66%`).
fn card_titles(
    app: &App,
    symbol: &str,
    selected: bool,
    now: DateTime<Utc>,
) -> (Line<'static>, Option<Line<'static>>) {
    let theme = &app.theme;
    let mut spans = vec![Span::styled(
        if selected {
            format!(" ▶ {symbol} ")
        } else {
            format!(" {symbol} ")
        },
        Style::new().add_modifier(Modifier::BOLD).fg(theme.accent),
    )];
    let Some(data) = app.data.get(symbol) else {
        return (Line::from(spans), None);
    };
    let q = &data.quote;
    spans.push(Span::styled(
        format!(" {} ", fmt_price(q.price)),
        app.price_style(
            symbol,
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ));
    if let (Some(change), Some(pct)) = (q.change(), q.change_pct()) {
        let color = Style::new().fg(dir_color(q, theme));
        let arrow = if change < 0.0 { "▼" } else { "▲" };
        spans.push(Span::styled(format!(" {arrow} {change:+.2} "), color));
        spans.push(Span::styled(format!(" {pct:+.2}% "), color));
    }
    let right = q.extended_price_at(now).map(|price| {
        let reference = q.extended_reference();
        let pct = if reference != 0.0 {
            (price - reference) / reference * 100.0
        } else {
            0.0
        };
        let session = q
            .timing
            .extended
            .and_then(market::window_at)
            .map(|w| w.session)
            .unwrap_or(market::clock_at(now).session);
        let label = if session == Session::Pre { "PRE" } else { "AH" };
        Line::from(vec![
            Span::styled(format!(" {label} "), theme.subtle()),
            Span::styled(fmt_price(price), Style::new().fg(theme.text)),
            Span::styled(
                format!(" {pct:+.2}% "),
                Style::new().fg(move_color(Some(price - reference), theme)),
            ),
        ])
    });
    (Line::from(spans), right)
}

/// `earnings in 3d` when the ticker's next confirmed report is two weeks
/// away or closer. Known only once the Calendar has looked the date up.
fn report_badge(app: &App, symbol: &str, now: DateTime<Utc>) -> Option<Line<'static>> {
    let day = crate::ui::calendar::next_report_day(app, symbol, now)?;
    let days = (day - market::et_date(now.timestamp())?).num_days();
    (0..=14).contains(&days).then(|| {
        Line::from(Span::styled(
            format!(" earnings {} ", when(days)),
            Style::new().fg(app.theme.accent),
        ))
    })
}

/// The path through the window: the closes as a braille line in the day's
/// direction, the previous close dashed, the extended sessions shaded as on
/// the chart, the alert's level, the news marks above the line, and the
/// window's high and low in a gutter where the card has the width.
#[allow(clippy::too_many_arguments)]
fn draw_path(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    symbol: &str,
    data: &TickerData,
    visible: &[Candle],
    interval: Interval,
    marks: &[news_marks::Mark],
) {
    let theme = &app.theme;
    let q = &data.quote;
    if visible.len() < 2 || area.width < 4 || area.height == 0 {
        buf.set_string(area.x, area.y, "…", theme.subtle());
        return;
    }
    let (mut lo, mut hi) = visible
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| {
            (lo.min(c.close), hi.max(c.close))
        });
    let (data_lo, data_hi) = (lo, hi);
    // The previous close belongs on the scale unless it is far away: a
    // line pinned to the edge says more than a path squashed flat.
    if let Some(prev) = q.prev_close.filter(|p| *p > 0.0) {
        let span = (hi - lo).max(hi.abs() * 0.001);
        if prev >= lo - span && prev <= hi + span {
            lo = lo.min(prev);
            hi = hi.max(prev);
        }
    }
    let pad = ((hi - lo) * 0.05).max(hi.abs() * 0.0005).max(1e-9);
    let (lo, hi) = (lo - pad, hi + pad);

    // The gutter: the window's high and low, where the card is wide enough.
    let labels = [fmt_price(data_hi), fmt_price(data_lo)];
    let gutter = if area.width >= 36 && area.height >= 2 {
        labels.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16 + 1
    } else {
        0
    };
    let plot = Rect {
        x: area.x + gutter,
        width: area.width - gutter,
        ..area
    };
    if gutter > 0 {
        buf.set_string(area.x, area.y, &labels[0], theme.subtle());
        buf.set_string(area.x, area.bottom() - 1, &labels[1], theme.subtle());
    }

    let n = visible.len();
    let dots_w = i32::from(plot.width) * 2 - 1;
    let dots_h = i32::from(plot.height) * 4 - 1;
    let x_of = |i: usize| (i as i32 * dots_w) / (n as i32 - 1).max(1);
    let y_of = |v: f64| (((hi - v) / (hi - lo)) * f64::from(dots_h)).round() as i32;
    let row_of = |v: f64| plot.y + (y_of(v).clamp(0, dots_h) / 4) as u16;

    // Extended sessions shaded as on the chart: intraday US equities only.
    if app.chart.session_shading && interval != Interval::D1 && market::is_us_equity(symbol) {
        for col in 0..plot.width {
            let i = (usize::from(col) * (n - 1)) / usize::from(plot.width.max(2) - 1);
            let color = match market::window_at(visible[i.min(n - 1)].ts).map(|w| w.session) {
                Some(Session::Pre) => theme.pre_market_bg,
                Some(Session::Post) => theme.post_market_bg,
                _ => continue,
            };
            for y in plot.y..plot.bottom() {
                if let Some(cell) = buf.cell_mut((plot.x + col, y)) {
                    cell.set_bg(color);
                }
            }
        }
    }
    let dashes = |buf: &mut Buffer, price: f64, ch: char, style: Style| {
        if price < lo || price > hi {
            return;
        }
        let row = row_of(price);
        for x in (plot.x..plot.right()).step_by(2) {
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_char(ch).set_style(style);
            }
        }
    };
    if let Some(prev) = q.prev_close {
        dashes(buf, prev, '┄', Style::new().fg(theme.ref_line));
    }
    if let Some(rule) = app.alerts.get(symbol) {
        dashes(buf, rule.level.price, '╌', super::alerts::style(app, rule));
    }

    let color = dir_color(q, theme);
    let mut line = BrailleOverlay::new(plot.width, plot.height);
    let mut previous = None;
    for (i, c) in visible.iter().enumerate() {
        let point = (x_of(i), y_of(c.close));
        match previous {
            Some(prev) => line.line(prev, point, color),
            None => line.dot(point.0, point.1, color),
        }
        previous = Some(point);
    }
    line.blit(buf, plot);

    // News marks one row above the line at their candle, one per column:
    // a card squeezes several candles into a cell, and the story that
    // scored highest speaks for it.
    let mut best: std::collections::BTreeMap<u16, &news_marks::Mark> = Default::default();
    for mark in marks {
        let x = (x_of(mark.col) / 2) as u16;
        let slot = best.entry(x).or_insert(mark);
        if mark.article.score() > slot.article.score() {
            *slot = mark;
        }
    }
    // And no more than a handful: the strongest stories, the newest on a
    // tie, so the card points at what moved it rather than at everything.
    let mut shown: Vec<&news_marks::Mark> = best.into_values().collect();
    shown.sort_by_key(|m| {
        std::cmp::Reverse((
            m.article.score(),
            m.article.published().map(|t| t.timestamp()).unwrap_or(0),
        ))
    });
    shown.truncate(CARD_MARKS);
    for mark in shown {
        let (ch, style) = news_marks::glyph(mark.article, symbol, theme);
        let x = plot.x + (x_of(mark.col) / 2) as u16;
        let row = row_of(visible[mark.col].close)
            .saturating_sub(1)
            .max(plot.y);
        if let Some(cell) = buf.cell_mut((x.min(plot.right() - 1), row)) {
            cell.set_char(ch).set_style(style);
        }
    }
}

/// The card's facts, as many as fit, most needed first: the window's move,
/// the day's range with the price in it, the position, the alert.
fn facts_line(
    app: &App,
    symbol: &str,
    data: &TickerData,
    visible: &[Candle],
    range: &str,
    width: u16,
) -> Line<'static> {
    let theme = &app.theme;
    let q = &data.quote;
    // Each fact in its forms, widest first; a fact whose narrowest form
    // does not fit is left out and the next one still gets its chance.
    let mut zones: Vec<Vec<Vec<Span<'static>>>> = Vec::new();
    if let (Some(first), Some(last)) = (visible.first(), visible.last())
        && first.close > 0.0
    {
        let pct = (last.close / first.close - 1.0) * 100.0;
        zones.push(vec![vec![
            Span::styled(format!("{range} "), theme.subtle()),
            Span::styled(
                format!("{pct:+.1}%"),
                Style::new().fg(move_color(Some(pct), theme)),
            ),
        ]]);
    }
    if let Some(pos) = app.positions.iter().find(|p| p.symbol == symbol) {
        let price = portfolio::price(q);
        let pnl = pos.pnl(price);
        let color = Style::new().fg(move_color(Some(pnl), theme));
        let pct = pos.pnl_pct(price);
        let mut full = vec![
            Span::styled(format!("×{} ", trim_qty(pos.qty)), theme.subtle()),
            Span::styled(fmt_signed(pnl), color),
        ];
        if let Some(pct) = pct {
            full.push(Span::styled(format!(" {pct:+.1}%"), color));
        }
        let mut forms = vec![full];
        if let Some(pct) = pct {
            forms.push(vec![
                Span::styled(format!("×{} ", trim_qty(pos.qty)), theme.subtle()),
                Span::styled(format!("{pct:+.1}%"), color),
            ]);
        }
        zones.push(forms);
    }
    if let Some(rule) = app.alerts.get(symbol) {
        zones.push(vec![vec![Span::styled(
            format!("alert {} {}", rule.marker(), rule.level.label),
            super::alerts::style(app, rule),
        )]]);
    }
    if let Some((lo, hi)) = q
        .day_range
        .or_else(|| crate::ui::rail::day_range(&data.candles))
    {
        let pos = if hi > lo {
            (((q.price - lo) / (hi - lo)) * (TRACK - 1) as f64).round() as usize
        } else {
            TRACK / 2
        }
        .min(TRACK - 1);
        let track = vec![
            Span::styled("━".repeat(pos), Style::new().fg(theme.ref_line)),
            Span::styled("●", Style::new().fg(dir_color(q, theme))),
            Span::styled("━".repeat(TRACK - 1 - pos), Style::new().fg(theme.ref_line)),
        ];
        let mut full = vec![Span::styled(
            format!("day {} ", fmt_price(lo)),
            theme.subtle(),
        )];
        full.extend(track.clone());
        full.push(Span::styled(format!(" {}", fmt_price(hi)), theme.subtle()));
        let mut bare = vec![Span::styled("day ", theme.subtle())];
        bare.extend(track);
        zones.push(vec![full, bare]);
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for forms in zones {
        let gap = if spans.is_empty() { 0 } else { 3 };
        let Some(form) = forms
            .into_iter()
            .find(|f| used + gap + f.iter().map(Span::width).sum::<usize>() <= usize::from(width))
        else {
            continue;
        };
        if gap > 0 {
            spans.push(Span::raw("   "));
        }
        used += gap + form.iter().map(Span::width).sum::<usize>();
        spans.extend(form);
    }
    Line::from(spans)
}

/// A quantity without trailing zeros: 300, 0.5, 12.25.
fn trim_qty(qty: f64) -> String {
    let s = format!("{qty:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// One line per ticker, for a watchlist longer than the cards that fit:
/// the quote, a sparkline of the window, the window's move, the position.
fn render_dense(f: &mut Frame, area: Rect, app: &mut App, order: &[usize], now: DateTime<Utc>) {
    let theme = app.theme;
    let block = theme.panel_titled(format!(" Watchlist · {} tickers ", order.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = usize::from(inner.height);
    if rows == 0 {
        return;
    }
    let pos = order.iter().position(|&i| i == app.selected).unwrap_or(0);
    let start = (pos / rows) * rows;
    for (r, &idx) in order.iter().skip(start).take(rows).enumerate() {
        let row = Rect {
            y: inner.y + r as u16,
            height: 1,
            ..inner
        };
        app.hits.cards.push((row, idx));
        let line = dense_line(app, idx, inner.width, now);
        let style = if idx == app.selected {
            theme.selected()
        } else {
            Style::new()
        };
        f.render_widget(Paragraph::new(line).style(style), row);
    }
}

fn dense_line(app: &App, idx: usize, width: u16, now: DateTime<Utc>) -> Line<'static> {
    let theme = &app.theme;
    let symbol = app.symbols[idx].as_str();
    let marker = if idx == app.selected { "▶ " } else { "  " };
    let mut spans = vec![Span::styled(
        format!("{marker}{symbol:<9}"),
        Style::new().add_modifier(Modifier::BOLD).fg(theme.accent),
    )];
    let Some(data) = app.data.get(symbol) else {
        spans.push(Span::styled("…", theme.subtle()));
        return Line::from(spans);
    };
    let q = &data.quote;
    let color = Style::new().fg(dir_color(q, theme));
    spans.push(Span::styled(
        format!("{:>10}", fmt_price(q.price)),
        app.price_style(symbol, Style::new().fg(theme.text)),
    ));
    spans.push(Span::styled(
        q.change_pct()
            .map_or_else(|| format!("{:>9}", ""), |p| format!("{p:>+8.2}%")),
        color,
    ));
    let ((range, _, _), _) = app.chart_window(symbol);
    let visible = &data.candles[visible_from(&data.candles, range)..];
    let closes: Vec<f64> = visible.iter().map(|c| c.close).collect();
    let spark_w = usize::from(width / 5).clamp(8, 30);
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        crate::ui::table::spark_line(&closes, spark_w),
        color,
    ));
    let used = spans.iter().map(Span::width).sum::<usize>() + 2;
    let room = usize::from(width).saturating_sub(used) as u16;
    let mut rest = facts_line(app, symbol, data, visible, range.as_str(), room).spans;
    if !rest.is_empty() {
        spans.push(Span::raw("  "));
        spans.append(&mut rest);
    }
    if let Some(badge) = report_badge(app, symbol, now) {
        spans.extend(badge.spans);
    }
    Line::from(spans)
}

// -- heat map -----------------------------------------------------------------

/// Columns for `count` heat tiles: the ones that bring a tile closest to
/// a square on screen (about 2.5 columns to a row) with the fewest empty
/// slots, within the minimum tile size, and that leave room for the big
/// figures (`figure_w` columns wide) where any choice does.
fn heat_columns(area: Rect, count: usize, figure_w: u16) -> usize {
    let most = columns(area, count, TILE_MIN_W, MAX_TILE_COLS);
    let score = |cols: usize| {
        let rows = count.div_ceil(cols).max(1);
        let w = f64::from(area.width) / cols as f64;
        let h = f64::from(area.height) / rows as f64;
        // The share of the grid left empty: five tickers read as three
        // and two, not as four and a lone one.
        let empty = (rows * cols - count) as f64 / (rows * cols) as f64;
        // A tile's frame takes two columns and two rows; the narrowest
        // tile of a row is one column short of the even share.
        let fits = (w as u16).saturating_sub(3) >= figure_w && (h as u16).saturating_sub(2) >= 5;
        let small = if fits { 0.0 } else { 0.5 };
        ((w / h.max(1.0)) / 2.5).ln().abs() + empty + small
    };
    (1..=most)
        .min_by(|&a, &b| score(a).total_cmp(&score(b)))
        .unwrap_or(1)
}

fn render_heatmap(f: &mut Frame, area: Rect, app: &mut App, order: &[usize]) {
    // The widest move on the page sets the figures' width.
    let figure_w = order
        .iter()
        .filter_map(|&i| bigtext::lines(&heat_text(app, i)))
        .map(|rows| rows[0].chars().count() as u16)
        .max()
        .unwrap_or(0);
    let cols = heat_columns(area, order.len(), figure_w);
    let fit_rows = (area.height / TILE_MIN_H).max(1) as usize;
    let per_page = cols * fit_rows;
    let pos = order.iter().position(|&i| i == app.selected).unwrap_or(0);
    let start = (pos / per_page) * per_page;
    let shown: Vec<usize> = order.iter().skip(start).take(per_page).copied().collect();
    let rows = shown.len().div_ceil(cols).max(1);
    let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);
    let cells: Vec<(Rect, usize)> = row_areas
        .iter()
        .enumerate()
        .flat_map(|(r, row_area)| {
            Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols])
                .split(*row_area)
                .iter()
                .enumerate()
                .filter_map(|(c, cell)| shown.get(r * cols + c).map(|&idx| (*cell, idx)))
                .collect::<Vec<_>>()
        })
        .collect();
    // Big figures on every tile or on none: the layout's rounding leaves
    // some tiles a column wider, and one big move among small ones reads
    // as the one that matters.
    let big = cells.iter().all(|(cell, idx)| {
        let (width, height) = (cell.width.saturating_sub(2), cell.height.saturating_sub(2));
        height >= 5
            && bigtext::lines(&heat_text(app, *idx))
                .is_some_and(|rows| rows[0].chars().count() as u16 <= width)
    });
    for (cell, idx) in cells {
        app.hits.cards.push((cell, idx));
        tile(f, cell, app, idx, big);
    }
}

/// A tile's move as its figures spell it.
fn heat_text(app: &App, idx: usize) -> String {
    app.data
        .get(&app.symbols[idx])
        .and_then(|d| d.quote.change_pct())
        .map_or_else(|| "…".to_string(), |p| format!("{p:+.2}%"))
}

/// A heat tile: the panel tinted toward the day's direction by how far the
/// ticker moved (fully at `HEAT_FULL` percent), the move in big figures
/// where the tile has the rows, the price and the window's move under it.
fn tile(f: &mut Frame, area: Rect, app: &App, idx: usize, big: bool) {
    let theme = &app.theme;
    let symbol = app.symbols[idx].as_str();
    let selected = idx == app.selected;
    let title = Span::styled(
        if selected {
            format!(" ▶ {symbol} ")
        } else {
            format!(" {symbol} ")
        },
        Style::new().add_modifier(Modifier::BOLD).fg(theme.accent),
    );
    let block = theme.panel().title(Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(data) = app.data.get(symbol) else {
        f.render_widget(Paragraph::new(Line::from("…").style(theme.subtle())), inner);
        return;
    };
    let q = &data.quote;
    let pct = q.change_pct();
    let (bg, figure) = heat(theme, pct);
    let fill = match bg {
        Some(bg) => Style::new().bg(bg),
        None => Style::new(),
    };
    f.buffer_mut().set_style(inner, fill);

    let pct_text = heat_text(app, idx);
    let ((range, _, _), _) = app.chart_window(symbol);
    let visible = &data.candles[visible_from(&data.candles, range)..];
    let mut under = vec![Span::styled(
        fmt_price(q.price),
        app.price_style(symbol, figure.add_modifier(Modifier::BOLD)),
    )];
    if let (Some(first), Some(last)) = (visible.first(), visible.last())
        && first.close > 0.0
    {
        let window = (last.close / first.close - 1.0) * 100.0;
        under.push(Span::styled(
            format!("   {} {window:+.1}%", range.as_str()),
            figure,
        ));
    }
    let under = Line::from(under).centered();

    let big = bigtext::lines(&pct_text).filter(|_| big);
    let mut lines: Vec<Line> = Vec::new();
    match big {
        Some(rows) => {
            for row in rows {
                lines.push(Line::styled(row, figure).centered());
            }
        }
        None => lines.push(Line::styled(pct_text, figure.add_modifier(Modifier::BOLD)).centered()),
    }
    if inner.height as usize > lines.len() {
        lines.push(under);
    }
    let top = inner.y + inner.height.saturating_sub(lines.len() as u16) / 2;
    let body = Rect {
        y: top,
        height: (lines.len() as u16).min(inner.height),
        ..inner
    };
    f.render_widget(Paragraph::new(lines), body);
}

/// The tile's background and the style of its figures for a move of
/// `pct`. On a palette of real colors the tint is a blend; on the ANSI
/// theme, which has none to blend, a strong move (from 3%) takes the
/// direction's own color with dark figures and a weaker one leaves the tile
/// clear with the figures in that color.
fn heat(theme: &Theme, pct: Option<f64>) -> (Option<Color>, Style) {
    let Some(pct) = pct else {
        return (None, theme.subtle());
    };
    let toward = move_color(Some(pct), theme);
    let strength = (pct.abs() / HEAT_FULL).min(1.0);
    let base = match theme.surface {
        Color::Rgb(..) => theme.surface,
        _ => theme.background,
    };
    match (base, toward) {
        (Color::Rgb(..), Color::Rgb(..)) => {
            let tint = blend(base, toward, strength * HEAT_MAX_TINT);
            (Some(tint), Style::new().fg(theme.text))
        }
        _ if strength >= 0.6 => (Some(toward), Style::new().fg(Color::Black)),
        _ => (None, Style::new().fg(toward)),
    }
}

/// `pct` percent of the way from `base` to `toward`.
fn blend(base: Color, toward: Color, pct: f64) -> Color {
    match (base, toward) {
        (Color::Rgb(r0, g0, b0), Color::Rgb(r1, g1, b1)) => {
            let mix = |a: u8, b: u8| {
                (f64::from(a) + (f64::from(b) - f64::from(a)) * pct / 100.0).round() as u8
            };
            Color::Rgb(mix(r0, r1), mix(g0, g1), mix(b0, b1))
        }
        _ => toward,
    }
}

// -- compare ------------------------------------------------------------------

/// One series of the comparison: the ticker's moves in percent of its first
/// visible close, by timestamp.
struct Series {
    idx: usize,
    color: Color,
    points: Vec<(i64, f64)>,
}

/// The colors of the comparison's lines: the palette's own hues other than
/// the up and down colors, which on this chart would read as the day's
/// direction. Repeats past the palette; the labels say which is which.
fn series_colors(theme: &Theme) -> Vec<Color> {
    let mut colors = Vec::new();
    for c in [
        theme.accent,
        theme.sma_fast,
        theme.sma_slow,
        theme.rsi_line,
        theme.warn,
        theme.score_high,
        theme.text,
    ] {
        if c != theme.up && c != theme.down && c != Color::Reset && !colors.contains(&c) {
            colors.push(c);
        }
    }
    if colors.is_empty() {
        colors.push(theme.text);
    }
    colors
}

fn render_compare(f: &mut Frame, area: Rect, app: &mut App, order: &[usize]) {
    let theme = app.theme;
    let colors = series_colors(&theme);
    let mut series: Vec<Series> = Vec::new();
    let mut window = None;
    for &idx in order {
        let symbol = &app.symbols[idx];
        let Some(data) = app.data.get(symbol) else {
            continue;
        };
        let ((range, interval, _), _) = app.chart_window(symbol);
        window.get_or_insert((range, interval));
        let visible = &data.candles[visible_from(&data.candles, range)..];
        if visible.len() < 2 {
            continue;
        }
        series.push(Series {
            idx,
            color: colors[idx % colors.len()],
            points: visible.iter().map(|c| (c.ts, c.close)).collect(),
        });
    }
    // One start for all: the latest first bar among them, so every line is
    // measured from the same moment (a ticker with a shorter history would
    // otherwise count from a later base and compare unlike with unlike).
    let start = series
        .iter()
        .filter_map(|s| s.points.first())
        .map(|p| p.0)
        .max()
        .unwrap_or(0);
    series.retain_mut(|s| {
        s.points.retain(|p| p.0 >= start);
        let Some(base) = s.points.first().map(|p| p.1).filter(|b| *b > 0.0) else {
            return false;
        };
        for p in &mut s.points {
            p.1 = (p.1 / base - 1.0) * 100.0;
        }
        s.points.len() >= 2
    });
    let heading = match window {
        Some((range, interval)) => format!(
            " Watchlist since the window's start · {} / {} ",
            range.as_str(),
            interval.as_str()
        ),
        None => " Watchlist since the window's start ".to_string(),
    };
    let block = theme.panel_titled(heading);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if series.is_empty() || inner.height < 3 || inner.width < 20 {
        f.render_widget(
            Paragraph::new(Line::from("waiting for prices").style(theme.subtle())).centered(),
            inner,
        );
        return;
    }

    let t0 = series
        .iter()
        .filter_map(|s| s.points.first())
        .map(|p| p.0)
        .min()
        .unwrap_or(0);
    let t1 = series
        .iter()
        .filter_map(|s| s.points.last())
        .map(|p| p.0)
        .max()
        .unwrap_or(t0 + 1);
    let (mut lo, mut hi) = (0.0f64, 0.0f64);
    for s in &series {
        for (_, p) in &s.points {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    let pad = ((hi - lo) * 0.05).max(0.1);
    let (lo, hi) = (lo - pad, hi + pad);

    // Labels on the right: "━ NBIS -14.3%", as wide as the widest.
    let label = |s: &Series| {
        let last = s.points.last().map_or(0.0, |p| p.1);
        let marker = if s.idx == app.selected { "▶" } else { "━" };
        format!("{marker} {} {last:+.1}%", app.symbols[s.idx])
    };
    let label_w = series
        .iter()
        .map(|s| label(s).chars().count())
        .max()
        .unwrap_or(0) as u16
        + 1;
    let y_labels = [format!("{hi:+.0}%"), format!("{lo:+.0}%")];
    let gutter = y_labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0) as u16
        + 1;
    if inner.width <= gutter + label_w + 8 {
        return;
    }
    let plot = Rect {
        x: inner.x + gutter,
        width: inner.width - gutter - label_w,
        ..inner
    };
    let dots_w = i64::from(plot.width) * 2 - 1;
    let dots_h = i32::from(plot.height) * 4 - 1;
    let x_of = |ts: i64| (((ts - t0) * dots_w) / (t1 - t0).max(1)) as i32;
    let y_of = |v: f64| (((hi - v) / (hi - lo)) * f64::from(dots_h)).round() as i32;
    let row_of = |v: f64| plot.y + (y_of(v).clamp(0, dots_h) / 4) as u16;

    let buf = f.buffer_mut();
    buf.set_string(inner.x, inner.y, &y_labels[0], theme.subtle());
    buf.set_string(inner.x, inner.bottom() - 1, &y_labels[1], theme.subtle());
    let zero = row_of(0.0);
    buf.set_string(inner.x, zero, "0%", theme.subtle());
    for x in (plot.x..plot.right()).step_by(2) {
        if let Some(cell) = buf.cell_mut((x, zero)) {
            cell.set_char('┄')
                .set_style(Style::new().fg(theme.ref_line));
        }
    }

    // The selected ticker last, so it wins the cells it crosses.
    series.sort_by_key(|s| s.idx == app.selected);
    let mut overlay = BrailleOverlay::new(plot.width, plot.height);
    for s in &series {
        let mut previous = None;
        for &(ts, p) in &s.points {
            let point = (x_of(ts), y_of(p));
            match previous {
                Some(prev) => overlay.line(prev, point, s.color),
                None => overlay.dot(point.0, point.1, s.color),
            }
            previous = Some(point);
        }
    }
    overlay.blit(buf, plot);

    // Labels at their line's last row, nudged apart top to bottom.
    let mut wanted: Vec<(u16, &Series)> = series
        .iter()
        .map(|s| (row_of(s.points.last().map_or(0.0, |p| p.1)), s))
        .collect();
    wanted.sort_by_key(|(row, s)| (*row, s.idx));
    let mut rows = Vec::with_capacity(wanted.len());
    let mut next_free = plot.y;
    for (row, _) in &wanted {
        let row = (*row).max(next_free);
        rows.push(row);
        next_free = row + 1;
    }
    // Pushed past the bottom: shift the column back up as a block.
    if let Some(&last) = rows.last()
        && last >= plot.bottom()
    {
        let over = last + 1 - plot.bottom();
        for (i, row) in rows.iter_mut().enumerate().rev() {
            let floor = plot.y + i as u16;
            *row = row.saturating_sub(over).max(floor);
        }
    }
    let x = plot.right() + 1;
    let mut hits = Vec::new();
    for ((_, s), row) in wanted.iter().zip(rows) {
        if row >= inner.bottom() {
            continue;
        }
        let mut style = Style::new().fg(s.color);
        if s.idx == app.selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        let text = label(s);
        buf.set_string(x, row, &text, style);
        hits.push((Rect::new(x, row, text.chars().count() as u16, 1), s.idx));
    }
    app.hits.cards.extend(hits);
}
