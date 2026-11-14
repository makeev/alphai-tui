//! The Insider view with sale labels: the filing list, the header and the
//! tiles, the label filter and its request budget, the people mode, and a
//! server from before the label. Live CRWV rows of 13.11.2026, on a clock
//! frozen at that day so the trailing year stays the fixture's year.

use chrono::TimeZone;
use ratatui::buffer::Buffer;

use super::*;
use crate::insider_decision::InsiderDecision;
use crate::theme::{PRESETS, Panels};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/insider.json")).unwrap()
}

fn trades(name: &str) -> alphai::InsiderTrades {
    serde_json::from_value(fixture()[name].clone()).unwrap()
}

fn feed(name: &str) -> Vec<Article> {
    let page: alphai::NewsPage =
        serde_json::from_value(fixture()[format!("feed_{name}")].clone()).unwrap();
    page.results
}

fn deliver(app: &mut App, symbol: &str, rows: Vec<Article>, side: Option<alphai::InsiderTrades>) {
    let min = app.insider_min_score;
    let decision = app.insider_decision;
    app.apply_alphai(alphai::Event::Feed {
        key: alphai::insider_key(symbol),
        articles: rows,
        side: side.map(|t| FeedPayload::Insider(Box::new(t))),
        next_cursor: Some("c1".into()),
        mode: alphai::FeedMode::Replace,
        min_relevance: Some(min),
        categories: Categories::default(),
        decision: Some(decision),
    });
}

fn crwv_app() -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (mut app, cmds) = empty_app_with_cmds(vec!["CRWV".into()]);
    app.frozen_now = Some(
        chrono::Utc
            .with_ymd_and_hms(2026, 11, 13, 19, 0, 0)
            .unwrap(),
    );
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    deliver(&mut app, "CRWV", feed("crwv"), Some(trades("crwv")));
    (app, cmds)
}

fn drain(cmds: &mut tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    while cmds.try_recv().is_ok() {}
}

fn buffer_of(app: &mut App, w: u16, h: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// The header counts the year in dollars by label, the rows say why each
/// sale happened, and no side column is left (the glyph says sell).
#[test]
fn filings_carry_the_label_and_the_header_counts_dollars() {
    let (mut app, _cmds) = crwv_app();
    let screen = render_sized(&mut app, 185, 42);
    for want in [
        "sold $5.17B",
        "▼ 51% discretionary $2.62B ×34",
        "▾ routine $2.55B ×305",
        "10b5-1 plan 250, tax on vesting 55",
        "chose to sell (4):",
        "Magnetar Financial LLC",
        "Sachin Jain",
        "▾ tax on vesting",
        "Role",
        "Why",
        " all ",
    ] {
        assert!(screen.contains(want), "{want:?} missing:\n{screen}");
    }
    assert!(!screen.contains(" Side "), "{screen}");
    assert!(!screen.contains("% under 10b5-1"), "{screen}");
}

/// On 110 columns the list beside the card keeps the name readable.
#[test]
fn a_110_column_list_keeps_the_name() {
    let (mut app, _cmds) = crwv_app();
    let screen = render_sized(&mut app, 110, 32);
    assert!(screen.contains("Sachin Jain"), "{screen}");
    assert!(screen.contains("▾ tax on vesting"), "{screen}");
    assert!(screen.contains("▼ 51%"), "{screen}");
}

/// A tall terminal trades the two header lines for three tiles.
#[test]
fn tiles_take_the_header_on_a_tall_terminal() {
    let (mut app, _cmds) = crwv_app();
    let screen = render_sized(&mut app, 185, 50);
    for want in [
        "DISCRETIONARY SALES · 12m",
        "ROUTINE SALES · 12m",
        "PURCHASES · 12m",
        "34 sales by 4 insiders",
        "305 sales by 9 insiders",
        "none in 12m",
        "Magnetar Financial LLC  $2.40B",
        "▄█▀▀",
    ] {
        assert!(screen.contains(want), "{want:?} missing:\n{screen}");
    }
    assert!(!screen.contains("chose to sell (4)"), "{screen}");
}

/// The card says what the label is and why, in the site's words.
#[test]
fn the_card_explains_the_reason() {
    let (mut app, _cmds) = crwv_app();
    let screen = render_sized(&mut app, 185, 42);
    assert!(screen.contains("non-discretionary"), "{screen}");
    assert!(
        screen.contains("why  tax on vesting: Shares sold to pay the tax due when stock vested."),
        "{screen}"
    );
}

/// f moves the filter: the rows on screen are filtered at once, and one
/// request asks for the list alone; the chart bundle is the whole year
/// whatever the filter and stays.
#[test]
fn f_refetches_the_list_alone_and_keeps_the_chart() {
    let (mut app, mut cmds) = crwv_app();
    drain(&mut cmds);
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.insider_decision, InsiderDecision::Discretionary);
    app.ensure_alphai_data();
    assert!(
        app.feeds[&alphai::insider_key("CRWV")].articles.is_empty(),
        "the routine rows on screen left with the filter"
    );
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchInsider {
            decision,
            side,
            cursor,
            sort,
            ..
        }) => {
            assert_eq!(decision, InsiderDecision::Discretionary);
            assert!(!side, "the chart bundle was fetched again");
            assert_eq!(cursor, None);
            assert_eq!(sort, alphai::Sort::Published);
        }
        other => panic!("expected a filtered head fetch, got {:?}", other.is_ok()),
    }
    assert!(cmds.try_recv().is_err(), "one press, one request");

    let mut row = feed("nvda")
        .into_iter()
        .find(|a| a.insider.as_ref().unwrap().decision.as_deref() == Some("discretionary"))
        .unwrap();
    row.enrichment.tickers = vec!["CRWV".into()];
    deliver(&mut app, "CRWV", vec![row], None);
    let b = &app.feeds[&alphai::insider_key("CRWV")];
    assert!(b.insider_trades().is_some(), "the chart bundle was dropped");
    assert_eq!(b.decision, Some(InsiderDecision::Discretionary));
    assert!(!app.is_unseen(&alphai::insider_key("CRWV"), &b.articles[0]));
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "a fresh filtered list refetched");
    let screen = render_sized(&mut app, 185, 42);
    assert!(screen.contains(" discretionary "), "{screen}");
    assert!(screen.contains("Mark Stevens"), "{screen}");
}

