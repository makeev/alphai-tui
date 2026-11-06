use super::*;
use crate::alerts::quote_sample;
use crate::domain::PriceFeed;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Duration;

fn app() -> App {
    let mut app = fake_app();
    app.frozen_now = Some(market_open_moment());
    app.animations = false;
    for data in app.data.values_mut() {
        data.quote.timing.regular = Some(market_open_moment().timestamp() - 60);
        data.quote.timing.regular_feed = PriceFeed::Yahoo;
    }
    app
}

fn arm(app: &mut App, text: &str) {
    press(app, KeyCode::Char('A'));
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
    press(app, KeyCode::Enter);
    assert!(!app.prompt.open, "{:?}", app.prompt.error);
}

fn data(app: &App, symbol: &str, price: f64, seconds: i64) -> TickerData {
    let mut data = app.data.get(symbol).cloned().unwrap_or_else(|| TickerData {
        quote: plain_quote(symbol, price, None, Some("USD")),
        candles: Vec::new(),
    });
    data.quote.price = price;
    data.quote.timing.regular = Some(app.now().timestamp() - 60 + seconds);
    data.quote.timing.regular_feed = PriceFeed::Yahoo;
    data
}

fn update(app: &mut App, symbol: &str, price: f64, seconds: i64) {
    let data = data(app, symbol, price, seconds);
    app.apply(SourceEvent::Data {
        params: None,
        source: current_source(app),
        symbol: symbol.into(),
        data,
    });
}

#[test]
fn prompt_validation_cancel_and_rearm_are_explicit_and_do_not_save_config() {
    let mut app = app();
    let before = toml::to_string(&app.config).unwrap();
    arm(&mut app, ">00215.000");
    assert_eq!(app.alerts.get("AAPL").unwrap().level.label, ">= 215.000");
    update(&mut app, "AAPL", 215., 1);
    assert!(app.alerts.get("AAPL").unwrap().fired.is_some());
    press(&mut app, KeyCode::Char('A'));
    let screen = render_sized(&mut app, 110, 32);
    assert!(screen.contains("Fired; Enter arms again"), "{screen}");
    assert!(screen.contains("Detected:"));
    assert!(screen.contains("This session only"));
    press(&mut app, KeyCode::Esc);
    assert!(app.alerts.get("AAPL").unwrap().fired.is_some());
    press(&mut app, KeyCode::Char('A'));
    app.prompt.input = "215".into();
    press(&mut app, KeyCode::Enter);
    assert!(app.prompt.open);
    assert!(app.prompt.error.as_ref().unwrap().contains(">= 150"));
    assert!(app.alerts.get("AAPL").unwrap().fired.is_some());
    press(&mut app, KeyCode::Esc);
    arm(&mut app, ">215");
    assert!(app.alerts.notification().is_none());
    update(&mut app, "AAPL", 216., 2);
    assert!(app.alerts.get("AAPL").unwrap().fired.is_none());
    update(&mut app, "AAPL", 214., 3);
    update(&mut app, "AAPL", 215., 4);
    assert!(app.alerts.get("AAPL").unwrap().fired.is_some());
    arm(&mut app, "");
    assert!(app.alerts.get("AAPL").is_none());
    assert!(app.alerts.notification().is_none());
    assert_eq!(toml::to_string(&app.config).unwrap(), before);
}

#[test]
fn cached_prices_wait_for_a_live_baseline_but_old_request_windows_still_observe_quotes() {
    let mut app = app();
    app.from_cache.insert("AAPL".into(), app.now().timestamp());
    arm(&mut app, ">215");
    assert!(app.alerts.get("AAPL").unwrap().previous.is_none());
    update(&mut app, "AAPL", 216., 1);
    assert!(app.alerts.get("AAPL").unwrap().fired.is_none());
    update(&mut app, "AAPL", 214., 2);
    let incoming = data(&app, "AAPL", 215., 3);
    app.apply(SourceEvent::Data {
        params: Some((Range::D5, Interval::M15, Sessions::Extended)),
        source: current_source(&app),
        symbol: "AAPL".into(),
        data: incoming,
    });
    assert_eq!(
        app.data["AAPL"].quote.price, 214.,
        "old candles replaced the active window"
    );
    assert!(app.alerts.get("AAPL").unwrap().fired.is_some());
}

