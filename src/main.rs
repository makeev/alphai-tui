mod alphai;
mod app;
mod config;
mod domain;
mod indicators;
mod keymap;
mod market;
mod poller;
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
use crate::domain::{Interval, Range, fmt_price};

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

    /// Print quotes once to stdout and exit (no TUI); handy for scripts
    #[arg(long)]
    once: bool,

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
    if let Some(info) = source::registry::find(&source_name)
        && let Some(msg) = source::registry::rate_warning(info, symbols.len(), every.max(2))
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
    if args.once {
        return print_once(&rt, source, &symbols, range, interval);
    }
    if let Some(ticker) = args.earnings.as_deref() {
        return print_earnings(&rt, cfg.alphai_key(), ticker);
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let refresh = Arc::new(tokio::sync::Notify::new());
    let shared: poller::SharedSource = Arc::new(RwLock::new(source.clone()));
    let params: poller::SharedParams = Arc::new(RwLock::new((range, interval)));
    let shared_every: poller::SharedEvery =
        Arc::new(RwLock::new(Duration::from_secs(every.max(2))));
    rt.spawn(poller::run(
        shared.clone(),
        symbols.clone(),
        params.clone(),
        shared_every.clone(),
        resolved.chart.sma_slow,
        tx.clone(),
        refresh.clone(),
    ));

    let alphai_key = cfg.alphai_key();
    let (alphai_tx, alphai_rx) = tokio::sync::mpsc::unbounded_channel();
    rt.spawn(alphai::run(alphai_key.clone(), alphai_rx, tx));

    let mut terminal = ratatui::init();
    let mut app = App::new(AppInit {
        symbols,
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
        ui: resolved.ui,
        keymap: resolved.keymap,
        alphai_enabled: alphai_key.is_some(),
        first_run: !cfg_existed,
    });
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

fn print_once(
    rt: &tokio::runtime::Runtime,
    source: Arc<dyn source::DataSource>,
    symbols: &[String],
    range: Range,
    interval: Interval,
) -> Result<()> {
    for symbol in symbols {
        match rt.block_on(source.fetch(symbol, range, interval)) {
            Ok(data) => {
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
            Err(e) => println!("{symbol:<8} error: {e:#}"),
        }
    }
    Ok(())
}