/// The delta poll asks with the filter on screen.
#[test]
fn the_delta_poll_carries_the_filter() {
    let (mut app, mut cmds) = crwv_app();
    drain(&mut cmds);
    app.insider_decision = InsiderDecision::Routine;
    // What a filtered answer holds: only rows the filter keeps (a row it
    // does not keep would mean a server that ignores the filter).
    let routine: Vec<Article> = feed("crwv")
        .into_iter()
        .filter(|a| InsiderDecision::Routine.keeps(a.insider.as_ref().unwrap().decision.as_deref()))
        .collect();
    assert!(!routine.is_empty());
    deliver(&mut app, "CRWV", routine, Some(trades("crwv")));
    assert_eq!(app.insider_decision, InsiderDecision::Routine);
    app.feeds
        .get_mut(&alphai::insider_key("CRWV"))
        .unwrap()
        .polled = Instant::now() - std::time::Duration::from_secs(600);
    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchInsider { decision, sort, .. }) => {
            assert_eq!(decision, InsiderDecision::Routine);
            assert_eq!(sort, alphai::Sort::Ingested);
        }
        other => panic!("expected a delta poll, got {:?}", other.is_ok()),
    }
}

/// A server that does not know the filter answers with every row: the
/// view says so and goes back to all instead of mislabelling the list.
#[test]
fn a_server_that_ignores_the_filter_falls_back_to_all() {
    let (mut app, _cmds) = crwv_app();
    press(&mut app, KeyCode::Char('f'));
    deliver(&mut app, "CRWV", feed("crwv"), None);
    assert_eq!(app.insider_decision, InsiderDecision::All);
    assert!(app.notice.is_some());
}

/// Without labels the view keeps its old look and f says why it does
/// nothing.
#[test]
fn a_server_without_labels_keeps_the_old_look() {
    let (mut app, _cmds) = empty_app_with_cmds(vec!["CRWV".into()]);
    app.frozen_now = Some(
        chrono::Utc
            .with_ymd_and_hms(2026, 11, 13, 19, 0, 0)
            .unwrap(),
    );
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    let mut rows = feed("crwv");
    for a in &mut rows {
        let i = a.insider.as_mut().unwrap();
        i.decision = None;
        i.decision_reason = None;
    }
    let mut t = trades("crwv");
    for e in &mut t.chart_events {
        e.decision = None;
        e.decision_reason = None;
    }
    deliver(&mut app, "CRWV", rows, Some(t));
    let screen = render_sized(&mut app, 185, 42);
    assert!(screen.contains("under 10b5-1 plans"), "{screen}");
    assert!(screen.contains("Side"), "{screen}");
    // The plain legend: plan sales are the dimmed sell.
    assert!(screen.contains("▼ sell  ▼ plan sale"), "{screen}");
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.insider_decision, InsiderDecision::All);
    assert!(app.notice.as_ref().unwrap().0.contains("does not label"));
}

