use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::Notify;

use crate::alphai::{self, Article, FeedPayload, InsiderTrades, SentimentSummary, TRENDING_KEY};
use crate::app::{
    App, AppInit, ChartStyle, FeedBundle, NewsLayout, NewsScope, PRICE_FLASH, SettingsRow,
    settings_rows,
};
use crate::config::{ChartDefaults, Config, UiDefaults};
use crate::domain::{Candle, Interval, Quote, Range, Sessions, TickerData};
use crate::poller::SourceEvent;
use crate::portfolio::Position;
use crate::source::make_source;
use crate::theme::Theme;
use crate::ui;

/// A quote carrying only what a test sets. The optional extras (extended
/// price, 52 week range, volume, name) default to absent, so adding one
/// more of them later does not mean editing every construction site.
fn plain_quote(symbol: &str, price: f64, prev_close: Option<f64>, currency: Option<&str>) -> Quote {
    Quote {
        timing: Default::default(),
        symbol: symbol.into(),
        price,
        prev_close,
        currency: currency.map(Into::into),
        extended: None,
        fifty_two_week: None,
        day_range: None,
        volume: None,
    }
}

fn empty_app(symbols: Vec<String>) -> App {
    let (app, _rx) = empty_app_with_cmds(symbols);
    app
}

/// Like `empty_app`, but keeps the AlphAI command receiver alive so tests
/// can assert which fetches the app requested.
fn empty_app_with_cmds(
    symbols: Vec<String>,
) -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (alphai_tx, alphai_rx) = tokio::sync::mpsc::unbounded_channel();
    let source = make_source("yahoo", &Config::default()).unwrap();
    let app = App::new(AppInit {
        positions: Vec::new(),
        shared_symbols: Arc::new(RwLock::new(symbols.clone())),
        symbols,
        sessions: Sessions::Regular,
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5, Sessions::Regular))),
        every: Arc::new(RwLock::new(std::time::Duration::from_secs(15))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        config_path: None,
        theme: Theme::default(),
        theme_name: crate::theme::DEFAULT_PRESET,
        chart: ChartDefaults::default(),
        ui: UiDefaults::default(),
        keymap: crate::keymap::Keymap::default(),
        alphai_enabled: true,
        first_run: false,
        source_fallback: true,
    });
    (app, alphai_rx)
}

fn current_source(app: &App) -> Arc<dyn crate::source::DataSource> {
    app.price_source()
}

#[test]
fn late_results_from_an_old_candle_request_are_discarded() {
    let mut app = fake_app();
    let source = current_source(&app);
    let original = app.data["AAPL"].quote.price;
    let mut data = app.data["AAPL"].clone();
    data.quote.price = 999.0;
    app.apply(SourceEvent::Data {
        params: Some((Range::D1, Interval::M5, Sessions::Extended)),
        source: source.clone(),
        symbol: "AAPL".into(),
        data,
    });
    assert_eq!(app.data["AAPL"].quote.price, original);
    app.apply(SourceEvent::Error {
        params: Some((Range::D5, Interval::M15, Sessions::Regular)),
        source,
        symbol: "AAPL".into(),
        error: "old request failed".into(),
    });
    assert!(!app.errors.contains_key("AAPL"));
}

#[test]
fn session_backgrounds_and_time_grid_share_all_three_panels() {
    use crate::domain::PriceFeed;
    let mut app = empty_app(vec!["AAPL".into()]);
    app.sessions = Sessions::Extended;
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.show_news_markers = false;
    app.theme = Theme::resolve(None, Some("catppuccin-mocha"), &mut Vec::new()).0;
    let first = chrono::DateTime::parse_from_rfc3339("2026-09-11T08:00:00Z")
        .unwrap()
        .timestamp();
    let candles: Vec<_> = (0..192)
        .map(|i| {
            let price = 100.0 + i as f64 * 0.02 + (i as f64 / 8.0).sin();
            Candle {
                ts: first + i * 300,
                open: price,
                high: price + 0.3,
                low: price - 0.2,
                close: price + (i as f64 / 3.0).sin() * 0.15,
                volume: Some(100.0 + (i % 13) as f64 * 100.0),
                feed: if (66..144).contains(&i) {
                    PriceFeed::Iex
                } else {
                    PriceFeed::DelayedSip
                },
            }
        })
        .collect();
    app.data.insert(
        "AAPL".into(),
        TickerData {
            quote: plain_quote("AAPL", 103.0, Some(99.5), Some("USD")),
            candles,
        },
    );
    for style in [ChartStyle::Candles, ChartStyle::Line] {
        app.chart_style = style;
        for (width, height) in [(160, 44), (80, 34), (44, 22), (20, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            if width != 160 {
                continue;
            }
            let lines: Vec<String> = (0..height)
                .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
                .collect();
            let text = lines.join("\n");
            assert!(text.contains("09:30") && text.contains("16:00"), "{text}");
            assert!(text.contains("SIP · delayed 15m"), "{text}");
            let vol = lines
                .iter()
                .position(|line| line.contains(" Vol "))
                .unwrap() as u16
                + 1;
            let rsi = lines
                .iter()
                .position(|line| line.contains(" RSI("))
                .unwrap() as u16
                + 1;
            let price = lines
                .iter()
                .position(|line| line.contains("SMA20"))
                .unwrap() as u16
                + 1;
            for color in [app.theme.pre_market_bg, app.theme.post_market_bg] {
                let cols = |y| {
                    (0..width)
                        .filter(|x| buffer[(*x, y)].bg == color)
                        .collect::<Vec<_>>()
                };
                assert!(!cols(price).is_empty(), "session background missing");
                assert_eq!(cols(price), cols(vol), "volume session columns differ");
                assert_eq!(cols(price), cols(rsi), "RSI session columns differ");
            }
            if let Ok(dir) = std::env::var("ALPHAI_TEST_RENDER_DIR") {
                let cells: Vec<_> = (0..height).flat_map(|y| (0..width).map(move |x| {
                    let cell = &buffer[(x, y)];
                    serde_json::json!({"x":x,"y":y,"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})
                })).collect();
                let name = if style == ChartStyle::Candles {
                    "candles"
                } else {
                    "line"
                };
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("{name}.json")),
                    serde_json::to_vec(
                        &serde_json::json!({"width":width,"height":height,"cells":cells}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
        }
    }
}

#[test]
fn short_premarket_charts_use_the_plot_width_at_every_interval() {
    use crate::domain::PriceFeed;
    let mut app = empty_app(vec!["AAPL".into()]);
    app.sessions = Sessions::Extended;
    app.show_sma = false;
    app.show_volume = false;
    app.show_rsi = false;
    app.show_news_markers = false;
    let first = chrono::DateTime::parse_from_rfc3339("2026-09-14T08:00:00Z")
        .unwrap()
        .timestamp();
    let mut starts = Vec::new();
    for interval in [Interval::M15, Interval::M5, Interval::M30] {
        app.interval = interval;
        let candles: Vec<_> = (0..7200 / interval.secs())
            .map(|i| Candle {
                ts: first + i * interval.secs(),
                open: 100.0 + i as f64 * 0.1,
                high: 100.3 + i as f64 * 0.1,
                low: 99.8 + i as f64 * 0.1,
                close: 100.2 + i as f64 * 0.1,
                feed: PriceFeed::Yahoo,
                ..Default::default()
            })
            .collect();
        app.data.insert(
            "AAPL".into(),
            TickerData {
                quote: plain_quote("AAPL", 100.0, None, Some("USD")),
                candles,
            },
        );
        for style in [ChartStyle::Candles, ChartStyle::Line] {
            app.chart_style = style;
            let mut terminal = Terminal::new(TestBackend::new(160, 24)).unwrap();
            terminal
                .draw(|f| ui::chart::render_chart(f, f.area(), &app))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let first_x = (1..159)
                .find(|&x| {
                    (1..21).any(|y| {
                        let cell = &buffer[(x, y)];
                        let glyph = cell.symbol().chars().next().unwrap_or(' ');
                        matches!(
                            glyph,
                            '█' | '▀' | '▄' | '│' | '╷' | '╵' | '\u{2801}'..='\u{28ff}'
                        ) && [app.theme.up, app.theme.down, app.theme.flat].contains(&cell.fg)
                    })
                })
                .unwrap();
            starts.push((interval, style, first_x));
        }
    }
    assert!(
        starts.iter().all(|(_, _, x)| *x <= 10),
        "first plotted column: {starts:?}"
    );
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}

/// A cell only candlesticks produce: the upper half block is not part of the
/// table sparkline glyph ramp, and the line chart is pure Braille.
fn has_candles(screen: &str) -> bool {
    let body = panels(screen);
    body.contains('▀') || body.contains('▄') || body.contains('█')
}

/// The screen from the first framed panel down, dropping the header and the
/// quote rail. Both draw glyphs of their own: the rail ends in a sparkline
/// whose full bar is `█`, and which zones it has room for depends on the
/// market session, so a whole-screen scan for candle bodies read the rail on a
/// weekday and not on a weekend.
fn panels(screen: &str) -> &str {
    screen.find('╭').map_or(screen, |at| &screen[at..])
}

fn fake_app() -> App {
    let mut app = empty_app(vec!["AAPL".into(), "MSFT".into()]);
    for (symbol, base) in [("AAPL", 200.0), ("MSFT", 400.0)] {
        let candles: Vec<Candle> = (0..30)
            .map(|i| {
                let close = base + i as f64 * 0.5;
                Candle {
                    feed: Default::default(),
                    ts: 1_700_000_000 + i * 300,
                    open: close - 0.2,
                    high: close + 0.3,
                    low: close - 0.4,
                    close,
                    // Uneven on purpose: equal volumes render as one solid
                    // block and would hide bar-height bugs.
                    volume: Some(1000.0 + (i % 5) as f64 * 400.0),
                }
            })
            .collect();
        let price = candles.last().unwrap().close;
        app.data.insert(
            symbol.into(),
            TickerData {
                quote: plain_quote(symbol, price, Some(base), Some("USD")),
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
                {{"ticker": "{ticker}", "impact_analysis": {{
                  "sentiment": "{sentiment}",
                  "confidence": "high",
                  "price_impact_prediction": "+2-4% near term",
                  "summary": "Impact summary for {title}."
                }}}}
              ],
              "news_trading_value": {{
                "actionability_score": "high",
                "information_novelty": 7,
                "timing_relevance": "pre-market"
              }},
              "alternative_perspectives": {{
                "contrarian_view": "Priced in already.",
                "overlooked_factors": "Margins are thinning."
              }}
            }},
            "news_context_enhancement": {{
              "background_context": "Follows last quarter's beat.",
              "key_entities": [
                {{"name": "Acme", "type": "company", "description": "widget maker"}}
              ],
              "market_relevance_summary": "Read-through for the sector."
            }}
          }}
        }}"#
    ))
    .unwrap()
}

/// A story-collapsed row as the market and trending feeds return them: two
/// tickers and an outlet count.
fn market_article(title: &str, sources: i64) -> Article {
    let mut a = article(title, "AAPL", 8, "positive");
    a.enrichment.tickers.push("MSFT".into());
    a.sources_count = Some(sources);
    a
}

/// A Form 4 row: templated title, ownership form, no AI enrichment (the
/// sentiment glyph must come from the title fallback).
fn filing(title: &str, form: &str) -> Article {
    serde_json::from_str(&format!(
        r#"{{
          "original": {{
            "title": "{title}",
            "url": "https://example.com/f",
            "time_published": "2026-07-10T12:00:00Z",
            "summary": "Summary of {title}.",
            "source_domain": "sec.gov",
            "ownership_form": "{form}"
          }},
          "enrichment": {{"category": "insider", "tickers": ["AAPL"], "relevance_score": 7}}
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
    app.view_idx = ui::view_index(ui::ViewId::Table);
    let screen = render(&mut app);
    assert!(screen.contains("Watchlist"), "screen:\n{screen}");
    assert!(screen.contains("AAPL"), "screen:\n{screen}");
    assert!(screen.contains("214.50"), "screen:\n{screen}"); // 200 + 29*0.5
    assert!(screen.contains("+14.50"), "screen:\n{screen}");
    assert!(screen.contains("+7.25%"), "screen:\n{screen}");
    assert!(screen.contains("▶"), "selection marker missing:\n{screen}");
}

/// The extended column earns its width only while there is a late print
/// to put in it, so it has to appear and disappear with the data rather
/// than sit empty through the session.
#[test]
fn table_shows_the_extended_column_only_when_a_row_has_one() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    assert!(
        !render(&mut app).contains("Ext"),
        "no row has a late print yet"
    );

    if let Some(data) = app.data.get_mut("AAPL") {
        // 214.50 in the session, 216.65 after the bell: +1.00%.
        data.quote.extended = Some(216.65);
    }
    let screen = render(&mut app);
    assert!(screen.contains("Ext"), "screen:\n{screen}");
    assert!(screen.contains("+1.00%"), "screen:\n{screen}");
    // MSFT has no late print, and an empty cell says that better than a
    // dash, which would read as missing data.
    assert!(!screen.contains("—"), "screen:\n{screen}");
}

/// The watchlist used to be reachable only through CLI arguments or a
/// hand-edited config, so following a name someone mentioned meant
/// quitting the app.
#[test]
fn a_adds_a_ticker_and_the_poller_sees_it() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('a'));
    assert!(app.prompt.open);
    for c in "nvda".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);

    assert!(!app.prompt.open);
    assert_eq!(app.symbols, vec!["AAPL", "MSFT", "NVDA"], "upper-cased");
    assert_eq!(app.selected, 2, "the new ticker is selected");
    // The poller reads its watchlist from the shared handle, so an add
    // that stops at `app.symbols` would never be polled.
    assert_eq!(*app.shared_symbols.read().unwrap(), app.symbols);
}

/// Typing has to win over the key bindings while the prompt is open, or a
/// ticker with a bound letter in it cannot be typed at all.
#[test]
fn the_prompt_swallows_action_keys() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('a'));
    for c in "ddog".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.prompt.input, "ddog");
    assert_eq!(app.symbols.len(), 2, "nothing was removed by the d keys");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.symbols, vec!["AAPL", "MSFT", "DDOG"]);
}

#[test]
fn adding_a_ticker_twice_says_so_and_moves_the_cursor() {
    let mut app = fake_app();
    app.selected = 0;
    press(&mut app, KeyCode::Char('a'));
    for c in "MSFT".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);

    assert!(app.prompt.open, "stays open with the message");
    assert!(
        app.prompt.error.is_some_and(|e| e.contains("MSFT")),
        "the reason has to name the ticker"
    );
    assert_eq!(app.symbols.len(), 2, "no duplicate row");
    assert_eq!(app.selected, 1, "cursor moved to where it already is");
}

#[test]
fn d_removes_the_selected_ticker_but_never_the_last_one() {
    let mut app = fake_app();
    app.selected = 1;
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.symbols, vec!["AAPL"]);
    assert_eq!(app.selected, 0, "cursor followed the shrinking list");
    assert_eq!(*app.shared_symbols.read().unwrap(), app.symbols);
    assert!(!app.data.contains_key("MSFT"), "stale price dropped");

    // Every view indexes the watchlist by the selection, so emptying it
    // would panic rather than show an empty state.
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.symbols, vec!["AAPL"]);
}

#[test]
fn escape_closes_the_prompt_without_adding() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('a'));
    for c in "TSLA".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Esc);
    assert!(!app.prompt.open);
    assert_eq!(app.symbols, vec!["AAPL", "MSFT"]);
}

/// The point of the view: every ticker's shape at once, which the table's
/// one-row sparkline cannot give.
#[test]
fn summary_view_charts_every_ticker() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Summary);
    let screen = render(&mut app);
    assert!(screen.contains("AAPL"), "screen:\n{screen}");
    assert!(screen.contains("MSFT"), "screen:\n{screen}");
    assert!(screen.contains("214.50"), "AAPL price in the card title");
    assert!(screen.contains("414.50"), "MSFT price in the card title");
    // Braille is what the card plots with; the table's block ramp is not
    // in this view at all.
    assert!(
        screen
            .chars()
            .any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
        "no plotted line:\n{screen}"
    );
}

#[test]
fn summary_marks_the_selected_card() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Summary);
    let first = render(&mut app);
    assert!(first.contains("▶ AAPL"), "screen:\n{first}");
    assert!(!first.contains("▶ MSFT"), "screen:\n{first}");

    press(&mut app, KeyCode::Down);
    let second = render(&mut app);
    assert!(second.contains("▶ MSFT"), "screen:\n{second}");
    assert!(!second.contains("▶ AAPL"), "screen:\n{second}");
}

/// More tickers than cards on screen: the page follows the cursor, so ↑↓
/// keep working as the only navigation without a scroll state of its own.
#[test]
fn summary_pages_with_the_cursor() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Summary);
    for extra in ["NVDA", "AMD", "TSLA", "META"] {
        app.symbols.push(extra.into());
    }
    // One card fits: 40 columns is a single column, 8 rows one row of cards.
    let first = render_sized(&mut app, 40, 8);
    assert!(first.contains("AAPL"), "screen:\n{first}");
    assert!(!first.contains("META"), "screen:\n{first}");

    app.selected = app.symbols.len() - 1;
    let last = render_sized(&mut app, 40, 8);
    assert!(last.contains("META"), "screen:\n{last}");
    assert!(!last.contains("AAPL"), "screen:\n{last}");
}

/// The extended candles are a different fetch, not a different render, so
/// the toggle has to reach the poller's parameters.
#[test]
fn e_asks_the_poller_for_the_extended_candles() {
    let mut app = fake_app();
    assert_eq!(app.sessions, Sessions::Regular);
    press(&mut app, KeyCode::Char('E'));
    assert_eq!(app.sessions, Sessions::Extended);
    assert_eq!(app.params.read().unwrap().2, Sessions::Extended);
    press(&mut app, KeyCode::Char('E'));
    assert_eq!(app.params.read().unwrap().2, Sessions::Regular);
}

