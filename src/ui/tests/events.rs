//! Corporate events (the `event` block of filings and releases) in the feed
//! rows, the article card and the chart marks.

use super::*;

/// A row of the live fixture, trimmed from the API's answers of 13.11.2026.
fn live(case: &str) -> Article {
    let page: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/events.json")).unwrap();
    let row = page["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["_case"] == case)
        .unwrap_or_else(|| panic!("no fixture case {case}"))
        .clone();
    serde_json::from_value(row).unwrap()
}

fn news_app(symbol: &str, layout: NewsLayout, rows: Vec<Article>) -> App {
    let (mut app, _cmds) = empty_app_with_cmds(vec![symbol.into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.news_layout = layout;
    let min = app.news_min_score;
    head_fetch(&mut app, symbol, rows, Some(min));
    app
}

/// Typed rows name their event in the category column, and the SEC's
/// generic headings give way to the event's figures in the list. The card
/// keeps the filing's own title, so the heading is on screen exactly once.
#[test]
fn feed_rows_name_their_event_and_replace_sec_headings() {
    let mut app = news_app(
        "CRWV",
        NewsLayout::Side,
        vec![
            live("crwv_offering"),
            live("crwv_debt"),
            live("crwv_earnings"),
        ],
    );
    let screen = render_sized(&mut app, 185, 40);
    assert!(screen.contains("offering"), "screen:\n{screen}");
    assert!(screen.contains("debt"), "screen:\n{screen}");
    assert!(
        screen.contains("8-K guide↑"),
        "the release raised its outlook:\n{screen}"
    );
    assert!(
        screen.contains("$3.7B · priced · upsized · 2.875% convertible senior notes"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("$2.6B · SOFR + 5.50% · delayed draw term loan"),
        "screen:\n{screen}"
    );
    assert_eq!(
        screen.matches("Entry into a Material Definitive").count(),
        1,
        "only the card shows the heading:\n{screen}"
    );
}

/// A narrow terminal keeps the category column within its usual width.
#[test]
fn event_labels_fit_the_category_column_at_80_columns() {
    let mut app = news_app(
        "PATK",
        NewsLayout::Side,
        vec![live("patk_div"), live("lcii_target"), live("patk_news")],
    );
    let screen = render_sized(&mut app, 80, 24);
    assert!(screen.contains("dividend ↑"), "screen:\n{screen}");
    assert!(screen.contains("M&A"), "screen:\n{screen}");
}

/// On 110 columns the list beside the card leaves a press release's title
/// about thirty columns: the company's name and an ellipsis. The event's
/// figures take its place.
#[test]
fn a_title_too_narrow_to_read_gives_way_to_the_event() {
    let mut app = news_app("PATK", NewsLayout::Side, vec![live("patk_div")]);
    let screen = render_sized(&mut app, 110, 32);
    assert!(screen.contains("$0.52 quarterly"), "screen:\n{screen}");
}

/// With room to spare the written headline stays and the event's figures
/// follow it.
#[test]
fn a_wide_list_follows_the_headline_with_the_event() {
    let mut app = news_app("FXNC", NewsLayout::Stacked, vec![live("fxnc_div")]);
    let screen = render_sized(&mut app, 185, 40);
    assert!(
        screen.contains("ANNOUNCES 11.8% INCREASE IN QUARTERLY CASH DIVIDENDS  $0.19 quarterly"),
        "screen:\n{screen}"
    );
}

/// A market roundup tagged with NVDA names four tickers and carries
/// another company's outlook: it has no single reporter, so it shows no
/// event at all, neither an arrow in the list nor a section in the card.
#[test]
fn a_roundup_without_a_single_reporter_shows_no_event() {
    let mut app = news_app(
        "NVDA",
        NewsLayout::Side,
        vec![live("prn_roundup"), live("prn_other")],
    );
    let screen = render_sized(&mut app, 185, 40);
    assert!(screen.contains("movers "), "the category stays:\n{screen}");
    assert!(!screen.contains("guide"), "screen:\n{screen}");
    assert!(!screen.contains("Also in"), "screen:\n{screen}");
}

/// The card lists the quoted facts and always names who reported the
/// event, since `role` is the reporter's side: in Patrick's feed, LCI is
/// the target.
#[test]
fn card_names_the_reporter_and_lists_the_quoted_facts() {
    let mut app = news_app("PATK", NewsLayout::Side, vec![live("lcii_target")]);
    let screen = render_sized(&mut app, 185, 40);
    assert!(
        screen.contains("Event · M&A · definitive"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("reported by LCII as the target"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("expected close: first half of 2027"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("values quoted from the document"),
        "screen:\n{screen}"
    );
}

/// The enumerated facts join the heading, the figures line up in a table,
/// withheld facts are owned up to, and secondary events follow.
#[test]
fn card_heads_the_section_with_the_enumerated_facts() {
    let mut app = news_app("PATK", NewsLayout::Side, vec![live("patk_div")]);
    let screen = render_sized(&mut app, 185, 40);
    assert!(
        screen.contains("Event · dividend · quarterly · increase"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("per share    $0.52 per share"),
        "screen:\n{screen}"
    );
    assert!(
        screen.contains("buyback · $200.0M · was $61.9M"),
        "screen:\n{screen}"
    );

    let mut app = news_app("CRWV", NewsLayout::Side, vec![live("crwv_debt")]);
    let screen = render_sized(&mut app, 185, 40);
    assert!(
        screen.contains("Event · debt financing"),
        "screen:\n{screen}"
    );
    assert!(screen.contains("some withheld"), "screen:\n{screen}");
}

/// A long value wraps under its own column, not back at the card's edge.
#[test]
fn card_values_wrap_under_their_column() {
    let mut app = news_app("CRWV", NewsLayout::Side, vec![live("crwv_offering")]);
    let screen = render_sized(&mut app, 110, 40);
    let card: Vec<&str> = screen
        .lines()
        .map(|l| l.rsplit('│').nth(1).unwrap_or(""))
        .collect();
    let at = card
        .iter()
        .position(|l| l.contains("instrument"))
        .unwrap_or_else(|| panic!("screen:\n{screen}"));
    let column = card[at].find("2.875%").unwrap();
    let next = card[at + 1];
    assert!(
        next[..column].trim().is_empty() && !next[column..].trim().is_empty(),
        "the instrument continues under itself:\n{screen}"
    );
}

/// An event the server has typed but not read yet says so instead of
/// showing an empty table.
#[test]
fn card_says_when_there_are_no_facts_yet() {
    let mut row = live("crwv_offering");
    let event = row.event.as_mut().unwrap();
    event.facts.clear();
    event.verified = false;
    let mut app = news_app("CRWV", NewsLayout::Side, vec![row]);
    let screen = render_sized(&mut app, 185, 40);
    assert!(screen.contains("no facts yet"), "screen:\n{screen}");
}

/// On the candle chart a filing's event is drawn in outline, and the
/// chart's bottom line names it by the event, type first.
#[test]
fn chart_marks_events_in_outline_and_names_them() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let mut row = live("crwv_offering");
    row.enrichment.tickers = vec!["AAPL".into()];
    row.original.title = "Apple Inc. (AAPL): Entry into a Material Definitive Agreement".into();
    row.original.time_published = chrono::DateTime::from_timestamp(1_700_000_000 + 20 * 300, 0)
        .unwrap()
        .to_rfc3339();
    let min = app.news_min_score;
    head_fetch(&mut app, "AAPL", vec![row], Some(min));
    let screen = render_sized(&mut app, 120, 30);
    let body = panels(&screen);
    assert!(body.contains('◇'), "screen:\n{screen}");
    assert!(
        body.contains("offering · $3.7B · priced"),
        "screen:\n{screen}"
    );
}

fn events_app(symbol: &str) -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (mut app, cmds) = empty_app_with_cmds(vec![symbol.into()]);
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(ui::ViewId::News);
    app.news_layout = NewsLayout::Side;
    app.news_scope = NewsScope::Events;
    (app, cmds)
}

fn events_feed(app: &mut App, symbol: &str, rows: Vec<Article>, next: Option<&str>) {
    app.apply_alphai(alphai::Event::Feed {
        key: alphai::events_key(symbol),
        articles: rows,
        side: None,
        next_cursor: next.map(str::to_string),
        mode: alphai::FeedMode::Replace,
        min_relevance: None,
        categories: Categories::default(),
        decision: None,
    });
}

/// The Events scope asks for the ticker's typed events, refetches its head
/// on the TTL (the server has no delta feed of events: `event_type` with
/// `sort=ingested` is a 400), pages on `j`, and ignores the score filter.
#[test]
fn the_events_scope_fetches_heads_and_pages_but_never_polls() {
    let (mut app, mut cmds) = events_app("CRWV");
    app.ensure_alphai_data();
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchEvents { symbol, cursor: None }) if symbol == "CRWV"
    ));
    events_feed(
        &mut app,
        "CRWV",
        vec![live("crwv_offering"), live("crwv_debt")],
        Some("c1"),
    );
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "a fresh feed is left alone");

    // The score filter belongs to the news scopes.
    press(&mut app, KeyCode::Char('+'));
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "the score refetched events");

    // A TTL later the tick is a head refetch, never a delta poll.
    let stale = Instant::now() - std::time::Duration::from_secs(600);
    let b = app.feeds.get_mut("ev:CRWV").unwrap();
    b.fetched = stale;
    b.polled = stale;
    app.ensure_alphai_data();
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchEvents { cursor: None, .. })
    ));
    events_feed(
        &mut app,
        "CRWV",
        vec![live("crwv_offering"), live("crwv_debt")],
        Some("c1"),
    );

    // j on the last row asks for the next page of events.
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchEvents { cursor: Some(c), .. }) if c == "c1"
    ));
}

