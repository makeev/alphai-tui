//! The mouse acts on what the last frame drew: every test renders first,
//! then reads the recorded regions back and clicks or scrolls inside them.

use super::*;
use crate::ui::hits::{ListHit, ListKind};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn draw(app: &mut App, width: u16, height: u16) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
}

fn event(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

fn click(app: &mut App, x: u16, y: u16) -> bool {
    app.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), x, y))
}

fn wheel(app: &mut App, x: u16, y: u16, down: bool) -> bool {
    let kind = if down {
        MouseEventKind::ScrollDown
    } else {
        MouseEventKind::ScrollUp
    };
    app.handle_mouse(event(kind, x, y))
}

fn list(app: &App, kind: ListKind) -> ListHit {
    app.hits
        .lists
        .iter()
        .find(|l| l.kind == kind)
        .cloned()
        .unwrap_or_else(|| panic!("no {kind:?} list recorded: {:?}", app.hits.lists))
}

/// A frame with three articles in the AAPL feed and a page behind them.
fn news_app(view: ui::ViewId) -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (mut app, cmds) = empty_app_with_cmds(vec!["AAPL".into(), "MSFT".into()]);
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(view);
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![
                article("Apple beats expectations", "AAPL", 9, "positive"),
                article("A supplier walks away", "AAPL", 8, "negative"),
                article("Buyback extended", "AAPL", 7, "positive"),
            ],
            None,
            Some("cur1".into()),
        ),
    );
    (app, cmds)
}

#[test]
fn header_pills_switch_views_and_rail_peers_select_tickers() {
    let mut app = fake_app();
    draw(&mut app, 160, 40);
    assert_eq!(app.hits.pills.len(), ui::VIEWS.len());
    let news = ui::view_index(ui::ViewId::News);
    let (pill, idx) = app.hits.pills[news];
    assert_eq!(idx, news);
    assert!(click(&mut app, pill.x + 1, pill.y));
    assert_eq!(app.view_id(), ui::ViewId::News);

    // The rail lists the other tickers; MSFT is the only peer of AAPL.
    draw(&mut app, 160, 40);
    let (peer, idx) = app.hits.peers[0];
    assert_eq!(idx, 1);
    assert!(click(&mut app, peer.x + peer.width - 1, peer.y));
    assert_eq!(app.selected_symbol(), "MSFT");
    // Nothing else sits on the header or rail rows: a click there is inert.
    assert!(!click(&mut app, 0, 1));
    assert_eq!(app.selected_symbol(), "MSFT");
}

#[test]
fn watchlist_rows_take_clicks_and_the_wheel() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    draw(&mut app, 100, 30);
    let rows = list(&app, ListKind::Watchlist);
    assert_eq!(rows.first, 0);
    assert!(click(&mut app, rows.area.x + 3, rows.area.y + 1));
    assert_eq!(app.selected, 1);
    // Blank rows under a short list select nothing.
    assert!(!click(&mut app, rows.area.x + 3, rows.area.y + 5));
    assert_eq!(app.selected, 1);
    assert!(wheel(&mut app, rows.area.x, rows.area.y, false));
    assert_eq!(app.selected, 0);
    for _ in 0..3 {
        wheel(&mut app, rows.area.x, rows.area.y, true);
    }
    assert_eq!(app.selected, 1, "the wheel stops at the last ticker");
    // The chart beside it is not a list: the wheel there does nothing, and
    // in particular never changes the preset.
    let (range, interval) = (app.range, app.interval);
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    draw(&mut app, 100, 30);
    assert!(!wheel(&mut app, 50, 15, true));
    assert_eq!((app.range, app.interval), (range, interval));
}

