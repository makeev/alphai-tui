# alphai-tui

Terminal dashboard for watching stocks: live quotes and charts next to
AI-scored financial news, sentiment and SEC Form 4 insider activity.
Built in Rust with [ratatui](https://ratatui.rs).

```text
 alphai-tui · yahoo · 1d/5m  1:Table 2:Chart 3:Split 4:News 5:Insider · alphai ✓ · upd 18:40:12
 7d sentiment  ▲ 12 bullish · 5 neutral · ▼ 3 bearish  (20 scored)
┌ News · NVDA ──────────────────────────────────────────────────────────────────┐
│▶ 2h   9  ▲  earnings  NVIDIA beats on Q2 data-center revenue                  │
│  5h   7  ▲  tech      Hyperscaler capex guidance lifts AI supply chain        │
│  1d   6  ·  movers    Chip names drift lower into the print                   │
└───────────────────────────────────────────────────────────────────────────────┘
┌ article · ⏎ open in browser ──────────────────────────────────────────────────┐
│ NVIDIA beats on Q2 data-center revenue                                        │
│ example.com · 2h ago · earnings · score 9 · NVDA                              │
│ Data-center revenue grew again as cloud buildouts continue...                 │
└───────────────────────────────────────────────────────────────────────────────┘
 q quit · tab/1-9 view · ↑↓ article · ←→ ticker · ⏎ open · f scope · r refresh · s settings
```

## What you get

- **Table**: watchlist with price, change, day range and unicode sparklines.
- **Chart**: Braille line chart of the selected ticker with a previous-close
  reference line. **Split** shows both side by side.
- **News**: enriched articles for the selected ticker (or the whole market,
  toggle with `f`), each with a 1 to 10 relevance score, category and a
  per-ticker AI sentiment call, plus a 7-day bullish/bearish rollup.
  Enter opens the article in your browser.
- **Insider**: SEC Form 4 activity for the selected ticker. A 30-day rollup
  (buys vs sells, dollar volumes, share of pre-arranged 10b5-1 plan trades,
  most active insiders) above the stream of filing events.
- In-app settings (`s`): pick the price source and paste API keys once.
  Everything is saved to a config file, so after the first run a bare
  `alphai-tui` is enough.

Prices work with no key at all (Yahoo). News, sentiment and insider views
use the [AlphaAI](https://alphai.io) API and need a free key.

## Install

With a Rust toolchain (1.85+):

```sh
cargo install --git https://github.com/makeev/alphai-stock-tui-platform
```

Or from a clone:

```sh
git clone https://github.com/makeev/alphai-stock-tui-platform
cd alphai-stock-tui-platform
cargo run --release -- AAPL MSFT NVDA BTC-USD
```

## Quick start

```sh
alphai-tui AAPL MSFT NVDA BTC-USD
```

The first run opens the settings screen: pick a price source and paste your
AlphaAI key (get one free at [alphai.io](https://alphai.io), Account >
API keys). Leave it empty if you only want quotes and charts. Your watchlist
and options persist in the config file, so next time plain `alphai-tui` works.

```sh
alphai-tui --once AAPL      # print quotes to stdout and exit (for scripts)
alphai-tui -s finnhub NVDA  # explicit source for one run
```

## Options

| Flag | Default | Meaning |
|------|---------|---------|
| `-s, --source` | `yahoo` | Price source: `yahoo` or `finnhub` |
| `-e, --every` | `15` | Poll interval, seconds |
| `-r, --range` | `1d` | History window: `1d 5d 1mo 3mo 6mo 1y` |
| `-i, --interval` | `5m` | Candle size: `1m 2m 5m 15m 30m 60m 1d` |
| `--once` | | Print quotes to stdout and exit |

CLI arguments win over the config file; the config file wins over built-in
defaults. API keys can also come from env vars, which win over the config:
`ALPHAI_API_KEY`, `FINNHUB_API_KEY`.

## Keys

| Key | Where | Action |
|-----|-------|--------|
| `Tab` / `1`..`5` | everywhere | switch view |
| `↑` `↓` / `j` `k` | table, chart, split | select ticker |
| `↑` `↓` / `j` `k` | news, insider | scroll articles |
| `←` `→` / `h` `l` | news, insider | switch ticker |
| `Enter` / `o` | news, insider | open article in browser |
| `f` | news | toggle selected ticker / whole market |
| `r` | everywhere | refresh prices and the visible news view |
| `s` | everywhere | settings |
| `q` / `Esc` / `Ctrl-C` | everywhere | quit |

## Data sources

**Prices**

- `yahoo`: no API key, quote and candle history in one request, roughly
  15 minutes delayed. Crypto and FX tickers work as `BTC-USD`, `EURUSD=X`.
- `finnhub`: needs a key (free at [finnhub.io](https://finnhub.io)).
  Real-time-ish quotes; historical candles are premium-only there, so charts
  build up from quotes collected during the session and reset on restart.
  Free tier is 60 req/min: keep `tickers x (60 / --every)` under 60.
  Crypto needs exchange-prefixed symbols (`BINANCE:BTCUSDT`).

**News, sentiment, insider**

- [AlphaAI](https://alphai.io): AI-enriched financial news feed. Every
  article carries validated tickers, a category, a deterministic 1 to 10
  relevance score and per-ticker sentiment; insider rows are generated
  from SEC EDGAR Form 4 filings, one row per economic event. The free tier
  (no card) allows 20 requests/min and 100/day. The app is careful with
  that budget: it fetches only what the visible view needs and caches each
  response for 5 minutes. Full API reference:
  [alphai.io/developers](https://alphai.io/developers).

Ticker forms follow the US/Yahoo convention (`AAPL`, `BTC-USD`, `VOD.L`),
which is also what AlphaAI uses. Finnhub-specific symbols like
`BINANCE:BTCUSDT` will not have news attached.

## Config file

`~/.config/alphai-tui/config.toml` on Linux and macOS (`%APPDATA%` on
Windows), created by the settings screen with mode 0600 since it can hold
keys. Saving the settings also persists the watchlist on screen:

```toml
source = "yahoo"
watchlist = ["AAPL", "MSFT", "NVDA", "BTC-USD"]
every = 15
range = "1d"
interval = "5m"

[keys]
alphai = "ak_live_..."
finnhub = ""
```

## Architecture

```
src/
  domain.rs      Quote, Candle, TickerData, Range/Interval
  config.rs      config file load/save (CLI > env > file > defaults)
  source/        DataSource trait + implementations
    yahoo.rs     Yahoo v8 chart endpoint (quote + history in one call)
    finnhub.rs   Finnhub /quote with synthetic session history
  alphai.rs      AlphaAI API client + demand-driven fetch task (TTL cache)
  poller.rs      fetches all symbols concurrently on a timer -> mpsc channel
  app.rs         App state + key handling + event loop + settings logic
  ui/            View trait + implementations
    table.rs     watchlist table
    chart.rs     line chart
    split.rs     table + chart
    news.rs      article list + sentiment rollup + detail pane
    insider.rs   Form 4 rollup + filing list
    settings.rs  modal settings overlay
```

Data flows one way: background tasks (price poller, AlphaAI fetcher) push
events over an mpsc channel into `App::apply`; views are stateless renderers
over `&mut App`. The UI never blocks on the network.

### Adding a price source

Implement `source::DataSource` (one async `fetch` returning quote + candles)
in a new module and register it in `source::make_source`.

### Adding a view

Implement `ui::View` (a stateless `render` over `&mut App`) as a unit struct
and add it to `ui::VIEWS`. Order in that array defines the `1`..`9` hotkeys.

## Development

```sh
cargo test          # unit + TestBackend rendering tests
cargo clippy --all-targets
cargo run -- --once AAPL             # network smoke test without a TTY
ALPHAI_API_KEY=ak_live_... cargo test live_api -- --ignored   # live API smoke
```

The roadmap lives in `PLAN.md` (streaming sources, Alpaca, candlestick view).
Issues and PRs are welcome.

## License

MIT. Not investment advice; data comes from third-party sources and can be
delayed or wrong. Respect the terms of the data providers you enable.
