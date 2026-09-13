//! Full-article card: a modal overlay (v in the News/Insider views) showing
//! everything the AlphAI enrichment carries for the selected article. Pure
//! render — the data was already fetched with the list, so opening the card
//! costs no API requests.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::alphai::{self, Article, EarningsRead, KeyMetric};
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
        .panel()
        .title(app.theme.heading(" Article "))
        .border_style(Style::new().fg(app.theme.accent));
    let earnings = app.find_earnings_by_uid(&a.original.uid);
    let lines = card_lines(a, app.selected_symbol(), earnings, &app.theme);
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
    let block = theme.panel().title(news::hint_title(
        " card ",
        "· pgup/pgdn scroll · v full ",
        theme,
    ));
    let Some(a) = article else {
        f.render_widget(block, area);
        return;
    };
    let lines = card_lines(a, symbol, earnings, theme);
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

/// The card body: title, meta, summary, then every enrichment section that
/// exists for this article (all of them optional on the wire).
fn card_lines(
    a: &Article,
    symbol: &str,
    earnings: Option<&EarningsRead>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(a.original.title.clone()).bold(),
        Line::from(news::meta_line(a, symbol).join(" · ")).dim(),
    ];
    if !a.original.summary.is_empty() {
        lines.push(Line::from(""));
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
        lines.push(Line::from("  press 6 for the full read").dim());
    } else if alphai::is_earnings_filing(a) {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Earnings read", Style::new().fg(theme.accent)),
            Span::raw(" · press 6").dim(),
        ]));
    }

    // Insider rows: the structured Form 4 event, straight from the filing.
    if let Some(t) = &a.insider {
        let mut trade: Vec<String> = Vec::new();
        if let Some(side) = &t.side {
            trade.push(side.to_uppercase());
        }
        if let Some(sh) = t.shares.as_deref() {
            trade.push(format!("{} sh", crate::alphai::fmt_shares(sh)));
        }
        // Per-share price keeps its cents; fmt_usd would band it to "$187".
        if let Some(p) = t.avg_price_usd.as_deref() {
            match p.parse::<f64>() {
                Ok(v) => trade.push(format!("@ ${}", crate::domain::fmt_price(v))),
                Err(_) => trade.push(format!("@ {p}")),
            }
        }
        if let Some(v) = t.total_value_usd.as_deref() {
            trade.push(format!("= {}", crate::alphai::fmt_usd(v)));
        }
        if let Some(code) = &t.transaction_code {
            trade.push(format!("(code {code})"));
        }
        let mut who: Vec<String> = Vec::new();
        if !t.insider_name.is_empty() {
            let role = t.role();
            who.push(if role.is_empty() {
                t.insider_name.clone()
            } else {
                format!("{} ({role})", t.insider_name)
            });
        }
        if let Some(d) = &t.transaction_date {
            who.push(d.clone());
        }
        if t.is_10b5_1 {
            who.push("pre-arranged 10b5-1 plan".to_string());
        }
        if !trade.is_empty() || !who.is_empty() {
            lines.push(Line::from(""));
            lines.push(section("Trade", theme));
            if !trade.is_empty() {
                lines.push(Line::from(format!("  {}", trade.join(" "))));
            }
            if !who.is_empty() {
                lines.push(Line::from(format!("  {}", who.join(" · "))).dim());
            }
        }
    }

    if let Some(insights) = &a.enrichment.ai_trading_insights {
        for t in insights.ticker_analysis.iter().take(4) {
            let Some(i) = &t.impact_analysis else {
                continue;
            };
            lines.push(Line::from(""));
            let mut head = vec![Span::styled(
                t.ticker.clone(),
                Style::new().fg(theme.accent).bold(),
            )];
            head.push(sentiment_span(i.sentiment.as_deref(), theme));
            if let Some(c) = &i.confidence {
                head.push(Span::raw(format!(" · {c} confidence")).dim());
            }
            lines.push(Line::from(head));
            if let Some(p) = &i.price_impact_prediction {
                lines.push(Line::from(format!("  price: {p}")));
            }
            if let Some(text) = i.summary.as_ref().or(i.reasoning.as_ref()) {
                lines.push(Line::from(format!("  {text}")).dim());
            }
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
            body.push(Line::from(format!("  entities: {}", entities.join("; "))).dim());
        }
        if let Some(m) = &ctx.market_relevance_summary {
            body.push(Line::from(format!("  market: {m}")).dim());
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
        _ => Style::new().dim(),
    };
    Span::styled(format!(" {verdict}"), style)
}

fn sentiment_span(sentiment: Option<&str>, theme: &Theme) -> Span<'static> {
    match sentiment {
        Some("positive") => Span::styled(" ▲ positive", Style::new().fg(theme.pos)),
        Some("negative") => Span::styled(" ▼ negative", Style::new().fg(theme.neg)),
        Some(other) => Span::raw(format!(" · {other}")).dim(),
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
