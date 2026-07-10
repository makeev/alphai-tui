# tr-monitor

Terminal TUI for watching ticker prices. Pluggable data sources, pluggable
views, built with [ratatui](https://ratatui.rs).

## Usage

```sh
cargo run --release -- AAPL MSFT NVDA BTC-USD
```

Options:

| Flag | Default | Meaning |
|------|---------|---------|
| `-s, --source` | `yahoo` | Data source: `yahoo`, `finnhub` (`alpaca` planned) |
| `-e, --every` | `15` | Poll interval, seconds |
| `-r, --range` | `1d` | History window: `1d 5d 1mo 3mo 6mo 1y` |
| `-i, --interval` | `5m` | Candle size: `1m 2m 5m 15m 30m 60m 1d` |
| `--once` | | Print quotes to stdout and exit (for scripts) |

Keys: `Tab`/`1`–`9` switch view · `↑↓`/`jk` select ticker · `r` refresh · `q` quit.

Views: **Table** (quotes + sparklines), **Chart** (line chart of the selected
ticker with previous-close reference line), **Split** (table + chart side by
side).

## Sources

- **yahoo** — no API key, quote + candle history in one request, ~15 min
  delayed. Crypto/FX work as `BTC-USD`, `EURUSD=X`.
- **finnhub** — needs `FINNHUB_API_KEY` env var (free at finnhub.io).
  Real-time-ish quotes; historical candles are premium-only, so charts are
  built from quotes accumulated during the session (reset on restart).
  Free tier is 60 req/min: keep `tickers × (60 / --every)` under 60.
  Crypto needs exchange-prefixed symbols (`BINANCE:BTCUSDT`).

## Architecture

```
src/
  domain.rs      Quote, Candle, TickerData, Range/Interval
  source/        DataSource trait + implementations
    yahoo.rs     Yahoo v8 chart endpoint (quote + history in one call)
  poller.rs      fetches all symbols concurrently on a timer -> mpsc channel
  app.rs         App state + key handling + event loop
  ui/            View trait + implementations
    table.rs     watchlist table
    chart.rs     line chart
    split.rs     table + chart
```

### Adding a data source

Implement `source::DataSource` (one async `fetch` returning quote + candles)
in a new module and register it in `source::make_source`. Streaming sources
(IBKR) will additionally push events into the same channel the poller uses.

### Adding a view

Implement `ui::View` (a stateless `render` over `&mut App`) as a unit struct
and add it to `ui::VIEWS`. Order in that array defines the `1`–`9` hotkeys.

## Roadmap

See `PLAN.md` for the phased roadmap: Alpaca source (real-time IEX
websocket), streaming, candlestick view, config file.
