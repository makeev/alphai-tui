use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph};

use crate::app::{App, ChartStyle};
use crate::config::ChartDefaults;
use crate::domain::{Candle, Quote, Range, TickerData, fmt_price};
use crate::indicators;
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
            Hint::act(&[Action::Help], "help"),
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
            Paragraph::new(msg).block(app.theme.panel_titled(format!(" {symbol} "))),
            area,
        );
        return;
    };
    if data.candles.len() < 2 {
        f.render_widget(
            Paragraph::new(Line::from("not enough history for a chart").dim())
                .block(app.theme.panel_titled(format!(" {symbol} "))),
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
        render_rsi(f, r, data, cut, &app.chart, &app.theme);
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

/// Columns of the plot kept free right of the newest candle (`[chart]
/// right_margin_pct`). The candle zone always keeps at least 2 columns.
fn margin_cols(width: u16, pct: u16) -> u16 {
    (width as u32 * pct as u32 / 100).min(width.saturating_sub(2) as u32) as u16
}

/// The x-axis upper bound stretched so the data occupies (100-pct)% of the
/// width and the rest stays free on the right.
fn x_with_margin(x_hi: f64, pct: u16) -> f64 {
    if pct == 0 { x_hi } else { x_hi * 100.0 / (100.0 - pct as f64) }
}

/// Timestamp at fractional candle index `x`; past the last candle it
/// extrapolates with the average spacing, so labels under the right margin
/// read as the near future instead of repeating the last candle.
fn time_at(candles: &[Candle], x: f64) -> i64 {
    let last = candles.len() - 1;
    if x <= last as f64 {
        return candles[(x.round().max(0.0) as usize).min(last)].ts;
    }
    let avg = if last > 0 {
        (candles[last].ts - candles[0].ts) as f64 / last as f64
    } else {
        0.0
    };
    candles[last].ts + ((x - last as f64) * avg) as i64
}

/// Style of the last-price value while its update pulse is active: the tick
/// direction's color, inverted so it visibly blinks.
fn flash_style(up: bool, theme: &Theme) -> Style {
    Style::new()
        .fg(if up { theme.up } else { theme.down })
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
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
    chart: &ChartDefaults,
    theme: &Theme,
    flash: Option<bool>,
) -> Line<'static> {
    let change_str = match (q.change(), q.change_pct()) {
        (Some(c), Some(p)) => format!("{c:+.2} ({p:+.2}%)"),
        _ => "—".into(),
    };
    // The price pulses in the tick's color right after an update, so a
    // live market is visible at a glance even in line mode.
    let price_style = flash.map_or(Style::new(), |up| flash_style(up, theme));
    let mut spans = vec![
        Span::styled(
            format!(" {symbol} "),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(fmt_price(q.price), price_style),
        Span::raw(format!(" {} ", q.currency.as_deref().unwrap_or(""))),
        Span::styled(format!("{change_str} "), Style::new().fg(dir_color(q, theme))),
    ];
    if show_sma {
        for (period, color) in [(chart.sma_fast, theme.sma_fast), (chart.sma_slow, theme.sma_slow)]
        {
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
    // And the quote itself: the snapshot can be fresher than the last candle,
    // and the last-price marker must stay on screen.
    lo = lo.min(q.price);
    hi = hi.max(q.price);
    let pad = ((hi - lo) * 0.05).max(hi.abs() * 0.0005).max(1e-9);
    let (y_lo, y_hi) = (lo - pad, hi + pad);
    let x_hi = (points.len() - 1) as f64;
    let x_max = x_with_margin(x_hi, app.chart.right_margin_pct);

    let prev_close_points: Vec<(f64, f64)> = q
        .prev_close
        .map(|pc| vec![(0.0, pc), (x_max, pc)])
        .unwrap_or_default();
    // Last-price marker: a thin line from the newest close into the right
    // margin, inverted while the update pulse is active.
    let flash = app.price_flash_dir(symbol);
    let marker_points: Vec<(f64, f64)> = if x_max > x_hi {
        vec![(x_hi, q.price), (x_max, q.price)]
    } else {
        Vec::new()
    };

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
        (sma_points(app.chart.sma_fast), sma_points(app.chart.sma_slow))
    } else {
        (Vec::new(), Vec::new())
    };

    let mut datasets = Vec::new();
    if !prev_close_points.is_empty() {
        // Braille, not Dot: the Dot marker fills every column with a heavy
        // bullet; braille keeps the reference a thin line under the data.
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Braille)
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
    if !marker_points.is_empty() {
        let style = match flash {
            Some(up) => flash_style(up, &app.theme),
            None => Style::new().fg(dir_color(q, &app.theme)),
        };
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(style)
                .data(&marker_points),
        );
    }

    let fmt = axis_time_fmt(visible[0].ts, time_at(visible, x_max));
    let x_labels = vec![
        time_label(visible[0].ts, fmt),
        time_label(time_at(visible, x_max / 2.0), fmt),
        time_label(time_at(visible, x_max), fmt),
    ];
    let y_labels = vec![
        fmt_price(y_lo),
        fmt_price((y_lo + y_hi) / 2.0),
        fmt_price(y_hi),
    ];

    let chart = Chart::new(datasets)
        .block(app.theme.panel().title(chart_title(
            symbol,
            q,
            data,
            app.show_sma,
            &app.chart,
            &app.theme,
            flash,
        )))
        .x_axis(
            Axis::default()
                .bounds([0.0, x_max])
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
    let flash = app.price_flash_dir(symbol);
    let block = app.theme.panel().title(chart_title(
        symbol,
        q,
        data,
        app.show_sma,
        &app.chart,
        &app.theme,
        flash,
    ));
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
    // The quote can be fresher than the last candle; the last-price marker
    // must stay on screen.
    lo = lo.min(q.price);
    hi = hi.max(q.price);
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
    // even buckets. The right margin stays out of the candle zone entirely:
    // it hosts the last-price marker.
    let margin = margin_cols(plot.width, app.chart.right_margin_pct);
    let usable = plot.width - margin;
    let max_cols = usable as usize;
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
    let slot_x = |i: usize| plot.x + usable - (n - i) as u16 * slot;

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

    // SMA overlay: connected braille lines threading between candles (bodies
    // win shared cells). Slow first so the fast line wins where they cross.
    // Computed over the full series; `sample_idx` entries index into
    // `visible`, hence the `cut` offset. Values pushed outside the visible
    // y-range by warm-up history break the line instead of pinning to the edge.
    if app.show_sma {
        let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
        let mut overlay = BrailleOverlay::new(plot.width, plot.height);
        for (period, color) in [
            (app.chart.sma_slow, app.theme.sma_slow),
            (app.chart.sma_fast, app.theme.sma_fast),
        ] {
            let line = indicators::sma(&closes, period);
            let mut prev: Option<(i32, i32)> = None;
            for (i, &raw) in sample_idx.iter().enumerate() {
                let x = slot_x(i) + (slot - body_w) + body_w / 2;
                let point = line[cut + raw]
                    .filter(|&v| (y_lo..=y_hi).contains(&v))
                    .map(|v| {
                        (
                            (x - plot.x) as i32 * 2,
                            scale(v, y_lo, y_hi, plot.height as usize * 4) as i32,
                        )
                    });
                match (prev, point) {
                    (Some(a), Some(b)) => overlay.line(a, b, color),
                    // A lone point (line break on either side) still shows.
                    (None, Some(b)) => overlay.dot(b.0, b.1, color),
                    _ => {}
                }
                prev = point;
            }
        }
        overlay.blit(buf, plot);
    }

    // Last-price marker in the right margin: a line at the quote's level
    // ending in the price tag, inverted in the tick's color while the
    // update pulse is active. With the margin off the title pulse remains.
    if margin > 0 {
        let row = plot.y + (scale(q.price, y_lo, y_hi, plot.height as usize * 2) / 2) as u16;
        let color = dir_color(q, &app.theme);
        for x in plot.x + usable..plot.x + plot.width {
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_char('─').set_fg(color);
            }
        }
        let tag = fmt_price(q.price);
        let len = tag.chars().count() as u16;
        if margin > len {
            let style = match flash {
                Some(up) => flash_style(up, &app.theme),
                None => Style::new().fg(color).add_modifier(Modifier::BOLD),
            };
            buf.set_string(plot.x + plot.width - len, row, &tag, style);
        }
    }

    // Axes: price labels right-aligned in the gutter, three time labels below.
    let dim = Style::new().dim();
    let label_ys = [plot.y, plot.y + plot.height / 2, plot.y + plot.height - 1];
    for (label, y) in y_labels.iter().zip(label_ys) {
        let x = plot.x - 1 - label.chars().count() as u16;
        buf.set_string(x, y, label, dim);
    }
    // The right edge sits `margin` columns past the newest candle, so its
    // label extrapolates that far into the future (in display-candle units).
    let axis_y = plot.y + plot.height;
    let edge_x = (n - 1) as f64 + margin as f64 / slot as f64;
    let fmt = axis_time_fmt(display[0].ts, time_at(&display, edge_x));
    let first = time_label(display[0].ts, fmt);
    let last = time_label(time_at(&display, edge_x), fmt);
    buf.set_string(plot.x, axis_y, &first, dim);
    let last_x = plot.x + plot.width - last.chars().count() as u16;
    buf.set_string(last_x, axis_y, &last, dim);
    if plot.width >= 30 {
        // Centered under the middle candle, not the middle of the plot: with
        // a margin the two differ.
        let mid = time_label(display[n / 2].ts, fmt);
        let len = mid.chars().count() as u16;
        let mid_x = (slot_x(n / 2) + slot / 2)
            .saturating_sub(len / 2)
            .clamp(plot.x, plot.x + plot.width - len);
        buf.set_string(mid_x, axis_y, &mid, dim);
    }
}

/// Price -> subrow index in `[0, sub_rows)`, 0 = top.
fn scale(v: f64, y_lo: f64, y_hi: f64, sub_rows: usize) -> usize {
    (((y_hi - v) / (y_hi - y_lo) * sub_rows as f64) as usize).min(sub_rows - 1)
}

// -- braille overlay ----------------------------------------------------------

/// Unicode braille bit for the dot at (dx in 0..2, dy in 0..4) of a cell.
const BRAILLE_BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Braille canvas over the candle plot, 2x4 dots per terminal cell. The SMA
/// overlay draws its segments here and `blit` merges the result into the
/// frame, with candle bodies keeping their cells.
struct BrailleOverlay {
    cols: u16,
    rows: u16,
    /// Per cell: accumulated dot mask + the color of the last line to touch
    /// it (so the fast SMA wins where the two cross, like the old dots did).
    cells: Vec<(u8, Color)>,
}

impl BrailleOverlay {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![(0, Color::Reset); cols as usize * rows as usize],
        }
    }

    /// Set one dot in dot coordinates (x in 0..cols*2, y in 0..rows*4);
    /// out-of-range dots are dropped, not clamped.
    fn dot(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 {
            return;
        }
        let (cx, cy) = (x as u16 / 2, y as u16 / 4);
        if cx >= self.cols || cy >= self.rows {
            return;
        }
        let cell = &mut self.cells[cy as usize * self.cols as usize + cx as usize];
        cell.0 |= BRAILLE_BITS[y as usize % 4][x as usize % 2];
        cell.1 = color;
    }

    /// Straight segment between two dots (integer Bresenham).
    fn line(&mut self, (x0, y0): (i32, i32), (x1, y1): (i32, i32), color: Color) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            self.dot(x, y, color);
            if x == x1 && y == y1 {
                return;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Merge into the frame at `plot`. Candle bodies keep their cells; wicks
    /// and the prev-close dashes give way, exactly like the old per-column
    /// dots did.
    fn blit(&self, buf: &mut ratatui::buffer::Buffer, plot: Rect) {
        for cy in 0..self.rows {
            for cx in 0..self.cols {
                let (mask, color) = self.cells[cy as usize * self.cols as usize + cx as usize];
                if mask == 0 {
                    continue;
                }
                let Some(cell) = buf.cell_mut((plot.x + cx, plot.y + cy)) else {
                    continue;
                };
                if matches!(cell.symbol(), "█" | "▀" | "▄") {
                    continue;
                }
                let ch = char::from_u32(0x2800 + mask as u32).unwrap_or('·');
                cell.set_char(ch).set_fg(color);
            }
        }
    }
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

fn render_rsi(
    f: &mut Frame,
    area: Rect,
    data: &TickerData,
    cut: usize,
    chart: &ChartDefaults,
    theme: &Theme,
) {
    let period = chart.rsi_period;
    let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
    let rsi = indicators::rsi(&closes, period);
    let Some(last) = rsi.last().copied().flatten() else {
        f.render_widget(
            Paragraph::new(Line::from(format!("not enough history for RSI({period})")).dim())
                .block(theme.panel_titled(format!(" RSI({period}) "))),
            area,
        );
        return;
    };

    // Same visible-window slicing as the price chart above it, including the
    // right margin, so the curves stay roughly column-aligned.
    let x_hi = (closes.len() - 1 - cut) as f64;
    let x_max = x_with_margin(x_hi, chart.right_margin_pct);
    let ref30 = [(0.0, 30.0), (x_max, 30.0)];
    let ref70 = [(0.0, 70.0), (x_max, 70.0)];
    let points: Vec<(f64, f64)> = rsi
        .iter()
        .enumerate()
        .skip(cut)
        .filter_map(|(i, v)| v.map(|v| ((i - cut) as f64, v)))
        .collect();

    let mut datasets = Vec::new();
    // Braille, not Dot, for the same reason as the prev-close reference:
    // the 30/70 levels must read as thin lines, not rows of bullets.
    for refline in [&ref30[..], &ref70[..]] {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Braille)
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
        Span::styled(
            format!(" RSI({period}) "),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{last:.1} "), Style::new().fg(val_color)),
    ]);
    let chart = Chart::new(datasets)
        .block(theme.panel().title(title))
        .x_axis(Axis::default().bounds([0.0, x_max]))
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
    fn margin_cols_scales_and_keeps_a_candle_zone() {
        assert_eq!(margin_cols(100, 20), 20);
        assert_eq!(margin_cols(100, 0), 0);
        assert_eq!(margin_cols(10, 50), 5);
        // Tiny plots always keep at least 2 drawable columns.
        assert_eq!(margin_cols(3, 50), 1);
    }

    #[test]
    fn x_with_margin_stretches_the_axis() {
        // 20% margin: the data's 80 units become 80% of a 100-unit axis.
        assert_eq!(x_with_margin(80.0, 20), 100.0);
        assert_eq!(x_with_margin(50.0, 50), 100.0);
        assert_eq!(x_with_margin(50.0, 0), 50.0);
    }

    #[test]
    fn time_at_reads_candles_and_extrapolates() {
        let candles = flat_series(11, 300); // ts 0, 300, …, 3000
        assert_eq!(time_at(&candles, 0.0), 0);
        assert_eq!(time_at(&candles, 4.0), 1200);
        assert_eq!(time_at(&candles, 10.0), 3000);
        // Past the last candle the average spacing carries on: the margin
        // labels read as the near future.
        assert_eq!(time_at(&candles, 12.5), 3750);
        // A single candle has no spacing to extrapolate with.
        assert_eq!(time_at(&candles[..1], 5.0), 0);
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
    fn braille_overlay_draws_lines_and_yields_to_bodies() {
        use ratatui::buffer::Buffer;

        // A horizontal segment across the top dot row of a 2x1 plot: both
        // cells get the two upper dots (0x01 | 0x08 = '⠉').
        let mut o = BrailleOverlay::new(2, 1);
        o.line((0, 0), (3, 0), Color::Red);
        let plot = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(plot);
        buf.cell_mut((0, 0)).unwrap().set_char('█'); // candle body
        o.blit(&mut buf, plot);
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "█", "body lost its cell");
        assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "⠉");
        assert_eq!(buf.cell((1, 0)).unwrap().fg, Color::Red);

        // Where two lines share a cell the later one wins the color but the
        // earlier dots stay merged into the glyph.
        let mut o = BrailleOverlay::new(1, 1);
        o.dot(0, 0, Color::Red);
        o.dot(0, 3, Color::Blue);
        let plot = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(plot);
        o.blit(&mut buf, plot);
        let cell = buf.cell((0, 0)).unwrap();
        assert_eq!(cell.symbol(), "⡁"); // 0x01 | 0x40
        assert_eq!(cell.fg, Color::Blue);

        // Out-of-range dots are dropped, never wrapped onto other cells.
        let mut o = BrailleOverlay::new(1, 1);
        o.dot(-1, 0, Color::Red);
        o.dot(2, 5, Color::Red);
        assert!(o.cells.iter().all(|&(mask, _)| mask == 0));
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
