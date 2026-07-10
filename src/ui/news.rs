use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};

use crate::alphai::Article;
use crate::app::{App, NewsScope};
use crate::ui::View;

pub struct NewsView;

impl View for NewsView {
    fn title(&self) -> &'static str {
        "News"
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let key = app.news_cache_key();
        let scope = app.news_scope;
        let symbol = app.selected_symbol().to_string();
        let label = scope.label(&symbol).to_string();
        let mut block = Block::bordered().title(format!(" News · {label} "));

        if render_gate(f, area, &block, app, &key) {
            return;
        }
        let bundle = &app.news[&key];

        let [head, main] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(area);

        f.render_widget(head_line(app, bundle.sentiment.as_ref()), head);

        if bundle.articles.is_empty() {
            let msg = empty_feed_message(scope, &label);
            f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), main);
            return;
        }

        // List and card side by side (x flips to list over card). The side
        // list drops the novelty column; the card carries it in its meta.
        let (list_area, card_area, full) = match app.news_layout {
            crate::app::NewsLayout::Side => {
                let [l, r] =
                    Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                        .areas(main);
                (l, r, false)
            }
            crate::app::NewsLayout::Stacked => {
                let [t, b] =
                    Layout::vertical([Constraint::Min(3), Constraint::Percentage(45)]).areas(main);
                (t, b, true)
            }
        };

        let at_edge = app.news_selected + 1 >= bundle.articles.len();
        if let Some(hint) = feed_bottom_hint(
            bundle.gated,
            bundle.page_error.as_deref(),
            bundle.next_cursor.is_some(),
            app.is_loading(&key),
            at_edge,
        ) {
            block = block.title_bottom(hint);
        }

        let now = Utc::now();
        let rows: Vec<Row> = bundle
            .articles
            .iter()
            .map(|a| article_row(a, scope, &symbol, now, full))
            .collect();

        let table = Table::new(rows, article_widths(scope, full))
            .block(block)
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        app.news_selected = app.news_selected.min(bundle.articles.len() - 1);
        app.news_table_state.select(Some(app.news_selected));
        f.render_stateful_widget(table, list_area, &mut app.news_table_state);

        crate::ui::article::render_pane(
            f,
            card_area,
            bundle.articles.get(app.news_selected),
            &symbol,
            &mut app.card_scroll,
        );
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
) -> Option<Line<'static>> {
    if gated {
        return Some(
            Line::from(format!(" {} ", crate::alphai::ARCHIVE_GATE_MSG))
                .style(Style::new().fg(Color::Yellow)),
        );
    }
    if let Some(e) = page_error {
        return Some(Line::from(format!(" {e} ")).style(Style::new().fg(Color::Red)));
    }
    if loading {
        return Some(Line::from(" loading… ").dim());
    }
    if !has_more {
        return None;
    }
    let hint = Line::from(" ↓ load older articles ");
    Some(if at_edge {
        hint.style(Style::new().fg(Color::Cyan))
    } else {
        hint.dim()
    })
}

fn empty_feed_message(scope: NewsScope, label: &str) -> String {
    match scope {
        NewsScope::Trending => "no trending stories right now".to_string(),
        _ => format!("no recent news for {label} (relevance score 4 or higher)"),
    }
}

/// Compact news strip for the Split view: the freshest articles for the
/// current scope, read only. Selection, detail pane and article opening live
/// in the full News view.
pub fn render_panel(f: &mut Frame, area: Rect, app: &mut App) {
    let key = app.news_cache_key();
    let scope = app.news_scope;
    let label = scope.label(app.selected_symbol()).to_string();
    let block = Block::bordered().title(format!(" News · {label} "));

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
    let bundle = &app.news[&key];
    if bundle.articles.is_empty() {
        let msg = empty_feed_message(scope, &label);
        f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), area);
        return;
    }

    let now = Utc::now();
    let rows: Vec<Row> = bundle
        .articles
        .iter()
        .map(|a| article_row(a, scope, app.selected_symbol(), now, false))
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
) -> Row<'static> {
    let mut cells = vec![Cell::from(a.age(now)).dim(), score_cell(a.score())];
    if full {
        cells.push(novelty_cell(a.novelty()));
    }
    if scope == NewsScope::Ticker {
        cells.push(sentiment_cell(a.sentiment_for(symbol)));
    } else {
        let tickers: Vec<&str> = a.enrichment.tickers.iter().take(2).map(String::as_str).collect();
        cells.push(Cell::from(tickers.join(",")).bold());
        cells.push(sources_cell(a.sources_badge()));
    }
    cells.push(Cell::from(short_category(a)).dim());
    cells.push(Cell::from(a.original.title.clone()));
    Row::new(cells)
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
        Span::styled(format!("▲ {} bullish", s.bullish), Style::new().fg(Color::Green)),
        Span::raw(" · ").dim(),
        Span::raw(format!("{} neutral", s.neutral)).dim(),
        Span::raw(" · ").dim(),
        Span::styled(format!("▼ {} bearish", s.bearish), Style::new().fg(Color::Red)),
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
                Style::new().fg(Color::Red),
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
    let missing = match app.view_idx {
        crate::ui::VIEW_INSIDER => !app.insider.contains_key(app.selected_symbol()),
        _ => !app.news.contains_key(key),
    };
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
pub fn render_detail(f: &mut Frame, area: Rect, article: Option<&Article>, ticker: &str) {
    let block = Block::bordered();
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
            .block(block.title(" article · ⏎ open · v card ")),
        area,
    );
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
    if let Some(n) = a.sources_badge() {
        meta.push(format!("×{n} outlets"));
    }
    if !a.enrichment.tickers.is_empty() {
        meta.push(a.enrichment.tickers.join(", "));
    }
    meta
}

pub fn score_cell(score: i64) -> Cell<'static> {
    let style = match score {
        8..=10 => Style::new().fg(Color::Yellow).bold(),
        6..=7 => Style::new(),
        _ => Style::new().dim(),
    };
    Cell::from(format!("{score:>2}")).style(style)
}

pub(crate) fn sentiment_cell(sentiment: Option<&str>) -> Cell<'static> {
    match sentiment {
        Some("positive") => Cell::from("▲").style(Style::new().fg(Color::Green)),
        Some("negative") => Cell::from("▼").style(Style::new().fg(Color::Red)),
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
