use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::Notify;

use crate::app::App;
use crate::domain::{Candle, Interval, Quote, Range, TickerData};
use crate::ui;

fn fake_app() -> App {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        vec!["AAPL".into(), "MSFT".into()],
        "yahoo",
        Range::D1,
        Interval::M5,
        rx,
        Arc::new(Notify::new()),
    );
    for (symbol, base) in [("AAPL", 200.0), ("MSFT", 400.0)] {
        let candles: Vec<Candle> = (0..30)
            .map(|i| {
                let close = base + i as f64 * 0.5;
                Candle {
                    ts: 1_700_000_000 + i * 300,
                    open: close - 0.2,
                    high: close + 0.3,
                    low: close - 0.4,
                    close,
                    volume: Some(1000.0),
                }
            })
            .collect();
        let price = candles.last().unwrap().close;
        app.data.insert(
            symbol.into(),
            TickerData {
                quote: Quote {
                    symbol: symbol.into(),
                    price,
                    prev_close: Some(base),
                    currency: Some("USD".into()),
                },
                candles,
            },
        );
    }
    app
}

fn render(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let area = buffer.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            out.push_str(buffer.cell((x, y)).unwrap().symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn table_view_shows_quotes() {
    let mut app = fake_app();
    app.view_idx = 0;
    let screen = render(&mut app);
    assert!(screen.contains("Watchlist"), "screen:\n{screen}");
    assert!(screen.contains("AAPL"), "screen:\n{screen}");
    assert!(screen.contains("214.50"), "screen:\n{screen}"); // 200 + 29*0.5
    assert!(screen.contains("+14.50"), "screen:\n{screen}");
    assert!(screen.contains("+7.25%"), "screen:\n{screen}");
    assert!(screen.contains("▶"), "selection marker missing:\n{screen}");
}

#[test]
fn chart_view_shows_selected_symbol() {
    let mut app = fake_app();
    app.view_idx = 1;
    app.selected = 1;
    let screen = render(&mut app);
    assert!(screen.contains("MSFT"), "screen:\n{screen}");
    assert!(screen.contains("414.50"), "screen:\n{screen}");
    // Braille line-chart cells must be present
    assert!(
        screen.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
        "no chart dots rendered:\n{screen}"
    );
}

#[test]
fn split_view_combines_table_and_chart() {
    let mut app = fake_app();
    app.view_idx = 2;
    let screen = render(&mut app);
    assert!(screen.contains("Watchlist"), "screen:\n{screen}");
    assert!(
        screen.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
        "no chart in split view:\n{screen}"
    );
}

#[test]
fn missing_data_renders_placeholders() {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        vec!["AAPL".into()],
        "yahoo",
        Range::D1,
        Interval::M5,
        rx,
        Arc::new(Notify::new()),
    );
    app.errors.insert("AAPL".into(), "boom".into());
    for view_idx in 0..ui::VIEWS.len() {
        app.view_idx = view_idx;
        let screen = render(&mut app); // must not panic
        assert!(screen.contains("AAPL"), "view {view_idx}:\n{screen}");
    }
}
