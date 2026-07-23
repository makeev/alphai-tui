use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::Notify;

use crate::alphai::{self, Article, FeedPayload, InsiderSummary, SentimentSummary, TRENDING_KEY};
use crate::app::{
    App, AppInit, ChartStyle, FeedBundle, NewsLayout, NewsScope, PRICE_FLASH, SettingsRow,
    settings_rows,
};
use crate::config::{ChartDefaults, Config, UiDefaults};
use crate::domain::{Candle, Interval, Quote, Range, TickerData};
use crate::poller::SourceEvent;
use crate::source::make_source;
use crate::theme::Theme;
use crate::ui;

fn empty_app(symbols: Vec<String>) -> App {
    let (app, _rx) = empty_app_with_cmds(symbols);
    app
}

/// Like `empty_app`, but keeps the AlphaAI command receiver alive so tests
/// can assert which fetches the app requested.
fn empty_app_with_cmds(
    symbols: Vec<String>,
) -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (alphai_tx, alphai_rx) = tokio::sync::mpsc::unbounded_channel();
    let source = make_source("yahoo", &Config::default()).unwrap();
    let app = App::new(AppInit {
        symbols,
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5))),
        every: Arc::new(RwLock::new(std::time::Duration::from_secs(15))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        config_path: None,
        theme: Theme::default(),
        chart: ChartDefaults::default(),
        ui: UiDefaults::default(),
        keymap: crate::keymap::Keymap::default(),
        alphai_enabled: true,
        first_run: false,
    });
    (app, alphai_rx)
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
        screen.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
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
    assert!(screen.matches("214.50").count() >= 2, "price tag missing:\n{screen}");

    app.chart.right_margin_pct = 0;
    let screen = render(&mut app);
    let flush = max_body_x(&screen);
    assert_eq!(screen.matches("214.50").count(), 1, "margin off, tag still drawn:\n{screen}");
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
    let data = |price: f64| SourceEvent::Data {
        symbol: "AAPL".into(),
        data: TickerData {
            quote: Quote {
                symbol: "AAPL".into(),
                price,
                prev_close: Some(price),
                currency: None,
            },
            candles: vec![],
        },
    };
    app.apply(data(100.0));
    assert_eq!(app.price_flash_dir("AAPL"), None, "first data must not flash");
    app.apply(data(101.0));
    assert_eq!(app.price_flash_dir("AAPL"), Some(true));
    app.apply(data(100.5));
    assert_eq!(app.price_flash_dir("AAPL"), Some(false));
    app.price_flash.clear();
    app.apply(data(100.5));
    assert_eq!(app.price_flash_dir("AAPL"), None, "unchanged price must not flash");
}

