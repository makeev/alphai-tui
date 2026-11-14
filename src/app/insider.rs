//! The Insider view's own state: the sale-label filter (f), the people mode
//! (x) and its cursor. The feed machinery itself stays in `feeds.rs`; the
//! people mode only reads the chart bundle the feed already holds.

use chrono::Days;

use crate::alphai;
use crate::insider_decision::{self, InsiderDecision, Person, YEAR_DAYS};

use super::{App, open_url};

impl App {
    /// The chart bundle of the selected ticker's insider feed, if loaded.
    pub(crate) fn insider_trades(&self) -> Option<&alphai::InsiderTrades> {
        self.feeds
            .get(&alphai::insider_key(self.selected_symbol()))
            .and_then(|b| b.insider_trades())
    }

    /// Whether the server labels Form 4 sales, as far as this session has
    /// seen: any labelled answer says yes, and nothing loaded yet counts as
    /// yes (the first fetch carries the filter anyway). Per server, not per
    /// ticker: a ticker without sales has nothing to label.
    fn insider_has_labels(&self) -> bool {
        self.insider_labels_seen
            || !self
                .feeds
                .contains_key(&alphai::insider_key(self.selected_symbol()))
    }

    /// f in the Insider view: the next label filter.
    pub(super) fn cycle_insider_decision(&mut self) {
        self.set_insider_decision(self.insider_decision.next());
    }

    /// A label filter, from f or a click on its pill. The rows on screen
    /// follow it at once (`ensure_feed_data` drops the ones it excludes
    /// before the refetch goes out), and back to all is always allowed.
    pub(super) fn set_insider_decision(&mut self, filter: InsiderDecision) {
        if filter == self.insider_decision {
            return;
        }
        if self.insider_decision == InsiderDecision::All && !self.insider_has_labels() {
            self.notice = Some((
                "this server does not label Form 4 sales yet".to_string(),
                std::time::Instant::now(),
            ));
            return;
        }
        self.insider_decision = filter;
        self.news_selected = 0;
        self.card_scroll = 0;
        self.insider_person = 0;
        *self.insider_people_state.offset_mut() = 0;
    }

    /// The people mode's table: the trailing year by reporting owner, under
    /// the label filter. Built from the chart bundle, so it costs nothing.
    pub(crate) fn insider_people_list(&self) -> Vec<Person> {
        let Some(t) = self.insider_trades() else {
            return Vec::new();
        };
        let today = self.now().date_naive();
        let from = today - Days::new(YEAR_DAYS - 1);
        insider_decision::people(&t.chart_events, from, today)
            .into_iter()
            .filter(|p| p.keeps(self.insider_decision))
            .collect()
    }

    pub(super) fn move_insider_person(&mut self, delta: isize) {
        let len = self.insider_people_list().len();
        if len == 0 {
            self.insider_person = 0;
            return;
        }
        self.insider_person = self
            .insider_person
            .saturating_add_signed(delta)
            .min(len - 1);
    }

    /// Enter in the people mode: the AlphAI article of the selected owner's
    /// latest event that has one. Events older than about six months mostly
    /// do not, and saying so beats opening nothing.
    pub(super) fn open_insider_person(&mut self) {
        let people = self.insider_people_list();
        let Some(person) = people.get(self.insider_person) else {
            return;
        };
        // The latest event whose article address can be built: one missing
        // its publish time cannot, and must not hide an older one that can.
        let url = self.insider_trades().and_then(|t| {
            t.chart_events
                .iter()
                .filter(|e| e.insider_name == person.name && e.has_article)
                .filter_map(|e| {
                    let uid = e.news_uid.as_deref().filter(|u| !u.is_empty())?;
                    let published = e
                        .news_published
                        .as_deref()
                        .and_then(|p| chrono::DateTime::parse_from_rfc3339(p).ok())
                        .map(|p| p.with_timezone(&chrono::Utc));
                    let title = e.news_title.clone().unwrap_or_default();
                    let url = alphai::article_url_for(uid, &title, published)?;
                    Some((e.transaction_date.clone().unwrap_or_default(), url))
                })
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, url)| url)
        });
        match url {
            Some(url) => open_url(&url),
            None => {
                self.notice = Some((
                    format!(
                        "no AlphAI article for {}'s recent filings",
                        insider_decision::display_name(&person.name)
                    ),
                    std::time::Instant::now(),
                ));
            }
        }
    }
}
