//! The Form 4 chart panel of the Insider view, mirroring the insider-trades
//! page on alphai.io: a log-value scatter of events (one triangle per
//! filing's tranche group) over two-sided weekly dollar bars, on a shared
//! calendar x-axis. Pure renderer over the fetched `InsiderTrades` bundle —
//! it costs zero extra requests and ignores the feed's score filter, so the
//! chart is always the full picture.

use chrono::{Datelike, Days, NaiveDate};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::alphai::{InsiderTrades, TradeEvent, fmt_usd};
use crate::app::InsiderChartWindow;
use crate::theme::Theme;

/// Scatter rows inside the panel; the log decades map onto these.
const SCATTER_ROWS: u16 = 5;
/// Weekly-bar rows; two-sided windows split them 2 up / 2 down.
const BARS_ROWS: u16 = 4;
/// Panel height with everything: borders + scatter + bars + month axis.
pub const FULL_HEIGHT: u16 = 2 + SCATTER_ROWS + BARS_ROWS + 1;
/// Bars dropped first when rows are scarce, like the price chart's panels.
pub const SHORT_HEIGHT: u16 = 2 + SCATTER_ROWS + 1;

/// Height the panel takes from `spare` rows (what the view can give without
/// starving its list): full, bars-less, or hidden.
pub fn panel_height(spare: u16) -> u16 {
    if spare >= FULL_HEIGHT {
        FULL_HEIGHT
    } else if spare >= SHORT_HEIGHT {
        SHORT_HEIGHT
    } else {
        0
    }
}

/// One plottable event: parsed, priced and inside the window.
struct Mark<'a> {
    date: NaiveDate,
    value: f64,
    event: &'a TradeEvent,
}

