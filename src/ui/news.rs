use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};

use crate::alphai::{Article, fmt_usd};
use crate::app::{App, FeedKind, NewsScope};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, View, ViewId};

/// Below this width the Side layout drops the card pane so the list keeps
/// readable columns; v still opens the fullscreen card.
const SIDE_CARD_MIN_WIDTH: u16 = 90;

pub struct NewsView;

impl View for NewsView {
    fn id(&self) -> ViewId {
        ViewId::News
    }

    fn title(&self) -> &'static str {
        "News"
    }

    fn hints(&self) -> &'static [Hint] {
        const HINTS: &[Hint] = &[
            Hint::act(&[Action::Quit], "quit"),
            Hint::act(&[Action::Up, Action::Down], "article"),
            Hint::act(&[Action::Left, Action::Right], "ticker"),
            Hint::act(&[Action::Open], "open"),
            Hint::act(&[Action::Card], "card"),
            Hint::act(&[Action::CycleLayout], "layout"),
            Hint::act(&[Action::CycleScope], "scope"),
            Hint::act(&[Action::ScoreUp, Action::ScoreDown], "score"),
            Hint::act(&[Action::Refresh], "refresh"),
            // No settings hint: the widest footer of the app, it must fit
            // 110 columns and the help overlay lists s anyway.
            Hint::act(&[Action::Help], "help"),
        ];
        HINTS
    }

    fn feed_shown(&self) -> Option<FeedKind> {
        Some(FeedKind::News)
    }

    fn navigates_articles(&self) -> bool {
        true
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let key = app.news_cache_key();
        let scope = app.news_scope;
        let symbol = app.selected_symbol().to_string();
        let label = scope.label(&symbol).to_string();
        let mut block = app
            .theme
            .panel_titled(news_title(scope, &label, app.news_min_score));

        if render_gate(f, area, &block, app, &key) {
            return;
        }
        let bundle = &app.feeds[&key];

        let [head, main] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(area);

        f.render_widget(head_line(app, bundle.sentiment()), head);

        if bundle.articles.is_empty() {
            let msg = empty_feed_message(scope, &label, app.news_min_score);
            f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), main);
            return;
        }

        // List and card side by side (x flips to list over card); a terminal
        // too narrow for two readable panes drops the card, v still opens the
        // fullscreen one. The side list drops the novelty column; the card
        // carries it in its meta.
        let (list_area, card_area, full) = match app.news_layout {
            crate::app::NewsLayout::Side if main.width < SIDE_CARD_MIN_WIDTH => {
                (main, None, false)
            }
            crate::app::NewsLayout::Side => {
                let [l, r] =
                    Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                        .areas(main);
                (l, Some(r), false)
            }
            crate::app::NewsLayout::Stacked => {
                let [t, b] =
                    Layout::vertical([Constraint::Min(3), Constraint::Percentage(45)]).areas(main);
                (t, Some(b), true)
            }
        };

        let theme = app.theme;
        let at_edge = app.news_selected + 1 >= bundle.articles.len();
        if let Some(hint) = feed_bottom_hint(
            bundle.gated,
            bundle.page_error.as_deref(),
            bundle.next_cursor.is_some(),
            app.is_loading(&key),
            at_edge,
            &theme,
        ) {
            block = block.title_bottom(hint);
        }

        let now = Utc::now();
        let rows: Vec<Row> = bundle
            .articles
            .iter()
            .map(|a| article_row(a, scope, &symbol, now, full, &theme, app.is_unseen(&key, a)))
            .collect();

        let table = Table::new(rows, article_widths(scope, full))
            .block(block)
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        app.news_selected = app.news_selected.min(bundle.articles.len() - 1);
        app.news_table_state.select(Some(app.news_selected));
        f.render_stateful_widget(table, list_area, &mut app.news_table_state);

        if let Some(card_area) = card_area {
            crate::ui::article::render_pane(
                f,
                card_area,
                bundle.articles.get(app.news_selected),
                &symbol,
                &mut app.card_scroll,
                &theme,
            );
        }
    }
}

