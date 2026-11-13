//! Corporate events on feed rows: the `event` block AlphAI attaches to SEC
//! 8-K filings, 6-K earnings releases and PR Newswire releases. This module
//! turns it into what the UI shows, with no I/O and no `App`: a short label
//! for the category column, a one-line summary of the facts (which replaces
//! the SEC's generic 8-K heading, "NVIDIA CORP (NVDA): Departure of
//! Directors or Certain Officers", and stands in for a headline the list
//! has no room for), and the card's layout of the quoted facts.
//!
//! The facts are quotes the server checked against the document; the type
//! and the role are the model's reading of it and are not checked. So the
//! summary leads with the quoted figures, phrases a deal neutrally ("deal
//! with X", never "to acquire X") and leaves the role to the card, where it
//! is labelled as such. Figures are never computed or rounded: only their
//! units are shortened ("$3.7 billion" -> "$3.7B", `alphai::short_metric`).
//!
//! The event's `role` is the side of the company that REPORTED it, which is
//! not always the ticker whose feed the row sits in, and a press release
//! about the whole market has no single reporter at all (`reporter`).

use std::borrow::Cow;

use crate::alphai::{self, AlsoEvent, Article, CorporateEvent, EventFact};
use crate::app::NewsScope;

/// One event type: the server's name, the category-column label (at most
/// `category::COLUMN_WIDTH` columns with its modifier) and the name the
/// card and the chart use.
struct Kind {
    api: &'static str,
    label: &'static str,
    name: &'static str,
}

const fn kind(api: &'static str, label: &'static str, name: &'static str) -> Kind {
    Kind { api, label, name }
}

const KINDS: [Kind; 17] = [
    kind("earnings", "earnings", "earnings"),
    kind("earnings_date", "earn date", "earnings date"),
    kind("guidance_update", "guide", "guidance"),
    kind("exec_change", "exec", "executive change"),
    kind("m_and_a", "M&A", "M&A"),
    kind("offering", "offering", "offering"),
    kind("debt_financing", "debt", "debt financing"),
    kind("dividend", "dividend", "dividend"),
    kind("buyback", "buyback", "buyback"),
    kind("contract_award", "contract", "contract"),
    kind("stock_split", "split", "stock split"),
    kind("listing_notice", "listing", "listing notice"),
    kind("shareholder_vote", "vote", "shareholder vote"),
    kind("bankruptcy", "bankruptcy", "bankruptcy"),
    kind("restructuring", "restructure", "restructuring"),
    kind("clinical_regulatory", "clinical", "clinical / regulatory"),
    kind("other", "other", "other"),
];

/// The `event_type` list a ticker's Events scope asks for: every type but
/// the catch-all `other`, which a market roundup gets.
pub const QUERY_TYPES: &str = "earnings,earnings_date,guidance_update,exec_change,m_and_a,\
offering,debt_financing,dividend,buyback,contract_award,stock_split,listing_notice,\
shareholder_vote,bankruptcy,restructuring,clinical_regulatory";

/// Widest label the category column may be asked to fit.
const LABEL_MAX: usize = crate::category::COLUMN_WIDTH as usize;

/// A list title narrower than this shows the event's summary in place of a
/// headline it could only show the start of: on a 110-column terminal a
/// press release's title is the company's name and an ellipsis.
pub const NARROW_TITLE: usize = 40;

fn by_api(api: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.api == api)
}

/// The card's name for an event type; one this build does not know is
/// shown as the server spells it.
pub fn name(kind: &str) -> Cow<'static, str> {
    match by_api(kind) {
        Some(k) => Cow::Borrowed(k.name),
        None => Cow::Owned(kind.replace('_', " ")),
    }
}

/// The company that reported the row. An SEC filing has one filer, listed
/// first among the row's tickers (checked live: it matches the ticker in
/// the SEC heading on every multi-ticker filing sampled). A press release
/// naming one or two tickers is that company's own; a release naming more
/// is a market roundup, and an outlook or a deal in it belongs to nobody
/// the row can name.
pub fn reporter(a: &Article) -> Option<&str> {
    let tickers = &a.enrichment.tickers;
    match a.original.source_domain.as_str() {
        "sec.gov" => tickers.first(),
        "prnewswire.com" if tickers.len() <= 2 => tickers.first(),
        _ => None,
    }
    .map(String::as_str)
}

/// The row's event when it has one worth showing: `other` is the catch-all
/// type, and an event nobody can be named as reporting is not shown at all.
pub fn shown(a: &Article) -> Option<&CorporateEvent> {
    a.event
        .as_ref()
        .filter(|e| !e.kind.is_empty() && e.kind != "other")
        .filter(|_| reporter(a).is_some())
}

/// Whether `symbol` reported the row.
pub fn reported_by(a: &Article, symbol: &str) -> bool {
    reporter(a).is_some_and(|r| r.eq_ignore_ascii_case(symbol))
}

/// Events that involve a company on both sides, so they belong in the
/// other party's feed too: a deal, a contract.
fn involves_both(kind: &str) -> bool {
    matches!(kind, "m_and_a" | "contract_award")
}

/// The event a row shows in a feed. In the ticker scope a row another
/// company reported keeps its event only when it involves both companies:
/// LCI's dividend in Patrick's feed is not Patrick's dividend. Outside the
/// ticker scope the tickers column names the reporter, so every event shows.
pub fn for_row<'a>(a: &'a Article, scope: NewsScope, symbol: &str) -> Option<&'a CorporateEvent> {
    shown(a).filter(|e| !scope.is_ticker() || reported_by(a, symbol) || involves_both(&e.kind))
}

