//! Shared time labels, session backgrounds and grid positions. Coordinates
//! come from the plotted bars, so all three panels use the same columns.

use chrono::{DateTime, Local};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use crate::config::ChartTimezone;
use crate::domain::{Candle, Interval, Sessions};
use crate::market::{self, Session};
use crate::theme::Theme;

pub struct Tick {
    pub x: u16,
    pub text: String,
    pub major: bool,
}
pub struct TimeAxis {
    pub ticks: Vec<Tick>,
    pub dates: Vec<Tick>,
    pub shades: Vec<(u16, Session)>,
    pub zone: &'static str,
}

pub fn label(ts: i64, fmt: &str, zone: ChartTimezone, us: bool) -> String {
    let Some(t) = DateTime::from_timestamp(ts, 0) else {
        return String::new();
    };
    match zone {
        ChartTimezone::Exchange if us => market::et_time(t).format(fmt).to_string(),
        ChartTimezone::Utc => t.format(fmt).to_string(),
        _ => t.with_timezone(&Local).format(fmt).to_string(),
    }
}

fn x_at(points: &[(i64, u16)], ts: i64, us: bool) -> u16 {
    let i = points.partition_point(|(t, _)| *t < ts);
    if i == 0 {
        return points[0].1;
    }
    if i == points.len() {
        return points[i - 1].1;
    }
    let (a, x) = points[i - 1];
    let (b, y) = points[i];
    if ts == b {
        return y;
    }
    if us && market::window_at(a) != market::window_at(b) {
        return x + (y - x).div_ceil(2);
    }
    x + (((ts - a) as f64 / (b - a).max(1) as f64) * (y - x) as f64).round() as u16
}

fn place(
    mut candidates: Vec<(i64, u8)>,
    points: &[(i64, u16)],
    plot: Rect,
    fmt: &str,
    zone: ChartTimezone,
    us: bool,
    reserve: u16,
) -> Vec<Tick> {
    candidates.sort_by_key(|&(ts, priority)| (std::cmp::Reverse(priority), ts));
    let mut labels: Vec<(u16, u16, Tick)> = Vec::new();
    for (ts, priority) in candidates {
        let x = x_at(points, ts, us);
        let text = label(ts, fmt, zone, us);
        let width = text.len() as u16;
        if width + reserve > plot.width {
            continue;
        }
        let left = x
            .saturating_sub(width / 2)
            .clamp(plot.x, plot.right() - reserve - width);
        let right = left + width;
        if labels
            .iter()
            .any(|(a, b, _)| left < b.saturating_add(2) && right.saturating_add(2) > *a)
        {
            continue;
        }
        // x is the grid column; labels are centered independently at draw.
        labels.push((
            left,
            right,
            Tick {
                x,
                text,
                major: priority >= 2,
            },
        ));
    }
    labels.sort_by_key(|(_, _, t)| t.x);
    labels.into_iter().map(|(_, _, t)| t).collect()
}

