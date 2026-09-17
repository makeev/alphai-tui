//! News marks on the price chart: the ticker's scored headlines placed on
//! the candles they were published in, so "why did it move" is answered
//! where the move is, not in another view.
//!
//! The rows come from the news bundle the News and Split views already
//! fetched for that ticker (`App::ticker_articles`), so the marks cost no
//! request and are simply absent until one of those views has loaded the
//! feed. Placement is by publish time, never by arrival: three rows in four
//! reach the feed later than they were published, and a mark that lies
//! about when something happened is worse than no mark at all.

use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};

use crate::alphai::Article;
use crate::domain::{Candle, Interval};
use crate::theme::Theme;
use crate::ui::{ellipsize, news};

/// Columns the chart needs before its bottom border names the newest mark;
/// narrower than this (the Split view's half-width chart) the marks stand
/// on their own.
const HEADLINE_MIN_WIDTH: u16 = 46;

/// Columns a headline is worth rendering in at all, once the glyph and the
/// age have taken theirs.
const TITLE_MIN_WIDTH: usize = 18;

/// One placed mark: an article and the chart column it belongs to.
pub(crate) struct Mark<'a> {
    /// Index into the candles the chart actually draws.
    pub col: usize,
    pub article: &'a Article,
}

/// At most one mark per column: the highest-scoring article published
/// inside that candle's slot, the newer one on a tie. Rows older than the
/// first candle on screen are dropped rather than clamped to the left edge,
/// and rows without a parsable timestamp are never placed. A candle ends
/// after its interval, so overnight gaps and missing recent bars cannot
/// pin unrelated news onto the previous candle.
pub(crate) fn place<'a>(
    articles: &'a [Article],
    candles: &[Candle],
    interval: Interval,
) -> Vec<Mark<'a>> {
    let Some(first) = candles.first() else {
        return Vec::new();
    };
    // Column -> (score, published, article); the tuple is the tie-break.
    let mut best: Vec<Option<(i64, i64, &Article)>> = vec![None; candles.len()];
    for a in articles {
        let Some(ts) = a.published().map(|t| t.timestamp()) else {
            continue;
        };
        if ts < first.ts {
            continue;
        }
        let col = candles.partition_point(|c| c.ts <= ts) - 1;
        if ts >= candles[col].ts.saturating_add(interval.secs()) {
            continue;
        }
        let cand = (a.score(), ts, a);
        if best[col].is_none_or(|(score, at, _)| (score, at) < (cand.0, cand.1)) {
            best[col] = Some(cand);
        }
    }
    best.into_iter()
        .enumerate()
        .filter_map(|(col, slot)| slot.map(|(_, _, article)| Mark { col, article }))
        .collect()
}

/// The mark the bottom border names: the freshest one on screen, which is
/// the "what just happened" a reader looks for.
pub(crate) fn latest<'a, 'm>(marks: &'m [Mark<'a>]) -> Option<&'m Mark<'a>> {
    marks
        .iter()
        .max_by_key(|m| m.article.published().map(|t| t.timestamp()).unwrap_or(0))
}

/// Shape of a mark: the AI sentiment call for this ticker, in the same
/// alphabet the news list's sentiment column uses.
fn shape(a: &Article, symbol: &str) -> char {
    match news::display_impact(a, symbol).and_then(|i| i.sentiment.as_deref()) {
        Some("positive") => '▲',
        Some("negative") => '▼',
        _ => '◆',
    }
}

/// Style of a mark: the accent color, never the sentiment's own green and
/// red. On a price chart those two colors already mean "up" and "down", so
/// a bullish mark drawn in green sinks into the candle it sits on; in the
/// accent it reads as a different kind of thing entirely, which is what it
/// is. The shape carries the sentiment and the weight the relevance score,
/// the same three bands the list's score column uses.
pub(crate) fn style(a: &Article, theme: &Theme) -> Style {
    let style = Style::new().fg(theme.accent);
    match a.score() {
        8..=10 => style.add_modifier(Modifier::BOLD),
        ..=5 => style.add_modifier(Modifier::DIM),
        _ => style,
    }
}

/// Glyph and style of one mark, for the candle chart.
pub(crate) fn glyph(a: &Article, symbol: &str, theme: &Theme) -> (char, Style) {
    (shape(a, symbol), style(a, theme))
}