/// Once pre and post market candles are drawn, folding the day's high and
/// low out of the candles stops meaning "the session". The source states
/// the session's own range, so the rail uses that.
#[test]
fn the_rail_day_range_comes_from_the_source_not_the_candles() {
    let mut app = fake_app();
    if let Some(data) = app.data.get_mut("AAPL") {
        data.quote.day_range = Some((200.0, 220.0));
        // A candle from an extended session, well outside it.
        data.candles.push(Candle {
            feed: Default::default(),
            ts: data.candles.last().unwrap().ts + 300,
            open: 214.5,
            high: 260.0,
            low: 190.0,
            close: 214.5,
            volume: None,
        });
    }
    let rail = ui::rail::text(&ui::rail::line(&app, 130, market_open_moment()));
    assert!(rail.contains("200.00"), "{rail}");
    assert!(rail.contains("220.00"), "{rail}");
    assert!(
        !rail.contains("260.00"),
        "the extended high is not the day's"
    );
}

#[test]
fn chart_view_shows_selected_symbol() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.selected = 1;
    let screen = render(&mut app);
    assert!(screen.contains("MSFT"), "screen:\n{screen}");
    assert!(screen.contains("414.50"), "screen:\n{screen}");
    assert!(has_candles(&screen), "no candle cells rendered:\n{screen}");
}

#[test]
fn chart_toggles_to_line_mode() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.chart_style, ChartStyle::Line);
    let screen = render(&mut app);
    assert!(
        screen
            .chars()
            .any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
        "no Braille line in line mode:\n{screen}"
    );
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.chart_style, ChartStyle::Candles);
}

/// The default 20% right margin keeps candles off the right edge and hosts
/// the last-price tag; `right_margin_pct = 0` restores the old flush layout.
#[test]
fn right_margin_frees_columns_and_hosts_the_price_tag() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.show_rsi = false;
    // Volume bars share the candles' glyphs; this is about the price plot.
    app.show_volume = false;
    // The tag is counted by occurrences of the price on screen, and the
    // rail prints it too.
    app.show_rail = false;
    let max_body_x = |screen: &str| {
        screen
            .lines()
            .flat_map(|l| {
                l.chars()
                    .enumerate()
                    .filter(|(_, c)| matches!(c, '█' | '▀' | '▄'))
                    .map(|(i, _)| i)
            })
            .max()
            .unwrap()
    };

    let screen = render(&mut app);
    let with_margin = max_body_x(&screen);
    // The price appears in the title and again as the tag in the margin.
    assert!(
        screen.matches("214.50").count() >= 2,
        "price tag missing:\n{screen}"
    );

    app.chart.right_margin_pct = 0;
    let screen = render(&mut app);
    let flush = max_body_x(&screen);
    assert_eq!(
        screen.matches("214.50").count(),
        1,
        "margin off, tag still drawn:\n{screen}"
    );
    assert!(
        with_margin + 10 <= flush,
        "margin freed no columns: {with_margin} vs {flush}\n{screen}"
    );
}

/// Line mode renders with the margin (marker line into it) and without.
#[test]
fn line_mode_renders_with_and_without_margin() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    press(&mut app, KeyCode::Char('c'));
    let braille = |s: &str| s.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c));
    assert!(braille(&render(&mut app)), "no line with the margin on");
    app.chart.right_margin_pct = 0;
    assert!(braille(&render(&mut app)), "no line with the margin off");
}

#[test]
fn price_flash_tracks_update_direction() {
    let mut app = empty_app(vec!["AAPL".into()]);
    let source = current_source(&app);
    let data = |price: f64| SourceEvent::Data {
        params: None,
        source: source.clone(),
        symbol: "AAPL".into(),
        data: TickerData {
            quote: plain_quote("AAPL", price, Some(price), None),
            candles: vec![],
        },
    };
    app.apply(data(100.0));
    assert_eq!(
        app.price_flash_dir("AAPL"),
        None,
        "first data must not flash"
    );
    app.apply(data(101.0));
    assert_eq!(app.price_flash_dir("AAPL"), Some(true));
    app.apply(data(100.5));
    assert_eq!(app.price_flash_dir("AAPL"), Some(false));
    app.price_flash.clear();
    app.apply(data(100.5));
    assert_eq!(
        app.price_flash_dir("AAPL"),
        None,
        "unchanged price must not flash"
    );
}

/// The live quote is folded into the in-progress last candle on apply, so
/// the candle redraws with every price tick instead of waiting for the
/// source's bar series to catch up.
#[test]
fn live_quote_updates_the_last_candle() {
    let mut app = empty_app(vec!["AAPL".into()]);
    let source = current_source(&app);
    let data = |price: f64| SourceEvent::Data {
        params: None,
        source: source.clone(),
        symbol: "AAPL".into(),
        data: TickerData {
            quote: Quote {
                timing: crate::domain::QuoteTiming {
                    regular: Some(1_783_698_320),
                    regular_feed: crate::domain::PriceFeed::Iex,
                    ..Default::default()
                },
                ..plain_quote("AAPL", price, Some(100.0), None)
            },
            candles: vec![
                Candle {
                    feed: crate::domain::PriceFeed::Iex,
                    ts: 1_783_697_700,
                    open: 99.0,
                    high: 100.0,
                    low: 98.0,
                    close: 99.5,
                    volume: None,
                },
                Candle {
                    feed: crate::domain::PriceFeed::Iex,
                    ts: 1_783_698_300,
                    open: 100.0,
                    high: 101.0,
                    low: 99.5,
                    close: 100.5,
                    volume: None,
                },
            ],
        },
    };
    // Price below the stale bar close: close follows, low extends down.
    app.apply(data(99.0));
    let last = *app.data["AAPL"].candles.last().unwrap();
    assert_eq!(last.close, 99.0);
    assert_eq!(last.low, 99.0);
    assert_eq!(last.high, 101.0);
    // Price above the bar high: close follows, high extends up.
    app.apply(data(101.5));
    let last = *app.data["AAPL"].candles.last().unwrap();
    assert_eq!(last.close, 101.5);
    assert_eq!(last.high, 101.5);
    assert_eq!(last.low, 99.5);
    // Earlier bars stay untouched.
    assert_eq!(app.data["AAPL"].candles[0].close, 99.5);
}

fn reversed_cells(app: &mut App) -> Vec<ratatui::style::Color> {
    use ratatui::style::Modifier;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = Vec::new();
    for y in 0..30 {
        for x in 0..100 {
            let cell = buffer.cell((x, y)).unwrap();
            if cell.modifier.contains(Modifier::REVERSED) {
                out.push(cell.fg);
            }
        }
    }
    out
}

/// An active pulse inverts the price (title and margin tag) in the tick's
/// color; an expired one reverts on the next frame. Nothing else in the
/// Chart view uses REVERSED, so the cell scan is unambiguous.
#[test]
fn price_flash_inverts_the_price_on_the_chart() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    assert!(
        reversed_cells(&mut app).is_empty(),
        "inverted cells without a flash"
    );

    app.price_flash
        .insert("AAPL".into(), (Instant::now(), true));
    let cells = reversed_cells(&mut app);
    assert!(!cells.is_empty(), "flash did not invert the price");
    assert!(
        cells.iter().all(|&fg| fg == app.theme.up),
        "up tick must use the up color"
    );

    app.price_flash
        .insert("AAPL".into(), (Instant::now() - PRICE_FLASH, true));
    assert!(
        reversed_cells(&mut app).is_empty(),
        "expired flash still inverted"
    );
}

/// The candle-mode SMA overlay draws connected braille lines, not the old
/// per-column dots. RSI is off so the price panel is the only braille source.
#[test]
fn candle_sma_overlay_is_a_braille_line() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.show_rsi = false;
    let braille = |s: &str| s.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c));
    let screen = render(&mut app);
    assert!(has_candles(&screen), "not in candle mode:\n{screen}");
    assert!(braille(&screen), "no braille SMA overlay:\n{screen}");
    // The header and footer use "·" as a separator; only the plot matters.
    let plot: String = screen.lines().skip(2).take(25).collect();
    assert!(
        !plot.contains('·'),
        "old per-column SMA dots still drawn:\n{screen}"
    );
    press(&mut app, KeyCode::Char('m'));
    let screen = render(&mut app);
    assert!(
        !braille(&screen),
        "SMA hidden but braille remains:\n{screen}"
    );
}

#[test]
fn sma_toggle_hides_legend() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
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
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let candles: Vec<Candle> = (0..600)
        .map(|i| {
            let close = 200.0 + (i % 40) as f64 * 0.5;
            Candle {
                feed: Default::default(),
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

/// An article published at a given second, for the chart's news marks: the
/// shared helper dates every row to the same fixed moment, which no
/// candle series in these tests covers.
fn article_at(title: &str, ts: i64, score: i64, sentiment: &str) -> Article {
    let mut a = article(title, "AAPL", score, sentiment);
    a.original.time_published = chrono::DateTime::from_timestamp(ts, 0)
        .unwrap()
        .to_rfc3339();
    a
}

/// The news the app already holds for a ticker, marked on the candles it
/// was published in: shape for the AI sentiment call, and the freshest
/// headline on the bottom border. `n` takes them away again.
#[test]
fn chart_marks_the_tickers_news_on_its_candles() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    // Candle 20 of AAPL's series, and candle 6.
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![
                article_at("Apple wins the appeal", 1_700_006_000, 9, "positive"),
                article_at("A supplier walks away", 1_700_001_900, 8, "negative"),
            ],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    // The rail draws its own arrows, so only the panels count here.
    let body = panels(&screen);
    assert!(body.contains('▲'), "bullish mark missing:\n{screen}");
    assert!(body.contains('▼'), "bearish mark missing:\n{screen}");
    assert!(
        body.contains("Apple wins the appeal"),
        "the freshest headline is not on the border:\n{screen}"
    );
    assert!(
        !body.contains("A supplier walks away"),
        "only the freshest mark is named:\n{screen}"
    );

    press(&mut app, KeyCode::Char('n'));
    assert!(!app.show_news_markers);
    let screen = render(&mut app);
    let body = panels(&screen);
    assert!(!body.contains('▲'), "mark survived the toggle:\n{screen}");
    assert!(
        !body.contains("Apple wins the appeal"),
        "headline survived the toggle:\n{screen}"
    );
}

/// A story older than the window does not get pulled onto its left edge,
/// where it would claim a move it had nothing to do with.
#[test]
fn chart_marks_ignore_news_older_than_the_window() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article_at(
                "Long before the window",
                1_699_900_000,
                9,
                "positive",
            )],
            None,
            None,
        ),
    );
    let body = panels(&render(&mut app)).to_string();
    assert!(!body.contains('▲'), "an old row was marked:\n{body}");
    assert!(!body.contains("Long before"), "and named:\n{body}");
}

/// Line mode cannot draw the sentiment shapes (the Chart widget owns its
/// markers), so the marks become dots in the same colors, lifted off the
/// close line so they are not hidden under it.
#[test]
fn line_mode_marks_the_news_with_dots() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article_at(
                "Apple wins the appeal",
                1_700_006_000,
                9,
                "positive",
            )],
            None,
            None,
        ),
    );
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.chart_style, ChartStyle::Line);
    let body = panels(&render(&mut app)).to_string();
    assert!(body.contains('•'), "no news dot in line mode:\n{body}");
    assert!(body.contains("Apple wins the appeal"), "{body}");
}

/// The marks are a garnish on cached rows: the Chart view still fetches
/// nothing, which is what keeps the free tier's 100 requests a day intact.
#[test]
fn chart_marks_never_cost_a_request() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.ensure_alphai_data();
    assert!(
        cmds.try_recv().is_err(),
        "the chart view asked for a feed of its own"
    );
}

#[test]
fn rsi_toggle_hides_panel() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let screen = render(&mut app);
    assert!(screen.contains("RSI(14)"), "screen:\n{screen}");
    press(&mut app, KeyCode::Char('i'));
    let screen = render(&mut app);
    assert!(!screen.contains("RSI(14)"), "screen:\n{screen}");
}

#[test]
fn rsi_panel_hidden_on_short_terminal() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    // Body height 16 is below the RSI threshold: price chart keeps it all.
    let screen = render_sized(&mut app, 100, 18);
    assert!(!screen.contains("RSI(14)"), "screen:\n{screen}");
    assert!(has_candles(&screen), "screen:\n{screen}");
}

/// `e` reweights the same two overlays: same periods, same colors, only the
/// legend and the math change.
#[test]
fn ma_type_key_switches_the_overlays_to_ema() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let braille = |s: &str| s.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c));
    let screen = render(&mut app);
    assert!(screen.contains("SMA20"), "screen:\n{screen}");
    press(&mut app, KeyCode::Char('e'));
    let screen = render(&mut app);
    assert!(screen.contains("EMA20"), "screen:\n{screen}");
    assert!(!screen.contains("SMA20"), "screen:\n{screen}");
    assert!(braille(&screen), "no overlay line left:\n{screen}");
    // Line mode reads the same switch.
    press(&mut app, KeyCode::Char('c'));
    assert!(render(&mut app).contains("EMA20"), "line mode kept SMA");
    press(&mut app, KeyCode::Char('e'));
    assert!(
        render(&mut app).contains("SMA20"),
        "the toggle does not go back"
    );
}

/// The volume panel ships on and rides the `b` toggle.
#[test]
fn volume_toggle_hides_panel() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let screen = render(&mut app);
    assert!(screen.contains("Vol"), "screen:\n{screen}");
    press(&mut app, KeyCode::Char('b'));
    let screen = render(&mut app);
    assert!(!screen.contains("Vol"), "screen:\n{screen}");
}

/// Space goes to the price chart first: RSI keeps the height at which it has
/// always appeared, and volume waits for a taller terminal.
#[test]
fn volume_panel_yields_to_the_price_chart() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let screen = render_sized(&mut app, 100, 24);
    assert!(screen.contains("RSI(14)"), "screen:\n{screen}");
    assert!(!screen.contains("Vol"), "screen:\n{screen}");
    assert!(has_candles(&screen), "screen:\n{screen}");
    // With RSI out of the way the same terminal has room for volume.
    press(&mut app, KeyCode::Char('i'));
    let screen = render_sized(&mut app, 100, 24);
    assert!(screen.contains("Vol"), "screen:\n{screen}");
}

/// Finnhub synthesizes candles from ticks and carries no volume; an empty
/// panel would cost the price chart rows for nothing.
#[test]
fn volume_panel_absent_without_data() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    for data in app.data.values_mut() {
        for candle in &mut data.candles {
            candle.volume = None;
        }
    }
    let screen = render(&mut app);
    assert!(!screen.contains("Vol"), "screen:\n{screen}");
    assert!(has_candles(&screen), "screen:\n{screen}");
}

/// Every bar sits under its candle: both panels end in the same column, and
/// the right margin stays clear in both.
#[test]
fn volume_bars_share_the_candle_columns() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let screen = render_sized(&mut app, 100, 40);
    let lines: Vec<&str> = screen.lines().collect();
    let vol_top = lines
        .iter()
        .position(|l| l.contains("Vol"))
        .expect("volume panel");
    let rsi_top = lines
        .iter()
        .position(|l| l.contains("RSI("))
        .expect("rsi panel");
    let max_body_x = |rows: &[&str]| {
        rows.iter()
            .flat_map(|l| {
                l.chars()
                    .enumerate()
                    .filter(|(_, c)| matches!(c, '█' | '▀' | '▄'))
                    .map(|(i, _)| i)
            })
            .max()
    };
    let chart_top = lines
        .iter()
        .position(|l| l.starts_with('╭'))
        .expect("chart panel");
    let candles = max_body_x(&lines[chart_top..vol_top]).expect("candles");
    let bars = max_body_x(&lines[vol_top + 1..rsi_top]).expect("bars");
    assert_eq!(
        candles, bars,
        "bars and candles end in different columns:\n{screen}"
    );
    // Both stop well short of the right border: that is the price margin.
    assert!(bars < 90, "the margin is not clear: {bars}\n{screen}");
}

/// Every view's footer must fit the 110-column terminal the README promises.
/// The chart hints were at 107 of them before the volume key arrived.
#[test]
fn footer_hints_fit_110_columns() {
    let mut app = fake_app();
    for (i, view) in ui::VIEWS.iter().enumerate() {
        app.view_idx = i;
        let width = crate::ui::hints_text(&app).chars().count();
        assert!(
            width <= 110,
            "{:?}: footer is {width} columns wide",
            view.id()
        );
    }
}

/// A source that has stopped answering is not a screen full of "error":
/// the last rows stay up, labelled with their age, until something live
/// replaces them.
#[test]
fn cached_rows_open_the_app_and_the_rail_says_how_old_they_are() {
    let mut app = empty_app(vec!["AAPL".into()]);
    let entry = crate::cache::Entry {
        // Two hours old, as an overnight restart would find it.
        fetched: chrono::Utc::now().timestamp() - 7_200,
        params: "1d/5m/regular".into(),
        data: TickerData {
            quote: plain_quote("AAPL", 214.5, Some(200.0), Some("USD")),
            candles: vec![
                Candle {
                    feed: Default::default(),
                    ts: 1_700_000_000,
                    open: 200.0,
                    high: 215.0,
                    low: 199.0,
                    close: 214.0,
                    volume: None,
                },
                Candle {
                    feed: Default::default(),
                    ts: 1_700_000_300,
                    open: 214.0,
                    high: 215.0,
                    low: 213.0,
                    close: 214.5,
                    volume: None,
                },
            ],
        },
    };
    app.seed_cached("AAPL".into(), entry);
    let rail = ui::rail::text(&ui::rail::line(&app, 130, market_open_moment()));
    assert!(rail.contains("214.50"), "cached price missing: {rail}");
    assert!(rail.contains("cached 2h"), "cached badge missing: {rail}");
    let narrow = ui::rail::line(&app, 32, market_open_moment());
    assert!(narrow.width() <= 32);
    assert!(
        ui::rail::text(&narrow).contains("cached 2h"),
        "freshness must outlive optional quote details"
    );

    // The first live poll retires the badge, and does not pulse: the move
    // from a two hour old price is not a tick that just happened.
    app.apply(SourceEvent::Data {
        params: None,
        source: current_source(&app),
        symbol: "AAPL".into(),
        data: TickerData {
            quote: plain_quote("AAPL", 216.0, Some(200.0), Some("USD")),
            candles: vec![Candle {
                feed: Default::default(),
                ts: 1_700_000_600,
                open: 214.5,
                high: 216.5,
                low: 214.0,
                close: 216.0,
                volume: None,
            }],
        },
    });
    let rail = ui::rail::text(&ui::rail::line(&app, 130, market_open_moment()));
    assert!(
        !rail.contains("cached"),
        "badge outlived the live poll: {rail}"
    );
    assert!(
        app.price_flash_dir("AAPL").is_none(),
        "the first live price pulsed as if it were a tick"
    );
}

