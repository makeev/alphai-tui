//! Keys on plates: in the footer, and beside the choices in the panel
//! titles they cycle. Every test renders, then clicks what the frame
//! recorded, the way the mouse would.

use super::*;
use crate::app::{InsiderChartWindow, NewsLayout, NewsScope};
use crate::keymap::Action;
use crate::ui::hits::Button;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

fn draw(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    buf.content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn button(app: &App, want: Button) -> Rect {
    app.hits
        .buttons
        .iter()
        .rev()
        .find(|(_, b)| *b == want)
        .map(|(r, _)| *r)
        .unwrap_or_else(|| panic!("no {want:?} in {:?}", app.hits.buttons))
}

fn press(app: &mut App, want: Button) {
    let r = button(app, want);
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: r.x + r.width / 2,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    };
    assert!(app.handle_mouse(click), "{want:?} did nothing");
}

/// A capital key is the letter with shift; a plate saying "A" would read
/// as the plain key, since keycaps print capitals.
#[test]
fn a_capital_key_names_its_shift() {
    assert_eq!(ui::keycap::label("A"), "⇧A");
    assert_eq!(ui::keycap::label("x"), "x");
    assert_eq!(ui::keycap::label("pgup/pgdn"), "pgup/pgdn");
    assert_eq!(ui::keycap::label("+-"), "+-");
}

/// The footer draws each key on its plate, and a click on a hint presses
/// its key; quit is never a click away.
#[test]
fn footer_hints_are_keycaps_a_click_presses() {
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
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let theme = app.theme;
    let footer_y = 39;
    let plate = &buf[(1, footer_y)];
    assert_eq!(plate.symbol(), "q");
    assert!(
        plate.modifier.contains(Modifier::REVERSED | Modifier::BOLD),
        "{plate:?}"
    );
    assert_eq!(
        buf[(5, footer_y)].fg,
        theme.subtle().fg.unwrap_or(Color::Reset)
    );
    assert!(
        !app.hits
            .buttons
            .iter()
            .any(|(_, b)| *b == Button::Key(Action::Quit)),
        "quit is clickable"
    );
    press(&mut app, Button::Key(Action::Card));
    assert!(app.article_overlay.open, "v card did not open the card");
}

/// The chart's title lists the presets behind their key and the session
/// switch behind its own; the bottom border carries the chart's switches,
/// each lit while on. A click works like the key.
#[test]
fn the_chart_title_and_toolbar_take_clicks() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    let screen = draw(&mut app, 160, 40);
    let title = screen.lines().find(|l| l.contains("╭ AAPL")).unwrap();
    assert!(title.contains(" t   1d/5m  5d  1mo  6mo  1y "), "{title}");
    assert!(title.contains("⇧E   ext "), "{title}");
    let bottom = screen
        .lines()
        .find(|l| l.contains("candles"))
        .unwrap_or_else(|| panic!("{screen}"));
    for want in [
        " c  candles",
        " m  MA",
        " e  SMA",
        " i  RSI",
        " b  vol",
        " n  news",
    ] {
        assert!(bottom.contains(want), "{want:?} missing: {bottom}");
    }

    press(&mut app, Button::Preset(2));
    assert_eq!((app.range, app.interval), app.chart.presets[2]);
    draw(&mut app, 160, 40);
    let before = app.sessions;
    press(&mut app, Button::Key(Action::ToggleExtended));
    assert_ne!(app.sessions, before);
    draw(&mut app, 160, 40);
    let rsi = app.show_rsi;
    press(&mut app, Button::Key(Action::ToggleRsi));
    assert_ne!(app.show_rsi, rsi);
    draw(&mut app, 160, 40);
    press(&mut app, Button::Key(Action::ChartStyle));
    assert_eq!(app.chart_style, crate::app::ChartStyle::Line);
}

/// Where the chart's keys do not act (the News view's chart), no toolbar.
#[test]
fn the_news_chart_has_no_toolbar() {
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
    draw(&mut app, 160, 40);
    assert!(
        !app.hits
            .buttons
            .iter()
            .any(|(_, b)| *b == Button::Key(Action::ToggleRsi)),
        "{:?}",
        app.hits.buttons
    );
    // The preset still works there, as its key does.
    press(&mut app, Button::Preset(1));
    assert_eq!((app.range, app.interval), app.chart.presets[1]);
}

/// The News title shows the four scopes and the three layouts as pills
/// behind their keys; a click picks one, the categories' key opens the
/// picker.
#[test]
fn news_scopes_and_layouts_are_pills() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::News);
    // The chart layout gives the list the full width the whole row needs.
    app.news_layout = NewsLayout::Chart;
    app.feeds.insert(
        "AAPL".into(),
        FeedBundle::new(
            vec![article("Apple beats expectations", "AAPL", 9, "positive")],
            None,
            None,
        ),
    );
    let screen = draw(&mut app, 240, 40);
    let title = screen.lines().find(|l| l.contains("╭ News ")).unwrap();
    assert!(
        title.contains(" f   AAPL  events  market  trending "),
        "{title}"
    );
    assert!(title.contains(" x   chart  stacked  side "), "{title}");
    press(&mut app, Button::NewsScope(NewsScope::Market));
    assert_eq!(app.news_scope, NewsScope::Market);
    draw(&mut app, 240, 40);
    press(&mut app, Button::NewsLayout(NewsLayout::Side));
    assert_eq!(app.news_layout, NewsLayout::Side);
    draw(&mut app, 240, 40);
    press(&mut app, Button::Key(Action::Categories));
    assert!(app.category_picker.open);
}