pub fn render(
    f: &mut Frame,
    area: Rect,
    trades: &InsiderTrades,
    window: InsiderChartWindow,
    selected_uid: Option<&str>,
    today: NaiveDate,
    theme: &Theme,
) {
    let Some(days) = window.days() else { return };
    let label = window.label();
    // The window never claims history from before coverage began.
    let mut start = today - Days::new(u64::from(days.saturating_sub(1)));
    if let Some(cov) = trades.coverage_start.as_deref().and_then(day)
        && cov > start
    {
        start = cov;
    }

    let in_window: Vec<&TradeEvent> = trades
        .chart_events
        .iter()
        .filter(|e| {
            e.transaction_date
                .as_deref()
                .and_then(day)
                .is_some_and(|d| d >= start && d <= today)
        })
        .collect();
    let marks: Vec<Mark> = in_window
        .iter()
        .filter_map(|e| {
            let date = e.transaction_date.as_deref().and_then(day)?;
            let value = e.total_value_usd.as_deref().and_then(usd)?;
            (value > 0.0).then_some(Mark { date, value, event: e })
        })
        .collect();

    let block = theme
        .panel()
        .title(title(&in_window, label, theme))
        .title_bottom(Line::from(" ▲ buy · ▼ sell · ▽ to issuer · dim = plan ").dim());
    let inner = block.inner(area);
    f.render_widget(block, area);

    if in_window.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(format!("no Form 4 events in the last {label}")).dim())
                .centered(),
            Rect { y: inner.y + inner.height / 2, height: 1, ..inner },
        );
        return;
    }

    // Left gutter sized to the widest decade label, like the candle chart.
    let (lo_exp, hi_exp) = log_domain(&marks);
    let y_labels: Vec<String> = (lo_exp..=hi_exp).map(decade_label).collect();
    let gutter = y_labels.iter().map(|s| s.chars().count()).max().unwrap_or(0) as u16 + 1;
    if inner.width <= gutter + 4 || inner.height < SCATTER_ROWS + 1 {
        return; // too small: leave the bare block
    }
    let with_bars = inner.height > SCATTER_ROWS + BARS_ROWS;
    let plot = Rect {
        x: inner.x + gutter,
        y: inner.y,
        width: inner.width - gutter,
        height: SCATTER_ROWS,
    };
    let span_days = (today - start).num_days().max(1) as f64;
    let col = |d: NaiveDate| -> u16 {
        let frac = (d - start).num_days().max(0) as f64 / span_days;
        plot.x + ((frac * (plot.width - 1) as f64).round() as u16).min(plot.width - 1)
    };

    let buf = f.buffer_mut();
    let dim = Style::new().dim();

    // Decade labels: top and bottom always, the middle decade when distinct.
    let row_of = |exp: f64| plot.y + value_row(10f64.powf(exp), lo_exp, hi_exp, SCATTER_ROWS);
    let mut label_rows = vec![(hi_exp, row_of(hi_exp as f64)), (lo_exp, row_of(lo_exp as f64))];
    let mid = (lo_exp + hi_exp) / 2;
    if mid != lo_exp && mid != hi_exp {
        label_rows.push((mid, row_of(mid as f64)));
    }
    for (exp, row) in label_rows {
        let text = decade_label(exp);
        let x = plot.x - 1 - text.chars().count() as u16;
        buf.set_string(x, row, &text, dim);
    }

    // Scatter: selected mark drawn last so its inverted cell always wins;
    // a taken cell nudges the mark up, then down, then overwrites.
    let mut taken = vec![false; plot.width as usize * SCATTER_ROWS as usize];
    let selected = |e: &TradeEvent| {
        selected_uid.is_some_and(|uid| e.news_uid.as_deref() == Some(uid))
    };
    let (chosen, rest): (Vec<&Mark>, Vec<&Mark>) =
        marks.iter().partition(|m| selected(m.event));
    for mark in rest.iter().chain(chosen.iter()) {
        let x = col(mark.date);
        let ideal = value_row(mark.value, lo_exp, hi_exp, SCATTER_ROWS);
        let row = nudge(&taken, plot.width, x - plot.x, ideal, SCATTER_ROWS);
        taken[row as usize * plot.width as usize + (x - plot.x) as usize] = true;
        let (glyph, color) = match (
            mark.event.transaction_code.as_deref(),
            mark.event.side.as_deref(),
        ) {
            (Some("D"), _) => ('▽', theme.neg),
            (_, Some("buy")) => ('▲', theme.pos),
            (_, Some("sell")) => ('▼', theme.neg),
            _ => ('·', theme.flat),
        };
        let mut style = Style::new().fg(color);
        if mark.event.is_10b5_1 {
            style = style.add_modifier(Modifier::DIM);
        }
        if selected(mark.event) {
            style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        if let Some(cell) = buf.cell_mut((x, plot.y + row)) {
            cell.set_char(glyph).set_style(style);
        }
    }

    // Weekly bars under the scatter, sharing its x mapping.
    if with_bars {
        let bars = Rect { y: plot.y + SCATTER_ROWS, height: BARS_ROWS, ..plot };
        render_bars(buf, bars, trades, start, today, gutter, &col, theme);
    }

    // Month ticks along the bottom row, thinned to whatever fits.
    let axis_y = inner.y + inner.height - 1;
    let mut last_end = 0u16;
    for m in month_starts(start, today) {
        let text = if m.month() == 1 {
            m.format("%b '%y").to_string()
        } else {
            m.format("%b").to_string()
        };
        let x = col(m);
        let len = text.chars().count() as u16;
        if x + len > plot.x + plot.width || (last_end > 0 && x < last_end + 2) {
            continue;
        }
        buf.set_string(x, axis_y, &text, dim);
        last_end = x + len;
    }
}

