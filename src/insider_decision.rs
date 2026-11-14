//! The sale label AlphAI puts on SEC Form 4 sales: whether the insider chose
//! to sell (`discretionary`), sold under a 10b5-1 plan or for paperwork such
//! as the tax due on vesting (`plan_like`, shown as "routine"), or the filing
//! does not let the server tell (`unknown`, "unclear"). Purchases and other
//! codes carry no label.
//!
//! Pure: labels, glyphs, names and roles for one line, and the rollups the
//! Insider view draws from the chart bundle it already holds (every event of
//! the trailing year, unfiltered by relevance). No I/O, no `App`, no ratatui.
//!
//! The words follow the AlphAI site, so the app and the site say the same
//! thing about one filing.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, Days, NaiveDate};

use crate::alphai::{self, TradeEvent};

/// The Insider view's filter on the label, sent to the server as
/// `?decision=` (the server filters, so a filtered page is a full page).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InsiderDecision {
    #[default]
    All,
    Discretionary,
    Routine,
}

impl InsiderDecision {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Discretionary,
            Self::Discretionary => Self::Routine,
            Self::Routine => Self::All,
        }
    }

    /// The `decision` query value; None asks for every row.
    pub fn param(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Discretionary => Some("discretionary"),
            Self::Routine => Some("plan_like"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Discretionary => "discretionary",
            Self::Routine => "routine",
        }
    }

    pub const ALL: [Self; 3] = [Self::All, Self::Discretionary, Self::Routine];

    /// `[ui] insider_decision`: the view's words, or the API's for routine.
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "all" => Some(Self::All),
            "discretionary" => Some(Self::Discretionary),
            "routine" | "plan_like" | "non-discretionary" => Some(Self::Routine),
            _ => None,
        }
    }

    /// Whether a row with this label belongs under the filter: the local
    /// mirror of the server's filter, for the rows on screen while a
    /// refetch for a new filter is in flight.
    pub fn keeps(self, decision: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Discretionary => decision == Some("discretionary"),
            Self::Routine => decision == Some("plan_like"),
        }
    }
}

/// What one Form 4 event is, for its glyph and its bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A sale the insider chose to make.
    Discretionary,
    /// A sale under a 10b5-1 plan or for paperwork (tax, exercise cover).
    Routine,
    /// A sale the filing does not let the server classify.
    Unclear,
    /// A sale with no label at all: a server from before the label.
    Sold,
    Buy,
    /// Code D: shares handed back to the issuer, not a market sale.
    ToIssuer,
    Other,
}

/// Kind of an event from its side, SEC code and label. The chart bundle
/// counts code D as a sell and the feed as "other": the code decides.
pub fn kind(side: Option<&str>, code: Option<&str>, decision: Option<&str>) -> Kind {
    if code == Some("D") {
        return Kind::ToIssuer;
    }
    match side {
        Some("buy") => Kind::Buy,
        Some("sell") => match decision {
            Some("discretionary") => Kind::Discretionary,
            Some("plan_like") => Kind::Routine,
            Some(_) => Kind::Unclear,
            None => Kind::Sold,
        },
        _ => Kind::Other,
    }
}

/// The glyph a kind draws with, the same in the list, the header and the
/// chart. `▾` rather than a hollow triangle: the hollow small one is missing
/// from common terminal fonts.
pub fn glyph(kind: Kind) -> char {
    match kind {
        Kind::Discretionary | Kind::Sold => '▼',
        Kind::Routine => '▾',
        Kind::Unclear => '?',
        Kind::Buy => '▲',
        Kind::ToIssuer => '◇',
        Kind::Other => '·',
    }
}

/// The label's name for the card ("non-discretionary", the site's word).
pub fn decision_label(decision: &str) -> Cow<'static, str> {
    match decision {
        "discretionary" => "discretionary".into(),
        "plan_like" => "non-discretionary".into(),
        "unknown" => "unclear".into(),
        other => Cow::Owned(other.replace('_', " ")),
    }
}

