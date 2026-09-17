//! The quote rail: one line under the header that keeps the selected
//! ticker's price on screen in every view.
//!
//! Only Table, Split and Chart ever showed a price, so in News, Insider and
//! Earnings the ticker was a word in a frame title and nothing more: the
//! reader could not see what it was doing, ←→ moved between tickers blind,
//! and the tick pulse (`App::price_flash`) was invisible. The rail carries
//! the selected quote, where it sits in the day, what the US session is
//! doing, and the rest of the watchlist compressed.
//!
//! Everything past the symbol, price and cache label is optional: zones go in in
//! priority order while they fit, the way the watchlist table drops whole
//! columns instead of squeezing every one of them.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::domain::{Candle, Quote, fmt_price, fmt_volume};
use crate::market::{self, Session};
use crate::portfolio::fmt_signed;
use crate::theme::Theme;
use crate::ui::chart::{dir_color, flash_style, move_color};
use crate::ui::table::spark_line;

/// Cells between the brackets of the day-range track.
const TRACK: usize = 8;
/// Cells of the rail's sparkline (the table's own is wider).
const SPARK: usize = 8;
/// Below this terminal height the rail is dropped: the row buys more as
/// list space than as a quote.
pub const MIN_HEIGHT: u16 = 12;

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(Paragraph::new(line(app, area.width, Utc::now())), area);
}

/// The rail as one line, fitted to `width`. Split out of `render`, with the
/// clock passed in, so the degradation ladder is testable at a fixed
/// moment (the session countdown changes the line's width).
pub(crate) fn line(app: &App, width: u16, now: DateTime<Utc>) -> Line<'static> {
    let symbol = app.selected_symbol().to_string();
    let budget = width as usize;
    let mut spans = head(app, &symbol);
    let mut used = total_width(&spans);

    for alternatives in optional_zones(app, &symbol, now) {
        // Widest first: the first form that still fits goes in, and a zone
        // with no form that fits is skipped rather than ending the line.
        // A cheap zone behind an expensive one therefore survives.
        if let Some(zone) = alternatives
            .into_iter()
            .find(|z| !z.is_empty() && used + total_width(z) <= budget)
        {
            used += total_width(&zone);
            spans.extend(zone);
        }
    }

    // What is left goes to the peers, right-aligned: the rail then answers
    // "where does ← → take me" as well as "what is this one doing".
    let peers = peers(app, &symbol, budget.saturating_sub(used));
    let peers_w = total_width(&peers);
    if peers_w > 0 {
        spans.push(Span::raw(" ".repeat(budget - used - peers_w)));
        spans.extend(peers);
    }
    Line::from(spans)
}

/// Symbol, price and freshness: the part that is never dropped.
fn head(app: &App, symbol: &str) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let mut out = vec![Span::styled(
        format!(" {symbol} "),
        Style::new().bold().fg(theme.accent),
    )];
    match app.data.get(symbol) {
        Some(data) => {
            // The pulse is the same one the chart title and the table use,
            // so a live market now reads as live in every view.
            let style = app
                .price_flash_dir(symbol)
                .map_or(Style::new().bold(), |up| flash_style(up, theme));
            out.push(Span::styled(fmt_price(data.quote.price), style));
            if let Some(age) = app.cached_age(symbol) {
                out.push(Span::styled(
                    format!("  cached {age} ago"),
                    Style::new().fg(theme.warn),
                ));
            }
        }
        None if app.errors.contains_key(symbol) => {
            out.push(Span::styled("error", Style::new().fg(theme.error)));
        }
        None => out.push(Span::raw("…").dim()),
    }
    out
}

