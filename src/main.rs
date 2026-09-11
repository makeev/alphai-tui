mod alphai;
mod app;
mod cache;
mod config;
mod domain;
mod indicators;
mod keymap;
mod market;
mod poller;
mod portfolio;
mod source;
mod theme;
mod ui;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use clap::ValueEnum;

use crate::app::{App, AppInit};
use crate::domain::{Interval, Range, Sessions, fetch_range, fmt_price};

#[derive(Parser, Debug)]
#[command(
    name = "alphai-tui",
    version,
    about = "Terminal stock dashboard: quotes, charts, AI-scored news and insider activity",
    after_help = "Every option falls back to the config file written by the in-app settings \
                  screen (press s), then to built-in defaults. Bare `alphai-tui` runs with the \
                  saved watchlist."
)]
struct Args {
    /// Ticker symbols to watch, e.g. AAPL MSFT NVDA BTC-USD (default: saved watchlist)
    symbols: Vec<String>,

    #[arg(short, long, help = source::registry::cli_source_help())]
    source: Option<String>,

    /// Poll interval in seconds [default: 15]
    #[arg(short, long)]
    every: Option<u64>,

    /// History window for charts [default: 1d]
    #[arg(short, long, value_enum)]
    range: Option<Range>,

    /// Candle granularity for charts [default: 5m]
    #[arg(short, long, value_enum)]
    interval: Option<Interval>,

    #[arg(long, value_name = "NAME", help = theme::cli_theme_help())]
    theme: Option<String>,

    /// Start without the header and footer, for a tmux pane that carries
    /// its own status bar (z toggles it live)
    #[arg(long)]
    bare: bool,

    /// Print quotes once to stdout and exit (no TUI); handy for scripts
    #[arg(long)]
    once: bool,

    /// Print those quotes as JSON instead of a text table, for status bars
    /// and scripts (implies --once)
    #[arg(long, conflicts_with = "earnings")]
    json: bool,

    /// Print the latest AlphaAI earnings read for one ticker and exit
    /// (no TUI); needs an API key. One request.
    #[arg(long, value_name = "TICKER")]
    earnings: Option<String>,

    /// Use an alternate config file (the settings screen saves back to it)
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (cfg, cfg_existed, config_path) = config::load_at(args.config.as_deref());
    // Validation warnings go to stderr before the TUI takes the terminal,
    // so they stay readable in scrollback after exit.
    let (resolved, warnings) = config::resolve(&cfg, args.theme.as_deref());
    for w in &warnings {
        eprintln!("warning: {w}");
    }

    let symbols: Vec<String> = if !args.symbols.is_empty() {
        args.symbols.iter().map(|s| s.to_uppercase()).collect()
    } else if !cfg.watchlist.is_empty() {
        cfg.watchlist.iter().map(|s| s.to_uppercase()).collect()
    } else {
        config::DEFAULT_WATCHLIST
            .iter()
            .map(|s| s.to_string())
            .collect()
    };

    let source_name = args
        .source
        .clone()
        .or_else(|| cfg.source.clone())
        .unwrap_or_else(|| source::registry::SOURCES[0].id.to_string());
    let source = source::make_source(&source_name, &cfg)?;
    let every = args.every.or(cfg.every).unwrap_or(15);
    // A poll cycle spends `reqs_per_symbol` per ticker: a watchlist long
    // enough to outrun the plan's ceiling turns tickers into error rows,
    // which reads as a broken key rather than as a budget.
    // Holdings off the watchlist are polled too, so they count here: a
    // budget warning that ignored them would understate the real load.
    let polled = portfolio::polled_symbols(&symbols, &resolved.positions);
    if let Some(info) = source::registry::find(&source_name)
        && let Some(msg) = source::registry::rate_warning(info, polled.len(), every.max(2))
    {
        eprintln!("warning: {msg}");
    }
    let range = args
        .range
        .or_else(|| parse_enum::<Range>(cfg.range.as_deref()))
        .unwrap_or(Range::D1);
    let interval = args
        .interval
        .or_else(|| parse_enum::<Interval>(cfg.interval.as_deref()))
        .unwrap_or(Interval::M5);

