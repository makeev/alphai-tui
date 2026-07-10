use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};

use crate::alphai::Article;
use crate::app::App;
use crate::ui::View;

pub struct NewsView;

impl View for NewsView {
    fn title(&self) -> &'static str {
        "News"
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let key = app.news_cache_key();
        let scope = if app.news_market_wide {
            "market".to_string()
        } else {
            app.selected_symbol().to_string()
        };
        let block = Block::bordered().title(format!(" News · {scope} "));

        if render_gate(f, area, &block, app, &key) {
            return;
        }
        let bundle = &app.news[&key];

        let [head, list_area, detail] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(7),
        ])
        .areas(area);

        f.render_widget(head_line(app, bundle.sentiment.as_ref()), head);

        if bundle.articles.is_empty() {
            let msg = format!("no recent news for {scope} (relevance score 4 or higher)");
            f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), list_area);
            return;
        }

        let now = Utc::now();
        let rows: Vec<Row> = bundle
            .articles
            .iter()
            .map(|a| article_row(a, app.news_market_wide, app.selected_symbol(), now))
            .collect();

        let table = Table::new(rows, article_widths(app.news_market_wide))
            .block(block)
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        app.news_selected = app.news_selected.min(bundle.articles.len() - 1);
        app.news_table_state.select(Some(app.news_selected));
        f.render_stateful_widget(table, list_area, &mut app.news_table_state);

        render_detail(f, detail, bundle.articles.get(app.news_selected));
    }
}

/// Compact news strip for the Split view: the freshest articles for the
/// current scope, read only. Selection, detail pane and article opening live
/// in the full News view.
pub fn render_panel(f: &mut Frame, area: Rect, app: &mut App) {
    let key = app.news_cache_key();
    let scope = if app.news_market_wide {
        "market".to_string()
    } else {
        app.selected_symbol().to_string()
    };
    let block = Block::bordered().title(format!(" News · {scope} "));

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
        let msg = format!("no recent news for {scope} (relevance score 4 or higher)");
        f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), area);
        return;
    }

    let now = Utc::now();
    let rows: Vec<Row> = bundle
        .articles
        .iter()
        .map(|a| article_row(a, app.news_market_wide, app.selected_symbol(), now))
        .collect();
    f.render_widget(
        Table::new(rows, article_widths(app.news_market_wide)).block(block),
        area,
    );
}

/// One article as a table row: age, score, sentiment (or tickers when
/// market-wide), category, title. Shared by the News view and the Split strip.
fn article_row(a: &Article, market_wide: bool, symbol: &str, now: DateTime<Utc>) -> Row<'static> {
    let mut cells = vec![Cell::from(a.age(now)).dim(), score_cell(a.score())];
    if market_wide {
        let tickers: Vec<&str> = a.enrichment.tickers.iter().take(2).map(String::as_str).collect();
        cells.push(Cell::from(tickers.join(",")).bold());
    } else {
        cells.push(sentiment_cell(a.sentiment_for(symbol)));
    }
    cells.push(Cell::from(short_category(a)).dim());
    cells.push(Cell::from(a.original.title.clone()));
    Row::new(cells)
}

fn article_widths(market_wide: bool) -> [Constraint; 5] {
    let ticker_col = if market_wide { 12 } else { 2 };
    [
        Constraint::Length(4),
        Constraint::Length(2),
        Constraint::Length(ticker_col),
        Constraint::Length(8),
        Constraint::Min(20),
    ]
}

fn head_line(app: &App, sentiment: Option<&crate::alphai::SentimentSummary>) -> Paragraph<'static> {
    if app.news_market_wide {
        return Paragraph::new(
            Line::from(" market-wide feed · press f to focus the selected ticker").dim(),
        );
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

/// Bottom pane with the selected article's full title, meta line and summary.
pub fn render_detail(f: &mut Frame, area: Rect, article: Option<&Article>) {
    let block = Block::bordered();
    let Some(a) = article else {
        f.render_widget(block, area);
        return;
    };
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
    if !a.enrichment.tickers.is_empty() {
        meta.push(a.enrichment.tickers.join(", "));
    }
    let lines = vec![
        Line::from(a.original.title.clone()).bold(),
        Line::from(meta.join(" · ")).dim(),
        Line::from(a.original.summary.clone()),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block.title(" article · ⏎ open in browser ")),
        area,
    );
}

pub fn score_cell(score: i64) -> Cell<'static> {
    let style = match score {
        8..=10 => Style::new().fg(Color::Yellow).bold(),
        6..=7 => Style::new(),
        _ => Style::new().dim(),
    };
    Cell::from(format!("{score:>2}")).style(style)
}

fn sentiment_cell(sentiment: Option<&str>) -> Cell<'static> {
    match sentiment {
        Some("positive") => Cell::from("▲").style(Style::new().fg(Color::Green)),
        Some("negative") => Cell::from("▼").style(Style::new().fg(Color::Red)),
        Some(_) => Cell::from("·").dim(),
        None => Cell::from(" "),
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