/// The chart's bottom border: the freshest mark's glyph, age and headline.
/// None when the chart is too narrow to say anything a reader could use.
pub(crate) fn headline(
    mark: &Mark,
    symbol: &str,
    width: u16,
    theme: &Theme,
) -> Option<Line<'static>> {
    if width < HEADLINE_MIN_WIDTH {
        return None;
    }
    let a = mark.article;
    let (ch, style) = glyph(a, symbol, theme);
    let age = a.age(chrono::Utc::now());
    let prefix = if age.is_empty() {
        " ".to_string()
    } else {
        format!(" {age} · ")
    };
    // Two columns for the glyph and its space, one for the trailing space,
    // two more for the border corners the title must not run into.
    let room = (width as usize).saturating_sub(prefix.chars().count() + 5);
    if room < TITLE_MIN_WIDTH {
        return None;
    }
    Some(Line::from(vec![
        Span::styled(format!(" {ch}"), style),
        Span::styled(prefix, Style::new().dim()),
        Span::raw(ellipsize(&a.original.title, room)).dim(),
        Span::raw(" "),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(start: i64, step: i64, n: usize) -> Vec<Candle> {
        (0..n as i64)
            .map(|i| Candle {
                feed: Default::default(),
                ts: start + i * step,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume: None,
            })
            .collect()
    }

    fn row(ts: i64, score: i64, sentiment: &str) -> Article {
        let published = chrono::DateTime::from_timestamp(ts, 0)
            .unwrap()
            .to_rfc3339();
        serde_json::from_str(&format!(
            r#"{{
              "original": {{"title": "Headline at {ts}", "uid": "u{ts}",
                            "time_published": "{published}"}},
              "enrichment": {{"tickers": ["AAPL"], "relevance_score": {score},
                "ai_trading_insights": {{"ticker_analysis": [
                  {{"ticker": "AAPL", "impact_analysis": {{"sentiment": "{sentiment}"}}}}
                ]}}}}
            }}"#
        ))
        .unwrap()
    }

    #[test]
    fn rows_land_on_the_candle_they_were_published_in() {
        let candles = candles(1_000, 300, 5); // 1000, 1300, 1600, 1900, 2200
        let articles = vec![
            row(1_450, 7, "positive"), // inside candle 1
            row(2_300, 7, "negative"), // after the last candle's open: candle 4
            row(1_000, 7, "neutral"),  // exactly on the first candle
        ];
        let marks = place(&articles, &candles, Interval::M5);
        let cols: Vec<usize> = marks.iter().map(|m| m.col).collect();
        assert_eq!(cols, vec![0, 1, 4]);
    }

    #[test]
    fn history_older_than_the_window_is_dropped_not_clamped() {
        let candles = candles(1_000, 300, 3);
        let old = [row(900, 9, "positive")];
        let marks = place(&old, &candles, Interval::M5);
        assert!(marks.is_empty(), "an older row must not stick to the edge");
    }

    #[test]
    fn news_after_the_last_candle_is_not_pinned_to_old_history() {
        let candles = candles(1_000, 300, 3);
        let articles = vec![row(1_899, 6, "positive"), row(1_900, 9, "negative")];
        let marks = place(&articles, &candles, Interval::M5);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].article.score(), 6);
    }

    #[test]
    fn news_in_a_session_gap_is_not_assigned_to_the_previous_session() {
        let mut candles = candles(1_000, 300, 2);
        candles.extend(self::candles(90_000, 300, 2));
        let articles = vec![row(2_000, 9, "positive")];
        assert!(place(&articles, &candles, Interval::M5).is_empty());
    }

    #[test]
    fn a_crowded_candle_keeps_its_biggest_story() {
        let candles = candles(1_000, 300, 2);
        let articles = vec![
            row(1_050, 6, "positive"),
            row(1_100, 9, "negative"), // the one that matters
            row(1_200, 6, "neutral"),
        ];
        let marks = place(&articles, &candles, Interval::M5);
        assert_eq!(marks.len(), 1, "one mark per column");
        assert_eq!(marks[0].article.score(), 9);
    }

    #[test]
    fn the_shape_is_the_sentiment_and_the_weight_is_the_score() {
        let theme = Theme::default();
        assert_eq!(glyph(&row(1, 9, "positive"), "AAPL", &theme).0, '▲');
        assert_eq!(glyph(&row(1, 9, "negative"), "AAPL", &theme).0, '▼');
        assert_eq!(glyph(&row(1, 9, "neutral"), "AAPL", &theme).0, '◆');
        let (_, loud) = glyph(&row(1, 9, "positive"), "AAPL", &theme);
        let (_, quiet) = glyph(&row(1, 3, "negative"), "AAPL", &theme);
        assert!(loud.add_modifier.contains(Modifier::BOLD));
        assert!(quiet.add_modifier.contains(Modifier::DIM));
        // The color says "news", not "up" or "down": both marks wear the
        // accent, whichever way their sentiment goes.
        assert_eq!(loud.fg, Some(theme.accent));
        assert_eq!(quiet.fg, Some(theme.accent));
    }

    #[test]
    fn the_headline_yields_on_a_narrow_chart() {
        let candles = candles(1_000, 300, 2);
        let articles = vec![row(1_050, 9, "positive")];
        let marks = place(&articles, &candles, Interval::M5);
        let mark = latest(&marks).unwrap();
        let theme = Theme::default();
        assert!(headline(mark, "AAPL", 40, &theme).is_none());
        assert!(headline(mark, "AAPL", 80, &theme).is_some());
    }
}
