use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph};

use crate::app::{App, ChartStyle};
use crate::domain::{Candle, Quote, Range, TickerData, fmt_price};
use crate::indicators::{self, SMA_FAST, SMA_SLOW};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, View, ViewId};

pub struct ChartView;

impl View for ChartView {
    fn id(&self) -> ViewId {
        ViewId::Chart
    }

    fn title(&self) -> &'static str {
        "Chart"
    }

    fn hints(&self) -> &'static [Hint] {
        const HINTS: &[Hint] = &[
            Hint::act(&[Action::Quit], "quit"),
            Hint::fixed("tab/1-9", "view"),
            Hint::act(&[Action::Up, Action::Down], "select"),
            Hint::act(&[Action::ChartStyle], "style"),
            Hint::act(&[Action::ToggleSma], "sma"),
            Hint::act(&[Action::ToggleRsi], "rsi"),
            Hint::act(&[Action::NextPreset], "interval"),
            Hint::act(&[Action::Refresh], "refresh"),
            Hint::act(&[Action::Settings], "settings"),
        ];
        HINTS
    }

    fn has_chart_panel(&self) -> bool {
        true
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        render_chart(f, area, app);
    }
}

const RSI_PERIOD: usize = 14;
const RSI_PANEL_HEIGHT: u16 = 8;
/// Below this total height the RSI panel is dropped so the price chart keeps
/// usable space (same graceful degradation as the split view's news half).
const RSI_MIN_CHART_HEIGHT: u16 = 20;

/// Price chart of the selected symbol: candlesticks by default, the classic
/// close line via the `c` toggle, optional SMA 20/100 overlays (`m`) and an
/// RSI(14) panel (`i`). Shared by ChartView and SplitView.
pub fn render_chart(f: &mut Frame, area: Rect, app: &App) {
    let symbol = app.selected_symbol().to_string();

    let Some(data) = app.data.get(&symbol) else {
        let msg = match app.errors.get(&symbol) {
            Some(e) => {
                Line::from(format!("{symbol}: {e}")).style(Style::new().fg(app.theme.error))
            }
            None => Line::from(format!("{symbol}: loading…")).dim(),
        };
        f.render_widget(
            Paragraph::new(msg).block(Block::bordered().title(format!(" {symbol} "))),
            area,
        );
        return;
    };
    if data.candles.len() < 2 {
        f.render_widget(
            Paragraph::new(Line::from("not enough history for a chart").dim())
                .block(Block::bordered().title(format!(" {symbol} "))),
            area,
        );
        return;
    }

    let (price_area, rsi_area) = if app.show_rsi && area.height >= RSI_MIN_CHART_HEIGHT {
        let [p, r] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(RSI_PANEL_HEIGHT)])
                .areas(area);
        (p, Some(r))
    } else {
        (area, None)
    };

    let cut = visible_from(&data.candles, app.range);
    match app.chart_style {
        ChartStyle::Line => render_price_line(f, price_area, app, &symbol, data, cut),
        ChartStyle::Candles => render_price_candles(f, price_area, app, &symbol, data, cut),
    }
    if let Some(r) = rsi_area {
        render_rsi(f, r, data, cut, &app.theme);
    }
}

/// Index of the first candle inside the visible window. Sources fetch extra
/// history for indicator warm-up (`domain::fetch_range`); the chart renders
/// only the trailing `range` worth, anchored on the newest candle so a
/// closed market still shows the last session. Indicators keep the full
/// series and are sliced with the same offset.
fn visible_from(candles: &[Candle], range: Range) -> usize {
    let Some(last) = candles.last() else { return 0 };
    let cutoff = last.ts - range.secs();
    let cut = candles.partition_point(|c| c.ts <= cutoff);
    // Never trim below a drawable pair of candles.
    cut.min(candles.len().saturating_sub(2))
}

fn dir_color(q: &Quote, theme: &Theme) -> Color {
    match q.change() {
        Some(c) if c < 0.0 => theme.down,
        Some(_) => theme.up,
        None => theme.flat,
    }
}

