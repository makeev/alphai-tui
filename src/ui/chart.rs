use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, ChartStyle};
use crate::config::ChartDefaults;
use crate::domain::{
    Candle, Interval, PriceFeed, Quote, Range, Sessions, TickerData, fmt_price, fmt_volume,
};
use crate::indicators;
use crate::keymap::Action;
use crate::market;
use crate::theme::Theme;
use crate::ui::{Hint, View, ViewId};
use crate::ui::{news_marks, time_axis::TimeAxis};

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
            // One grouped hint, like the split view: five separate ones no
            // longer fit 110 columns, and the help overlay spells them out.
            Hint::act(
                &[
                    Action::ChartStyle,
                    Action::ToggleSma,
                    Action::ToggleRsi,
                    Action::ToggleVolume,
                    Action::MaType,
                    Action::NewsMarkers,
                ],
                "chart",
            ),
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
const VOLUME_PANEL_HEIGHT: u16 = 6;
/// The price chart never shrinks below this: the panels under it drop out
/// one by one until it fits (same graceful degradation as the split view's
/// news half).
const PRICE_MIN_HEIGHT: u16 = 12;

/// Heights of the panels under the price chart, top to bottom: (volume,
/// rsi), 0 meaning not shown. RSI reserves its space first so the height at
/// which it appears stays exactly where it has always been.
fn panel_split(height: u16, volume: bool, rsi: bool) -> (u16, u16) {
    let mut rest = height;
    let mut take = |want: u16, wanted: bool| {
        if wanted && rest >= want + PRICE_MIN_HEIGHT {
            rest -= want;
            want
        } else {
            0
        }
    };
    let rsi_h = take(RSI_PANEL_HEIGHT, rsi);
    let volume_h = take(VOLUME_PANEL_HEIGHT, volume);
    (volume_h, rsi_h)
}