/// Bottom-border status of a paginated feed list: the archive upsell, a
/// non-destructive page error, the in-flight spinner, or the load-more hint.
/// The hint lights up when the selection sits on the last row, i.e. one
/// keypress away from actually loading.
pub(crate) fn feed_bottom_hint(
    gated: bool,
    page_error: Option<&str>,
    has_more: bool,
    loading: bool,
    at_edge: bool,
    theme: &Theme,
) -> Option<Line<'static>> {
    if gated {
        return Some(
            Line::from(format!(" {} ", crate::alphai::ARCHIVE_GATE_MSG))
                .style(Style::new().fg(theme.warn)),
        );
    }
    if let Some(e) = page_error {
        return Some(Line::from(format!(" {e} ")).style(Style::new().fg(theme.error)));
    }
    if loading {
        return Some(Line::from(" loading… ").dim());
    }
    if !has_more {
        return None;
    }
    let hint = Line::from(" ↓ load older articles ");
    Some(if at_edge {
        hint.style(Style::new().fg(theme.accent))
    } else {
        hint.dim()
    })
}

/// Block title of a news list: the scope label plus the active score filter
/// (trending is server-curated, there is no filter to show).
fn news_title(scope: NewsScope, label: &str, min_score: u8) -> String {
    match scope {
        NewsScope::Trending => format!(" News · {label} "),
        _ => format!(" News · {label} · score {min_score}+ "),
    }
}

fn empty_feed_message(scope: NewsScope, label: &str, min_score: u8) -> String {
    match scope {
        NewsScope::Trending => "no trending stories right now".to_string(),
        _ if min_score > 1 => {
            format!("no recent news for {label} with score {min_score}+ (press - to lower the filter)")
        }
        _ => format!("no recent news for {label}"),
    }
}

/// Compact news strip for the Split view: the freshest articles for the
/// current scope, read only. Selection, detail pane and article opening live
/// in the full News view.
pub fn render_panel(f: &mut Frame, area: Rect, app: &mut App) {
    let key = app.news_cache_key();
    let scope = app.news_scope;
    let label = scope.label(app.selected_symbol()).to_string();
    let block = app
        .theme
        .panel_titled(news_title(scope, &label, app.news_min_score));

    if !app.alphai_enabled {
        let line =
            Line::from(" AI news needs a free AlphaAI key from https://alphai.io, press s to add it")
                .dim();
        f.render_widget(Paragraph::new(line).block(block), area);
        return;
    }
    if render_gate(f, area, &block, app, &key) {
        return;
    }
    let bundle = &app.feeds[&key];
    if bundle.articles.is_empty() {
        let msg = empty_feed_message(scope, &label, app.news_min_score);
        f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), area);
        return;
    }

    let now = Utc::now();
    let rows: Vec<Row> = bundle
        .articles
        .iter()
        .map(|a| {
            article_row(a, scope, app.selected_symbol(), now, false, &app.theme, app.is_unseen(&key, a))
        })
        .collect();
    f.render_widget(
        Table::new(rows, article_widths(scope, false)).block(block),
        area,
    );
}

/// One article as a table row: age, score, novelty (full News view only),
/// sentiment (or tickers plus outlet count outside the ticker scope),
/// category, title. Shared by the News view (`full`) and the Split strip.
fn article_row(
    a: &Article,
    scope: NewsScope,
    symbol: &str,
    now: DateTime<Utc>,
    full: bool,
    theme: &Theme,
    unseen: bool,
) -> Row<'static> {
    // Breaking rows stand out: a fresh age renders in the accent color.
    let age = if is_fresh(a, now) {
        Cell::from(a.age(now)).style(Style::new().fg(theme.accent))
    } else {
        Cell::from(a.age(now)).dim()
    };
    let mut cells = vec![age, score_cell(a.score(), theme)];
    if full {
        cells.push(novelty_cell(a.novelty()));
    }
    if scope == NewsScope::Ticker {
        cells.push(sentiment_cell(a.sentiment_for(symbol), theme));
    } else {
        let tickers: Vec<&str> = a.enrichment.tickers.iter().take(2).map(String::as_str).collect();
        cells.push(Cell::from(tickers.join(",")).bold());
        cells.push(sources_cell(a.sources_badge()));
    }
    cells.push(Cell::from(short_category(a)).dim());
    cells.push(title_cell(a.original.title.clone(), Style::new(), unseen, theme));
    Row::new(cells)
}

/// Title cell of a feed row; rows new since the reader last looked carry
/// the accent unseen marker until the cursor rests on them.
pub(crate) fn title_cell(
    title: String,
    style: Style,
    unseen: bool,
    theme: &Theme,
) -> Cell<'static> {
    if unseen {
        Cell::from(Line::from(vec![
            Span::styled("● ", Style::new().fg(theme.accent)),
            Span::styled(title, style),
        ]))
    } else {
        Cell::from(Span::styled(title, style))
    }
}

