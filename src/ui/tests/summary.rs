//! The Summary view: the day's line over three layouts (cards, heat map,
//! comparison), its order, and the keys and clicks that pick them.

use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::*;
use crate::app::{SummaryLayout, SummaryOrder};
use crate::theme::{PRESETS, Panels};
use crate::ui::hits::Button;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn summary_app() -> App {
    let mut app = fake_app();
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(ui::ViewId::Summary);
    // MSFT down on the day, AAPL up: one of each for the breadth and order.
    app.data.get_mut("MSFT").unwrap().quote.prev_close = Some(430.0);
    app
}

fn buffer(app: &mut App, w: u16, h: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn text(buf: &Buffer) -> String {
    buf.content()
        .chunks(usize::from(buf.area.width))
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn click(app: &mut App, want: Button) {
    let r = app
        .hits
        .buttons
        .iter()
        .find(|(_, b)| *b == want)
        .map(|(r, _)| *r)
        .unwrap_or_else(|| panic!("no {want:?} in {:?}", app.hits.buttons));
    assert!(app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: r.x + r.width / 2,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    }));
}

/// The line over the layout: the session, the breadth and the average
/// move, and the layout and order as pills a click picks.
#[test]
fn the_day_line_sums_up_the_watchlist() {
    let mut app = summary_app();
    let screen = text(&buffer(&mut app, 160, 40));
    let line = screen.lines().nth(2).unwrap();
    assert!(line.contains("▲ 1  ▼ 1"), "{line}");
    assert!(line.contains("avg "), "{line}");
    assert!(line.contains(" x   cards  heatmap  compare "), "{line}");
    assert!(line.contains(" f   list  change "), "{line}");

    click(&mut app, Button::SummaryLayout(SummaryLayout::Heatmap));
    assert_eq!(app.summary_layout, SummaryLayout::Heatmap);
    buffer(&mut app, 160, 40);
    click(&mut app, Button::SummaryOrder(SummaryOrder::Change));
    assert_eq!(app.summary_order, SummaryOrder::Change);
}

/// The portfolio's day joins the line when there are positions.
#[test]
fn the_day_line_counts_the_portfolio() {
    let mut app = summary_app();
    app.positions = vec![crate::portfolio::Position {
        symbol: "AAPL".into(),
        qty: 10.0,
        avg_price: 150.0,
    }];
    let screen = text(&buffer(&mut app, 160, 40));
    let line = screen.lines().nth(2).unwrap();
    assert!(line.contains("portfolio today +145.00"), "{line}");
}

/// A card: the quote across the top, the path with the previous close
/// dashed, the window's move and the day's range under it, the position
/// and the alert where they fit.
#[test]
fn a_card_says_more_than_a_line() {
    let mut app = summary_app();
    app.positions = vec![crate::portfolio::Position {
        symbol: "AAPL".into(),
        qty: 10.0,
        avg_price: 150.0,
    }];
    let level = crate::alerts::Level::parse(">= 230").unwrap();
    app.alerts.arm("AAPL".into(), level, None);
    let screen = text(&buffer(&mut app, 160, 40));
    assert!(
        screen.contains("▶ AAPL  214.50  ▲ +14.50  +7.25%"),
        "{screen}"
    );
    assert!(
        screen.contains("MSFT  414.50  ▼ -15.50  -3.60%"),
        "{screen}"
    );
    // The card's own line of facts, not the rail's.
    let facts = screen
        .lines()
        .find(|l| l.contains("×10 +645.00 +43.0%"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(facts.contains("alert"), "{facts}");
    // MSFT's previous close sits above its path: a dashed line.
    assert!(screen.contains('┄'), "{screen}");
}

/// Ordered by the day's move, the biggest gain comes first and the cursor
/// walks the tickers in that order.
#[test]
fn the_change_order_ranks_and_the_cursor_follows() {
    let mut app = summary_app();
    app.symbols = vec!["MSFT".into(), "AAPL".into()];
    app.selected = 0;
    assert_eq!(ui::summary::order(&app), vec![0, 1]);
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.summary_order, SummaryOrder::Change);
    assert_eq!(
        ui::summary::order(&app),
        vec![1, 0],
        "AAPL is up, MSFT down"
    );
    // MSFT (index 0) is last in this order: ↑ moves to AAPL.
    press(&mut app, KeyCode::Up);
    assert_eq!(app.selected, 1);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.selected, 1, "already first");
}

/// x cycles the three layouts.
#[test]
fn x_cycles_the_layouts() {
    let mut app = summary_app();
    for want in [
        SummaryLayout::Heatmap,
        SummaryLayout::Compare,
        SummaryLayout::Cards,
    ] {
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.summary_layout, want);
    }
}

