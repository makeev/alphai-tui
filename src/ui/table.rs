use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::widgets::{Block, Cell, Row, Table};

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

/// Shared by TableView and SplitView.
pub fn render_table(f: &mut Frame, area: Rect, app: &mut App) {
    let spark_width = 24usize;
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

            Row::new(vec![
                Cell::from(symbol.clone()).bold(),
                Cell::from(fmt_price(q.price)),
                Cell::from(change).style(dir_style),
                Cell::from(change_pct).style(dir_style),
                Cell::from(range).dim(),
                Cell::from(spark_line(&closes, spark_width)).style(dir_style),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(9),
        Constraint::Length(19),
        Constraint::Min(spark_width as u16),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new(["Symbol", "Price", "Δ", "Δ%", "Lo–Hi", "Spark"])
                .style(Style::new().bold().underlined()),
        )
        .block(Block::bordered().title(" Watchlist "))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    app.table_state.select(Some(app.selected));
    f.render_stateful_widget(table, area, &mut app.table_state);
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