/// Legend labels appear only for SMA lines that actually have points on
/// screen: an SMA needs `period` candles of history, which short series
/// (finnhub's growing synthetic one, thin symbols) may not have yet.
fn chart_title(
    symbol: &str,
    q: &Quote,
    data: &TickerData,
    show_sma: bool,
    theme: &Theme,
) -> Line<'static> {
    let change_str = match (q.change(), q.change_pct()) {
        (Some(c), Some(p)) => format!("{c:+.2} ({p:+.2}%)"),
        _ => "—".into(),
    };
    let mut spans = vec![
        Span::styled(format!(" {symbol} "), Style::new().bold()),
        Span::raw(format!(
            "{} {} ",
            fmt_price(q.price),
            q.currency.as_deref().unwrap_or("")
        )),
        Span::styled(format!("{change_str} "), Style::new().fg(dir_color(q, theme))),
    ];
    if show_sma {
        for (period, color) in [(SMA_FAST, theme.sma_fast), (SMA_SLOW, theme.sma_slow)] {
            if data.candles.len() >= period {
                spans.push(Span::styled(format!("SMA{period} "), Style::new().fg(color)));
            }
        }
    }
    Line::from(spans)
}

/// Clock labels inside a ~day, dates beyond: "19:00" is ambiguous once the
/// window spans several days (e.g. the 1mo/60m preset).
fn axis_time_fmt(first_ts: i64, last_ts: i64) -> &'static str {
    if last_ts - first_ts <= 2 * 86_400 { "%H:%M" } else { "%d %b" }
}

fn time_label(ts: i64, fmt: &str) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&Local).format(fmt).to_string())
        .unwrap_or_default()
}

// -- line mode --------------------------------------------------------------

fn render_price_line(
    f: &mut Frame,
    area: Rect,
    app: &App,
    symbol: &str,
    data: &TickerData,
    cut: usize,
) {
    let q = &data.quote;
    let visible = &data.candles[cut..];
    let points: Vec<(f64, f64)> = visible
        .iter()
        .enumerate()
        .map(|(i, c)| (i as f64, c.close))
        .collect();

    let (mut lo, mut hi) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(_, y)| {
            (lo.min(y), hi.max(y))
        });
    // Keep the previous close visible: it is the natural reference line.
    if let Some(pc) = q.prev_close {
        lo = lo.min(pc);
        hi = hi.max(pc);
    }
    let pad = ((hi - lo) * 0.05).max(hi.abs() * 0.0005).max(1e-9);
    let (y_lo, y_hi) = (lo - pad, hi + pad);
    let x_hi = (points.len() - 1) as f64;

    let prev_close_points: Vec<(f64, f64)> = q
        .prev_close
        .map(|pc| vec![(0.0, pc), (x_hi, pc)])
        .unwrap_or_default();

    // Indicators run over the full series (warm-up included), then shift to
    // the visible window's x coordinates.
    let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
    let sma_points = |period: usize| -> Vec<(f64, f64)> {
        indicators::sma(&closes, period)
            .into_iter()
            .enumerate()
            .skip(cut)
            .filter_map(|(i, v)| v.map(|v| ((i - cut) as f64, v)))
            .collect()
    };
    let (sma_fast, sma_slow) = if app.show_sma {
        (sma_points(SMA_FAST), sma_points(SMA_SLOW))
    } else {
        (Vec::new(), Vec::new())
    };

    let mut datasets = Vec::new();
    if !prev_close_points.is_empty() {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Dot)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(app.theme.ref_line))
                .data(&prev_close_points),
        );
    }
    for (pts, color) in [(&sma_slow, app.theme.sma_slow), (&sma_fast, app.theme.sma_fast)] {
        if !pts.is_empty() {
            datasets.push(
                Dataset::default()
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::new().fg(color))
                    .data(pts),
            );
        }
    }
    datasets.push(
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(dir_color(q, &app.theme)))
            .data(&points),
    );

    let fmt = axis_time_fmt(visible[0].ts, visible.last().unwrap().ts);
    let mid = visible.len() / 2;
    let x_labels = vec![
        time_label(visible[0].ts, fmt),
        time_label(visible[mid].ts, fmt),
        time_label(visible.last().unwrap().ts, fmt),
    ];
    let y_labels = vec![
        fmt_price(y_lo),
        fmt_price((y_lo + y_hi) / 2.0),
        fmt_price(y_hi),
    ];

    let chart = Chart::new(datasets)
        .block(Block::bordered().title(chart_title(symbol, q, data, app.show_sma, &app.theme)))
        .x_axis(
            Axis::default()
                .bounds([0.0, x_hi])
                .labels(x_labels)
                .style(Style::new().dim()),
        )
        .y_axis(
            Axis::default()
                .bounds([y_lo, y_hi])
                .labels(y_labels)
                .style(Style::new().dim()),
        );
    f.render_widget(chart, area);
}

