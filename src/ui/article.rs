//! Full-article card: a modal overlay (v in the News/Insider views) showing
//! everything the AlphAI enrichment carries for the selected article. Pure
//! render — the data was already fetched with the list, so opening the card
//! costs no API requests.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::alphai::{self, Article, EarningsRead, KeyMetric, TickerAnalysis};
use crate::app::App;
use crate::theme::Theme;
use crate::ui::{centered, news};

/// The fullscreen card overlay (v in the News/Insider views).
pub fn render(f: &mut Frame, app: &mut App) {
    let Some(a) = app
        .visible_articles()
        .and_then(|list| list.get(app.news_selected))
    else {
        // The list changed under the overlay (ticker switch race): self-heal.
        app.article_overlay = Default::default();
        return;
    };

    let area = centered(
        f.area(),
        94.min(f.area().width.saturating_sub(4)),
        26.min(f.area().height.saturating_sub(4)),
    );
    f.render_widget(Clear, area);
    let block = app
        .theme
        .modal()
        .title(app.theme.heading(" Article "))
        .border_style(Style::new().fg(app.theme.accent));
    let earnings = app.find_earnings_by_uid(&a.original.uid);
    let extra = app
        .feeds
        .get(&alphai::insider_key(app.selected_symbol()))
        .and_then(|b| super::insider::event_extra(b.insider_trades(), &a.original.uid));
    let lines = card_lines(
        a,
        app.selected_symbol(),
        earnings,
        extra.as_deref(),
        &app.theme,
    );
    let mut scroll = app.article_overlay.scroll;
    render_card(f, area, block, lines, &mut scroll);
    app.article_overlay.scroll = scroll;
}

/// The card as an embedded pane (News view layouts). Scrolls via
/// `app.card_scroll`; an empty selection renders a bare frame.
pub fn render_pane(
    f: &mut Frame,
    area: Rect,
    article: Option<&Article>,
    symbol: &str,
    earnings: Option<&EarningsRead>,
    scroll: &mut u16,
    theme: &Theme,
) {
    render_pane_with(
        f,
        area,
        article,
        symbol,
        earnings,
        None,
        "· pgup/pgdn scroll · v expand ",
        scroll,
        theme,
    );
}

/// The card pane with the caller's own key hint and, for Form 4 filings,
/// the extras the chart bundle knows (stake moved, tranches, late filing)
/// alongside the structured trade facts.
#[allow(clippy::too_many_arguments)]
pub fn render_pane_with(
    f: &mut Frame,
    area: Rect,
    article: Option<&Article>,
    symbol: &str,
    earnings: Option<&EarningsRead>,
    extra: Option<String>,
    hint: &str,
    scroll: &mut u16,
    theme: &Theme,
) {
    let block = theme.panel().title(news::hint_title(" card ", hint, theme));
    let Some(a) = article else {
        f.render_widget(block, area);
        return;
    };
    let lines = card_lines(a, symbol, earnings, extra.as_deref(), theme);
    render_card(f, area, block, lines, scroll);
}

/// The card read in place in the News chart layout (v there): the full
/// width under the chart, scrolled by the overlay's own keys.
pub fn render_reading(
    f: &mut Frame,
    area: Rect,
    article: Option<&Article>,
    symbol: &str,
    earnings: Option<&EarningsRead>,
    scroll: &mut u16,
    theme: &Theme,
) {
    let block = theme.panel().title(news::hint_title(
        " article ",
        "· ↑↓ scroll · ⏎ open · v back ",
        theme,
    ));
    let Some(a) = article else {
        f.render_widget(block, area);
        return;
    };
    let lines = card_lines(a, symbol, earnings, None, theme);
    render_card(f, area, block, lines, scroll);
}

/// Shared card body: clamps the scroll to the wrapped height and renders.
/// The height estimate divides by character width instead of real word-wrap
/// points, so it can be off by a line or two on very long cards; harmless
/// for a clamp.
fn render_card(
    f: &mut Frame,
    area: Rect,
    block: Block,
    lines: Vec<Line<'static>>,
    scroll: &mut u16,
) {
    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);
    let max_scroll = wrapped_height(&lines, inner_w).saturating_sub(inner_h);
    *scroll = (*scroll).min(max_scroll);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((*scroll, 0))
            .block(block),
        area,
    );
}