/// More tickers than cards fit: one line each, with a sparkline.
#[test]
fn a_long_watchlist_gets_one_line_each() {
    let mut app = summary_app();
    let data = app.data["AAPL"].clone();
    for i in 0..12 {
        let symbol = format!("T{i:02}");
        app.symbols.push(symbol.clone());
        app.data.insert(symbol, data.clone());
    }
    let screen = text(&buffer(&mut app, 80, 24));
    assert!(screen.contains("Watchlist · 14 tickers"), "{screen}");
    let row = screen
        .lines()
        .find(|l| l.contains("▶ AAPL"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(row.chars().any(|c| ('▁'..='█').contains(&c)), "{row}");
}

/// The heat map tints a tile toward the day's direction, harder for a
/// bigger move, and prints the move in big figures where the tile is tall.
#[test]
fn the_heat_map_tints_by_the_move() {
    let mut app = summary_app();
    app.theme = PRESETS
        .iter()
        .find(|(n, _)| *n == "dracula")
        .map(|(_, t)| *t)
        .unwrap();
    app.summary_layout = SummaryLayout::Heatmap;
    let buf = buffer(&mut app, 160, 40);
    let screen = text(&buf);
    // Big figures for +7.25%: the plus sign's stroke.
    assert!(screen.contains("▄█▄"), "{screen}");
    let tile_bg = |name: &str| {
        let (y, line) = screen
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains(&format!("╭ {name}")) || l.contains(&format!("╭ ▶ {name}")))
            .unwrap();
        let x = line[..line.find(name).unwrap()].chars().count() as u16;
        buf[(x + 2, y as u16 + 2)].bg
    };
    let (aapl, msft) = (tile_bg("AAPL"), tile_bg("MSFT"));
    let (Color::Rgb(ar, ag, _), Color::Rgb(mr, mg, _)) = (aapl, msft) else {
        panic!("tiles not tinted: {aapl:?} {msft:?}");
    };
    assert!(ag > ar, "AAPL is up, its tile leans green: {aapl:?}");
    assert!(mr > mg, "MSFT is down, its tile leans red: {msft:?}");
}

/// The comparison starts every line at the same moment, labels each at
/// its end, best on top, the selected one marked.
#[test]
fn the_comparison_labels_every_line() {
    let mut app = summary_app();
    app.summary_layout = SummaryLayout::Compare;
    // MSFT's history starts later: AAPL is measured from the same bar.
    app.data.get_mut("MSFT").unwrap().candles.drain(..10);
    let screen = text(&buffer(&mut app, 160, 40));
    let aapl = screen.lines().position(|l| l.contains("▶ AAPL")).unwrap();
    let msft = screen.lines().position(|l| l.contains("━ MSFT")).unwrap();
    assert!(aapl <= msft, "{screen}");
    // AAPL from candle 10 (205.0) to 214.5: +4.6%, not +7.3%.
    assert!(screen.contains("▶ AAPL +4.6%"), "{screen}");
    assert!(screen.contains("since the window's start"), "{screen}");
}

/// Every layout paints its own colors on every preset.
#[test]
fn every_layout_paints_on_every_preset() {
    let mut app = summary_app();
    for (name, theme) in PRESETS.iter().skip(1) {
        app.theme = *theme;
        for panels in Panels::ALL {
            app.theme.panels = panels;
            for layout in SummaryLayout::ALL {
                app.summary_layout = layout;
                for (w, h) in [(160, 40), (80, 24)] {
                    let buf = buffer(&mut app, w, h);
                    super::themes::assert_explicit_colors(
                        &buf,
                        &format!("{name}, {panels:?}, {layout:?}, {w}x{h}"),
                    );
                }
            }
        }
    }
}

/// On the ANSI theme a weak move leaves the tile clear with colored
/// figures; a strong one takes the direction's color with dark figures.
#[test]
fn the_ansi_heat_map_has_two_steps() {
    let mut app = summary_app();
    app.summary_layout = SummaryLayout::Heatmap;
    let buf = buffer(&mut app, 160, 40);
    let screen = text(&buf);
    let (y, line) = screen
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("╭ ▶ AAPL"))
        .unwrap();
    let x = line[..line.find("AAPL").unwrap()].chars().count() as u16;
    // +7.25% is a strong move.
    assert_eq!(buf[(x + 2, y as u16 + 2)].bg, app.theme.up);
    // -3.6% too, past the 3% step.
    let (y, line) = screen
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("╭ MSFT"))
        .unwrap();
    let x = line[..line.find("MSFT").unwrap()].chars().count() as u16;
    assert_eq!(buf[(x + 2, y as u16 + 2)].bg, app.theme.down);
    // A weak move: the tile stays clear.
    app.data.get_mut("MSFT").unwrap().quote.prev_close = Some(416.0);
    let buf = buffer(&mut app, 160, 40);
    assert_ne!(buf[(x + 2, y as u16 + 2)].bg, app.theme.down);
}

/// The figures for a percent move draw in big text.
#[test]
fn big_figures_spell_a_percent_move() {
    assert!(ui::bigtext::lines("+7.25%").is_some());
    assert!(ui::bigtext::lines("-9.92%").is_some());
}

/// `[ui] summary_layout` starts the view; an unknown name warns and keeps
/// the cards.
#[test]
fn the_layout_starts_from_the_config() {
    let cfg: crate::config::Config = toml::from_str("[ui]\nsummary_layout = \"compare\"").unwrap();
    let (resolved, warnings) = crate::config::resolve(&cfg, None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(resolved.ui.summary_layout, SummaryLayout::Compare);
    let cfg: crate::config::Config = toml::from_str("[ui]\nsummary_layout = \"grid\"").unwrap();
    let (resolved, warnings) = crate::config::resolve(&cfg, None);
    assert_eq!(resolved.ui.summary_layout, SummaryLayout::Cards);
    assert!(
        warnings.iter().any(|w| w.contains("summary_layout")),
        "{warnings:?}"
    );
}

/// Big figures go on every heat tile or on none: the layout's rounding
/// leaves one tile a column wider, and a lone big move among plain ones
/// reads as the one that matters.
#[test]
fn heat_tiles_share_one_figure_size() {
    let mut app = summary_app();
    app.summary_layout = SummaryLayout::Heatmap;
    for w in 50..64 {
        let screen = text(&buffer(&mut app, w, 24));
        // Below the header, the rail and the day's line.
        let body: String = screen.lines().skip(3).collect::<Vec<_>>().join("\n");
        assert_eq!(
            body.contains("+7.25%"),
            body.contains("-3.60%"),
            "{w} columns: one tile big, one plain:\n{body}"
        );
    }
}
