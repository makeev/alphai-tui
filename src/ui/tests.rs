use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::Notify;

use crate::alphai::{Article, InsiderSummary, SentimentSummary};
use crate::app::{App, AppInit, ChartStyle, InsiderBundle, NewsBundle};
use crate::config::Config;
use crate::domain::{Candle, Interval, Quote, Range, TickerData};
use crate::source::make_source;
use crate::ui;

fn empty_app(symbols: Vec<String>) -> App {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (alphai_tx, _alphai_rx) = tokio::sync::mpsc::unbounded_channel();
    let source = make_source("yahoo", None, None).unwrap();
    App::new(AppInit {
        symbols,
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        alphai_enabled: true,
        first_run: false,
    })
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}

/// A cell only candlesticks produce: the upper half block is not part of the
/// table sparkline glyph ramp, and the line chart is pure Braille.
fn has_candles(screen: &str) -> bool {
    screen.contains('▀') || screen.contains('▄') || screen.contains('█')
}

fn fake_app() -> App {
    let mut app = empty_app(vec!["AAPL".into(), "MSFT".into()]);
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

fn article(title: &str, ticker: &str, score: i64, sentiment: &str) -> Article {
    serde_json::from_str(&format!(
        r#"{{
          "original": {{
            "title": "{title}",
            "url": "https://example.com/a",
            "time_published": "2026-07-10T12:00:00Z",
            "summary": "Summary of {title}.",
            "source_domain": "example.com"
          }},
          "enrichment": {{
            "category": "earnings",
            "tickers": ["{ticker}"],
            "relevance_score": {score},
            "ai_trading_insights": {{
              "ticker_analysis": [
                {{"ticker": "{ticker}", "impact_analysis": {{"sentiment": "{sentiment}"}}}}
              ]
            }}
          }}
        }}"#
    ))
    .unwrap()
}

fn render(app: &mut App) -> String {
    render_sized(app, 100, 30)
}

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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
    app.view_idx = 2; // Table
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
    app.view_idx = ui::VIEW_CHART;
    app.selected = 1;
    let screen = render(&mut app);
    assert!(screen.contains("MSFT"), "screen:\n{screen}");
    assert!(screen.contains("414.50"), "screen:\n{screen}");
    assert!(has_candles(&screen), "no candle cells rendered:\n{screen}");
}

#[test]
fn chart_toggles_to_line_mode() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_CHART;
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.chart_style, ChartStyle::Line);
    let screen = render(&mut app);
    assert!(
        screen.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
        "no Braille line in line mode:\n{screen}"
    );
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.chart_style, ChartStyle::Candles);
}

#[test]
fn sma_toggle_hides_legend() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_CHART;
    let screen = render(&mut app);
    assert!(screen.contains("SMA20"), "screen:\n{screen}");
    // The 30-candle fixture cannot produce an SMA100 line, so its legend
    // label must not appear either.
    assert!(!screen.contains("SMA100"), "screen:\n{screen}");
    press(&mut app, KeyCode::Char('m'));
    let screen = render(&mut app);
    assert!(!screen.contains("SMA20"), "screen:\n{screen}");
}

/// With warm-up history behind the visible window (fetch_range over-fetches
/// for exactly this) the SMA100 legend appears; rendering also exercises the
/// candle renderer and RSI panel with a non-zero visible-window offset.
#[test]
fn sma_slow_appears_with_warmup_history() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_CHART;
    let candles: Vec<Candle> = (0..600)
        .map(|i| {
            let close = 200.0 + (i % 40) as f64 * 0.5;
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
    app.data.get_mut("AAPL").unwrap().candles = candles;
    let screen = render(&mut app);
    assert!(screen.contains("SMA100"), "screen:\n{screen}");
    press(&mut app, KeyCode::Char('c'));
    let screen = render(&mut app);
    assert!(screen.contains("SMA100"), "screen:\n{screen}");
}

#[test]
fn rsi_toggle_hides_panel() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_CHART;
    let screen = render(&mut app);
    assert!(screen.contains("RSI(14)"), "screen:\n{screen}");
    press(&mut app, KeyCode::Char('i'));
    let screen = render(&mut app);
    assert!(!screen.contains("RSI(14)"), "screen:\n{screen}");
}