/// Short form of a reason, for a list cell (the site's words).
pub fn reason_label(reason: &str) -> Cow<'static, str> {
    let label = match reason {
        "plan_10b5_1" => "10b5-1 plan",
        "tax_withholding" => "tax on vesting",
        "exercise_cover" => "exercise cover",
        "stated_non_discretionary" => "stated automatic",
        "open_market" => "open market",
        "exercise_and_sell" => "exercise and sell",
        "sold_all_vested" => "sold all vested",
        "partial_vest_sale" => "part of a vest",
        "cover_ambiguous" => "cover or choice",
        "mixed_acquisitions" => "vest and exercise",
        "price_disputed" => "price not verified",
        "no_lines" => "filing not read",
        other => return Cow::Owned(other.replace('_', " ")),
    };
    Cow::Borrowed(label)
}

/// Long form of a reason, for the card (the site's tooltip texts).
pub fn reason_text(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "plan_10b5_1" => {
            "The 10b5-1 box is checked, or a footnote places the sale under a pre-arranged plan."
        }
        "tax_withholding" => "Shares sold to pay the tax due when stock vested.",
        "exercise_cover" => "The sale covers the cost of an option exercise and its tax.",
        "stated_non_discretionary" => {
            "A footnote says the sale was automatic or outside the insider's discretion."
        }
        "open_market" => {
            "Nothing in the filing points to a plan or paperwork, or the sale is far larger than what vested or was exercised."
        }
        "exercise_and_sell" => {
            "Options were exercised and the shares sold, beyond what covering the exercise needs."
        }
        "sold_all_vested" => {
            "The shares sold plus those withheld for tax cover everything that vested."
        }
        "partial_vest_sale" => {
            "Part of the newly vested shares was sold, more than the tax alone would need."
        }
        "cover_ambiguous" => "The sale could be an exercise cover at a high tax rate, or a choice.",
        "mixed_acquisitions" => {
            "The filing both vests shares and exercises options, so the sale cannot be tied to either."
        }
        "price_disputed" => {
            "The filed price failed the price check, so the share arithmetic is not trusted."
        }
        "no_lines" => "The rest of the filing has not been read.",
        _ => return None,
    })
}

/// The Why words of a row: the main words and an optional detail for a
/// wide list. A discretionary sale says so and names its reason in the
/// detail; a routine one names its reason, which says why it is routine; a
/// purchase or an unlabelled sale under a plan keeps the plan mark it
/// always had.
pub fn why_words(kind: Kind, reason: Option<&str>, is_10b5_1: bool) -> (String, Option<String>) {
    let plain = |s: &str| (s.to_string(), None);
    match kind {
        Kind::Discretionary => (
            "discretionary".into(),
            reason.map(|r| reason_label(r).into_owned()),
        ),
        Kind::Routine => (
            reason.map_or_else(|| "routine".into(), |r| reason_label(r).into_owned()),
            None,
        ),
        Kind::Unclear => plain("unclear"),
        Kind::Buy | Kind::Sold if is_10b5_1 => plain("10b5-1 plan"),
        Kind::Buy => plain("buy"),
        Kind::Sold => plain("sell"),
        Kind::ToIssuer => plain("to issuer"),
        Kind::Other => plain("other"),
    }
}

/// Words that mark a reporting owner as an entity rather than a person:
/// its name stays exactly as filed.
const ENTITY_WORDS: [&str; 16] = [
    "llc",
    "l.l.c.",
    "lp",
    "l.p.",
    "inc",
    "inc.",
    "ltd",
    "ltd.",
    "fund",
    "trust",
    "capital",
    "partners",
    "management",
    "holdings",
    "corp",
    "corp.",
];

const SUFFIXES: [&str; 7] = ["jr", "jr.", "sr", "sr.", "ii", "iii", "iv"];