fn article_widths(scope: NewsScope, full: bool) -> Vec<Constraint> {
    let mut widths = vec![Constraint::Length(4), Constraint::Length(2)];
    if full {
        widths.push(Constraint::Length(2)); // novelty
    }
    if scope == NewsScope::Ticker {
        widths.push(Constraint::Length(2)); // sentiment glyph
    } else {
        widths.push(Constraint::Length(12)); // tickers
        widths.push(Constraint::Length(3)); // outlet count
    }
    widths.push(Constraint::Length(8)); // category
    widths.push(Constraint::Min(20)); // title
    widths
}

fn head_line(app: &App, sentiment: Option<&crate::alphai::SentimentSummary>) -> Paragraph<'static> {
    match app.news_scope {
        NewsScope::Market => {
            return Paragraph::new(
                Line::from(" market-wide feed · reprints collapsed (×N = outlets) · f: trending")
                    .dim(),
            );
        }
        NewsScope::Trending => {
            return Paragraph::new(
                Line::from(format!(
                    " trending · top 10 of the last 48h · f: back to {}",
                    app.selected_symbol()
                ))
                .dim(),
            );
        }
        NewsScope::Ticker => {}
    }
    let Some(s) = sentiment else {
        return Paragraph::new(Line::from(""));
    };
    Paragraph::new(Line::from(vec![
        Span::raw(format!(" {}d sentiment  ", s.days)).dim(),
        Span::styled(format!("▲ {} bullish", s.bullish), Style::new().fg(app.theme.pos)),
        Span::raw(" · ").dim(),
        Span::raw(format!("{} neutral", s.neutral)).dim(),
        Span::raw(" · ").dim(),
        Span::styled(format!("▼ {} bearish", s.bearish), Style::new().fg(app.theme.neg)),
        Span::raw(format!("  ({} scored)", s.total)).dim(),
    ]))
}

/// Renders the no-key / error / loading placeholder when there is nothing to
/// list yet. Returns true when the caller should stop.
pub fn render_gate(f: &mut Frame, area: Rect, block: &Block, app: &App, key: &str) -> bool {
    if !app.alphai_enabled {
        let lines = vec![
            Line::from(""),
            Line::from("  This view shows AI-scored data from the AlphaAI API.").bold(),
            Line::from(""),
            Line::from("  1. Get a free API key at https://alphai.io (Account -> API keys)"),
            Line::from("     Free tier: 20 requests/min, 100/day. No card needed."),
            Line::from("  2. Press s and paste the key in Settings."),
        ];
        f.render_widget(Paragraph::new(lines).block(block.clone()), area);
        return true;
    }
    if let Some(err) = app.alphai_errors.get(key) {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {err}"),
                Style::new().fg(app.theme.error),
            )),
            Line::from(""),
            Line::from("  press r to retry").dim(),
        ];
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(block.clone()),
            area,
        );
        return true;
    }
    let missing = !app.feeds.contains_key(key);
    if missing {
        f.render_widget(
            Paragraph::new(Line::from("loading…").dim()).block(block.clone()),
            area,
        );
        return true;
    }
    false
}

/// The AI impact analysis to show for an article: the context ticker's when
/// present, otherwise the first enriched ticker's (market/trending rows where
/// the selected symbol is not mentioned).
pub fn display_impact<'a>(a: &'a Article, ticker: &str) -> Option<&'a crate::alphai::Impact> {
    a.impact_for(ticker)
        .or_else(|| a.enrichment.tickers.first().and_then(|t| a.impact_for(t)))
}

/// Bottom pane with the selected article's full title, meta line and summary.
/// `ticker` picks which per-ticker AI analysis feeds the meta line.
pub fn render_detail(
    f: &mut Frame,
    area: Rect,
    article: Option<&Article>,
    ticker: &str,
    theme: &Theme,
) {
    let block = theme.panel();
    let Some(a) = article else {
        f.render_widget(block, area);
        return;
    };
    let lines = vec![
        Line::from(a.original.title.clone()).bold(),
        Line::from(meta_line(a, ticker).join(" · ")).dim(),
        Line::from(a.original.summary.clone()),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block.title(hint_title(" article ", "· ⏎ open · v card ", theme))),
        area,
    );
}

