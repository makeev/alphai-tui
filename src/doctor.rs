//! `--doctor`: one plain-text report on everything the app leans on, the
//! config, the keys, each price source with a key, AlphAI, the quote cache
//! and the terminal, meant to be pasted into an issue as it is.
//!
//! The sections are built by pure functions from facts gathered up front,
//! so every message is tested without a network. `run` is the thin part
//! that reads files, asks the sources and prints. Nothing here writes: a
//! check that created a config or a cache would change the very state it
//! reports on.
//!
//! The report goes into public issues, so no key is printed: each one shows
//! masked the way the settings screen shows it, and a last pass over the
//! finished text masks any key that came back inside an error (Finnhub
//! carries its token in the URL, and a failed request prints the URL), and
//! turns the home directory into `~`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::alphai::{self, Probe};
use crate::cache::{self, Summary};
use crate::config::{self, Config, FileState, KeyField, KeyOrigin};
use crate::domain::{Interval, Range, Sessions, fmt_price};
use crate::market::{self, Session};
use crate::portfolio;
use crate::source::extended::{Coverage, ExtendedSource, Provider};
use crate::source::http::Budget;
use crate::source::{self, registry};

/// The ticker each source is asked for, unless one is given on the command
/// line: listed everywhere, liquid, priced by every source.
pub const DEFAULT_SYMBOL: &str = "AAPL";

const CRATES_URL: &str = "https://crates.io/api/v1/crates/alphai-tui";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
    Info,
    /// Not configured, and fine that way.
    Off,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
            Level::Info => "info",
            Level::Off => "--",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: String,
    pub lines: Vec<Line>,
}

impl Section {
    fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            lines: Vec::new(),
        }
    }

    fn push(&mut self, level: Level, text: impl Into<String>) {
        self.lines.push(Line {
            level,
            text: text.into(),
        });
    }
}

/// A section as text: the title, then one indented line per finding with
/// its level as a word. No color: the word carries the meaning, and the
/// text is headed for an issue.
pub fn render_section(section: &Section) -> String {
    let mut out = format!("{}\n", section.title);
    for line in &section.lines {
        out.push_str(&format!("  {:<6}{}\n", line.level.tag(), line.text));
    }
    out
}

/// 1 when anything failed, 0 otherwise: a warning does not fail the check.
pub fn exit_code(sections: &[Section]) -> i32 {
    let failed = sections
        .iter()
        .flat_map(|s| &s.lines)
        .any(|l| l.level == Level::Fail);
    i32::from(failed)
}

/// The last pass over the report: every key value the config or the
/// environment holds is masked wherever it shows up, and the home
/// directory becomes `~`.
pub struct Redact {
    secrets: Vec<String>,
    home: Option<String>,
}

impl Redact {
    pub fn new(mut secrets: Vec<String>, home: Option<&Path>) -> Self {
        // A value this short is no real key, and replacing it everywhere
        // would cut words and numbers out of the report.
        secrets.retain(|s| s.trim().chars().count() >= 8);
        // Longest first, so a key that contains another is masked whole.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let home = home
            .map(|h| {
                h.to_string_lossy()
                    .trim_end_matches(['/', '\\'])
                    .to_string()
            })
            // A home of "/" would turn every path into a tilde.
            .filter(|h| h.len() > 1);
        Self { secrets, home }
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            out = out.replace(secret.trim(), &mask(secret));
        }
        if let Some(home) = &self.home {
            out = tilde(&out, home);
        }
        out
    }
}

