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
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::pad_right;
use crate::app::App;
use crate::domain::{Candle, Quote, fmt_price, fmt_volume};
use crate::market::{self, Session};
use crate::portfolio::fmt_signed;
use crate::theme::Theme;
use crate::ui::chart::{dir_color, move_color};
use crate::ui::table::spark_line;

/// Cells between the brackets of the day-range track.
const TRACK: usize = 8;
/// Cells of the rail's sparkline (the table's own is wider).
const SPARK: usize = 8;
/// Below this terminal height the rail is dropped: the row buys more as
/// list space than as a quote.
pub const MIN_HEIGHT: u16 = 12;

pub(crate) fn visible(app: &App, height: u16) -> bool {
    app.show_rail && height >= MIN_HEIGHT
}

pub fn render(f: &mut Frame, area: Rect, app: &mut App) {
    let (line, peers) = line_with_peers(app, area.width, Utc::now());
    f.render_widget(Paragraph::new(line), area);
    app.hits.peers = peers
        .into_iter()
        .map(|(x, w, idx)| (Rect::new(area.x.saturating_add(x), area.y, w, 1), idx))
        .collect();
}

/// The rail as one line, fitted to `width`. Split out of `render`, with the
/// clock passed in, so the degradation ladder is testable at a fixed
/// moment (the session countdown changes the line's width).
#[cfg(test)]
pub(crate) fn line(app: &App, width: u16, now: DateTime<Utc>) -> Line<'static> {
    line_with_peers(app, width, now).0
}

/// The line plus, for the mouse, where each peer ticker starts, how wide
/// it is and which watchlist entry it is.
fn line_with_peers(
    app: &App,
    width: u16,
    now: DateTime<Utc>,
) -> (Line<'static>, Vec<(u16, u16, usize)>) {
    let symbol = app.focused_symbol();
    let budget = width as usize;
    let mut spans = head(app, &symbol, width);
    let mut used = total_width(&spans);
    // Zones are told apart by a thin rule, the parts of one zone by single
    // spaces: the eye groups by the wider gap, so the gaps inside a zone
    // must never be the wide ones. Borderless panels draw no lines, so
    // there the gap alone separates; a narrow terminal keeps two spaces.
    let separator = match (width >= 80, app.theme.panels) {
        (false, _) => Span::raw("  "),
        (true, crate::theme::Panels::Surface) => Span::raw("   "),
        (true, _) => Span::styled(" │ ", app.theme.faint()),
    };

    for alternatives in optional_zones(app, &symbol, now, width >= 80) {
        // Widest first: the first form that still fits goes in, and a zone
        // with no form that fits is skipped rather than ending the line.
        // A cheap zone behind an expensive one therefore survives.
        let zone = alternatives
            .into_iter()
            .filter(|z| !z.is_empty())
            .map(|z| separated(z, &separator))
            .find(|z| used + total_width(z) <= budget);
        if let Some(zone) = zone {
            used += total_width(&zone);
            spans.extend(zone);
        }
    }

    // What is left goes to the peers, right-aligned: the rail then answers
    // "where does ← → take me" as well as "what is this one doing".
    let peers = peers(app, &symbol, budget.saturating_sub(used), width >= 80);
    let peers_w = peers.iter().map(|(_, g)| total_width(g)).sum::<usize>();
    let mut zones = Vec::new();
    if peers_w > 0 {
        spans.push(Span::raw(" ".repeat(budget - used - peers_w)));
        let mut x = budget - peers_w;
        for (idx, group) in peers {
            let w = total_width(&group);
            zones.push((x as u16, w as u16, idx));
            x += w;
            spans.extend(group);
        }
    }
    (Line::from(spans), zones)
}