/// The optional zones in priority order, each as its forms from widest to
/// narrowest (`line` picks one form per zone).
fn optional_zones(app: &App, symbol: &str, now: DateTime<Utc>) -> Vec<Vec<Vec<Span<'static>>>> {
    let theme = &app.theme;
    let Some(data) = app.data.get(symbol) else {
        return vec![event_zone(app, symbol, now)];
    };
    let quote = &data.quote;
    let color = dir_color(quote, theme);
    let mut zones = Vec::new();

    if let (Some(change), Some(pct)) = (quote.change(), quote.change_pct()) {
        let arrow = if change > 0.0 {
            "▲"
        } else if change < 0.0 {
            "▼"
        } else {
            "·"
        };
        zones.push(vec![
            vec![Span::styled(
                format!("  {arrow} {change:+.2} {pct:+.2}%"),
                Style::new().fg(color),
            )],
            // On a narrow terminal the percentage alone still says which
            // way the day is going, and the color says it without the
            // arrow: this zone must outlive the badges behind it.
            vec![Span::styled(
                format!("  {arrow} {pct:+.2}%"),
                Style::new().fg(color),
            )],
            vec![Span::styled(
                format!("  {pct:+.2}%"),
                Style::new().fg(color),
            )],
        ]);
    }
    zones.push(extended_zone(quote, now, theme));
    zones.push(position_zone(
        app,
        symbol,
        crate::portfolio::price_at(quote, now),
        theme,
    ));
    zones.push(session_zone(app, symbol, now));
    if let Some(note) = app.source_delay {
        zones.push(vec![vec![Span::styled(
            format!("  {note}"),
            Style::new().fg(theme.warn),
        )]]);
    }
    zones.push(range_zone(quote, &data.candles, color, theme));
    let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
    if !closes.is_empty() {
        zones.push(vec![vec![
            Span::raw("  ").dim(),
            Span::styled(spark_line(&closes, SPARK), Style::new().fg(color)),
        ]]);
    }
    zones.push(event_zone(app, symbol, now));
    // Last, because they are context rather than news: they say where the
    // day sits in the year and how heavily it traded, and a narrow terminal
    // gives their width back to everything above.
    if let Some((lo, hi)) = quote.fifty_two_week {
        zones.push(vec![vec![Span::styled(
            format!("  52w {}–{}", fmt_price(lo), fmt_price(hi)),
            Style::new().dim(),
        )]]);
    }
    if let Some(volume) = quote.volume.filter(|v| *v > 0.0) {
        zones.push(vec![vec![Span::styled(
            format!("  vol {}", fmt_volume(volume)),
            Style::new().dim(),
        )]]);
    }
    zones
}

fn event_zone(app: &App, symbol: &str, now: DateTime<Utc>) -> Vec<Vec<Span<'static>>> {
    let Some(flag) = super::calendar::flag(app, symbol, now) else {
        return Vec::new();
    };
    let style = if flag.urgent {
        Style::new().fg(app.theme.warn).bold()
    } else {
        Style::new().fg(app.theme.accent)
    };
    [flag.full, flag.compact]
        .into_iter()
        .map(|text| vec![Span::styled(format!("  {text}"), style)])
        .collect()
}

/// What this ticker has made its holder, for the rows that are held. It
/// sits ahead of the session badge on purpose: someone who owns the name
/// reads their own number first, and the badge says the same thing for
/// every symbol on the list. Absent for a ticker that is only watched, so
/// it costs nothing to the rest of the line.
fn position_zone(app: &App, symbol: &str, price: f64, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let Some(position) = app.position(symbol) else {
        return Vec::new();
    };
    let pnl = position.pnl(price);
    let style = Style::new().fg(move_color(Some(pnl), theme));
    let money = fmt_signed(pnl);
    let pct = position.pnl_pct(price);
    let mut forms = Vec::new();
    if let Some(pct) = pct {
        forms.push(vec![Span::styled(
            format!("  ×{} {money} {pct:+.2}%", position.qty),
            style,
        )]);
        forms.push(vec![Span::styled(format!("  {money} {pct:+.2}%"), style)]);
        forms.push(vec![Span::styled(format!("  {pct:+.2}%"), style)]);
    } else {
        // No cost basis to measure against, so the money is all there is.
        forms.push(vec![Span::styled(
            format!("  ×{} {money}", position.qty),
            style,
        )]);
        forms.push(vec![Span::styled(format!("  {money}"), style)]);
    }
    forms
}

