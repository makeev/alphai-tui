use super::*;
use crossterm::event::KeyModifiers;

fn type_query(app: &mut App, text: &str) {
    for c in text.chars() {
        assert!(!app.handle_key(KeyEvent::from(KeyCode::Char(c))));
    }
}

#[test]
fn ticker_search_ranks_symbols_and_finds_loaded_company_names() {
    let mut app = empty_app(vec![
        "XAAPL".into(),
        "AAPL.L".into(),
        "AAPL".into(),
        "MSFT".into(),
    ]);
    let mut quote = plain_quote("MSFT", 400.0, None, None);
    quote.name = Some("Microsoft Corporation".into());
    app.data.insert(
        "MSFT".into(),
        TickerData {
            quote,
            candles: vec![],
        },
    );
    press(&mut app, KeyCode::Char('/'));
    type_query(&mut app, " aApL ");
    assert_eq!(app.ticker_matches(), vec![2, 1, 0]);
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    type_query(&mut app, "MICROSOFT");
    assert_eq!(app.ticker_matches(), vec![3]);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.selected_symbol(), "MSFT");
    assert!(!app.ticker_search.open);
}

#[test]
fn ticker_search_browsing_and_cancel_leave_the_view_and_feed_untouched() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into(), "MSFT".into()]);
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.feeds
        .insert("AAPL".into(), FeedBundle::new(vec![], None, None));
    app.news_selected = 3;
    app.card_scroll = 7;
    let original = app.symbols.clone();
    press(&mut app, KeyCode::Char('/'));
    type_query(&mut app, "ms");
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "searching requested another feed");
    assert_eq!(app.selected_symbol(), "AAPL");
    assert!(!app.handle_key(KeyEvent::from(KeyCode::Esc)));
    assert_eq!(app.news_selected, 3);
    assert_eq!(app.card_scroll, 7);
    assert_eq!(app.symbols, original);
    assert!(app.pending_edits.is_empty());
    assert_eq!(app.view_id(), ui::ViewId::News);

    press(&mut app, KeyCode::Char('/'));
    type_query(&mut app, "ms");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.selected_symbol(), "MSFT");
    assert_eq!(app.news_selected, 0);
    assert_eq!(app.card_scroll, 0);
    assert_eq!(app.view_id(), ui::ViewId::News);
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchNews { symbol: Some(symbol), .. }) if symbol == "MSFT")
    );
}

#[test]
fn ticker_search_no_match_is_safe_and_text_never_fires_actions() {
    let mut app = fake_app();
    let theme = app.theme_name;
    press(&mut app, KeyCode::Char('/'));
    type_query(&mut app, "qdasjkl19?{}^");
    assert!(app.ticker_matches().is_empty());
    assert_eq!(app.symbols, vec!["AAPL", "MSFT"]);
    assert_eq!(app.theme_name, theme);
    assert!(!app.settings.open && !app.help.open && !app.prompt.open);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Enter);
    assert!(app.ticker_search.open);
    assert_eq!(app.selected_symbol(), "AAPL");
    assert!(render(&mut app).contains("No matches in your watchlist"));
    let input = app.ticker_search.input.clone();
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT));
    assert_eq!(app.ticker_search.input, input);
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
}

#[test]
fn ticker_search_current_selection_paging_and_reopening() {
    let mut app = empty_app((0..30).map(|i| format!("SYM{i:02}")).collect());
    app.selected = 23;
    app.card_scroll = 12;
    press(&mut app, KeyCode::Char('/'));
    assert_eq!(app.ticker_search.selected, Some(23));
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.card_scroll, 12,
        "confirming the same ticker reset reading position"
    );
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.ticker_search.selected, Some(13));
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.ticker_search.selected, Some(0));
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.ticker_search.selected, Some(10));
    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.ticker_search.selected, Some(29));
    let screen = render_sized(&mut app, 80, 10);
    assert!(screen.contains("▶ SYM29"), "{screen}");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.selected_symbol(), "SYM29");
    press(&mut app, KeyCode::Char('/'));
    assert!(app.ticker_search.input.is_empty());
    assert_eq!(app.ticker_search.selected, Some(29));
}