/// Lead with the selected ticker's analysis, or the filing's structured
/// facts. The general summary and other companies follow that context.
fn card_lines(
    a: &Article,
    symbol: &str,
    earnings: Option<&EarningsRead>,
    extra: Option<&str>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let title = a
        .insider
        .as_ref()
        .filter(|t| !t.insider_name.is_empty() && !a.enrichment.tickers.is_empty())
        .map(|t| {
            if t.role().is_empty() {
                format!("{} · {}", a.enrichment.tickers.join(", "), t.insider_name)
            } else {
                format!(
                    "{} · {} ({})",
                    a.enrichment.tickers.join(", "),
                    t.insider_name,
                    t.role()
                )
            }
        })
        .unwrap_or_else(|| a.original.title.clone());
    let mut lines = vec![
        Line::from(title).bold(),
        Line::from(news::source_meta(a).join(" · ")).style(theme.subtle()),
    ];
    lines.extend(trade_lines(a, extra, theme));
    let selected = a.enrichment.ai_trading_insights.as_ref().and_then(|i| {
        i.ticker_analysis
            .iter()
            .find(|t| t.ticker.eq_ignore_ascii_case(symbol) && t.impact_analysis.is_some())
    });
    if let Some(t) = selected {
        lines.push(Line::from(""));
        lines.extend(impact_lines(t, theme));
    }
    if !a.original.summary.is_empty() {
        lines.push(Line::from(""));
        lines.push(section(
            if a.insider.is_some() {
                "Filing summary"
            } else {
                "Summary"
            },
            theme,
        ));
        lines.push(Line::from(a.original.summary.clone()));
    }

    // Earnings filings: the read, when one is already cached. The card
    // never fetches, so scrolling the feed costs nothing; the full read is
    // one keypress away in the Earnings view.
    if let Some(read) = earnings {
        lines.push(Line::from(""));
        let r = read.report();
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "Earnings read · {}",
                    alphai::short_fiscal_period(&r.fiscal_period)
                ),
                Style::new().fg(theme.accent),
            ),
            verdict_span(&r.verdict, theme),
        ]));
        if !r.verdict_reason.is_empty() {
            lines.push(Line::from(format!("  {}", r.verdict_reason)));
        }
        for m in card_metrics(&r.key_metrics) {
            let mut row = format!("  {} {}", m.name, alphai::short_metric(&m.value));
            for (change, label) in [(&m.qoq_change, "q/q"), (&m.yoy_change, "y/y")] {
                if let Some(c) = change.as_deref().filter(|c| !c.trim().is_empty()) {
                    row.push_str(&format!(" · {} {label}", alphai::signed_change(c)));
                }
            }
            lines.push(Line::from(row));
        }
        if let Some(revenue) = r
            .guidance
            .as_ref()
            .and_then(|g| g.revenue.as_deref().map(|v| (g.period.clone(), v)))
        {
            lines.push(Line::from(format!(
                "  Outlook {}: revenue {}",
                revenue.0,
                alphai::short_metric(revenue.1)
            )));
        }
        lines.push(Line::from("  press 6 for the full read").style(theme.subtle()));
    } else if alphai::is_earnings_filing(a) {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Earnings read", Style::new().fg(theme.accent)),
            Span::styled(" · press 6", theme.subtle()),
        ]));
    }

    let mut details = Vec::new();
    if let Some(category) = &a.enrichment.category {
        details.push(category.replace('_', " "));
    }
    details.push(format!("score {}", a.score()));
    if let Some(n) = a.novelty() {
        details.push(format!("nov {n}"));
    }
    if let Some(n) = a.sources_badge() {
        details.push(format!("{n} outlets"));
    }
    if !a.enrichment.tickers.is_empty() {
        details.push(a.enrichment.tickers.join(", "));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(details.join(" · ")).style(theme.subtle()));

    if let Some(insights) = &a.enrichment.ai_trading_insights {
        // Keep the selected company even if it was fifth or later in the
        // API response. Missing analyses do not consume a display slot.
        let others = insights
            .ticker_analysis
            .iter()
            .filter(|t| t.impact_analysis.is_some())
            .filter(|t| selected.is_none_or(|s| !t.ticker.eq_ignore_ascii_case(&s.ticker)))
            .take(if selected.is_some() { 3 } else { 4 });
        for t in others {
            lines.push(Line::from(""));
            lines.extend(impact_lines(t, theme));
        }

        if let Some(tv) = &insights.news_trading_value {
            let mut parts: Vec<String> = Vec::new();
            if let Some(act) = &tv.actionability_score {
                parts.push(format!("actionability {act}"));
            }
            if let Some(n) = a.novelty() {
                parts.push(format!("novelty {n}"));
            }
            if let Some(t) = &tv.timing_relevance {
                parts.push(format!("timing: {t}"));
            }
            if !parts.is_empty() {
                lines.push(Line::from(""));
                lines.push(section("Trading value", theme));
                lines.push(Line::from(format!("  {}", parts.join(" · "))));
            }
        }
    }

    if let Some(ctx) = &a.enrichment.news_context_enhancement {
        let mut body: Vec<Line> = Vec::new();
        if let Some(b) = &ctx.background_context {
            body.push(Line::from(format!("  {b}")));
        }
        if !ctx.key_entities.is_empty() {
            let entities: Vec<String> = ctx
                .key_entities
                .iter()
                .take(3)
                .map(|e| {
                    let kind = e
                        .kind
                        .as_deref()
                        .map(|k| format!(" ({k})"))
                        .unwrap_or_default();
                    let desc = e
                        .description
                        .as_deref()
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default();
                    format!("{}{kind}{desc}", e.name)
                })
                .collect();
            body.push(Line::from(format!("  entities: {}", entities.join("; "))));
        }
        if let Some(m) = &ctx.market_relevance_summary {
            body.push(Line::from(format!("  market: {m}")));
        }
        if !body.is_empty() {
            lines.push(Line::from(""));
            lines.push(section("Context", theme));
            lines.extend(body);
        }
    }

    if let Some(alt) = a
        .enrichment
        .ai_trading_insights
        .as_ref()
        .and_then(|i| i.alternative_perspectives.as_ref())
    {
        let mut body: Vec<Line> = Vec::new();
        if let Some(c) = &alt.contrarian_view {
            body.push(Line::from(format!("  contrarian: {c}")));
        }
        if let Some(o) = &alt.overlooked_factors {
            body.push(Line::from(format!("  overlooked: {o}")));
        }
        if !body.is_empty() {
            lines.push(Line::from(""));
            lines.push(section("Other views", theme));
            lines.extend(body);
        }
    }

    lines
}

