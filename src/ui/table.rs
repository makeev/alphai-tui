use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Text;
use ratatui::widgets::{Cell, Row, Table};

use crate::app::App;
use crate::domain::fmt_price;
use crate::ui::{View, ViewId};

pub struct TableView;

impl View for TableView {
    fn id(&self) -> ViewId {
        ViewId::Table
    }

    fn title(&self) -> &'static str {
        "Table"
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        render_table(f, area, app);
    }
}

/// Widths of the fixed columns, plus the chrome ratatui adds around them:
/// one blank column between cells and the selection marker's own column.
const W_SYMBOL: u16 = 10;
const W_PRICE: u16 = 12;
const W_CHANGE: u16 = 10;
const W_PCT: u16 = 9;
const W_RANGE: u16 = 19;
const SPARK_MIN: u16 = 8;
const SPARK_MAX: u16 = 24;
const GAP: u16 = 1;
const MARKER: u16 = 2;

/// Which optional columns are shown. When the fixed widths do not fit,
/// ratatui squeezes every column at once (prices become "206.", a range
/// "319.54–3"), which is exactly what the split view used to look like.
/// So the table drops whole columns instead, least useful first, and the
/// survivors keep their full width.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Columns {
    change: bool,
    pct: bool,
    range: bool,
    /// 0 hides the sparkline.
    spark: u16,
}

/// Width the fixed columns need together, gaps included.
fn fixed_width(change: bool, pct: bool, range: bool) -> u16 {
    W_SYMBOL
        + GAP
        + W_PRICE
        + if change { GAP + W_CHANGE } else { 0 }
        + if pct { GAP + W_PCT } else { 0 }
        + if range { GAP + W_RANGE } else { 0 }
}

/// The widest column set that fits `avail` (the inner width minus the
/// selection marker). Symbol and price always stay.
fn columns(avail: u16) -> Columns {
    for (change, pct, range, spark) in [
        (true, true, true, true),
        (true, true, false, true),
        (false, true, false, true),
        (false, true, false, false),
        (false, false, false, false),
    ] {
        let fixed = fixed_width(change, pct, range);
        if fixed + if spark { GAP + SPARK_MIN } else { 0 } <= avail {
            let spark = if spark {
                (avail - fixed - GAP).min(SPARK_MAX)
            } else {
                0
            };
            return Columns { change, pct, range, spark };
        }
    }
    // Narrower than symbol plus price: nothing left to drop.
    Columns { change: false, pct: false, range: false, spark: 0 }
}

/// Shared by TableView and SplitView.
pub fn render_table(f: &mut Frame, area: Rect, app: &mut App) {
    let cols = columns(area.width.saturating_sub(2 + MARKER));
    let spark_width = cols.spark as usize;
    let rows: Vec<Row> = app
        .symbols
        .iter()
        .map(|symbol| {
            let Some(data) = app.data.get(symbol) else {
                let status = if app.errors.contains_key(symbol) {
                    Cell::from("error").style(Style::new().fg(app.theme.error))
                } else {
                    Cell::from("…").dim()
                };
                return Row::new(vec![Cell::from(symbol.clone()).bold(), status]);
            };

            let q = &data.quote;
            let dir_style = match q.change() {
                Some(c) if c > 0.0 => Style::new().fg(app.theme.up),
                Some(c) if c < 0.0 => Style::new().fg(app.theme.down),
                _ => Style::new().dim(),
            };
            // Freshly updated price pulses in the tick's color (see
            // `App::price_flash_dir`), so the table reads as live too.
            let price_style = match app.price_flash_dir(symbol) {
                Some(up) => Style::new()
                    .fg(if up { app.theme.up } else { app.theme.down })
                    .add_modifier(Modifier::BOLD),
                None => Style::new(),
            };
            let change = q
                .change()
                .map(|c| format!("{c:+.2}"))
                .unwrap_or_else(|| "—".into());
            let change_pct = q
                .change_pct()
                .map(|p| format!("{p:+.2}%"))
                .unwrap_or_else(|| "—".into());

            let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
            let (lo, hi) = closes
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &c| {
                    (lo.min(c), hi.max(c))
                });
            let range = if closes.is_empty() {
                "—".into()
            } else {
                format!("{}–{}", fmt_price(lo), fmt_price(hi))
            };

            // Numbers right-align so the decimal points line up down the
            // column; the symbol and the sparkline stay left.
            let mut cells = vec![
                Cell::from(symbol.clone()).bold(),
                Cell::from(right(fmt_price(q.price))).style(price_style),
            ];
            if cols.change {
                cells.push(Cell::from(right(change)).style(dir_style));
            }
            if cols.pct {
                cells.push(Cell::from(right(change_pct)).style(dir_style));
            }
            if cols.range {
                cells.push(Cell::from(right(range)).dim());
            }
            if cols.spark > 0 {
                cells.push(Cell::from(spark_line(&closes, spark_width)).style(dir_style));
            }
            Row::new(cells)
        })
        .collect();

    let mut widths = vec![Constraint::Length(W_SYMBOL), Constraint::Length(W_PRICE)];
    let mut header = vec![Cell::from("Symbol"), Cell::from(right("Price"))];
    if cols.change {
        widths.push(Constraint::Length(W_CHANGE));
        header.push(Cell::from(right("Δ")));
    }
    if cols.pct {
        widths.push(Constraint::Length(W_PCT));
        header.push(Cell::from(right("Δ%")));
    }
    if cols.range {
        widths.push(Constraint::Length(W_RANGE));
        header.push(Cell::from(right("Lo–Hi")));
    }
    if cols.spark > 0 {
        widths.push(Constraint::Length(cols.spark));
        header.push(Cell::from("Spark"));
    }
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(Style::new().bold().underlined()))
        .block(app.theme.panel_titled(" Watchlist "))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    app.table_state.select(Some(app.selected));
    f.render_stateful_widget(table, area, &mut app.table_state);
}

/// Right-aligned cell content (numbers line up under each other).
fn right(text: impl Into<String>) -> Text<'static> {
    Text::from(text.into()).right_aligned()
}

/// Downsample a series into a fixed-width string of block characters.
fn spark_line(values: &[f64], width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let chunk = (values.len() as f64 / width as f64).max(1.0);
    let mut sampled = Vec::with_capacity(width);
    let mut i = 0.0;
    while (i as usize) < values.len() && sampled.len() < width {
        let start = i as usize;
        let end = (((i + chunk) as usize).max(start + 1)).min(values.len());
        let avg = values[start..end].iter().sum::<f64>() / (end - start) as f64;
        sampled.push(avg);
        i += chunk;
    }
    let (lo, hi) = sampled
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    let span = (hi - lo).max(1e-9);
    sampled
        .iter()
        .map(|v| BARS[(((v - lo) / span) * 7.0).round() as usize])
        .collect()
}