#[test]
fn fallback_preserves_crossing_and_ignores_the_abandoned_source() {
    let mut app = app();
    app.config.keys.insert("finnhub".into(), "test-key".into());
    update(&mut app, "AAPL", 214., 0);
    arm(&mut app, ">215");
    let old = current_source(&app);
    for symbol in ["AAPL", "MSFT"] {
        app.apply(SourceEvent::Error {
            params: None,
            source: old.clone(),
            symbol: symbol.into(),
            error: "unavailable".into(),
        });
    }
    app.source_trouble_since = Some(Instant::now() - Duration::from_secs(120));
    app.fallback_if_stuck();
    assert_eq!(app.source_name, "finnhub");
    assert!(
        app.alert_sample("AAPL").is_none(),
        "retained source must not arm a new rule"
    );
    let mut incoming = data(&app, "AAPL", 216., 1);
    app.apply(SourceEvent::Data {
        params: None,
        source: old,
        symbol: "AAPL".into(),
        data: incoming.clone(),
    });
    assert!(app.alerts.get("AAPL").unwrap().fired.is_none());
    incoming.quote.currency = None;
    incoming.quote.timing.regular_feed = PriceFeed::Finnhub;
    app.apply(SourceEvent::Data {
        params: None,
        source: current_source(&app),
        symbol: "AAPL".into(),
        data: incoming,
    });
    assert!(app.alerts.get("AAPL").unwrap().fired.is_some());
}

#[test]
fn after_hours_gap_across_feeds_fires_with_sessions_hidden() {
    let mut app = app();
    app.frozen_now = Some(post_market_moment());
    let q = &mut app.data.get_mut("AAPL").unwrap().quote;
    q.price = 214.;
    q.timing.regular = Some(post_market_moment().timestamp() - 90 * 60);
    q.timing.regular_feed = PriceFeed::Iex;
    arm(&mut app, ">215");
    let mut incoming = app.data["AAPL"].clone();
    incoming.quote.timing.regular = None;
    incoming.quote.extended = Some(216.);
    incoming.quote.timing.extended = Some(post_market_moment().timestamp() - 1);
    incoming.quote.timing.extended_feed = PriceFeed::DelayedSip;
    assert_eq!(app.sessions, Sessions::Regular);
    app.apply(SourceEvent::Data {
        params: None,
        source: current_source(&app),
        symbol: "AAPL".into(),
        data: incoming,
    });
    let fired = app.alerts.get("AAPL").unwrap().fired.as_ref().unwrap();
    assert_eq!(fired.sample.price, 216.);
    app.chart.timezone = crate::config::ChartTimezone::Utc;
    let screen = render_sized(&mut app, 160, 30);
    assert!(
        screen
            .lines()
            .last()
            .unwrap()
            .contains("AH 10 Sep 21:29:59 UTC"),
        "{screen}"
    );
}

#[test]
fn alerts_remap_in_every_view_and_portfolio_uses_its_focused_holding() {
    for view in ui::VIEWS {
        let mut app = app();
        app.view_idx = ui::view_index(view.id());
        app.positions = vec![Position {
            symbol: "MSFT".into(),
            qty: 1.,
            avg_price: 400.,
        }];
        app.keymap =
            crate::keymap::Keymap::from_config([("price_alert", vec!["ctrl-a"])], &mut vec![]);
        press(&mut app, KeyCode::Char('A'));
        assert!(!app.prompt.open);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(app.prompt.open);
        assert_eq!(
            app.prompt.target,
            if view.id() == ui::ViewId::Portfolio {
                "MSFT"
            } else {
                "AAPL"
            }
        );
        let target = app.prompt.target.clone();
        // Input owns digits and letters; no view switch or watchlist mutation.
        for c in ">500".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.view_id(), view.id());
        press(&mut app, KeyCode::Enter);
        assert!(app.alerts.get(&target).is_some());
    }
}

#[test]
fn removing_the_last_polled_reference_prunes_the_rule_and_pending_event() {
    let mut app = app();
    press(&mut app, KeyCode::Char('p'));
    app.prompt.input = "1 200".into();
    press(&mut app, KeyCode::Enter);
    arm(&mut app, ">215");
    update(&mut app, "AAPL", 215., 1);
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.symbols, vec!["MSFT"]);
    assert!(app.alerts.get("AAPL").is_some());
    press(&mut app, KeyCode::Char('8'));
    press(&mut app, KeyCode::Char('p'));
    app.prompt.input.clear();
    press(&mut app, KeyCode::Enter);
    assert!(app.alerts.get("AAPL").is_none());
    assert!(app.alerts.notification().is_none());
    update(&mut app, "AAPL", 216., 2); // In-flight answer for a removed symbol.
    assert!(app.alerts.notification().is_none());
}