/// x lists the year by person, largest first, with the selected person's
/// filings beside; the mode sends nothing and the cursor keys stay in it.
#[test]
fn people_mode_lists_the_year_and_sends_nothing() {
    let (mut app, mut cmds) = crwv_app();
    drain(&mut cmds);
    press(&mut app, KeyCode::Char('x'));
    assert!(app.insider_people);
    let screen = render_sized(&mut app, 185, 42);
    for want in [
        "Insiders · CRWV · 12m",
        "Share of $ sold",
        "chose to sell",
        "27 filings",
        "open market",
    ] {
        assert!(screen.contains(want), "{want:?} missing:\n{screen}");
    }
    let first = screen
        .lines()
        .find(|l| l.contains('▶'))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(first.contains("Magnetar Financial LLC"), "{first}");

    // Stale or not, the people mode asks for nothing.
    let b = app.feeds.get_mut(&alphai::insider_key("CRWV")).unwrap();
    let stale = Instant::now() - std::time::Duration::from_secs(3_600);
    b.fetched = stale;
    b.polled = stale;
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "the people mode fetched");

    // The cursor keys move the person, never page the feed; the size
    // filter belongs to the filings.
    let people = app.insider_people_list().len();
    for _ in 0..people + 3 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.insider_person, people - 1);
    // +/- move the filings' size filter here too, but the refetch waits
    // for the filings to come back.
    let score = app.insider_min_score;
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(app.insider_min_score, score + 1);
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "a key in the people mode fetched");

    // The filter narrows the people without a request.
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.insider_people_list().len(), 4);
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err());

    // Back on the filings, the moved filters refetch the list once.
    press(&mut app, KeyCode::Char('x'));
    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchInsider {
            min_relevance,
            decision,
            ..
        }) => {
            assert_eq!(min_relevance, Some(score + 1));
            assert_eq!(decision, InsiderDecision::Discretionary);
        }
        other => panic!("expected the deferred refetch, got {:?}", other.is_ok()),
    }
    assert!(cmds.try_recv().is_err(), "one request for both filters");
}

/// The default shows every filing, so the list matches the chart; a raised
/// size filter says how many the chart draws that the list does not.
#[test]
fn a_raised_size_filter_says_what_it_hides() {
    let (mut app, _cmds) = crwv_app();
    assert_eq!(app.insider_min_score, 1);
    let screen = render_sized(&mut app, 185, 42);
    assert!(!screen.contains("hidden by score"), "{screen}");
    assert!(!screen.contains("score 1+"), "{screen}");
    // The same rows under a recorded filter of 6: the chart's other
    // filings since the oldest row are the ones it hides.
    app.insider_min_score = 6;
    app.feeds
        .get_mut(&alphai::insider_key("CRWV"))
        .unwrap()
        .min_score = Some(6);
    let screen = render_sized(&mut app, 185, 42);
    assert!(screen.contains("score 6+"), "{screen}");
    assert!(
        screen.contains("hidden by score 6+ ·  -  shows them"),
        "{screen}"
    );
}

/// The selected person's events keep their colors on the chart and
/// everyone else's step back to the faint tier.
#[test]
fn the_chart_lights_up_the_selected_person() {
    let (mut app, _cmds) = crwv_app();
    app.insider_chart = crate::app::InsiderChartWindow::M12;
    press(&mut app, KeyCode::Char('x'));
    let warn_marks = |app: &mut App| {
        let buf = buffer_of(app, 185, 42);
        let theme = app.theme;
        let width = usize::from(buf.area.width);
        buf.content()
            .chunks(width)
            // The legend on the chart's title row draws its glyph like the
            // marks; it names the color for everyone, highlight or not.
            .filter(|row| {
                !row.iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
                    .contains("Form 4")
            })
            .flatten()
            // The scatter's marks are bold; the header's glyphs share the
            // color but not the weight.
            .filter(|c| {
                c.symbol() == "▼"
                    && c.fg == theme.warn
                    && c.modifier.contains(ratatui::style::Modifier::BOLD)
            })
            .count()
    };
    assert!(
        warn_marks(&mut app) > 0,
        "Magnetar's sales lost their color"
    );
    let intrator = app
        .insider_people_list()
        .iter()
        .position(|p| p.name == "Intrator Michael N")
        .unwrap();
    app.insider_person = intrator;
    assert_eq!(
        warn_marks(&mut app),
        0,
        "someone else's sales kept the color"
    );
}

/// Enter opens the person's latest filing with an article, and says so
/// when none of them has one.
#[test]
fn enter_without_an_article_says_so() {
    let (mut app, _cmds) = empty_app_with_cmds(vec!["CRWV".into()]);
    app.frozen_now = Some(
        chrono::Utc
            .with_ymd_and_hms(2026, 11, 13, 19, 0, 0)
            .unwrap(),
    );
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(ui::ViewId::Insider);
    let mut t = trades("crwv");
    for e in &mut t.chart_events {
        e.has_article = false;
    }
    deliver(&mut app, "CRWV", feed("crwv"), Some(t));
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Enter);
    let notice = app.notice.as_ref().map(|n| n.0.clone()).unwrap_or_default();
    assert!(
        notice.contains("no AlphAI article for Magnetar Financial LLC"),
        "{notice}"
    );
}