/// The outlook change an earnings filing reports, as an arrow: every
/// guidance the document gives (the main event's and those in `also`)
/// names its metric and moves the same way. A guidance without a metric is
/// often a cost (a lowered cost per pound is good news), and mixed
/// directions have no single arrow.
pub fn guidance_arrow(e: &CorporateEvent) -> Option<&'static str> {
    let main = (e.kind == "guidance_update").then(|| (e.fact("metric"), e.fact("direction")));
    let also = e
        .also
        .iter()
        .filter(|x| x.kind == "guidance_update")
        .map(|x| (x.fact("metric"), x.fact("direction")));
    let mut arrow = None;
    for (metric, direction) in main.into_iter().chain(also) {
        metric?;
        let this = match direction {
            Some("raise") => "↑",
            Some("lower") => "↓",
            _ => return None,
        };
        if arrow.is_some_and(|a| a != this) {
            return None;
        }
        arrow = Some(this);
    }
    arrow
}

/// Category-column label of an event; an earnings filing keeps its form
/// instead (see `ui::news::category_label`).
pub fn label(e: &CorporateEvent) -> String {
    let base = by_api(&e.kind).map_or_else(|| e.kind.replace('_', " "), |k| k.label.to_string());
    let out = match (e.kind.as_str(), e.fact("kind"), e.fact("direction")) {
        ("guidance_update", _, Some("raise")) => format!("{base} ↑"),
        ("guidance_update", _, Some("lower")) => format!("{base} ↓"),
        ("guidance_update", _, Some("reaffirm")) => format!("{base} ="),
        ("guidance_update", _, Some("withdraw")) => format!("{base} off"),
        ("dividend", Some("increase" | "initiation"), _) => format!("{base} ↑"),
        ("dividend", Some("cut" | "suspension"), _) => format!("{base} ↓"),
        ("buyback", _, _) if e.fact("is_increase") == Some("true") => format!("{base} ↑"),
        ("stock_split", _, _) if e.fact("is_reverse") == Some("true") => "rev split".into(),
        ("listing_notice", Some("deficiency"), _) => "deficiency".into(),
        ("listing_notice", Some("regained_compliance"), _) => "compliant".into(),
        ("listing_notice", Some(k @ ("delisting" | "uplisting" | "transfer")), _) => k.into(),
        _ => base,
    };
    out.chars().take(LABEL_MAX).collect()
}

/// Whether the event can move a stock on its own, which earns it a lead
/// over ordinary coverage for a column of the price chart. An allow-list:
/// a type the server adds later has to be judged before it gets the lead.
pub fn is_material(e: &CorporateEvent) -> bool {
    match e.kind.as_str() {
        "offering"
        | "debt_financing"
        | "m_and_a"
        | "buyback"
        | "exec_change"
        | "contract_award"
        | "stock_split"
        | "bankruptcy"
        | "restructuring"
        | "clinical_regulatory" => true,
        "dividend" => matches!(
            e.fact("kind"),
            Some("increase" | "cut" | "suspension" | "initiation" | "special")
        ),
        "guidance_update" => matches!(e.fact("direction"), Some("raise" | "lower" | "withdraw")),
        "listing_notice" => matches!(e.fact("kind"), Some("deficiency" | "delisting")),
        _ => false,
    }
}

/// The SEC heading `NAME (TICKER): item names` split into the ticker and
/// the item names, when the row is an SEC filing whose heading names one
/// of its own tickers. A headline someone wrote (a press release inside an
/// 8-K, "CoreWeave Reports Strong Third Quarter 2026 Results") is not one.
fn template(a: &Article) -> Option<(&str, &str)> {
    if a.original.source_domain != "sec.gov" {
        return None;
    }
    let title = &a.original.title;
    let close = title.find("): ")?;
    let open = title[..close].rfind(" (")?;
    let ticker = &title[open + 2..close];
    a.enrichment
        .tickers
        .iter()
        .any(|t| t.eq_ignore_ascii_case(ticker))
        .then(|| (ticker, title[close + 3..].trim()))
        .filter(|(_, rest)| !rest.is_empty())
}

#[cfg(test)]
fn is_template_title(a: &Article) -> bool {
    template(a).is_some()
}

/// What a feed row prints in its title column.
pub struct RowTitle<'a> {
    pub text: Cow<'a, str>,
    /// The event's summary, printed after a written headline when both fit.
    pub detail: Option<String>,
}

/// The title of a feed row `width` columns wide:
/// - an SEC heading gives way to the event's summary, or loses its company
///   name when there is no summary (the ticker is on the row already);
/// - a written headline too narrow to be read gives way to the summary;
/// - a written headline with room to spare is followed by the summary.
///
/// In a ticker's feed, a summary of another company's filing names it.
pub fn row_title<'a>(a: &'a Article, scope: NewsScope, symbol: &str, width: usize) -> RowTitle<'a> {
    let foreign = scope.is_ticker() && !reported_by(a, symbol);
    let named = |text: String| -> Cow<'a, str> {
        match reporter(a) {
            Some(r) if foreign => Cow::Owned(format!("{r}: {text}")),
            _ => Cow::Owned(text),
        }
    };
    if let Some((ticker, rest)) = template(a) {
        let text = match shown(a).and_then(headline) {
            Some(summary) => named(summary),
            None if scope.is_ticker() && !ticker.eq_ignore_ascii_case(symbol) => {
                Cow::Owned(format!("{ticker}: {rest}"))
            }
            None => Cow::Borrowed(rest),
        };
        return RowTitle { text, detail: None };
    }
    let title = Cow::Borrowed(a.original.title.as_str());
    let Some(summary) = for_row(a, scope, symbol).and_then(headline) else {
        return RowTitle {
            text: title,
            detail: None,
        };
    };
    if width > 0 && width < NARROW_TITLE {
        return RowTitle {
            text: named(summary),
            detail: None,
        };
    }
    let detail = named(summary).into_owned();
    let fits = a.original.title.chars().count() + 2 + detail.chars().count() <= width;
    RowTitle {
        text: title,
        detail: fits.then_some(detail),
    }
}