/// Yahoo blocks by IP for tens of minutes and no retry shortens that, so a
/// source that has stopped answering every symbol is swapped for one that
/// still works, with the rows left on screen and a line saying what moved.
#[test]
fn a_dead_source_is_swapped_for_a_configured_one() {
    let mut app = fake_app();
    app.config.keys.insert("finnhub".into(), "test-key".into());
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: current_source(&app),
            symbol: symbol.into(),
            error: crate::source::yahoo::THROTTLE_MSG.into(),
        });
    }
    // One failing ticker is a bad ticker; the clock only starts when the
    // whole watchlist is down.
    assert!(app.source_trouble_since.is_some());
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "yahoo", "switched before the grace period");

    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "finnhub");
    assert!(app.errors.is_empty(), "errors survived the swap");
    assert!(
        app.data.contains_key("AAPL"),
        "the rows on screen were thrown away"
    );
    assert!(
        app.notice()
            .is_some_and(|n| n.contains("yahoo stopped answering, switched to finnhub")),
        "the swap went unexplained"
    );
    // Wide enough for the hints and the notice behind them: on a narrow
    // terminal the header, which names the live source for good, carries it.
    let screen = render_sized(&mut app, 200, 30);
    assert!(
        screen.contains("switched to finnhub"),
        "notice missing from the footer:\n{screen}"
    );
    assert!(screen.contains("· finnhub"), "header still names yahoo");

    // And it never walks back into the source that just failed: with only
    // those two configured there is nothing left, and the search stops
    // instead of rebuilding a client on every frame.
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: current_source(&app),
            symbol: symbol.into(),
            error: "finnhub API 500".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "finnhub", "fell back into the dead source");
}

/// With no other source configured (the keyless default is all most people
/// have), the errors stay and nothing is swapped behind the user's back.
#[test]
fn nothing_is_swapped_when_there_is_nowhere_to_go() {
    let mut app = fake_app();
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: current_source(&app),
            symbol: symbol.into(),
            error: "yahoo API 429".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "yahoo");
    assert!(app.notice().is_none(), "a notice with nothing to report");
    assert_eq!(
        app.errors.len(),
        2,
        "errors must stay on the failing source"
    );
}

/// `source_fallback = false` keeps the choice of source entirely manual.
#[test]
fn the_fallback_can_be_turned_off() {
    let mut app = fake_app();
    app.source_fallback = false;
    app.config.keys.insert("finnhub".into(), "test-key".into());
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: current_source(&app),
            symbol: symbol.into(),
            error: "yahoo API 429".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "yahoo");
}

#[test]
fn a_manual_source_change_starts_a_fresh_fallback_grace_period() {
    let mut app = fake_app();
    app.config.keys.insert("finnhub".into(), "test-key".into());
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: current_source(&app),
            symbol: symbol.into(),
            error: "unavailable".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    app.from_cache.insert("AAPL".into(), 1_700_000_000);
    app.price_flash
        .insert("AAPL".into(), (Instant::now(), true));
    app.open_settings();
    app.settings.source_choice = "finnhub".into();
    app.settings.cursor = settings_rows()
        .iter()
        .position(|row| matches!(row, SettingsRow::Save))
        .unwrap();
    press(&mut app, KeyCode::Enter);
    // No config path in this fixture: Save applies live but leaves the
    // write-error overlay open. Close it before exercising fallback.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.source_name, "finnhub");
    assert!(
        app.source_trouble_since.is_none(),
        "the previous source's outage survived Save"
    );
    assert!(app.from_cache.is_empty());
    assert!(app.price_flash.is_empty());
    app.fallback_if_stuck();
    assert_eq!(
        app.source_name, "finnhub",
        "the manual choice was immediately undone"
    );
}

#[test]
fn saving_an_alternative_key_reenables_an_exhausted_fallback() {
    let mut app = fake_app();
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: current_source(&app),
            symbol: symbol.into(),
            error: "unavailable".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "yahoo");
    app.open_settings();
    app.settings.key_values.insert("finnhub", "test-key".into());
    app.settings.cursor = settings_rows()
        .iter()
        .position(|row| matches!(row, SettingsRow::Save))
        .unwrap();
    press(&mut app, KeyCode::Enter);
    // No config path in this fixture: Save applies live but leaves the
    // write-error overlay open. Close it before exercising fallback.
    press(&mut app, KeyCode::Esc);
    app.fallback_if_stuck();
    assert_eq!(
        app.source_name, "finnhub",
        "new credentials were never considered"
    );
}

#[test]
fn fallback_keeps_prices_labelled_and_rejects_late_responses() {
    let mut app = fake_app();
    app.config.keys.insert("finnhub".into(), "test-key".into());
    let old_source = current_source(&app);
    let data = app.data["AAPL"].clone();
    app.apply(SourceEvent::Data {
        params: None,
        source: old_source.clone(),
        symbol: "AAPL".into(),
        data: data.clone(),
    });
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: old_source.clone(),
            symbol: symbol.into(),
            error: "unavailable".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "finnhub");
    assert!(
        app.cached_age("AAPL").is_some(),
        "retained prices look live"
    );
    let mut late = data.clone();
    late.quote.price = 1.0;
    app.apply(SourceEvent::Data {
        params: None,
        source: old_source.clone(),
        symbol: "AAPL".into(),
        data: late,
    });
    app.apply(SourceEvent::Error {
        params: None,
        source: old_source,
        symbol: "AAPL".into(),
        error: "late failure".into(),
    });
    assert_eq!(app.data["AAPL"].quote.price, data.quote.price);
    assert!(app.errors.is_empty());
    assert!(app.cached_age("AAPL").is_some());
    app.apply(SourceEvent::Data {
        params: None,
        source: current_source(&app),
        symbol: "AAPL".into(),
        data,
    });
    assert!(app.cached_age("AAPL").is_none());
}

#[test]
fn replacing_credentials_rejects_the_same_providers_old_responses() {
    let mut app = fake_app();
    app.config.keys.insert("finnhub".into(), "old-key".into());
    app.open_settings();
    app.settings.source_choice = "finnhub".into();
    app.settings.cursor = settings_rows()
        .iter()
        .position(|row| matches!(row, SettingsRow::Save))
        .unwrap();
    press(&mut app, KeyCode::Enter);
    // No config path in this fixture: Save applies live but leaves the
    // write-error overlay open. Close it before exercising fallback.
    press(&mut app, KeyCode::Esc);
    let old_source = current_source(&app);
    app.open_settings();
    app.settings.key_values.insert("finnhub", "new-key".into());
    app.settings.cursor = settings_rows()
        .iter()
        .position(|row| matches!(row, SettingsRow::Save))
        .unwrap();
    press(&mut app, KeyCode::Enter);
    // No config path in this fixture: Save applies live but leaves the
    // write-error overlay open. Close it before exercising fallback.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.source_name, old_source.name());
    app.apply(SourceEvent::Error {
        params: None,
        source: old_source,
        symbol: "AAPL".into(),
        error: "old key rejected".into(),
    });
    assert!(app.errors.is_empty());
}

#[test]
fn a_new_symbol_gets_a_chance_before_fallback() {
    let mut app = fake_app();
    app.config.keys.insert("finnhub".into(), "test-key".into());
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: current_source(&app),
            symbol: symbol.into(),
            error: "unavailable".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - std::time::Duration::from_secs(120));
    press(&mut app, KeyCode::Char('a'));
    for c in "NVDA".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "yahoo");
    assert!(app.source_trouble_since.is_none());
}

#[test]
fn range_keys_cycle_presets_and_update_header() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    press(&mut app, KeyCode::Char('t'));
    assert_eq!((app.range, app.interval), (Range::D5, Interval::M15));
    let screen = render(&mut app);
    assert!(screen.contains("5d / 15m (t: change)"), "screen:\n{screen}");
    // Wrap backwards past the first preset.
    press(&mut app, KeyCode::Char('T'));
    press(&mut app, KeyCode::Char('T'));
    assert_eq!((app.range, app.interval), (Range::Y1, Interval::D1));
    assert!(render(&mut app).contains("1y / 1d (t: change)"));
    // Old data stays on screen until the poller answers.
    assert!(app.data.contains_key("AAPL"));
}

/// Budget invariant: a range switch must wake only the price poller. The
/// visible AlphAI bundle stays cached (manual_refresh would drop it and
/// trigger a refetch on the next draw).
#[test]
fn range_switch_keeps_news_bundle() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    press(&mut app, KeyCode::Char('t'));
    assert!(
        app.feeds.contains_key("AAPL"),
        "range switch dropped the news bundle"
    );
}

#[test]
fn split_view_combines_table_chart_and_news() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Split);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("Watchlist"), "screen:\n{screen}");
    assert!(
        screen.contains('▀'),
        "no candle chart in split view:\n{screen}"
    );
    assert!(screen.contains("News · AAPL"), "screen:\n{screen}");
    assert!(
        screen.contains("Apple beats expectations"),
        "screen:\n{screen}"
    );
}

#[test]
fn split_view_news_panel_without_key_shows_one_line_hint() {
    let mut app = fake_app();
    app.alphai_enabled = false;
    app.view_idx = ui::view_index(ui::ViewId::Split);
    let screen = render(&mut app);
    assert!(screen.contains("alphai.io"), "screen:\n{screen}");
    assert!(screen.contains("press s"), "screen:\n{screen}");
}

#[test]
fn split_view_drops_news_panel_on_tiny_terminal() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Split);
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
    assert!(
        !screen.contains("News ·"),
        "news strip should be hidden:\n{screen}"
    );
}

/// Every view registers a distinct ViewId (a duplicate would make
/// `view_index` land on the wrong tab) and carries footer hints.
#[test]
fn view_ids_are_unique_and_indexable() {
    for (i, view) in ui::VIEWS.iter().enumerate() {
        assert_eq!(
            ui::view_index(view.id()),
            i,
            "duplicate ViewId {:?}",
            view.id()
        );
        assert!(
            !view.hints().is_empty(),
            "{:?}: empty footer hints",
            view.id()
        );
    }
}

/// The footer renders the keys actually bound in the keymap, in the
/// traditional shapes ("↑↓ select", "c/m/i/b/e/n chart").
#[test]
fn footer_shows_bound_keys() {
    let mut app = fake_app();
    let screen = render(&mut app);
    assert!(screen.contains("q quit"), "screen:\n{screen}");
    assert!(screen.contains("tab/1-9 view"), "screen:\n{screen}");
    assert!(screen.contains("↑↓ select"), "screen:\n{screen}");
    assert!(screen.contains("c/m/i/b/e/n chart"), "screen:\n{screen}");
    app.view_idx = ui::view_index(ui::ViewId::News);
    let screen = render(&mut app);
    assert!(screen.contains("⏎ open"), "screen:\n{screen}");
    assert!(screen.contains("x layout"), "screen:\n{screen}");
}

/// A `[keybindings]` remap flows through to the footer and to dispatch:
/// the hint shows the new key and pressing it fires the action.
#[test]
fn footer_and_dispatch_follow_a_remap() {
    let mut app = fake_app();
    let mut warnings = Vec::new();
    app.keymap = crate::keymap::Keymap::from_config(
        [("open", vec!["w"]), ("card", vec!["u"])],
        &mut warnings,
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("w open"), "screen:\n{screen}");
    assert!(!screen.contains("⏎ open"), "screen:\n{screen}");
    // The new key drives the action, the old one is gone.
    press(&mut app, KeyCode::Char('v'));
    assert!(
        !app.article_overlay.open,
        "the replaced default still fired"
    );
    press(&mut app, KeyCode::Char('u'));
    assert!(app.article_overlay.open, "the remapped key did not fire");
}

/// ? opens the help overlay listing every action with its live keys and
/// its [keybindings] name; esc closes it, q still quits. The coverage walk
/// over ACTIONS keeps the table exhaustive: a new action cannot ship
/// without a help row.
#[test]
fn help_overlay_lists_every_action() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('?'));
    assert!(app.help.open);
    // Taller than any real terminal on purpose: this checks that every
    // action HAS a row, not that they all fit on one screen. The overlay
    // scrolls, and the table has been longer than 30 rows for a while.
    let screen = render_sized(&mut app, 90, 70);
    for (_, name) in crate::keymap::ACTIONS.iter() {
        assert!(screen.contains(name), "action {name} missing:\n{screen}");
    }
    assert!(screen.contains("ctrl-c"), "screen:\n{screen}");
    assert!(screen.contains("[keybindings]"), "screen:\n{screen}");
    // Esc closes the overlay without quitting the app.
    assert!(!app.handle_key(KeyEvent::from(KeyCode::Esc)));
    assert!(!app.help.open);
    // A remap shows up in the table: the overlay reads the live keymap.
    app.keymap = crate::keymap::Keymap::from_config([("refresh", vec!["f5"])], &mut Vec::new());
    press(&mut app, KeyCode::Char('?'));
    let screen = render_sized(&mut app, 90, 45);
    assert!(screen.contains("f5"), "screen:\n{screen}");
    // q inside the overlay still quits.
    assert!(app.handle_key(KeyEvent::from(KeyCode::Char('q'))));
}

/// Split embeds the news strip, but j/k must keep driving the watchlist
/// selection and must never page the feed (budget guard).
#[test]
fn split_j_moves_watchlist_not_articles() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into(), "MSFT".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Split);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            Some("cur1".into()),
        ),
    );
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected, 1, "j did not move the watchlist selection");
    assert_eq!(app.news_selected, 0, "j leaked into the article list");
    assert!(cmds.try_recv().is_err(), "split view paged the feed");
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
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![
                article("Apple beats expectations", "AAPL", 9, "positive"),
                article("Supplier note weighs on outlook", "AAPL", 6, "negative"),
            ],
            Some(FeedPayload::Sentiment(SentimentSummary {
                days: 7,
                total: 20,
                bullish: 12,
                neutral: 5,
                bearish: 3,
            })),
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("News · AAPL"), "screen:\n{screen}");
    assert!(
        screen.contains("Apple beats expectations"),
        "screen:\n{screen}"
    );
    assert!(screen.contains("12 bullish"), "screen:\n{screen}");
    assert!(screen.contains("▲"), "sentiment glyph missing:\n{screen}");
    // detail pane shows the selected article's summary and the AI meta calls
    assert!(
        screen.contains("Summary of Apple beats expectations"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("nov 7"),
        "novelty missing from meta:\n{screen}"
    );
    assert!(
        screen.contains("positive/high"),
        "sentiment/confidence missing:\n{screen}"
    );
    assert!(
        screen.contains("act high"),
        "actionability missing:\n{screen}"
    );
}

#[test]
fn news_view_without_key_shows_hint() {
    let mut app = fake_app();
    app.alphai_enabled = false;
    app.view_idx = ui::view_index(ui::ViewId::News);
    let screen = render(&mut app);
    assert!(screen.contains("alphai.io"), "screen:\n{screen}");
    assert!(screen.contains("free API key"), "screen:\n{screen}");
}

#[test]
fn news_view_shows_error_state() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.alphai_errors
        .insert("AAPL".into(), "invalid AlphAI API key".into());
    let screen = render(&mut app);
    assert!(
        screen.contains("invalid AlphAI API key"),
        "screen:\n{screen}"
    );
    assert!(screen.contains("press r to retry"), "screen:\n{screen}");
}

#[test]
fn insider_view_shows_summary_and_filings() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    let trades: InsiderTrades = serde_json::from_str(
        r#"{
          "ticker": "AAPL",
          "summary": {
            "last_12m": { "buy_count": 2, "sell_count": 12,
                          "buy_value_usd": "1240000.00", "sell_value_usd": "224580213.05",
                          "unique_insiders": 6, "pct_10b5_1": 85 },
            "top_insiders": [{"name": "COOK TIMOTHY", "title": "CEO", "event_count": 3, "net_value_usd": "-50300000.00"}]
          }
        }"#,
    )
    .unwrap();
    app.feeds.insert(
        alphai::insider_key("AAPL"),
        FeedBundle::new(
            vec![filing("Apple insider sold $12.5M of stock", "direct")],
            Some(FeedPayload::Insider(Box::new(trades))),
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("Insider · AAPL"), "screen:\n{screen}");
    assert!(screen.contains("14 events"), "screen:\n{screen}");
    assert!(screen.contains("$224.6M"), "screen:\n{screen}");
    assert!(screen.contains("85% under 10b5-1"), "screen:\n{screen}");
    assert!(screen.contains("6 insiders"), "screen:\n{screen}");
    assert!(screen.contains("COOK TIMOTHY"), "screen:\n{screen}");
    assert!(screen.contains("×3"), "event count missing:\n{screen}");
    // No AI enrichment and no structured block on this legacy filing: the
    // sell glyph comes from the title fallback, the ownership marker follows,
    // and the plan/value columns stay blank.
    assert!(screen.contains("▼  D"), "screen:\n{screen}");
    assert!(screen.contains("Apple insider sold"), "screen:\n{screen}");
    assert!(
        screen.contains("score 4+"),
        "size filter missing from the title:\n{screen}"
    );
}