/// The live quote is folded into the in-progress last candle on apply, so
/// the candle redraws with every price tick instead of waiting for the
/// source's bar series to catch up.
#[test]
fn live_quote_updates_the_last_candle() {
    let mut app = empty_app(vec!["AAPL".into()]);
    let data = |price: f64| SourceEvent::Data {
        symbol: "AAPL".into(),
        data: TickerData {
            quote: Quote {
                symbol: "AAPL".into(),
                price,
                prev_close: Some(100.0),
                currency: None,
            },
            candles: vec![
                Candle {
                    ts: 0,
                    open: 99.0,
                    high: 100.0,
                    low: 98.0,
                    close: 99.5,
                    volume: None,
                },
                Candle {
                    ts: 300,
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
    assert!(reversed_cells(&mut app).is_empty(), "inverted cells without a flash");

    app.price_flash.insert("AAPL".into(), (Instant::now(), true));
    let cells = reversed_cells(&mut app);
    assert!(!cells.is_empty(), "flash did not invert the price");
    assert!(cells.iter().all(|&fg| fg == app.theme.up), "up tick must use the up color");

    app.price_flash.insert("AAPL".into(), (Instant::now() - PRICE_FLASH, true));
    assert!(reversed_cells(&mut app).is_empty(), "expired flash still inverted");
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
    assert!(!plot.contains('·'), "old per-column SMA dots still drawn:\n{screen}");
    press(&mut app, KeyCode::Char('m'));
    let screen = render(&mut app);
    assert!(!braille(&screen), "SMA hidden but braille remains:\n{screen}");
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

#[test]
fn range_keys_cycle_presets_and_update_header() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
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
    assert!(app.feeds.contains_key("AAPL"), "range switch dropped the news bundle");
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
    assert!(screen.contains("Apple beats expectations"), "screen:\n{screen}");
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
    assert!(!screen.contains("News ·"), "news strip should be hidden:\n{screen}");
}

/// Every view registers a distinct ViewId (a duplicate would make
/// `view_index` land on the wrong tab) and carries footer hints.
#[test]
fn view_ids_are_unique_and_indexable() {
    for (i, view) in ui::VIEWS.iter().enumerate() {
        assert_eq!(ui::view_index(view.id()), i, "duplicate ViewId {:?}", view.id());
        assert!(!view.hints().is_empty(), "{:?}: empty footer hints", view.id());
    }
}

/// The footer renders the keys actually bound in the keymap, in the
/// traditional shapes ("↑↓ select", "c/m/i chart").
#[test]
fn footer_shows_bound_keys() {
    let mut app = fake_app();
    let screen = render(&mut app);
    assert!(screen.contains("q quit"), "screen:\n{screen}");
    assert!(screen.contains("tab/1-9 view"), "screen:\n{screen}");
    assert!(screen.contains("↑↓ select"), "screen:\n{screen}");
    assert!(screen.contains("c/m/i chart"), "screen:\n{screen}");
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
        [("open", vec!["z"]), ("card", vec!["b"])],
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
    assert!(screen.contains("z open"), "screen:\n{screen}");
    assert!(!screen.contains("⏎ open"), "screen:\n{screen}");
    // The new key drives the action, the old one is gone.
    press(&mut app, KeyCode::Char('v'));
    assert!(!app.article_overlay.open, "the replaced default still fired");
    press(&mut app, KeyCode::Char('b'));
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
    let screen = render_sized(&mut app, 90, 45);
    for (_, name) in crate::keymap::ACTIONS.iter() {
        assert!(screen.contains(name), "action {name} missing:\n{screen}");
    }
    assert!(screen.contains("ctrl-c"), "screen:\n{screen}");
    assert!(screen.contains("[keybindings]"), "screen:\n{screen}");
    // Esc closes the overlay without quitting the app.
    assert!(!app.handle_key(KeyEvent::from(KeyCode::Esc)));
    assert!(!app.help.open);
    // A remap shows up in the table: the overlay reads the live keymap.
    app.keymap =
        crate::keymap::Keymap::from_config([("refresh", vec!["f5"])], &mut Vec::new());
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
    assert!(screen.contains("Apple beats expectations"), "screen:\n{screen}");
    assert!(screen.contains("12 bullish"), "screen:\n{screen}");
    assert!(screen.contains("▲"), "sentiment glyph missing:\n{screen}");
    // detail pane shows the selected article's summary and the AI meta calls
    assert!(
        screen.contains("Summary of Apple beats expectations"),
        "screen:\n{screen}"
    );
    assert!(screen.contains("nov 7"), "novelty missing from meta:\n{screen}");
    assert!(screen.contains("positive/high"), "sentiment/confidence missing:\n{screen}");
    assert!(screen.contains("act high"), "actionability missing:\n{screen}");
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
        .insert("AAPL".into(), "invalid AlphaAI API key".into());
    let screen = render(&mut app);
    assert!(screen.contains("invalid AlphaAI API key"), "screen:\n{screen}");
    assert!(screen.contains("press r to retry"), "screen:\n{screen}");
}

#[test]
fn insider_view_shows_summary_and_filings() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Insider);
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
    app.feeds.insert(
        alphai::insider_key("AAPL"),
        FeedBundle::new(
            vec![filing("Apple insider sold $12.5M of stock", "direct")],
            Some(FeedPayload::Insider(summary)),
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("Insider · AAPL"), "screen:\n{screen}");
    assert!(screen.contains("14 filings"), "screen:\n{screen}");
    assert!(screen.contains("$224.6M"), "screen:\n{screen}");
    assert!(screen.contains("85% under 10b5-1"), "screen:\n{screen}");
    assert!(screen.contains("COOK TIMOTHY"), "screen:\n{screen}");
    assert!(screen.contains("×3"), "transaction count missing:\n{screen}");
    // No AI enrichment and no structured block on this legacy filing: the
    // sell glyph comes from the title fallback, the ownership marker follows,
    // and the plan/value columns stay blank.
    assert!(screen.contains("▼  D"), "screen:\n{screen}");
    assert!(screen.contains("Apple insider sold"), "screen:\n{screen}");
    assert!(screen.contains("score 4+"), "size filter missing from the title:\n{screen}");
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
                with_block("Y sold $1.0M of stock to the issuer", "other", Some("1000000.00"), false),
            ],
            None,
            None,
        ),
    );
    let screen = render(&mut app);
    assert!(screen.contains("$4.7M"), "value column missing:\n{screen}");
    assert!(screen.contains("▼  D p"), "sell glyph + plan flag missing:\n{screen}");
    assert!(screen.contains("·  D"), "structured \"other\" not neutral:\n{screen}");
    // The detail meta carries the structured trade.
    assert!(screen.contains("SELL $4.7M"), "meta trade missing:\n{screen}");
    assert!(screen.contains("10b5-1 plan"), "meta plan flag missing:\n{screen}");
    // The fullscreen card shows the full structured trade.
    press(&mut app, KeyCode::Char('v'));
    let card = render(&mut app);
    assert!(card.contains("SELL 25,000 sh @ $187.32 = $4.7M (code S)"), "card:\n{card}");
    assert!(card.contains("STEVENS MARK A (Director)"), "card:\n{card}");
    assert!(card.contains("2026-07-09"), "card:\n{card}");
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
    assert_eq!(app.news_min_score, news_before, "+ leaked into the news filter");
    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchInsider { symbol, cursor, min_relevance }) => {
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
    assert!(screen.contains("AAPL,MSFT"), "ticker cell missing:\n{screen}");
    assert!(screen.contains("×7"), "outlet count missing:\n{screen}");
    assert!(screen.contains("reprints collapsed"), "head line missing:\n{screen}");
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
    assert!(screen.contains("top 10 of the last 48h"), "screen:\n{screen}");
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
    assert!(screen.contains("price: +2-4% near term"), "impact missing:\n{screen}");
    assert!(screen.contains("Trading value"), "screen:\n{screen}");
    assert!(screen.contains("contrarian: Priced in already."), "screen:\n{screen}");
    assert!(screen.contains("entities: Acme (company)"), "screen:\n{screen}");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.article_overlay.scroll, 1);
    render(&mut app); // must not panic; the render pass clamps the scroll
    press(&mut app, KeyCode::Esc);
    assert!(!app.article_overlay.open);
    // The card pane still shows the article; only the modal frame must go.
    assert!(!render(&mut app).contains(" Article "), "overlay did not close");
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
    assert!(screen.contains("Officer bought 10,000 shares"), "screen:\n{screen}");
    assert!(screen.contains("Summary of Officer bought"), "screen:\n{screen}");
}

