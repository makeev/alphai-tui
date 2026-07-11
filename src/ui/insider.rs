use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::alphai::{Article, InsiderSummary, fmt_usd, insider_key};
use crate::app::{App, FeedKind};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, View, ViewId};
use crate::ui::news::{feed_bottom_hint, render_detail, render_gate, score_cell, sentiment_cell};

pub struct InsiderView;

impl View for InsiderView {
    fn id(&self) -> ViewId {
        ViewId::Insider
    }

    fn title(&self) -> &'static str {
        "Insider"
    }

    fn hints(&self) -> &'static [Hint] {
        const HINTS: &[Hint] = &[
            Hint::act(&[Action::Quit], "quit"),
            Hint::fixed("1-9", "view"),
            Hint::act(&[Action::Up, Action::Down], "article"),
            Hint::act(&[Action::Left, Action::Right], "ticker"),
            Hint::act(&[Action::Open], "open"),
            Hint::act(&[Action::Card], "card"),
            Hint::act(&[Action::Refresh], "refresh"),
            Hint::act(&[Action::Settings], "settings"),
        ];
        HINTS
    }

    fn feed_shown(&self) -> Option<FeedKind> {
        Some(FeedKind::Insider)
    }

    fn navigates_articles(&self) -> bool {
        true
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let symbol = app.selected_symbol().to_string();
        let key = insider_key(&symbol);
        let mut block = Block::bordered().title(format!(" Insider · {symbol} (SEC Form 4) "));

        if render_gate(f, area, &block, app, &key) {
            return;
        }
        let bundle = &app.feeds[&key];
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

        let [head, list_area, detail] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(6),
        ])
        .areas(area);

        f.render_widget(summary_lines(bundle.insider_summary(), &theme), head);

        if bundle.articles.is_empty() {
            let msg = format!("no Form 4 activity for {symbol} in the feed");
            f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), list_area);
            return;
        }

        let now = Utc::now();
        let rows: Vec<Row> = bundle
            .articles
            .iter()
            .map(|a| filing_row(a, &symbol, now, &theme))
            .collect();
        let widths = [
            Constraint::Length(4),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(20),
        ];
        let table = Table::new(rows, widths)
            .block(block)
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        app.news_selected = app.news_selected.min(bundle.articles.len() - 1);
        app.news_table_state.select(Some(app.news_selected));
        f.render_stateful_widget(table, list_area, &mut app.news_table_state);

        render_detail(f, detail, bundle.articles.get(app.news_selected), &symbol);
    }
}

/// One Form 4 filing as a row: age, score, buy/sell glyph, direct/indirect
/// marker, title colored by trade side when known. The glyph reports the
/// trade direction, so the deterministic title template wins over the AI
/// sentiment call (which can rate a routine sale as neutral).
fn filing_row(a: &Article, symbol: &str, now: chrono::DateTime<Utc>, theme: &Theme) -> Row<'static> {
    let side = side_from_title(&a.original.title)
        .or_else(|| a.sentiment_for(symbol).map(str::to_string));
    let title_style = match side.as_deref() {
        Some("positive") => Style::new().fg(theme.pos),
        Some("negative") => Style::new().fg(theme.neg),
        _ => Style::new(),
    };
    Row::new(vec![
        Cell::from(a.age(now)).dim(),
        score_cell(a.score(), theme),
        sentiment_cell(side.as_deref(), theme),
        ownership_cell(a.original.ownership_form.as_deref()),
        Cell::from(a.original.title.clone()).style(title_style),
    ])
}

/// Trade side from the deterministic Form 4 row template, for rows whose
/// enrichment carries no per-ticker sentiment. Tied to the current wording;
/// if the templates change it degrades to an uncolored row, nothing worse.
fn side_from_title(title: &str) -> Option<String> {
    let t = title.to_lowercase();
    if t.contains("sold") || t.contains("sale") {
        Some("negative".to_string())
    } else if t.contains("bought") || t.contains("purchase") || t.contains("buy")
        || t.contains("acquired")
    {
        Some("positive".to_string())
    } else {
        None
    }
}

/// Which holding pool the filing touched: dim "D" (direct) / "I" (indirect).
fn ownership_cell(form: Option<&str>) -> Cell<'static> {
    match form {
        Some("direct") => Cell::from("D").dim(),
        Some("indirect") => Cell::from("I").dim(),
        _ => Cell::from(" "),
    }
}

fn summary_lines(summary: Option<&InsiderSummary>, theme: &Theme) -> Paragraph<'static> {
    let Some(s) = summary else {
        return Paragraph::new(Line::from(" 30d summary unavailable").dim());
    };
    let buys = s
        .buy_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let sells = s
        .sell_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let stats = Line::from(vec![
        Span::raw(format!(" {}d  ", s.days)).dim(),
        Span::raw(format!("{} filings · ", s.total_transactions)),
        Span::styled(
            format!("▲ {} buys{buys}", s.buy_count),
            Style::new().fg(theme.pos),
        ),
        Span::raw(" · ").dim(),
        Span::styled(
            format!("▼ {} sells{sells}", s.sell_count),
            Style::new().fg(theme.neg),
        ),
        Span::raw(format!(" · {}% under 10b5-1 plans", s.pct_10b5_1)).dim(),
    ]);
    let top: Vec<String> = s
        .top_insiders
        .iter()
        .take(3)
        .map(|t| {
            let title = if t.title.is_empty() {
                String::new()
            } else {
                format!(" ({})", t.title)
            };
            let net = t
                .net_value
                .as_deref()
                .map(|v| format!(" {}", fmt_usd(v)))
                .unwrap_or_default();
            let count = if t.transaction_count > 0 {
                format!(" ×{}", t.transaction_count)
            } else {
                String::new()
            };
            format!("{}{title}{net}{count}", t.name)
        })
        .collect();
    let top_line = if top.is_empty() {
        Line::from("")
    } else {
        Line::from(format!(" top: {}", top.join(" · "))).dim()
    };
    Paragraph::new(vec![stats, top_line])
}