#[test]
fn rsi_panel_hidden_on_short_terminal() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_CHART;
    // Body height 16 is below the RSI threshold: price chart keeps it all.
    let screen = render_sized(&mut app, 100, 18);
    assert!(!screen.contains("RSI(14)"), "screen:\n{screen}");
    assert!(has_candles(&screen), "screen:\n{screen}");
}

#[test]
fn range_keys_cycle_presets_and_update_header() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_CHART;
    press(&mut app, KeyCode::Char('t'));
    assert_eq!((app.range, app.interval), (Range::D5, Interval::M15));
    let screen = render(&mut app);
    assert!(screen.contains("· 15m"), "screen:\n{screen}");
    // Wrap backwards past the first preset.
    press(&mut app, KeyCode::Char('T'));
    press(&mut app, KeyCode::Char('T'));
    assert_eq!((app.range, app.interval), (Range::Y1, Interval::D1));
    assert!(render(&mut app).contains("· 1d"));
    // Old data stays on screen until the poller answers.
    assert!(app.data.contains_key("AAPL"));
}

/// Budget invariant: a range switch must wake only the price poller. The
/// visible AlphaAI bundle stays cached (manual_refresh would drop it and
/// trigger a refetch on the next draw).
#[test]
fn range_switch_keeps_news_bundle() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_NEWS;
    app.news.insert(
        "AAPL".into(),
        NewsBundle {
            articles: vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            sentiment: None,
            fetched: Instant::now(),
        },
    );
    press(&mut app, KeyCode::Char('t'));
    assert!(app.news.contains_key("AAPL"), "range switch dropped the news bundle");
}

#[test]
fn split_view_combines_table_chart_and_news() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_SPLIT;
    app.news.insert(
        "AAPL".into(),
        NewsBundle {
            articles: vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            sentiment: None,
            fetched: Instant::now(),
        },
    );
    let screen = render(&mut app);
    assert!(screen.contains("Watchlist"), "screen:\n{screen}");
    assert!(
        screen.contains('▀'),
        "no candle chart in split view:\n{screen}"
    );
    assert!(screen.contains("News · AAPL"), "screen:\n{screen}");
    assert!(screen.contains("Apple beats expectations"), "screen:\n{screen}");
}

#[test]
fn split_view_news_panel_without_key_shows_one_line_hint() {
    let mut app = fake_app();
    app.alphai_enabled = false;
    app.view_idx = ui::VIEW_SPLIT;
    let screen = render(&mut app);
    assert!(screen.contains("alphai.io"), "screen:\n{screen}");
    assert!(screen.contains("press s"), "screen:\n{screen}");
}

#[test]
fn split_view_drops_news_panel_on_tiny_terminal() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_SPLIT;
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut screen = String::new();
    for y in 0..12 {
        for x in 0..100 {
            screen.push_str(buffer.cell((x, y)).unwrap().symbol());
        }
        screen.push('\n');
    }
    assert!(screen.contains("Watchlist"), "screen:\n{screen}");
    assert!(!screen.contains("News ·"), "news strip should be hidden:\n{screen}");
}

#[test]
fn missing_data_renders_placeholders() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.errors.insert("AAPL".into(), "boom".into());
    for view_idx in 0..ui::VIEWS.len() {
        app.view_idx = view_idx;
        let screen = render(&mut app); // must not panic
        assert!(!screen.is_empty(), "view {view_idx} rendered nothing");
    }
}

#[test]
fn news_view_lists_articles_and_sentiment() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_NEWS;
    app.news.insert(
        "AAPL".into(),
        NewsBundle {
            articles: vec![
                article("Apple beats expectations", "AAPL", 9, "positive"),
                article("Supplier note weighs on outlook", "AAPL", 6, "negative"),
            ],
            sentiment: Some(SentimentSummary {
                days: 7,
                total: 20,
                bullish: 12,
                neutral: 5,
                bearish: 3,
            }),
            fetched: Instant::now(),
        },
    );
    let screen = render(&mut app);
    assert!(screen.contains("News · AAPL"), "screen:\n{screen}");
    assert!(screen.contains("Apple beats expectations"), "screen:\n{screen}");
    assert!(screen.contains("12 bullish"), "screen:\n{screen}");
    assert!(screen.contains("▲"), "sentiment glyph missing:\n{screen}");
    // detail pane shows the selected article's summary
    assert!(
        screen.contains("Summary of Apple beats expectations"),
        "screen:\n{screen}"
    );
}

