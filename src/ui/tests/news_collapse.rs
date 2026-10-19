use super::*;

fn grouping_row(app: &mut App) {
    app.settings.cursor = settings_rows()
        .iter()
        .position(|row| matches!(row, SettingsRow::CollapseNews))
        .unwrap();
}

/// Exercise the real settings save and reload, without touching user config.
fn save_grouping(app: &mut App, collapse: bool) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "alphai-grouping-{}-{}.toml",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    app.config_path = Some(path.clone());
    app.open_settings();
    grouping_row(app);
    if app.settings.collapse_news_choice != collapse {
        press(app, KeyCode::Char(' '));
    }
    app.settings.cursor = settings_rows().len() - 1;
    press(app, KeyCode::Enter);
    assert!(!app.settings.open, "{:?}", app.settings.message);
    let cfg = crate::config::load_from(&path).unwrap().unwrap();
    let (resolved, warnings) = crate::config::resolve(&cfg, None);
    assert!(warnings.is_empty());
    assert_eq!(resolved.ui.collapse_news, collapse);
    assert_eq!(app.collapse_news, collapse);
    std::fs::remove_file(path).unwrap();
    app.config_path = None;
}

#[test]
fn grouping_defaults_on_and_settings_only_apply_on_save() {
    let mut app = empty_app(vec!["AAPL".into()]);
    assert!(app.collapse_news);
    app.open_settings();
    grouping_row(&mut app);
    let screen = render(&mut app);
    assert!(screen.contains("Collapse news"), "{screen}");
    assert!(screen.contains("‹ on ›"), "{screen}");
    press(&mut app, KeyCode::Enter);
    assert!(!app.settings.collapse_news_choice);
    assert!(app.collapse_news, "draft must not change the feed");
    press(&mut app, KeyCode::Esc);
    app.open_settings();
    assert!(app.settings.collapse_news_choice, "Esc discarded the draft");
    save_grouping(&mut app, false);
    assert_eq!(app.config.ui.as_ref().unwrap().collapse_news, Some(false));
    save_grouping(&mut app, true);
    assert_eq!(app.config.ui.as_ref().and_then(|ui| ui.collapse_news), None);
}

#[test]
fn changing_grouping_clears_only_news_pages_and_cursors() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
    cache_empty_calendar(&mut app);
    app.view_idx = ui::view_index(ui::ViewId::News);
    for key in [
        "AAPL",
        "BINANCE:BTCUSDT",
        alphai::MARKET_KEY,
        TRENDING_KEY,
        "ins:AAPL",
    ] {
        let mut bundle = FeedBundle::new(vec![uid_article("one", "One")], None, Some("old".into()));
        bundle.delta_cursor = Some("old-delta".into());
        app.feeds.insert(key.into(), bundle);
    }
    app.alphai_errors.insert("MSFT".into(), "old error".into());
    app.news_selected = 4;
    app.card_scroll = 12;
    save_grouping(&mut app, false);
    for key in ["AAPL", "BINANCE:BTCUSDT", alphai::MARKET_KEY] {
        assert!(!app.feeds.contains_key(key));
    }
    assert!(app.feeds.contains_key(TRENDING_KEY));
    assert!(app.feeds.contains_key("ins:AAPL"));
    assert!(app.calendar.is_some());
    assert!(!app.alphai_errors.contains_key("MSFT"));
    assert_eq!((app.news_selected, app.card_scroll), (0, 0));
    assert!(
        cmds.try_recv().is_err(),
        "Save itself must not fetch every cached ticker"
    );
    app.ensure_alphai_data();
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchNews {
            collapse: false,
            cursor: None,
            sort: alphai::Sort::Published,
            ..
        })
    ));
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err(), "one visible head request only");
}

#[test]
fn late_news_replies_cannot_restore_the_previous_grouping() {
    for mode in [
        alphai::FeedMode::Replace,
        alphai::FeedMode::Append,
        alphai::FeedMode::Merge,
    ] {
        for error in [false, true] {
            let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
            cache_empty_calendar(&mut app);
            app.view_idx = ui::view_index(ui::ViewId::News);
            app.ensure_alphai_data();
            assert!(matches!(
                cmds.try_recv(),
                Ok(alphai::Cmd::FetchNews { collapse: true, .. })
            ));
            save_grouping(&mut app, false);
            app.ensure_alphai_data();
            assert!(cmds.try_recv().is_err(), "old request still owns inflight");
            let event = if error {
                match mode {
                    alphai::FeedMode::Replace => alphai::Event::Error {
                        key: "AAPL".into(),
                        error: "old".into(),
                    },
                    alphai::FeedMode::Append => alphai::Event::PageError {
                        key: "AAPL".into(),
                        error: "old".into(),
                        gated: true,
                    },
                    alphai::FeedMode::Merge => alphai::Event::PollError {
                        key: "AAPL".into(),
                        error: "old".into(),
                    },
                }
            } else {
                alphai::Event::Feed {
                    key: "AAPL".into(),
                    articles: vec![uid_article("old", "Old mode")],
                    side: None,
                    next_cursor: Some("old-cursor".into()),
                    mode,
                    min_relevance: Some(7),
                    categories: Categories::default(),
                }
            };
            app.apply_alphai(event);
            assert!(!app.feeds.contains_key("AAPL"));
            assert!(!app.alphai_errors.contains_key("AAPL"));
            app.ensure_alphai_data();
            assert!(matches!(
                cmds.try_recv(),
                Ok(alphai::Cmd::FetchNews {
                    collapse: false,
                    cursor: None,
                    sort: alphai::Sort::Published,
                    ..
                })
            ));
        }
    }
}