/// A filing with the structured `insider` block: side/value/plan render from
/// it, and the block beats the title wording (a code D "sold to issuer" row
/// must stay neutral even though the title says "sold").
#[test]
fn insider_structured_block_drives_row_and_card() {
    fn with_block(title: &str, side: &str, value: Option<&str>, plan: bool) -> Article {
        let value_json = value.map_or("null".to_string(), |v| format!("\"{v}\""));
        serde_json::from_str(&format!(
            r#"{{
              "original": {{"title": "{title}", "time_published": "2026-07-10T12:00:00Z",
                            "ownership_form": "direct"}},
              "enrichment": {{"category": "insider", "tickers": ["AAPL"], "relevance_score": 7}},
              "insider": {{
                "side": "{side}", "transaction_code": "S",
                "shares": "25000.0000", "avg_price_usd": "187.3200",
                "total_value_usd": {value_json}, "is_10b5_1": {plan},
                "insider_name": "STEVENS MARK A", "insider_title": "Director",
                "is_officer": false, "is_director": true,
                "is_ten_percent_owner": false, "transaction_date": "2026-07-09"
              }}
            }}"#
        ))
        .unwrap()
    }
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    app.feeds.insert(
        alphai::insider_key("AAPL"),
        FeedBundle::new(
            vec![
                with_block("X sold $4.7M of stock", "sell", Some("4683000.00"), true),
                // Code D: shares sold BACK TO THE ISSUER; side "other" must
                // not fall back to the "sold" keyword in the title.
                with_block(
                    "Y sold $1.0M of stock to the issuer",
                    "other",
                    Some("1000000.00"),
                    false,
                ),
            ],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("$4.7M"), "value column missing:\n{screen}");
    assert!(
        screen.contains("▼  D p"),
        "sell glyph + plan flag missing:\n{screen}"
    );
    assert!(
        screen.contains("·  D"),
        "structured \"other\" not neutral:\n{screen}"
    );
    // The detail meta carries the structured trade.
    assert!(
        screen.contains("SELL $4.7M"),
        "meta trade missing:\n{screen}"
    );
    assert!(
        screen.contains("10b5-1 plan"),
        "meta plan flag missing:\n{screen}"
    );
    // The fullscreen card shows the full structured trade.
    press(&mut app, KeyCode::Char('v'));
    let card = render(&mut app);
    assert!(
        card.contains("SELL 25,000 sh @ $187.32 = $4.7M (code S)"),
        "card:\n{card}"
    );
    assert!(card.contains("STEVENS MARK A (Director)"), "card:\n{card}");
    assert!(card.contains("2026-07-09"), "card:\n{card}");
}

/// An Insider bundle with the trades chart payload, its events placed
/// relative to today so the trailing windows always cover them.
fn insider_app_with_chart() -> App {
    use chrono::{Datelike, Days, Utc};

    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    let today = Utc::now().date_naive();
    let day = |ago: u64| (today - Days::new(ago)).format("%Y-%m-%d").to_string();
    let monday = |ago: u64| {
        let d = today - Days::new(ago);
        (d - Days::new(u64::from(d.weekday().num_days_from_monday())))
            .format("%Y-%m-%d")
            .to_string()
    };
    let trades: InsiderTrades = serde_json::from_str(&format!(
        r#"{{
          "summary": {{ "last_12m": {{ "buy_count": 1, "sell_count": 2,
                        "buy_value_usd": "500000", "sell_value_usd": "10886021",
                        "unique_insiders": 2, "pct_10b5_1": 33 }} }},
          "series_weekly": [
            {{ "week_start": "{w1}", "buy_count": 0, "sell_count": 1,
               "buy_value_usd": "0", "sell_value_usd": "9886021" }},
            {{ "week_start": "{w2}", "buy_count": 1, "sell_count": 1,
               "buy_value_usd": "500000", "sell_value_usd": "1000000" }}
          ],
          "chart_events": [
            {{ "side": "sell", "transaction_code": "S", "total_value_usd": "9886021.65",
               "tranche_count": 8, "stake_change_pct": "-100.0", "is_10b5_1": true,
               "insider_name": "A", "transaction_date": "{e1}", "news_uid": "uid-1" }},
            {{ "side": "sell", "transaction_code": "D", "total_value_usd": "1000000",
               "insider_name": "B", "transaction_date": "{e2}" }},
            {{ "side": "buy", "transaction_code": "P", "total_value_usd": "500000",
               "insider_name": "C", "transaction_date": "{e2}" }}
          ]
        }}"#,
        w1 = monday(5),
        w2 = monday(12),
        e1 = day(5),
        e2 = day(12),
    ))
    .unwrap();
    let mut sold = filing("Apple insider sold $9.9M of stock", "direct");
    sold.original.uid = "uid-1".into();
    app.feeds.insert(
        alphai::insider_key("AAPL"),
        FeedBundle::new(
            vec![sold],
            Some(FeedPayload::Insider(Box::new(trades))),
            None,
        ),
    );
    app
}

/// The chart panel: log scatter of the window's events over the weekly
/// bars, joined to the list by uid, cycled off and back by g.
#[test]
fn insider_chart_panel_draws_and_cycles() {
    let mut app = insider_app_with_chart();
    let screen = render(&mut app);
    assert!(
        screen.contains("Form 4 · 3m"),
        "panel title missing:\n{screen}"
    );
    // Window totals in the title: both sides priced.
    assert!(screen.contains("▼ $10.9M"), "sell total missing:\n{screen}");
    assert!(screen.contains("▲ $500.0K"), "buy total missing:\n{screen}");
    assert!(
        screen.contains("3 events"),
        "event count missing:\n{screen}"
    );
    // The three mark shapes: filled sell, hollow sale-to-issuer, filled buy.
    assert!(
        screen.contains("▽"),
        "sale-to-issuer mark missing:\n{screen}"
    );
    // Log decade labels from $100K up to $10M.
    assert!(screen.contains("$10M"), "decade label missing:\n{screen}");
    assert!(screen.contains("$100K"), "decade label missing:\n{screen}");
    assert!(
        screen.contains("▲ buy · ▼ sell · ▽ to issuer"),
        "legend missing:\n{screen}"
    );
    // Weekly bars: two-sided window renders block glyphs under the scatter.
    assert!(has_candles(&screen), "weekly bars missing:\n{screen}");
    // The selected filing's extras join the detail pane by uid.
    assert!(
        screen.contains("stake -100.0%"),
        "stake extra missing:\n{screen}"
    );
    assert!(
        screen.contains("8 tranches"),
        "tranche extra missing:\n{screen}"
    );

    // g cycles 3m -> 12m -> off -> 3m; the panel yields its rows when off.
    press(&mut app, KeyCode::Char('g'));
    assert!(render(&mut app).contains("Form 4 · 12m"));
    press(&mut app, KeyCode::Char('g'));
    let screen = render(&mut app);
    assert!(
        !screen.contains("Form 4 ·"),
        "panel must hide when off:\n{screen}"
    );
    press(&mut app, KeyCode::Char('g'));
    assert!(render(&mut app).contains("Form 4 · 3m"));
}

/// Rows are scarce before they are gone: the weekly bars drop first, the
/// whole panel only when the list would starve.
#[test]
fn insider_chart_degrades_bars_then_panel() {
    let mut app = insider_app_with_chart();
    // 24-row terminal: the panel fits only without its bars.
    let screen = render_sized(&mut app, 100, 24);
    assert!(
        screen.contains("Form 4 · 3m"),
        "short panel missing:\n{screen}"
    );
    assert!(
        !has_candles(&screen),
        "bars must drop on short terminals:\n{screen}"
    );
    // 20 rows: the panel is gone, the list and detail stay.
    let screen = render_sized(&mut app, 100, 20);
    assert!(!screen.contains("Form 4 ·"), "panel must yield:\n{screen}");
    assert!(
        screen.contains("Apple insider sold"),
        "list lost:\n{screen}"
    );
}

/// +/- on the Insider view adjust the insider filter only, and the refetch
/// carries the new min_relevance (the news filter stays untouched).
#[test]
fn insider_score_keys_adjust_own_filter() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    let mut bundle = FeedBundle::new(
        vec![filing("Apple insider sold $12.5M of stock", "direct")],
        None,
        None,
    );
    bundle.min_score = Some(app.insider_min_score);
    app.feeds.insert(alphai::insider_key("AAPL"), bundle);
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "matching filter refetched");

    let news_before = app.news_min_score;
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(app.insider_min_score, 5);
    assert_eq!(
        app.news_min_score, news_before,
        "+ leaked into the news filter"
    );
    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchInsider {
            symbol,
            cursor,
            min_relevance,
            ..
        }) => {
            assert_eq!(symbol, "AAPL");
            assert_eq!(cursor, None);
            assert_eq!(min_relevance, Some(5));
        }
        other => panic!("expected a filtered insider fetch, got {:?}", other.is_ok()),
    }
}

#[test]
fn news_scope_cycles_and_relabels() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    assert_eq!(app.news_cache_key(), "AAPL");
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.news_scope, NewsScope::Market);
    assert_eq!(app.news_cache_key(), "*");
    assert!(render(&mut app).contains("News · market"));
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.news_scope, NewsScope::Trending);
    assert_eq!(app.news_cache_key(), TRENDING_KEY);
    assert!(render(&mut app).contains("News · trending"));
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.news_cache_key(), "AAPL");
}

#[test]
fn market_scope_shows_tickers_and_sources_count() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.news_scope = NewsScope::Market;
    app.feeds.insert(
        "*".into(),
        FeedBundle::new(
            vec![market_article("Fed minutes move futures", 7)],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(
        screen.contains("AAPL,MSFT"),
        "ticker cell missing:\n{screen}"
    );
    assert!(screen.contains("×7"), "outlet count missing:\n{screen}");
    assert!(
        screen.contains("reprints collapsed"),
        "head line missing:\n{screen}"
    );
}

#[test]
fn trending_view_lists_articles() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.news_scope = NewsScope::Trending;
    app.feeds.insert(
        TRENDING_KEY.into(),
        FeedBundle::new(
            vec![market_article("Chip stocks rally on export news", 5)],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("News · trending"), "screen:\n{screen}");
    assert!(
        screen.contains("top 10 of the last 48h"),
        "screen:\n{screen}"
    );
    assert!(screen.contains("Chip stocks rally"), "screen:\n{screen}");
    assert!(screen.contains("×5"), "outlet count missing:\n{screen}");
}

#[test]
fn article_overlay_opens_scrolls_and_closes() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    press(&mut app, KeyCode::Char('v'));
    assert!(app.article_overlay.open);
    let screen = render(&mut app);
    assert!(screen.contains("Article"), "overlay missing:\n{screen}");
    assert!(
        screen.contains("price: +2-4% near term"),
        "impact missing:\n{screen}"
    );
    assert!(screen.contains("Trading value"), "screen:\n{screen}");
    assert!(
        screen.contains("contrarian: Priced in already."),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("entities: Acme (company)"),
        "screen:\n{screen}"
    );
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.article_overlay.scroll, 1);
    render(&mut app); // must not panic; the render pass clamps the scroll
    press(&mut app, KeyCode::Esc);
    assert!(!app.article_overlay.open);
    // The card pane still shows the article; only the modal frame must go.
    assert!(
        !render(&mut app).contains(" Article "),
        "overlay did not close"
    );
}

#[test]
fn overlay_steals_nav_keys() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![
                article("First", "AAPL", 9, "positive"),
                article("Second", "AAPL", 8, "positive"),
            ],
            None,
            None,
        ),
    );
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(app.news_selected, 0, "overlay leaked j to the article list");
    assert_eq!(app.selected, 0, "overlay leaked l to the ticker list");
}

#[test]
fn overlay_works_from_insider_view() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    app.feeds.insert(
        alphai::insider_key("AAPL"),
        FeedBundle::new(
            vec![filing("Officer bought 10,000 shares", "indirect")],
            None,
            None,
        ),
    );
    press(&mut app, KeyCode::Char('v'));
    assert!(app.article_overlay.open);
    let screen = render(&mut app);
    assert!(
        screen.contains("Officer bought 10,000 shares"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("Summary of Officer bought"),
        "screen:\n{screen}"
    );
}

#[test]
fn overlay_noop_when_no_articles() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    press(&mut app, KeyCode::Char('v'));
    assert!(
        !app.article_overlay.open,
        "overlay opened with nothing to show"
    );
}

#[test]
fn news_card_pane_shown_by_default() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    // The full AI card sits next to the list without opening the overlay.
    assert!(screen.contains(" card ·"), "card pane missing:\n{screen}");
    assert!(
        screen.contains("Trading value"),
        "card content missing:\n{screen}"
    );
    assert!(
        screen.contains("contrarian: Priced in already."),
        "screen:\n{screen}"
    );
}

#[test]
fn x_cycles_news_layout() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    assert_eq!(app.news_layout, NewsLayout::Side);
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(app.news_layout, NewsLayout::Stacked);
    let screen = render(&mut app);
    assert!(
        screen.contains(" card ·"),
        "card pane missing in stacked:\n{screen}"
    );
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(app.news_layout, NewsLayout::Side);
}

/// +/- move the score filter; the cached bundle's recorded filter stops
/// matching, so the next ensure pass refetches page 1 with the new value,
/// without waiting for the top row (a filter change is an explicit ask).
#[test]
fn score_keys_adjust_filter_and_refetch() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    let mut bundle = FeedBundle::new(
        vec![
            article("Apple beats expectations", "AAPL", 9, "positive"),
            article("Supplier note", "AAPL", 8, "negative"),
        ],
        None,
        None,
    );
    bundle.min_score = Some(app.news_min_score);
    app.feeds.insert("AAPL".into(), bundle);
    app.news_selected = 1;
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "matching filter refetched");

    // The title advertises the active filter before and after the change.
    assert!(
        render(&mut app).contains("score 7+"),
        "filter missing from the title"
    );
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(app.news_min_score, 8);
    assert_eq!(
        app.news_selected, 0,
        "selection not reset on a filter change"
    );
    assert!(render(&mut app).contains("score 8+"));

    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchNews {
            symbol,
            cursor,
            min_relevance,
            ..
        }) => {
            assert_eq!(symbol.as_deref(), Some("AAPL"));
            assert_eq!(cursor, None, "filter change must restart from page 1");
            assert_eq!(min_relevance, Some(8));
        }
        other => panic!("expected a filtered head fetch, got {:?}", other.is_ok()),
    }
    // The second ensure pass is absorbed by the inflight guard.
    app.ensure_alphai_data();
    assert!(
        cmds.try_recv().is_err(),
        "duplicate refetch for one filter change"
    );
}

#[test]
fn score_filter_clamps_at_bounds() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.news_min_score = 10;
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(app.news_min_score, 10);
    app.news_min_score = 1;
    press(&mut app, KeyCode::Char('-'));
    assert_eq!(app.news_min_score, 1);
    // Outside a news-feed view the keys are inert.
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(app.news_min_score, 1);
}

/// Below the width floor the side layout hands the whole width to the list;
/// the embedded card pane disappears but v still opens the fullscreen card.
#[test]
fn narrow_news_view_hides_side_card() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    // The pane title carries "pgup/pgdn scroll"; the footer's "v card" hint
    // stays in both sizes, so it cannot be the needle.
    let wide = render_sized(&mut app, 100, 30);
    assert!(
        wide.contains("pgup/pgdn scroll"),
        "card pane missing at 100 cols:\n{wide}"
    );
    let narrow = render_sized(&mut app, 80, 30);
    assert!(
        !narrow.contains("pgup/pgdn scroll"),
        "card pane still there at 80 cols:\n{narrow}"
    );
    assert!(
        narrow.contains("Apple beats expectations"),
        "list missing:\n{narrow}"
    );
    press(&mut app, KeyCode::Char('v'));
    let overlay = render_sized(&mut app, 80, 30);
    assert!(
        overlay.contains(" Article "),
        "v overlay unavailable when narrow:\n{overlay}"
    );
}

/// Ages under 15 minutes count as breaking (they render in the accent
/// color); unparsable timestamps never do.
#[test]
fn article_freshness_window() {
    use chrono::{Duration, Utc};
    let mut a = article("Fresh", "AAPL", 9, "positive");
    let now = Utc::now();
    a.original.time_published = (now - Duration::minutes(5)).to_rfc3339();
    assert!(ui::news::is_fresh(&a, now));
    a.original.time_published = (now - Duration::minutes(30)).to_rfc3339();
    assert!(!ui::news::is_fresh(&a, now));
    a.original.time_published = "not-a-date".into();
    assert!(!ui::news::is_fresh(&a, now));
}

#[test]
fn j_at_last_row_requests_next_page() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            Some("cur1".into()),
        ),
    );
    assert!(
        render(&mut app).contains("↓ load older articles"),
        "load-more hint missing"
    );
    press(&mut app, KeyCode::Char('j'));
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchNews {
            symbol,
            cursor,
            min_relevance,
            ..
        }) => {
            assert_eq!(symbol.as_deref(), Some("AAPL"));
            assert_eq!(cursor.as_deref(), Some("cur1"));
            assert_eq!(min_relevance, Some(app.news_min_score));
        }
        other => panic!("expected a page fetch, got {:?}", other.is_ok()),
    }
    // While the page is in flight the hint reports progress and a second j
    // must not spend another request.
    assert!(
        render(&mut app).contains("loading…"),
        "no in-flight feedback"
    );
    press(&mut app, KeyCode::Char('j'));
    assert!(cmds.try_recv().is_err(), "duplicate page fetch sent");
}

#[test]
fn insider_j_at_last_row_requests_next_page() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    app.feeds.insert(
        alphai::insider_key("AAPL"),
        FeedBundle::new(
            vec![filing("Apple insider sold $12.5M of stock", "direct")],
            None,
            Some("cur9".into()),
        ),
    );
    press(&mut app, KeyCode::Char('j'));
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchInsider {
            symbol,
            cursor,
            min_relevance,
            ..
        }) => {
            assert_eq!(symbol, "AAPL");
            assert_eq!(cursor.as_deref(), Some("cur9"));
            assert_eq!(min_relevance, Some(app.insider_min_score));
        }
        other => panic!("expected an insider page fetch, got {:?}", other.is_ok()),
    }
}

/// A minimal row with a uid, as every real API article carries one (the
/// richer `article` fixture leaves it empty, staying invisible to the
/// unseen-marker tracking).
fn uid_article(uid: &str, title: &str) -> Article {
    serde_json::from_str(&format!(
        r#"{{"original": {{"uid": "{uid}", "title": "{title}"}}}}"#
    ))
    .unwrap()
}

/// A head (non-append) news fetch for `key`, as the AlphAI task delivers it.
fn head_fetch(app: &mut App, key: &str, articles: Vec<Article>, min_relevance: Option<u8>) {
    app.apply_alphai(alphai::Event::Feed {
        key: key.into(),
        articles,
        side: None,
        next_cursor: None,
        mode: alphai::FeedMode::Replace,
        min_relevance,
    });
}