// -- candle mode --------------------------------------------------------------

/// Hand-rolled candlestick renderer writing straight into the buffer at
/// half-block resolution: two subrows per terminal row, body `█ ▀ ▄`, wick
/// `│ ╵ ╷`. ratatui's Chart widget has no candle graph type.
fn render_price_candles(
    f: &mut Frame,
    area: Rect,
    app: &App,
    symbol: &str,
    data: &TickerData,
    cut: usize,
) {
    let q = &data.quote;
    let visible = &data.candles[cut..];
    let block = Block::bordered().title(chart_title(symbol, q, data, app.show_sma, &app.theme));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Aggregation preserves the visible low/high, so the y-range can be
    // folded over the raw visible candles before the downsampling decision.
    let (mut lo, mut hi) = visible
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| {
            (lo.min(c.low), hi.max(c.high))
        });
    if let Some(pc) = q.prev_close {
        lo = lo.min(pc);
        hi = hi.max(pc);
    }
    let pad = ((hi - lo) * 0.05).max(hi.abs() * 0.0005).max(1e-9);
    let (y_lo, y_hi) = (lo - pad, hi + pad);

    let y_labels = [fmt_price(y_hi), fmt_price((y_lo + y_hi) / 2.0), fmt_price(y_lo)];
    let gutter = y_labels.iter().map(|s| s.chars().count()).max().unwrap() as u16 + 1;
    if inner.width <= gutter + 2 || inner.height <= 2 {
        return; // too small: leave the bare block
    }
    let plot = Rect {
        x: inner.x + gutter,
        y: inner.y,
        width: inner.width - gutter,
        height: inner.height - 1, // bottom row = time axis
    };

    // Every candle gets a slot of body + 1 column of gap so neighbours never
    // fuse into a solid mass; history beyond width/2 candles aggregates into
    // even buckets.
    let max_cols = plot.width as usize;
    let max_candles = (max_cols / 2).max(1);
    let (display, sample_idx): (Vec<Candle>, Vec<usize>) = if visible.len() > max_candles {
        let ranges = bucket_ranges(visible.len(), max_candles);
        (
            ranges.iter().map(|r| aggregate(&visible[r.clone()])).collect(),
            ranges.iter().map(|r| r.end - 1).collect(),
        )
    } else {
        (visible.to_vec(), (0..visible.len()).collect())
    };
    // With few candles widen each one instead of leaving the plot empty:
    // body up to 3 columns.
    let n = display.len();
    let slot = (max_cols / n).clamp(2, 4) as u16;
    let body_w = slot - 1;
    let slot_x = |i: usize| plot.x + plot.width - (n - i) as u16 * slot;

    let buf = f.buffer_mut();

    // Previous-close reference first; candles draw over it.
    if let Some(pc) = q.prev_close {
        let row = (scale(pc, y_lo, y_hi, plot.height as usize * 2) / 2) as u16;
        for x in (plot.x..plot.x + plot.width).step_by(2) {
            if let Some(cell) = buf.cell_mut((x, plot.y + row)) {
                cell.set_char('╌').set_fg(app.theme.ref_line);
            }
        }
    }

    for (i, c) in display.iter().enumerate() {
        let prev_close = (i > 0).then(|| display[i - 1].close);
        let color = candle_color(c, prev_close, &app.theme);
        let body_x = slot_x(i) + (slot - body_w);
        let wick_x = body_x + body_w / 2;
        for (row, ch) in candle_column(c, y_lo, y_hi, plot.height) {
            for x in body_x..body_x + body_w {
                // Wick glyphs only in the center column; the rest of the body
                // width shows body halves alone.
                let ch = if x == wick_x { ch } else { body_only(ch) };
                if ch == ' ' {
                    continue;
                }
                if let Some(cell) = buf.cell_mut((x, plot.y + row)) {
                    cell.set_char(ch).set_fg(color);
                }
            }
        }
    }

    // SMA overlay: one dot per column, threading between candles (bodies win
    // shared cells). Slow first so the fast line wins where they cross.
    // Computed over the full series; `sample_idx` entries index into
    // `visible`, hence the `cut` offset. Values pushed outside the visible
    // y-range by warm-up history are skipped, not pinned to the edge.
    if app.show_sma {
        let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
        for (period, color) in [(SMA_SLOW, app.theme.sma_slow), (SMA_FAST, app.theme.sma_fast)] {
            let line = indicators::sma(&closes, period);
            for (i, &raw) in sample_idx.iter().enumerate() {
                let Some(v) = line[cut + raw] else { continue };
                if v < y_lo || v > y_hi {
                    continue;
                }
                let row = (scale(v, y_lo, y_hi, plot.height as usize * 2) / 2) as u16;
                let x = slot_x(i) + (slot - body_w) + body_w / 2;
                if let Some(cell) = buf.cell_mut((x, plot.y + row))
                    && matches!(cell.symbol(), " " | "│" | "╵" | "╷" | "╌" | "·")
                {
                    cell.set_char('·').set_fg(color);
                }
            }
        }
    }

    // Axes: price labels right-aligned in the gutter, three time labels below.
    let dim = Style::new().dim();
    let label_ys = [plot.y, plot.y + plot.height / 2, plot.y + plot.height - 1];
    for (label, y) in y_labels.iter().zip(label_ys) {
        let x = plot.x - 1 - label.chars().count() as u16;
        buf.set_string(x, y, label, dim);
    }
    let axis_y = plot.y + plot.height;
    let fmt = axis_time_fmt(display[0].ts, display[n - 1].ts);
    let first = time_label(display[0].ts, fmt);
    let last = time_label(display[n - 1].ts, fmt);
    buf.set_string(plot.x, axis_y, &first, dim);
    let last_x = plot.x + plot.width - last.chars().count() as u16;
    buf.set_string(last_x, axis_y, &last, dim);
    if plot.width >= 30 {
        let mid = time_label(display[n / 2].ts, fmt);
        let mid_x = plot.x + (plot.width - mid.chars().count() as u16) / 2;
        buf.set_string(mid_x, axis_y, &mid, dim);
    }
}