#[test]
fn banner_is_an_overlay_in_all_views_and_modal_input_survives() {
    for view in ui::VIEWS {
        for bare in [false, true] {
            let mut app = app();
            app.bare = bare;
            app.show_rail = false;
            app.view_idx = ui::view_index(view.id());
            arm(&mut app, ">215");
            let before = render_sized(&mut app, 110, 32);
            let lists = app
                .hits
                .lists
                .iter()
                .map(|h| (h.area, h.first))
                .collect::<Vec<_>>();
            // The fired flag is changed by Data, never by draw.
            let quote = data(&app, "AAPL", 215., 1).quote;
            app.alerts
                .observe("AAPL", quote_sample(&quote, app.now()).unwrap(), app.now());
            let after = render_sized(&mut app, 110, 32);
            assert!(after.lines().last().unwrap().contains("ALERT AAPL >= 215"));
            assert_eq!(
                lists,
                app.hits
                    .lists
                    .iter()
                    .map(|h| (h.area, h.first))
                    .collect::<Vec<_>>()
            );
            assert_eq!(before.lines().count(), after.lines().count());
            let clock = Instant::now();
            assert!(app.alerts.tick(clock), "draw consumed the bell");
            assert!(!app.alerts.tick(clock));
            press(&mut app, KeyCode::Char('a'));
            for c in "NVDA".chars() {
                press(&mut app, KeyCode::Char(c));
            }
            let screen = render_sized(&mut app, 80, 24);
            assert!(screen.contains("NVDA"));
            assert!(screen.lines().last().unwrap().contains("ALERT AAPL"));
            assert_eq!(app.prompt.input, "NVDA");
            assert!(!app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 23,
                modifiers: KeyModifiers::NONE
            }));
            assert_eq!(app.prompt.input, "NVDA");
        }
    }
}

#[test]
fn alert_form_fits_small_terminals_and_retains_the_input_tail() {
    let mut app = app();
    press(&mut app, KeyCode::Char('A'));
    app.prompt.input = ">= 0000000000000000000000001.2500".into();
    for (w, h) in [
        (110, 32),
        (80, 24),
        (50, 10),
        (32, 7),
        (20, 4),
        (10, 2),
        (1, 1),
        (0, 0),
    ] {
        let screen = render_sized(&mut app, w, h);
        if w >= 10 && h >= 2 {
            assert!(screen.contains("1.2500▏"), "{w}x{h}: {screen}");
            assert!(screen.contains("enter"), "{w}x{h}: {screen}");
        }
    }
    app.prompt.input = "215".into();
    press(&mut app, KeyCode::Enter);
    let screen = render_sized(&mut app, 80, 7);
    assert!(screen.contains("Use >= 150 or <= 140"), "{screen}");
    assert!(screen.contains("esc cancel"), "{screen}");
}

#[test]
fn chart_levels_do_not_change_the_scale_or_create_events_during_render() {
    for chart_style in [ChartStyle::Candles, ChartStyle::Line] {
        for margin in [0, 20] {
            let mut app = app();
            app.chart_style = chart_style;
            app.chart.right_margin_pct = margin;
            app.view_idx = ui::view_index(ui::ViewId::Chart);
            app.show_rail = false;
            let before = render_sized(&mut app, 110, 32);
            arm(&mut app, ">10000");
            let after = render_sized(&mut app, 110, 32);
            assert!(after.contains("alert >= 10000"), "{after}");
            // Only the bar-context line changes when the level is off scale.
            for (old, new) in before.lines().zip(after.lines()) {
                if !old.contains("Last bar") {
                    assert_eq!(old, new);
                }
            }
            arm(&mut app, ">210");
            let inside = render_sized(&mut app, 110, 32);
            assert!(inside.contains("alert >= 210"));
            assert!(app.alerts.get("AAPL").unwrap().fired.is_none());
            assert!(!app.alerts.tick(Instant::now()));
        }
    }
}