#[test]
fn page_append_extends_list_and_dedupes() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(vec![uid_article("aaa", "First")], None, Some("c1".into())),
    );
    app.apply_alphai(alphai::Event::Feed {
        key: "AAPL".into(),
        articles: vec![
            uid_article("aaa", "First reprint"),
            uid_article("bbb", "Second"),
        ],
        side: None,
        next_cursor: Some("c2".into()),
        mode: alphai::FeedMode::Append,
        min_relevance: Some(7),
    });
    let b = &app.feeds["AAPL"];
    assert_eq!(b.articles.len(), 2, "page boundary duplicate not dropped");
    assert_eq!(b.articles[1].original.title, "Second");
    assert_eq!(b.next_cursor.as_deref(), Some("c2"));
}

/// Rows brought in by a refetch carry the unseen marker until the cursor
/// rests on them; the first fetch of a feed is a baseline and marks nothing.
#[test]
fn refetch_marks_new_rows_until_hovered() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    head_fetch(
        &mut app,
        "AAPL",
        vec![uid_article("aaa", "First"), uid_article("bbb", "Second")],
        Some(7),
    );
    assert!(
        !app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]),
        "first fetch marked its own baseline"
    );

    head_fetch(
        &mut app,
        "AAPL",
        vec![
            uid_article("ccc", "Breaking story"),
            uid_article("aaa", "First"),
            uid_article("bbb", "Second"),
        ],
        Some(7),
    );
    assert!(
        app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]),
        "new row unmarked"
    );
    assert!(
        !app.is_unseen("AAPL", &app.feeds["AAPL"].articles[1]),
        "old row marked"
    );

    // The marker renders while the cursor is parked elsewhere...
    app.news_selected = 1;
    let screen = render(&mut app);
    assert!(
        screen.contains("● Breaking story"),
        "marker missing:\n{screen}"
    );
    // ...and hovering the row retires it.
    app.news_selected = 0;
    let screen = render(&mut app);
    assert!(
        !screen.contains("●"),
        "marker survived the hover:\n{screen}"
    );

    // Manual r drops the bundle but not the seen set: the refetch after it
    // still marks only what is genuinely new.
    press(&mut app, KeyCode::Char('r'));
    head_fetch(
        &mut app,
        "AAPL",
        vec![
            uid_article("ddd", "Newer still"),
            uid_article("ccc", "Breaking story"),
        ],
        Some(7),
    );
    assert!(
        app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]),
        "post-r new row unmarked"
    );
    assert!(
        !app.is_unseen("AAPL", &app.feeds["AAPL"].articles[1]),
        "post-r hovered row marked"
    );
}

/// Paged-in older rows and a refetch after moving the score filter are the
/// reader asking for more, not news arriving: neither marks.
#[test]
fn pagination_and_filter_moves_never_mark() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    head_fetch(&mut app, "AAPL", vec![uid_article("aaa", "First")], Some(7));
    app.apply_alphai(alphai::Event::Feed {
        key: "AAPL".into(),
        articles: vec![uid_article("old1", "Older story")],
        side: None,
        next_cursor: None,
        mode: alphai::FeedMode::Append,
        min_relevance: Some(7),
    });
    assert!(
        !app.is_unseen("AAPL", &app.feeds["AAPL"].articles[1]),
        "paged-in row marked as new"
    );

    // Loosening the filter reveals lower-scored rows: old news, no markers.
    head_fetch(
        &mut app,
        "AAPL",
        vec![
            uid_article("aaa", "First"),
            uid_article("low", "Low score story"),
        ],
        Some(6),
    );
    assert!(
        !app.is_unseen("AAPL", &app.feeds["AAPL"].articles[1]),
        "filter-revealed row marked as new"
    );
}

/// The Split strip shows the markers but has no cursor, so rendering it
/// never retires them.
#[test]
fn split_strip_shows_markers_without_clearing() {
    let mut app = empty_app(vec!["AAPL".into()]);
    head_fetch(&mut app, "AAPL", vec![uid_article("aaa", "First")], Some(7));
    head_fetch(
        &mut app,
        "AAPL",
        vec![
            uid_article("ccc", "Breaking story"),
            uid_article("aaa", "First"),
        ],
        Some(7),
    );
    app.view_idx = ui::view_index(ui::ViewId::Split);
    let screen = render(&mut app);
    assert!(
        screen.contains("● Breaking story"),
        "marker missing:\n{screen}"
    );
    let screen = render(&mut app);
    assert!(
        screen.contains("● Breaking story"),
        "the strip retired a marker:\n{screen}"
    );
}

#[test]
fn archive_gate_shows_upsell_and_stops_paging() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            Some("cur1".into()),
        ),
    );
    app.apply_alphai(alphai::Event::PageError {
        key: "AAPL".into(),
        error: alphai::ARCHIVE_GATE_MSG.into(),
        gated: true,
    });
    let b = &app.feeds["AAPL"];
    assert!(b.gated);
    assert_eq!(b.next_cursor, None, "gated feed still offers a cursor");
    // The list survives and the hint sells the upgrade instead of an error.
    let screen = render(&mut app);
    assert!(
        screen.contains("Apple beats expectations"),
        "list dropped:\n{screen}"
    );
    assert!(
        screen.contains("archive limit"),
        "upsell missing:\n{screen}"
    );
    press(&mut app, KeyCode::Char('j'));
    assert!(cmds.try_recv().is_err(), "gated feed still paged");
}

/// A uid row with an explicit publication time, for the merge ordering.
fn timed_article(uid: &str, title: &str, published: &str) -> Article {
    serde_json::from_str(&format!(
        r#"{{"original": {{"uid": "{uid}", "title": "{title}", "time_published": "{published}"}}}}"#
    ))
    .unwrap()
}

/// A delta poll's page, as the AlphAI task delivers it.
fn delta_page(app: &mut App, key: &str, articles: Vec<Article>, cursor: &str) {
    app.apply_alphai(alphai::Event::Feed {
        key: key.into(),
        articles,
        side: None,
        next_cursor: Some(cursor.into()),
        mode: alphai::FeedMode::Merge,
        min_relevance: Some(7),
    });
}

/// Arrivals land above the shown feed, newest publication first, and a row
/// the feed already carries is dropped. The paging cursor is untouched: the
/// two cursor families are mutually unreadable and crossing them is a 400.
#[test]
fn delta_merge_prepends_arrivals_and_dedupes() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![timed_article("aaa", "Shown", "2026-07-10T12:00:00Z")],
            None,
            Some("page1".into()),
        ),
    );
    delta_page(&mut app, "AAPL", vec![], "d1");
    delta_page(
        &mut app,
        "AAPL",
        vec![
            timed_article("aaa", "Shown, reported again", "2026-07-10T12:00:00Z"),
            timed_article("bbb", "Arrived, published earlier", "2026-07-10T11:00:00Z"),
            timed_article("ccc", "Arrived, published later", "2026-07-10T11:30:00Z"),
        ],
        "d2",
    );
    let b = &app.feeds["AAPL"];
    let titles: Vec<&str> = b
        .articles
        .iter()
        .map(|a| a.original.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Arrived, published later",
            "Arrived, published earlier",
            "Shown"
        ],
        "arrivals must sit on top, newest publication first"
    );
    assert_eq!(b.delta_cursor.as_deref(), Some("d2"));
    assert_eq!(
        b.next_cursor.as_deref(),
        Some("page1"),
        "merge moved the paging cursor"
    );
}

/// The priming poll only parks the polling position, so it is a baseline.
/// Every later poll carries genuine arrivals and marks.
#[test]
fn delta_prime_is_a_baseline_and_later_polls_mark() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    head_fetch(&mut app, "AAPL", vec![uid_article("aaa", "First")], Some(7));
    delta_page(
        &mut app,
        "AAPL",
        vec![uid_article("bbb", "Late arrival")],
        "d1",
    );
    assert!(
        !app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]),
        "the priming page marked a row as new"
    );
    delta_page(&mut app, "AAPL", vec![uid_article("ccc", "Just in")], "d2");
    assert!(
        app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]),
        "an arrival is not marked as new"
    );
}

/// The priming page is the newest rows BY ARRIVAL, so its bottom edge sits at
/// a different row than the published page's, and the rows below that edge are
/// old news the head page cut off, not arrivals. They must not be merged: a
/// twelve-day-old article was landing on top of a feed whose newest row was an
/// hour old. Rows above the floor still merge, because an article that reached
/// the feed between the head fetch and the first poll is a real arrival.
#[test]
fn delta_prime_drops_rows_below_the_window() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    head_fetch(
        &mut app,
        "AAPL",
        vec![
            timed_article("aaa", "Newest shown", "2026-09-08T13:41:00Z"),
            timed_article("bbb", "Oldest shown", "2026-08-27T16:33:00Z"),
        ],
        Some(7),
    );
    delta_page(
        &mut app,
        "AAPL",
        vec![
            timed_article("ccc", "Slow to reach the feed", "2026-08-27T13:39:00Z"),
            timed_article("aaa", "Newest shown", "2026-09-08T13:41:00Z"),
            timed_article(
                "ddd",
                "Arrived since the head fetch",
                "2026-09-08T14:00:00Z",
            ),
        ],
        "d1",
    );
    let titles: Vec<&str> = app.feeds["AAPL"]
        .articles
        .iter()
        .map(|a| a.original.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Arrived since the head fetch",
            "Newest shown",
            "Oldest shown"
        ],
        "the priming page merged a row from below the window"
    );

    // A later poll knows its rows arrived while the reader watched, so an old
    // publication date is not a reason to hide one: that is what polling by
    // arrival is for.
    delta_page(
        &mut app,
        "AAPL",
        vec![timed_article("eee", "Late but new", "2026-08-20T10:00:00Z")],
        "d2",
    );
    assert_eq!(
        app.feeds["AAPL"].articles[0].original.title, "Late but new",
        "a real arrival below the window was dropped"
    );
    assert!(
        app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]),
        "an arrival is not marked as new"
    );
}

/// A background merge never changes the row under the cursor: without the
/// shift, `mark_selected_seen` would retire the marker of a row the reader
/// never opened. A merge into a feed that is not on screen moves nothing.
#[test]
fn merge_keeps_the_row_under_the_cursor() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    head_fetch(
        &mut app,
        "AAPL",
        vec![uid_article("aaa", "First"), uid_article("bbb", "Second")],
        Some(7),
    );
    app.news_selected = 1;
    delta_page(&mut app, "AAPL", vec![], "d1");
    delta_page(
        &mut app,
        "AAPL",
        vec![
            uid_article("ccc", "Arrived"),
            uid_article("ddd", "Also arrived"),
        ],
        "d2",
    );
    assert_eq!(
        app.news_selected, 3,
        "the cursor did not move with the rows"
    );
    assert_eq!(
        app.feeds["AAPL"].articles[app.news_selected].original.title, "Second",
        "the cursor changed rows under the reader"
    );

    let insider = alphai::insider_key("AAPL");
    app.feeds.insert(
        insider.clone(),
        FeedBundle::new(vec![uid_article("i1", "Filing")], None, None),
    );
    delta_page(
        &mut app,
        &insider,
        vec![uid_article("i2", "New filing")],
        "d3",
    );
    assert_eq!(app.news_selected, 3, "a merge off screen moved the cursor");
}

/// A failed poll is not a failed view: the feed stays, the reason goes under
/// the list, and polling stops until the reader retries, because nothing here
/// ever retries by itself.
#[test]
fn poll_error_keeps_the_feed_and_stops_polling() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    app.apply_alphai(alphai::Event::PollError {
        key: "AAPL".into(),
        error: "AlphAI API 429: slow down".into(),
    });
    let screen = render(&mut app);
    assert!(
        screen.contains("Apple beats expectations"),
        "poll error blanked the feed:\n{screen}"
    );
    assert!(screen.contains("429"), "poll error not reported:\n{screen}");
    app.feeds.get_mut("AAPL").unwrap().polled =
        Instant::now() - std::time::Duration::from_secs(600);
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "a failed poll retried by itself");
}

/// A rejected poll position (400) or one past the archive horizon (403) is
/// not worth showing: drop it and prime a fresh one on the next tick.
#[test]
fn poll_reprime_drops_the_position_and_primes_again() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(vec![article("First", "AAPL", 9, "positive")], None, None),
    );
    app.feeds.get_mut("AAPL").unwrap().delta_cursor = Some("stale".into());
    app.apply_alphai(alphai::Event::PollReprime { key: "AAPL".into() });
    let b = &app.feeds["AAPL"];
    assert_eq!(b.delta_cursor, None, "the rejected position survived");
    assert!(!b.poll_stopped, "a reprime must not stop polling");
    assert!(
        app.alphai_errors.is_empty(),
        "a reprime surfaced as an error"
    );
    app.feeds.get_mut("AAPL").unwrap().polled =
        Instant::now() - std::time::Duration::from_secs(600);
    app.ensure_alphai_data();
    assert!(
        matches!(
            cmds.try_recv(),
            Ok(alphai::Cmd::FetchNews {
                cursor: None,
                sort: alphai::Sort::Ingested,
                ..
            })
        ),
        "the next tick did not prime a fresh position"
    );
}

/// Every fetch pauses while an overlay is open, the poll included: merging
/// rows under a reader who is inside the article would move the list out
/// from under them.
#[test]
fn an_open_overlay_pauses_the_poll() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(vec![article("First", "AAPL", 9, "positive")], None, None),
    );
    app.feeds.get_mut("AAPL").unwrap().polled =
        Instant::now() - std::time::Duration::from_secs(600);
    app.article_overlay.open = true;
    app.ensure_alphai_data();
    assert!(
        cmds.try_recv().is_err(),
        "polled with the article overlay open"
    );
    app.article_overlay.open = false;
    app.ensure_alphai_data();
    assert!(
        matches!(
            cmds.try_recv(),
            Ok(alphai::Cmd::FetchNews {
                sort: alphai::Sort::Ingested,
                ..
            })
        ),
        "the poll did not resume once the overlay closed"
    );
}

/// Trending is a fixed top-10 endpoint with no cursor and no `sort`, so its
/// tick stays the head refetch every feed used to do.
#[test]
fn trending_tick_stays_a_head_refetch() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.news_scope = NewsScope::Trending;
    app.feeds.insert(
        alphai::TRENDING_KEY.into(),
        FeedBundle::new(
            vec![article("Trending story", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    let stale = Instant::now() - std::time::Duration::from_secs(600);
    let b = app.feeds.get_mut(alphai::TRENDING_KEY).unwrap();
    b.fetched = stale;
    b.polled = stale;
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchTrending)),
        "trending stopped refetching its head"
    );
}

/// The TTL tick is a delta poll now, and a merge keeps the reader's place,
/// so it goes out wherever the cursor sits. It used to do nothing at all
/// below the top row, which left a scrolled reader with no updates.
#[test]
fn ttl_tick_polls_wherever_the_reader_is() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![
                article("First", "AAPL", 9, "positive"),
                article("Second", "AAPL", 8, "positive"),
            ],
            None,
            None,
        ),
    );
    app.feeds.get_mut("AAPL").unwrap().polled =
        Instant::now() - std::time::Duration::from_secs(600);
    app.news_selected = 1;
    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchNews { cursor, sort, .. }) => {
            assert_eq!(cursor, None, "the first poll primes a position");
            assert_eq!(
                sort,
                alphai::Sort::Ingested,
                "the tick must be a delta poll"
            );
        }
        other => panic!("expected a delta poll, got {:?}", other.is_ok()),
    }
    assert!(cmds.try_recv().is_err(), "one tick, one request");
}

/// The head fetch replaces the bundle, so it still waits for the top row —
/// but it is now the rarer side-payload refresh, several TTLs apart, and the
/// tick under a scrolled reader is a poll instead of nothing.
#[test]
fn head_refresh_still_waits_for_top_row() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(vec![article("First", "AAPL", 9, "positive")], None, None),
    );
    let stale = Instant::now() - std::time::Duration::from_secs(1_500);
    let b = app.feeds.get_mut("AAPL").unwrap();
    b.fetched = stale;
    b.polled = stale;
    app.news_selected = 1;
    app.ensure_alphai_data();
    assert!(
        matches!(
            cmds.try_recv(),
            Ok(alphai::Cmd::FetchNews {
                sort: alphai::Sort::Ingested,
                ..
            })
        ),
        "a head fetch cannot run under the reader"
    );
    // The poll comes back caught up, which also clears the in-flight guard.
    app.apply_alphai(alphai::Event::Feed {
        key: "AAPL".into(),
        articles: vec![],
        side: None,
        next_cursor: Some("delta1".into()),
        mode: alphai::FeedMode::Merge,
        min_relevance: None,
    });
    app.news_selected = 0;
    app.ensure_alphai_data();
    assert!(
        matches!(
            cmds.try_recv(),
            Ok(alphai::Cmd::FetchNews {
                cursor: None,
                sort: alphai::Sort::Published,
                ..
            })
        ),
        "no head refresh at the top row"
    );
}