/// Symbol, price and freshness: the part that is never dropped.
fn head(app: &App, symbol: &str, width: u16) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let stable = width >= 80;
    let mut out = vec![Span::styled(
        format!(" {symbol} "),
        Style::new().bold().fg(theme.accent),
    )];
    if app.bare && width >= 60 {
        out.push(Span::styled(
            format!("{} ", app.refresh_marker()),
            theme.subtle(),
        ));
    }
    match app.data.get(symbol) {
        Some(data) => {
            // The pulse is the same one the chart title and the table use,
            // so a live market now reads as live in every view.
            let price_start = total_width(&out);
            let style = app.price_style(symbol, Style::new().fg(theme.text).bold());
            out.push(Span::styled(fmt_price(data.quote.price), style));
            if let Some(currency) = data
                .quote
                .currency
                .as_deref()
                .filter(|c| !c.is_empty() && (width >= 60 || *c != "USD"))
            {
                out.push(Span::styled(format!(" {currency}"), theme.subtle()));
            }
            if stable {
                out.push(Span::raw(
                    " ".repeat(10usize.saturating_sub(total_width(&out) - price_start)),
                ));
            }
            if let Some(age) = app.cached_age(symbol) {
                let age = if stable { pad_right(&age, 6) } else { age };
                out.push(Span::styled(
                    format!("  cached {age} ago"),
                    Style::new().fg(theme.warn),
                ));
            }
        }
        None if app.errors.contains_key(symbol) => {
            out.push(Span::styled("error", Style::new().fg(theme.error)));
        }
        None => out.push(Span::styled("…", theme.subtle())),
    }
    out
}

/// The optional zones in priority order, each as its forms from widest to
/// narrowest (`line` picks one form per zone).
fn optional_zones(
    app: &App,
    symbol: &str,
    now: DateTime<Utc>,
    stable: bool,
) -> Vec<Vec<Vec<Span<'static>>>> {
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
        let style = Style::new().fg(color);
        zones.push(vec![
            reserved(
                vec![Span::styled(
                    format!("{arrow} {change:+.2} {pct:+.2}%"),
                    style,
                )],
                17,
                stable,
            ),
            // On a narrow terminal the percentage alone still says which
            // way the day is going, and the color says it without the
            // arrow: this zone must outlive the badges behind it.
            vec![Span::styled(format!("{arrow} {pct:+.2}%"), style)],
            vec![Span::styled(format!("{pct:+.2}%"), style)],
        ]);
    }
    // Near-term events should remain visible before secondary quote context.
    zones.push(event_zone(app, symbol, now));
    // Price provenance outranks holdings, session countdowns and charts
    // when the trade behind the price is old (a recently fetched response
    // can still carry one); a current price keeps it at the end, where it
    // does not crowd the numbers a reader came for.
    let stale = quote_is_stale(quote, now);
    if stale {
        zones.push(quote_context(app, quote, now, stale));
    }
    zones.push(extended_zone(quote, now, theme, stable));
    zones.push(position_zone(
        app,
        symbol,
        crate::portfolio::price_at(quote, now),
        theme,
        stable,
    ));
    zones.push(super::alerts::rail_zone(app, symbol));
    zones.push(session_zone(app, symbol, now, stable));
    if !stale {
        zones.push(quote_context(app, quote, now, stale));
    }
    zones.push(range_zone(quote, &data.candles, color, theme));
    let closes: Vec<f64> = data.candles.iter().map(|c| c.close).collect();
    if !closes.is_empty() {
        zones.push(vec![vec![
            Span::raw("  "),
            Span::styled(spark_line(&closes, SPARK), Style::new().fg(color)),
        ]]);
    }
    // Last, because they are context rather than news: they say where the
    // day sits in the year and how heavily it traded, and a narrow terminal
    // gives their width back to everything above.
    if let Some((lo, hi)) = quote.fifty_two_week {
        zones.push(vec![vec![Span::styled(
            format!("52w {}–{}", fmt_price(lo), fmt_price(hi)),
            theme.subtle(),
        )]]);
    }
    if let Some(volume) = quote.volume.filter(|v| *v > 0.0) {
        zones.push(vec![vec![Span::styled(
            format!("vol {}", fmt_volume(volume)),
            theme.subtle(),
        )]]);
    }
    zones
}

/// The trade behind the headline price is old: more than 20 minutes during
/// the regular session (or at any hour for crypto). A delayed feed's quarter
/// of an hour is not old; a price that stopped moving is.
fn quote_is_stale(quote: &Quote, now: DateTime<Utc>) -> bool {
    let Some(ts) = quote.timing.regular else {
        return false;
    };
    let old = now.timestamp().saturating_sub(ts) > 20 * 60;
    if market::is_crypto(&quote.symbol) {
        return old;
    }
    old && market::is_us_equity(&quote.symbol) && market::clock_at(now).session == Session::Open
}