#[test]
fn ticker_search_remaps_and_opens_from_every_view() {
    for view in ui::VIEWS {
        let mut app = fake_app();
        app.positions = vec![
            Position {
                symbol: "AAPL".into(),
                qty: 1.0,
                avg_price: 200.0,
            },
            Position {
                symbol: "MSFT".into(),
                qty: 2.0,
                avg_price: 400.0,
            },
        ];
        app.view_idx = ui::view_index(view.id());
        app.keymap =
            crate::keymap::Keymap::from_config([("find_ticker", vec!["ctrl-f"])], &mut vec![]);
        press(&mut app, KeyCode::Char('/'));
        assert!(!app.ticker_search.open);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(app.ticker_search.open, "{:?}", view.id());
        type_query(&mut app, "msft");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_symbol(), "MSFT");
        assert_eq!(app.view_id(), view.id());
        if view.id() == ui::ViewId::Portfolio {
            assert_eq!(app.portfolio_selected, 1);
        }
    }
}

#[test]
fn ticker_search_opens_the_chart_for_an_unheld_ticker_from_portfolio() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Portfolio);
    app.positions = vec![Position {
        symbol: "AAPL".into(),
        qty: 1.0,
        avg_price: 200.0,
    }];
    press(&mut app, KeyCode::Char('/'));
    type_query(&mut app, "msft");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.selected_symbol(), "MSFT");
    assert_eq!(app.view_id(), ui::ViewId::Chart);
    assert_eq!(app.positions.len(), 1);
}

#[test]
fn ticker_search_preserves_identity_when_new_names_reorder_matches() {
    let mut app = fake_app();
    app.data.get_mut("AAPL").unwrap().quote.name = Some("Example Inc.".into());
    press(&mut app, KeyCode::Char('/'));
    type_query(&mut app, "example");
    assert_eq!(app.ticker_matches(), vec![0]);
    // A quote arrival expands the result list without changing its cursor.
    app.data.get_mut("MSFT").unwrap().quote.name = Some("Example Systems".into());
    press(&mut app, KeyCode::Down);
    assert_eq!(app.ticker_search.selected, Some(1));
    app.data.get_mut("AAPL").unwrap().quote.name = None;
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.selected_symbol(), "MSFT");
}

#[test]
fn ticker_search_render_keeps_names_quotes_and_freshness_readable() {
    let mut app = fake_app();
    app.data.get_mut("AAPL").unwrap().quote.name = Some("Apple Inc.".into());
    app.from_cache.insert("AAPL".into(), 1_700_000_000);
    app.errors.insert("MSFT".into(), "network error".into());
    press(&mut app, KeyCode::Char('/'));
    let screen = render_sized(&mut app, 100, 24);
    assert!(screen.contains("Apple Inc."), "{screen}");
    let row = screen.lines().find(|line| line.contains("▶ AAPL")).unwrap();
    assert!(
        row.contains("214.50 USD") && row.contains("+7.25%") && row.contains("cached"),
        "{row}"
    );
    assert!(screen.contains("stale"), "{screen}");
    for (width, height) in [(84, 20), (60, 12), (40, 10), (24, 8), (10, 5), (2, 2)] {
        render_sized(&mut app, width, height);
    }
    type_query(&mut app, "界".repeat(64).as_str());
    type_query(&mut app, "overflow");
    assert_eq!(app.ticker_search.input.chars().count(), 64);
    let screen = render_sized(&mut app, 40, 10);
    assert!(screen.contains('▏'), "long input hid its cursor:\n{screen}");
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    type_query(&mut app, "e\u{301}");
    press(&mut app, KeyCode::Backspace);
    assert!(app.ticker_search.input.is_empty());
}