/// The scope reads like the ticker's feed: its own title and head line,
/// the event labels, the summaries in place of SEC headings.
#[test]
fn the_events_scope_renders_as_the_tickers_ledger() {
    let (mut app, _cmds) = events_app("CRWV");
    events_feed(
        &mut app,
        "CRWV",
        vec![
            live("crwv_earnings"),
            live("crwv_offering"),
            live("crwv_debt"),
        ],
        None,
    );
    let screen = render_sized(&mut app, 185, 40);
    assert!(screen.contains("CRWV  events  market"), "screen:\n{screen}");
    assert!(screen.contains("corporate events"), "screen:\n{screen}");
    assert!(screen.contains("8-K guide↑"), "screen:\n{screen}");
    assert!(
        screen.contains("$3.7B · priced · upsized"),
        "screen:\n{screen}"
    );
    assert!(
        !screen.contains("score 5+"),
        "no score filter here:\n{screen}"
    );
}

/// Once the Events scope has loaded, the chart marks reach as far back as
/// the events do, with no request of the chart's own.
#[test]
fn chart_marks_include_the_events_scope() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let mut row = live("crwv_offering");
    row.enrichment.tickers = vec!["AAPL".into()];
    row.original.title = "Apple Inc. (AAPL): Entry into a Material Definitive Agreement".into();
    row.original.time_published = chrono::DateTime::from_timestamp(1_700_000_000 + 20 * 300, 0)
        .unwrap()
        .to_rfc3339();
    events_feed(&mut app, "AAPL", vec![row], None);
    assert!(!app.feeds.contains_key("AAPL"), "no news feed loaded");
    let screen = render_sized(&mut app, 120, 30);
    assert!(panels(&screen).contains('◇'), "screen:\n{screen}");
}

/// The Earnings view lists what else the read's release announced, joined
/// by uid to the filing's row in any loaded feed; no request of its own.
#[test]
fn earnings_view_lists_what_else_the_release_announced() {
    let (mut app, mut cmds) = earnings_app(FULL_METRICS, "2026-11-17");
    let mut filing = live("crwv_earnings");
    filing.original.uid = "earn1".into();
    filing.enrichment.tickers = vec!["NVDA".into()];
    let min = app.news_min_score;
    head_fetch(&mut app, "NVDA", vec![filing], Some(min));
    while cmds.try_recv().is_ok() {}
    let screen = render_sized(&mut app, 110, 60);
    assert!(screen.contains("Also in this release"), "screen:\n{screen}");
    assert!(
        screen.contains("guidance · full year 2026 revenue · $13.0B to $13.3B"),
        "screen:\n{screen}"
    );
    assert!(cmds.try_recv().is_err(), "rendering fetched");
}

#[test]
fn the_config_can_start_in_the_events_scope() {
    let raw: crate::config::Config = toml::from_str("[ui]\nnews_scope = \"events\"").unwrap();
    let (resolved, warnings) = crate::config::resolve(&raw, None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(resolved.ui.news_scope, NewsScope::Events);
}