/// Panel title: totals of the window's events (the full picture, not the
/// score-filtered list).
fn title(events: &[&TradeEvent], label: &str, theme: &Theme) -> Line<'static> {
    let mut buys = 0.0;
    let mut sells = 0.0;
    let mut plans = 0usize;
    for e in events {
        let v = e.total_value_usd.as_deref().and_then(usd).unwrap_or(0.0);
        match e.side.as_deref() {
            Some("buy") => buys += v,
            _ => sells += v,
        }
        if e.is_10b5_1 {
            plans += 1;
        }
    }
    let mut spans = vec![Span::styled(
        format!(" Form 4 · {label} "),
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    )];
    if buys > 0.0 {
        spans.push(Span::raw("· ").dim());
        spans.push(Span::styled(
            format!("▲ {} ", fmt_usd(&buys.to_string())),
            Style::new().fg(theme.pos),
        ));
    }
    if sells > 0.0 {
        spans.push(Span::raw("· ").dim());
        spans.push(Span::styled(
            format!("▼ {} ", fmt_usd(&sells.to_string())),
            Style::new().fg(theme.neg),
        ));
    }
    if !events.is_empty() {
        let pct = (plans * 100 + events.len() / 2) / events.len();
        spans.push(Span::styled(
            format!("· {} events · {pct}% plan ", events.len()),
            Style::new().dim(),
        ));
    }
    Line::from(spans)
}

/// The weekly buy/sell bars: two-sided from an implicit zero line between
/// the halves when the window has both sides, otherwise the full height
/// grows upward (the color already names the side).
#[allow(clippy::too_many_arguments)]
fn render_bars(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    trades: &InsiderTrades,
    start: NaiveDate,
    today: NaiveDate,
    gutter: u16,
    col: &dyn Fn(NaiveDate) -> u16,
    theme: &Theme,
) {
    struct Bar {
        c0: u16,
        c1: u16,
        buy: f64,
        sell: f64,
        buy_stub: bool,
        sell_stub: bool,
    }
    let mut bars: Vec<Bar> = Vec::new();
    for b in &trades.series_weekly {
        let Some(week) = day(&b.week_start) else { continue };
        let week_end = week + Days::new(6);
        if week_end < start || week > today {
            continue;
        }
        let buy = b.buy_value_usd.as_deref().and_then(usd).unwrap_or(0.0);
        let sell = b.sell_value_usd.as_deref().and_then(usd).unwrap_or(0.0);
        let c0 = col(week.max(start));
        let mut c1 = col(week_end.min(today));
        if c1 > c0 {
            c1 -= 1; // gap column so neighbouring weeks never fuse
        }
        bars.push(Bar {
            c0,
            c1,
            buy,
            sell,
            buy_stub: b.buy_count > 0,
            sell_stub: b.sell_count > 0,
        });
    }
    let vmax = bars.iter().fold(0.0_f64, |m, b| m.max(b.buy).max(b.sell));
    if bars.is_empty() || (vmax <= 0.0 && !bars.iter().any(|b| b.buy_stub || b.sell_stub)) {
        return;
    }
    let two_sided = bars.iter().any(|b| b.buy > 0.0 || b.buy_stub)
        && bars.iter().any(|b| b.sell > 0.0 || b.sell_stub);

    // Subrows a value fills on its side; a traded-but-unpriced week keeps a
    // one-subrow stub so activity never disappears (the web draws 2px).
    let filled = |v: f64, stub: bool, sub_rows: usize| -> usize {
        if vmax <= 0.0 {
            return usize::from(stub);
        }
        let f = ((v / vmax) * sub_rows as f64).round() as usize;
        if v > 0.0 || stub { f.clamp(1, sub_rows) } else { 0 }
    };
    // Rows filled from the bottom up ('▄' half-step), like the volume panel.
    let up = |buf: &mut ratatui::buffer::Buffer, x0: u16, x1: u16, y: u16, h: u16, f: usize, color| {
        for row in 0..h {
            let lower = (h - 1 - row) as usize * 2;
            let ch = match f.saturating_sub(lower) {
                0 => continue,
                1 => '▄',
                _ => '█',
            };
            for x in x0..=x1 {
                if let Some(cell) = buf.cell_mut((x, y + row)) {
                    cell.set_char(ch).set_fg(color);
                }
            }
        }
    };
    // And hanging from the zero line down ('▀' half-step) for the sell half.
    let down = |buf: &mut ratatui::buffer::Buffer, x0: u16, x1: u16, y: u16, h: u16, f: usize, color| {
        for row in 0..h {
            let above = row as usize * 2;
            let ch = match f.saturating_sub(above) {
                0 => continue,
                1 => '▀',
                _ => '█',
            };
            for x in x0..=x1 {
                if let Some(cell) = buf.cell_mut((x, y + row)) {
                    cell.set_char(ch).set_fg(color);
                }
            }
        }
    };

    let half = area.height / 2;
    for b in &bars {
        if two_sided {
            let sub = half as usize * 2;
            up(buf, b.c0, b.c1, area.y, half, filled(b.buy, b.buy_stub, sub), theme.pos);
            down(
                buf,
                b.c0,
                b.c1,
                area.y + half,
                area.height - half,
                filled(b.sell, b.sell_stub, (area.height - half) as usize * 2),
                theme.neg,
            );
        } else {
            let sub = area.height as usize * 2;
            let (v, stub, color) = if b.sell > 0.0 || b.sell_stub {
                (b.sell, b.sell_stub, theme.neg)
            } else {
                (b.buy, b.buy_stub, theme.pos)
            };
            up(buf, b.c0, b.c1, area.y, area.height, filled(v, stub, sub), color);
        }
    }

    // Peak dollar value right-aligned in the gutter, like the volume peak.
    if vmax > 0.0 {
        let text = fmt_usd(&vmax.to_string());
        let len = text.chars().count() as u16;
        if len < gutter {
            buf.set_string(area.x - 1 - len, area.y, &text, Style::new().dim());
        }
    }
}