/// Where the headline price comes from and when it traded, in the faint
/// tier: `Tiingo 13 Nov 16:00`, the date only when it is not today. A
/// `stale` trade (`quote_is_stale`, decided once by the caller, which also
/// places the zone) takes the warning color and keeps a short form.
fn quote_context(
    app: &App,
    quote: &Quote,
    now: DateTime<Utc>,
    stale: bool,
) -> Vec<Vec<Span<'static>>> {
    let feed = quote.timing.regular_feed.label();
    let mut parts = Vec::new();
    if !feed.is_empty() {
        parts.push(feed.to_string());
    }
    if let Some(note) = app.source_delay.filter(|note| !feed.contains(note)) {
        parts.push(note.to_string());
    }
    let source = parts.join(" · ");
    let style = if stale {
        Style::new().fg(app.theme.warn)
    } else {
        app.theme.faint()
    };
    if let Some(ts) = quote.timing.regular {
        let us = market::is_us_equity(&quote.symbol);
        let today = market::et_date(ts) == market::et_date(now.timestamp());
        let format = if today { "%H:%M" } else { "%d %b %H:%M" };
        parts.push(super::time_axis::label(ts, format, app.chart.timezone, us));
    }
    // The source alone, without the time, only where it warns: a current
    // price's provenance either says when it traded or keeps quiet.
    let mut forms = vec![parts.join(" ")];
    if stale {
        forms.push(source);
    }
    forms
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| vec![Span::styled(s, style)])
        .collect()
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
fn position_zone(
    app: &App,
    symbol: &str,
    price: f64,
    theme: &Theme,
    stable: bool,
) -> Vec<Vec<Span<'static>>> {
    let Some(position) = app.position(symbol) else {
        return Vec::new();
    };
    let pnl = position.pnl(price);
    let style = Style::new().fg(move_color(Some(pnl), theme));
    let money = fmt_signed(pnl);
    let pct = position.pnl_pct(price);
    let qty = Span::styled(format!("×{} ", position.qty), theme.subtle());
    let mut forms = Vec::new();
    if let Some(pct) = pct {
        let pct = format!("{pct:+.2}%");
        forms.push(reserved(
            vec![qty, Span::styled(format!("{money} {pct}"), style)],
            26,
            stable,
        ));
        forms.push(vec![Span::styled(format!("{money} {pct}"), style)]);
        forms.push(vec![Span::styled(pct, style)]);
    } else {
        // No cost basis to measure against, so the money is all there is.
        forms.push(reserved(
            vec![qty, Span::styled(money.clone(), style)],
            18,
            stable,
        ));
        forms.push(vec![Span::styled(money, style)]);
    }
    forms
}

/// The extended-hours print, when there is one, measured against the
/// closing bell. This is the zone the rail exists for on a news view: a
/// filing lands at 20:00 and the headline price, which is the regular
/// close, cannot move until the next open. It sits ahead of the session
/// badge because it is the newer fact, and it disappears by itself during
/// the regular session, when there is no separate print to show.
fn extended_zone(
    quote: &Quote,
    now: DateTime<Utc>,
    theme: &Theme,
    stable: bool,
) -> Vec<Vec<Span<'static>>> {
    let session = market::clock_at(now).session;
    let Some(price) = quote.extended_price_at(now) else {
        if market::is_us_equity(&quote.symbol) && matches!(session, Session::Pre | Session::Post) {
            let name = if session == Session::Pre { "PRE" } else { "AH" };
            return vec![vec![
                Span::styled(format!("{name} "), theme.subtle()),
                Span::styled("no data", Style::new().fg(theme.flat)),
            ]];
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
    // The print's time (the date too when it is not today's) in the faint
    // tier, never louder than the move itself. Which feed carried it is
    // the chart title's business; here it only crowded the line.
    let time = quote.timing.extended.and_then(|ts| {
        let t = chrono::DateTime::from_timestamp(ts, 0)?;
        let today = market::et_date(ts) == market::et_date(now.timestamp());
        Some(
            market::et_time(t)
                .format(if today { "%H:%M" } else { "%d %b %H:%M" })
                .to_string(),
        )
    });
    let move_style = Style::new().fg(move_color(Some(change), theme));
    let core = |with_price: bool| {
        let mut spans = vec![Span::styled(format!("{label} "), theme.subtle())];
        if with_price {
            spans.push(Span::styled(
                format!("{} ", fmt_price(price)),
                Style::new().fg(theme.text),
            ));
        }
        spans.push(Span::styled(format!("{pct:+.2}%"), move_style));
        spans
    };
    let mut forms = Vec::new();
    if let Some(time) = &time {
        let mut timed = core(true);
        timed.push(Span::styled(format!(" · {time}"), theme.faint()));
        forms.push(reserved(timed, 30, stable));
    }
    forms.push(reserved(core(true), 17, stable));
    forms.push(core(false));
    forms
}

/// Session badge plus the countdown to the next bell. Crypto pairs trade
/// around the clock, so they get the fact rather than a countdown.
fn session_zone(
    app: &App,
    symbol: &str,
    now: DateTime<Utc>,
    stable: bool,
) -> Vec<Vec<Span<'static>>> {
    let theme = &app.theme;
    if market::is_crypto(symbol) {
        return vec![vec![Span::styled(
            "● 24/7".to_string(),
            Style::new().fg(theme.pos),
        )]];
    }
    let clock = market::clock_at(now);
    let style = match clock.session {
        Session::Open => Style::new().fg(theme.pos),
        Session::Pre | Session::Post => Style::new().fg(theme.warn),
        Session::Closed => theme.subtle(),
    };
    let label = clock.session.label();
    let countdown = clock.countdown();
    let badge = Span::styled(label.to_string(), style);
    vec![
        reserved(
            vec![
                badge.clone(),
                Span::styled(format!(" · {countdown}"), theme.faint()),
            ],
            26,
            stable,
        ),
        vec![badge],
    ]
}