/// The extended-hours print, when there is one, measured against the
/// closing bell. This is the zone the rail exists for on a news view: a
/// filing lands at 20:00 and the headline price, which is the regular
/// close, cannot move until the next open. It sits ahead of the session
/// badge because it is the newer fact, and it disappears by itself during
/// the regular session, when there is no separate print to show.
fn extended_zone(quote: &Quote, now: DateTime<Utc>, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let session = market::clock_at(now).session;
    let Some(price) = quote.extended_price_at(now) else {
        if market::is_us_equity(&quote.symbol) && matches!(session, Session::Pre | Session::Post) {
            let name = if session == Session::Pre { "PRE" } else { "AH" };
            return vec![vec![Span::styled(
                format!("  {name} no data"),
                Style::new().fg(theme.flat),
            )]];
        }
        return Vec::new();
    };
    let reference = quote.extended_reference();
    let change = price - reference;
    let pct = if reference != 0.0 {
        change / reference * 100.0
    } else {
        0.0
    };
    let print_session = quote
        .timing
        .extended
        .and_then(market::window_at)
        .map(|w| w.session)
        .unwrap_or(session);
    let label = if print_session == Session::Pre {
        "PRE"
    } else {
        "AH"
    };
    let detail = quote
        .timing
        .extended
        .map(|ts| {
            let age = now.timestamp().saturating_sub(ts).max(0) / 60;
            let time = chrono::DateTime::from_timestamp(ts, 0)
                .map(|t| {
                    market::et_time(t)
                        .format(if market::et_date(ts) == market::et_date(now.timestamp()) {
                            "%H:%M ET"
                        } else {
                            "%d %b %H:%M ET"
                        })
                        .to_string()
                })
                .unwrap_or_default();
            format!(
                " · {} · {time} · {} old",
                quote.timing.extended_feed.label(),
                if age >= 1440 {
                    format!("{}d{}h", age / 1440, age % 1440 / 60)
                } else if age >= 60 {
                    format!("{}h{}m", age / 60, age % 60)
                } else {
                    format!("{age}m")
                }
            )
        })
        .unwrap_or_default();
    let source = if quote.timing.extended.is_some() {
        format!(" · {}", quote.timing.extended_feed.label())
    } else {
        String::new()
    };
    let style = Style::new().fg(move_color(Some(change), theme));
    vec![
        vec![Span::styled(
            format!(
                "  {label} {} {change:+.2} {pct:+.2}%{detail}",
                fmt_price(price)
            ),
            style,
        )],
        vec![Span::styled(
            format!("  {label} {} {pct:+.2}%{source}", fmt_price(price)),
            style,
        )],
        vec![Span::styled(format!("  {label} {pct:+.2}%{source}"), style)],
    ]
}

/// Session badge plus the countdown to the next bell. Crypto pairs trade
/// around the clock, so they get the fact rather than a countdown.
fn session_zone(app: &App, symbol: &str, now: DateTime<Utc>) -> Vec<Vec<Span<'static>>> {
    let theme = &app.theme;
    if market::is_crypto(symbol) {
        return vec![vec![Span::styled(
            "  ● 24/7".to_string(),
            Style::new().fg(theme.pos),
        )]];
    }
    let clock = market::clock_at(now);
    let style = match clock.session {
        Session::Open => Style::new().fg(theme.pos),
        Session::Pre | Session::Post => Style::new().fg(theme.warn),
        Session::Closed => Style::new().dim(),
    };
    let badge = Span::styled(format!("  {}", clock.session.label()), style);
    vec![
        vec![
            badge.clone(),
            Span::styled(format!(" {}", clock.countdown()), Style::new().dim()),
        ],
        vec![badge],
    ]
}