/// "YYYY-MM-DD" -> date; anything else is None (tolerant like the rest of
/// the API surface).
fn day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// API decimal string -> positive dollars (magnitudes; signs live in `side`).
fn usd(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().map(f64::abs)
}

/// Log domain in whole decades over the priced marks, padded so a flat
/// window still spans a decade; (3, 7) — $1K to $10M — when nothing priced.
fn log_domain(marks: &[Mark]) -> (i32, i32) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for m in marks {
        lo = lo.min(m.value);
        hi = hi.max(m.value);
    }
    if !lo.is_finite() {
        return (3, 7);
    }
    let lo_exp = lo.max(1.0).log10().floor() as i32;
    let mut hi_exp = hi.max(1.0).log10().ceil() as i32;
    if hi_exp <= lo_exp {
        hi_exp = lo_exp + 1;
    }
    (lo_exp, hi_exp)
}

/// Scatter row of a dollar value on the log scale, 0 = top.
fn value_row(value: f64, lo_exp: i32, hi_exp: i32, rows: u16) -> u16 {
    let frac = (value.max(1.0).log10() - lo_exp as f64) / (hi_exp - lo_exp) as f64;
    let row = ((1.0 - frac.clamp(0.0, 1.0)) * (rows - 1) as f64).round() as u16;
    row.min(rows - 1)
}

/// A mark keeps its value row when free, else the nearest free row in its
/// column (up first — stacks read like the web's clusters); a full column
/// falls back to the ideal row and overwrites.
fn nudge(taken: &[bool], width: u16, x: u16, ideal: u16, rows: u16) -> u16 {
    let free = |r: u16| !taken[r as usize * width as usize + x as usize];
    if free(ideal) {
        return ideal;
    }
    for d in 1..rows {
        if ideal >= d && free(ideal - d) {
            return ideal - d;
        }
        if ideal + d < rows && free(ideal + d) {
            return ideal + d;
        }
    }
    ideal
}

/// "$10K" / "$1M" / "$100M" for a whole decade.
fn decade_label(exp: i32) -> String {
    let (div, suffix) = match exp {
        ..=2 => (0, ""),
        3..=5 => (3, "K"),
        6..=8 => (6, "M"),
        _ => (9, "B"),
    };
    format!("${}{suffix}", 10f64.powi(exp - div) as i64)
}