/// Price -> subrow index in `[0, sub_rows)`, 0 = top.
fn scale(v: f64, y_lo: f64, y_hi: f64, sub_rows: usize) -> usize {
    (((y_hi - v) / (y_hi - y_lo) * sub_rows as f64) as usize).min(sub_rows - 1)
}

#[derive(Clone, Copy, PartialEq)]
enum Half {
    Body,
    Wick,
    Empty,
}

/// The glyphs of one candle's wick column, as (row offset, char) pairs.
/// Each terminal row covers subrows 2r and 2r+1; the body spans
/// [scale(max(o,c)), scale(min(o,c))] inclusive, so a doji still occupies
/// one subrow and every candle stays visible.
fn candle_column(c: &Candle, y_lo: f64, y_hi: f64, rows: u16) -> Vec<(u16, char)> {
    let sub_rows = rows as usize * 2;
    let body_top = scale(c.open.max(c.close), y_lo, y_hi, sub_rows);
    let body_bot = scale(c.open.min(c.close), y_lo, y_hi, sub_rows);
    let wick_top = scale(c.high, y_lo, y_hi, sub_rows);
    let wick_bot = scale(c.low, y_lo, y_hi, sub_rows);
    let half = |sub: usize| {
        if (body_top..=body_bot).contains(&sub) {
            Half::Body
        } else if (wick_top..=wick_bot).contains(&sub) {
            Half::Wick
        } else {
            Half::Empty
        }
    };
    (0..rows)
        .filter_map(|row| {
            let ch = match (half(row as usize * 2), half(row as usize * 2 + 1)) {
                (Half::Body, Half::Body) => '█',
                (Half::Body, _) => '▀',
                (_, Half::Body) => '▄',
                (Half::Wick, Half::Wick) => '│',
                (Half::Wick, Half::Empty) => '╵',
                (Half::Empty, Half::Wick) => '╷',
                (Half::Empty, Half::Empty) => return None,
            };
            Some((row, ch))
        })
        .collect()
}