/// Where the price sits between the session's low and high:
/// `218.13├───●────┤227.42`.
fn range_zone(
    quote: &Quote,
    candles: &[Candle],
    color: Color,
    theme: &Theme,
) -> Vec<Vec<Span<'static>>> {
    // The source's own figure first: it is the regular session's range by
    // definition, while the candles are whatever was fetched, which now
    // includes the extended sessions when those are switched on.
    let Some((lo, hi)) = quote.day_range.or_else(|| day_range(candles)) else {
        return Vec::new();
    };
    let price = quote.price;
    let span = hi - lo;
    let pos = if span > 0.0 {
        (((price - lo) / span) * (TRACK - 1) as f64).round() as isize
    } else {
        (TRACK / 2) as isize
    }
    .clamp(0, TRACK as isize - 1) as usize;
    let bars = |lead: &str| {
        vec![
            Span::styled(
                format!("{lead}├{}", "─".repeat(pos)),
                Style::new().fg(theme.ref_line),
            ),
            Span::styled("●", Style::new().fg(color)),
            Span::styled(
                format!("{}┤", "─".repeat(TRACK - 1 - pos)),
                Style::new().fg(theme.ref_line),
            ),
        ]
    };
    let mut labelled = vec![Span::styled(
        format!("  {}", fmt_price(lo)),
        Style::new().dim(),
    )];
    labelled.extend(bars(""));
    labelled.push(Span::styled(fmt_price(hi), Style::new().dim()));
    // The bare track keeps the position when the numbers do not fit; the
    // watchlist table carries the low and high anyway.
    vec![labelled, bars("  ")]
}

/// Low and high of the session the newest candle belongs to. Daily bars
/// put one candle in a day, so this is that bar's own range; intraday bars
/// fold into their New York date, extended-hours ones included (which is
/// what a source that sends them is showing as the day anyway).
fn day_range(candles: &[Candle]) -> Option<(f64, f64)> {
    let last = candles.last()?;
    let day = market::et_date(last.ts)?;
    let (lo, hi) = candles
        .iter()
        .filter(|c| market::et_date(c.ts) == Some(day))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| {
            (lo.min(c.low), hi.max(c.high))
        });
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// The rest of the watchlist as percentages, in watchlist order, as many
/// as `room` fits. A ticker with no data yet reads as a dash rather than
/// vanishing: the row it is missing from is the answer to "is it loading".
fn peers(app: &App, selected: &str, room: usize) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for symbol in app.symbols.iter().filter(|s| s.as_str() != selected) {
        let (text, style) = match app.data.get(symbol).and_then(|d| d.quote.change_pct()) {
            Some(pct) => (
                format!("{pct:+.2}%"),
                Style::new().fg(dir_color(&app.data[symbol].quote, theme)),
            ),
            None => ("—".to_string(), Style::new().dim()),
        };
        let group = vec![
            Span::styled(format!("  {symbol} "), Style::new().dim()),
            Span::styled(text, style),
        ];
        let w = total_width(&group);
        if used + w > room {
            break;
        }
        used += w;
        out.extend(group);
    }
    out
}

fn total_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| s.width()).sum()
}

/// The rail's text, as a reader sees it.
#[cfg(test)]
pub(crate) fn text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(day_ts: i64) -> Vec<Candle> {
        (0..6)
            .map(|i| Candle {
                feed: Default::default(),
                ts: day_ts + i * 300,
                open: 100.0 + i as f64,
                high: 101.0 + i as f64,
                low: 99.0 + i as f64,
                close: 100.5 + i as f64,
                volume: Some(1000.0),
            })
            .collect()
    }

    #[test]
    fn day_range_folds_one_new_york_session() {
        // 14:00 ET on 10 September 2026 plus the following half hour.
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 10)
            .unwrap()
            .and_hms_opt(18, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let mut series = candles(day);
        // A bar from the session before must not widen today's range.
        series.insert(
            0,
            Candle {
                feed: Default::default(),
                ts: day - 86_400,
                open: 50.0,
                high: 300.0,
                low: 10.0,
                close: 60.0,
                volume: None,
            },
        );
        assert_eq!(day_range(&series), Some((99.0, 106.0)));
        assert_eq!(day_range(&[]), None);
    }
}