/// First days of months inside [start, end].
fn month_starts(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut m = NaiveDate::from_ymd_opt(start.year(), start.month(), 1).unwrap();
    if m < start {
        m = next_month(m);
    }
    while m <= end {
        out.push(m);
        m = next_month(m);
    }
    out
}

fn next_month(m: NaiveDate) -> NaiveDate {
    let (y, mo) = if m.month() == 12 {
        (m.year() + 1, 1)
    } else {
        (m.year(), m.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, mo, 1).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        day(s).unwrap()
    }

    #[test]
    fn panel_degrades_bars_first() {
        assert_eq!(panel_height(30), FULL_HEIGHT);
        assert_eq!(panel_height(FULL_HEIGHT), FULL_HEIGHT);
        assert_eq!(panel_height(FULL_HEIGHT - 1), SHORT_HEIGHT);
        assert_eq!(panel_height(SHORT_HEIGHT), SHORT_HEIGHT);
        assert_eq!(panel_height(SHORT_HEIGHT - 1), 0);
    }

    #[test]
    fn log_domain_pads_to_whole_decades() {
        let mark = |v: f64| Mark {
            date: d("2026-08-01"),
            value: v,
            event: Box::leak(Box::default()),
        };
        // $429K..$18.4M -> $100K..$100M (5..8).
        assert_eq!(log_domain(&[mark(429_000.0), mark(18_400_000.0)]), (5, 8));
        // A flat window still spans one decade.
        assert_eq!(log_domain(&[mark(5_000_000.0)]), (6, 7));
        assert_eq!(log_domain(&[]), (3, 7));
    }

    #[test]
    fn value_rows_span_the_scale_top_down() {
        // Domain $100K..$100M over 5 rows: top row = biggest.
        assert_eq!(value_row(100_000_000.0, 5, 8, 5), 0);
        assert_eq!(value_row(100_000.0, 5, 8, 5), 4);
        assert_eq!(value_row(10_000_000.0, 5, 8, 5), 1);
        // Out-of-domain values clamp instead of escaping the plot.
        assert_eq!(value_row(1.0, 5, 8, 5), 4);
        assert_eq!(value_row(1e12, 5, 8, 5), 0);
    }

    #[test]
    fn nudge_stacks_collisions_upward_first() {
        let width = 3u16;
        let mut taken = vec![false; 3 * 5];
        assert_eq!(nudge(&taken, width, 1, 2, 5), 2);
        taken[2 * 3 + 1] = true;
        assert_eq!(nudge(&taken, width, 1, 2, 5), 1);
        taken[3 + 1] = true;
        assert_eq!(nudge(&taken, width, 1, 2, 5), 3);
        // A full column falls back to the ideal row.
        for r in 0..5 {
            taken[r * 3 + 1] = true;
        }
        assert_eq!(nudge(&taken, width, 1, 2, 5), 2);
        // Other columns are unaffected.
        assert_eq!(nudge(&taken, width, 0, 2, 5), 2);
    }

    #[test]
    fn decade_labels_use_money_bands() {
        assert_eq!(decade_label(3), "$1K");
        assert_eq!(decade_label(5), "$100K");
        assert_eq!(decade_label(6), "$1M");
        assert_eq!(decade_label(8), "$100M");
        assert_eq!(decade_label(9), "$1B");
        assert_eq!(decade_label(10), "$10B");
    }

    #[test]
    fn month_starts_cover_the_window() {
        assert_eq!(
            month_starts(d("2026-05-13"), d("2026-08-11")),
            vec![d("2026-06-01"), d("2026-07-01"), d("2026-08-01")]
        );
        // A window starting on the 1st keeps that month.
        assert_eq!(
            month_starts(d("2026-08-01"), d("2026-08-11")),
            vec![d("2026-08-01")]
        );
        // December wraps the year.
        assert_eq!(
            month_starts(d("2026-12-05"), d("2027-01-20")),
            vec![d("2027-01-01")]
        );
    }
}