/// Body columns beside the wick column keep only the body halves.
fn body_only(ch: char) -> char {
    match ch {
        '█' | '▀' | '▄' => ch,
        _ => ' ',
    }
}

/// Finnhub synthesizes flat o=h=l=c candles, so a doji falls back to the
/// direction against the previous candle's close.
fn candle_color(c: &Candle, prev_close: Option<f64>, theme: &Theme) -> Color {
    if c.close > c.open {
        theme.up
    } else if c.close < c.open {
        theme.down
    } else {
        match prev_close {
            Some(p) if c.close < p => theme.down,
            Some(_) => theme.up,
            None => theme.flat,
        }
    }
}

/// Candles spread evenly over the columns (bucket sizes differ by at most
/// one), so the plot always fills its full width and the newest candle ends
/// the last bucket.
fn bucket_ranges(len: usize, max_cols: usize) -> Vec<std::ops::Range<usize>> {
    let cols = max_cols.min(len);
    (0..cols)
        .map(|i| (i * len / cols)..((i + 1) * len / cols))
        .collect()
}

fn aggregate(chunk: &[Candle]) -> Candle {
    let mut volume = None;
    let (mut high, mut low) = (f64::NEG_INFINITY, f64::INFINITY);
    for c in chunk {
        high = high.max(c.high);
        low = low.min(c.low);
        if let Some(v) = c.volume {
            volume = Some(volume.unwrap_or(0.0) + v);
        }
    }
    Candle {
        ts: chunk[0].ts,
        open: chunk[0].open,
        high,
        low,
        close: chunk[chunk.len() - 1].close,
        volume,
    }
}

// -- RSI panel ----------------------------------------------------------------