/// Resolved [chart] and [ui] values seed the startup state (the session
/// keys keep toggling everything afterwards) and the t cycle walks the
/// configured presets.
#[test]
fn config_defaults_seed_startup_state() {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (alphai_tx, _alphai_rx) = tokio::sync::mpsc::unbounded_channel();
    let source = make_source("yahoo", &Config::default()).unwrap();
    let mut app = App::new(AppInit {
        positions: Vec::new(),
        shared_symbols: Arc::new(RwLock::new(vec!["AAPL".into()])),
        symbols: vec!["AAPL".into()],
        sessions: Sessions::Regular,
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5, Sessions::Regular))),
        every: Arc::new(RwLock::new(std::time::Duration::from_secs(15))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        config_path: None,
        theme: Theme::default(),
        theme_name: crate::theme::DEFAULT_PRESET,
        chart: ChartDefaults {
            style: ChartStyle::Line,
            sma: false,
            ma_type: crate::indicators::MaType::Ema,
            rsi: false,
            volume: false,
            presets: vec![(Range::D5, Interval::M15), (Range::Y1, Interval::D1)],
            ..Default::default()
        },
        ui: UiDefaults {
            view_idx: ui::view_index(ui::ViewId::News),
            news_layout: NewsLayout::Stacked,
            news_scope: NewsScope::Market,
            news_min_score: 6,
            insider_min_score: 5,
            ..Default::default()
        },
        keymap: crate::keymap::Keymap::default(),
        alphai_enabled: false,
        first_run: false,
        source_fallback: true,
    });
    assert_eq!(app.view_idx, ui::view_index(ui::ViewId::News));
    assert_eq!(app.chart_style, ChartStyle::Line);
    assert!(!app.show_sma && !app.show_rsi && !app.show_volume);
    assert_eq!(app.ma_type, crate::indicators::MaType::Ema);
    assert_eq!(app.news_layout, NewsLayout::Stacked);
    assert_eq!(app.news_scope, NewsScope::Market);
    assert_eq!(app.news_min_score, 6);
    assert_eq!(app.insider_min_score, 5);
    // t cycles the configured presets, not the built-in table.
    press(&mut app, KeyCode::Char('t'));
    assert_eq!((app.range, app.interval), (Range::D5, Interval::M15));
    press(&mut app, KeyCode::Char('t'));
    assert_eq!((app.range, app.interval), (Range::Y1, Interval::D1));
    press(&mut app, KeyCode::Char('t'));
    assert_eq!((app.range, app.interval), (Range::D5, Interval::M15));
}

/// Empty config = the default look; a remapped accent slot recolors the
/// brand cell in the header.
#[test]
fn theme_accent_recolors_the_header() {
    use ratatui::style::Color;
    let mut app = fake_app();
    let fg_of_brand = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        terminal.backend().buffer().cell((1, 0)).unwrap().fg
    };
    assert_eq!(fg_of_brand(&mut app), Color::Cyan);
    app.theme.accent = Color::Magenta;
    assert_eq!(fg_of_brand(&mut app), Color::Magenta);
}

/// A pane too narrow for every column drops whole columns instead of
/// letting ratatui squeeze all of them (which turned prices into "206."
/// and ranges into "319.54–3" in the split view).
#[test]
fn narrow_watchlist_drops_columns_instead_of_truncating() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    // The assertions read the whole screen; the quote rail carries the
    // same numbers and would answer for the table's own columns.
    app.show_rail = false;
    let wide = render_sized(&mut app, 100, 12);
    assert!(wide.contains("Lo–Hi"), "screen:\n{wide}");
    assert!(wide.contains("214.50"), "screen:\n{wide}");

    // Room for everything but the range column.
    let mid = render_sized(&mut app, 60, 12);
    assert!(
        !mid.contains("Lo–Hi"),
        "range column should be gone:\n{mid}"
    );
    assert!(mid.contains("214.50"), "price truncated:\n{mid}");
    assert!(mid.contains("+7.25%"), "percent truncated:\n{mid}");

    // Narrower still: the absolute change goes too, the price stays whole.
    let narrow = render_sized(&mut app, 46, 12);
    assert!(
        !narrow.contains("+14.50"),
        "change should be gone:\n{narrow}"
    );
    assert!(narrow.contains("214.50"), "price truncated:\n{narrow}");
    assert!(narrow.contains("+7.25%"), "percent truncated:\n{narrow}");
}

/// Headlines wider than their column end in an ellipsis instead of being
/// chopped mid-word at the border.
#[test]
fn long_headlines_are_ellipsized() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article(
                "Apple reports the single longest headline any newsroom has ever \
                 published about a quarterly earnings call",
                "AAPL",
                9,
                "positive",
            )],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    // The list row is cut with an ellipsis (the card pane still has it all).
    let row = screen
        .lines()
        .find(|l| l.contains('▶'))
        .unwrap_or_else(|| panic!("no selected row:\n{screen}"));
    assert!(row.contains('…'), "no ellipsis in the row:\n{screen}");
    assert!(!row.contains("newsroom"), "row not cut:\n{screen}");
}

#[test]
fn ellipsize_counts_characters_not_bytes() {
    assert_eq!(ui::ellipsize("abcdef", 4), "abc…");
    // Multi-byte input must not split a character (byte truncation panics).
    assert_eq!(ui::ellipsize("привет", 3), "пр…");
    assert_eq!(ui::ellipsize("short", 20), "short");
    // Width 0 means the caller does not know the column: pass it through.
    assert_eq!(ui::ellipsize("short", 0), "short");
}

/// The cycle key rebuilds the theme the same way a restart would: the
/// preset lands, the slots written out in `[theme]` still win, and the
/// frame style from `[ui] borders` survives.
#[test]
fn theme_key_cycles_presets_over_explicit_slots() {
    use ratatui::style::Color;
    use ratatui::widgets::BorderType;

    let mut app = fake_app();
    app.config.theme = Some(std::collections::BTreeMap::from([(
        "up".to_string(),
        "#00c853".to_string(),
    )]));
    app.theme.border_type = BorderType::Plain;
    app.set_theme("catppuccin-mocha");
    assert_eq!(app.theme.accent, Color::Rgb(0xcb, 0xa6, 0xf7));
    assert_eq!(
        app.theme.up,
        Color::Rgb(0x00, 0xc8, 0x53),
        "explicit slot lost"
    );
    assert_eq!(
        app.theme.border_type,
        BorderType::Plain,
        "[ui] borders lost"
    );

    press(&mut app, KeyCode::Char('}'));
    assert_eq!(app.theme_name, "catppuccin-macchiato");
    assert_eq!(app.theme.accent, Color::Rgb(0xc6, 0xa0, 0xf6));

    // { walks back, so overshooting is one keypress to undo.
    press(&mut app, KeyCode::Char('{'));
    assert_eq!(app.theme_name, "catppuccin-mocha");
    press(&mut app, KeyCode::Char('}'));

    // Every preset is reachable from the keyboard, and the cycle wraps
    // back to where it started after a full lap.
    for _ in 0..crate::theme::PRESETS.len() {
        press(&mut app, KeyCode::Char('}'));
    }
    assert_eq!(app.theme_name, "catppuccin-macchiato");
    assert_eq!(app.theme.up, Color::Rgb(0x00, 0xc8, 0x53));
}

/// Save persists the picked preset into `[theme] preset`; picking the
/// built-in theme takes the line back out rather than writing a no-op.
#[test]
fn settings_theme_row_persists_the_preset() {
    let mut app = empty_app(vec!["AAPL".into()]);
    press(&mut app, KeyCode::Char('s'));
    while !matches!(
        settings_rows()[app.settings.cursor],
        SettingsRow::ThemeChoice
    ) {
        press(&mut app, KeyCode::Down);
    }
    let screen = render(&mut app);
    assert!(screen.contains("Theme"), "screen:\n{screen}");

    // Cycling previews live, like the p key does, and the arrows really
    // point somewhere: left walks back through the list, right forward.
    press(&mut app, KeyCode::Left);
    assert_eq!(
        app.settings.theme_choice,
        crate::theme::PRESETS[crate::theme::PRESETS.len() - 1].0,
        "left arrow must step back, not forward"
    );
    press(&mut app, KeyCode::Right);
    assert_eq!(app.settings.theme_choice, crate::theme::DEFAULT_PRESET);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.settings.theme_choice, "catppuccin-mocha");
    assert_eq!(app.theme_name, "catppuccin-mocha");
    let cfg = app.settings_merged_config();
    assert_eq!(
        cfg.theme
            .as_ref()
            .and_then(|t| t.get("preset"))
            .map(String::as_str),
        Some("catppuccin-mocha")
    );

    // All the way back around to the built-in theme: no key, and no empty
    // [theme] table left behind either.
    for _ in 1..crate::theme::PRESETS.len() {
        press(&mut app, KeyCode::Right);
    }
    assert_eq!(app.settings.theme_choice, crate::theme::DEFAULT_PRESET);
    assert_eq!(app.settings_merged_config().theme, None);
}

/// Every framed panel must come from `Theme::panel`, so a theme really
/// recolors the whole frame. Corners are the tell: nothing but a block
/// border draws them, and a panel built with a bare `Block::bordered()`
/// would keep the default color and the square corner set.
#[test]
fn borders_are_themed() {
    use ratatui::style::Color;
    const ROUNDED: [&str; 4] = ["╭", "╮", "╰", "╯"];
    const SQUARE: [&str; 4] = ["┌", "┐", "└", "┘"];

    let mut app = fake_app();
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    app.feeds.insert(
        alphai::insider_key("AAPL"),
        FeedBundle::new(
            vec![filing("Apple insider sold $12.5M of stock", "direct")],
            None,
            None,
        ),
    );
    app.theme.border = Color::Rgb(1, 2, 3);

    for view in ui::VIEWS {
        app.view_idx = ui::view_index(view.id());
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut corners = 0;
        for y in buffer.area.top()..buffer.area.bottom() {
            for x in buffer.area.left()..buffer.area.right() {
                let cell = buffer.cell((x, y)).unwrap();
                assert!(
                    !SQUARE.contains(&cell.symbol()),
                    "{:?}: a panel bypassed Theme::panel (square corner at {x},{y})",
                    view.id()
                );
                if ROUNDED.contains(&cell.symbol()) {
                    corners += 1;
                    assert_eq!(
                        cell.fg,
                        Color::Rgb(1, 2, 3),
                        "{:?}: unthemed border at {x},{y}",
                        view.id()
                    );
                }
            }
        }
        assert!(corners >= 4, "{:?}: no framed panel rendered", view.id());
    }
}

/// Save must start from the loaded config and replace only the settings
/// screen's own fields, so file-only sections like [theme] survive it.
#[test]
fn settings_save_merge_preserves_file_only_sections() {
    let mut app = fake_app();
    app.config.positions = vec![Position {
        symbol: "AAPL".into(),
        qty: 12.0,
        avg_price: 182.31,
    }];
    app.config.theme = Some(std::collections::BTreeMap::from([(
        "accent".to_string(),
        "magenta".to_string(),
    )]));
    app.open_settings();
    app.settings
        .key_values
        .insert("alphai", "ak_live_new".into());
    let merged = app.settings_merged_config();
    assert_eq!(
        merged
            .theme
            .as_ref()
            .and_then(|t| t.get("accent"))
            .map(String::as_str),
        Some("magenta")
    );
    assert_eq!(
        merged.keys.get("alphai").map(String::as_str),
        Some("ak_live_new")
    );
    assert_eq!(
        merged.watchlist,
        vec!["AAPL".to_string(), "MSFT".to_string()]
    );
    // Positions are written by their own prompt, so Save has to carry the
    // loaded ones through untouched rather than drop them.
    assert_eq!(merged.positions, app.config.positions);
    assert_eq!(merged.positions.len(), 1);
}

/// The single-copy guards must hold for every feed kind: the insider tick
/// polls too, and its head refresh still waits for the top row. Delta mode
/// matters most here, since a Form 4 is filed days after its trade and
/// lands below the head of the published page.
#[test]
fn insider_tick_polls_and_head_refresh_waits_for_top_row() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    let key = alphai::insider_key("AAPL");
    app.feeds.insert(
        key.clone(),
        FeedBundle::new(
            vec![
                filing("Apple insider sold $12.5M of stock", "direct"),
                filing("Officer bought 10,000 shares", "indirect"),
            ],
            None,
            None,
        ),
    );
    let stale = Instant::now() - std::time::Duration::from_secs(1_500);
    let b = app.feeds.get_mut(&key).unwrap();
    b.fetched = stale;
    b.polled = stale;
    app.news_selected = 1;
    app.ensure_alphai_data();
    assert!(
        matches!(
            cmds.try_recv(),
            Ok(alphai::Cmd::FetchInsider {
                sort: alphai::Sort::Ingested,
                ..
            })
        ),
        "insider head fetch ran under the reader"
    );
    app.apply_alphai(alphai::Event::Feed {
        key: key.clone(),
        articles: vec![],
        side: None,
        next_cursor: Some("delta1".into()),
        mode: alphai::FeedMode::Merge,
        min_relevance: None,
    });
    app.news_selected = 0;
    app.ensure_alphai_data();
    assert!(
        matches!(
            cmds.try_recv(),
            Ok(alphai::Cmd::FetchInsider {
                cursor: None,
                sort: alphai::Sort::Published,
                ..
            })
        ),
        "no insider head refresh at the top row"
    );
}

#[test]
fn settings_alphai_key_hint_when_missing() {
    if std::env::var("ALPHAI_API_KEY").is_ok() {
        return; // the env-override hint takes precedence; nothing to assert
    }
    let mut app = fake_app();
    app.open_settings();
    let screen = render(&mut app);
    assert!(
        screen.contains("get free on alphai.io/developers"),
        "missing-key hint absent:\n{screen}"
    );
    // Once a key is stored the hint disappears.
    app.config
        .keys
        .insert("alphai".into(), "ak_live_abcdefgh1234".into());
    app.open_settings();
    let screen = render(&mut app);
    assert!(
        !screen.contains("get free on alphai.io/developers"),
        "hint shown next to a configured key:\n{screen}"
    );
}

#[test]
fn settings_overlay_masks_keys() {
    let mut app = fake_app();
    app.config
        .keys
        .insert("alphai".into(), "ak_live_abcdefgh1234".into());
    app.config
        .keys
        .insert("alpaca_secret".into(), "alpaca-secret-abcd9876".into());
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
    let source = make_source("yahoo", &Config::default()).unwrap();
    let mut app = App::new(AppInit {
        positions: Vec::new(),
        shared_symbols: Arc::new(RwLock::new(vec!["AAPL".into()])),
        symbols: vec!["AAPL".into()],
        sessions: Sessions::Regular,
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5, Sessions::Regular))),
        every: Arc::new(RwLock::new(std::time::Duration::from_secs(15))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        config_path: None,
        theme: Theme::default(),
        theme_name: crate::theme::DEFAULT_PRESET,
        chart: ChartDefaults::default(),
        ui: UiDefaults::default(),
        keymap: crate::keymap::Keymap::default(),
        alphai_enabled: false,
        first_run: true,
        source_fallback: true,
    });
    assert!(app.settings.open);
    let screen = render(&mut app);
    assert!(
        screen.contains("Welcome to alphai-tui"),
        "screen:\n{screen}"
    );
    assert!(screen.contains("https://alphai.io"), "screen:\n{screen}");
}

/// The Poll every settings row: seeded from the live interval, edited like a
/// key field, validated and applied on Save. config_path is None here so the
/// file write fails, but the live interval and in-memory config still apply.
#[test]
fn settings_poll_every_saves_and_applies_live() {
    let mut app = empty_app(vec!["AAPL".into()]);
    press(&mut app, KeyCode::Char('s'));
    assert!(app.settings.open);
    assert_eq!(app.settings.every_input, "15");
    let screen = render(&mut app);
    assert!(screen.contains("Poll every"), "screen:\n{screen}");
    assert!(screen.contains("15s"), "screen:\n{screen}");

    while !matches!(settings_rows()[app.settings.cursor], SettingsRow::PollEvery) {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.settings.editing);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Char('5'));
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.settings.every_input, "5");

    while !matches!(settings_rows()[app.settings.cursor], SettingsRow::Save) {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.config.every, Some(5));
    // Reopening reseeds from the shared interval the poller reads: the live
    // value really changed.
    app.open_settings();
    assert_eq!(app.settings.every_input, "5");
}

/// A bad interval blocks Save with a message instead of half-applying.
#[test]
fn settings_poll_every_rejects_bad_input() {
    for bad in ["abc", "1", "0", ""] {
        let mut app = empty_app(vec!["AAPL".into()]);
        press(&mut app, KeyCode::Char('s'));
        app.settings.every_input = bad.to_string();
        while !matches!(settings_rows()[app.settings.cursor], SettingsRow::Save) {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Enter);
        assert!(
            app.settings.open,
            "at {bad:?}: settings closed on bad input"
        );
        assert!(
            app.settings
                .message
                .as_deref()
                .is_some_and(|m| m.contains("poll interval")),
            "at {bad:?}: no validation message"
        );
        assert_eq!(app.config.every, None, "at {bad:?}: bad value persisted");
        app.open_settings();
        assert_eq!(
            app.settings.every_input, "15",
            "at {bad:?}: live interval changed"
        );
    }
}

// ---------------------------------------------------------------------------
// Earnings view, the article card's read, and the request budget behind both.

/// One ticker's earnings payload, trimmed to what a render needs.
fn earnings_data(ticker: &str, next: &str, metrics: &str) -> alphai::TickerEarnings {
    let next = if next.is_empty() {
        "null".to_string()
    } else {
        format!("\"{next}\"")
    };
    serde_json::from_str(&format!(
        r#"{{
          "ticker": "{ticker}",
          "next_report_date": {next},
          "reports": [{{
            "uid": "earn1",
            "time_published": "2026-08-26T20:21:19Z",
            "title": "{ticker} CORP: Results of Operations and Financial Condition",
            "source_type": "sec_form8k",
            "ticker": "{ticker}",
            "fiscal_period": "Second Quarter Fiscal 2027",
            "analysis": {{
              "company": "{ticker} CORP",
              "ticker": "{ticker}",
              "fiscal_period": "Second Quarter Fiscal 2027",
              "period_end": "July 26, 2026",
              "headline": "{ticker} announces second quarter results",
              "verdict": "strong",
              "verdict_reason": "Revenue grew 18% sequentially.",
              "key_metrics": [{metrics}],
              "segments": [{{"name": "Data Center", "revenue": "$89.0 billion",
                             "yoy_change": "117%", "qoq_change": "18%",
                             "driver": "Vera Rubin ramping."}}],
              "guidance": {{"period": "Third quarter fiscal 2027",
                           "revenue": "$108.0 billion, plus or minus 2%",
                           "gross_margin": null, "operating_expenses": null,
                           "tax_rate": null, "other": []}},
              "vs_prior_guidance": [],
              "capital_returns": [], "balance_sheet_cash_flow": [], "drivers": [],
              "concerns": ["No China compute revenue is assumed."],
              "what_to_watch": [], "quotes": [],
              "analysis": "The quarter was strong.",
              "missing_items": [],
              "numbers_verified_from_document": true
            }}
          }}]
        }}"#
    ))
    .unwrap()
}