#[test]
fn news_view_without_key_shows_hint() {
    let mut app = fake_app();
    app.alphai_enabled = false;
    app.view_idx = ui::VIEW_NEWS;
    let screen = render(&mut app);
    assert!(screen.contains("alphai.io"), "screen:\n{screen}");
    assert!(screen.contains("free API key"), "screen:\n{screen}");
}

#[test]
fn news_view_shows_error_state() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_NEWS;
    app.alphai_errors
        .insert("AAPL".into(), "invalid AlphaAI API key".into());
    let screen = render(&mut app);
    assert!(screen.contains("invalid AlphaAI API key"), "screen:\n{screen}");
    assert!(screen.contains("press r to retry"), "screen:\n{screen}");
}

#[test]
fn insider_view_shows_summary_and_filings() {
    let mut app = fake_app();
    app.view_idx = ui::VIEW_INSIDER;
    let summary: InsiderSummary = serde_json::from_str(
        r#"{
          "ticker": "AAPL", "days": 30, "total_transactions": 14,
          "buy_count": 2, "sell_count": 12,
          "buy_value_usd": "1240000.00", "sell_value_usd": "224580213.05",
          "pct_10b5_1": 85,
          "top_insiders": [{"name": "COOK TIMOTHY", "title": "CEO", "transaction_count": 3, "net_value": "-50300000.00"}]
        }"#,
    )
    .unwrap();
    app.insider.insert(
        "AAPL".into(),
        InsiderBundle {
            articles: vec![article("Apple insider sold $12.5M of stock", "AAPL", 7, "negative")],
            summary: Some(summary),
            fetched: Instant::now(),
        },
    );
    let screen = render(&mut app);
    assert!(screen.contains("Insider · AAPL"), "screen:\n{screen}");
    assert!(screen.contains("14 filings"), "screen:\n{screen}");
    assert!(screen.contains("$224.6M"), "screen:\n{screen}");
    assert!(screen.contains("85% under 10b5-1"), "screen:\n{screen}");
    assert!(screen.contains("COOK TIMOTHY"), "screen:\n{screen}");
    assert!(
        screen.contains("Apple insider sold $12.5M of stock"),
        "screen:\n{screen}"
    );
}

#[test]
fn settings_overlay_masks_keys() {
    let mut app = fake_app();
    app.config.keys.alphai = Some("ak_live_abcdefgh1234".into());
    app.config.keys.alpaca_secret = Some("alpaca-secret-abcd9876".into());
    app.open_settings();
    let screen = render(&mut app);
    assert!(screen.contains("Settings"), "screen:\n{screen}");
    assert!(screen.contains("Price source"), "screen:\n{screen}");
    assert!(screen.contains("Alpaca secret"), "screen:\n{screen}");
    assert!(screen.contains("News opens"), "screen:\n{screen}");
    assert!(screen.contains("‹ alphai ›"), "screen:\n{screen}");
    assert!(screen.contains("ak_liv…1234"), "screen:\n{screen}");
    assert!(
        !screen.contains("ak_live_abcdefgh1234"),
        "raw key leaked to screen:\n{screen}"
    );
    assert!(screen.contains("alpaca…9876"), "screen:\n{screen}");
    assert!(
        !screen.contains("alpaca-secret-abcd9876"),
        "raw alpaca secret leaked to screen:\n{screen}"
    );
}

#[test]
fn first_run_opens_settings_with_welcome() {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (alphai_tx, _alphai_rx) = tokio::sync::mpsc::unbounded_channel();
    let source = make_source("yahoo", None, None).unwrap();
    let mut app = App::new(AppInit {
        symbols: vec!["AAPL".into()],
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        alphai_enabled: false,
        first_run: true,
    });
    assert!(app.settings.open);
    let screen = render(&mut app);
    assert!(screen.contains("Welcome to alphai-tui"), "screen:\n{screen}");
    assert!(screen.contains("https://alphai.io"), "screen:\n{screen}");
}