fn render_rsi(f: &mut Frame, area: Rect, data: &TickerData, cut: usize, theme: &Theme) {
    let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
    let rsi = indicators::rsi(&closes, RSI_PERIOD);
    let Some(last) = rsi.last().copied().flatten() else {
        f.render_widget(
            Paragraph::new(
                Line::from(format!("not enough history for RSI({RSI_PERIOD})")).dim(),
            )
            .block(Block::bordered().title(format!(" RSI({RSI_PERIOD}) "))),
            area,
        );
        return;
    };

    // Same visible-window slicing as the price chart above it.
    let x_hi = (closes.len() - 1 - cut) as f64;
    let ref30 = [(0.0, 30.0), (x_hi, 30.0)];
    let ref70 = [(0.0, 70.0), (x_hi, 70.0)];
    let points: Vec<(f64, f64)> = rsi
        .iter()
        .enumerate()
        .skip(cut)
        .filter_map(|(i, v)| v.map(|v| ((i - cut) as f64, v)))
        .collect();

    let mut datasets = Vec::new();
    for refline in [&ref30[..], &ref70[..]] {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Dot)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(theme.ref_line))
                .data(refline),
        );
    }
    datasets.push(
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(theme.rsi_line))
            .data(&points),
    );

    // Overbought reads bearish, oversold bullish; between them, neutral.
    let val_color = if last >= 70.0 {
        theme.down
    } else if last <= 30.0 {
        theme.up
    } else {
        theme.flat
    };
    let title = Line::from(vec![
        Span::styled(format!(" RSI({RSI_PERIOD}) "), Style::new().bold()),
        Span::styled(format!("{last:.1} "), Style::new().fg(val_color)),
    ]);
    let chart = Chart::new(datasets)
        .block(Block::bordered().title(title))
        .x_axis(Axis::default().bounds([0.0, x_hi]))
        .y_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(["0", "50", "100"])
                .style(Style::new().dim()),
        );
    f.render_widget(chart, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle { ts: 0, open, high, low, close, volume: None }
    }

    fn flat_series(n: usize, step_secs: i64) -> Vec<Candle> {
        (0..n)
            .map(|i| {
                let mut c = candle(1.0, 1.0, 1.0, 1.0);
                c.ts = i as i64 * step_secs;
                c
            })
            .collect()
    }

    #[test]
    fn visible_from_trims_to_the_trailing_range() {
        // 3 days of gapless 5m candles, 1d window: only the last day shows.
        let candles = flat_series(3 * 288, 300);
        assert_eq!(visible_from(&candles, Range::D1), 2 * 288);
        // Everything fits into the window: nothing to trim.
        assert_eq!(visible_from(&candles[..10], Range::D1), 0);
        assert_eq!(visible_from(&[], Range::D1), 0);
    }

    #[test]
    fn visible_from_keeps_a_drawable_pair() {
        // Candles sparser than the window (10 days apart, 1d range): the
        // window alone would leave a single candle.
        let candles = flat_series(5, 10 * 86_400);
        assert_eq!(visible_from(&candles, Range::D1), 3);
    }

    #[test]
    fn candle_column_worked_example() {
        let cols = candle_column(&candle(2.0, 9.0, 1.0, 6.0), 0.0, 10.0, 5);
        assert_eq!(cols, vec![(0, '╷'), (1, '│'), (2, '█'), (3, '█'), (4, '▀')]);
    }

    #[test]
    fn doji_is_always_visible() {
        let cols = candle_column(&candle(5.0, 5.0, 5.0, 5.0), 0.0, 10.0, 5);
        assert_eq!(cols.len(), 1);
        assert!(matches!(cols[0].1, '▀' | '▄'));
    }

    #[test]
    fn bucket_ranges_fill_all_columns() {
        assert_eq!(bucket_ranges(10, 4), vec![0..2, 2..5, 5..7, 7..10]);
        assert_eq!(bucket_ranges(130, 108).len(), 108);
        // Fewer candles than columns: identity buckets.
        assert_eq!(bucket_ranges(5, 10), (0..5).map(|i| i..i + 1).collect::<Vec<_>>());
    }

    #[test]
    fn aggregate_merges_ohlcv() {
        let mut a = candle(10.0, 12.0, 9.0, 11.0);
        a.volume = Some(100.0);
        let mut b = candle(11.0, 15.0, 10.5, 14.0);
        b.volume = Some(50.0);
        let m = aggregate(&[a, b]);
        assert_eq!((m.open, m.high, m.low, m.close), (10.0, 15.0, 9.0, 14.0));
        assert_eq!(m.volume, Some(150.0));

        let m = aggregate(&[candle(1.0, 2.0, 0.5, 1.5)]);
        assert_eq!(m.volume, None);
    }

    #[test]
    fn candle_colors() {
        let t = Theme::default();
        assert_eq!(candle_color(&candle(1.0, 2.0, 1.0, 2.0), None, &t), t.up);
        assert_eq!(candle_color(&candle(2.0, 2.0, 1.0, 1.0), None, &t), t.down);
        // Doji: direction against the previous close, flat without one.
        let doji = candle(5.0, 5.0, 5.0, 5.0);
        assert_eq!(candle_color(&doji, Some(6.0), &t), t.down);
        assert_eq!(candle_color(&doji, Some(4.0), &t), t.up);
        assert_eq!(candle_color(&doji, None, &t), t.flat);
    }
}
