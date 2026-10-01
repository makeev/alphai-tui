use super::*;
use crate::domain::PriceFeed;

#[test]
fn quote_and_bar_keep_their_own_price_feed_and_clock() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.source_delay = None;
    app.sessions = Sessions::Extended;
    let ts = chrono::DateTime::parse_from_rfc3339("2026-09-30T14:00:00Z")
        .unwrap()
        .timestamp();
    let data = app.data.get_mut("AAPL").unwrap();
    data.quote.price = 220.25;
    data.quote.currency = Some("EUR".into());
    data.quote.timing.regular = Some(ts + 1800);
    data.quote.timing.regular_feed = PriceFeed::Iex;
    for (i, bar) in data.candles.iter_mut().enumerate() {
        bar.ts = ts - (29 - i) as i64 * 300;
        bar.feed = PriceFeed::DelayedSip;
    }
    let screen = render_sized(&mut app, 160, 40);
    let rail = screen.lines().nth(1).unwrap();
    for part in ["quote 220.25 EUR", "IEX", "30 Sep 10:30 ET"] {
        assert!(rail.contains(part), "{rail}");
    }
    let bar = screen.lines().find(|l| l.contains("Last bar")).unwrap();
    for part in ["214.50", "SIP · delayed 15m", "30 Sep 10:00 ET"] {
        assert!(bar.contains(part), "{bar}");
    }
    assert!(!screen.lines().nth(2).unwrap().contains("220.25"));
    app.show_rail = false;
    let screen = render_sized(&mut app, 160, 40);
    let title = screen.lines().nth(1).unwrap();
    assert!(
        title.contains("quote 220.25 EUR") && title.contains("+20.25"),
        "{title}"
    );
}

#[test]
fn news_keeps_the_selected_company_even_after_four_other_analyses() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    let mut a = article("Sector news", "AAPL", 9, "positive");
    let insights = a.enrichment.ai_trading_insights.as_mut().unwrap();
    let mut selected = insights.ticker_analysis[0].clone();
    selected.ticker = "aapl".into();
    selected.impact_analysis.as_mut().unwrap().summary =
        Some("Selected company explanation".into());
    insights.ticker_analysis = (0..4)
        .map(|i| {
            let mut t = selected.clone();
            t.ticker = format!("OTHER{i}");
            t.impact_analysis.as_mut().unwrap().summary = Some("Other company explanation".into());
            t
        })
        .chain(std::iter::once(selected.clone()))
        .collect();
    app.feeds
        .insert("AAPL".into(), FeedBundle::new(vec![a], None, None));
    let screen = render_sized(&mut app, 160, 45);
    let selected = screen.find("Selected company explanation").expect(&screen);
    let summary = screen.find("Summary of Sector news").expect(&screen);
    let other = screen.find("Other company explanation").expect(&screen);
    assert!(selected < summary && summary < other, "{screen}");
    assert_eq!(screen.matches("Selected company explanation").count(), 1);
}

#[test]
fn a_market_filing_names_its_company_before_the_trade_and_ai_analysis() {
    let mut a = article("Officer sold stock", "AVGO", 8, "negative");
    a.insider = Some(alphai::InsiderEvent {
        side: Some("sell".into()),
        total_value_usd: Some("250000000".into()),
        insider_name: "EXAMPLE OFFICER".into(),
        is_10b5_1: true,
        ..Default::default()
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|f| {
            ui::article::render_pane_with(
                f,
                f.area(),
                Some(&a),
                "AAPL",
                None,
                Some("stake -1.9% · 16 tranches".into()),
                "",
                &mut 0,
                &Theme::default(),
            )
        })
        .unwrap();
    let buf = terminal.backend().buffer();
    let text: String = (0..24)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
        .collect();
    assert!(
        text.lines()
            .nth(1)
            .unwrap()
            .contains("AVGO · EXAMPLE OFFICER"),
        "{text}"
    );
    let trade = text.find("SELL $250.0M · 10b5-1 plan").expect(&text);
    let stake = text.find("stake -1.9% · 16 tranches").expect(&text);
    let ai = text.find("AVGO · AI impact").expect(&text);
    assert!(trade < stake && stake < ai);
    assert!(
        !text.contains("AAPL"),
        "the watchlist ticker leaked into another company's filing"
    );
}
