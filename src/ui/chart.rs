use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph};

use crate::app::App;
use crate::domain::fmt_price;
use crate::ui::View;

pub struct ChartView;

impl View for ChartView {
    fn title(&self) -> &'static str {
        "Chart"
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        render_chart(f, area, app);
    }
}

/// Line chart of closes for the selected symbol. Shared by ChartView and
/// SplitView.
pub fn render_chart(f: &mut Frame, area: Rect, app: &App) {
    let symbol = app.selected_symbol().to_string();
    let block = Block::bordered();

    let Some(data) = app.data.get(&symbol) else {
        let msg = match app.errors.get(&symbol) {
            Some(e) => Line::from(format!("{symbol}: {e}")).style(Style::new().fg(Color::Red)),
            None => Line::from(format!("{symbol}: loading…")).dim(),
        };
        f.render_widget(
            Paragraph::new(msg).block(block.title(format!(" {symbol} "))),
            area,
        );
        return;
    };
    if data.candles.len() < 2 {
        f.render_widget(
            Paragraph::new(Line::from("not enough history for a chart").dim())
                .block(block.title(format!(" {symbol} "))),
            area,
        );
        return;
    }

    let q = &data.quote;
    let points: Vec<(f64, f64)> = data
        .candles
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

    let dir_color = match q.change() {
        Some(c) if c < 0.0 => Color::Red,
        Some(_) => Color::Green,
        None => Color::Gray,
    };

    let prev_close_points: Vec<(f64, f64)> = q
        .prev_close
        .map(|pc| vec![(0.0, pc), (x_hi, pc)])
        .unwrap_or_default();

    let mut datasets = Vec::new();
    if !prev_close_points.is_empty() {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Dot)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(Color::DarkGray))
                .data(&prev_close_points),
        );
    }
    datasets.push(
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(dir_color))
            .data(&points),
    );

    let time_fmt = if app.interval.is_intraday() { "%H:%M" } else { "%d %b" };
    let ts_label = |c: &crate::domain::Candle| -> String {
        DateTime::from_timestamp(c.ts, 0)
            .map(|t| t.with_timezone(&Local).format(time_fmt).to_string())
            .unwrap_or_default()
    };
    let mid = data.candles.len() / 2;
    let x_labels = vec![
        ts_label(&data.candles[0]),
        ts_label(&data.candles[mid]),
        ts_label(data.candles.last().unwrap()),
    ];
    let y_labels = vec![
        fmt_price(y_lo),
        fmt_price((y_lo + y_hi) / 2.0),
        fmt_price(y_hi),
    ];

    let change_str = match (q.change(), q.change_pct()) {
        (Some(c), Some(p)) => format!("{c:+.2} ({p:+.2}%)"),
        _ => "—".into(),
    };
    let title = Line::from(vec![
        Span::styled(format!(" {symbol} "), Style::new().bold()),
        Span::raw(format!(
            "{} {} ",
            fmt_price(q.price),
            q.currency.as_deref().unwrap_or("")
        )),
        Span::styled(format!("{change_str} "), Style::new().fg(dir_color)),
    ]);

    let chart = Chart::new(datasets)
        .block(block.title(title))
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