/// `home` replaced by `~` where it is a whole path component: `/home/al`
/// must not turn `/home/alex` into `~ex`.
fn tilde(text: &str, home: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(home) {
        let after = &rest[at + home.len()..];
        let whole = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')));
        out.push_str(&rest[..at]);
        out.push_str(if whole { "~" } else { home });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// A key as the report shows it: the first two characters (paper and live
/// Alpaca keys differ there) and the last four. Shorter than the settings
/// screen's mask on purpose: that one stays on the owner's screen, this
/// one goes into public issues, where half of a 20-character token is too
/// much of it.
fn mask(key: &str) -> String {
    let key = key.trim();
    let n = key.chars().count();
    if n <= 12 {
        return "••••••".into();
    }
    let start: String = key.chars().take(2).collect();
    let end: String = key.chars().skip(n - 4).collect();
    format!("{start}…{end}")
}

// ---------------------------------------------------------------- Config

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigFacts {
    /// None on a platform with no config directory.
    pub path: Option<PathBuf>,
    pub state: FileState,
    /// Permission bits of the file, on unix.
    pub mode: Option<u32>,
    /// `<config>.broken`, when the settings screen kept one.
    pub broken_copy: Option<PathBuf>,
    /// What `config::resolve` warned about.
    pub warnings: Vec<String>,
    pub watchlist: std::result::Result<Watch, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    /// False when the config holds no watchlist and the default one runs.
    pub saved: bool,
    pub watched: usize,
    pub held: usize,
    pub polled: usize,
}

pub fn config_section(c: &ConfigFacts) -> Section {
    let mut s = Section::new("Config");
    let Some(path) = &c.path else {
        s.push(
            Level::Warn,
            "no config directory on this platform: settings cannot be saved, pass --config PATH",
        );
        return s;
    };
    let shown = path.display();
    match &c.state {
        FileState::Missing => s.push(
            Level::Info,
            format!(
                "no config file yet at {shown}: running on defaults (s in the app saves one)"
            ),
        ),
        FileState::Loaded => match c.mode {
            Some(mode) if mode & 0o077 != 0 => s.push(
                Level::Warn,
                format!(
                    "{shown} loads, but other users can read it (mode {:04o}) and it holds keys: chmod 600 it",
                    mode & 0o777
                ),
            ),
            Some(mode) => s.push(
                Level::Ok,
                format!("{shown} loads, mode {:04o}", mode & 0o777),
            ),
            None => s.push(Level::Ok, format!("{shown} loads")),
        },
        FileState::Broken(why) => {
            s.push(
                Level::Fail,
                format!(
                    "{shown} does not load ({why}): the app runs on defaults and will not write over it"
                ),
            );
            if let Some(copy) = &c.broken_copy {
                s.push(
                    Level::Info,
                    format!("an earlier copy that did not load is kept at {}", copy.display()),
                );
            }
        }
    }
    for w in &c.warnings {
        s.push(Level::Warn, w.clone());
    }
    match &c.watchlist {
        Ok(w) if !w.saved => s.push(
            Level::Info,
            format!(
                "no saved watchlist, the default {} runs ({}); holdings {}, polled {}",
                w.watched,
                config::DEFAULT_WATCHLIST.join(" "),
                w.held,
                w.polled
            ),
        ),
        Ok(w) => s.push(
            Level::Ok,
            format!(
                "watchlist {}, holdings {}, polled {}",
                w.watched, w.held, w.polled
            ),
        ),
        Err(e) => s.push(Level::Fail, e.clone()),
    }
    s
}

// ------------------------------------------------------------------ Keys

#[derive(Clone, Debug, PartialEq)]
pub struct KeyState {
    pub label: &'static str,
    pub env_var: &'static str,
    pub found: Option<(String, KeyOrigin)>,
}

/// The keys one consumer needs together: AlphAI's one, or a source's.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyGroup {
    pub owner: &'static str,
    pub keys: Vec<KeyState>,
}

pub fn key_groups(cfg: &Config) -> Vec<KeyGroup> {
    let state = |f: &KeyField| KeyState {
        label: f.label,
        env_var: f.env_var,
        found: cfg.key_value(f).zip(cfg.key_origin(f)),
    };
    let mut groups = vec![KeyGroup {
        owner: "AlphAI",
        keys: vec![state(&config::ALPHAI_KEY_FIELD)],
    }];
    for info in registry::SOURCES
        .iter()
        .filter(|i| !i.key_fields.is_empty())
    {
        groups.push(KeyGroup {
            owner: info.id,
            keys: info.key_fields.iter().map(state).collect(),
        });
    }
    groups
}

pub fn keys_section(groups: &[KeyGroup], config_broken: bool) -> Section {
    let mut s = Section::new("Keys");
    let width = groups
        .iter()
        .flat_map(|g| &g.keys)
        .map(|k| k.label.len())
        .max()
        .unwrap_or(0);
    let mut unset = Vec::new();
    for group in groups {
        let found = group.keys.iter().filter(|k| k.found.is_some()).count();
        if found == 0 {
            unset.extend(group.keys.iter().map(|k| k.label));
            continue;
        }
        for key in &group.keys {
            match &key.found {
                Some((value, origin)) => {
                    let origin = match origin {
                        KeyOrigin::Env => format!("env {}", key.env_var),
                        KeyOrigin::File => "config".to_string(),
                    };
                    s.push(
                        Level::Ok,
                        format!(
                            "{:<width$}  {}  {origin}",
                            key.label,
                            mask(value)
                        ),
                    );
                }
                None => s.push(
                    Level::Fail,
                    format!(
                        "{:<width$}  not set: {} needs every one of its keys ({} or the settings screen)",
                        key.label, group.owner, key.env_var
                    ),
                ),
            }
        }
    }
    if !unset.is_empty() {
        s.push(Level::Off, format!("{}: not set", unset.join(", ")));
    }
    if config_broken {
        s.push(
            Level::Info,
            "the config did not load, so only keys from the environment are read",
        );
    }
    s
}

// ---------------------------------------------------------------- Prices

#[derive(Clone, Debug, PartialEq)]
pub struct PriceOk {
    pub price: f64,
    pub currency: Option<String>,
    pub elapsed: Duration,
    pub delay: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriceProbe {
    pub id: &'static str,
    pub result: std::result::Result<PriceOk, String>,
    /// What the key had left after the check, also when the fetch failed:
    /// a drained Alpaca key can answer one of its two requests and refuse
    /// the other, and the count is the explanation.
    pub budget: Option<Budget>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriceFacts {
    pub symbol: String,
    /// The source the app would start on, or why that name is no source.
    pub active: std::result::Result<&'static str, String>,
    /// The active source's missing-key message, when its keys are absent.
    pub active_unkeyed: Option<String>,
    pub probes: Vec<PriceProbe>,
    pub every: u64,
    /// Tickers polled; None when the watchlist does not parse.
    pub polled: Option<usize>,
    /// Requests a minute polling costs and the plan's ceiling, for a source
    /// that publishes one. The others are left out: their cost is not
    /// tickers times interval (Tiingo quotes the whole list in one
    /// request), and nothing would compare against the number anyway.
    pub per_min: Option<(u64, u32)>,
    pub rate_warning: Option<String>,
    /// Where pre and post market come from, as a sentence.
    pub extended: Option<String>,
    pub fallback: bool,
}

pub fn prices_section(p: &PriceFacts) -> Section {
    let mut s = Section::new(format!("Prices ({}, one fetch per source)", p.symbol));
    let active = match &p.active {
        Ok(id) => Some(*id),
        Err(e) => {
            s.push(Level::Fail, e.clone());
            None
        }
    };
    if let (Some(id), Some(msg)) = (active, &p.active_unkeyed) {
        s.push(Level::Fail, format!("{id} is the source in use, but {msg}"));
    }
    for probe in &p.probes {
        let in_use = active == Some(probe.id);
        let tag = if in_use { "  in use" } else { "" };
        let budget = probe
            .budget
            .map(|b| format!(", key budget {} of {} this minute", b.remaining, b.limit))
            .unwrap_or_default();
        match &probe.result {
            Ok(ok) => {
                let price = match &ok.currency {
                    Some(cur) => format!("{} {cur}", fmt_price(ok.price)),
                    None => fmt_price(ok.price),
                };
                let mut text = format!("{:<8} {price}  {} ms", probe.id, ok.elapsed.as_millis());
                if let Some(delay) = ok.delay {
                    text.push_str(&format!(", {delay}"));
                }
                text.push_str(&budget);
                text.push_str(tag);
                s.push(Level::Ok, text);
            }
            Err(e) => {
                // A failure of the source in use is the app's failure; a
                // spare source failing is worth knowing, not fatal.
                let (level, lead) = if in_use {
                    (Level::Fail, "the source in use: ")
                } else {
                    (Level::Warn, "")
                };
                s.push(level, format!("{:<8} {lead}{e}{budget}", probe.id));
            }
        }
        if let Some(b) = probe.budget
            && b.remaining * 4 < b.limit
        {
            s.push(
                Level::Warn,
                format!(
                    "{}: {} of {} requests left this minute after this check: something else is spending the key (another app, or another alphai-tui pane)",
                    probe.id, b.remaining, b.limit
                ),
            );
        }
    }
    if let (Some(id), Some(polled)) = (active, p.polled) {
        match &p.rate_warning {
            Some(w) => s.push(Level::Warn, w.clone()),
            None => {
                let cost = p
                    .per_min
                    .map(|(n, limit)| {
                        format!(", about {n} of the {limit} requests a minute it allows")
                    })
                    .unwrap_or_default();
                s.push(
                    Level::Info,
                    format!("{id} polls {polled} tickers every {}s{cost}", p.every),
                );
            }
        }
    }
    if let Some(ext) = &p.extended {
        s.push(Level::Info, format!("pre and after hours: {ext}"));
    }
    s.push(
        Level::Info,
        if p.fallback {
            "fallback on: a source that stops answering for every ticker is swapped for one that works"
        } else {
            "fallback off (source_fallback = false): a source that stops answering stays"
        },
    );
    s
}

/// Where pre and post market prices come from, as one sentence.
pub fn describe_extended(
    choice: ExtendedSource,
    provider: Provider,
    id: &str,
    coverage: Coverage,
) -> String {
    let what = match provider {
        Provider::Own => match coverage {
            Coverage::Market => format!("{id}'s own, the whole market"),
            Coverage::Venue => {
                format!("{id}'s own, one exchange that barely trades outside the session")
            }
            Coverage::Nothing => format!("none, {id} has no pre or after hours"),
        },
        Provider::Sip { backup: true } => {
            "Alpaca's consolidated tape, 15 minutes behind, Yahoo as backup".to_string()
        }
        Provider::Sip { backup: false } => {
            "Alpaca's consolidated tape, 15 minutes behind".to_string()
        }
        Provider::Yahoo => "Yahoo".to_string(),
    };
    format!("{what} (extended_source {})", choice.name())
}

// ---------------------------------------------------------------- AlphAI

#[derive(Clone, Debug, PartialEq)]
pub enum AlphaiFacts {
    NoKey,
    Answered { symbol: String, probe: Probe },
    Failed(String),
}

pub fn alphai_section(a: &AlphaiFacts, now: DateTime<Utc>) -> Section {
    let mut s = Section::new("AlphAI");
    match a {
        AlphaiFacts::NoKey => s.push(
            Level::Info,
            "no key: News, Insider, Earnings and Calendar stay empty. A free key is at https://alphai.io",
        ),
        AlphaiFacts::Failed(e) => s.push(Level::Fail, e.clone()),
        AlphaiFacts::Answered { symbol, probe } => {
            match probe.budget {
                Some(b) => {
                    let plan = b.plan().map(|p| format!("{p} plan, ")).unwrap_or_default();
                    let reset = b
                        .reset
                        .and_then(|ts| DateTime::from_timestamp(ts, 0))
                        .map(|at| {
                            let left = (at - now).num_seconds().max(0);
                            format!(
                                ", resets {} (in {})",
                                at.format("%H:%M UTC"),
                                span(left)
                            )
                        })
                        .unwrap_or_default();
                    let text = format!(
                        "{plan}{} of {} requests left today{reset}",
                        group(b.remaining),
                        group(b.limit)
                    );
                    if b.remaining * 10 < b.limit {
                        s.push(
                            Level::Warn,
                            format!("{text}: the feeds stop fetching when it runs out"),
                        );
                    } else {
                        s.push(Level::Ok, text);
                    }
                }
                None => s.push(
                    Level::Ok,
                    "the key works; the answer did not say what is left of the day",
                ),
            }
            if !probe.known {
                s.push(
                    Level::Info,
                    format!("AlphAI lists no ticker {symbol} (crypto is not covered); the key still answered"),
                );
            }
        }
    }
    s
}

// ----------------------------------------------------------------- Cache

pub fn cache_section(path: Option<&Path>, summary: Option<&Summary>, now: i64) -> Section {
    let mut s = Section::new("Cache");
    let (Some(path), Some(summary)) = (path, summary) else {
        s.push(
            Level::Info,
            "no cache directory on this platform: prices are not kept between runs",
        );
        return s;
    };
    let shown = path.display();
    match summary {
        Summary::Missing => s.push(
            Level::Info,
            format!("{shown}: none yet, written once prices arrive"),
        ),
        Summary::Unusable(why) => s.push(
            Level::Warn,
            format!(
                "{shown} cannot be used ({why}): the app starts without cached prices and writes a new one"
            ),
        ),
        Summary::Usable {
            bytes,
            entries,
            fresh,
            newest,
        } => {
            let age = newest
                .map(|ts| format!(", newest {} ago", span((now - ts).max(0))))
                .unwrap_or_default();
            let size = file_size(*bytes);
            if *fresh == 0 {
                s.push(
                    Level::Info,
                    format!(
                        "{shown}, {size}, {entries} entries{age}: none younger than a week, so the app starts without cached prices"
                    ),
                );
            } else {
                s.push(
                    Level::Ok,
                    format!("{shown}, {size}, {fresh} of {entries} entries usable{age}"),
                );
            }
        }
    }
    s
}

// -------------------------------------------------------------- Terminal

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TermFacts {
    pub no_color: bool,
    pub term: Option<String>,
    pub colorterm: Option<String>,
    pub term_program: Option<String>,
    pub tmux: bool,
    pub size: Option<(u16, u16)>,
    pub theme: String,
    /// Whether the theme paints with 24-bit colors.
    pub theme_rgb: bool,
}

impl TermFacts {
    fn truecolor(&self) -> bool {
        self.colorterm.as_deref().is_some_and(|c| {
            matches!(
                c.trim().to_ascii_lowercase().as_str(),
                "truecolor" | "24bit"
            )
        })
    }
}

pub fn terminal_section(t: &TermFacts) -> Section {
    let mut s = Section::new("Terminal");
    let mut parts = vec![
        format!("TERM {}", t.term.as_deref().unwrap_or("unset")),
        format!("COLORTERM {}", t.colorterm.as_deref().unwrap_or("unset")),
    ];
    // tmux sets TERM_PROGRAM to its own name; the line says tmux anyway.
    if let Some(program) = t.term_program.as_ref().filter(|p| !t.tmux || *p != "tmux") {
        parts.push(program.clone());
    }
    if let Some((w, h)) = t.size {
        parts.push(format!("{w}x{h}"));
    }
    if t.tmux {
        parts.push("inside tmux".into());
    }
    parts.push(format!("theme {}", t.theme));
    s.push(Level::Ok, parts.join(", "));
    if t.no_color {
        s.push(
            Level::Warn,
            "NO_COLOR is set: the app draws without any color, which reads as broken (run it as NO_COLOR= alphai-tui)",
        );
    }
    if t.theme_rgb && !t.truecolor() {
        s.push(
            Level::Warn,
            format!(
                "theme {} uses 24-bit colors and COLORTERM does not announce truecolor: if the colors look wrong, try --theme default",
                t.theme
            ),
        );
    }
    match t.size {
        Some((w, h)) if w < 80 || h < 24 => s.push(
            Level::Warn,
            format!("{w}x{h} is under 80x24: some panels and the settings screen will not fit"),
        ),
        Some(_) => {}
        None => s.push(Level::Info, "no terminal attached, size not checked"),
    }
    s
}

// ---------------------------------------------------------------- Market

pub fn market_section(now: DateTime<Utc>) -> Section {
    let mut s = Section::new("Market");
    let clock = market::clock_at(now);
    let session = match clock.session {
        Session::Pre => "in pre-market",
        Session::Open => "open",
        Session::Post => "in after hours",
        Session::Closed => "closed",
    };
    s.push(
        Level::Info,
        format!("US market {session}, {}", clock.countdown()),
    );
    s
}

// --------------------------------------------------------------- Version

pub fn version_section(current: &str, latest: &std::result::Result<String, String>) -> Section {
    let mut s = Section::new("Version");
    match latest {
        Err(e) => s.push(
            Level::Info,
            format!("{current}; could not ask crates.io for the latest release ({e})"),
        ),
        Ok(latest) => match (version(current), version(latest)) {
            (Some(c), Some(l)) if l > c => s.push(
                Level::Warn,
                format!("{latest} is out, this is {current}: upgrade the way you installed it"),
            ),
            (Some(c), Some(l)) if l < c => s.push(
                Level::Ok,
                format!("{current}, newer than the latest release {latest}"),
            ),
            (Some(_), Some(_)) => s.push(Level::Ok, format!("{current}, the latest release")),
            _ => s.push(
                Level::Info,
                format!("{current}; crates.io says the latest is {latest}"),
            ),
        },
    }
    s
}

/// `MAJOR.MINOR.PATCH` as numbers, a pre-release suffix ignored.
fn version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

// --------------------------------------------------------------- helpers

/// "45s", "12m", "3h05m", "2d4h": a duration the way a reader says it.
fn span(secs: i64) -> String {
    match secs {
        ..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// "12 KB", "6.3 MB".
fn file_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// 96429 as "96,429".
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ------------------------------------------------------------------- run

/// What `run` takes from the command line.
pub struct Options {
    pub config: Option<PathBuf>,
    pub source: Option<String>,
    pub every: Option<u64>,
    pub theme: Option<String>,
    pub symbol: Option<String>,
}

/// Gathers the facts, prints each section as soon as it is known (the
/// network ones take seconds) and returns the exit code.
pub fn run(opts: Options) -> Result<i32> {
    let now = Utc::now();
    let path = opts.config.clone().or_else(config::path);
    let (cfg, state) = match path.as_deref().map(config::load_from) {
        None | Some(Ok(None)) => (Config::default(), FileState::Missing),
        Some(Ok(Some(cfg))) => (cfg, FileState::Loaded),
        Some(Err(e)) => (Config::default(), FileState::Broken(config::load_error(&e))),
    };
    let (resolved, warnings) = config::resolve(&cfg, opts.theme.as_deref());
    let watchlist = crate::startup_symbols(&[], &cfg.watchlist).map(|symbols| Watch {
        saved: !cfg.watchlist.is_empty(),
        watched: symbols.len(),
        held: resolved.positions.len(),
        polled: portfolio::polled_symbols(&symbols, &resolved.positions).len(),
    });
    let polled = watchlist.as_ref().ok().map(|w| w.polled);

    let mut secrets: Vec<String> = cfg.keys.values().cloned().collect();
    for group in key_groups(&cfg) {
        secrets.extend(group.keys.into_iter().filter_map(|k| k.found.map(|f| f.0)));
    }
    let redact = Redact::new(secrets, dirs::home_dir().as_deref());
    let mut sections = Vec::new();
    let mut print = |section: Section| {
        println!("{}", redact.apply(&render_section(&section)));
        let _ = std::io::stdout().flush();
        sections.push(section);
    };

    println!(
        "alphai-tui {} doctor, {} {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    print(config_section(&ConfigFacts {
        broken_copy: path.as_deref().map(broken_copy).filter(|p| p.exists()),
        mode: path.as_deref().and_then(file_mode),
        path: path.clone(),
        state: state.clone(),
        warnings,
        watchlist: watchlist.map_err(|e| e.to_string()),
    }));
    print(keys_section(
        &key_groups(&cfg),
        matches!(state, FileState::Broken(_)),
    ));

    let symbol = opts
        .symbol
        .clone()
        .unwrap_or_else(|| DEFAULT_SYMBOL.to_string());
    let rt = tokio::runtime::Runtime::new()?;
    // AlphAI and crates.io answer while the price sources are asked one by
    // one; the sources go in turn so each timing is its own.
    let alphai = rt.spawn(probe_alphai(cfg.alphai_key(), symbol.clone()));
    let latest = rt.spawn(latest_release());

    let name = opts
        .source
        .clone()
        .or_else(|| cfg.source.clone())
        .unwrap_or_else(|| registry::SOURCES[0].id.to_string());
    let every = opts.every.or(cfg.every).unwrap_or(15).max(2);
    let info = registry::find(&name);
    let probes = rt.block_on(probe_prices(&cfg, &symbol));
    let facts = PriceFacts {
        symbol: symbol.clone(),
        active: info.map(|i| i.id).ok_or_else(|| {
            format!(
                "unknown data source '{name}' (available: {})",
                registry::ids().join(", ")
            )
        }),
        active_unkeyed: info
            .filter(|i| i.key_fields.iter().any(|f| cfg.key_value(f).is_none()))
            .map(registry::missing_keys_msg),
        probes,
        every,
        polled,
        per_min: info.zip(polled).and_then(|(i, n)| {
            let limit = i.rate_limit_per_min?;
            Some((n as u64 * u64::from(i.reqs_per_symbol) * 60 / every, limit))
        }),
        rate_warning: info.zip(polled).and_then(|(i, polled)| {
            registry::rate_warning(
                i,
                polled,
                every,
                matches!(source::extended_provider(i, &cfg), Provider::Sip { .. }),
            )
        }),
        extended: info.map(|i| {
            describe_extended(
                ExtendedSource::from_config(&cfg),
                source::extended_provider(i, &cfg),
                i.id,
                (i.coverage)(),
            )
        }),
        fallback: cfg.source_fallback.unwrap_or(true),
    };
    print(prices_section(&facts));

    let alphai = rt
        .block_on(alphai)
        .unwrap_or_else(|e| AlphaiFacts::Failed(e.to_string()));
    print(alphai_section(&alphai, Utc::now()));

    let cache_path = cache::default_path();
    let summary = cache_path.as_deref().map(cache::summary);
    print(cache_section(
        cache_path.as_deref(),
        summary.as_ref(),
        now.timestamp(),
    ));
    print(terminal_section(&term_facts(
        resolved.theme_name,
        theme_is_rgb(&resolved.theme),
    )));
    print(market_section(now));
    let latest = rt.block_on(latest).unwrap_or_else(|e| Err(e.to_string()));
    print(version_section(env!("CARGO_PKG_VERSION"), &latest));

    let code = exit_code(&sections);
    if code != 0 {
        println!("Something failed: the lines marked fail say what.");
    }
    Ok(code)
}

fn broken_copy(path: &Path) -> PathBuf {
    let mut copy = path.as_os_str().to_owned();
    copy.push(".broken");
    PathBuf::from(copy)
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

/// One fetch per source that can be asked: Yahoo always, the others when
/// every key they need is set. Each is built bare, without the pre and
/// post market borrowing the app puts under it, so a refusal is the
/// source's own and the timing is one source's.
async fn probe_prices(cfg: &Config, symbol: &str) -> Vec<PriceProbe> {
    let mut out = Vec::new();
    for info in registry::SOURCES {
        let keys: Option<Vec<String>> = info.key_fields.iter().map(|f| cfg.key_value(f)).collect();
        let Some(keys) = keys else {
            continue;
        };
        let src = match (info.make)(&keys) {
            Ok(src) => src,
            Err(e) => {
                out.push(PriceProbe {
                    id: info.id,
                    result: Err(format!("{e:#}")),
                    budget: None,
                });
                continue;
            }
        };
        let started = Instant::now();
        src.begin_cycle(&[symbol.to_string()]);
        // The window the app opens on, so the check asks what the first
        // poll will ask.
        let result = src
            .fetch(symbol, Range::D5, Interval::M15, Sessions::Regular)
            .await
            .map(|data| PriceOk {
                price: data.quote.price,
                currency: data.quote.currency.clone(),
                elapsed: started.elapsed(),
                delay: src.delay_note(),
            })
            .map_err(|e| format!("{e:#}"));
        out.push(PriceProbe {
            id: info.id,
            result,
            budget: src.budget(),
        });
    }
    out
}

async fn probe_alphai(key: Option<String>, symbol: String) -> AlphaiFacts {
    let Some(key) = key else {
        return AlphaiFacts::NoKey;
    };
    let client = match alphai::Client::new(key) {
        Ok(c) => c,
        Err(e) => return AlphaiFacts::Failed(format!("{e:#}")),
    };
    match client.probe(&symbol).await {
        Ok(probe) => AlphaiFacts::Answered { symbol, probe },
        Err(e) => AlphaiFacts::Failed(format!("{e:#}")),
    }
}

/// The newest stable version on crates.io. Its own client and agent: the
/// crates.io policy asks for a way to reach the tool's maintainers, and the
/// app's own agent string is a format AlphAI counts installs by.
async fn latest_release() -> std::result::Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(rename = "crate")]
        krate: Crate,
    }
    #[derive(serde::Deserialize)]
    struct Crate {
        max_stable_version: String,
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "alphai-tui/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/makeev/alphai-tui)"
        ))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(CRATES_URL)
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("crates.io answered {}", resp.status()));
    }
    let body: Body = resp.json().await.map_err(|e| e.without_url().to_string())?;
    Ok(body.krate.max_stable_version)
}