#[test]
fn grouping_choice_reaches_head_paging_and_delta_in_both_scopes() {
    for scope in [NewsScope::Ticker, NewsScope::Market] {
        for collapse in [false, true] {
            let (mut app, mut cmds) = empty_app_with_cmds(vec!["AAPL".into()]);
            cache_empty_calendar(&mut app);
            app.view_idx = ui::view_index(ui::ViewId::News);
            app.news_scope = scope;
            save_grouping(&mut app, collapse);
            let key = app.news_cache_key();
            app.ensure_alphai_data();
            assert!(matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchNews {
                collapse: actual, cursor: None, sort: alphai::Sort::Published, ..
            }) if actual == collapse));
            head_fetch(&mut app, &key, vec![uid_article("one", "One")], Some(7));
            app.feeds.get_mut(&key).unwrap().next_cursor = Some("older".into());
            press(&mut app, KeyCode::Char('j'));
            assert!(matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchNews {
                collapse: actual, cursor: Some(c), sort: alphai::Sort::Published, ..
            }) if actual == collapse && c == "older"));
            app.apply_alphai(alphai::Event::Feed {
                key: key.clone(),
                articles: vec![],
                side: None,
                next_cursor: None,
                mode: alphai::FeedMode::Append,
                min_relevance: Some(7),
                categories: Categories::default(),
            });
            let bundle = app.feeds.get_mut(&key).unwrap();
            bundle.polled = Instant::now() - std::time::Duration::from_secs(600);
            bundle.delta_cursor = Some("delta".into());
            app.ensure_alphai_data();
            assert!(matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchNews {
                collapse: actual, cursor: Some(c), sort: alphai::Sort::Ingested, ..
            }) if actual == collapse && c == "delta"));
        }
    }
}

#[test]
fn ticker_shows_outlet_count_and_market_copy_matches_grouping() {
    let mut app = empty_app(vec!["AAPL".into()]);
    app.view_idx = ui::view_index(ui::ViewId::News);
    let mut a = uid_article("one", "One story");
    a.sources_count = Some(4);
    app.feeds
        .insert("AAPL".into(), FeedBundle::new(vec![a], None, None));
    assert!(render(&mut app).contains("×4"));
    save_grouping(&mut app, false);
    app.news_scope = NewsScope::Market;
    app.feeds.insert(
        alphai::MARKET_KEY.into(),
        FeedBundle::new(vec![uid_article("one", "One")], None, None),
    );
    let screen = render(&mut app);
    assert!(screen.contains("all articles"), "{screen}");
    assert!(!screen.contains("reprints collapsed"), "{screen}");
}

#[test]
fn changed_representative_is_one_story_across_pages_and_updates_only_when_grouped() {
    for collapse in [true, false] {
        for mode in [alphai::FeedMode::Append, alphai::FeedMode::Merge] {
            let mut app = empty_app(vec!["AAPL".into()]);
            save_grouping(&mut app, collapse);
            let mut original = uid_article("first", "First representative");
            original.story_id = Some("stable-story".into());
            head_fetch(&mut app, "AAPL", vec![original], Some(7));
            app.feeds.get_mut("AAPL").unwrap().delta_cursor = Some("delta1".into());
            let mut promoted = uid_article("promoted", "Stronger representative");
            promoted.story_id = Some("stable-story".into());
            app.apply_alphai(alphai::Event::Feed {
                key: "AAPL".into(),
                articles: vec![promoted, uid_article("unrelated", "Another story")],
                side: None,
                next_cursor: Some("next".into()),
                mode,
                min_relevance: Some(7),
                categories: Categories::default(),
            });
            let bundle = &app.feeds["AAPL"];
            assert_eq!(bundle.articles.len(), if collapse { 2 } else { 3 });
            assert!(bundle.articles.iter().any(|a| a.original.uid == "first"));
            assert!(
                bundle
                    .articles
                    .iter()
                    .any(|a| a.original.uid == "unrelated")
            );
            assert_eq!(
                bundle.articles.iter().any(|a| a.original.uid == "promoted"),
                !collapse
            );
            let cursor = match mode {
                alphai::FeedMode::Append => &bundle.next_cursor,
                _ => &bundle.delta_cursor,
            };
            assert_eq!(cursor.as_deref(), Some("next"));
        }
    }
}