/// The title the chart's bottom border gives a mark. The chart has no
/// category column, so an event names its type: `offering · $3.7B ·
/// priced · upsized · 2.875% convertible senior notes due 2033`.
pub fn chart_title<'a>(a: &'a Article, symbol: &str) -> Cow<'a, str> {
    let event = for_row(a, NewsScope::Ticker, symbol);
    match event.and_then(|e| headline(e).map(|s| (e, s))) {
        Some((e, summary)) => {
            let text = format!("{} · {summary}", name(&e.kind));
            match reporter(a) {
                Some(r) if !r.eq_ignore_ascii_case(symbol) => Cow::Owned(format!("{r}: {text}")),
                _ => Cow::Owned(text),
            }
        }
        None => row_title(a, NewsScope::Ticker, symbol, 0).text,
    }
}

/// The main event's one-line summary, or None when its facts say too
/// little to beat the row's own title.
pub fn headline(e: &CorporateEvent) -> Option<String> {
    summary(&e.kind, &e.facts)
}

/// One line for a secondary event, which has no column to name it: its
/// type, then its summary when the facts allow one.
pub fn also_line(x: &AlsoEvent) -> String {
    match summary(&x.kind, &x.facts) {
        Some(s) => format!("{} · {s}", name(&x.kind)),
        None => name(&x.kind).into_owned(),
    }
}

/// What an earnings release announced besides its results, one line each:
/// its main event when the release was typed as something other than
/// `earnings` (an outlook change, a report date), then everything in
/// `also` (a buyback, a dividend, a guidance change).
pub fn release_lines(e: &CorporateEvent) -> Vec<String> {
    let main = (e.kind != "earnings")
        .then(|| headline(e).map(|h| format!("{} · {h}", name(&e.kind))))
        .flatten();
    main.into_iter()
        .chain(e.also.iter().map(also_line))
        .collect()
}

/// Facts quoted as an amount, a price or a percentage, whose units the list
/// may shorten. Names, dates and titles are left exactly as written:
/// `short_metric` also cuts text at " as of ".
const FIGURES: [&str; 18] = [
    "deal_value",
    "price_per_share",
    "premium_pct",
    "gross_proceeds",
    "coupon_pct",
    "amount",
    "interest_rate",
    "amount_per_share",
    "prior_amount_per_share",
    "change_pct",
    "authorization_amount",
    "prior_authorization_amount",
    "pct_of_shares_outstanding",
    "value",
    "new_value",
    "prior_value",
    "headcount_pct",
    "charges",
];

/// A quote fit for one line: whitespace runs (a quote can span a line break
/// in the document) collapsed, and a figure's units shortened.
fn one_line(name: &str, value: &str) -> String {
    let v = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if FIGURES.contains(&name) {
        alphai::short_metric(&v)
    } else {
        v
    }
}

/// An officer's title shortened for one line the way a reader would say it
/// ("Chief Financial Officer" -> "CFO"); the card keeps the filing's words.
fn short_title(title: &str) -> String {
    let mut out = title.to_string();
    for (long, short) in [
        ("Chief Executive Officer", "CEO"),
        ("Chief Financial Officer", "CFO"),
        ("Chief Operating Officer", "COO"),
        ("Chief Technology Officer", "CTO"),
        ("Executive Vice President", "EVP"),
        ("Senior Vice President", "SVP"),
    ] {
        out = out.replace(long, short);
    }
    out
}

