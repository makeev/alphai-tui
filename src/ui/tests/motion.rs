use super::*;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

#[test]
fn ellipses_fit_cells_without_splitting_a_grapheme() {
    for (text, width, expected) in [
        ("你好世界", 5, "你好…"),
        ("e\u{301}clair", 3, "e\u{301}c…"),
        ("👩‍💻 report", 4, "👩‍💻 …"),
        ("ab界", 3, "ab…"),
        ("界界", 1, "…"),
    ] {
        let cut = ui::ellipsize(text, width);
        assert_eq!(cut, expected);
        assert!(cut.width() <= width);
    }
}

#[test]
fn animations_switch_off_live_and_survive_settings_save() {
    let mut app = fake_app();
    app.price_flash
        .insert("AAPL".into(), (Instant::now(), true));
    press(&mut app, KeyCode::Char('s'));
    app.settings.cursor = settings_rows()
        .iter()
        .position(|r| matches!(r, SettingsRow::Animations))
        .unwrap();
    press(&mut app, KeyCode::Right);
    assert!(!app.animations);
    assert_eq!(app.price_flash_dir("AAPL"), None);
    let base = Style::new().fg(app.theme.accent);
    assert_eq!(app.price_style("AAPL", base), base);
    let cfg = app.settings_merged_config();
    assert_eq!(cfg.ui.as_ref().unwrap().animations, Some(false));
    assert!(!crate::config::resolve(&cfg, None).0.ui.animations);
    press(&mut app, KeyCode::Right);
    assert!(app.animations);
    assert_eq!(
        app.settings_merged_config()
            .ui
            .unwrap_or_default()
            .animations,
        None
    );
}

#[test]
fn refresh_marker_tracks_work_and_ignores_an_old_source() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.animations = false;
    let source = app.price_source();
    assert_eq!(app.refresh_marker(), ' ');
    app.apply(SourceEvent::Refreshing {
        source: source.clone(),
        active: true,
    });
    assert_eq!(app.refresh_marker(), '*');
    let stale = make_source("yahoo", &Config::default()).unwrap();
    app.apply(SourceEvent::Refreshing {
        source: stale,
        active: false,
    });
    assert_eq!(app.refresh_marker(), '*');
    app.apply(SourceEvent::Refreshing {
        source,
        active: false,
    });
    assert_eq!(app.refresh_marker(), ' ');
}

#[test]
fn navigation_keeps_ticker_and_tab_styles_steady() {
    for preset in ["default", "dracula"] {
        let mut app = fake_app();
        app.theme = Theme::resolve(None, Some(preset), &mut Vec::new()).0;
        let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
        for key in [
            KeyCode::Tab,
            KeyCode::Char('4'),
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::BackTab,
        ] {
            press(&mut app, key);
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let ticker = buffer.cell((1, 1)).unwrap();
            assert_eq!(ticker.fg, app.theme.accent, "{preset}, {key:?}");
            assert_eq!(ticker.modifier, Modifier::BOLD, "{preset}, {key:?}");
            let tab: Vec<_> = (0..160)
                .map(|x| buffer.cell((x, 0)).unwrap())
                .filter(|cell| cell.bg == app.theme.accent)
                .collect();
            assert!(!tab.is_empty());
            assert!(
                tab.iter()
                    .all(|cell| { cell.fg == app.theme.accent_text && cell.modifier.is_empty() }),
                "{preset}, {key:?}: active tab changed emphasis"
            );
        }
        assert_eq!(app.selected_symbol(), "AAPL");
    }
}

#[test]
fn quote_rail_keeps_context_in_place_across_digit_and_countdown_changes() {
    let mut app = fake_app();
    let now = market_open_moment();
    let positions = |app: &App, now| {
        let line = ui::rail::text(&ui::rail::line(app, 240, now));
        ["quote", "timing varies", "● live", "├"].map(|s| {
            let index = line
                .find(s)
                .unwrap_or_else(|| panic!("missing {s}: {line}"));
            line[..index].width()
        })
    };
    let quote = &mut app.data.get_mut("AAPL").unwrap().quote;
    quote.price = 99.99;
    quote.prev_close = Some(99.0);
    let before = positions(&app, now);
    app.data.get_mut("AAPL").unwrap().quote.price = 100.01;
    assert_eq!(before, positions(&app, now));
    // 6h15m remaining -> 59m, with the same session and data.
    let later = now + chrono::Duration::minutes(316);
    assert_eq!(before, positions(&app, later));
}

#[test]
fn extended_price_age_and_holdings_do_not_shift_the_session_badge() {
    let mut app = premarket_holding();
    let now: chrono::DateTime<chrono::Utc> = "2026-09-11T10:30:00Z".parse().unwrap();
    let quote = &mut app.data.get_mut("CRWV").unwrap().quote;
    quote.timing.extended = Some((now - chrono::Duration::minutes(9)).timestamp());
    let session_column = |app: &App, at| {
        let line = ui::rail::text(&ui::rail::line(app, 300, at));
        let index = line.find("◐ pre").unwrap_or_else(|| panic!("{line}"));
        line[..index].width()
    };
    let before = session_column(&app, now);
    // The age gains a digit and the holding's gain crosses a thousands separator.
    app.data.get_mut("CRWV").unwrap().quote.extended = Some(95.28);
    assert_eq!(
        before,
        session_column(&app, now + chrono::Duration::minutes(1))
    );
}

#[test]
fn motion_and_sync_defaults_can_be_disabled_independently() {
    let defaults = crate::config::resolve(&Config::default(), None).0;
    assert!(defaults.ui.animations && defaults.ui.synchronized_output);
    for (animations, sync) in [(false, true), (true, false), (false, false)] {
        let text = format!("[ui]\nanimations = {animations}\nsynchronized_output = {sync}\n");
        let cfg: Config = toml::from_str(&text).unwrap();
        let (resolved, warnings) = crate::config::resolve(&cfg, None);
        assert!(warnings.is_empty());
        assert_eq!(resolved.ui.animations, animations);
        assert_eq!(resolved.ui.synchronized_output, sync);
    }
}

#[test]
fn all_settings_remain_reachable_in_small_panes() {
    let mut app = fake_app();
    press(&mut app, KeyCode::Char('s'));
    for (width, height) in [(80, 24), (60, 16), (40, 10)] {
        for (i, row) in settings_rows().iter().enumerate() {
            app.settings.cursor = i;
            let screen = render_sized(&mut app, width, height);
            assert!(
                screen.contains('▶'),
                "cursor lost at row {i}, {width}x{height}:\n{screen}"
            );
            if matches!(row, SettingsRow::Animations) {
                assert!(screen.contains("Animations"), "{screen}");
            }
            if matches!(row, SettingsRow::Save) {
                assert!(screen.contains("Save and close"), "{screen}");
            }
        }
    }
}