/// Price chart of the selected symbol: candlesticks by default, the classic
/// close line via the `c` toggle, optional 20/100 moving average overlays
/// (`m`, simple or exponential by `e`), a volume panel (`b`) and an RSI(14)
/// panel (`i`). Shared by ChartView and SplitView.
pub fn render_chart(f: &mut Frame, area: Rect, app: &App) {
    let symbol = app.selected_symbol().to_string();

    let Some(data) = app.data.get(&symbol) else {
        let msg = match app.errors.get(&symbol) {
            Some(e) => Line::from(format!("{symbol}: {e}")).style(Style::new().fg(app.theme.error)),
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

    let cut = visible_from(&data.candles, app.range);
    // An empty volume panel would only steal rows from the price chart:
    // finnhub synthesizes candles from ticks and carries no volume at all.
    let has_volume = app.show_volume && data.candles[cut..].iter().any(|c| c.volume.is_some());
    let single_venue = single_venue_volume(&data.candles);
    let (volume_h, rsi_h) = panel_split(area.height, has_volume, app.show_rsi);
    let [price_area, volume_area, rsi_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(volume_h),
        Constraint::Length(rsi_h),
    ])
    .areas(area);

    let geom = render_price_candles(f, price_area, app, &symbol, data, cut);
    if let Some(geom) = geom {
        if volume_h > 0 {
            render_volume_candles(f, volume_area, &geom, single_venue, &app.chart, &app.theme);
        }
        if rsi_h > 0 {
            render_rsi(f, rsi_area, data, cut, &geom, &app.chart, &app.theme);
        }
    }
}

/// Index of the first candle inside the visible window. Sources fetch extra
/// history for indicator warm-up (`domain::fetch_range`); the chart renders
/// only the trailing `range` worth, anchored on the newest candle so a
/// closed market still shows the last session. Indicators keep the full
/// series and are sliced with the same offset.
pub(crate) fn visible_from(candles: &[Candle], range: Range) -> usize {
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

/// Style of the last-price value while its update pulse is active: the tick
/// direction's color, inverted so it visibly blinks.
pub(crate) fn flash_style(up: bool, theme: &Theme) -> Style {
    Style::new()
        .fg(if up { theme.up } else { theme.down })
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

/// Color for a price move. Split out of `dir_color` so a view can color a
/// move the quote does not carry as its headline change, such as the rail's
/// extended-hours print.
pub(crate) fn move_color(change: Option<f64>, theme: &Theme) -> Color {
    match change {
        Some(c) if c < 0.0 => theme.down,
        Some(_) => theme.up,
        None => theme.flat,
    }
}

pub(crate) fn dir_color(q: &Quote, theme: &Theme) -> Color {
    move_color(q.change(), theme)
}

/// Legend labels appear only for average lines that actually have points on
/// screen: an average needs `period` candles of history, which short series
/// (finnhub's growing synthetic one, thin symbols) may not have yet.
/// `note` is the bar count when the plot is too narrow for the whole
/// window, so a narrow chart says what it left out.
fn chart_title(
    symbol: &str,
    data: &TickerData,
    app: &App,
    flash: Option<bool>,
    note: Option<&str>,
) -> Line<'static> {
    let (q, theme) = (&data.quote, &app.theme);
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
        Span::styled(
            format!("{change_str} "),
            Style::new().fg(dir_color(q, theme)),
        ),
    ];
    if let Some(note) = note {
        spans.push(Span::styled(
            format!("{note} "),
            Style::new().fg(theme.flat),
        ));
    }
    if app.interval != Interval::D1 && market::is_us_equity(symbol) {
        let mut feeds = Vec::new();
        let cut = visible_from(&data.candles, app.range);
        for c in &data.candles[cut..] {
            if market::window_at(c.ts).is_some_and(|w| w.session != market::Session::Open)
                && !feeds.contains(&c.feed)
            {
                feeds.push(c.feed);
            }
        }
        let text = if app.sessions == Sessions::Regular {
            "EXT: off".to_string()
        } else if feeds.is_empty() {
            "EXT: no data".to_string()
        } else {
            format!(
                "EXT: {}",
                feeds
                    .iter()
                    .map(|f| f.label())
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
        };
        spans.push(Span::styled(
            format!("{text} "),
            Style::new().fg(theme.flat),
        ));
        let key = app.keymap.labels(&[Action::ToggleExtended]);
        if !key.is_empty() {
            let key = if key.len() == 1 && key.chars().all(|c| c.is_ascii_uppercase()) {
                format!("Shift+{key}")
            } else {
                key
            };
            let action = if app.sessions == Sessions::Extended {
                "off"
            } else {
                "on"
            };
            spans.push(Span::styled(
                format!("({key}: {action}) "),
                Style::new().fg(theme.flat),
            ));
        }
    }
    // Keep the session switch ahead of optional legends on narrow charts.
    if app.show_sma {
        for (period, color) in [
            (app.chart.sma_fast, theme.sma_fast),
            (app.chart.sma_slow, theme.sma_slow),
        ] {
            if data.candles.len() >= period {
                spans.push(Span::styled(
                    format!("{}{period} ", app.ma_type.label()),
                    Style::new().fg(color),
                ));
            }
        }
    }
    Line::from(spans)
}

/// The chart's bottom border when the news marks have something to name:
/// the freshest of them, with its headline. Shared by both chart modes.
fn headline(
    marks: &[news_marks::Mark],
    symbol: &str,
    width: u16,
    theme: &Theme,
) -> Option<Line<'static>> {
    news_marks::headline(news_marks::latest(marks)?, symbol, width, theme)
}

// -- candle mode --------------------------------------------------------------

/// Column layout of the candle plot, handed to the volume panel below it so
/// every bar sits under the candle it belongs to.
struct CandleGeom {
    /// Width of the price-label gutter left of the plot.
    gutter: u16,
    plot_x: u16,
    body_w: u16,
    xs: Vec<u16>,
    sample_idx: Vec<usize>,
    axis: TimeAxis,
    /// The candles actually drawn: the newest of the window that fit.
    display: Vec<Candle>,
}

impl CandleGeom {
    /// Left column of candle `i`'s body.
    fn body_x(&self, i: usize) -> u16 {
        self.xs[i]
    }
}

/// Hand-rolled candlestick renderer writing straight into the buffer at
/// half-block resolution: two subrows per terminal row, body `█ ▀ ▄`, wick
/// `│ ╵ ╷`. ratatui's Chart widget has no candle graph type. Returns the
/// column layout for the volume panel, or None when the area was too small
/// to plot anything.
fn render_price_candles(
    f: &mut Frame,
    area: Rect,
    app: &App,
    symbol: &str,
    data: &TickerData,
    cut: usize,
) -> Option<CandleGeom> {
    let q = &data.quote;
    let draw_extended = app.sessions == Sessions::Extended && app.interval != Interval::D1;
    let extended_price = q.extended_price();
    let marker_price = if draw_extended {
        extended_price.unwrap_or_else(|| data.candles.last().map_or(q.price, |c| c.close))
    } else {
        q.price
    };
    let reference = if draw_extended && extended_price.is_some() {
        Some(q.extended_reference())
    } else {
        q.prev_close
    };
    let visible = &data.candles[cut..];
    let flash = app.price_flash_dir(symbol);
    let panel = app.theme.panel();
    // Titles live on the border row, so the inner area is known before any
    // of them is: the block itself is rendered further down, once the
    // candle columns (and with them the candle size and the news marks)
    // are laid out.
    let inner = panel.inner(area);

    // The gutter is sized on the whole window, so the layout stays put when
    // the bars on screen change; the axis itself follows those bars.
    let (lo, hi) = y_range(visible, reference, marker_price);
    let gutter = y_labels(lo, hi)
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap() as u16
        + 1;
    if inner.width <= gutter + 2 || inner.height <= 3 {
        let block = panel.title(chart_title(symbol, data, app, flash, None));
        f.render_widget(block, area);
        return None; // too small: leave the bare block
    }
    let plot = Rect {
        x: inner.x + gutter,
        y: inner.y,
        width: inner.width - gutter,
        height: inner.height - 2, // time labels and date/timezone row
    };

    // Every candle gets a slot of body + 1 column of gap so neighbours never
    // fuse into a solid mass. A bar is its interval whatever the width: when
    // the window holds more bars than fit, the newest ones show and the
    // title counts the rest. The right margin stays out of the candle zone
    // entirely: it hosts the last-price marker.
    let margin = margin_cols(plot.width, app.chart.right_margin_pct);
    let usable = plot.width - margin;
    let max_cols = usable as usize;
    let max_candles = (max_cols / 2).max(1);
    let shown = newest_that_fit(visible.len(), max_candles);
    let display: Vec<Candle> = visible[shown.clone()].to_vec();
    let sample_idx: Vec<usize> = shown.clone().collect();
    let note =
        (shown.start > 0).then(|| format!("last {} of {} bars", display.len(), visible.len()));
    let block = panel.title(chart_title(symbol, data, app, flash, note.as_deref()));
    let (y_lo, y_hi) = y_range(&display, reference, marker_price);
    let y_labels = y_labels(y_lo, y_hi);
    // Spread the visible history across the candle zone even when only
    // a few pre-market bars exist. Keep bodies at most 3 columns wide.
    let n = display.len();
    let slot = (max_cols / n).clamp(1, 4) as u16;
    let body_w = slot.saturating_sub(1).max(1);
    let xs: Vec<u16> = (0..n)
        .map(|i| plot.x + (i * (usable - body_w) as usize / n.saturating_sub(1).max(1)) as u16)
        .collect();
    let axis = TimeAxis::build(
        &display,
        &xs.iter().map(|x| x + body_w / 2).collect::<Vec<_>>(),
        plot,
        app.interval,
        app.sessions,
        app.chart.timezone,
        market::is_us_equity(symbol),
        visible.last().unwrap().ts,
    );

    // The ticker's news, placed on the candles on screen; rows older than
    // the first of them are dropped by `place`. Read from the cache the News
    // and Split views fill, so this costs nothing.
    let marks = if app.show_news_markers {
        news_marks::place(app.ticker_articles(symbol), &display, app.interval)
    } else {
        Vec::new()
    };
    let block = match headline(&marks, symbol, area.width, &app.theme) {
        Some(line) => block.title_bottom(line),
        None => block,
    };
    f.render_widget(block, area);

    let buf = f.buffer_mut();
    axis.backdrop(
        buf,
        plot,
        &app.theme,
        app.chart.session_shading,
        app.chart.time_grid,
    );

    // Previous-close reference first; candles draw over it.
    if let Some(pc) = reference {
        let row = (scale(pc, y_lo, y_hi, plot.height as usize * 2) / 2) as u16;
        for x in (plot.x..plot.x + plot.width).step_by(2) {
            if let Some(cell) = buf.cell_mut((x, plot.y + row)) {
                cell.set_char('╌').set_fg(app.theme.ref_line);
            }
        }
    }

    if app.chart_style == ChartStyle::Line {
        let mut line = BrailleOverlay::new(plot.width, plot.height);
        let mut previous = None;
        for (i, c) in display.iter().enumerate() {
            let point = (
                ((xs[i] + body_w / 2 - plot.x) * 2) as i32,
                scale(c.close, y_lo, y_hi, plot.height as usize * 4) as i32,
            );
            if let Some(prev) = previous {
                line.line(prev, point, dir_color(q, &app.theme));
            } else {
                line.dot(point.0, point.1, dir_color(q, &app.theme));
            }
            previous = Some(point);
        }
        line.blit(buf, plot);
    } else {
        for (i, c) in display.iter().enumerate() {
            let prev_close = (i > 0).then(|| display[i - 1].close);
            let color = candle_color(c, prev_close, &app.theme);
            let body_x = xs[i];
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
            let line = indicators::ma(app.ma_type, &closes, period);
            let mut prev: Option<(i32, i32)> = None;
            for (i, &raw) in sample_idx.iter().enumerate() {
                let x = xs[i] + body_w / 2;
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

    // News marks: one glyph per candle that carried a story, sitting just
    // above its high, so the move and its reason share a column. Drawn
    // after the averages, which a mark may safely overwrite.
    for mark in &marks {
        let x = xs[mark.col] + body_w / 2;
        let top = (scale(display[mark.col].high, y_lo, y_hi, plot.height as usize * 2) / 2) as u16;
        let (ch, style) = news_marks::glyph(mark.article, symbol, &app.theme);
        let ch = if app.chart_style == ChartStyle::Line {
            '•'
        } else {
            ch
        };
        if let Some(cell) = buf.cell_mut((x, plot.y + top.saturating_sub(1))) {
            cell.set_char(ch).set_style(style);
        }
    }

    // Last-price marker in the right margin: a line at the quote's level
    // ending in the price tag, inverted in the tick's color while the
    // update pulse is active. With the margin off the title pulse remains.
    if margin > 0 {
        let row = plot.y + (scale(marker_price, y_lo, y_hi, plot.height as usize * 2) / 2) as u16;
        let color = if draw_extended && extended_price.is_some() {
            move_color(Some(marker_price - q.extended_reference()), &app.theme)
        } else {
            dir_color(q, &app.theme)
        };
        for x in plot.x + usable..plot.x + plot.width {
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_char('─').set_fg(color);
            }
        }
        let tag = fmt_price(marker_price);
        let len = tag.chars().count() as u16;
        if margin > len {
            let style = match flash {
                Some(up) => flash_style(up, &app.theme),
                None => Style::new().fg(color).add_modifier(Modifier::BOLD),
            };
            buf.set_string(plot.x + plot.width - len, row, &tag, style);
        }
    }

    // Price labels in the gutter; the shared adaptive time axis below.
    let dim = Style::new().dim();
    let label_ys = [plot.y, plot.y + plot.height / 2, plot.y + plot.height - 1];
    for (label, y) in y_labels.iter().zip(label_ys) {
        let x = (plot.x - 1)
            .saturating_sub(label.chars().count() as u16)
            .max(inner.x);
        buf.set_string(x, y, label, dim);
    }
    axis.render(buf, plot);

    Some(CandleGeom {
        gutter,
        plot_x: plot.x,
        body_w,
        xs,
        sample_idx,
        axis,
        display,
    })
}

/// Price -> subrow index in `[0, sub_rows)`, 0 = top.
fn scale(v: f64, y_lo: f64, y_hi: f64, sub_rows: usize) -> usize {
    (((y_hi - v) / (y_hi - y_lo) * sub_rows as f64) as usize).min(sub_rows - 1)
}

/// The bars a plot with room for `max` candles shows: the newest ones. Bars
/// are never merged into larger candles; a bar is its interval at every
/// width, and a narrow plot simply reaches less far back.
fn newest_that_fit(len: usize, max: usize) -> std::ops::Range<usize> {
    len.saturating_sub(max)..len
}

/// Vertical span of the plot: the candles' lows and highs, the reference
/// line and the last-price marker (the quote can be fresher than the last
/// candle, and the marker must stay on screen), padded off the frame.
fn y_range(candles: &[Candle], reference: Option<f64>, marker: f64) -> (f64, f64) {
    let (mut lo, mut hi) = candles
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| {
            (lo.min(c.low), hi.max(c.high))
        });
    if let Some(pc) = reference {
        lo = lo.min(pc);
        hi = hi.max(pc);
    }
    lo = lo.min(marker);
    hi = hi.max(marker);
    let pad = ((hi - lo) * 0.05).max(hi.abs() * 0.0005).max(1e-9);
    (lo - pad, hi + pad)
}

/// Top, middle and bottom price labels of the gutter.
fn y_labels(y_lo: f64, y_hi: f64) -> [String; 3] {
    [
        fmt_price(y_hi),
        fmt_price((y_lo + y_hi) / 2.0),
        fmt_price(y_lo),
    ]
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

// -- volume panel -------------------------------------------------------------

/// IEX is one exchange, a few percent of the tape. Its share counts survive
/// only on charts without consolidated bars (`extended::apply` swaps them
/// out otherwise), and then the panel names the venue rather than passing
/// them off as the market's volume.
fn single_venue_volume(candles: &[Candle]) -> bool {
    let mut feeds = candles
        .iter()
        .filter(|c| c.volume.is_some())
        .map(|c| c.feed);
    feeds.next() == Some(PriceFeed::Iex) && feeds.all(|f| f == PriceFeed::Iex)
}

fn volume_title(last: Option<f64>, single_venue: bool, theme: &Theme) -> Line<'static> {
    let mut spans = vec![Span::styled(
        " Vol ",
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    )];
    if let Some(v) = last {
        spans.push(Span::styled(
            format!("{} ", fmt_volume(v)),
            Style::new().fg(theme.flat),
        ));
    }
    if single_venue {
        spans.push(Span::styled("IEX only ", Style::new().fg(theme.flat)));
    }
    Line::from(spans)
}

/// Lower block glyphs by eighths: four panel rows resolve 32 bar heights,
/// enough to tell a regular session from a quiet premarket next to an
/// auction print.
const LOWER_EIGHTHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Volume bars sharing the candles' columns exactly: same gutter, same
/// slots, and the right margin left empty (the last-price marker lives
/// there, a bar under it would read as one more candle). Each bar takes its
/// candle's color, so the panel shows which side the volume was on.
fn render_volume_candles(
    f: &mut Frame,
    area: Rect,
    geom: &CandleGeom,
    single_venue: bool,
    chart: &ChartDefaults,
    theme: &Theme,
) {
    let vmax = geom
        .display
        .iter()
        .filter_map(|c| c.volume)
        .fold(0.0, f64::max);
    // The newest bars can lack a count while the delayed consolidated feed
    // catches up; the title keeps the latest one known.
    let last = geom.display.iter().rev().find_map(|c| c.volume);
    let block = theme.panel().title(volume_title(last, single_venue, theme));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if vmax <= 0.0 || inner.height == 0 || inner.width <= geom.gutter {
        return;
    }

    let buf = f.buffer_mut();
    let plot = Rect::new(
        geom.plot_x,
        inner.y,
        inner.right().saturating_sub(geom.plot_x),
        inner.height,
    );
    geom.axis
        .backdrop(buf, plot, theme, chart.session_shading, chart.time_grid);
    let levels = inner.height as usize * LOWER_EIGHTHS.len();
    for (i, c) in geom.display.iter().enumerate() {
        let Some(v) = c.volume else { continue };
        // Eighths filled from the bottom up. A traded candle keeps at least a
        // stub so a quiet session still reads as data, not as a gap.
        let filled = (((v / vmax) * levels as f64).round() as usize).clamp(1, levels);
        let color = candle_color(c, (i > 0).then(|| geom.display[i - 1].close), theme);
        let body_x = geom.body_x(i);
        for row in 0..inner.height {
            let lower = (inner.height - 1 - row) as usize * LOWER_EIGHTHS.len();
            let ch = match filled.saturating_sub(lower).min(LOWER_EIGHTHS.len()) {
                0 => continue,
                n => LOWER_EIGHTHS[n - 1],
            };
            for x in body_x..body_x + geom.body_w {
                if let Some(cell) = buf.cell_mut((x, inner.y + row)) {
                    cell.set_char(ch).set_fg(color);
                }
            }
        }
    }

    // Peak right-aligned in the price chart's gutter, like its own labels.
    let label = fmt_volume(vmax);
    let len = label.chars().count() as u16;
    if len < geom.gutter {
        buf.set_string(geom.plot_x - 1 - len, inner.y, &label, Style::new().dim());
    }
}

// -- RSI panel ----------------------------------------------------------------

fn render_rsi(
    f: &mut Frame,
    area: Rect,
    data: &TickerData,
    cut: usize,
    geom: &CandleGeom,
    chart: &ChartDefaults,
    theme: &Theme,
) {
    let period = chart.rsi_period;
    let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
    let values = indicators::rsi(&closes, period);
    let Some(last) = values.last().copied().flatten() else {
        f.render_widget(
            Paragraph::new(Line::from(format!("not enough history for RSI({period})")).dim())
                .block(theme.panel_titled(format!(" RSI({period}) "))),
            area,
        );
        return;
    };
    let color = if last >= 70.0 {
        theme.down
    } else if last <= 30.0 {
        theme.up
    } else {
        theme.flat
    };
    let block = theme.panel().title(Line::from(vec![
        Span::styled(
            format!(" RSI({period}) "),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{last:.1} "), Style::new().fg(color)),
    ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.right() <= geom.plot_x {
        return;
    }
    let plot = Rect::new(
        geom.plot_x,
        inner.y,
        inner.right() - geom.plot_x,
        inner.height,
    );
    let buf = f.buffer_mut();
    geom.axis
        .backdrop(buf, plot, theme, chart.session_shading, chart.time_grid);
    for value in [30.0, 70.0] {
        let y = plot.y + (scale(value, 0.0, 100.0, plot.height as usize * 2) / 2) as u16;
        for x in (plot.x..plot.right()).step_by(2) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char('╌').set_fg(theme.ref_line);
            }
        }
    }
    for value in [0.0, 50.0, 100.0] {
        let y = plot.y + (scale(value, 0.0, 100.0, plot.height as usize * 2) / 2) as u16;
        let text = format!("{value:.0}");
        buf.set_string(plot.x - 1 - text.len() as u16, y, text, Style::new().dim());
    }
    let mut line = BrailleOverlay::new(plot.width, plot.height);
    let mut prev = None;
    for (i, raw) in geom.sample_idx.iter().enumerate() {
        let point = values[cut + raw].map(|v| {
            (
                ((geom.body_x(i) + geom.body_w / 2 - plot.x) * 2) as i32,
                scale(v, 0.0, 100.0, plot.height as usize * 4) as i32,
            )
        });
        match (prev, point) {
            (Some(a), Some(b)) => line.line(a, b, theme.rsi_line),
            (None, Some(b)) => line.dot(b.0, b.1, theme.rsi_line),
            _ => {}
        }
        prev = point;
    }
    line.blit(buf, plot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_plot_shows_the_newest_bars_and_never_merges() {
        assert_eq!(newest_that_fit(192, 70), 122..192);
        assert_eq!(newest_that_fit(30, 70), 0..30);
        assert_eq!(newest_that_fit(0, 70), 0..0);
    }

    #[test]
    fn volume_names_a_single_venue_only_when_nothing_else_counts() {
        let bar = |feed, volume| Candle {
            feed,
            volume,
            ..Default::default()
        };
        let iex = bar(PriceFeed::Iex, Some(10.0));
        let sip = bar(PriceFeed::DelayedSip, Some(900.0));
        assert!(single_venue_volume(&[iex, iex]));
        assert!(!single_venue_volume(&[iex, sip]));
        assert!(!single_venue_volume(&[sip]));
        assert!(!single_venue_volume(&[bar(PriceFeed::Iex, None)]));
        assert!(single_venue_volume(&[
            bar(PriceFeed::DelayedSip, None),
            iex
        ]));
    }

    fn candle(open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            feed: Default::default(),
            ts: 0,
            open,
            high,
            low,
            close,
            volume: None,
        }
    }

    /// RSI reserves first, so its threshold is exactly where it always was
    /// (height 20), and volume only takes rows the price chart can spare.
    #[test]
    fn panel_split_feeds_the_price_chart_first() {
        assert_eq!(
            panel_split(40, true, true),
            (VOLUME_PANEL_HEIGHT, RSI_PANEL_HEIGHT)
        );
        assert_eq!(panel_split(20, true, true), (0, RSI_PANEL_HEIGHT));
        // One row short of RSI, but the cheaper volume panel still fits.
        assert_eq!(panel_split(19, true, true), (VOLUME_PANEL_HEIGHT, 0));
        assert_eq!(panel_split(17, true, true), (0, 0));
        assert_eq!(panel_split(20, true, false), (VOLUME_PANEL_HEIGHT, 0));
        // Toggled off panels never reserve anything.
        assert_eq!(panel_split(40, false, false), (0, 0));
        assert_eq!(panel_split(40, false, true), (0, RSI_PANEL_HEIGHT));
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
        assert_eq!(
            buf.cell((0, 0)).unwrap().symbol(),
            "█",
            "body lost its cell"
        );
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