/// A person's name the way a reader says it. SEC files the reporting owner
/// as "Last First [Middle]": "Intrator Michael N" reads "Michael Intrator",
/// "McBee Brannin" "Brannin McBee" (the filed casing kept), an all-caps
/// filing in title case ("STEVENS MARK A" -> "Mark Stevens"), a suffix at
/// the end ("Robertson Donald F Jr" -> "Donald Robertson Jr"). Entities
/// and anything longer stay as filed, which is what the card shows anyway.
pub fn display_name(raw: &str) -> String {
    let raw = raw.trim();
    let caps = raw.chars().any(char::is_alphabetic)
        && !raw.chars().any(|c| c.is_alphabetic() && c.is_lowercase());
    let tidy = |s: &str| -> String {
        if caps {
            s.split_whitespace()
                .map(title_word)
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            s.to_string()
        }
    };
    let mut tokens: Vec<&str> = raw.split_whitespace().collect();
    if tokens.len() < 2
        || tokens
            .iter()
            .any(|t| ENTITY_WORDS.contains(&t.to_lowercase().as_str()))
    {
        return tidy(raw);
    }
    let suffix = tokens
        .last()
        .filter(|t| SUFFIXES.contains(&t.to_lowercase().as_str()))
        .copied();
    if suffix.is_some() {
        tokens.pop();
    }
    if tokens.len() < 2 || tokens.len() > 3 {
        return tidy(raw);
    }
    let last = tokens[0];
    let given: Vec<&str> = tokens[1..]
        .iter()
        .copied()
        .filter(|t| !is_initial(t))
        .collect();
    if given.is_empty() {
        return tidy(raw);
    }
    let mut out = format!("{} {last}", given.join(" "));
    if let Some(s) = suffix {
        out.push(' ');
        out.push_str(s);
    }
    tidy(&out)
}

fn is_initial(token: &str) -> bool {
    let letters: Vec<char> = token.chars().filter(|c| *c != '.').collect();
    letters.len() == 1 && letters[0].is_alphabetic()
}