fn impact_lines(t: &TickerAnalysis, theme: &Theme) -> Vec<Line<'static>> {
    let Some(i) = &t.impact_analysis else {
        return Vec::new();
    };
    let mut head = vec![
        Span::styled(t.ticker.clone(), Style::new().fg(theme.accent).bold()),
        Span::styled(" · AI impact", theme.subtle()),
        sentiment_span(i.sentiment.as_deref(), theme),
    ];
    if let Some(c) = &i.confidence {
        head.push(Span::styled(format!(" · {c} confidence"), theme.subtle()));
    }
    let mut lines = vec![Line::from(head)];
    if let Some(text) = i.summary.as_ref().or(i.reasoning.as_ref()) {
        lines.push(Line::from(text.clone()));
    }
    if let Some(p) = &i.price_impact_prediction {
        lines.push(Line::from(format!("Price outlook: {p}")));
    }
    lines
}

/// Facts extracted from Form 4, kept separate from AI interpretation.
fn trade_lines(a: &Article, extra: Option<&str>, theme: &Theme) -> Vec<Line<'static>> {
    let Some(t) = &a.insider else {
        return extra
            .map(|s| vec![Line::from(s.to_string())])
            .unwrap_or_default();
    };
    let side = t.side.as_deref().unwrap_or("trade");
    let color = match side {
        "buy" => theme.pos,
        "sell" => theme.neg,
        _ => theme.text,
    };
    let mut head = vec![Span::styled(
        side.to_uppercase(),
        Style::new().fg(color).bold(),
    )];
    if let Some(v) = &t.total_value_usd {
        head.push(Span::raw(format!(" {}", alphai::fmt_usd(v))).bold());
    }
    if t.is_10b5_1 {
        head.push(Span::styled(" · 10b5-1 plan", theme.subtle()));
    }
    let mut lines = vec![Line::from(head)];
    if let Some(extra) = extra {
        lines.push(Line::from(extra.to_string()));
    }
    let mut trade = Vec::new();
    if let Some(sh) = &t.shares {
        trade.push(format!("{} sh", alphai::fmt_shares(sh)));
    }
    if let Some(p) = &t.avg_price_usd {
        let price = p
            .parse::<f64>()
            .map(crate::domain::fmt_price)
            .unwrap_or_else(|_| p.clone());
        trade.push(format!("@ ${price}"));
    }
    if let Some(code) = &t.transaction_code {
        trade.push(format!("(code {code})"));
    }
    if !trade.is_empty() {
        lines.push(Line::from(trade.join(" ")));
    }
    let mut when = Vec::new();
    if let Some(d) = &t.transaction_date {
        when.push(d.clone());
    }
    if let Some(form) = &a.original.ownership_form {
        when.push(format!("{form} holdings"));
    }
    if !when.is_empty() {
        lines.push(Line::from(when.join(" · ")).style(theme.subtle()));
    }
    lines
}