#[test]
fn overlay_noop_when_no_articles() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    press(&mut app, KeyCode::Char('v'));
    assert!(!app.article_overlay.open, "overlay opened with nothing to show");
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
    assert!(screen.contains("Trading value"), "card content missing:\n{screen}");
    assert!(screen.contains("contrarian: Priced in already."), "screen:\n{screen}");
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
    assert!(screen.contains(" card ·"), "card pane missing in stacked:\n{screen}");
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
    assert!(render(&mut app).contains("score 7+"), "filter missing from the title");
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(app.news_min_score, 8);
    assert_eq!(app.news_selected, 0, "selection not reset on a filter change");
    assert!(render(&mut app).contains("score 8+"));

    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchNews { symbol, cursor, min_relevance }) => {
            assert_eq!(symbol.as_deref(), Some("AAPL"));
            assert_eq!(cursor, None, "filter change must restart from page 1");
            assert_eq!(min_relevance, Some(8));
        }
        other => panic!("expected a filtered head fetch, got {:?}", other.is_ok()),
    }
    // The second ensure pass is absorbed by the inflight guard.
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "duplicate refetch for one filter change");
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
    assert!(wide.contains("pgup/pgdn scroll"), "card pane missing at 100 cols:\n{wide}");
    let narrow = render_sized(&mut app, 80, 30);
    assert!(!narrow.contains("pgup/pgdn scroll"), "card pane still there at 80 cols:\n{narrow}");
    assert!(narrow.contains("Apple beats expectations"), "list missing:\n{narrow}");
    press(&mut app, KeyCode::Char('v'));
    let overlay = render_sized(&mut app, 80, 30);
    assert!(overlay.contains(" Article "), "v overlay unavailable when narrow:\n{overlay}");
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
        Ok(alphai::Cmd::FetchNews { symbol, cursor, min_relevance }) => {
            assert_eq!(symbol.as_deref(), Some("AAPL"));
            assert_eq!(cursor.as_deref(), Some("cur1"));
            assert_eq!(min_relevance, Some(app.news_min_score));
        }
        other => panic!("expected a page fetch, got {:?}", other.is_ok()),
    }
    // While the page is in flight the hint reports progress and a second j
    // must not spend another request.
    assert!(render(&mut app).contains("loading…"), "no in-flight feedback");
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
        Ok(alphai::Cmd::FetchInsider { symbol, cursor, min_relevance }) => {
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

/// A head (non-append) news fetch for `key`, as the AlphaAI task delivers it.
fn head_fetch(app: &mut App, key: &str, articles: Vec<Article>, min_relevance: Option<u8>) {
    app.apply_alphai(alphai::Event::Feed {
        key: key.into(),
        articles,
        side: None,
        next_cursor: None,
        append: false,
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
        articles: vec![uid_article("aaa", "First reprint"), uid_article("bbb", "Second")],
        side: None,
        next_cursor: Some("c2".into()),
        append: true,
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
    assert!(app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]), "new row unmarked");
    assert!(!app.is_unseen("AAPL", &app.feeds["AAPL"].articles[1]), "old row marked");

    // The marker renders while the cursor is parked elsewhere...
    app.news_selected = 1;
    let screen = render(&mut app);
    assert!(screen.contains("● Breaking story"), "marker missing:\n{screen}");
    // ...and hovering the row retires it.
    app.news_selected = 0;
    let screen = render(&mut app);
    assert!(!screen.contains("●"), "marker survived the hover:\n{screen}");

    // Manual r drops the bundle but not the seen set: the refetch after it
    // still marks only what is genuinely new.
    press(&mut app, KeyCode::Char('r'));
    head_fetch(
        &mut app,
        "AAPL",
        vec![uid_article("ddd", "Newer still"), uid_article("ccc", "Breaking story")],
        Some(7),
    );
    assert!(app.is_unseen("AAPL", &app.feeds["AAPL"].articles[0]), "post-r new row unmarked");
    assert!(!app.is_unseen("AAPL", &app.feeds["AAPL"].articles[1]), "post-r hovered row marked");
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
        append: true,
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
        vec![uid_article("aaa", "First"), uid_article("low", "Low score story")],
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
        vec![uid_article("ccc", "Breaking story"), uid_article("aaa", "First")],
        Some(7),
    );
    app.view_idx = ui::view_index(ui::ViewId::Split);
    let screen = render(&mut app);
    assert!(screen.contains("● Breaking story"), "marker missing:\n{screen}");
    let screen = render(&mut app);
    assert!(screen.contains("● Breaking story"), "the strip retired a marker:\n{screen}");
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
    assert!(screen.contains("Apple beats expectations"), "list dropped:\n{screen}");
    assert!(screen.contains("archive limit"), "upsell missing:\n{screen}");
    press(&mut app, KeyCode::Char('j'));
    assert!(cmds.try_recv().is_err(), "gated feed still paged");
}