/// The footer names the new keys within the 110 columns it has.
#[test]
fn the_footer_names_the_filter_and_the_people() {
    let (mut app, _cmds) = crwv_app();
    let footer = crate::ui::hints_text(&app);
    assert!(footer.contains("f  filter"), "{footer}");
    assert!(footer.contains("x  people"), "{footer}");
    assert!(footer.chars().count() <= 110, "{footer}");
    app.insider_people = true;
    let people = crate::ui::hints_text(&app);
    assert!(people.chars().count() <= 110, "{people}");
    assert!(
        people.contains("person") && people.contains("x  filings"),
        "{people}"
    );
    assert!(
        !people.contains("card"),
        "the people mode has no card: {people}"
    );
}

/// Every named preset paints the labelled view, tiles and people mode
/// included, in its own colors.
#[test]
fn every_preset_paints_the_labelled_view() {
    let (mut app, _cmds) = crwv_app();
    for (name, theme) in PRESETS.iter().skip(1) {
        app.theme = *theme;
        for panels in Panels::ALL {
            app.theme.panels = panels;
            for people in [false, true] {
                app.insider_people = people;
                for (w, h) in [(120, 47), (110, 32)] {
                    let buf = buffer_of(&mut app, w, h);
                    super::themes::assert_explicit_colors(
                        &buf,
                        &format!("{name}, {panels:?}, people {people}, {w}x{h}"),
                    );
                }
            }
        }
    }
}

/// A row of the live fixture with its label and side changed, to stand for
/// the kinds the live sample has none of.
fn row_as(side: &str, code: &str, decision: Option<&str>, reason: Option<&str>) -> Article {
    let mut a = feed("crwv").remove(0);
    let i = a.insider.as_mut().unwrap();
    i.side = Some(side.into());
    i.transaction_code = Some(code.into());
    i.decision = decision.map(str::to_string);
    i.decision_reason = reason.map(str::to_string);
    i.is_10b5_1 = false;
    a.original.uid = format!("{side}-{code}-{decision:?}");
    a
}

/// Cycling the filter round in the people mode leaves the filings as they
/// were: nothing was trimmed under a filter that never reached the list.
#[test]
fn a_full_filter_round_in_the_people_mode_keeps_the_rows() {
    let (mut app, mut cmds) = crwv_app();
    drain(&mut cmds);
    let rows = app.feeds[&alphai::insider_key("CRWV")].articles.len();
    press(&mut app, KeyCode::Char('x'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Char('f'));
        app.ensure_alphai_data();
    }
    press(&mut app, KeyCode::Char('x'));
    app.ensure_alphai_data();
    assert_eq!(app.insider_decision, InsiderDecision::All);
    assert_eq!(app.feeds[&alphai::insider_key("CRWV")].articles.len(), rows);
    assert!(cmds.try_recv().is_err(), "nothing moved, nothing to fetch");
}

/// An answer to a filter since moved never sits under the new pill: its
/// rows leave on the next tick, and the right filter is asked for.
#[test]
fn an_answer_to_an_old_filter_is_trimmed_to_the_new_one() {
    let (mut app, mut cmds) = crwv_app();
    drain(&mut cmds);
    press(&mut app, KeyCode::Char('f')); // discretionary
    app.ensure_alphai_data();
    drain(&mut cmds);
    press(&mut app, KeyCode::Char('f')); // routine, the first still in flight
    let mut old = row_as("sell", "S", Some("discretionary"), Some("open_market"));
    old.enrichment.tickers = vec!["CRWV".into()];
    let min = app.insider_min_score;
    app.apply_alphai(alphai::Event::Feed {
        key: alphai::insider_key("CRWV"),
        articles: vec![old],
        side: None,
        next_cursor: None,
        mode: alphai::FeedMode::Replace,
        min_relevance: Some(min),
        categories: Categories::default(),
        decision: Some(InsiderDecision::Discretionary),
    });
    app.ensure_alphai_data();
    assert!(app.feeds[&alphai::insider_key("CRWV")].articles.is_empty());
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchInsider {
            decision: InsiderDecision::Routine,
            side: false,
            ..
        })
    ));
}

/// The people mode leaves room for people on the default 80x24 terminal:
/// the chart gives way before the table does.
#[test]
fn the_people_mode_fits_a_small_terminal() {
    let (mut app, _cmds) = crwv_app();
    press(&mut app, KeyCode::Char('x'));
    let screen = render_sized(&mut app, 80, 24);
    for name in [
        "Magnetar Financial LLC",
        "Brian Venturo",
        "Michael Intrator",
    ] {
        assert!(screen.contains(name), "{name} missing:\n{screen}");
    }
}