fn term_facts(theme: &str, theme_rgb: bool) -> TermFacts {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    TermFacts {
        no_color: var("NO_COLOR").is_some(),
        term: var("TERM"),
        colorterm: var("COLORTERM"),
        term_program: var("TERM_PROGRAM"),
        tmux: var("TMUX").is_some(),
        size: crossterm::terminal::size()
            .ok()
            .filter(|(w, h)| *w > 0 && *h > 0),
        theme: theme.to_string(),
        theme_rgb,
    }
}

fn theme_is_rgb(theme: &crate::theme::Theme) -> bool {
    use ratatui::style::Color;
    [
        theme.accent,
        theme.up,
        theme.down,
        theme.text,
        theme.background,
        theme.surface,
    ]
    .iter()
    .any(|c| matches!(c, Color::Rgb(..)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-11-25T07:02:26Z".parse().unwrap()
    }

    fn texts(s: &Section) -> Vec<(Level, &str)> {
        s.lines.iter().map(|l| (l.level, l.text.as_str())).collect()
    }

    fn facts(state: FileState) -> ConfigFacts {
        ConfigFacts {
            path: Some(PathBuf::from("/home/u/.config/alphai-tui/config.toml")),
            state,
            mode: Some(0o100600),
            broken_copy: None,
            warnings: vec![],
            watchlist: Ok(Watch {
                saved: true,
                watched: 5,
                held: 2,
                polled: 6,
            }),
        }
    }

    #[test]
    fn a_loaded_private_config_is_ok_and_counts_the_tickers() {
        let s = config_section(&facts(FileState::Loaded));
        assert_eq!(
            texts(&s),
            [
                (
                    Level::Ok,
                    "/home/u/.config/alphai-tui/config.toml loads, mode 0600"
                ),
                (Level::Ok, "watchlist 5, holdings 2, polled 6"),
            ]
        );
    }

    #[test]
    fn a_config_others_can_read_is_a_warning() {
        let mut f = facts(FileState::Loaded);
        f.mode = Some(0o100644);
        let s = config_section(&f);
        assert_eq!(s.lines[0].level, Level::Warn);
        assert!(s.lines[0].text.contains("mode 0644"), "{}", s.lines[0].text);
        assert!(s.lines[0].text.contains("chmod 600"));
        assert_eq!(exit_code(&[s]), 0, "a warning does not fail the check");
    }

    #[test]
    fn a_broken_config_fails_and_points_at_the_copy() {
        let mut f = facts(FileState::Broken("expected `=`, at line 3".into()));
        f.broken_copy = Some(PathBuf::from(
            "/home/u/.config/alphai-tui/config.toml.broken",
        ));
        f.warnings = vec!["[ui] borders: unknown \"nonee\"".into()];
        let s = config_section(&f);
        assert_eq!(s.lines[0].level, Level::Fail);
        assert!(s.lines[0].text.contains("expected `=`, at line 3"));
        assert!(s.lines[1].text.contains("config.toml.broken"));
        assert_eq!(
            s.lines[2],
            Line {
                level: Level::Warn,
                text: "[ui] borders: unknown \"nonee\"".into()
            }
        );
        assert_eq!(exit_code(&[s]), 1);
    }

    #[test]
    fn a_first_run_and_a_bad_watchlist_read_apart() {
        let mut f = facts(FileState::Missing);
        f.watchlist = Ok(Watch {
            saved: false,
            watched: 4,
            held: 0,
            polled: 4,
        });
        let s = config_section(&f);
        assert_eq!(s.lines[0].level, Level::Info);
        assert!(s.lines[0].text.contains("no config file yet"));
        assert!(s.lines[1].text.contains("AAPL MSFT NVDA BTC-USD"));
        f.watchlist = Err("invalid ticker \"NASDAQ COMPOSITE\" in config watchlist".into());
        let s = config_section(&f);
        assert_eq!(s.lines[1].level, Level::Fail);
    }

    fn key(label: &'static str, env: &'static str, found: Option<(&str, KeyOrigin)>) -> KeyState {
        KeyState {
            label,
            env_var: env,
            found: found.map(|(v, o)| (v.to_string(), o)),
        }
    }

    #[test]
    fn keys_show_masked_with_where_they_came_from() {
        let groups = vec![
            KeyGroup {
                owner: "AlphAI",
                keys: vec![key(
                    "AlphAI key",
                    "ALPHAI_API_KEY",
                    Some(("ak_live_0123456789abcdef9f3c", KeyOrigin::File)),
                )],
            },
            KeyGroup {
                owner: "finnhub",
                keys: vec![key("Finnhub key", "FINNHUB_API_KEY", None)],
            },
            KeyGroup {
                owner: "alpaca",
                keys: vec![
                    key(
                        "Alpaca key ID",
                        "APCA_API_KEY_ID",
                        Some(("PKAB12CDEFGHXY7Q", KeyOrigin::Env)),
                    ),
                    key("Alpaca secret", "APCA_API_SECRET_KEY", None),
                ],
            },
            KeyGroup {
                owner: "tiingo",
                keys: vec![key("Tiingo key", "TIINGO_API_KEY", None)],
            },
        ];
        let s = keys_section(&groups, false);
        assert_eq!(
            texts(&s),
            [
                (Level::Ok, "AlphAI key     ak…9f3c  config"),
                (Level::Ok, "Alpaca key ID  PK…XY7Q  env APCA_API_KEY_ID"),
                (
                    Level::Fail,
                    "Alpaca secret  not set: alpaca needs every one of its keys (APCA_API_SECRET_KEY or the settings screen)"
                ),
                (Level::Off, "Finnhub key, Tiingo key: not set"),
            ]
        );
        let out = render_section(&s);
        assert!(!out.contains("ak_live_0123456789abcdef9f3c"));
        assert!(!out.contains("PKAB12CDEFGHXY7Q"));
    }

    #[test]
    fn redact_masks_keys_inside_errors_and_tilde_the_home() {
        let token = "d4abcdefghijklmnopqr";
        let redact = Redact::new(
            vec![token.into(), "  ".into(), "ak_live_secretvalue1234".into()],
            Some(Path::new("/Users/someone")),
        );
        let error = format!(
            "request failed: error sending request for url (https://finnhub.io/api/v1/quote?symbol=AAPL&token={token})"
        );
        let out = redact.apply(&format!(
            "{error}\n/Users/someone/.config/alphai-tui/config.toml loads\nak_live_secretvalue1234"
        ));
        assert!(!out.contains(token), "{out}");
        assert!(!out.contains("ak_live_secretvalue1234"), "{out}");
        assert!(out.contains("token=d4…opqr"), "{out}");
        assert_eq!(mask("short-key"), "••••••");
        assert!(out.contains("~/.config/alphai-tui/config.toml"), "{out}");
        let short = Redact::new(vec!["200".into()], None);
        assert_eq!(short.apply("1 of 200"), "1 of 200");
        let al = Redact::new(vec![], Some(Path::new("/home/al")));
        assert_eq!(
            al.apply("/home/al/.config and /home/alex/x, /home/al"),
            "~/.config and /home/alex/x, ~"
        );
        // A root home would turn every path into a tilde.
        let root = Redact::new(vec![], Some(Path::new("/")));
        assert_eq!(root.apply("/etc/x"), "/etc/x");
    }

    fn price_facts(probes: Vec<PriceProbe>) -> PriceFacts {
        PriceFacts {
            symbol: "AAPL".into(),
            active: Ok("alpaca"),
            active_unkeyed: None,
            probes,
            every: 15,
            polled: Some(6),
            per_min: Some((48, 200)),
            rate_warning: None,
            extended: Some("Alpaca's consolidated tape (extended_source auto)".into()),
            fallback: true,
        }
    }

    fn probe(
        id: &'static str,
        result: std::result::Result<f64, &str>,
        remaining: Option<u64>,
    ) -> PriceProbe {
        PriceProbe {
            id,
            result: result
                .map(|price| PriceOk {
                    price,
                    currency: Some("USD".into()),
                    elapsed: Duration::from_millis(180),
                    delay: None,
                })
                .map_err(str::to_string),
            budget: remaining.map(|remaining| Budget {
                remaining,
                limit: 200,
            }),
        }
    }

    #[test]
    fn prices_name_the_source_in_use_and_its_budget() {
        let s = prices_section(&price_facts(vec![
            probe("yahoo", Err("yahoo is blocking this IP"), None),
            probe("alpaca", Ok(268.8), Some(187)),
        ]));
        assert_eq!(s.title, "Prices (AAPL, one fetch per source)");
        assert_eq!(
            s.lines[0],
            Line {
                level: Level::Warn,
                text: "yahoo    yahoo is blocking this IP".into()
            },
            "a spare source failing only warns"
        );
        assert_eq!(s.lines[1].level, Level::Ok);
        assert_eq!(
            s.lines[1].text,
            "alpaca   268.80 USD  180 ms, key budget 187 of 200 this minute  in use"
        );
        assert_eq!(
            s.lines[2].text,
            "alpaca polls 6 tickers every 15s, about 48 of the 200 requests a minute it allows"
        );
        assert_eq!(exit_code(&[s]), 0);
    }

    /// Tiingo quotes the whole list in one request, so tickers times the
    /// interval would overstate it several times over: a source without
    /// a published ceiling gets no estimate at all.
    #[test]
    fn no_request_estimate_without_a_published_ceiling() {
        let mut f = price_facts(vec![probe("tiingo", Ok(354.07), None)]);
        f.active = Ok("tiingo");
        f.per_min = None;
        f.every = 2;
        f.polled = Some(5);
        let s = prices_section(&f);
        assert_eq!(s.lines[1].text, "tiingo polls 5 tickers every 2s");
        // A watchlist that does not parse polls nothing: no line for it.
        f.polled = None;
        let s = prices_section(&f);
        assert!(s.lines.iter().all(|l| !l.text.contains("polls")), "{s:?}");
    }

    #[test]
    fn a_drained_key_warns_and_a_failing_source_in_use_fails() {
        let s = prices_section(&price_facts(vec![probe("alpaca", Ok(268.8), Some(3))]));
        assert_eq!(s.lines[1].level, Level::Warn);
        assert!(s.lines[1].text.contains("3 of 200 requests left"));
        assert!(s.lines[1].text.contains("another alphai-tui pane"));
        // A quarter left is still fine.
        let s = prices_section(&price_facts(vec![probe("alpaca", Ok(268.8), Some(50))]));
        assert!(s.lines.iter().all(|l| l.level != Level::Warn));

        let s = prices_section(&price_facts(vec![probe(
            "alpaca",
            Err("alpaca rate limit hit"),
            None,
        )]));
        assert_eq!(
            s.lines[0],
            Line {
                level: Level::Fail,
                text: "alpaca   the source in use: alpaca rate limit hit".into()
            }
        );
        assert_eq!(exit_code(&[s]), 1);
        // The refusal keeps the count that explains it: one request of a
        // drained key answered with 0 left, the other was refused.
        let s = prices_section(&price_facts(vec![probe(
            "alpaca",
            Err("alpaca rate limit hit"),
            Some(0),
        )]));
        assert_eq!(
            s.lines[0].text,
            "alpaca   the source in use: alpaca rate limit hit, key budget 0 of 200 this minute"
        );
        assert_eq!(s.lines[1].level, Level::Warn);
        assert!(s.lines[1].text.contains("0 of 200 requests left"));
    }

    #[test]
    fn an_unknown_or_unkeyed_source_fails() {
        let mut f = price_facts(vec![]);
        f.active = Err("unknown data source 'yahooo' (available: yahoo)".into());
        let s = prices_section(&f);
        assert_eq!(s.lines[0].level, Level::Fail);
        assert!(s.lines.iter().all(|l| !l.text.contains("polls")));

        let mut f = price_facts(vec![]);
        f.active_unkeyed = Some("alpaca needs APCA_API_KEY_ID".into());
        f.rate_warning = Some("6 symbols every 2s is up to 360 requests a minute".into());
        f.fallback = false;
        let s = prices_section(&f);
        assert_eq!(s.lines[0].level, Level::Fail);
        assert!(
            s.lines[0]
                .text
                .starts_with("alpaca is the source in use, but")
        );
        assert_eq!(s.lines[1].level, Level::Warn);
        assert!(s.lines.last().unwrap().text.starts_with("fallback off"));
    }

    #[test]
    fn extended_hours_read_as_a_sentence() {
        assert_eq!(
            describe_extended(
                ExtendedSource::Auto,
                Provider::Sip { backup: true },
                "tiingo",
                Coverage::Venue
            ),
            "Alpaca's consolidated tape, 15 minutes behind, Yahoo as backup (extended_source auto)"
        );
        assert_eq!(
            describe_extended(
                ExtendedSource::Same,
                Provider::Own,
                "finnhub",
                Coverage::Nothing
            ),
            "none, finnhub has no pre or after hours (extended_source same)"
        );
    }

    fn answered(limit: u64, remaining: u64, reset: Option<i64>, known: bool) -> AlphaiFacts {
        AlphaiFacts::Answered {
            symbol: "AAPL".into(),
            probe: Probe {
                budget: Some(alphai::DailyBudget {
                    limit,
                    remaining,
                    reset,
                }),
                known,
            },
        }
    }

    #[test]
    fn alphai_names_the_plan_and_the_day_left() {
        let s = alphai_section(&answered(100, 87, Some(1_795_651_200), true), now());
        assert_eq!(
            texts(&s),
            [(
                Level::Ok,
                "Free plan, 87 of 100 requests left today, resets 00:00 UTC (in 16h57m)"
            )]
        );
        let s = alphai_section(&answered(100_000, 96_429, None, true), now());
        assert_eq!(
            s.lines[0].text,
            "Pro plan, 96,429 of 100,000 requests left today"
        );
        let s = alphai_section(&answered(250, 200, None, true), now());
        assert_eq!(s.lines[0].text, "200 of 250 requests left today");
    }

    #[test]
    fn alphai_warns_on_a_spent_day_and_fails_on_a_refusal() {
        let s = alphai_section(&answered(100, 9, None, true), now());
        assert_eq!(s.lines[0].level, Level::Warn);
        let s = alphai_section(&answered(100, 10, None, false), now());
        assert_eq!(s.lines[0].level, Level::Ok);
        assert_eq!(s.lines[1].level, Level::Info);
        assert!(s.lines[1].text.contains("no ticker AAPL"));
        let s = alphai_section(
            &AlphaiFacts::Answered {
                symbol: "AAPL".into(),
                probe: Probe {
                    budget: None,
                    known: true,
                },
            },
            now(),
        );
        assert_eq!(s.lines[0].level, Level::Ok);
        let s = alphai_section(&AlphaiFacts::Failed("invalid AlphAI API key".into()), now());
        assert_eq!(exit_code(&[s]), 1);
        let s = alphai_section(&AlphaiFacts::NoKey, now());
        assert_eq!(s.lines[0].level, Level::Info);
        assert!(s.lines[0].text.contains("alphai.io"));
    }

    #[test]
    fn cache_reads_its_state() {
        let path = Path::new("/c/alphai-tui/quotes.json");
        let at = now().timestamp();
        let s = cache_section(Some(path), Some(&Summary::Missing), at);
        assert_eq!(s.lines[0].level, Level::Info);
        let s = cache_section(Some(path), Some(&Summary::Unusable("format 1".into())), at);
        assert_eq!(s.lines[0].level, Level::Warn);
        let s = cache_section(
            Some(path),
            Some(&Summary::Usable {
                bytes: 300_000,
                entries: 7,
                fresh: 6,
                newest: Some(at - 120),
            }),
            at,
        );
        assert_eq!(
            texts(&s),
            [(
                Level::Ok,
                "/c/alphai-tui/quotes.json, 293 KB, 6 of 7 entries usable, newest 2m ago"
            )]
        );
        // Live 2026-11-25: a 6.3 MB file whose newest entry was ten days old.
        let s = cache_section(
            Some(path),
            Some(&Summary::Usable {
                bytes: 6_627_711,
                entries: 45,
                fresh: 0,
                newest: Some(at - 9 * 86_400 - 16 * 3600),
            }),
            at,
        );
        assert_eq!(s.lines[0].level, Level::Info);
        assert_eq!(
            s.lines[0].text,
            "/c/alphai-tui/quotes.json, 6.3 MB, 45 entries, newest 9d16h ago: none younger than a week, so the app starts without cached prices"
        );
        let s = cache_section(None, None, at);
        assert_eq!(s.lines[0].level, Level::Info);
    }

    #[test]
    fn terminal_flags_no_color_rgb_themes_and_small_windows() {
        let good = TermFacts {
            term: Some("xterm-256color".into()),
            colorterm: Some("truecolor".into()),
            term_program: Some("ghostty".into()),
            tmux: true,
            size: Some((110, 32)),
            theme: "dracula".into(),
            theme_rgb: true,
            ..Default::default()
        };
        let s = terminal_section(&good);
        assert_eq!(
            texts(&s),
            [(
                Level::Ok,
                "TERM xterm-256color, COLORTERM truecolor, ghostty, 110x32, inside tmux, theme dracula"
            )]
        );
        let bad = TermFacts {
            no_color: true,
            colorterm: None,
            size: Some((70, 20)),
            ..good.clone()
        };
        let s = terminal_section(&bad);
        let warns: Vec<_> = s.lines.iter().filter(|l| l.level == Level::Warn).collect();
        assert_eq!(warns.len(), 3, "{s:?}");
        assert!(warns[0].text.contains("NO_COLOR"));
        assert!(warns[1].text.contains("theme dracula"));
        assert!(warns[2].text.contains("70x20"));
        // The ANSI default theme needs no truecolor.
        let ansi = TermFacts {
            colorterm: None,
            theme: "default".into(),
            theme_rgb: false,
            size: None,
            ..good
        };
        let s = terminal_section(&ansi);
        assert!(s.lines.iter().all(|l| l.level != Level::Warn), "{s:?}");
        assert!(
            s.lines
                .iter()
                .any(|l| l.text.contains("no terminal attached"))
        );
    }

    #[test]
    fn market_tells_the_session_and_the_countdown() {
        // 02:02:26 ET on a Wednesday: closed, the bell in 7h27m34s, which
        // the rail's countdown rounds up.
        let s = market_section(now());
        assert_eq!(s.lines[0].text, "US market closed, opens in 7h28m");
    }

    #[test]
    fn version_compares_as_numbers() {
        let s = version_section("0.44.0", &Ok("0.45.0".into()));
        assert_eq!(s.lines[0].level, Level::Warn);
        assert!(s.lines[0].text.starts_with("0.45.0 is out, this is 0.44.0"));
        let s = version_section("0.44.0", &Ok("0.44.0".into()));
        assert_eq!(s.lines[0].text, "0.44.0, the latest release");
        // 0.10 is newer than 0.9: no string comparison.
        let s = version_section("0.10.0", &Ok("0.9.3".into()));
        assert_eq!(s.lines[0].level, Level::Ok);
        assert!(s.lines[0].text.contains("newer than"));
        let s = version_section("0.44.0", &Err("timed out".into()));
        assert_eq!(s.lines[0].level, Level::Info);
        assert_eq!(version("1.2.3-beta.1"), Some((1, 2, 3)));
        assert_eq!(version("1.2"), None);
    }

    #[test]
    fn render_aligns_levels_and_counts_failures() {
        let mut s = Section::new("Keys");
        s.push(Level::Ok, "a");
        s.push(Level::Off, "b");
        assert_eq!(render_section(&s), "Keys\n  ok    a\n  --    b\n");
        assert_eq!(exit_code(&[s.clone()]), 0);
        s.push(Level::Fail, "c");
        assert_eq!(exit_code(&[s]), 1);
    }

    #[test]
    fn spans_and_groups_read_naturally() {
        assert_eq!(span(45), "45s");
        assert_eq!(span(720), "12m");
        assert_eq!(span(3 * 3600 + 300), "3h05m");
        assert_eq!(span(2 * 86_400 + 4 * 3600), "2d4h");
        assert_eq!(group(96_429), "96,429");
        assert_eq!(group(100), "100");
        assert_eq!(group(1_000_000), "1,000,000");
    }
}