#[test]
fn ttl_refetch_waits_for_top_row() {
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
    app.feeds.get_mut("AAPL").unwrap().fetched =
        Instant::now() - std::time::Duration::from_secs(600);
    // Reader is mid-list: a refetch would drop loaded pages, so hold off.
    app.news_selected = 1;
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "refetched under the reader");
    app.news_selected = 0;
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchNews { cursor: None, .. })),
        "no refetch at the top row"
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
        symbols: vec!["AAPL".into()],
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5))),
        every: Arc::new(RwLock::new(std::time::Duration::from_secs(15))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        config_path: None,
        theme: Theme::default(),
        chart: ChartDefaults {
            style: ChartStyle::Line,
            sma: false,
            rsi: false,
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
    });
    assert_eq!(app.view_idx, ui::view_index(ui::ViewId::News));
    assert_eq!(app.chart_style, ChartStyle::Line);
    assert!(!app.show_sma && !app.show_rsi);
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

/// Save must start from the loaded config and replace only the settings
/// screen's own fields, so file-only sections like [theme] survive it.
#[test]
fn settings_save_merge_preserves_file_only_sections() {
    let mut app = fake_app();
    app.config.theme = Some(std::collections::BTreeMap::from([(
        "accent".to_string(),
        "magenta".to_string(),
    )]));
    app.open_settings();
    app.settings.key_values.insert("alphai", "ak_live_new".into());
    let merged = app.settings_merged_config();
    assert_eq!(
        merged.theme.as_ref().and_then(|t| t.get("accent")).map(String::as_str),
        Some("magenta")
    );
    assert_eq!(merged.keys.get("alphai").map(String::as_str), Some("ak_live_new"));
    assert_eq!(merged.watchlist, vec!["AAPL".to_string(), "MSFT".to_string()]);
}