/// Where the price sits between the session's low and high:
/// `218.13 ├───●────┤ 227.42`. Near the end of the line, so it needs no
/// reserved width: only the sparkline sits behind it.
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
    let mut labelled = vec![Span::styled(format!("{} ", fmt_price(lo)), theme.subtle())];
    labelled.extend(bars(""));
    labelled.push(Span::styled(format!(" {}", fmt_price(hi)), theme.subtle()));
    // The bare track keeps the position when the numbers do not fit; the
    // watchlist table carries the low and high anyway.
    vec![labelled, bars("")]
}

/// Low and high of the session the newest candle belongs to. Daily bars
/// put one candle in a day, so this is that bar's own range; intraday bars
/// fold into their New York date, extended-hours ones included (which is
/// what a source that sends them is showing as the day anyway).
pub(crate) fn day_range(candles: &[Candle]) -> Option<(f64, f64)> {
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
/// as `room` fits, each with its watchlist index. A ticker with no data
/// yet reads as a dash rather than vanishing: the row it is missing from
/// is the answer to "is it loading".
#[allow(clippy::type_complexity)]
fn peers(app: &App, selected: &str, room: usize, stable: bool) -> Vec<(usize, Vec<Span<'static>>)> {
    let theme = &app.theme;
    let mut out = Vec::new();
    let mut used = 0;
    for (idx, symbol) in app
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| s.as_str() != selected)
    {
        let (text, style) = match app.data.get(symbol).and_then(|d| d.quote.change_pct()) {
            Some(pct) => (
                format!("{pct:+.2}%"),
                Style::new().fg(dir_color(&app.data[symbol].quote, theme)),
            ),
            None => ("—".to_string(), theme.faint()),
        };
        let group = vec![
            Span::styled(format!("  {symbol} "), theme.subtle()),
            Span::styled(field(text, 8, stable), style),
        ];
        let w = total_width(&group);
        if used + w > room {
            break;
        }
        used += w;
        out.push((idx, group));
    }
    out
}

/// A zone behind the separator, the leading spaces its builders still
/// put in front of it taken off.
fn separated(mut zone: Vec<Span<'static>>, separator: &Span<'static>) -> Vec<Span<'static>> {
    if let Some(first) = zone.first_mut() {
        let trimmed = first.content.trim_start().to_string();
        first.content = trimmed.into();
    }
    zone.retain(|s| !s.content.is_empty());
    zone.insert(0, separator.clone());
    zone
}

/// A zone padded at its end to `reserve` cells on a wide rail, so the
/// zones behind it stay put while its digits change; a longer zone grows.
fn reserved(mut zone: Vec<Span<'static>>, reserve: usize, stable: bool) -> Vec<Span<'static>> {
    let w = total_width(&zone);
    if stable && w < reserve {
        zone.push(Span::raw(" ".repeat(reserve - w)));
    }
    zone
}

fn total_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| s.width()).sum()
}

/// Compact panes spend every cell on content. Wider rails reserve common
/// numeric widths; unusually large values grow rather than losing digits.
fn field(text: String, width: usize, stable: bool) -> String {
    if stable {
        pad_right(&text, width)
    } else {
        text
    }
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