#[test]
fn feed_rows_select_then_open_and_the_wheel_never_pages() {
    let (mut app, mut cmds) = news_app(ui::ViewId::News);
    draw(&mut app, 160, 40);
    while cmds.try_recv().is_ok() {}
    let rows = list(&app, ListKind::Feed);
    assert!(click(&mut app, rows.area.x + 4, rows.area.y + 2));
    assert_eq!(app.news_selected, 2);
    assert!(!app.article_overlay.open);
    // The selected row again opens it, the way v does.
    assert!(click(&mut app, rows.area.x + 4, rows.area.y + 2));
    assert!(app.article_overlay.open);
    press(&mut app, KeyCode::Esc);
    assert!(!app.article_overlay.open);
    // Past the loaded rows: nothing selected, nothing fetched.
    assert!(!click(&mut app, rows.area.x + 4, rows.area.y + 3));
    assert_eq!(app.news_selected, 2);
    assert!(!wheel(&mut app, rows.area.x, rows.area.y, true));
    assert_eq!(app.news_selected, 2);
    assert!(cmds.try_recv().is_err(), "the wheel paged the feed");
    assert!(wheel(&mut app, rows.area.x, rows.area.y, false));
    assert_eq!(app.news_selected, 1);

    // The card pane beside the list scrolls by lines, not by rows.
    let (card, _) = app
        .hits
        .scrolls
        .iter()
        .find(|(_, s)| *s == crate::ui::hits::Scroll::Card)
        .copied()
        .expect("card pane recorded");
    assert!(wheel(&mut app, card.x + 2, card.y + 2, true));
    assert_eq!(app.card_scroll, 3);
    assert_eq!(app.news_selected, 1, "the card's wheel moved the list");
}

#[test]
fn split_strip_click_opens_the_news_view_on_that_row() {
    let (mut app, mut cmds) = news_app(ui::ViewId::Split);
    draw(&mut app, 160, 40);
    while cmds.try_recv().is_ok() {}
    let strip = list(&app, ListKind::NewsStrip);
    // The strip has no cursor, so the wheel has nothing to move there.
    assert!(!wheel(&mut app, strip.area.x, strip.area.y, true));
    assert!(click(&mut app, strip.area.x + 4, strip.area.y + 1));
    assert_eq!(app.view_id(), ui::ViewId::News);
    assert_eq!(app.news_selected, 1);
    assert!(cmds.try_recv().is_err(), "a click requested a feed");
}

#[test]
fn summary_cards_and_portfolio_rows_select_on_click() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Summary);
    draw(&mut app, 120, 40);
    let (card, idx) = app.hits.cards[1];
    assert_eq!(idx, 1);
    assert!(click(&mut app, card.x + 2, card.y + 2));
    assert_eq!(app.selected, 1);
    assert!(wheel(&mut app, card.x + 2, card.y + 2, false));
    assert_eq!(app.selected, 0);

    app.positions = vec![
        Position {
            symbol: "MSFT".into(),
            qty: 2.0,
            avg_price: 300.0,
        },
        Position {
            symbol: "AAPL".into(),
            qty: 5.0,
            avg_price: 150.0,
        },
    ];
    app.view_idx = ui::view_index(ui::ViewId::Portfolio);
    draw(&mut app, 120, 40);
    let rows = list(&app, ListKind::Portfolio);
    assert!(click(&mut app, rows.area.x + 3, rows.area.y + 1));
    assert_eq!(app.portfolio_selected, 1);
    assert_eq!(app.selected_symbol(), "AAPL", "the rail follows the row");
    // The totals row is not a holding.
    assert!(!click(&mut app, rows.area.x + 3, rows.area.y + 2));
    assert_eq!(app.portfolio_selected, 1);
    assert!(wheel(&mut app, rows.area.x, rows.area.y, false));
    assert_eq!(app.portfolio_selected, 0);
    assert_eq!(app.selected_symbol(), "MSFT");
}

#[test]
fn reading_overlays_scroll_with_the_wheel_and_close_on_a_click_outside() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('?'));
    draw(&mut app, 100, 30);
    let (modal, _) = app.hits.modal.expect("help recorded");
    assert!(
        wheel(&mut app, 0, 0, true),
        "the overlay takes the wheel anywhere"
    );
    assert_eq!(app.help.scroll, 3);
    assert!(!click(&mut app, modal.x + 2, modal.y + 2));
    assert!(app.help.open, "a click inside is not a close");
    assert!(click(&mut app, 0, 0));
    assert!(!app.help.open);

    let (mut app, _cmds) = news_app(ui::ViewId::News);
    press(&mut app, KeyCode::Char('v'));
    draw(&mut app, 160, 40);
    assert!(app.article_overlay.open);
    assert!(wheel(&mut app, 80, 20, true));
    assert_eq!(app.article_overlay.scroll, 3);
    let (modal, _) = app.hits.modal.expect("article recorded");
    assert!(click(&mut app, modal.x, modal.y.saturating_sub(1)));
    assert!(!app.article_overlay.open);
}