impl TimeAxis {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        candles: &[Candle],
        xs: &[u16],
        plot: Rect,
        interval: Interval,
        sessions: Sessions,
        zone: ChartTimezone,
        us: bool,
        raw_bars: usize,
        last_ts: i64,
    ) -> Self {
        let zone_name = match zone {
            ChartTimezone::Exchange if us => "ET",
            ChartTimezone::Utc => "UTC",
            _ => "local",
        };
        let mut points: Vec<_> = candles.iter().zip(xs).map(|(c, x)| (c.ts, *x)).collect();
        let (first, _) = points[0];
        let last_x = *xs.last().unwrap();
        if last_ts > points.last().unwrap().0 {
            points.push((last_ts, last_x));
        }
        let intraday = interval != Interval::D1;
        let per_bar = interval.secs() * raw_bars.max(1) as i64 / candles.len().max(1) as i64;
        let spacing = if xs.len() > 1 {
            (last_x - xs[0]) as f64 / (xs.len() - 1) as f64
        } else {
            2.0
        }
        .max(1.0);
        for x in last_x.saturating_add(1)..plot.right() {
            let secs = ((x - last_x) as f64 / spacing * per_bar as f64).round() as i64;
            let ts = if us && intraday {
                market::advance_trading_time(last_ts, secs, sessions == Sessions::Extended)
            } else if us {
                // Daily candles advance by trading dates, not weekends.
                let days = secs / 86_400;
                let mut date = market::et_date(last_ts).unwrap();
                let mut remaining = days;
                while remaining > 0 {
                    date = date.succ_opt().unwrap();
                    if !market::windows(date).is_empty() {
                        remaining -= 1;
                    }
                }
                date.and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp()
            } else {
                last_ts + secs
            };
            points.push((ts, x));
        }
        let end = points.last().unwrap().0;
        let target = (plot.width / 10).clamp(2, 12) as i64;
        let mut dates = Vec::new();
        let mut candidates = Vec::new();
        if intraday {
            let mut windows = Vec::new();
            if us {
                let mut date = market::et_date(first).unwrap();
                let last_date = market::et_date(end).unwrap();
                while date <= last_date {
                    windows.extend(
                        market::windows(date).into_iter().filter(|w| {
                            sessions == Sessions::Extended || w.session == Session::Open
                        }),
                    );
                    date = date.succ_opt().unwrap();
                }
            }
            let span = if us {
                windows
                    .iter()
                    .map(|w| (w.end.min(end) - w.start.max(first)).max(0))
                    .sum()
            } else {
                end - first
            };
            let step = [900, 1800, 3600, 7200, 14400, 21600, 43200, 86400]
                .into_iter()
                .find(|step| *step >= span / target)
                .unwrap_or(86400);
            let mut t = first.div_euclid(step) * step;
            while t <= end {
                if t >= first
                    && (!us
                        || market::window_at(t).is_some_and(|w| {
                            sessions == Sessions::Extended || w.session == Session::Open
                        }))
                {
                    candidates.push((t, 1));
                }
                t += step;
            }
            for w in windows {
                if w.start >= first && w.start <= end {
                    candidates.push((
                        w.start,
                        if w.session == Session::Open || w.session == Session::Post {
                            3
                        } else {
                            2
                        },
                    ));
                }
            }
        }
        let mut previous = String::new();
        for &(ts, _) in &points {
            let date = label(ts, "%Y-%m-%d", zone, us);
            if date != previous {
                dates.push((ts, 2));
                if !intraday {
                    candidates.push((ts, 1));
                }
                previous = date;
            }
        }
        candidates.push((first, 0));
        if !intraday {
            candidates.push((end, 0));
        }
        let mut ticks = place(
            candidates.clone(),
            &points,
            plot,
            if intraday { "%H:%M" } else { "%d %b" },
            zone,
            us,
            0,
        );
        // An arbitrary fractional position in the future margin is not a
        // useful clock tick. Use it only when a very short window otherwise
        // has fewer than two labels.
        if ticks.len() < 2 && intraday {
            candidates.push((end, 0));
            ticks = place(candidates, &points, plot, "%H:%M", zone, us, 0);
        }
        let dates = if intraday {
            place(
                dates,
                &points,
                plot,
                "%d %b",
                zone,
                us,
                zone_name.len() as u16 + 2,
            )
        } else {
            Vec::new()
        };
        let mut shades = Vec::new();
        if intraday && us && sessions == Sessions::Extended {
            for x in xs[0].saturating_sub((spacing / 2.0) as u16).max(plot.x)..plot.right() {
                let i = points.partition_point(|(_, px)| *px <= x).saturating_sub(1);
                let ts = if let Some(&(next, nx)) = points.get(i + 1) {
                    if x > points[i].1 + (nx - points[i].1) / 2 {
                        next
                    } else {
                        points[i].0
                    }
                } else {
                    points[i].0
                };
                if let Some(w) = market::window_at(ts) {
                    shades.push((x, w.session));
                }
            }
        }
        Self {
            ticks,
            dates,
            shades,
            zone: zone_name,
        }
    }

    pub fn backdrop(&self, buf: &mut Buffer, plot: Rect, theme: &Theme, shading: bool, grid: bool) {
        if shading {
            for &(x, session) in &self.shades {
                let color = match session {
                    Session::Pre => theme.pre_market_bg,
                    Session::Post => theme.post_market_bg,
                    _ => continue,
                };
                for y in plot.y..plot.bottom() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_bg(color);
                    }
                }
            }
        }
        if grid {
            for tick in &self.ticks {
                for y in plot.y..plot.bottom() {
                    if (y - plot.y) % 2 == 0 || tick.major {
                        if let Some(cell) = buf.cell_mut((tick.x, y)) {
                            cell.set_char('┆')
                                .set_style(Style::new().fg(theme.ref_line));
                        }
                    }
                }
            }
        }
    }

    pub fn render(&self, buf: &mut Buffer, plot: Rect) {
        let dim = Style::new().add_modifier(ratatui::style::Modifier::DIM);
        for (row, ticks, reserve) in [
            (plot.bottom(), &self.ticks, 0),
            (plot.bottom() + 1, &self.dates, self.zone.len() as u16 + 2),
        ] {
            for tick in ticks {
                let width = tick.text.len() as u16;
                let left = tick
                    .x
                    .saturating_sub(width / 2)
                    .clamp(plot.x, plot.right() - reserve - width);
                buf.set_string(left, row, &tick.text, dim);
            }
        }
        let zone = &self.zone[..self.zone.len().min(plot.width as usize)];
        buf.set_string(
            plot.right() - zone.len() as u16,
            plot.bottom() + 1,
            zone,
            dim,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intraday_ticks_prioritize_bells_and_show_both_extended_backgrounds() {
        let first = DateTime::parse_from_rfc3339("2026-09-11T08:00:00Z")
            .unwrap()
            .timestamp();
        let bars: Vec<_> = (0..64)
            .map(|i| Candle {
                ts: first + i * 900,
                ..Default::default()
            })
            .collect();
        let xs: Vec<_> = (0..64).map(|i| 10 + i * 2).collect();
        let plot = Rect::new(10, 0, 160, 10);
        let axis = TimeAxis::build(
            &bars,
            &xs,
            plot,
            Interval::M15,
            Sessions::Extended,
            ChartTimezone::Exchange,
            true,
            bars.len(),
            bars.last().unwrap().ts,
        );
        assert!(axis.ticks.len() >= 6, "{} ticks", axis.ticks.len());
        assert!(axis.ticks.iter().any(|t| t.text == "09:30" && t.major));
        assert!(axis.ticks.iter().any(|t| t.text == "16:00" && t.major));
        assert!(axis.shades.iter().any(|(_, s)| *s == Session::Pre));
        assert!(axis.shades.iter().any(|(_, s)| *s == Session::Post));
        assert_eq!(axis.zone, "ET");
    }

    #[test]
    fn narrow_axes_do_not_overlap_or_write_outside_the_buffer() {
        let first = DateTime::parse_from_rfc3339("2026-09-11T13:30:00Z")
            .unwrap()
            .timestamp();
        let bars: Vec<_> = (0..10)
            .map(|i| Candle {
                ts: first + i * 300,
                ..Default::default()
            })
            .collect();
        for width in 3..60 {
            let plot = Rect::new(0, 0, width, 5);
            let xs: Vec<_> = (0..10).map(|i| i * (width - 1) / 9).collect();
            let axis = TimeAxis::build(
                &bars,
                &xs,
                plot,
                Interval::M5,
                Sessions::Regular,
                ChartTimezone::Local,
                true,
                bars.len(),
                bars.last().unwrap().ts,
            );
            let mut buf = Buffer::empty(Rect::new(0, 0, width, 7));
            axis.render(&mut buf, plot);
            assert!(axis.shades.is_empty());
        }
    }
}