const FULL_METRICS: &str = r#"
  {"name": "Revenue", "value": "$96,221 million", "basis": "GAAP",
   "prior_year": "$46,743 million", "prior_quarter": "$81,615 million",
   "yoy_change": "106%", "qoq_change": "18%"},
  {"name": "Cost of revenue", "value": "$24,079 million", "basis": "GAAP",
   "prior_year": "$12,890 million", "prior_quarter": "$20,458 million",
   "yoy_change": null, "qoq_change": null},
  {"name": "Basic earnings per share", "value": "$2.47 per share", "basis": "GAAP",
   "prior_year": "$1.08 per share", "prior_quarter": null,
   "yoy_change": null, "qoq_change": null},
  {"name": "Diluted earnings per share", "value": "$2.46 per diluted share", "basis": "GAAP",
   "prior_year": "$1.08 per diluted share", "prior_quarter": "$2.39 per diluted share",
   "yoy_change": "128%", "qoq_change": "3%"}
"#;

/// Deliver a fetched payload the way the background task would.
fn earnings_fetch(app: &mut App, symbol: &str, data: alphai::TickerEarnings) {
    app.apply_alphai(alphai::Event::Earnings {
        key: alphai::earnings_key(symbol),
        data: Box::new(data),
    });
}

fn earnings_app(
    metrics: &str,
    next: &str,
) -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (mut app, cmds) = empty_app_with_cmds(vec!["NVDA".into(), "AVGO".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Earnings);
    let data = earnings_data("NVDA", next, metrics);
    earnings_fetch(&mut app, "NVDA", data);
    (app, cmds)
}

#[test]
fn earnings_view_renders_a_read() {
    let (mut app, _cmds) = earnings_app(FULL_METRICS, "2026-11-17");
    let screen = render_sized(&mut app, 110, 32);
    assert!(
        screen.contains("Second Quarter Fiscal 2027"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("strong"),
        "the verdict is the one colored word"
    );
    assert!(
        screen.contains("prior Q") && screen.contains("y/y"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("$96,221M"),
        "figures shorten units, not numbers"
    );
    assert!(
        !screen.contains("$96.2B"),
        "a rounded figure is a new number"
    );
    assert!(screen.contains("Data Center"), "segments render");
    assert!(
        screen.contains("Third quarter fiscal 2027"),
        "guidance renders"
    );
    assert!(
        screen.contains("No China compute revenue"),
        "concerns render"
    );
    assert!(
        screen.contains("next report Nov 17, 2026"),
        "screen:\n{screen}"
    );
}

/// The most common screen of the feature: covered, nothing published yet.
/// It has to read as an answer, not as a failure.
#[test]
fn earnings_empty_state_shows_the_next_report_date() {
    let (mut app, _cmds) = empty_app_with_cmds(vec!["AVGO".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Earnings);
    earnings_fetch(
        &mut app,
        "AVGO",
        serde_json::from_str(r#"{"ticker":"AVGO","reports":[],"next_report_date":"2026-09-02"}"#)
            .unwrap(),
    );
    let screen = render(&mut app);
    assert!(
        screen.contains("No earnings read for AVGO yet"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("next report Sep 2, 2026"),
        "screen:\n{screen}"
    );
    assert!(
        !screen.to_lowercase().contains("error"),
        "screen:\n{screen}"
    );
    assert!(!screen.contains("press r"), "nothing here is retryable");

    // No confirmed date is a different sentence, and still not an error.
    earnings_fetch(
        &mut app,
        "AVGO",
        serde_json::from_str(r#"{"ticker":"AVGO","reports":[],"next_report_date":null}"#).unwrap(),
    );
    let screen = render(&mut app);
    assert!(screen.contains("not confirmed yet"), "screen:\n{screen}");
}

/// A 404 is a state, not a failure: offering a retry that can never work is
/// exactly the loop the budget rules forbid.
#[test]
fn earnings_unknown_ticker_is_terminal() {
    let (mut app, _cmds) = empty_app_with_cmds(vec!["BTC-USD".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Earnings);
    let data = alphai::TickerEarnings {
        unknown: true,
        ..Default::default()
    };
    earnings_fetch(&mut app, "BTC-USD", data);
    let screen = render(&mut app);
    assert!(
        screen.contains("no earnings coverage for BTC-USD"),
        "screen:\n{screen}"
    );
    assert!(!screen.contains("press r"), "screen:\n{screen}");
}

/// Thirty numeric rows only read if the eye can get from a name to its
/// figures: alternating bands and a leader of dots do that work.
#[test]
fn earnings_metric_rows_are_banded_and_led() {
    let (mut app, _cmds) = earnings_app(FULL_METRICS, "2026-11-17");
    let screen = render_sized(&mut app, 200, 32);
    assert!(
        screen.contains("Revenue   · ·"),
        "no leader between a short name and its figures:\n{screen}"
    );

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 32)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    // Locate the two rows by their own figures, so this measures the metric
    // rows themselves and not whatever prose happens to sit above them.
    let row_of = |needle: &str| {
        (0..32)
            .find(|y| {
                (0..200)
                    .map(|x| buffer.cell((x, *y)).unwrap().symbol())
                    .collect::<String>()
                    .contains(needle)
            })
            .unwrap_or_else(|| panic!("no row carrying {needle}"))
    };
    let revenue = row_of("$96,221M");
    let next = row_of("$24,079M");
    assert_eq!(next, revenue + 1, "the two rows are not neighbours");
    assert_ne!(
        buffer.cell((4, revenue)).unwrap().style(),
        buffer.cell((4, next)).unwrap().style(),
        "neighbouring metric rows render identically, so nothing bands them"
    );
}

#[test]
fn earnings_view_fits_80x24() {
    let (mut app, _cmds) = earnings_app(FULL_METRICS, "2026-11-17");
    let screen = render_sized(&mut app, 80, 24);
    assert!(screen.contains("strong"), "screen:\n{screen}");
    assert!(screen.contains("value"), "the column header survives");
    assert!(screen.contains("$96,221M"), "screen:\n{screen}");
}

/// Budget: nothing is fetched for a view that is not on screen, the fetch
/// is one request per ticker, and a second pass is absorbed by the inflight
/// guard rather than spending another.
#[test]
fn earnings_fetch_is_demand_driven() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["NVDA".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Table);
    app.ensure_alphai_data();
    while let Ok(cmd) = cmds.try_recv() {
        assert!(
            !matches!(cmd, alphai::Cmd::FetchEarnings { .. }),
            "a hidden view fetched its earnings"
        );
    }

    app.view_idx = ui::view_index(ui::ViewId::Earnings);
    app.ensure_alphai_data();
    let mut fetches = 0;
    while let Ok(cmd) = cmds.try_recv() {
        if matches!(cmd, alphai::Cmd::FetchEarnings { .. }) {
            fetches += 1;
        }
    }
    assert_eq!(fetches, 1, "one request per ticker");

    app.ensure_alphai_data();
    assert!(
        cmds.try_recv().is_err(),
        "a second pass fetched again while the first was in flight"
    );

    earnings_fetch(
        &mut app,
        "NVDA",
        earnings_data("NVDA", "2026-11-17", FULL_METRICS),
    );
    app.ensure_alphai_data();
    assert!(
        cmds.try_recv().is_err(),
        "a fresh read refetched inside its TTL"
    );
}

/// Walking the watchlist costs one request per round trip, not one per
/// frame: only one earnings fetch may be in flight at a time.
#[test]
fn earnings_arrows_do_not_burst_requests() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["NVDA".into(), "AVGO".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Earnings);
    app.ensure_alphai_data();
    press(&mut app, KeyCode::Right);
    app.ensure_alphai_data();
    press(&mut app, KeyCode::Left);
    app.ensure_alphai_data();
    let fetches = std::iter::from_fn(|| cmds.try_recv().ok())
        .filter(|c| matches!(c, alphai::Cmd::FetchEarnings { .. }))
        .count();
    assert_eq!(fetches, 1, "arrow keys queued a request per frame");
}

/// r refreshes what is on screen and nothing else.
#[test]
fn earnings_refresh_touches_only_the_visible_surface() {
    let (mut app, _cmds) = earnings_app(FULL_METRICS, "2026-11-17");
    app.feeds.insert(
        "NVDA".into(),
        FeedBundle::new(vec![article("Kept", "NVDA", 8, "positive")], None, None),
    );
    app.calendar = Some((Vec::new(), Instant::now()));
    press(&mut app, KeyCode::Char('r'));
    assert!(
        !app.earnings.contains_key("NVDA"),
        "r left the stale read in place"
    );
    assert!(
        app.feeds.contains_key("NVDA"),
        "r dropped a feed it was not showing"
    );
    assert!(
        app.calendar.is_none(),
        "r left an empty calendar with no way to retry it"
    );

    // A calendar that holds events is not worth a second request: the
    // schedule moves about once a month.
    earnings_fetch(
        &mut app,
        "NVDA",
        earnings_data("NVDA", "2026-11-17", FULL_METRICS),
    );
    app.apply_alphai(alphai::Event::Calendar {
        events: vec![alphai::CalendarEvent::default()],
    });
    press(&mut app, KeyCode::Char('r'));
    assert!(
        app.calendar.is_some(),
        "r spent a request on a fresh calendar"
    );
}

/// The body is a document: up/down scroll it, left/right walk the watchlist.
#[test]
fn earnings_arrows_switch_ticker_and_jk_scroll() {
    let (mut app, _cmds) = earnings_app(FULL_METRICS, "2026-11-17");
    render_sized(&mut app, 110, 20);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.earnings_scroll, 1,
        "j moved the watchlist instead of the page"
    );
    assert_eq!(app.selected, 0);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.selected, 1, "left/right walk the watchlist");
    assert_eq!(app.earnings_scroll, 0, "the new ticker starts at the top");
    render_sized(&mut app, 110, 20);
}

/// The card shows a read only when one is already cached, and never fetches
/// on its own: scrolling a feed of filings has to stay free.
#[test]
fn earnings_card_renders_from_cache_and_never_fetches() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["NVDA".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    let mut row = article("NVDA CORP: Results of Operations", "NVDA", 9, "positive");
    row.original.uid = "earn1".into();
    row.original.source_domain = "sec.gov".into();
    row.original.source = "SEC EDGAR 8-K".into();
    head_fetch(&mut app, "NVDA", vec![row], Some(7));

    // Nothing cached yet: a pointer to the view, and no request.
    let screen = render_sized(&mut app, 110, 32);
    assert!(screen.contains("Earnings read"), "screen:\n{screen}");
    assert!(screen.contains("press 6"), "screen:\n{screen}");
    while let Ok(cmd) = cmds.try_recv() {
        assert!(
            !matches!(cmd, alphai::Cmd::FetchEarnings { .. }),
            "the card fetched a read by itself"
        );
    }

    earnings_fetch(
        &mut app,
        "NVDA",
        earnings_data("NVDA", "2026-11-17", FULL_METRICS),
    );
    let screen = render_sized(&mut app, 110, 32);
    assert!(screen.contains("Q2 FY27"), "screen:\n{screen}");
    assert!(screen.contains("Revenue"), "screen:\n{screen}");
    while let Ok(cmd) = cmds.try_recv() {
        assert!(
            !matches!(cmd, alphai::Cmd::FetchEarnings { .. }),
            "rendering the card fetched"
        );
    }
}

/// In a feed full of coverage of one quarter, the filing itself is the row
/// that carries a read, and the category cell says so.
#[test]
fn news_row_marks_the_filing_itself() {
    let (mut app, _cmds) = empty_app_with_cmds(vec!["NVDA".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    let mut filing = article(
        "Results of Operations and Financial Condition",
        "NVDA",
        9,
        "positive",
    );
    filing.original.source_domain = "sec.gov".into();
    filing.original.source = "SEC EDGAR 8-K".into();
    let coverage = article("Nvidia beats again, says the street", "NVDA", 8, "positive");
    head_fetch(&mut app, "NVDA", vec![filing, coverage], Some(7));
    let screen = render_sized(&mut app, 110, 32);
    assert!(screen.contains("8-K"), "screen:\n{screen}");
    assert!(
        screen.contains("earnings"),
        "coverage keeps the category cell"
    );
}

/// The figure columns follow the data: a filing that states no prior
/// periods prints none, and the name column never starves.
#[test]
fn earnings_metric_columns_follow_the_data() {
    let full: alphai::TickerEarnings = earnings_data("NVDA", "2026-11-17", FULL_METRICS);
    let metrics = &full.latest().unwrap().report().key_metrics;
    let wide = ui::earnings::metric_columns(metrics, 108);
    assert!(
        wide.cells.iter().all(|w| *w > 0),
        "every column has data: {wide:?}"
    );
    let narrow = ui::earnings::metric_columns(metrics, 78);
    assert!(
        narrow.cells.iter().all(|w| *w > 0),
        "80 columns still fits the full table: {narrow:?}"
    );
    assert!(narrow.name >= 18, "the name column starved: {narrow:?}");

    // A 6-K with nothing to compare against: only the value column survives.
    let bare: alphai::TickerEarnings = earnings_data(
        "TSM",
        "",
        r#"{"name": "Second quarter consolidated revenue", "value": "NT$1,270.38 billion",
            "basis": "other", "prior_year": null, "prior_quarter": null,
            "yoy_change": null, "qoq_change": null}"#,
    );
    let cols = ui::earnings::metric_columns(&bare.latest().unwrap().report().key_metrics, 108);
    assert!(cols.cells[0] > 0, "the value column never drops");
    assert!(
        cols.cells[1..].iter().all(|w| *w == 0),
        "empty columns still took space: {cols:?}"
    );

    // A wide terminal must not push the figures half a screen away from the
    // name they belong to: the table is as wide as its content.
    let longest = metrics
        .iter()
        .map(|m| m.name.chars().count())
        .max()
        .unwrap();
    let roomy = ui::earnings::metric_columns(metrics, 250);
    assert_eq!(roomy.name, longest, "the name column stretched: {roomy:?}");

    // Squeezed hard, the prior periods go first and the value stays.
    let cols = ui::earnings::metric_columns(metrics, 40);
    assert_eq!(cols.cells[1], 0, "prior Q outlived the squeeze: {cols:?}");
    assert_eq!(cols.cells[2], 0, "prior Y outlived the squeeze: {cols:?}");
    assert!(cols.cells[0] > 0, "the value column dropped: {cols:?}");
}

/// A fixed moment inside the regular session: the rail's countdown changes
/// its own width, so the ladder is only measurable at a known instant.
/// Thursday 10 September 2026, 17:30 in New York: after the bell, inside
/// the post session.
fn post_market_moment() -> chrono::DateTime<chrono::Utc> {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 10)
        .unwrap()
        .and_hms_opt(21, 30, 0)
        .unwrap()
        .and_utc()
}

fn market_open_moment() -> chrono::DateTime<chrono::Utc> {
    // Thursday 10 September 2026, 09:45 in New York.
    chrono::NaiveDate::from_ymd_opt(2026, 9, 10)
        .unwrap()
        .and_hms_opt(13, 45, 0)
        .unwrap()
        .and_utc()
}

/// The rail exists for the views with no price of their own (News,
/// Insider, Earnings), so it has to survive every view, not just the ones
/// built around a quote.
#[test]
fn quote_rail_shows_the_selected_quote_in_every_view() {
    let mut app = fake_app();
    for id in [
        ui::ViewId::Split,
        ui::ViewId::News,
        ui::ViewId::Table,
        ui::ViewId::Chart,
        ui::ViewId::Insider,
        ui::ViewId::Earnings,
    ] {
        app.view_idx = ui::view_index(id);
        let screen = render_sized(&mut app, 120, 30);
        let rail = screen.lines().nth(1).unwrap_or_default();
        assert!(
            rail.contains("AAPL") && rail.contains("214.50"),
            "{id:?} lost the quote rail:\n{screen}"
        );
    }
}

/// Zones go in priority order and a zone that does not fit is skipped, not
/// truncated: the symbol and price survive to the narrowest terminal, and
/// a cheap zone still lands when an expensive one in front of it cannot.
#[test]
fn quote_rail_drops_zones_as_the_terminal_narrows() {
    let app = fake_app();
    let now = market_open_moment();
    let at = |w: u16| ui::rail::text(&ui::rail::line(&app, w, now));

    let wide = at(120);
    for part in [
        "AAPL",
        "214.50",
        "+14.50",
        "+7.25%",
        "● live",
        "timing varies",
        "199.60",
        "214.80",
        "MSFT",
    ] {
        assert!(wide.contains(part), "120 columns lost {part}: {wide}");
    }

    // The peers go first, then the sparkline, then the range labels (the
    // bare track keeps the position), then the track itself.
    let no_peers = at(105);
    assert!(!no_peers.contains("MSFT"), "{no_peers}");
    assert!(
        no_peers.contains("▇"),
        "sparkline dropped too early: {no_peers}"
    );
    assert!(!at(95).contains("▇"), "{}", at(95));
    let track_only = at(88);
    assert!(!track_only.contains("199.60"), "{track_only}");
    assert!(track_only.contains("├"), "{track_only}");
    assert!(!at(70).contains("├"), "{}", at(70));
    assert!(!at(60).contains("delayed"), "{}", at(60));
    assert!(at(60).contains("closes in"), "{}", at(60));
    // The badge outlives its countdown; the percentage outlives the badge.
    assert!(!at(45).contains("closes in"), "{}", at(45));
    assert!(at(45).contains("● live"), "{}", at(45));
    assert!(!at(25).contains("+14.50"), "{}", at(25));
    assert!(at(25).contains("+7.25%"), "{}", at(25));
    assert!(at(21).contains("+7.25%"), "{}", at(21));
    // Nothing left to drop: the symbol and its price are never cut.
    assert_eq!(at(12).trim(), "AAPL 214.50");
    assert!(at(12).chars().count() <= 12, "{}", at(12));
}

/// Crypto has no opening bell, and a source that is real time says nothing
/// about a delay.
#[test]
fn quote_rail_badges_crypto_and_realtime_sources() {
    let mut app = empty_app(vec!["BTC-USD".into()]);
    app.source_delay = None;
    app.data.insert(
        "BTC-USD".into(),
        TickerData {
            quote: plain_quote("BTC-USD", 64_000.0, Some(63_000.0), Some("USD")),
            candles: vec![Candle {
                feed: Default::default(),
                ts: 1_700_000_000,
                open: 63_500.0,
                high: 64_200.0,
                low: 63_100.0,
                close: 64_000.0,
                volume: None,
            }],
        },
    );
    let rail = ui::rail::text(&ui::rail::line(&app, 120, market_open_moment()));
    assert!(rail.contains("24/7"), "{rail}");
    assert!(!rail.contains("live"), "{rail}");
    assert!(!rail.contains("delayed"), "{rail}");
}

/// The reason the rail carries an extended zone at all: after the bell the
/// headline price is frozen at the close, so the move a filing caused is
/// only visible if the late print gets its own zone.
#[test]
fn quote_rail_shows_the_after_hours_print() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.source_delay = None;
    let mut quote = plain_quote("AAPL", 315.34, Some(316.22), Some("USD"));
    quote.extended = Some(316.90);
    app.data.insert(
        "AAPL".into(),
        TickerData {
            quote,
            candles: vec![Candle {
                feed: Default::default(),
                ts: 1_700_000_000,
                open: 315.0,
                high: 319.15,
                low: 309.9,
                close: 315.34,
                volume: None,
            }],
        },
    );
    let rail = ui::rail::text(&ui::rail::line(&app, 120, post_market_moment()));
    // Measured against the close, not the previous one: the day was down
    // 0.28% while the after-hours print is up 0.49%, and the zone has to
    // report the second number without disturbing the first.
    assert!(rail.contains("AH"), "{rail}");
    assert!(rail.contains("316.90"), "{rail}");
    assert!(rail.contains("+0.49%"), "{rail}");
    assert!(rail.contains("-0.28%"), "{rail}");
}

/// During the regular session the two prices are the same trade, so the
/// zone must not claim a zero move.
#[test]
fn quote_rail_hides_the_extended_zone_during_the_session() {
    let mut app = empty_app(vec!["AAPL".into()]);
    let mut quote = plain_quote("AAPL", 315.34, Some(316.22), Some("USD"));
    quote.extended = Some(315.34);
    app.data.insert(
        "AAPL".into(),
        TickerData {
            quote,
            candles: Vec::new(),
        },
    );
    let rail = ui::rail::text(&ui::rail::line(&app, 120, market_open_moment()));
    assert!(!rail.contains("AH"), "{rail}");
    assert!(!rail.contains("PRE"), "{rail}");
}

/// A ticker still loading, and one the source rejected, both have to read
/// as themselves rather than as a blank rail.
#[test]
fn quote_rail_says_when_a_ticker_has_no_price() {
    let mut app = empty_app(vec!["AAPL".into()]);
    let now = market_open_moment();
    assert!(ui::rail::text(&ui::rail::line(&app, 80, now)).contains('…'));
    app.errors.insert("AAPL".into(), "429 rate limited".into());
    assert!(ui::rail::text(&ui::rail::line(&app, 80, now)).contains("error"));
}

/// Moving the watchlist selection moves the rail: that is what makes ← →
/// between tickers something other than a blind jump.
#[test]
fn quote_rail_follows_the_selection() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    assert!(
        render_sized(&mut app, 100, 20)
            .lines()
            .nth(1)
            .unwrap()
            .contains("AAPL")
    );
    press(&mut app, KeyCode::Down);
    let screen = render_sized(&mut app, 100, 20);
    let rail = screen.lines().nth(1).unwrap();
    assert!(rail.contains("MSFT") && rail.contains("414.50"), "{screen}");
}