fn section(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::new().fg(theme.accent),
    ))
}

/// The metrics a card has room for, most reported first. Prefix matching,
/// not `contains`: "Cost of revenue" must not answer for revenue, nor
/// "Basic earnings per share" for the diluted one. Filings that name their
/// lines differently (a bank's net interest income, a REIT's FFO) fall back
/// to whatever the filing itself compared against a prior period.
fn card_metrics(metrics: &[KeyMetric]) -> Vec<&KeyMetric> {
    const PRIORITY: [&str; 8] = [
        "revenue",
        "total net sales",
        "net sales",
        "gross margin",
        "operating income",
        "net income",
        "diluted earnings per share",
        "free cash flow",
    ];
    const CAP: usize = 5;

    let bare = |m: &KeyMetric| {
        let name = m.name.to_lowercase();
        name.strip_prefix("non-gaap ").unwrap_or(&name).to_string()
    };
    let mut picked: Vec<usize> = Vec::new();
    for want in PRIORITY {
        if picked.len() == CAP {
            break;
        }
        if let Some(i) = metrics
            .iter()
            .enumerate()
            .position(|(i, m)| !picked.contains(&i) && bare(m).starts_with(want))
        {
            picked.push(i);
        }
    }
    for (i, m) in metrics.iter().enumerate() {
        if picked.len() == CAP {
            break;
        }
        if !picked.contains(&i) && (m.yoy_change.is_some() || m.qoq_change.is_some()) {
            picked.push(i);
        }
    }
    picked.into_iter().filter_map(|i| metrics.get(i)).collect()
}

/// The verdict word, in the site's colors. The metric changes stay
/// uncolored: whether a rise is good depends on the metric.
fn verdict_span(verdict: &str, theme: &Theme) -> Span<'static> {
    let style = match verdict {
        "strong" | "solid" => Style::new().fg(theme.pos),
        "weak" => Style::new().fg(theme.neg),
        _ => theme.subtle(),
    };
    Span::styled(format!(" {verdict}"), style)
}

fn sentiment_span(sentiment: Option<&str>, theme: &Theme) -> Span<'static> {
    match sentiment {
        Some("positive") => Span::styled(" ▲ positive", Style::new().fg(theme.pos)),
        Some("negative") => Span::styled(" ▼ negative", Style::new().fg(theme.neg)),
        Some(other) => Span::styled(format!(" · {other}"), theme.subtle()),
        None => Span::raw(""),
    }
}

/// Rendered height of `lines` at `width` under character wrapping. `Wrap`
/// breaks on word boundaries, so this slightly overestimates fit; good
/// enough to clamp the scroll.
fn wrapped_height(lines: &[Line], width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    lines
        .iter()
        .map(|l| ((l.width() as u16).div_ceil(width)).max(1))
        .sum()
}
