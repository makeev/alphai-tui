use super::*;
use crate::theme::{PRESETS, Panels};
use ratatui::buffer::Buffer;
use ratatui::style::Color;

fn palette_app() -> App {
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
    app
}

/// A preset must render with the same colors on any terminal. Even one
/// reset background can expose a white hole in a dark theme; reset text
/// can turn dark terminal text invisible on a dark selection or modal.
fn assert_explicit_colors(buffer: &Buffer, context: &str) {
    for y in buffer.area.top()..buffer.area.bottom() {
        for x in buffer.area.left()..buffer.area.right() {
            let cell = &buffer[(x, y)];
            assert!(
                matches!(cell.bg, Color::Rgb(..)),
                "{context}: terminal background at {x},{y} ({:?})",
                cell.symbol()
            );
            assert!(
                matches!(cell.fg, Color::Rgb(..)),
                "{context}: terminal foreground at {x},{y} ({:?})",
                cell.symbol()
            );
        }
    }
}

#[test]
fn named_presets_cover_every_view_and_panel_style() {
    let mut app = palette_app();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    for (name, theme) in PRESETS.iter().skip(1) {
        app.theme = *theme;
        for panels in Panels::ALL {
            app.theme.panels = panels;
            for view in ui::VIEWS {
                app.view_idx = ui::view_index(view.id());
                terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
                let context = format!("{name}, {panels:?}, {:?}", view.id());
                let buffer = terminal.backend().buffer();
                assert_explicit_colors(buffer, &context);
                assert_eq!(buffer[(0, 0)].bg, theme.background, "{context}: chrome");
                assert_eq!(buffer[(0, 5)].bg, theme.background, "{context}: gutter");
            }
        }
    }
}

#[test]
fn overlays_keep_the_palette_after_clearing_their_area() {
    for name in ["catppuccin-mocha", "catppuccin-latte", "gruvbox-light"] {
        for panels in Panels::ALL {
            for key in ['?', 's', 'a', 'A', '/', 'C', 'v'] {
                let mut app = palette_app();
                app.set_theme(name);
                app.theme.panels = panels;
                app.view_idx = ui::view_index(ui::ViewId::News);
                app.news_layout = NewsLayout::Side;
                press(&mut app, KeyCode::Char(key));
                let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
                terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
                let context = format!("{name}, {panels:?}, {key}");
                assert!(app.hits.modal.is_some(), "{context}: no overlay rendered");
                assert_explicit_colors(terminal.backend().buffer(), &context);
                // The compact price alert has no block to restore its palette.
                if key == 'A' {
                    let mut terminal = Terminal::new(TestBackend::new(60, 4)).unwrap();
                    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
                    assert_explicit_colors(terminal.backend().buffer(), &context);
                }
            }
        }
    }
}

#[test]
fn switching_back_to_default_restores_terminal_colors() {
    let mut app = palette_app();
    app.chart.session_shading = false;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let original = terminal.backend().buffer().clone();
    for name in ["catppuccin-mocha", "catppuccin-latte", "default"] {
        app.set_theme(name);
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    }
    let buffer = terminal.backend().buffer();
    for (before, after) in original.content.iter().zip(&buffer.content) {
        assert_eq!((after.fg, after.bg), (before.fg, before.bg));
    }
    assert_eq!(buffer[(0, 5)].bg, Color::Reset);
}

#[test]
fn explicit_canvas_overrides_apply_to_chrome_panels_and_overlays() {
    let mut app = palette_app();
    app.config.theme = Some(std::collections::BTreeMap::from([
        ("background".into(), "#102030".into()),
        ("text".into(), "#d0e0f0".into()),
    ]));
    app.set_theme("catppuccin-mocha");
    press(&mut app, KeyCode::Char('a'));
    app.prompt.input = "NVDA".into();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let bg = Color::Rgb(0x10, 0x20, 0x30);
    let fg = Color::Rgb(0xd0, 0xe0, 0xf0);
    assert_eq!(buffer[(0, 0)].bg, bg);
    assert_eq!(buffer[(0, 5)].bg, bg);
    let (area, _) = app.hits.modal.unwrap();
    let input = &buffer[(area.x + 2, area.y + 1)];
    assert_eq!(input.symbol(), "N");
    assert_eq!((input.fg, input.bg), (fg, bg));
}