/// `r` while a list-only refetch is in flight leaves nothing to inherit the
/// chart from: the next tick asks for the head again, chart included, once.
#[test]
fn a_reload_during_a_list_only_refetch_brings_the_chart_back() {
    let (mut app, mut cmds) = crwv_app();
    drain(&mut cmds);
    press(&mut app, KeyCode::Char('f'));
    app.ensure_alphai_data();
    drain(&mut cmds);
    app.feeds.remove(&alphai::insider_key("CRWV")); // what r does
    deliver(&mut app, "CRWV", Vec::new(), None);
    assert!(
        app.feeds[&alphai::insider_key("CRWV")]
            .insider_trades()
            .is_none()
    );
    app.ensure_alphai_data();
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchInsider { side: true, .. })
    ));
}

/// A chart bundle that fails to load keeps the one the feed holds and does
/// not make the head due again: no request storm.
#[test]
fn a_failed_chart_bundle_does_not_refetch_every_tick() {
    let (mut app, mut cmds) = crwv_app();
    drain(&mut cmds);
    deliver(&mut app, "CRWV", feed("crwv"), None);
    assert!(
        app.feeds[&alphai::insider_key("CRWV")]
            .insider_trades()
            .is_some()
    );
    for _ in 0..3 {
        app.ensure_alphai_data();
    }
    assert!(cmds.try_recv().is_err(), "a failed bundle refetched");
}

/// A server without the label filter answers it with a 400: back to all,
/// with a notice, and no error screen that `r` would only repeat.
#[test]
fn a_400_on_the_filter_falls_back_to_all() {
    let (mut app, _cmds) = crwv_app();
    press(&mut app, KeyCode::Char('f'));
    app.apply_alphai(alphai::Event::Error {
        key: alphai::insider_key("CRWV"),
        error: "API 400: Validation error".into(),
    });
    assert_eq!(app.insider_decision, InsiderDecision::All);
    assert!(!app.alphai_errors.contains_key(&alphai::insider_key("CRWV")));
    assert!(app.notice.is_some());
}

/// The server labels sales, so a ticker with none to label keeps the
/// labelled view and its filter.
#[test]
fn a_ticker_without_sales_keeps_the_filter() {
    let (mut app, _cmds) = crwv_app();
    app.symbols.push("BUYS".into());
    app.selected = 1;
    let mut buy = row_as("buy", "P", None, None);
    buy.enrichment.tickers = vec!["BUYS".into()];
    let mut t = trades("crwv");
    t.chart_events.retain(|e| e.side.as_deref() == Some("buy"));
    deliver(&mut app, "BUYS", vec![buy], Some(t));
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.insider_decision, InsiderDecision::Discretionary);
    let screen = render_sized(&mut app, 185, 42);
    assert!(screen.contains(" discretionary "), "{screen}");
}

/// The kinds the live sample lacks: an unclear sale, a buy and a hand-back
/// to the issuer, in the list and in the card.
#[test]
fn every_kind_has_its_glyph_and_words() {
    let (mut app, _cmds) = crwv_app();
    let rows = vec![
        row_as("sell", "S", Some("unknown"), None),
        row_as("buy", "P", None, None),
        row_as("other", "D", None, None),
    ];
    deliver(&mut app, "CRWV", rows, Some(trades("crwv")));
    let screen = render_sized(&mut app, 185, 42);
    for want in [
        "? unclear",
        "▲ buy",
        "◇ to issuer",
        "the filing does not let us tell",
    ] {
        assert!(screen.contains(want), "{want:?} missing:\n{screen}");
    }
}

/// A click picks a person; a click on the empty space below does nothing.
#[test]
fn a_click_picks_a_person() {
    let (mut app, _cmds) = crwv_app();
    press(&mut app, KeyCode::Char('x'));
    let screen = render_sized(&mut app, 185, 42);
    let (y, line) = screen
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("Michael Intrator"))
        .unwrap_or_else(|| panic!("{screen}"));
    let x = line.find("Michael").unwrap() as u16;
    let click = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: x,
        row: y as u16,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(click));
    let people = app.insider_people_list();
    assert_eq!(people[app.insider_person].name, "Intrator Michael N");
}

/// A person with only routine sales still stands out on the chart: their
/// marks step up to body text while everyone else's are faint.
#[test]
fn a_routine_seller_stands_out_on_the_chart() {
    let (mut app, _cmds) = crwv_app();
    app.insider_chart = crate::app::InsiderChartWindow::M12;
    press(&mut app, KeyCode::Char('x'));
    app.insider_person = app
        .insider_people_list()
        .iter()
        .position(|p| p.name == "Intrator Michael N")
        .unwrap();
    // On the ANSI theme of the tests text and faint share the terminal's
    // color; what tells them apart is DIM, which only the faint tier has.
    let buf = buffer_of(&mut app, 185, 42);
    let dim = ratatui::style::Modifier::DIM;
    let marks: Vec<_> = buf.content().iter().filter(|c| c.symbol() == "▾").collect();
    let lit = marks.iter().filter(|c| !c.modifier.contains(dim)).count();
    let faded = marks.iter().filter(|c| c.modifier.contains(dim)).count();
    assert!(lit > 0, "the routine seller's marks did not light up");
    assert!(faded > 0, "everyone else's marks did not step back");
}

