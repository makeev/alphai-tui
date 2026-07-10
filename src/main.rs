mod app;
mod domain;
mod poller;
mod source;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;

use crate::app::App;
use crate::domain::{Interval, Range, fmt_price};

#[derive(Parser, Debug)]
#[command(name = "tr-monitor", about = "Terminal ticker price monitor")]
struct Args {
    /// Ticker symbols to watch, e.g. AAPL MSFT NVDA
    #[arg(required = true)]
    symbols: Vec<String>,

    /// Data source (yahoo; more to come: ibkr, alphavantage)
    #[arg(short, long, default_value = "yahoo")]
    source: String,

    /// Poll interval in seconds
    #[arg(short, long, default_value_t = 15)]
    every: u64,

    /// History window for charts
    #[arg(short, long, value_enum, default_value_t = Range::D1)]
    range: Range,

    /// Candle granularity for charts
    #[arg(short, long, value_enum, default_value_t = Interval::M5)]
    interval: Interval,

    /// Print quotes once to stdout and exit (no TUI); handy for scripts
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let symbols: Vec<String> = args.symbols.iter().map(|s| s.to_uppercase()).collect();
    let source = crate::source::make_source(&args.source)?;
    let rt = tokio::runtime::Runtime::new()?;

    if args.once {
        return print_once(&rt, source, &symbols, args.range, args.interval);
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let refresh = Arc::new(tokio::sync::Notify::new());
    rt.spawn(poller::run(
        source.clone(),
        symbols.clone(),
        args.range,
        args.interval,
        Duration::from_secs(args.every.max(2)),
        tx,
        refresh.clone(),
    ));

    let mut terminal = ratatui::init();
    let mut app = App::new(symbols, source.name(), args.range, args.interval, rx, refresh);
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
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