/// The Calendar filter and the Form 4 chart's window are pills too.
#[test]
fn calendar_filter_and_insider_window_are_pills() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Calendar);
    let screen = draw(&mut app, 160, 40);
    assert!(screen.contains(" f   all  reports  macro "), "{screen}");
    press(
        &mut app,
        Button::CalendarFilter(ui::calendar::Filter::Macro),
    );
    assert_eq!(app.calendar_filter, ui::calendar::Filter::Macro);

    let mut app = insider_app_with_chart();
    draw(&mut app, 160, 40);
    press(&mut app, Button::InsiderWindow(InsiderChartWindow::M12));
    assert_eq!(app.insider_chart, InsiderChartWindow::M12);
}

/// The watchlist names its keys in the corner; adding and finding are a
/// click away, removing a saved ticker stays a key.
#[test]
fn the_watchlist_names_its_keys_but_remove_takes_the_key() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Table);
    let screen = draw(&mut app, 160, 40);
    let title = screen.lines().find(|l| l.contains("Watchlist")).unwrap();
    assert!(title.contains(" a  add  d  remove  /  find "), "{title}");
    assert!(
        !app.hits
            .buttons
            .iter()
            .any(|(_, b)| *b == Button::Key(Action::RemoveTicker)),
        "remove is clickable"
    );
    press(&mut app, Button::Key(Action::AddTicker));
    assert!(app.prompt.open);
}

/// Earnings steps through the watchlist from its title.
#[test]
fn earnings_steps_tickers_from_its_title() {
    let (mut app, _cmds) = earnings_app(FULL_METRICS, "2026-11-17");
    let screen = draw(&mut app, 110, 32);
    assert!(screen.contains("Earnings  ←  NVDA  →"), "{screen}");
    press(&mut app, Button::Key(Action::Right));
    assert_eq!(app.earnings_symbol(), "AVGO");
}

/// The header names the keys to the settings and the help beside the
/// connection status, and a click opens either; narrow, the plates go
/// bare before the status loses them.
#[test]
fn the_header_names_settings_and_help() {
    let mut app = fake_app();
    let screen = draw(&mut app, 160, 40);
    let header = screen.lines().next().unwrap();
    assert!(
        header.trim_end().ends_with("s  settings  ?  help"),
        "{header}"
    );
    press(&mut app, Button::Key(Action::Help));
    assert!(app.help.open);
    app.help.open = false;
    draw(&mut app, 160, 40);
    press(&mut app, Button::Key(Action::Settings));
    assert!(app.settings.open);

    let mut app = fake_app();
    let screen = draw(&mut app, 80, 24);
    let header = screen.lines().next().unwrap();
    assert!(header.trim_end().ends_with("s   ?"), "{header}");
}

/// Without a key the status no longer says "(s)": the plate beside it
/// does.
#[test]
fn the_header_points_a_keyless_reader_at_the_plate() {
    let mut app = fake_app();
    app.alphai_enabled = false;
    let screen = draw(&mut app, 160, 40);
    let header = screen.lines().next().unwrap();
    assert!(header.contains("alphai: no key  s  settings"), "{header}");
    assert!(!header.contains("(s)"), "{header}");
}

/// The chart's corner names the kinds of news mark it draws, in the marks'
/// own accent, and only those; with the marks off the key goes too.
#[test]
fn the_chart_legend_names_the_marks_on_screen() {
    let mut app = fake_app();
    app.view_idx = ui::view_index(ui::ViewId::Chart);
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
    let screen = draw(&mut app, 160, 40);
    let row = screen
        .lines()
        .find(|l| l.contains("Last bar"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(row.trim_end().ends_with("▲ bullish  ▼ bearish │"), "{row}");
    assert!(!row.contains("neutral"), "{row}");

    app.show_news_markers = false;
    let screen = draw(&mut app, 160, 40);
    assert!(!screen.contains("bullish"), "{screen}");

    // The line chart draws every mark as a dot, and says so.
    app.show_news_markers = true;
    app.chart_style = crate::app::ChartStyle::Line;
    let screen = draw(&mut app, 160, 40);
    assert!(screen.contains("• news"), "{screen}");
}

/// The pre-market shade is named in the corner with a swatch of its own
/// color, when the window shows it.
#[test]
fn the_chart_legend_names_the_session_shade() {
    use crate::domain::PriceFeed;
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Chart);
    app.sessions = Sessions::Extended;
    app.interval = Interval::M5;
    app.show_news_markers = false;
    // 04:00 to 06:00 ET: pre-market only.
    let first = chrono::DateTime::parse_from_rfc3339("2026-09-14T08:00:00Z")
        .unwrap()
        .timestamp();
    let candles: Vec<_> = (0..24)
        .map(|i| Candle {
            ts: first + i * 300,
            open: 100.0,
            high: 100.3,
            low: 99.8,
            close: 100.2,
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
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let screen = buf
        .content()
        .chunks(160)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>();
    let (y, row) = screen
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains(" pre "))
        .unwrap_or_else(|| panic!("{}", screen.join("\n")));
    assert!(!row.contains(" post "), "{row}");
    let x = row[..row.find(" pre ").unwrap()].chars().count() as u16;
    assert_eq!(buf[(x - 1, y as u16)].bg, app.theme.pre_market_bg, "{row}");
}
