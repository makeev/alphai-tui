//! Two ET windows, batched by listing. No company-history sweep or automatic
//! error retries: each successful group has its own coverage and age.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};

use crate::alphai::{self, ReportEvent, ReportRequest, ReportStatus};
use crate::{market, ui};

use super::App;

const HOUR: Duration = Duration::from_secs(3600);
pub(crate) const REPORT_PACE: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Window {
    Near,
    Far,
}

pub(crate) struct ReportSlot {
    pub window: Window,
    pub request: ReportRequest,
    pub items: Vec<ReportEvent>,
    pub fetched: Option<Instant>,
    pub covered: bool,
    pub error: Option<String>,
    refresh: bool,
}

#[derive(Default)]
pub(crate) struct ReportCalendar {
    pub slots: Vec<ReportSlot>,
    pub universe: Vec<String>,
    pub inflight: Option<ReportRequest>,
    pub paused: Option<String>,
    pub ever_covered: HashSet<String>,
    day: Option<NaiveDate>,
    generation: u64,
    asked: Option<Instant>,
}

impl ReportCalendar {
    pub fn interval(ttl: Duration, active: bool) -> Duration {
        if active {
            (ttl * 12).max(HOUR)
        } else {
            (ttl * 72).max(HOUR * 6)
        }
    }