/// A panel title that is a heading plus a dim key hint, e.g.
/// " article · ⏎ open · v card ".
pub(crate) fn hint_title(heading: &str, hint: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            heading.to_string(),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(hint.to_string(), Style::new().dim()),
    ])
}

/// Meta tokens for the detail pane and the article card: source, age,
/// category, scores and the AI calls that exist for this article.
pub fn meta_line(a: &Article, ticker: &str) -> Vec<String> {
    let source = if a.original.source_domain.is_empty() {
        a.original.source.clone()
    } else {
        a.original.source_domain.clone()
    };
    let mut meta: Vec<String> = Vec::new();
    if !source.is_empty() {
        meta.push(source);
    }
    let age = a.age(Utc::now());
    if !age.is_empty() {
        meta.push(format!("{age} ago"));
    }
    if let Some(c) = &a.enrichment.category {
        meta.push(c.clone());
    }
    meta.push(format!("score {}", a.score()));
    if let Some(n) = a.novelty() {
        meta.push(format!("nov {n}"));
    }
    if let Some(i) = display_impact(a, ticker) {
        match (&i.sentiment, &i.confidence) {
            (Some(s), Some(c)) => meta.push(format!("{s}/{c}")),
            (Some(s), None) => meta.push(s.clone()),
            _ => {}
        }
    }
    if let Some(act) = a.trading_value().and_then(|t| t.actionability_score.clone()) {
        meta.push(format!("act {act}"));
    }
    // Insider rows: the structured trade beats anything parsed from text.
    if let Some(t) = &a.insider {
        match (t.side.as_deref(), t.total_value_usd.as_deref()) {
            (Some(side), Some(v)) => meta.push(format!("{} {}", side.to_uppercase(), fmt_usd(v))),
            (Some(side), None) => meta.push(side.to_uppercase()),
            _ => {}
        }
        if t.is_10b5_1 {
            meta.push("10b5-1 plan".to_string());
        }
    }
    if let Some(n) = a.sources_badge() {
        meta.push(format!("×{n} outlets"));
    }
    if !a.enrichment.tickers.is_empty() {
        meta.push(a.enrichment.tickers.join(", "));
    }
    meta
}

/// Under 15 minutes old counts as breaking; unparsable timestamps never do.
pub(crate) fn is_fresh(a: &Article, now: DateTime<Utc>) -> bool {
    a.published().is_some_and(|ts| (now - ts).num_minutes() < 15)
}

pub fn score_cell(score: i64, theme: &Theme) -> Cell<'static> {
    let style = match score {
        8..=10 => Style::new().fg(theme.score_high).bold(),
        6..=7 => Style::new(),
        _ => Style::new().dim(),
    };
    Cell::from(format!("{score:>2}")).style(style)
}

pub(crate) fn sentiment_cell(sentiment: Option<&str>, theme: &Theme) -> Cell<'static> {
    match sentiment {
        Some("positive") => Cell::from("▲").style(Style::new().fg(theme.pos)),
        Some("negative") => Cell::from("▼").style(Style::new().fg(theme.neg)),
        Some(_) => Cell::from("·").dim(),
        None => Cell::from(" "),
    }
}

/// Novelty 1-10, always dim so it reads apart from the styled score.
fn novelty_cell(novelty: Option<i64>) -> Cell<'static> {
    match novelty {
        Some(n) => Cell::from(format!("{n:>2}")).dim(),
        None => Cell::from("  "),
    }
}

/// Outlet count on story-collapsed rows: "×7" when more than one outlet.
fn sources_cell(count: Option<i64>) -> Cell<'static> {
    match count {
        Some(n) => Cell::from(format!("×{n}")).dim(),
        None => Cell::from("   "),
    }
}

fn short_category(a: &Article) -> &'static str {
    match a.enrichment.category.as_deref() {
        Some("earnings") => "earnings",
        Some("mergers_acquisitions") => "m&a",
        Some("regulation") => "reg",
        Some("macro_economy") => "macro",
        Some("sector_analysis") => "sector",
        Some("market_movers") => "movers",
        Some("technology") => "tech",
        Some("commodities") => "commod",
        Some("crypto") => "crypto",
        Some("ipo") => "ipo",
        Some("geopolitics") => "geo",
        Some("insider") => "insider",
        Some("corporate_actions") => "corp",
        _ => "",
    }
}
