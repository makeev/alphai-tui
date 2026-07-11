//! Full-article card: a modal overlay (v in the News/Insider views) showing
//! everything the AlphaAI enrichment carries for the selected article. Pure
//! render — the data was already fetched with the list, so opening the card
//! costs no API requests.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::alphai::Article;
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
    let block = Block::bordered()
        .title(" Article ")
        .border_style(Style::new().fg(app.theme.accent));
    let lines = card_lines(a, app.selected_symbol(), &app.theme);
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
    scroll: &mut u16,
    theme: &Theme,
) {
    let block = Block::bordered().title(" card · pgup/pgdn scroll · v full ");
    let Some(a) = article else {
        f.render_widget(block, area);
        return;
    };
    let lines = card_lines(a, symbol, theme);
    render_card(f, area, block, lines, scroll);
}

/// Shared card body: clamps the scroll to the wrapped height and renders.
/// The height estimate divides by character width instead of real word-wrap
/// points, so it can be off by a line or two on very long cards; harmless
/// for a clamp.
fn render_card(f: &mut Frame, area: Rect, block: Block, lines: Vec<Line<'static>>, scroll: &mut u16) {
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
fn card_lines(a: &Article, symbol: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(a.original.title.clone()).bold(),
        Line::from(news::meta_line(a, symbol).join(" · ")).dim(),
    ];
    if !a.original.summary.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(a.original.summary.clone()));
    }

    if let Some(insights) = &a.enrichment.ai_trading_insights {
        for t in insights.ticker_analysis.iter().take(4) {
            let Some(i) = &t.impact_analysis else { continue };
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
                    let kind = e.kind.as_deref().map(|k| format!(" ({k})")).unwrap_or_default();
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