/// On 80 columns the header keeps both sums: the bar and the long labels
/// give way first.
#[test]
fn the_header_keeps_both_sums_at_80_columns() {
    let (mut app, _cmds) = crwv_app();
    let screen = render_sized(&mut app, 80, 24);
    let header = screen.lines().nth(2).unwrap_or_default();
    assert!(header.contains("$2.62B ×34"), "{header}");
    assert!(header.contains("$2.55B ×305"), "{header}");
    assert!(header.contains('█') || header.contains('▒'), "{header}");
}

/// A list beside the card on a 90-column terminal keeps the owner's name
/// readable: the Why column gives way first.
#[test]
fn a_90_column_list_keeps_the_name() {
    let (mut app, _cmds) = crwv_app();
    let screen = render_sized(&mut app, 90, 30);
    let row = screen
        .lines()
        .find(|l| l.contains("Sachin Jain"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(row.contains("Sachin Jain  ▾"), "the name was cut: {row}");
}

/// The chart's every kind of mark, and its bars when the window holds
/// purchases and sales both: an unclear sale `?`, a hand-back `◇`, a buy
/// `▲`, with the window's counts in the legend.
#[test]
fn the_chart_draws_every_kind_and_both_sides() {
    let (mut app, _cmds) = crwv_app();
    let day = |d: &str| format!("2026-{d}");
    let event = |side: &str, code: &str, decision: Option<&str>, date: &str, usd: &str| {
        let mut e = trades("crwv").chart_events.remove(0);
        e.side = Some(side.into());
        e.transaction_code = Some(code.into());
        e.decision = decision.map(str::to_string);
        e.decision_reason = None;
        e.transaction_date = Some(day(date));
        e.total_value_usd = Some(usd.into());
        e.news_uid = Some(format!("{side}{code}{date}"));
        e
    };
    let mut t = trades("crwv");
    t.chart_events = vec![
        event("sell", "S", Some("discretionary"), "10-06", "50000000"),
        event("sell", "S", Some("plan_like"), "10-13", "5000000"),
        event("sell", "S", Some("unknown"), "10-20", "2000000"),
        event("buy", "P", None, "10-27", "300000"),
        event("sell", "D", None, "11-03", "1000000"),
    ];
    deliver(&mut app, "CRWV", feed("crwv"), Some(t));
    let screen = render_sized(&mut app, 185, 42);
    let body = panels(&screen);
    for glyph in ['▼', '▾', '?', '▲', '◇'] {
        assert!(body.contains(glyph), "{glyph} missing:\n{screen}");
    }
    // The legend in the chart's corner names every kind drawn, in order.
    let legend = "▼ discretionary  ▾ routine  ? unclear  ▲ buy  ◇ to issuer";
    assert!(screen.contains(legend), "legend missing:\n{screen}");
    // Two-sided bars: the purchase grows up from the middle in its own
    // color, the sales hang under it.
    let buf = buffer_of(&mut app, 185, 42);
    let theme = app.theme;
    assert!(
        buf.content()
            .iter()
            .any(|c| matches!(c.symbol(), "█" | "▄") && c.fg == theme.pos),
        "no purchase bar:\n{screen}"
    );
}

/// Column of `needle` in a rendered line, in cells (the frame glyphs are
/// several bytes each).
fn column(line: &str, needle: &str) -> u16 {
    let at = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {line}"));
    line[..at].chars().count() as u16
}

fn row_of<'a>(screen: &'a str, needle: &str) -> (u16, &'a str) {
    screen
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(needle))
        .map(|(y, l)| (y as u16, l))
        .unwrap_or_else(|| panic!("{needle:?} missing:\n{screen}"))
}

fn click_at(app: &mut App, x: u16, y: u16) -> bool {
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: x,
        row: y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    })
}