    let rt = tokio::runtime::Runtime::new()?;
    if args.once || args.json {
        return print_once(
            &rt,
            source,
            &symbols,
            &resolved.positions,
            range,
            interval,
            args.json.then_some(source_name.as_str()),
        );
    }
    if let Some(ticker) = args.earnings.as_deref() {
        return print_earnings(&rt, cfg.alphai_key(), ticker);
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let refresh = Arc::new(tokio::sync::Notify::new());
    let shared: poller::SharedSource = Arc::new(RwLock::new(source.clone()));
    let sessions = if resolved.chart.extended_hours {
        Sessions::Extended
    } else {
        Sessions::Regular
    };
    let params: poller::SharedParams = Arc::new(RwLock::new((range, interval, sessions)));
    let shared_every: poller::SharedEvery =
        Arc::new(RwLock::new(Duration::from_secs(every.max(2))));
    // The watchlist is shared rather than moved: the add and remove keys
    // edit it live and the poller picks the change up on its next tick.
    let shared_symbols: poller::SharedSymbols = Arc::new(RwLock::new(polled.clone()));
    // Last-good prices from the previous run: the screen starts with
    // numbers on it instead of "loading…", and a source that is blocked
    // right now has something to be blocked in front of.
    let cached = cache::Store::load();
    let window = cache::params_key(
        fetch_range(range, interval, resolved.chart.sma_slow),
        interval,
        sessions,
    );
    let seeds: Vec<(String, cache::Entry)> = polled
        .iter()
        .filter_map(|symbol| {
            cached
                .get(source.name(), symbol, &window)
                .map(|entry| (symbol.clone(), entry.clone()))
        })
        .collect();
    rt.spawn(poller::run(poller::Poller {
        source: shared.clone(),
        symbols: shared_symbols.clone(),
        params: params.clone(),
        every: shared_every.clone(),
        slow_bars: resolved.chart.sma_slow,
        tx: tx.clone(),
        refresh: refresh.clone(),
        cache: cached,
    }));

    let alphai_key = cfg.alphai_key();
    let (alphai_tx, alphai_rx) = tokio::sync::mpsc::unbounded_channel();
    rt.spawn(alphai::run(alphai_key.clone(), alphai_rx, tx));

    // CLI over config, like every other startup value; the z key moves it
    // either way afterwards.
    let mut ui = resolved.ui;
    ui.bare |= args.bare;

    // Read before the config is handed to the app, which takes it whole.
    let source_fallback = cfg.source_fallback.unwrap_or(true);

    let mut terminal = ratatui::init();
    let mut app = App::new(AppInit {
        symbols,
        positions: resolved.positions,
        shared_symbols,
        sessions,
        source: shared,
        source_name: source.name(),
        range,
        interval,
        params,
        every: shared_every,
        rx,
        refresh,
        alphai_tx,
        config: cfg,
        config_path,
        theme: resolved.theme,
        theme_name: resolved.theme_name,
        chart: resolved.chart,
        ui,
        keymap: resolved.keymap,
        alphai_enabled: alphai_key.is_some(),
        first_run: !cfg_existed,
        source_fallback,
    });
    for (symbol, entry) in seeds {
        app.seed_cached(symbol, entry);
    }
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

/// `--earnings TICKER`: the newest earnings read in plain text, for a pipe
/// or a tmux pane. One request, and the figures print exactly as the filing
/// wrote them.
fn print_earnings(rt: &tokio::runtime::Runtime, key: Option<String>, ticker: &str) -> Result<()> {
    let Some(key) = key else {
        println!(
            "no AlphaAI API key. Get a free one at https://alphai.io (Account -> API keys),\n             then set ALPHAI_API_KEY or press s in the app to save it."
        );
        return Ok(());
    };
    let ticker = ticker.to_uppercase();
    let client = alphai::Client::new(key)?;
    let data = match rt.block_on(client.earnings(&ticker)) {
        Ok(data) => data,
        // Not an error: the API owns no listing for this string.
        Err(e) if alphai::is_unknown_symbol(&format!("{e:#}")) => {
            println!("no earnings coverage for {ticker}: reads come from SEC filings.");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let next = data
        .next_report_date
        .as_deref()
        .map_or_else(|| "not confirmed yet".to_string(), str::to_string);
    let Some(read) = data.reports.first() else {
        println!(
            "no earnings read for {} yet · next report {next}",
            data.ticker
        );
        return Ok(());
    };
    let r = read.report();

    println!("{}", read.title);
    let filed = read
        .filed()
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default();
    println!(
        "{} filed {filed} · {} · period end {}",
        read.form(),
        r.fiscal_period,
        r.period_end.as_deref().unwrap_or("not stated")
    );
    println!("verdict  {} · {}", r.verdict, r.verdict_reason);
    println!("headline {}", r.headline);
    println!(
        "numbers verified against the filing: {}",
        if r.numbers_verified_from_document {
            "yes"
        } else {
            "no"
        }
    );
    if let Some(url) = read.alphai_url() {
        println!("url      {url}");
    }
    println!("uid      {}", read.uid);

    if !r.key_metrics.is_empty() {
        println!("\nkey metrics (value | prior Q | prior Y | q/q | y/y):");
        // Figures print exactly as the filing wrote them, so the columns are
        // sized to the data rather than the data trimmed to the columns.
        let cell = |v: &Option<String>| v.clone().unwrap_or_default();
        let rows: Vec<[String; 7]> = r
            .key_metrics
            .iter()
            .map(|m| {
                [
                    m.name.clone(),
                    m.basis.clone(),
                    m.value.clone(),
                    cell(&m.prior_quarter),
                    cell(&m.prior_year),
                    cell(&m.qoq_change),
                    cell(&m.yoy_change),
                ]
            })
            .collect();
        let mut w = [0usize; 7];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                w[i] = w[i].max(cell.chars().count());
            }
        }
        for row in &rows {
            let line = format!(
                "  {:<n$} {:<b$}  {:>v$} | {:>pq$} | {:>py$} | {:>q$} | {:>y$}",
                row[0],
                row[1],
                row[2],
                row[3],
                row[4],
                row[5],
                row[6],
                n = w[0],
                b = w[1],
                v = w[2],
                pq = w[3],
                py = w[4],
                q = w[5],
                y = w[6],
            );
            println!("{}", line.trim_end());
        }
    }
    if !r.segments.is_empty() {
        println!("\nsegments:");
        for s in &r.segments {
            println!(
                "  {:<28} {:>18} | {:>9} q/q | {:>9} y/y",
                s.name,
                s.revenue,
                s.qoq_change.clone().unwrap_or_default(),
                s.yoy_change.clone().unwrap_or_default(),
            );
            if !s.driver.is_empty() {
                println!("    {}", s.driver);
            }
        }
    }
    if let Some(g) = &r.guidance {
        println!("\nguidance ({}):", g.period);
        for (label, value) in [
            ("revenue", &g.revenue),
            ("gross margin", &g.gross_margin),
            ("operating expenses", &g.operating_expenses),
            ("tax rate", &g.tax_rate),
        ] {
            if let Some(v) = value {
                println!("  {label:<20} {v}");
            }
        }
        for other in &g.other {
            println!("  {:<20} {other}", "other");
        }
    }
    if !r.vs_prior_guidance.is_empty() {
        println!("\nversus prior guidance:");
        for c in &r.vs_prior_guidance {
            println!(
                "  {:<28} {} against {} ({})",
                c.metric, c.actual, c.prior_guidance, c.verdict
            );
        }
    }
    for (title, items) in [
        ("drivers", &r.drivers),
        ("concerns", &r.concerns),
        ("what to watch", &r.what_to_watch),
        ("capital returns", &r.capital_returns),
        ("balance sheet and cash flow", &r.balance_sheet_cash_flow),
        ("not in the filing", &r.missing_items),
    ] {
        if items.is_empty() {
            continue;
        }
        println!("\n{title}:");
        for item in items {
            println!("  - {item}");
        }
    }
    for q in &r.quotes {
        println!(
            "\n{}{}:",
            q.speaker,
            q.role
                .as_deref()
                .map(|role| format!(", {role}"))
                .unwrap_or_default()
        );
        println!("  {}", q.text);
    }
    if !r.narrative().is_empty() {
        println!("\nanalysis:");
        for para in r.narrative().split("\n\n") {
            if !para.trim().is_empty() {
                println!("  {}", para.trim());
                println!();
            }
        }
    }
    println!("next report: {next}");
    if data.reports.len() > 1 {
        println!(
            "{} older reads available in the app",
            data.reports.len() - 1
        );
    }
    Ok(())
}

fn parse_enum<T: ValueEnum>(value: Option<&str>) -> Option<T> {
    T::from_str(value?, true).ok()
}

/// `--once`, and with `json` the same run as a JSON array on stdout (any
/// warning has already gone to stderr, so the document stays machine
/// readable). One row per symbol, in the order they were asked for.
fn print_once(
    rt: &tokio::runtime::Runtime,
    source: Arc<dyn source::DataSource>,
    symbols: &[String],
    positions: &[portfolio::Position],
    range: Range,
    interval: Interval,
    json: Option<&str>,
) -> Result<()> {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for symbol in symbols {
        // Regular sessions only: this prints a quote and a candle count
        // for a script, and the extended candles change neither.
        let fetched = rt.block_on(source.fetch(symbol, range, interval, Sessions::Regular));
        match (fetched, json) {
            (Ok(data), None) => {
                let q = &data.quote;
                let change = match (q.change(), q.change_pct()) {
                    (Some(c), Some(p)) => format!("{c:+.2} ({p:+.2}%)"),
                    _ => "—".into(),
                };
                println!(
                    "{:<8} {:>10} {} {}  [{} candles]",
                    q.symbol,
                    fmt_price(q.price),
                    q.currency.as_deref().unwrap_or(""),
                    change,
                    data.candles.len()
                );
            }
            (Ok(data), Some(source_name)) => {
                let held = positions.iter().find(|p| p.symbol == data.quote.symbol);
                rows.push(quote_json(&data, source_name, held));
            }
            (Err(e), None) => println!("{symbol:<8} error: {e:#}"),
            // A failed symbol is a row of its own rather than a missing
            // one: a script watching four tickers still gets four rows and
            // can say which of them is the broken one.
            (Err(e), Some(_)) => rows.push(serde_json::json!({
                "symbol": symbol,
                "error": format!("{e:#}"),
            })),
        }
    }
    if json.is_some() {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    }
    Ok(())
}

/// One ticker as JSON. Absent figures are left out rather than sent as
/// null: sources differ in what they can answer, and a key that is there
/// only sometimes is easier to test for than a value that is null only
/// sometimes. `symbol` and `price` are the two that are always present.
fn quote_json(
    data: &domain::TickerData,
    source_name: &str,
    position: Option<&portfolio::Position>,
) -> serde_json::Value {
    quote_json_at(data, source_name, position, chrono::Utc::now())
}

fn quote_json_at(
    data: &domain::TickerData,
    source_name: &str,
    position: Option<&portfolio::Position>,
    now: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    use serde_json::{Value, json};

    let q = &data.quote;
    let mut out = json!({
        "symbol": q.symbol,
        "price": q.price,
        "candles": data.candles.len(),
        "source": source_name,
        // Whole seconds and a Z, the shape a log line or a cache key wants.
        "fetched": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    let map = out.as_object_mut().expect("object");
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(v) = value {
            map.insert(key.to_string(), v);
        }
    };
    let range = |r: Option<(f64, f64)>| r.map(|(low, high)| json!({ "low": low, "high": high }));
    put("currency", q.currency.clone().map(Value::from));
    put("prev_close", q.prev_close.map(Value::from));
    put("change", q.change().map(|c| json!(trimmed(c, 6))));
    put("change_pct", q.change_pct().map(|p| json!(trimmed(p, 4))));
    put("day_range", range(q.day_range));
    put("fifty_two_week", range(q.fifty_two_week));
    put("volume", q.volume.map(Value::from));
    // The extended print is its own object: its move is measured from the
    // regular close, not from the previous one, so putting the two moves
    // side by side under one set of keys would invite reading them alike.
    if let Some(price) = q.extended_price() {
        let mut ext = json!({ "price": price });
        let ext_map = ext.as_object_mut().expect("object");
        if let Some(c) = q.extended_change() {
            ext_map.insert("change".into(), json!(trimmed(c, 6)));
        }
        if let Some(p) = q.extended_change_pct() {
            ext_map.insert("change_pct".into(), json!(trimmed(p, 4)));
        }
        map.insert("extended".into(), ext);
    }
    // Only for a ticker the config holds: a status bar that asks for a
    // price gets a price, and one that tracks a holding gets the money.
    if let Some(held) = position {
        let price = portfolio::price(q);
        let mut pos = json!({
            "qty": held.qty,
            "avg_price": held.avg_price,
            "cost": trimmed(held.cost(), 6),
            "price": price,
            "value": trimmed(held.value(price), 6),
            "pnl": trimmed(held.pnl(price), 6),
        });
        let pos_map = pos.as_object_mut().expect("object");
        if let Some(pct) = held.pnl_pct(price) {
            pos_map.insert("pnl_pct".into(), json!(trimmed(pct, 4)));
        }
        if let Some(day) = held.day_pnl(q, now) {
            pos_map.insert("day_pnl".into(), json!(trimmed(day, 6)));
        }
        map.insert("position".into(), pos);
    }
    out
}

/// A derived figure with its float noise trimmed: a change of
/// 11.230000000000018 is arithmetic showing through, not data. Prices and
/// volumes come from the source untouched.
fn trimmed(v: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (v * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Candle, Quote, TickerData};

    fn data(quote: Quote) -> TickerData {
        TickerData {
            quote,
            candles: vec![Candle {
                ts: 1_700_000_000,
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: 1.5,
                volume: None,
            }],
        }
    }

    fn plain(price: f64, prev_close: Option<f64>) -> Quote {
        Quote {
            symbol: "AAPL".into(),
            price,
            prev_close,
            currency: Some("USD".into()),
            extended: None,
            fifty_two_week: None,
            day_range: None,
            volume: None,
        }
    }

    /// A held ticker carries its money too, with the same float-noise
    /// trimming the quote's own derived figures get.
    #[test]
    fn json_carries_the_position_when_the_ticker_is_held() {
        let held = portfolio::Position {
            symbol: "AAPL".into(),
            qty: 12.0,
            avg_price: 182.31,
        };
        let json = quote_json(&data(plain(326.57, Some(315.34))), "yahoo", Some(&held));
        let pos = &json["position"];
        assert_eq!(pos["qty"], 12.0);
        assert_eq!(pos["avg_price"], 182.31);
        assert_eq!(pos["cost"], 2187.72);
        assert_eq!(pos["value"], 3918.84);
        assert_eq!(pos["pnl"], 1731.12);
        assert_eq!(pos["pnl_pct"], 79.129);
        assert_eq!(pos["day_pnl"], 134.76);
    }

    #[test]
    fn json_values_a_position_at_premarket_with_the_latest_close_as_reference() {
        let held = portfolio::Position {
            symbol: "CRWV".into(),
            qty: 300.0,
            avg_price: 90.26,
        };
        let mut quote = plain(89.12, Some(94.94));
        quote.symbol = held.symbol.clone();
        quote.extended = Some(91.28);
        let now = "2026-09-11T10:30:00Z".parse().unwrap();
        let json = quote_json_at(&data(quote), "yahoo", Some(&held), now);
        assert_eq!(json["price"], 89.12);
        assert_eq!(json["extended"]["price"], 91.28);
        assert_eq!(json["position"]["price"], 91.28);
        assert_eq!(json["position"]["value"], 27384.0);
        assert_eq!(json["position"]["pnl"], 306.0);
        assert_eq!(json["position"]["pnl_pct"], 1.1301);
        assert_eq!(json["position"]["day_pnl"], 648.0);
    }

    /// No previous close, no day figure: the key is left out rather than
    /// sent as null, like every other absent figure here.
    #[test]
    fn a_position_without_a_previous_close_has_no_day_figure() {
        let held = portfolio::Position {
            symbol: "AAPL".into(),
            qty: 12.0,
            avg_price: 182.31,
        };
        let json = quote_json(&data(plain(326.57, None)), "yahoo", Some(&held));
        assert!(json["position"]["day_pnl"].is_null());
        assert_eq!(json["position"]["pnl"], 1731.12);
    }

    /// A ticker that is only watched says nothing about money.
    #[test]
    fn json_has_no_position_key_for_an_unheld_ticker() {
        let json = quote_json(&data(plain(326.57, Some(315.34))), "yahoo", None);
        assert!(json["position"].is_null());
    }

    /// The keys a script can count on, and the derived figures without the
    /// float noise the subtraction leaves behind.
    #[test]
    fn json_carries_the_quote_and_its_derived_moves() {
        let json = quote_json(&data(plain(326.57, Some(315.34))), "yahoo", None);
        assert_eq!(json["symbol"], "AAPL");
        assert_eq!(json["price"], 326.57);
        assert_eq!(json["currency"], "USD");
        assert_eq!(json["prev_close"], 315.34);
        assert_eq!(json["source"], "yahoo");
        assert_eq!(json["candles"], 1);
        // 326.57 - 315.34 is 11.230000000000018 in binary floating point.
        assert_eq!(json["change"], 11.23);
        assert_eq!(json["change_pct"], 3.5612);
        assert!(json["fetched"].is_string());
    }

    /// Sources answer different subsets, so what is missing is missing
    /// rather than null: `has("extended")` beats a null check.
    #[test]
    fn json_leaves_out_what_the_source_did_not_answer() {
        let json = quote_json(&data(plain(100.0, None)), "finnhub", None);
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("change"), "{json}");
        assert!(!obj.contains_key("prev_close"), "{json}");
        assert!(!obj.contains_key("extended"), "{json}");
        assert!(!obj.contains_key("volume"), "{json}");
        assert!(!obj.contains_key("day_range"), "{json}");
    }

    /// The extended print keeps its own object: its move is measured from
    /// the regular close, the headline one from the previous close.
    #[test]
    fn json_keeps_the_extended_print_apart() {
        let mut quote = plain(315.34, Some(310.0));
        quote.extended = Some(317.22);
        quote.day_range = Some((310.5, 316.0));
        quote.fifty_two_week = Some((164.08, 340.0));
        quote.volume = Some(64_900_000.0);
        let json = quote_json(&data(quote), "yahoo", None);
        assert_eq!(json["extended"]["price"], 317.22);
        assert_eq!(json["extended"]["change"], 1.88);
        assert_eq!(json["extended"]["change_pct"], 0.5962);
        assert_eq!(json["day_range"]["high"], 316.0);
        assert_eq!(json["fifty_two_week"]["low"], 164.08);
        assert_eq!(json["volume"], 64_900_000.0);
        // The headline move still counts from the previous close.
        assert_eq!(json["change"], 5.34);
    }

    /// A quote whose extended print is the regular one (the market is open,
    /// or the source has no extended data) carries no extended object.
    #[test]
    fn json_omits_an_extended_print_that_is_the_regular_one() {
        let mut quote = plain(315.34, Some(310.0));
        quote.extended = Some(315.34);
        let json = quote_json(&data(quote), "alpaca", None);
        assert!(
            !json.as_object().unwrap().contains_key("extended"),
            "{json}"
        );
    }
}