#[test]
fn forms_keep_their_text_and_pick_rows_on_click() {
    // The add-ticker prompt ignores the mouse altogether: a click on the
    // terminal to focus it must not throw the line away.
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Char('N'));
    draw(&mut app, 100, 30);
    assert!(!click(&mut app, 0, 0));
    assert!(!wheel(&mut app, 50, 15, true));
    assert!(app.prompt.open);
    assert_eq!(app.prompt.input, "N");
    press(&mut app, KeyCode::Esc);

    // The ticker search: a click on a result selects it.
    press(&mut app, KeyCode::Char('/'));
    draw(&mut app, 100, 30);
    let rows = list(&app, ListKind::TickerSearch);
    assert!(
        !click(&mut app, 0, 0),
        "outside the box: nothing, not a cancel"
    );
    assert!(app.ticker_search.open);
    assert!(click(&mut app, rows.area.x + 2, rows.area.y + 1));
    assert!(!app.ticker_search.open);
    assert_eq!(app.selected_symbol(), "MSFT");

    // The category picker: a click on a row moves the cursor there and
    // toggles it, like space; the wheel only moves.
    let (mut app, _cmds) = news_app(ui::ViewId::News);
    press(&mut app, KeyCode::Char('C'));
    draw(&mut app, 100, 30);
    let rows = list(&app, ListKind::Categories);
    assert!(click(&mut app, rows.area.x + 2, rows.area.y + 3));
    assert_eq!(app.category_picker.cursor, 3);
    assert!(!app.category_picker.draft.shows(3));
    assert!(wheel(&mut app, 0, 0, true));
    assert_eq!(app.category_picker.cursor, 4);
    assert!(!app.category_picker.draft.shows(3));
    assert!(app.category_picker.draft.shows(4));

    // Settings: the wheel walks the rows, a click changes nothing.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('s'));
    draw(&mut app, 100, 40);
    assert!(wheel(&mut app, 50, 20, true));
    assert_eq!(app.settings.cursor, 1);
    assert!(!click(&mut app, 0, 0));
    assert!(app.settings.open);
}

#[test]
fn the_mouse_key_and_the_settings_row_hand_the_mouse_back() {
    let mut app = fake_app();
    assert!(app.mouse);
    press(&mut app, KeyCode::Char('M'));
    assert!(!app.mouse);
    assert!(app.notice().is_some_and(|n| n.contains("Mouse off")));
    draw(&mut app, 100, 30);
    let (pill, _) = app.hits.pills[ui::view_index(ui::ViewId::News)];
    assert!(!click(&mut app, pill.x, pill.y), "off means off");
    assert_eq!(app.view_id(), ui::ViewId::Split);
    press(&mut app, KeyCode::Char('M'));
    assert!(app.mouse);

    press(&mut app, KeyCode::Char('s'));
    app.settings.cursor = settings_rows()
        .iter()
        .position(|r| matches!(r, SettingsRow::Mouse))
        .unwrap();
    press(&mut app, KeyCode::Right);
    assert!(!app.mouse, "the row applies live");
    let cfg = app.settings_merged_config();
    assert_eq!(cfg.ui.as_ref().and_then(|ui| ui.mouse), Some(false));
    press(&mut app, KeyCode::Left);
    assert!(app.mouse);
    let cfg = app.settings_merged_config();
    assert_eq!(
        cfg.ui.as_ref().and_then(|ui| ui.mouse),
        None,
        "on is the default"
    );
    let screen = render_sized(&mut app, 100, 40);
    assert!(screen.contains("Mouse"), "screen:\n{screen}");
}