/// Summary of an event from its facts, figures first: the parts that are
/// present, joined by ` · `. The category column (or `also_line`) names
/// the type, so the summary does not repeat it.
fn summary(kind: &str, facts: &[EventFact]) -> Option<String> {
    let f = |name: &str| {
        facts
            .iter()
            .find(|x| x.name == name)
            .map(|x| one_line(name, &x.value))
            .filter(|v| !v.is_empty())
    };
    let raw = |name: &str| {
        facts
            .iter()
            .find(|x| x.name == name)
            .map(|x| x.value.trim())
            .filter(|v| !v.is_empty())
    };
    let mut parts: Vec<String> = Vec::new();
    match kind {
        "exec_change" => {
            let person = f("person_name")?;
            let action = raw("action");
            parts.push(match (action, f("title").map(|t| short_title(&t))) {
                (Some(a @ ("appointed" | "elected")), Some(t)) => format!("{person} {a} {t}"),
                (Some("promoted"), Some(t)) => format!("{person} promoted to {t}"),
                (Some("interim"), Some(t)) if t.to_lowercase().contains("interim") => {
                    format!("{person} named {t}")
                }
                (Some("interim"), Some(t)) => format!("{person} named interim {t}"),
                (Some(a @ ("resigned" | "retired" | "terminated")), Some(t)) => {
                    format!("{person} {a} as {t}")
                }
                (Some(a), Some(t)) => format!("{person} {} · {t}", a.replace('_', " ")),
                (Some(a), None) => format!("{person} {}", a.replace('_', " ")),
                (None, Some(t)) => format!("{person} · {t}"),
                (None, None) => person.clone(),
            });
            let other = |name: &str| f(name).filter(|o| !o.eq_ignore_ascii_case(&person));
            match action {
                Some("appointed" | "elected" | "promoted" | "interim") => {
                    parts.extend(other("predecessor_name").map(|p| format!("replaces {p}")));
                }
                Some("resigned" | "retired" | "terminated") => {
                    parts.extend(other("successor_name").map(|s| format!("successor {s}")));
                }
                _ => {}
            }
        }
        "m_and_a" => {
            let other = f("counterparty");
            let value = f("deal_value");
            if other.is_none() && value.is_none() {
                return None;
            }
            parts.push(match other {
                Some(o) => format!("deal with {o}"),
                None => "deal".into(),
            });
            parts.extend(value);
            parts.extend(raw("stage").map(|s| match s {
                "definitive" => "definitive agreement".to_string(),
                other => other.replace('_', " "),
            }));
        }
        "offering" => {
            let proceeds = f("gross_proceeds");
            let instrument = f("instrument");
            if proceeds.is_none() && instrument.is_none() {
                return None;
            }
            parts.extend(proceeds);
            parts.extend(f("stage"));
            if raw("upsized") == Some("true") {
                parts.push("upsized".into());
            }
            parts.extend(instrument);
        }
        "debt_financing" => {
            let amount = f("amount");
            let instrument = f("instrument");
            if amount.is_none() && instrument.is_none() {
                return None;
            }
            parts.extend(amount);
            // "9.625% Senior Notes due 2032" already says its rate.
            parts.extend(f("interest_rate").filter(|r| {
                instrument
                    .as_deref()
                    .is_none_or(|i| !i.to_lowercase().contains(&r.to_lowercase()))
            }));
            parts.extend(instrument);
        }
        "dividend" => {
            let k = raw("kind");
            let amount = f("amount_per_share");
            let prior = f("prior_amount_per_share");
            let change = f("change_pct");
            if k == Some("suspension") {
                parts.push("suspended".into());
            } else if amount.is_none() && prior.is_none() && change.is_none() {
                // Frequency and dates alone say no more than the
                // document's own headline.
                return None;
            } else {
                let freq = if k == Some("special") {
                    Some("special".to_string())
                } else {
                    f("frequency")
                };
                match (amount, freq) {
                    (Some(a), Some(fq)) => parts.push(format!("{a} {fq}")),
                    (Some(a), None) => parts.push(a),
                    (None, Some(fq)) => parts.push(fq),
                    (None, None) => {}
                }
            }
            if let Some(prior) = prior {
                parts.push(format!("was {prior}"));
            } else if let Some(change) = change {
                parts.push(match k {
                    Some("increase" | "initiation") => format!("raised {change}"),
                    Some("cut" | "suspension") => format!("cut {change}"),
                    _ => change,
                });
            }
            parts.extend(f("payable_date").map(|d| format!("payable {d}")));
        }
        "buyback" => {
            let size = f("authorization_amount").or_else(|| {
                f("authorization_shares").map(|s| {
                    if s.to_lowercase().contains("share") {
                        s
                    } else {
                        format!("{s} shares")
                    }
                })
            })?;
            parts.push(size);
            parts.extend(f("prior_authorization_amount").map(|p| format!("was {p}")));
            parts.extend(f("expires_at").map(|d| format!("through {d}")));
        }
        "contract_award" => {
            let other = f("counterparty");
            let value = f("value");
            if other.is_none() && value.is_none() {
                return None;
            }
            parts.extend(other.map(|o| format!("with {o}")));
            parts.extend(value);
            parts.extend(f("duration"));
        }
        "guidance_update" => {
            let metric = f("metric").map(|m| {
                let cut = m.len().saturating_sub(" guidance".len());
                if m.to_lowercase().ends_with(" guidance") {
                    m[..cut].to_string()
                } else {
                    m
                }
            });
            let new = f("new_value");
            if metric.is_none() && new.is_none() {
                return None;
            }
            // A metric that already names its period ("full year 2026
            // revenue") is not told the period twice.
            match (metric, f("period")) {
                (Some(m), Some(p)) if m.to_lowercase().contains(&p.to_lowercase()) => parts.push(m),
                (Some(m), Some(p)) => parts.push(format!("{m}, {p}")),
                (Some(m), None) => parts.push(m),
                (None, Some(p)) => parts.push(p),
                (None, None) => {}
            }
            parts.extend(new);
            parts.extend(f("prior_value").map(|p| format!("was {p}")));
            parts.extend(raw("direction").map(|d| match d {
                "raise" => "raised".to_string(),
                "lower" => "lowered".to_string(),
                "reaffirm" => "reaffirmed".to_string(),
                "withdraw" => "withdrawn".to_string(),
                "initiate" => "initiated".to_string(),
                other => other.replace('_', " "),
            }));
        }
        "stock_split" => {
            let ratio = f("ratio")?;
            parts.push(if raw("is_reverse") == Some("true") {
                format!("{ratio} reverse split")
            } else {
                format!("{ratio} split")
            });
            parts.extend(f("effective_date").map(|d| {
                if d.to_lowercase().starts_with("effective") {
                    d
                } else {
                    format!("effective {d}")
                }
            }));
        }
        "listing_notice" => {
            let exchange = f("exchange");
            let rule = f("rule");
            if exchange.is_none() && rule.is_none() {
                return None;
            }
            parts.extend(exchange);
            parts.extend(rule.map(|r| {
                if r.to_lowercase().contains("rule") {
                    r
                } else {
                    format!("rule {r}")
                }
            }));
        }
        "restructuring" => {
            match (f("headcount_reduction"), f("headcount_pct")) {
                (Some(n), Some(p)) => parts.push(format!("job cuts {n} ({p})")),
                (Some(n), None) => parts.push(format!("job cuts {n}")),
                (None, Some(p)) => parts.push(format!("job cuts {p}")),
                (None, None) => {}
            }
            parts.extend(f("charges").map(|c| format!("charges {c}")));
        }
        "earnings_date" => {
            parts.push(format!("reports {}", f("report_date")?));
            parts.extend(f("call_time").map(|c| format!("call {c}")));
        }
        "clinical_regulatory" => {
            parts.extend(raw("milestone").map(|m| match m {
                "topline_data" => "topline data".to_string(),
                "fda_approval" => "FDA approval".to_string(),
                "crl" => "complete response letter".to_string(),
                "pdufa_date" => "PDUFA date".to_string(),
                "nda_submission" => "NDA submitted".to_string(),
                "trial_start" => "trial started".to_string(),
                "discontinuation" => "discontinued".to_string(),
                other => other.replace('_', " "),
            }));
            parts.extend(f("product"));
            parts.extend(f("indication"));
        }
        _ => return None,
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Enumerated facts the card puts in the section's heading rather than in
/// its table: one word each ("priced · upsized", "quarterly · increase").
const HEADING_FACTS: [&str; 7] = [
    "action",
    "frequency",
    "kind",
    "direction",
    "upsized",
    "is_increase",
    "is_reverse",
];

/// The words the card's heading adds after the type and the stage.
pub fn heading_words(e: &CorporateEvent) -> Vec<String> {
    HEADING_FACTS
        .iter()
        .filter_map(|name| {
            let v = e.fact(name)?;
            match (*name, v) {
                (_, "false") => None,
                ("upsized", "true") => Some("upsized".into()),
                ("is_increase", "true") => Some("increase".into()),
                ("is_reverse", "true") => Some("reverse".into()),
                _ => Some(v.replace('_', " ")),
            }
        })
        .collect()
}

/// The order the card lists facts in, the figures of each type first; the
/// server's own order varies from one document to the next. A fact this
/// build does not know follows the known ones.
const FACT_ORDER: [&str; 46] = [
    // exec_change
    "person_name",
    "title",
    "effective_date",
    "predecessor_name",
    "successor_name",
    // m_and_a
    "deal_value",
    "price_per_share",
    "premium_pct",
    "consideration",
    "seller",
    "expected_close",
    // offering, debt_financing
    "gross_proceeds",
    "amount",
    "shares",
    "coupon_pct",
    "interest_rate",
    "maturity",
    "instrument",
    "lender",
    "use_of_proceeds",
    // dividend
    "amount_per_share",
    "prior_amount_per_share",
    "change_pct",
    "ex_date",
    "record_date",
    "payable_date",
    // buyback
    "authorization_amount",
    "authorization_shares",
    "prior_authorization_amount",
    "pct_of_shares_outstanding",
    "expires_at",
    // contract_award
    "value",
    "duration",
    "scope",
    // guidance_update
    "metric",
    "period",
    "new_value",
    "prior_value",
    // stock_split, listing_notice
    "ratio",
    "exchange",
    "rule",
    // restructuring
    "headcount_reduction",
    "headcount_pct",
    "charges",
    // earnings_date, clinical_regulatory
    "report_date",
    "call_time",
];

/// The facts the card's table lists, in `FACT_ORDER`. `stage` and
/// `counterparty` sit in the heading and the reporter line, the enumerated
/// ones in the heading, and `currency` repeats what an amount's quote says.
pub fn card_facts(e: &CorporateEvent) -> Vec<&EventFact> {
    let mut facts: Vec<&EventFact> = e
        .facts
        .iter()
        .filter(|f| !f.value.trim().is_empty())
        .filter(|f| {
            !matches!(f.name.as_str(), "stage" | "counterparty" | "currency")
                && !HEADING_FACTS.contains(&f.name.as_str())
        })
        .collect();
    facts.sort_by_key(|f| {
        FACT_ORDER
            .iter()
            .position(|n| *n == f.name)
            .unwrap_or(FACT_ORDER.len())
    });
    facts
}

/// The card's label for a fact; one this build does not know is shown as
/// the server names it.
pub fn fact_label(name: &str) -> Cow<'static, str> {
    let label = match name {
        "person_name" => "person",
        "effective_date" => "effective",
        "successor_name" => "successor",
        "predecessor_name" => "predecessor",
        "deal_value" => "value",
        "price_per_share" => "per share",
        "consideration" => "paid in",
        "premium_pct" => "premium",
        "expected_close" => "expected close",
        "gross_proceeds" => "proceeds",
        "coupon_pct" => "coupon",
        "use_of_proceeds" => "use of proceeds",
        "interest_rate" => "rate",
        "amount_per_share" => "per share",
        "prior_amount_per_share" => "was",
        "change_pct" => "change",
        "record_date" => "record date",
        "payable_date" => "payable",
        "ex_date" => "ex-date",
        "authorization_amount" => "authorized",
        "authorization_shares" => "shares",
        "prior_authorization_amount" => "was",
        "expires_at" => "expires",
        "pct_of_shares_outstanding" => "of shares out",
        "new_value" => "new",
        "prior_value" => "was",
        "headcount_reduction" => "job cuts",
        "headcount_pct" => "of staff",
        "report_date" => "report date",
        "call_time" => "call",
        other => return Cow::Owned(other.replace('_', " ")),
    };
    Cow::Borrowed(label)
}

/// A fact's value for the card: the quote verbatim, whitespace runs aside.
pub fn fact_value(fact: &EventFact) -> String {
    let v = fact.value.split_whitespace().collect::<Vec<_>>().join(" ");
    match fact.name.as_str() {
        "consideration" | "milestone" => v.replace('_', " "),
        _ => v,
    }
}

/// How the reporting company stands in the event, for the card's
/// "reported by" line; `issuer` and `other` say nothing worth a phrase.
pub fn role_phrase(role: Option<&str>) -> Option<&'static str> {
    match role {
        Some("acquirer") => Some("as the acquirer"),
        Some("target") => Some("as the target"),
        Some("customer") => Some("as the customer"),
        Some("supplier") => Some("as the supplier"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<(String, Article)> {
        let page: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/events.json")).unwrap();
        page["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let case = row["_case"].as_str().unwrap().to_string();
                (case, serde_json::from_value(row.clone()).unwrap())
            })
            .collect()
    }

    fn case(name: &str) -> Article {
        fixture()
            .into_iter()
            .find(|(c, _)| c == name)
            .unwrap_or_else(|| panic!("no fixture case {name}"))
            .1
    }

    fn ev(name: &str) -> CorporateEvent {
        case(name).event.unwrap()
    }

    fn event(kind: &str, facts: &[(&str, &str)]) -> CorporateEvent {
        CorporateEvent {
            kind: kind.into(),
            facts: facts
                .iter()
                .map(|(n, v)| EventFact {
                    name: (*n).into(),
                    value: (*v).into(),
                })
                .collect(),
            verified: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_events_scope_asks_for_every_type_but_other() {
        let asked: Vec<&str> = QUERY_TYPES.split(',').collect();
        let known: Vec<&str> = KINDS
            .iter()
            .map(|k| k.api)
            .filter(|k| *k != "other")
            .collect();
        assert_eq!(asked, known);
    }

    #[test]
    fn every_live_row_parses() {
        let rows = fixture();
        assert_eq!(rows.len(), 17);
        let with_event = rows.iter().filter(|(_, a)| a.event.is_some()).count();
        assert_eq!(with_event, 16, "the one news row has event: null");
    }

    /// The block is new and still being tuned on the server: a shape this
    /// build does not expect costs the row its event at most, never the row.
    #[test]
    fn an_unexpected_block_never_costs_the_row() {
        let row = |event: &str| -> Article {
            serde_json::from_str(&format!(
                r#"{{"original": {{"title": "T", "source_domain": "sec.gov"}},
                    "enrichment": {{"tickers": ["X"]}}, "event": {event}}}"#
            ))
            .unwrap()
        };
        let e = row(
            r#"{"type": "dividend", "facts": null, "also": ["buyback", 7], "verified": null,
                "role": 3, "stage": ""}"#,
        )
        .event
        .unwrap();
        assert!(e.facts.is_empty());
        assert_eq!(e.also.len(), 1, "a bare type name reads, a number does not");
        assert_eq!(e.also[0].kind, "buyback");
        assert_eq!(e.role.as_deref(), Some("3"));
        assert_eq!(e.stage, None);
        let e = row(
            r#"{"type": "dividend", "facts": [{"name": "amount_per_share", "value": 0.52}, 5, {"value": "x"}]}"#,
        )
        .event
        .unwrap();
        assert_eq!(e.fact("amount_per_share"), Some("0.52"));
        assert_eq!(e.facts.len(), 2);
        assert!(row(r#""dividend""#).event.is_none());
        assert!(row(r#"{"facts": []}"#).event.is_none(), "no type, no event");
        assert!(row("null").event.is_none());
        // And a page with such a row still reads.
        let page: alphai::NewsPage = serde_json::from_str(
            r#"{"results": [{"original": {"title": "A"}, "event": {"type": 5, "facts": {}}},
                            {"original": {"title": "B"}}]}"#,
        )
        .unwrap();
        assert_eq!(page.results.len(), 2);
    }

    #[test]
    fn unknown_types_and_fact_names_show_as_spelled() {
        let mut a = case("crwv_offering");
        a.event = Some(event("spin_off", &[("new_field", "x")]));
        let e = shown(&a).unwrap();
        assert_eq!(label(e), "spin off");
        assert_eq!(name(&e.kind), "spin off");
        assert_eq!(fact_label("new_field"), "new field");
        assert!(
            !is_material(e),
            "a new type is judged before it gets the lead"
        );
    }

    #[test]
    fn labels_carry_the_direction_the_document_states() {
        assert_eq!(label(&ev("patk_div")), "dividend ↑");
        assert_eq!(label(&ev("cinf_div")), "dividend");
        assert_eq!(label(&ev("crwv_offering")), "offering");
        assert_eq!(label(&ev("crwv_debt")), "debt");
        assert_eq!(label(&ev("nvda_exec")), "exec");
        assert_eq!(label(&ev("nvda_vote")), "vote");
        let g = |d: &str| label(&event("guidance_update", &[("direction", d)]));
        assert_eq!(g("lower"), "guide ↓");
        assert_eq!(g("reaffirm"), "guide =");
        assert_eq!(g("withdraw"), "guide off");
        assert_eq!(
            label(&event("buyback", &[("is_increase", "true")])),
            "buyback ↑"
        );
        assert_eq!(
            label(&event("stock_split", &[("is_reverse", "true")])),
            "rev split"
        );
        let l = |k: &str| label(&event("listing_notice", &[("kind", k)]));
        assert_eq!(l("deficiency"), "deficiency");
        assert_eq!(l("regained_compliance"), "compliant");
        assert_eq!(l("delisting"), "delisting");
        for k in &KINDS {
            assert!(k.label.chars().count() <= LABEL_MAX, "{} too wide", k.api);
        }
    }

    #[test]
    fn the_guidance_arrow_needs_a_metric_and_one_direction() {
        assert_eq!(guidance_arrow(&ev("crwv_earnings")), Some("↑"));
        // Hudbay lowered a cost per pound: no metric, no arrow.
        assert_eq!(guidance_arrow(&ev("hbm_6k")), None);
        let mut mixed = ev("crwv_earnings");
        let mut lower = mixed.also[0].clone();
        lower.facts.retain(|f| f.name != "direction");
        lower.facts.push(EventFact {
            name: "direction".into(),
            value: "lower".into(),
        });
        mixed.also.push(lower);
        assert_eq!(guidance_arrow(&mixed), None);
    }

    #[test]
    fn a_release_without_a_single_reporter_shows_no_event() {
        let roundup = case("prn_roundup");
        assert_eq!(reporter(&roundup), None, "a roundup of four tickers");
        assert!(shown(&roundup).is_none());
        assert!(for_row(&roundup, NewsScope::Market, "PLTR").is_none());
        assert_eq!(reporter(&case("patk_div")), Some("PATK"));
        assert_eq!(reporter(&case("lcii_target")), Some("LCII"));
    }

    #[test]
    fn another_companys_event_keeps_only_the_kinds_that_involve_both() {
        let lcii = case("lcii_target");
        assert!(
            for_row(&lcii, NewsScope::Ticker, "PATK").is_some(),
            "a deal involves both"
        );
        let mut dividend = case("patk_div");
        dividend.enrichment.tickers = vec!["LCII".into(), "PATK".into()];
        assert!(for_row(&dividend, NewsScope::Ticker, "PATK").is_none());
        assert!(for_row(&dividend, NewsScope::Market, "PATK").is_some());
    }

    #[test]
    fn sec_headings_are_told_apart_from_written_headlines() {
        for c in [
            "crwv_offering",
            "nvda_exec",
            "nvda_vote",
            "rain_mna",
            "tmgi_done",
            "hbm_6k",
        ] {
            assert!(is_template_title(&case(c)), "{c}");
        }
        for c in [
            "crwv_earnings",
            "fxnc_div",
            "lcii_target",
            "patk_div",
            "patk_news",
        ] {
            assert!(!is_template_title(&case(c)), "{c}");
        }
        let mut other = case("crwv_offering");
        other.enrichment.tickers = vec!["NBIS".into()];
        assert!(
            !is_template_title(&other),
            "the ticker must be the row's own"
        );
    }

    #[test]
    fn sec_headings_become_the_event_summary_figures_first() {
        let t = |c: &str, symbol: &str| {
            row_title(&case(c), NewsScope::Ticker, symbol, 70)
                .text
                .into_owned()
        };
        assert_eq!(
            t("crwv_offering", "CRWV"),
            "$3.7B · priced · upsized · 2.875% convertible senior notes due 2033"
        );
        assert_eq!(
            t("nvda_exec", "NVDA"),
            "Nicholas Parker appointed EVP, Worldwide Field Operations · replaces Ajay K. Puri"
        );
        assert_eq!(
            t("nvda_mna", "NVDA"),
            "deal with SB Energy · $1.5B · announced"
        );
        assert_eq!(
            t("crwv_debt", "CRWV"),
            "$2.6B · SOFR + 5.50% · delayed draw term loan facility"
        );
        // No facts to tell: the heading loses only the company's name.
        assert_eq!(
            t("nvda_vote", "NVDA"),
            "Submission of Matters to a Vote of Security Holders"
        );
        assert_eq!(t("hbm_6k", "HBM"), "Financial results for Q3 2026");
        // Another company's filing in a ticker's feed names the filer.
        assert!(t("crwv_offering", "NVDA").starts_with("CRWV: $3.7B"));
        assert!(t("nvda_vote", "AAPL").starts_with("NVDA: Submission"));
    }

    #[test]
    fn written_headlines_give_way_only_when_too_narrow_to_read() {
        let a = case("patk_div");
        let wide = row_title(&a, NewsScope::Ticker, "PATK", 200);
        assert_eq!(wide.text, a.original.title);
        assert_eq!(
            wide.detail.as_deref(),
            Some("$0.52 quarterly · was $0.47 · payable December 7, 2026")
        );
        // Another company's deal in this ticker's feed names its filer.
        let lcii_row = case("lcii_target");
        let lcii = row_title(&lcii_row, NewsScope::Events, "PATK", 300);
        assert_eq!(
            lcii.detail.as_deref(),
            Some("LCII: deal with Patrick Industries · definitive agreement")
        );
        let medium = row_title(&a, NewsScope::Ticker, "PATK", 73);
        assert_eq!(medium.text, a.original.title);
        assert_eq!(medium.detail, None, "the summary does not fit beside it");
        let narrow = row_title(&a, NewsScope::Ticker, "PATK", 31);
        assert_eq!(
            narrow.text,
            "$0.52 quarterly · was $0.47 · payable December 7, 2026"
        );
        // Another company's dividend is not this ticker's to summarize.
        let mut foreign = a.clone();
        foreign.enrichment.tickers = vec!["LCII".into(), "PATK".into()];
        assert_eq!(
            row_title(&foreign, NewsScope::Ticker, "PATK", 31).text,
            a.original.title
        );
    }

    #[test]
    fn chart_titles_name_the_type() {
        assert_eq!(
            chart_title(&case("crwv_offering"), "CRWV"),
            "offering · $3.7B · priced · upsized · 2.875% convertible senior notes due 2033"
        );
        assert_eq!(
            chart_title(&case("patk_news"), "PATK"),
            case("patk_news").original.title
        );
    }

    #[test]
    fn summaries_read_as_the_document_states_them() {
        assert_eq!(
            headline(&ev("fxnc_div")).unwrap(),
            "$0.19 quarterly · was $0.17 · payable December 11, 2026"
        );
        // The amount was withheld, and dates alone say less than the title.
        assert_eq!(headline(&ev("cinf_div")), None);
        assert_eq!(
            headline(&ev("lcii_target")).unwrap(),
            "deal with Patrick Industries · definitive agreement"
        );
        let also = |c: &str| -> Vec<String> { ev(c).also.iter().map(also_line).collect() };
        assert_eq!(also("patk_div"), ["buyback · $200.0M · was $61.9M"]);
        assert_eq!(
            also("crwv_earnings"),
            ["guidance · full year 2026 revenue · $13.0B to $13.3B · raised"]
        );
        assert_eq!(
            headline(&event(
                "dividend",
                &[
                    ("kind", "increase"),
                    ("frequency", "quarterly"),
                    ("change_pct", "4 percent"),
                    ("payable_date", "January 1, 2027"),
                ]
            ))
            .as_deref(),
            Some("quarterly · raised 4% · payable January 1, 2027")
        );
        assert_eq!(
            headline(&event(
                "debt_financing",
                &[
                    ("amount", "$1,250 million"),
                    ("interest_rate", "9.625%"),
                    ("instrument", "9.625% Senior Notes due 2032"),
                ]
            ))
            .as_deref(),
            Some("$1,250M · 9.625% Senior Notes due 2032"),
            "the rate the instrument already names is not repeated"
        );
        assert_eq!(
            headline(&event(
                "exec_change",
                &[
                    ("person_name", "Danielle Abrahamsen"),
                    ("title", "Interim Chief Financial Officer"),
                    ("action", "interim"),
                    ("predecessor_name", "Gary Merrill"),
                ]
            ))
            .as_deref(),
            Some("Danielle Abrahamsen named Interim CFO · replaces Gary Merrill")
        );
        assert_eq!(
            headline(&event(
                "exec_change",
                &[
                    ("person_name", "A B"),
                    ("title", "CEO"),
                    ("action", "resigned"),
                    ("predecessor_name", "A B"),
                ]
            ))
            .as_deref(),
            Some("A B resigned as CEO"),
            "a predecessor who is the person is no predecessor"
        );
    }

    #[test]
    fn summaries_without_enough_facts_give_way() {
        assert_eq!(headline(&event("exec_change", &[("title", "CFO")])), None);
        assert_eq!(
            headline(&event("m_and_a", &[("stage", "definitive")])),
            None
        );
        assert_eq!(
            headline(&event("dividend", &[("frequency", "quarterly")])),
            None
        );
        assert_eq!(headline(&event("bankruptcy", &[])), None);
        assert_eq!(headline(&event("restructuring", &[])), None);
        assert_eq!(
            headline(&event("dividend", &[("kind", "suspension")])).as_deref(),
            Some("suspended")
        );
        assert_eq!(
            headline(&event(
                "stock_split",
                &[
                    ("ratio", "1-for-10"),
                    ("is_reverse", "true"),
                    ("effective_date", "effective immediately"),
                ]
            ))
            .as_deref(),
            Some("1-for-10 reverse split · effective immediately")
        );
    }

    #[test]
    fn material_events_are_an_allow_list() {
        assert!(is_material(&ev("crwv_offering")));
        assert!(is_material(&ev("patk_div")));
        assert!(!is_material(&ev("cinf_div")));
        assert!(!is_material(&ev("crwv_earnings")));
        assert!(!is_material(&ev("nvda_vote")));
        assert!(!is_material(&event(
            "guidance_update",
            &[("direction", "reaffirm")]
        )));
        assert!(!is_material(&event(
            "listing_notice",
            &[("kind", "regained_compliance")]
        )));
    }

    #[test]
    fn the_card_heading_takes_the_enumerated_facts_and_the_table_the_rest() {
        let e = ev("crwv_offering");
        assert_eq!(heading_words(&e), ["upsized"]);
        let names: Vec<&str> = card_facts(&e).iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "gross_proceeds",
                "coupon_pct",
                "maturity",
                "instrument",
                "use_of_proceeds"
            ]
        );
        let e = ev("patk_div");
        assert_eq!(heading_words(&e), ["quarterly", "increase"]);
        let names: Vec<&str> = card_facts(&e).iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "amount_per_share",
                "prior_amount_per_share",
                "record_date",
                "payable_date"
            ]
        );
    }
}