/// The single-copy guards must hold for every feed kind: the insider TTL
/// refetch also waits for the reader to return to the top row.
#[test]
fn insider_ttl_refetch_also_waits_for_top_row() {
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
    app.feeds.get_mut(&key).unwrap().fetched =
        Instant::now() - std::time::Duration::from_secs(600);
    app.news_selected = 1;
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "insider refetched under the reader");
    app.news_selected = 0;
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchInsider { cursor: None, .. })),
        "no insider refetch at the top row"
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
    app.config.keys.insert("alphai".into(), "ak_live_abcdefgh1234".into());
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
    app.config.keys.insert("alphai".into(), "ak_live_abcdefgh1234".into());
    app.config.keys.insert("alpaca_secret".into(), "alpaca-secret-abcd9876".into());
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
        symbols: vec!["AAPL".into()],
        source: Arc::new(RwLock::new(source)),
        source_name: "yahoo",
        range: Range::D1,
        interval: Interval::M5,
        params: Arc::new(RwLock::new((Range::D1, Interval::M5))),
        every: Arc::new(RwLock::new(std::time::Duration::from_secs(15))),
        rx,
        refresh: Arc::new(Notify::new()),
        alphai_tx,
        config: Config::default(),
        config_path: None,
        theme: Theme::default(),
        chart: ChartDefaults::default(),
        ui: UiDefaults::default(),
        keymap: crate::keymap::Keymap::default(),
        alphai_enabled: false,
        first_run: true,
    });
    assert!(app.settings.open);
    let screen = render(&mut app);
    assert!(screen.contains("Welcome to alphai-tui"), "screen:\n{screen}");
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
        assert!(app.settings.open, "at {bad:?}: settings closed on bad input");
        assert!(
            app.settings.message.as_deref().is_some_and(|m| m.contains("poll interval")),
            "at {bad:?}: no validation message"
        );
        assert_eq!(app.config.every, None, "at {bad:?}: bad value persisted");
        app.open_settings();
        assert_eq!(app.settings.every_input, "15", "at {bad:?}: live interval changed");
    }
}
