use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use crate::alphai::{Article, InsiderTrades, fmt_usd, insider_key};
use crate::app::{App, FeedKind};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, View, ViewId};
use crate::ui::insider_chart;
use crate::ui::news::{
    feed_bottom_hint, is_fresh, render_detail, render_gate, score_cell, sentiment_cell, title_cell,
    title_width,
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
            Hint::act(&[Action::InsiderChart], "chart"),
            Hint::act(&[Action::Refresh], "refresh"),
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
        let mut block = app.theme.panel_titled(format!(
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

        // The chart panel takes rows only while the g window is on and the
        // bundle actually has events; the list keeps a usable minimum and
        // the weekly bars drop before the panel does (like panel_split).
        let trades = bundle.insider_trades();
        let chart_h = match (app.insider_chart.days(), trades) {
            (Some(_), Some(t)) if !t.chart_events.is_empty() => {
                insider_chart::panel_height(area.height.saturating_sub(2 + 6 + 5))
            }
            _ => 0,
        };
        let [head, chart_area, list_area, detail] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(chart_h),
            Constraint::Min(3),
            Constraint::Length(6),
        ])
        .areas(area);

        f.render_widget(summary_lines(trades, &theme), head);
        if let (true, Some(t)) = (chart_h > 0, trades) {
            // The chart ignores the score filter (it plots the full bundle),
            // but the selected filing's mark renders inverted: the list and
            // the chart stay joined by the article uid.
            let selected_uid = bundle
                .articles
                .get(app.news_selected)
                .map(|a| a.original.uid.as_str())
                .filter(|u| !u.is_empty());
            insider_chart::render(
                f,
                chart_area,
                t,
                app.insider_chart,
                selected_uid,
                Utc::now().date_naive(),
                &theme,
            );
        }

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
        let widths = [
            Constraint::Length(4),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1), // 10b5-1 plan marker
            Constraint::Length(8), // trade value
            Constraint::Min(20),
        ];
        let title_w = title_width(&widths, list_area.width, true);
        let rows: Vec<Row> = bundle
            .articles
            .iter()
            .map(|a| filing_row(a, &symbol, now, &theme, app.is_unseen(&key, a), title_w))
            .collect();
        let table = Table::new(rows, widths)
            .block(block)
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        app.news_selected = app.news_selected.min(bundle.articles.len() - 1);
        app.news_table_state.select(Some(app.news_selected));
        f.render_stateful_widget(table, list_area, &mut app.news_table_state);

        // The chart bundle knows more about the selected filing than the
        // feed row does (stake moved, tranches, late flag): join by uid and
        // hand it to the detail pane's meta line.
        let extra = bundle
            .articles
            .get(app.news_selected)
            .filter(|a| !a.original.uid.is_empty())
            .and_then(|a| event_extra(trades, &a.original.uid));
        render_detail(
            f,
            detail,
            bundle.articles.get(app.news_selected),
            &symbol,
            extra,
            &theme,
        );
    }
}

/// Detail-pane extras for one filing, joined from the chart bundle by the
/// article uid: the stake share it moved, the tranche count of the folded
/// group, and the late-filing flag. None when nothing extra is known.
fn event_extra(trades: Option<&InsiderTrades>, uid: &str) -> Option<String> {
    let e = trades?
        .chart_events
        .iter()
        .find(|e| e.news_uid.as_deref() == Some(uid))?;
    let mut parts = Vec::new();
    if let Some(pct) = e.stake_change_pct.as_deref().and_then(|p| p.parse::<f64>().ok()) {
        parts.push(format!("stake {pct:+.1}%"));
    }
    if let Some(n) = e.tranche_count.filter(|&n| n > 1) {
        parts.push(format!("{n} tranches"));
    }
    if e.late_filing {
        parts.push("late filing".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
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
    title_w: usize,
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
        title_cell(a.original.title.clone(), title_style, unseen, theme, title_w),
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

fn summary_lines(trades: Option<&InsiderTrades>, theme: &Theme) -> Paragraph<'static> {
    let Some(w) = trades
        .and_then(|t| t.summary.as_ref())
        .and_then(|s| s.last_12m.as_ref())
    else {
        return Paragraph::new(Line::from(" Form 4 rollup unavailable").dim());
    };
    let buys = w
        .buy_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let sells = w
        .sell_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let stats = Line::from(vec![
        Span::raw(" 12m  ").dim(),
        Span::raw(format!("{} events · ", w.buy_count + w.sell_count)),
        Span::styled(
            format!("▲ {} buys{buys}", w.buy_count),
            Style::new().fg(theme.pos),
        ),
        Span::raw(" · ").dim(),
        Span::styled(
            format!("▼ {} sells{sells}", w.sell_count),
            Style::new().fg(theme.neg),
        ),
        Span::raw(format!(
            " · {}% under 10b5-1 plans · {} insiders",
            w.pct_10b5_1, w.unique_insiders
        ))
        .dim(),
    ]);
    let top: Vec<String> = trades
        .and_then(|t| t.summary.as_ref())
        .map(|s| s.top_insiders.as_slice())
        .unwrap_or_default()
        .iter()
        .take(3)
        .map(|t| {
            let title = if t.title.is_empty() {
                String::new()
            } else {
                format!(" ({})", t.title)
            };
            let net = t
                .net_value_usd
                .as_deref()
                .map(|v| format!(" {}", fmt_usd(v)))
                .unwrap_or_default();
            let count = if t.event_count > 0 {
                format!(" ×{}", t.event_count)
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