fn screen_of(buf: &Buffer) -> String {
    let width = usize::from(buf.area.width);
    buf.content()
        .chunks(width)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The chart names its colors in the top right corner, each glyph drawn
/// exactly like the marks it names; where the sums leave no room there,
/// the legend moves to the bottom edge.
#[test]
fn the_legend_names_each_color_in_the_corner() {
    let (mut app, _cmds) = crwv_app();
    app.insider_chart = crate::app::InsiderChartWindow::M12;
    let buf = buffer_of(&mut app, 185, 42);
    let screen = screen_of(&buf);
    let (y, line) = row_of(&screen, "Form 4  g   3m  12m  off");
    assert!(
        line.trim_end().ends_with("▼ discretionary  ▾ routine ╮"),
        "{line}"
    );
    let look = |x: u16, y: u16| {
        let c = &buf[(x, y)];
        (c.symbol().to_string(), c.fg, c.modifier)
    };
    for (glyph, word) in [("▼", "▼ discretionary"), ("▾", "▾ routine")] {
        let legend = look(column(line, word), y);
        let mark = (y + 1..y + 6)
            .flat_map(|my| (0..185).map(move |mx| (mx, my)))
            .map(|(mx, my)| look(mx, my))
            .find(|m| m.0 == glyph && (glyph != "▼" || m.1 == app.theme.warn))
            .unwrap_or_else(|| panic!("no {glyph} mark:\n{screen}"));
        assert_eq!(legend, mark, "{word} is drawn unlike its marks");
    }

    let screen = render_sized(&mut app, 72, 30);
    let (_, top) = row_of(&screen, "Form 4  g ");
    assert!(!top.contains("discretionary"), "{top}");
    let (_, bottom) = row_of(&screen, "╰ ▼ discretionary  ▾ routine ─");
    assert!(bottom.starts_with('╰'), "{bottom}");
}

/// The list's title carries the switch between the filings and the people
/// beside the label filter, both behind their keys, the current pill lit,
/// and a click on any pill acts like its key.
#[test]
fn the_title_switches_modes_and_filters_by_click() {
    let (mut app, _cmds) = crwv_app();
    let theme = app.theme;
    let buf = buffer_of(&mut app, 185, 42);
    let screen = screen_of(&buf);
    let (y, line) = row_of(&screen, "Insider · CRWV");
    assert!(line.contains(" f   all  discretionary  routine"), "{line}");
    assert!(line.contains(" x   filings  people"), "{line}");
    // The keys sit on their own plates, apart from the lit pill.
    let cap = &buf[(column(line, " f ") + 1, y)];
    assert!(
        cap.modifier
            .contains(ratatui::style::Modifier::REVERSED | ratatui::style::Modifier::BOLD),
        "{cap:?}"
    );
    assert_ne!(buf[(column(line, " f ") + 3, y)].bg, theme.accent);
    assert_eq!(buf[(column(line, " filings ") + 1, y)].bg, theme.accent);
    assert_ne!(buf[(column(line, " people ") + 1, y)].bg, theme.accent);

    assert!(click_at(&mut app, column(line, " people ") + 1, y));
    assert!(app.insider_people);
    let buf = buffer_of(&mut app, 185, 42);
    let screen = screen_of(&buf);
    let (y, line) = row_of(&screen, "Insiders · CRWV · 12m");
    assert!(line.contains(" x   filings  people"), "{line}");
    assert_eq!(buf[(column(line, " people ") + 1, y)].bg, theme.accent);

    assert!(click_at(&mut app, column(line, " routine ") + 1, y));
    assert_eq!(app.insider_decision, InsiderDecision::Routine);
    // The key letter steps the filter on, like the key.
    let screen = render_sized(&mut app, 185, 42);
    let (y, line) = row_of(&screen, "Insiders · CRWV · 12m");
    assert!(click_at(&mut app, column(line, " f ") + 1, y));
    assert_eq!(app.insider_decision, InsiderDecision::All);

    let screen = render_sized(&mut app, 185, 42);
    let (y, line) = row_of(&screen, "Insiders · CRWV · 12m");
    assert!(click_at(&mut app, column(line, " filings ") + 1, y));
    assert!(!app.insider_people);
}

/// A narrow list keeps the switch and says where x leads; the filter shows
/// only its lit pill.
#[test]
fn a_narrow_title_keeps_the_switch() {
    let (mut app, _cmds) = crwv_app();
    let screen = render_sized(&mut app, 110, 32);
    let (_, line) = row_of(&screen, "Insider · CRWV");
    assert!(line.contains(" f   all  "), "{line}");
    assert!(!line.contains("discretionary"), "{line}");
    assert!(line.contains(" x   filings  people"), "{line}");
    app.insider_min_score = 6;
    let screen = render_sized(&mut app, 110, 32);
    let (_, line) = row_of(&screen, "Insider · CRWV");
    assert!(line.contains("score 6+"), "{line}");
    // The switch outranks the score's keys: the score goes back to text.
    assert!(line.contains(" x   people "), "{line}");
    assert!(!line.contains(" -  score"), "{line}");
}

/// Where the title has room, the score sits between the keys that move it,
/// and a click on either key moves it like the key; the list's hint about
/// hidden filings names its key the same way.
#[test]
fn the_score_stepper_takes_clicks() {
    let (mut app, _cmds) = crwv_app();
    app.insider_card = false;
    app.insider_min_score = 6;
    let screen = render_sized(&mut app, 185, 42);
    let (y, line) = row_of(&screen, "Insider · CRWV");
    assert!(line.contains("·  -  score 6+  +  "), "{line}");
    assert!(click_at(&mut app, column(line, " + ") + 1, y));
    assert_eq!(app.insider_min_score, 7);
    let screen = render_sized(&mut app, 185, 42);
    let (y, line) = row_of(&screen, "Insider · CRWV");
    assert!(click_at(&mut app, column(line, " -  score") + 1, y));
    assert_eq!(app.insider_min_score, 6);
    let screen = render_sized(&mut app, 185, 42);
    let (y, line) = row_of(&screen, "shows them");
    assert!(click_at(&mut app, column(line, " -  shows") + 1, y));
    assert_eq!(app.insider_min_score, 5);
}

/// Rows cut locally to follow a filter are not the list they were cut
/// from: when the refetch fails with a 400 and the filter falls back to
/// all, the cut list is refetched rather than shown under the all pill.
#[test]
fn a_cut_list_is_refetched_when_the_filter_comes_back() {
    let (mut app, mut cmds) = crwv_app();
    let all_rows = app.feeds[&alphai::insider_key("CRWV")].articles.len();
    press(&mut app, KeyCode::Char('f'));
    app.ensure_alphai_data();
    let key = alphai::insider_key("CRWV");
    assert!(app.feeds[&key].articles.len() < all_rows, "nothing was cut");
    drain(&mut cmds);
    // The refetch's answer: the error clears it from the requests in flight.
    app.apply_alphai(alphai::Event::Error {
        key: key.clone(),
        error: "API 400: Validation error".into(),
    });
    assert_eq!(app.insider_decision, InsiderDecision::All);
    app.ensure_alphai_data();
    match cmds.try_recv() {
        Ok(alphai::Cmd::FetchInsider { decision, .. }) => {
            assert_eq!(decision, InsiderDecision::All);
        }
        other => panic!("the cut list was kept: {:?}", other.is_ok()),
    }
}

/// One sale still waiting for its label leaves the labelled view in place;
/// it reads as unlabelled beside the others.
#[test]
fn one_unlabelled_sale_keeps_the_labels() {
    let mut t = trades("crwv");
    let first_sale = t
        .chart_events
        .iter_mut()
        .find(|e| e.decision.is_some())
        .unwrap();
    first_sale.decision = None;
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
    let r = crate::insider_decision::Rollup::over(
        t.chart_events.iter(),
        today - chrono::Days::new(364),
        today,
    );
    assert!(r.sold.count >= 1);
    assert!(r.has_decisions);
}

/// A co-filed copy of a trade counts once: in the sums, the people and the
/// weeks.
#[test]
fn a_co_filed_copy_counts_once() {
    let t = trades("crwv");
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
    let from = today - chrono::Days::new(364);
    let before = crate::insider_decision::Rollup::over(t.chart_events.iter(), from, today);
    let mut doubled = t.chart_events.clone();
    let mut copy = doubled
        .iter()
        .find(|e| e.decision.as_deref() == Some("discretionary"))
        .unwrap()
        .clone();
    copy.duplicate_of = copy.news_uid.clone().or(Some("original".into()));
    copy.insider_name = "General Partner LP".into();
    doubled.push(copy);
    let after = crate::insider_decision::Rollup::over(doubled.iter(), from, today);
    assert_eq!(before.discretionary.count, after.discretionary.count);
    assert_eq!(before.discretionary.usd, after.discretionary.usd);
    let people = crate::insider_decision::people(doubled.iter(), from, today);
    assert!(!people.iter().any(|p| p.name == "General Partner LP"));
}

/// The people mode stops fetching only while it has a chart bundle to list
/// people from; without one the head fetch that brings it still goes out.
#[test]
fn the_people_mode_fetches_a_missing_chart_bundle() {
    let (mut app, mut cmds) = crwv_app();
    let key = alphai::insider_key("CRWV");
    app.feeds.get_mut(&key).unwrap().side = None;
    let stale = std::time::Instant::now() - std::time::Duration::from_secs(3_600);
    app.feeds.get_mut(&key).unwrap().fetched = stale;
    press(&mut app, KeyCode::Char('x'));
    drain(&mut cmds);
    app.ensure_alphai_data();
    assert!(
        matches!(
            cmds.try_recv(),
            Ok(alphai::Cmd::FetchInsider { side: true, .. })
        ),
        "no head fetch for the missing chart bundle"
    );
}

/// A new key forgets which head fetches went out without the chart bundle:
/// their replies are dropped, and the marks would outlive them.
#[test]
fn a_new_key_forgets_skipped_chart_bundles() {
    let (mut app, _cmds) = crwv_app();
    app.insider_side_skipped.insert(alphai::insider_key("CRWV"));
    app.change_alphai_key(Some("ak_live_new".into()));
    assert!(app.insider_side_skipped.is_empty());
}
