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
use crate::ui::news::{
    feed_bottom_hint, is_fresh, render_detail, render_gate, score_cell, sentiment_cell, title_cell,
};

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
            Hint::act(&[Action::ScoreUp, Action::ScoreDown], "size"),
            Hint::act(&[Action::Refresh], "refresh"),
            Hint::act(&[Action::Settings], "settings"),
            Hint::act(&[Action::Help], "help"),
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
        let mut block = Block::bordered().title(format!(
            " Insider · {symbol} (SEC Form 4) · score {}+ ",
            app.insider_min_score
        ));

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
            let msg = if app.insider_min_score > 1 {
                format!(
                    "no Form 4 activity for {symbol} with score {}+ (press - to lower the filter)",
                    app.insider_min_score
                )
            } else {
                format!("no Form 4 activity for {symbol} in the feed")
            };
            f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), list_area);
            return;
        }

        let now = Utc::now();
        let rows: Vec<Row> = bundle
            .articles
            .iter()
            .map(|a| filing_row(a, &symbol, now, &theme, app.is_unseen(&key, a)))
            .collect();
        let widths = [
            Constraint::Length(4),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1), // 10b5-1 plan marker
            Constraint::Length(8), // trade value
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
/// marker, a 10b5-1 plan flag, the trade value, title colored by trade side
/// when known.
fn filing_row(
    a: &Article,
    symbol: &str,
    now: chrono::DateTime<Utc>,
    theme: &Theme,
    unseen: bool,
) -> Row<'static> {
    let side = filing_side(a, symbol);
    let title_style = match side.as_deref() {
        Some("positive") => Style::new().fg(theme.pos),
        Some("negative") => Style::new().fg(theme.neg),
        _ => Style::new(),
    };
    // Same freshness accent as the news rows: a just-filed Form 4 stands out.
    let age = if is_fresh(a, now) {
        Cell::from(a.age(now)).style(Style::new().fg(theme.accent))
    } else {
        Cell::from(a.age(now)).dim()
    };
    let plan = match &a.insider {
        Some(t) if t.is_10b5_1 => Cell::from("p").dim(),
        _ => Cell::from(" "),
    };
    let value = a
        .insider
        .as_ref()
        .and_then(|t| t.total_value_usd.as_deref())
        .map(|v| Cell::from(format!("{:>8}", fmt_usd(v))).dim())
        .unwrap_or_else(|| Cell::from(" "));
    Row::new(vec![
        age,
        score_cell(a.score(), theme),
        sentiment_cell(side.as_deref(), theme),
        ownership_cell(a.original.ownership_form.as_deref()),
        plan,
        value,
        title_cell(a.original.title.clone(), title_style, unseen, theme),
    ])
}

/// Trade side of a filing row. The API's structured `insider.side` is
/// authoritative when present: "buy"/"sell" color the row, "other" (grants,
/// code D sales back to the issuer, ...) deliberately stays neutral rather
/// than falling back to keyword guessing. Legacy rows without the block keep
/// the old chain: title template first, then the AI sentiment call (which
/// can rate a routine sale as neutral).
fn filing_side(a: &Article, symbol: &str) -> Option<String> {
    match a.insider.as_ref().and_then(|t| t.side.as_deref()) {
        Some("buy") => Some("positive".to_string()),
        Some("sell") => Some("negative".to_string()),
        Some(_) => Some("neutral".to_string()),
        None => side_from_title(&a.original.title)
            .or_else(|| a.sentiment_for(symbol).map(str::to_string)),
    }
}

/// Trade side from the deterministic Form 4 row template, for legacy rows
/// without the structured block. Tied to the current wording; if the
/// templates change it degrades to an uncolored row, nothing worse.
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
