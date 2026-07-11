mod alphai;
mod app;
mod config;
mod domain;
mod indicators;
mod poller;
mod source;
mod ui;

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

    /// Print quotes once to stdout and exit (no TUI); handy for scripts
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (cfg, cfg_existed) = config::load();

    let symbols: Vec<String> = if !args.symbols.is_empty() {
        args.symbols.iter().map(|s| s.to_uppercase()).collect()
    } else if !cfg.watchlist.is_empty() {
        cfg.watchlist.iter().map(|s| s.to_uppercase()).collect()
    } else {
        config::DEFAULT_WATCHLIST.iter().map(|s| s.to_string()).collect()
    };

    let source_name = args
        .source
        .clone()
        .or_else(|| cfg.source.clone())
        .unwrap_or_else(|| source::registry::SOURCES[0].id.to_string());
    let source = source::make_source(&source_name, &cfg)?;
    let every = args.every.or(cfg.every).unwrap_or(15);
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

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let refresh = Arc::new(tokio::sync::Notify::new());
    let shared: poller::SharedSource = Arc::new(RwLock::new(source.clone()));
    let params: poller::SharedParams = Arc::new(RwLock::new((range, interval)));
    rt.spawn(poller::run(
        shared.clone(),
        symbols.clone(),
        params.clone(),
        Duration::from_secs(every.max(2)),
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
        rx,
        refresh,
        alphai_tx,
        config: cfg,
        alphai_enabled: alphai_key.is_some(),
        first_run: !cfg_existed,
    });
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
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