/// The row is worth more to a list than to a quote on a terminal this
/// short, and `[ui] quote_rail = false` gives it back on any terminal.
#[test]
fn quote_rail_yields_its_row_when_it_should() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    let short = render_sized(&mut app, 100, ui::rail::MIN_HEIGHT - 1);
    assert!(
        short.lines().nth(1).unwrap().starts_with("╭"),
        "the rail kept its row on a short terminal:\n{short}"
    );
    app.show_rail = false;
    let off = render_sized(&mut app, 100, 20);
    assert!(
        off.lines().nth(1).unwrap().starts_with("╭"),
        "the rail ignored [ui] quote_rail = false:\n{off}"
    );
}

/// Bare mode is two rows of chrome handed to the view: nothing else moves,
/// and the rail stays because it is the only thing naming the ticker once
/// the header is gone.
#[test]
fn bare_mode_gives_the_chrome_rows_to_the_view() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    let full = render_sized(&mut app, 100, 20);
    assert!(full.lines().next().unwrap().contains("alphai-tui"));
    assert!(full.lines().last().unwrap().contains("quit"));

    app.bare = true;
    let bare = render_sized(&mut app, 100, 20);
    let first = bare.lines().next().unwrap();
    let last = bare.lines().last().unwrap();
    assert!(!first.contains("alphai-tui"), "header survived:\n{bare}");
    assert!(first.contains("AAPL"), "the rail went with it:\n{bare}");
    assert!(!last.contains("quit"), "footer survived:\n{bare}");
    // The panel now reaches the last row: both rows went to the view.
    assert!(last.starts_with("╰"), "the view did not grow:\n{bare}");
    let rows = |screen: &str| screen.lines().filter(|l| l.contains("│")).count();
    assert_eq!(rows(&bare), rows(&full) + 2, "{bare}");
}

/// The key toggles it both ways, from either starting state.
#[test]
fn z_toggles_bare_mode() {
    let mut app = fake_app();
    assert!(!app.bare);
    press(&mut app, KeyCode::Char('z'));
    assert!(app.bare);
    assert!(!render_sized(&mut app, 100, 20).contains("alphai-tui"));
    press(&mut app, KeyCode::Char('z'));
    assert!(!app.bare);
    assert!(render_sized(&mut app, 100, 20).contains("alphai-tui"));
}

/// `fake_app` with holdings: 10 AAPL bought at 180 (now 214.50, so up
/// 345.00 and 19.17%) and 3 MSFT bought at 500 (now 414.50, so down
/// 256.50 and 17.10%). Every figure below is distinct, so an assertion
/// cannot pass by matching the wrong column.
fn held_app() -> App {
    let mut app = fake_app();
    app.positions = vec![
        Position {
            symbol: "AAPL".into(),
            qty: 10.0,
            avg_price: 180.0,
        },
        Position {
            symbol: "MSFT".into(),
            qty: 3.0,
            avg_price: 500.0,
        },
    ];
    app
}

fn portfolio_screen(app: &mut App) -> String {
    app.view_idx = ui::view_index(ui::ViewId::Portfolio);
    render_sized(app, 120, 20)
}

#[test]
fn portfolio_view_values_every_holding_and_totals_them() {
    let mut app = held_app();
    let screen = portfolio_screen(&mut app);
    assert!(screen.contains("Portfolio"), "screen:\n{screen}");
    assert!(screen.contains("2,145.00"), "AAPL value, screen:\n{screen}");
    assert!(screen.contains("+345.00"), "AAPL P&L, screen:\n{screen}");
    assert!(
        screen.contains("+19.17%"),
        "AAPL percent, screen:\n{screen}"
    );
    assert!(screen.contains("-256.50"), "MSFT P&L, screen:\n{screen}");
    assert!(
        screen.contains("-17.10%"),
        "MSFT percent, screen:\n{screen}"
    );
    assert!(screen.contains("Total"), "screen:\n{screen}");
    assert!(
        screen.contains("3,388.50"),
        "total value, screen:\n{screen}"
    );
    assert!(screen.contains("+88.50"), "total P&L, screen:\n{screen}");
    assert!(screen.contains("cost 3,300.00"), "basis, screen:\n{screen}");
}

fn premarket_holding() -> App {
    let mut app = empty_app(vec!["CRWV".into()]);
    app.positions = vec![Position {
        symbol: "CRWV".into(),
        qty: 300.0,
        avg_price: 90.26,
    }];
    let mut quote = plain_quote("CRWV", 89.12, Some(94.94), Some("USD"));
    quote.extended = Some(91.28);
    app.data.insert(
        "CRWV".into(),
        TickerData {
            quote,
            candles: vec![],
        },
    );
    app
}

#[test]
fn portfolio_prices_premarket_even_with_extended_candles_disabled() {
    let mut app = premarket_holding();
    assert_eq!(app.sessions, Sessions::Regular);
    let now = "2026-09-11T10:30:00Z".parse().unwrap();
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal
        .draw(|f| ui::portfolio::render_portfolio_at(f, f.area(), &mut app, now))
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("91.28*"), "{screen}");
    assert_eq!(
        screen.matches("27,384.00").count(),
        2,
        "row and total: {screen}"
    );
    assert_eq!(
        screen.matches("+306.00").count(),
        2,
        "row and total: {screen}"
    );
    assert_eq!(
        screen.matches("+648.00").count(),
        2,
        "premarket day and total: {screen}"
    );
    assert!(screen.contains("+1.13%"), "{screen}");
    assert!(screen.contains("* pre/after-hours"), "{screen}");

    app.data.get_mut("CRWV").unwrap().quote.extended = None;
    let screen = portfolio_screen(&mut app);
    assert!(screen.contains("26,736.00"), "{screen}");
    assert!(screen.contains("-342.00"), "{screen}");
    assert!(!screen.contains("pre/after-hours"), "{screen}");
}

#[test]
fn the_rail_and_watchlist_use_the_portfolios_extended_valuation() {
    let mut app = premarket_holding();
    let now = "2026-09-11T10:30:00Z".parse().unwrap();
    let rail = ui::rail::text(&ui::rail::line(&app, 180, now));
    assert!(rail.contains("PRE 91.28"), "{rail}");
    assert!(rail.contains("×300 +306.00 +1.13%"), "{rail}");
    app.view_idx = ui::view_index(ui::ViewId::Table);
    let screen = render_sized(&mut app, 180, 20);
    let body = panels(&screen);
    assert!(body.contains("89.12"), "regular quote: {body}");
    assert!(body.contains("27,384.00"), "holding value: {body}");
    assert!(body.contains("+306.00"), "holding P&L: {body}");
}

/// The view is in the tab cycle whether or not anything is held, so the
/// empty screen has to teach both ways of filling it.
#[test]
fn portfolio_view_says_how_to_start_when_nothing_is_held() {
    let mut app = fake_app();
    let screen = portfolio_screen(&mut app);
    assert!(screen.contains("Nothing held yet"), "screen:\n{screen}");
    assert!(screen.contains("[[positions]]"), "screen:\n{screen}");
    assert!(screen.contains("avg_price"), "screen:\n{screen}");
}

/// A holding with no price yet is not worth zero. It says so, and the
/// totals count how many rows they actually cover.
#[test]
fn portfolio_marks_the_rows_that_have_no_price_yet() {
    let mut app = held_app();
    app.positions.push(Position {
        symbol: "DDOG".into(),
        qty: 4.0,
        avg_price: 100.0,
    });
    let screen = portfolio_screen(&mut app);
    assert!(screen.contains("DDOG"), "screen:\n{screen}");
    assert!(screen.contains("2/3 priced"), "screen:\n{screen}");
    // The basis of the unpriced row stays out of the total as well, or the
    // percentage would read as a 100% loss on money that is fine.
    assert!(screen.contains("cost 3,300.00"), "screen:\n{screen}");
}

/// There is no currency conversion in this app, so a total that spans two
/// of them has to say what it is instead of looking exact.
#[test]
fn portfolio_admits_when_the_total_mixes_currencies() {
    let mut app = held_app();
    let screen = portfolio_screen(&mut app);
    assert!(!screen.contains("mixed currencies"), "screen:\n{screen}");
    app.data.get_mut("MSFT").unwrap().quote.currency = Some("EUR".into());
    let screen = portfolio_screen(&mut app);
    assert!(screen.contains("mixed currencies"), "screen:\n{screen}");
}

/// Narrow terminals lose whole columns, least useful first, and never a
/// half-printed number.
#[test]
fn portfolio_drops_columns_before_it_squeezes_them() {
    let mut app = held_app();
    app.view_idx = ui::view_index(ui::ViewId::Portfolio);
    let wide = render_sized(&mut app, 120, 20);
    assert!(wide.contains("Wt%"), "screen:\n{wide}");
    assert!(wide.contains("Avg"), "screen:\n{wide}");

    let narrow = render_sized(&mut app, 46, 20);
    assert!(narrow.contains("P&L%"), "the percentage stays:\n{narrow}");
    assert!(narrow.contains("+19.17%"), "screen:\n{narrow}");
    assert!(!narrow.contains("Wt%"), "weight should be gone:\n{narrow}");
    assert!(!narrow.contains("Avg"), "average should be gone:\n{narrow}");
}

/// The rail sits above every view, so the cursor and the rail must quote
/// the same ticker; moving the portfolio cursor therefore moves the
/// watchlist cursor with it.
#[test]
fn the_portfolio_cursor_pulls_the_watchlist_cursor_along() {
    let mut app = held_app();
    app.view_idx = ui::view_index(ui::ViewId::Portfolio);
    assert_eq!(app.portfolio_selected, 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.portfolio_selected, 1);
    assert_eq!(app.selected_symbol(), "MSFT");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.portfolio_selected, 0);
    assert_eq!(app.selected_symbol(), "AAPL");
}

/// Holdings off the watchlist are polled too: a row that cannot be priced
/// cannot be valued either.
#[test]
fn holdings_off_the_watchlist_are_still_polled() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.positions = vec![Position {
        symbol: "DDOG".into(),
        qty: 4.0,
        avg_price: 100.0,
    }];
    assert_eq!(
        app.polled_symbols(),
        vec!["AAPL".to_string(), "DDOG".into()]
    );
}

/// The watchlist table stays as it was for anyone who holds nothing, and
/// grows two columns for anyone who does. A watched-only row leaves them
/// blank rather than printing a dash, like the extended column does.
#[test]
fn the_table_prices_holdings_only_when_there_are_any() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    let screen = render_sized(&mut app, 120, 20);
    assert!(!screen.contains("Value"), "screen:\n{screen}");

    app.positions = vec![Position {
        symbol: "AAPL".into(),
        qty: 10.0,
        avg_price: 180.0,
    }];
    let screen = render_sized(&mut app, 120, 20);
    assert!(screen.contains("Value"), "screen:\n{screen}");
    assert!(screen.contains("2,145.00"), "screen:\n{screen}");
    assert!(screen.contains("+345.00"), "screen:\n{screen}");
    let msft = screen
        .lines()
        .find(|l| l.contains("MSFT"))
        .expect("a MSFT row");
    assert!(!msft.contains('—'), "watched-only row dashed: {msft}");
}

/// The holder's own number, in every view, ahead of the session badge.
#[test]
fn the_rail_carries_the_position_of_the_selected_ticker() {
    let mut app = held_app();
    app.source_delay = None;
    let rail = ui::rail::text(&ui::rail::line(&app, 130, market_open_moment()));
    assert!(rail.contains("×10"), "{rail}");
    assert!(rail.contains("+345.00"), "{rail}");
    assert!(rail.contains("+19.17%"), "{rail}");

    app.positions.clear();
    let rail = ui::rail::text(&ui::rail::line(&app, 130, market_open_moment()));
    assert!(!rail.contains("+345.00"), "{rail}");
}

/// A position typed into the prompt is user data, not a display
/// preference: it reaches the config file on Enter rather than waiting for
/// Save in the settings screen.
#[test]
fn the_position_prompt_writes_the_config_straight_away() {
    let dir = std::env::temp_dir().join(format!("alphai-tui-positions-{}", std::process::id()));
    let path = dir.join("config.toml");
    let _ = std::fs::remove_dir_all(&dir);
    let mut app = fake_app();
    app.config_path = Some(path.clone());

    press(&mut app, KeyCode::Char('p'));
    assert!(app.prompt.open, "the prompt did not open");
    for c in "10 180".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert!(!app.prompt.open, "error: {:?}", app.prompt.error);
    assert_eq!(app.positions.len(), 1);
    assert_eq!(app.positions[0].symbol, "AAPL");
    assert_eq!(app.positions[0].qty, 10.0);

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("[[positions]]"), "file:\n{written}");
    assert!(written.contains("AAPL"), "file:\n{written}");
    // The live watchlist is the settings screen's business, not this
    // keypress's: the file keeps the empty list it was loaded with.
    assert!(written.contains("watchlist = []"), "file:\n{written}");
    assert!(!written.contains("MSFT"), "file:\n{written}");

    // Reopening prefills what is held, and an emptied line clears it.
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.prompt.input, "10 180");
    for _ in 0..app.prompt.input.len() {
        press(&mut app, KeyCode::Backspace);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.positions.is_empty(), "the holding was not cleared");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("[[positions]]"), "file:\n{written}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The prompt swallows every key, or a line with a bound letter in it
/// could not be typed; the numbers a position needs are no different.
#[test]
fn the_position_prompt_swallows_the_keys_it_needs() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('p'));
    for c in "nvda -2.5 1,204.50".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.prompt.input, "nvda -2.5 1,204.50");
    press(&mut app, KeyCode::Enter);
    assert!(!app.prompt.open, "error: {:?}", app.prompt.error);
    let held = app.position("NVDA").expect("a short position");
    assert_eq!(held.qty, -2.5);
    assert_eq!(held.avg_price, 1204.5);
}

/// A line the parser cannot read keeps the prompt open with the reason,
/// the way a duplicate ticker does.
#[test]
fn the_position_prompt_keeps_a_bad_line_on_screen() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('p'));
    for c in "twelve 180".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.prompt.open, "the prompt closed on a bad line");
    assert!(app.prompt.error.is_some());
    assert!(app.positions.is_empty());
}

/// A modified key is a gesture, not text: ctrl-h used to arrive as a plain
/// "h" and land in the middle of a typed quantity. Ctrl-U clears the line,
/// which the prefilled position prompt needs often.
#[test]
fn the_prompt_ignores_modified_keys_and_clears_on_ctrl_u() {
    use crossterm::event::KeyModifiers;

    let mut app = held_app();
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.prompt.input, "10 180");
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(app.prompt.input, "10 180", "a ctrl key was typed in");

    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.prompt.input, "");
    for c in "4 150".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let held = app.position("AAPL").expect("the holding");
    assert_eq!(held.qty, 4.0);
    assert_eq!(held.avg_price, 150.0);
}