    pub fn sync(&mut self, mut universe: Vec<String>, day: NaiveDate) {
        universe = universe
            .into_iter()
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| alphai::earnings_calendar::valid_ticker(s) && !market::is_crypto(s))
            .collect();
        universe.sort();
        universe.dedup();
        if self.universe == universe && self.day == Some(day) {
            return;
        }
        self.generation += 1;
        self.day = Some(day);
        self.universe = universe;
        self.ever_covered.retain(|s| self.universe.contains(s));
        let old = std::mem::take(&mut self.slots);
        for (window, start, end) in [(Window::Near, -7, 7), (Window::Far, 7, 45)] {
            for tickers in self.universe.chunks(50) {
                let request = ReportRequest {
                    generation: self.generation,
                    tickers: tickers.to_vec(),
                    from: day + chrono::Duration::days(start),
                    to: day + chrono::Duration::days(end),
                };
                let matching: Vec<_> = old
                    .iter()
                    .filter(|s| s.request.tickers == tickers)
                    .collect();
                let exact = matching
                    .iter()
                    .find(|s| s.request.from == request.from && s.request.to == request.to);
                let items = matching
                    .iter()
                    .flat_map(|s| &s.items)
                    .filter(|e| {
                        e.report_date >= request.from
                            && e.report_date < request.to
                            && (e.report_read.is_some() || e.report_date >= day)
                    })
                    .cloned()
                    .collect();
                self.slots.push(ReportSlot {
                    window,
                    request,
                    items,
                    fetched: exact.and_then(|s| s.fetched),
                    covered: exact.is_some_and(|s| s.covered),
                    error: exact.and_then(|s| s.error.clone()),
                    refresh: false,
                });
            }
        }
    }

    pub fn next(
        &mut self,
        clock: Instant,
        ttl: Duration,
        calendar: bool,
        pending: Option<&str>,
    ) -> Option<ReportRequest> {
        if self.paused.is_some()
            || self.inflight.is_some()
            || self
                .asked
                .is_some_and(|at| clock.saturating_duration_since(at) < REPORT_PACE)
        {
            return None;
        }
        let slot = self.slots.iter_mut().find(|s| {
            let visible = s.window == Window::Near || calendar;
            let active = s.window == Window::Near
                && (calendar || pending.is_some_and(|p| s.request.tickers.iter().any(|t| t == p)));
            let due = s.refresh
                || !s.covered
                || s.fetched.is_none_or(|at| {
                    clock.saturating_duration_since(at) >= Self::interval(ttl, active)
                });
            visible && due && s.error.is_none()
        })?;
        slot.refresh = false;
        self.asked = Some(clock);
        self.inflight = Some(slot.request.clone());
        Some(slot.request.clone())
    }

    pub fn apply(
        &mut self,
        request: ReportRequest,
        result: Result<Vec<ReportEvent>, String>,
        blocked: bool,
        clock: Instant,
    ) -> bool {
        if self.inflight.as_ref() != Some(&request) {
            return false;
        }
        self.inflight = None;
        if request.generation != self.generation {
            return false;
        }
        let Some(slot) = self.slots.iter_mut().find(|s| s.request == request) else {
            return false;
        };
        match result {
            Ok(items) => {
                self.ever_covered.extend(request.tickers);
                self.ever_covered
                    .extend(items.iter().flat_map(|e| e.listings.iter().cloned()));
                slot.items = items;
                slot.fetched = Some(clock);
                slot.covered = true;
                slot.error = None;
            }
            Err(error) => {
                if blocked {
                    self.paused = Some(error.clone());
                }
                slot.error = Some(error);
            }
        }
        true
    }

    pub fn refresh(&mut self, symbol: Option<&str>) -> bool {
        // A key repeat cannot extend a running sweep or queue duplicate groups.
        if self.inflight.is_some()
            || (self.paused.is_none() && self.slots.iter().any(|s| s.refresh))
        {
            return false;
        }
        self.paused = None;
        for slot in &mut self.slots {
            if symbol.is_none_or(|s| {
                slot.window == Window::Near && slot.request.tickers.iter().any(|t| t == s)
            }) {
                slot.error = None;
                slot.refresh = true;
            }
        }
        true
    }

    pub fn updating(&self, window: Window) -> bool {
        self.slots.iter().any(|s| {
            s.window == window && (s.refresh || self.inflight.as_ref() == Some(&s.request))
        })
    }

    pub fn fresh(&self, slot: &ReportSlot, ttl: Duration, clock: Instant, active: bool) -> bool {
        self.paused.is_none()
            && slot.error.is_none()
            && slot.covered
            && slot
                .fetched
                .is_some_and(|at| clock.saturating_duration_since(at) < Self::interval(ttl, active))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EarningsTarget {
    pub symbol: String,
    pub event: ReportEvent,
    pub phase: TargetPhase,
    pub refresh_pending: bool,
    pub no_longer_listed: bool,
    pub pending_read: Option<Box<alphai::EarningsRead>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetPhase {
    Owner,
    OwnerLoading,
    Fallback,
    FallbackLoading,
    Done,
}

impl App {
    pub(crate) fn report_slot_fresh(&self, slot: &ReportSlot, clock: Instant) -> bool {
        let pending = self.earnings_target.as_ref().is_some_and(|t| {
            t.event.status == ReportStatus::Reported
                && t.event.report_read.is_none()
                && slot.request.tickers.contains(&t.symbol)
        });
        let active =
            slot.window == Window::Near && (self.view_id() == ui::ViewId::Calendar || pending);
        self.report_calendar
            .fresh(slot, self.alphai_ttl, clock, active)
    }
    pub(crate) fn earnings_symbol(&self) -> &str {
        self.earnings_target
            .as_ref()
            .map_or_else(|| self.selected_symbol(), |t| t.symbol.as_str())
    }

    pub(crate) fn shown_earnings(&self) -> Option<&alphai::EarningsRead> {
        if let Some(target) = &self.earnings_target
            && target.event.report_read.is_none()
            && let Some(read) = &target.pending_read
        {
            return Some(read);
        }
        let data = &self.earnings.get(self.earnings_symbol())?.data;
        if let Some(read) = self
            .earnings_target
            .as_ref()
            .and_then(|t| t.event.report_read.as_ref())
        {
            data.reports.iter().find(|r| r.uid == read.uid)
        } else {
            data.latest()
        }
    }

    pub(super) fn ensure_reports(&mut self, now: DateTime<Utc>, clock: Instant) {
        self.report_calendar
            .sync(self.polled_symbols(), market::et_time(now).date());
        let pending = self
            .earnings_target
            .as_ref()
            .filter(|t| t.event.status == ReportStatus::Reported && t.event.report_read.is_none());
        if let Some(request) = self.report_calendar.next(
            clock,
            self.alphai_ttl,
            self.view_id() == ui::ViewId::Calendar,
            pending.map(|t| t.symbol.as_str()),
        ) {
            let _ = self
                .alphai_tx
                .send(alphai::Cmd::FetchReportCalendar(request));
        }
    }

    pub(super) fn earnings_arrived(&mut self, symbol: &str) {
        let Some(target) = self.earnings_target.as_mut().filter(|t| t.symbol == symbol) else {
            return;
        };
        if target.event.report_read.is_none() && target.pending_read.is_none() {
            target.pending_read = self
                .earnings
                .get(symbol)
                .and_then(|s| s.data.latest())
                .cloned()
                .map(Box::new);
        }
        let Some(pointer) = &target.event.report_read else {
            return;
        };
        let found = self
            .earnings
            .get(symbol)
            .is_some_and(|s| s.data.reports.iter().any(|r| r.uid == pointer.uid));
        let unknown = self.earnings.get(symbol).is_some_and(|s| s.data.unknown);
        target.phase = if !found
            && !unknown
            && pointer.ticker != target.symbol
            && matches!(target.phase, TargetPhase::Owner | TargetPhase::OwnerLoading)
        {
            TargetPhase::Fallback
        } else {
            TargetPhase::Done
        };
    }

    pub(crate) fn current_target_event(&self) -> Option<&ReportEvent> {
        let target = self.earnings_target.as_ref()?;
        let items: Vec<_> = self
            .report_calendar
            .slots
            .iter()
            .flat_map(|s| &s.items)
            .collect();
        if let Some(event) = items.iter().find(|e| e.same_report(&target.event)) {
            return Some(event);
        }
        let matches: Vec<_> = items
            .into_iter()
            .filter(|e| e.listings.iter().any(|s| target.event.contains(s)))
            .collect();
        let first = *matches.first()?;
        matches
            .iter()
            .all(|e| e.same_report(first))
            .then_some(first)
    }

    pub(super) fn reports_arrived(&mut self, request: &ReportRequest) {
        let Some(target) = self.earnings_target.as_ref() else {
            return;
        };
        if !target.refresh_pending || !request.tickers.contains(&target.symbol) {
            return;
        }
        let event = self.current_target_event().cloned();
        let target = self.earnings_target.as_mut().unwrap();
        target.refresh_pending = false;
        target.no_longer_listed = event.is_none();
        if let Some(event) = event {
            target.event = event;
            target.phase = TargetPhase::Owner;
            self.earnings_scroll = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_long_schedules_meet_the_calendar_budget_at_default_and_minimum_ttl() {
        for ttl in [30, 300] {
            for (count, groups) in [(25, 1), (51, 2)] {
                for calendar in [false, true] {
                    let mut cache = ReportCalendar::default();
                    cache.sync(
                        (0..count).map(|i| format!("T{i:02}")).collect(),
                        "2026-11-07".parse().unwrap(),
                    );
                    let start = Instant::now();
                    let mut requests = 0;
                    for seconds in (0..86400).step_by(4) {
                        let clock = start + Duration::from_secs(seconds);
                        if let Some(request) =
                            cache.next(clock, Duration::from_secs(ttl), calendar, None)
                        {
                            requests += 1;
                            cache.apply(request, Ok(vec![]), false, clock);
                        }
                    }
                    assert_eq!(requests, if calendar { 28 * groups } else { 4 * groups });
                }
            }
        }
    }

    #[test]
    fn only_the_pending_targets_near_group_gets_the_hourly_interval() {
        let mut cache = ReportCalendar::default();
        cache.sync(
            (0..51).map(|i| format!("T{i:02}")).collect(),
            "2026-11-07".parse().unwrap(),
        );
        let start = Instant::now();
        let mut requests = 0;
        for seconds in (0..86400).step_by(4) {
            let clock = start + Duration::from_secs(seconds);
            if let Some(request) = cache.next(clock, Duration::from_secs(30), false, Some("T50")) {
                assert_eq!(request.to.to_string(), "2026-11-14");
                requests += 1;
                cache.apply(request, Ok(vec![]), false, clock);
            }
        }
        assert_eq!(requests, 28); // one hourly group plus one six-hourly group
    }
}