fn title_word(word: &str) -> String {
    let lower = word.to_lowercase();
    if ["ii", "iii", "iv"].contains(&lower.as_str()) {
        return word.to_uppercase();
    }
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Officer titles a reader knows by their letters, checked in this order
/// so "CEO and President" and "EVP & Chief Financial Officer" find their
/// C-level role first.
const C_LEVEL: [(&str, &str); 14] = [
    ("chief executive officer", "CEO"),
    ("ceo", "CEO"),
    ("chief financial officer", "CFO"),
    ("cfo", "CFO"),
    ("chief operating officer", "COO"),
    ("coo", "COO"),
    ("chief technology officer", "CTO"),
    ("chief revenue officer", "CRO"),
    ("chief strategy officer", "CSO"),
    ("chief development officer", "CDO"),
    ("chief accounting officer", "CAO"),
    ("principal accounting officer", "CAO"),
    ("chief legal officer", "CLO"),
    ("general counsel", "GC"),
];

/// A role in at most `ROLE_WIDTH` columns: a C-level title by its letters,
/// otherwise the title's opening words (EVP, SVP, GC and Secretary ...),
/// otherwise what the filing's role flags say. The full title stays in the
/// card.
pub fn role_short(title: &str, officer: bool, director: bool, ten_pct: bool) -> String {
    let t = title.trim();
    if t.is_empty() {
        return if director {
            "director"
        } else if ten_pct {
            "10% owner"
        } else if officer {
            "officer"
        } else {
            ""
        }
        .to_string();
    }
    let lower = t.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    for (needle, short) in C_LEVEL {
        let hit = if needle.contains(' ') {
            lower.contains(needle)
        } else {
            words.contains(&needle)
        };
        if hit {
            return short.to_string();
        }
    }
    if lower.starts_with("gc ") || lower == "gc" {
        return "GC".into();
    }
    // Any other "Chief ... Officer" by its letters: Chief Infrastructure
    // Officer -> CIO, Chief Product and Technology Officer -> CPTO.
    if words.first() == Some(&"chief") && words.contains(&"officer") {
        let letters: String = words
            .iter()
            .filter(|w| !["and", "of", "the"].contains(w))
            .filter_map(|w| w.chars().next())
            .collect::<String>()
            .to_uppercase();
        if letters.len() <= 5 {
            return letters;
        }
    }
    for prefix in ["evp", "svp", "vp"] {
        if words.first() == Some(&prefix) {
            return prefix.to_uppercase();
        }
    }
    if lower.contains("director") {
        return "director".into();
    }
    cut_words(t, ROLE_WIDTH)
}

/// Columns a role takes in the lists.
pub const ROLE_WIDTH: usize = 10;

/// Whole words that fit in `width`, or the first word cut to it.
fn cut_words(text: &str, width: usize) -> String {
    let mut out = String::new();
    for w in text.split_whitespace() {
        let w = w.trim_end_matches(',');
        let next = if out.is_empty() {
            w.to_string()
        } else {
            format!("{out} {w}")
        };
        if next.chars().count() > width {
            break;
        }
        out = next;
    }
    if out.is_empty() {
        text.chars().take(width).collect()
    } else {
        out
    }
}

/// A dollar sum for the rollups: billions with two decimals, so the parts
/// add up to the total on screen ($2.62B + $2.55B = $5.17B, where one decimal
/// would print $2.6B + $2.5B = $5.2B). Smaller sums as everywhere else.
pub fn money(v: f64) -> String {
    if v.abs() >= 1e9 {
        let sign = if v < 0.0 { "-" } else { "" };
        format!("{sign}${:.2}B", v.abs() / 1e9)
    } else {
        alphai::fmt_usd(&v.to_string())
    }
}

/// Days the rollups, the tiles and the people mode cover: the chart
/// bundle's trailing year.
pub const YEAR_DAYS: u64 = 365;

/// "YYYY-MM-DD" (or a timestamp starting with one) as a date.
pub fn day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn value(e: &TradeEvent) -> f64 {
    e.total_value_usd
        .as_deref()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn event_kind(e: &TradeEvent) -> Kind {
    kind(
        e.side.as_deref(),
        e.transaction_code.as_deref(),
        e.decision.as_deref(),
    )
}

/// The event's day when it falls in the window and counts: a co-filed copy
/// of a trade already in the bundle (`duplicate_of`) never does, or a fund
/// and its general partner filing the same sale would count it twice.
fn in_window(e: &TradeEvent, from: NaiveDate, to: NaiveDate) -> Option<NaiveDate> {
    if e.duplicate_of.is_some() {
        return None;
    }
    e.transaction_date
        .as_deref()
        .and_then(day)
        .filter(|d| *d >= from && *d <= to)
}

/// Count, dollars and people of one bucket.
#[derive(Clone, Debug, Default)]
pub struct Bucket {
    pub count: usize,
    pub usd: f64,
    pub people: BTreeSet<String>,
}

impl Bucket {
    fn add(&mut self, e: &TradeEvent) {
        self.count += 1;
        self.usd += value(e);
        if !e.insider_name.is_empty() {
            self.people.insert(e.insider_name.clone());
        }
    }
}

/// The window's Form 4 activity by label. Sales without a label (a server
/// from before it) land in `sold`; `has_decisions` says whether any sale
/// carried one, and the view falls back to its unlabelled look without.
#[derive(Clone, Debug, Default)]
pub struct Rollup {
    pub discretionary: Bucket,
    pub routine: Bucket,
    pub unclear: Bucket,
    pub sold: Bucket,
    pub buys: Bucket,
    pub to_issuer: Bucket,
    /// Routine reasons by count, most common first.
    pub reasons: Vec<(String, usize)>,
    pub has_decisions: bool,
    pub insiders: usize,
}

impl Rollup {
    pub fn over<'a>(
        events: impl IntoIterator<Item = &'a TradeEvent>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Self {
        let mut r = Rollup::default();
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        let mut names = BTreeSet::new();
        for e in events {
            if in_window(e, from, to).is_none() {
                continue;
            }
            if !e.insider_name.is_empty() {
                names.insert(e.insider_name.as_str());
            }
            match event_kind(e) {
                Kind::Discretionary => r.discretionary.add(e),
                Kind::Routine => {
                    r.routine.add(e);
                    if let Some(reason) = e.decision_reason.as_deref() {
                        *reasons.entry(reason.to_string()).or_default() += 1;
                    }
                }
                Kind::Unclear => r.unclear.add(e),
                Kind::Sold => r.sold.add(e),
                Kind::Buy => r.buys.add(e),
                Kind::ToIssuer => r.to_issuer.add(e),
                Kind::Other => {}
            }
        }
        // Any labelled sale means the server labels: a sale still waiting
        // for its label then reads as unlabelled beside the others rather
        // than turning the whole view back to the look from before labels.
        r.has_decisions = r.discretionary.count + r.routine.count + r.unclear.count > 0;
        r.insiders = names.len();
        r.reasons = reasons.into_iter().collect();
        r.reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        r
    }

    /// Every market sale of the window, labelled or not.
    pub fn sold_usd(&self) -> f64 {
        self.discretionary.usd + self.routine.usd + self.unclear.usd + self.sold.usd
    }

    pub fn sold_count(&self) -> usize {
        self.discretionary.count + self.routine.count + self.unclear.count + self.sold.count
    }

    /// Share of the window's sold dollars the insiders chose to sell.
    pub fn discretionary_share(&self) -> f64 {
        let total = self.sold_usd();
        if total > 0.0 {
            self.discretionary.usd / total
        } else {
            0.0
        }
    }
}

/// One reporting owner's year, for the people mode.
#[derive(Clone, Debug)]
pub struct Person {
    /// As filed: the key, and what the detail pane prints.
    pub name: String,
    /// From the owner's latest event (a title can change in a year).
    pub role: String,
    pub discretionary: f64,
    pub routine: f64,
    pub unclear: f64,
    pub sold: f64,
    pub bought: f64,
    pub count: usize,
    pub last: NaiveDate,
}

impl Person {
    pub fn total(&self) -> f64 {
        self.discretionary + self.routine + self.unclear + self.sold + self.bought
    }

    /// Whether the person belongs under a filter (people without a sale of
    /// that kind drop out of the table).
    pub fn keeps(&self, filter: InsiderDecision) -> bool {
        match filter {
            InsiderDecision::All => true,
            InsiderDecision::Discretionary => self.discretionary > 0.0,
            InsiderDecision::Routine => self.routine > 0.0,
        }
    }
}

/// The window's reporting owners, largest dollar activity first.
pub fn people<'a>(
    events: impl IntoIterator<Item = &'a TradeEvent>,
    from: NaiveDate,
    to: NaiveDate,
) -> Vec<Person> {
    let mut by: BTreeMap<&str, (Person, NaiveDate)> = BTreeMap::new();
    for e in events {
        let Some(d) = in_window(e, from, to) else {
            continue;
        };
        if e.insider_name.is_empty() {
            continue;
        }
        let role = role_short(
            &e.insider_title,
            e.is_officer,
            e.is_director,
            e.is_ten_percent_owner,
        );
        let (p, role_day) = by.entry(e.insider_name.as_str()).or_insert_with(|| {
            (
                Person {
                    name: e.insider_name.clone(),
                    role: role.clone(),
                    discretionary: 0.0,
                    routine: 0.0,
                    unclear: 0.0,
                    sold: 0.0,
                    bought: 0.0,
                    count: 0,
                    last: d,
                },
                d,
            )
        });
        if d > *role_day && !role.is_empty() {
            p.role = role;
            *role_day = d;
        }
        let v = value(e);
        match event_kind(e) {
            Kind::Discretionary => p.discretionary += v,
            Kind::Routine => p.routine += v,
            Kind::Unclear => p.unclear += v,
            Kind::Sold => p.sold += v,
            Kind::Buy => p.bought += v,
            Kind::ToIssuer | Kind::Other => {}
        }
        p.count += 1;
        p.last = p.last.max(d);
    }
    let mut out: Vec<Person> = by.into_values().map(|(p, _)| p).collect();
    out.sort_by(|a, b| {
        b.total()
            .partial_cmp(&a.total())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    out
}

/// One Monday-keyed week of the chart's bars.
#[derive(Clone, Debug, PartialEq)]
pub struct Week {
    pub start: NaiveDate,
    pub discretionary: f64,
    /// Routine, unclear and unlabelled sales: everything that is not a
    /// choice, as one shade.
    pub routine: f64,
    pub buys: f64,
}

impl Week {
    pub fn sells(&self) -> f64 {
        self.discretionary + self.routine
    }
}

/// The window's weeks with their dollars by label, from the events inside
/// the window only (the server's weekly series has no label split, and a
/// week straddling the window's first day would carry days before it).
/// Code D stays out: it is not a market sale.
pub fn weekly<'a>(
    events: impl IntoIterator<Item = &'a TradeEvent>,
    start: NaiveDate,
    today: NaiveDate,
) -> Vec<Week> {
    let monday = |d: NaiveDate| d - Days::new(u64::from(d.weekday().num_days_from_monday()));
    let mut weeks: BTreeMap<NaiveDate, Week> = BTreeMap::new();
    for e in events {
        let Some(d) = in_window(e, start, today) else {
            continue;
        };
        let w = weeks.entry(monday(d)).or_insert_with(|| Week {
            start: monday(d),
            discretionary: 0.0,
            routine: 0.0,
            buys: 0.0,
        });
        let v = value(e);
        match event_kind(e) {
            Kind::Discretionary => w.discretionary += v,
            Kind::Routine | Kind::Unclear | Kind::Sold => w.routine += v,
            Kind::Buy => w.buys += v,
            Kind::ToIssuer | Kind::Other => {}
        }
    }
    weeks.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(name: &str) -> alphai::InsiderTrades {
        let page: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/insider.json")).unwrap();
        serde_json::from_value(page[name].clone()).unwrap()
    }

    fn d(s: &str) -> NaiveDate {
        day(s).unwrap()
    }

    /// The label reads leniently on both surfaces: null, empty, missing or a
    /// number never fail the row, and only a real value counts as a label.
    #[test]
    fn the_label_parses_leniently() {
        for (field, want) in [
            (r#""decision": "discretionary","#, Some("discretionary")),
            (r#""decision": null,"#, None),
            (r#""decision": "","#, None),
            ("", None),
            (r#""decision": 5,"#, Some("5")),
        ] {
            let row: alphai::InsiderEvent =
                serde_json::from_str(&format!(r#"{{ {field} "side": "sell" }}"#)).unwrap();
            assert_eq!(row.decision.as_deref(), want, "{field}");
            let event: alphai::TradeEvent =
                serde_json::from_str(&format!(r#"{{ {field} "side": "sell" }}"#)).unwrap();
            assert_eq!(event.decision.as_deref(), want, "{field}");
        }
    }

    #[test]
    fn filter_cycles_and_reads_the_config() {
        assert_eq!(InsiderDecision::All.next(), InsiderDecision::Discretionary);
        assert_eq!(InsiderDecision::Routine.next(), InsiderDecision::All);
        assert_eq!(InsiderDecision::Routine.param(), Some("plan_like"));
        assert_eq!(
            InsiderDecision::from_config(" Routine "),
            Some(InsiderDecision::Routine)
        );
        assert_eq!(InsiderDecision::from_config("choice"), None);
        assert!(InsiderDecision::Discretionary.keeps(Some("discretionary")));
        assert!(!InsiderDecision::Discretionary.keeps(Some("plan_like")));
        assert!(!InsiderDecision::Routine.keeps(None));
    }

    #[test]
    fn kinds_follow_the_code_then_the_side_then_the_label() {
        assert_eq!(
            kind(Some("sell"), Some("D"), Some("discretionary")),
            Kind::ToIssuer
        );
        assert_eq!(kind(Some("buy"), Some("P"), None), Kind::Buy);
        assert_eq!(
            kind(Some("sell"), Some("S"), Some("discretionary")),
            Kind::Discretionary
        );
        assert_eq!(
            kind(Some("sell"), Some("S"), Some("plan_like")),
            Kind::Routine
        );
        assert_eq!(
            kind(Some("sell"), Some("S"), Some("unknown")),
            Kind::Unclear
        );
        assert_eq!(kind(Some("sell"), Some("S"), None), Kind::Sold);
    }

    #[test]
    fn the_why_cell_names_the_reason() {
        assert_eq!(
            why_words(Kind::Discretionary, Some("open_market"), false),
            ("discretionary".into(), Some("open market".into()))
        );
        assert_eq!(
            why_words(Kind::Routine, Some("tax_withholding"), false),
            ("tax on vesting".into(), None)
        );
        assert_eq!(why_words(Kind::Unclear, None, false).0, "unclear");
        assert_eq!(why_words(Kind::Buy, None, true).0, "10b5-1 plan");
        assert_eq!(why_words(Kind::Buy, None, false).0, "buy");
        assert_eq!(why_words(Kind::ToIssuer, None, false).0, "to issuer");
        assert_eq!(reason_label("new_reason"), "new reason");
        assert!(reason_text("tax_withholding").unwrap().contains("vested"));
        assert_eq!(reason_text("new_reason"), None);
    }

    #[test]
    fn names_read_first_name_first() {
        for (raw, shown) in [
            ("Intrator Michael N", "Michael Intrator"),
            ("McBee Brannin", "Brannin McBee"),
            ("STEVENS MARK A", "Mark Stevens"),
            ("SEAWELL A BROOKE", "Brooke Seawell"),
            ("Robertson Donald F Jr", "Donald Robertson Jr"),
            ("Teter Timothy S.", "Timothy Teter"),
            ("Magnetar Financial LLC", "Magnetar Financial LLC"),
            ("Van Der Berg John Paul", "Van Der Berg John Paul"),
            ("Cher", "Cher"),
        ] {
            assert_eq!(display_name(raw), shown, "{raw}");
        }
    }

    #[test]
    fn roles_shorten_to_what_a_reader_says() {
        for (title, shown) in [
            ("CEO and President", "CEO"),
            ("EVP & Chief Financial Officer", "CFO"),
            ("EVP, General Counsel and Secretary", "GC"),
            ("GC and Secretary", "GC"),
            ("Principal Accounting Officer", "CAO"),
            ("EVP, Product & Engineering", "EVP"),
            ("SVP of Engineering", "SVP"),
            ("Chief Strategy Officer", "CSO"),
            ("Treasurer", "Treasurer"),
            ("Corporate Controller and Treasurer", "Corporate"),
            ("Chief Infrastructure Officer", "CIO"),
            ("Chief Product and Technology Officer", "CPTO"),
        ] {
            assert_eq!(role_short(title, true, false, false), shown, "{title}");
        }
        assert_eq!(role_short("", false, true, false), "director");
        assert_eq!(role_short("", false, false, true), "10% owner");
    }

    #[test]
    fn billions_keep_two_decimals_so_the_parts_add_up() {
        assert_eq!(money(5_166_432_561.8), "$5.17B");
        assert_eq!(money(2_621_100_000.0), "$2.62B");
        assert_eq!(money(931_100_000.0), "$931.1M");
    }

    #[test]
    fn the_live_year_of_crwv_splits_by_label() {
        let t = bundle("crwv");
        let r = Rollup::over(&t.chart_events, d("2025-11-14"), d("2026-11-13"));
        assert!(r.has_decisions);
        assert_eq!(r.discretionary.count, 34);
        assert_eq!(money(r.discretionary.usd), "$2.62B");
        assert_eq!(r.discretionary.people.len(), 4);
        assert_eq!(r.routine.count, 305);
        assert_eq!(money(r.routine.usd), "$2.55B");
        assert_eq!(r.routine.people.len(), 9);
        assert_eq!(money(r.sold_usd()), "$5.17B");
        assert_eq!(r.reasons[0], ("plan_10b5_1".to_string(), 250));
        assert_eq!(r.reasons[1], ("tax_withholding".to_string(), 55));
        assert_eq!(r.buys.count, 0);
        assert_eq!(r.insiders, 12);
        assert!((r.discretionary_share() - 0.507).abs() < 0.01);
    }

    #[test]
    fn people_rank_by_dollars_with_their_latest_role() {
        let t = bundle("crwv");
        let people = people(&t.chart_events, d("2025-11-14"), d("2026-11-13"));
        assert_eq!(people[0].name, "Magnetar Financial LLC");
        assert_eq!(people[0].role, "10% owner");
        assert_eq!(money(people[0].discretionary), "$2.40B");
        assert_eq!(people[0].routine, 0.0);
        let goldberg = people.iter().find(|p| p.name == "Goldberg Chen").unwrap();
        assert_eq!(goldberg.role, "EVP", "the latest title wins");
        let baker = people.iter().find(|p| p.name == "Baker Jeff").unwrap();
        assert!(baker.discretionary > 0.0 && baker.routine > 0.0);
        assert!(
            !people
                .iter()
                .filter(|p| p.keeps(InsiderDecision::Discretionary))
                .any(|p| p.name == "Intrator Michael N")
        );
    }

    #[test]
    fn weeks_hold_only_the_window() {
        let t = bundle("crwv");
        let start = d("2026-08-14");
        let weeks = weekly(&t.chart_events, start, d("2026-11-13"));
        // Magnetar's $445M of 12-13 Aug falls before the window.
        let first = &weeks[0];
        assert_eq!(first.start, d("2026-08-10"));
        assert!(first.discretionary < 40e6, "{first:?}");
        assert!(weeks.iter().all(|w| w.buys == 0.0));
    }

    #[test]
    fn a_server_without_labels_reads_as_unlabelled() {
        let mut t = bundle("crwv");
        for e in &mut t.chart_events {
            e.decision = None;
            e.decision_reason = None;
        }
        let r = Rollup::over(&t.chart_events, d("2025-11-14"), d("2026-11-13"));
        assert!(!r.has_decisions);
        assert_eq!(r.sold.count, 339);
        assert_eq!(money(r.sold_usd()), "$5.17B");
    }
}
