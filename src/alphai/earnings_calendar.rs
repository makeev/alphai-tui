//! Corporate calendar wire contract. Dates are New York days, not UTC bounds.

use anyhow::{Result, ensure};
use chrono::{DateTime, NaiveDate, Utc};
use serde::de::Error;
use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportRequest {
    pub generation: u64,
    pub tickers: Vec<String>,
    pub from: NaiveDate,
    pub to: NaiveDate,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Scheduled,
    Reported,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReportGuidance {
    pub period: Option<String>,
    pub revenue: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PreviousRead {
    pub fiscal_period: String,
    pub published_at: DateTime<Utc>,
    pub verdict: Option<String>,
    pub guidance: Option<ReportGuidance>,
    #[serde(default)]
    pub what_to_watch: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReportRead {
    pub uid: String,
    pub ticker: String,
    pub fiscal_period: String,
    pub published_at: DateTime<Utc>,
    pub verdict: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReportEvent {
    pub ticker: String,
    pub company: String,
    pub sector: Option<String>,
    #[serde(deserialize_with = "day")]
    pub report_date: NaiveDate,
    pub session: Option<String>,
    pub status: ReportStatus,
    pub reported_at: Option<DateTime<Utc>>,
    pub previous_read: Option<PreviousRead>,
    pub report_read: Option<ReportRead>,
    // Missing only during the additive server rollout. Never infer siblings.
    #[serde(default, deserialize_with = "listings")]
    pub listings: Vec<String>,
    #[serde(skip)]
    pub incomplete_listings: bool,
}

impl ReportEvent {
    pub fn contains(&self, symbol: &str) -> bool {
        self.listings.iter().any(|s| s == symbol)
    }

    pub fn same_report(&self, other: &Self) -> bool {
        if let (Some(a), Some(b)) = (&self.report_read, &other.report_read) {
            return a.uid == b.uid;
        }
        self.report_date == other.report_date && self.listings.iter().any(|s| other.contains(s))
    }

    pub fn session_label(&self) -> &'static str {
        match self.session.as_deref() {
            Some("before_open") => "BMO",
            Some("after_close") => "AMC",
            _ => "—",
        }
    }
}

#[derive(Deserialize)]
pub struct ReportPage {
    #[serde(deserialize_with = "day")]
    from_date: NaiveDate,
    #[serde(deserialize_with = "day")]
    to_date: NaiveDate,
    items: Vec<ReportEvent>,
}

fn day<'de, D: Deserializer<'de>>(deserializer: D) -> Result<NaiveDate, D::Error> {
    let text = String::deserialize(deserializer)?;
    if text.len() != 10 {
        return Err(D::Error::custom("expected YYYY-MM-DD"));
    }
    text.parse().map_err(D::Error::custom)
}

fn listings<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let values = Vec::<String>::deserialize(deserializer)?;
    if values.is_empty() {
        return Err(D::Error::custom("listings must not be empty"));
    }
    Ok(values)
}

pub fn valid_ticker(ticker: &str) -> bool {
    !ticker.is_empty()
        && ticker.len() <= 16
        && ticker
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

impl ReportPage {
    pub fn validate(mut self, request: &ReportRequest) -> Result<Vec<ReportEvent>> {
        ensure!(
            self.from_date == request.from && self.to_date == request.to,
            "earnings calendar returned a different window"
        );
        for item in &mut self.items {
            ensure!(
                valid_ticker(&item.ticker),
                "earnings calendar returned an invalid ticker"
            );
            ensure!(
                item.report_date >= request.from && item.report_date < request.to,
                "earnings calendar returned an out-of-window date"
            );
            if item.listings.is_empty() {
                item.incomplete_listings = true;
                item.listings.push(item.ticker.clone());
            }
            ensure!(
                item.contains(&item.ticker) && item.listings.iter().all(|s| valid_ticker(s)),
                "earnings calendar returned invalid listings"
            );
            item.listings.sort();
            item.listings.dedup();
            if let Some(read) = &item.report_read {
                ensure!(
                    !read.uid.trim().is_empty() && valid_ticker(&read.ticker),
                    "earnings calendar returned an invalid read pointer"
                );
            }
        }
        Ok(self.items)
    }
}
