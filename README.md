# alphai-tui

[![crates.io](https://img.shields.io/crates/v/alphai-tui.svg)](https://crates.io/crates/alphai-tui)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/makeev/alphai-tui/blob/main/LICENSE)

Terminal dashboard for watching stocks: live quotes and charts next to
AI-scored financial news, sentiment and SEC Form 4 insider activity.
Built in Rust with [ratatui](https://ratatui.rs).

![alphai-tui split view: watchlist with sparklines, candlestick chart with SMA 20/100 and RSI(14), AI-scored news feed](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/split.png)

## What you get

- **Split** (the default view): watchlist and chart side by side in the top
  half, the news feed in the bottom half (hidden on very small terminals).
- **News**: enriched articles for the selected ticker, the whole market or
  the 48-hour trending top 10 (`f` cycles the three scopes), shown as a
  list next to a full article card with the complete AI analysis: price
  impact prediction with confidence, relevance and novelty scores,
  actionability, background context, key entities and a contrarian view.
  `x` flips the layout between side-by-side and list-over-card, `v` expands
  the card to full screen, PgUp/PgDn scroll it. Pressing down on the last
  row loads the next page of the feed; the page size adapts to your plan
  automatically (10 per page, 50 on Pro keys). Market and trending scopes
  collapse syndicated reprints to one row per story and show how many
  outlets carry it (`×7`). Enter opens the article page on alphai.io; a
  settings toggle switches that to the original source site. A 7-day
  bullish/bearish rollup tops the ticker scope.

  ![alphai-tui news view: article list next to the full AI analysis card with price impact, trading value, context and a contrarian view](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/news.png)

- **Table**: watchlist with price, change, day range and unicode sparklines.
- **Chart**: candlestick chart of the selected ticker at half-block
  resolution, with a previous-close reference line, SMA 20/100 overlays and
  an RSI(14) panel. `c` switches to the classic Braille line chart, `m` and
  `i` toggle the indicators, `t` cycles interval presets on the fly.
  The client quietly fetches extra history beyond the visible window, so
  the SMA and RSI lines are fully drawn from the first candle on screen
  instead of waiting a hundred candles to warm up.
- **Insider**: SEC Form 4 activity for the selected ticker. A 30-day rollup
  (buys vs sells, dollar volumes, share of pre-arranged 10b5-1 plan trades,
  most active insiders with their transaction counts) above the stream of
  filing events. Each filing row carries a buy/sell glyph and a `D`/`I`
  marker for direct or indirect ownership. The stream pages like the news
  feed: down on the last row loads more.

  ![alphai-tui insider view: 30-day Form 4 rollup with top insiders above the filing stream, article card open on a 10b5-1 plan sale](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/insider.png)

- In-app settings (`s`): pick the price source, paste API keys once and
  choose where Enter opens news articles.
  Everything is saved to a config file, so after the first run a bare
  `alphai-tui` is enough.

Prices work with no key at all (Yahoo). News, sentiment and insider views
use the [AlphaAI](https://alphai.io) API and need a free key.

## Install

With a Rust toolchain (1.85+):

```sh
cargo install alphai-tui
```

Or straight from the repository:

```sh
cargo install --git https://github.com/makeev/alphai-tui
```

Or from a clone:

```sh
git clone https://github.com/makeev/alphai-tui
cd alphai-tui
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
| `-s, --source` | `yahoo` | Price source: `yahoo`, `finnhub` or `alpaca` |
| `-e, --every` | `15` | Poll interval, seconds |
| `-r, --range` | `1d` | History window: `1d 5d 1mo 3mo 6mo 1y 2y` |
| `-i, --interval` | `5m` | Candle size: `1m 2m 5m 15m 30m 60m 1d` |
| `--once` | | Print quotes to stdout and exit |

`-r` and `-i` set the startup window; the `t` key cycles preset combinations
for the session without persisting them.

CLI arguments win over the config file; the config file wins over built-in
defaults. API keys can also come from env vars, which win over the config:
`ALPHAI_API_KEY`, `FINNHUB_API_KEY`, `APCA_API_KEY_ID`, `APCA_API_SECRET_KEY`.

## Keys

| Key | Where | Action |
|-----|-------|--------|
| `Tab` / `1`..`5` | everywhere | switch view |
| `↑` `↓` / `j` `k` | table, chart, split | select ticker |
| `↑` `↓` / `j` `k` | news, insider | scroll articles |
| `←` `→` / `h` `l` | news, insider | switch ticker |
| `Enter` / `o` | news, insider | open article in browser |
| `v` | news, insider | fullscreen article card; scroll with `↑` `↓`, `Esc` closes |
| `x` | news | flip the list/card layout: side-by-side or stacked |
| `PgUp` `PgDn` | news | scroll the article card pane |
| `↓` / `j` on the last row | news, insider | load the next page of the feed |
| `f` | news, split | cycle news scope: selected ticker, whole market, trending |
| `c` | chart, split | toggle candlestick / line chart |
| `m` | chart, split | toggle SMA 20 and SMA 100 overlays |
| `i` | chart, split | toggle the RSI(14) panel |
| `t` / `T` | everywhere | cycle candle interval presets forward / back (`5m` `15m` `60m` `1d`, each with a matching history window) |
| `r` | everywhere | refresh prices and the visible news view |
| `s` | everywhere | settings |
| `q` / `Esc` / `Ctrl-C` | everywhere | quit |

## A tmux workspace

alphai-tui is a single self-contained process, so a terminal multiplexer
(tmux, zellij, screen, or your terminal's own splits) turns it into a
custom trading workspace: run one instance per pane and switch each pane
to the view you want with `1`..`5`.

```sh
tmux new-session -d -s market 'alphai-tui CRWV'
tmux split-window  -h 'alphai-tui AAPL'      # news pane on the right
tmux split-window -v -t market:0.0 'alphai-tui NVDA'
tmux split-window -v -t market:0.1 'alphai-tui NBIS'
tmux attach -t market
```

Press `4` in the chart panes and `2` in the news pane, and you get a wall
of charts next to a live scored feed:

![four alphai-tui instances in tmux panes: three candlestick charts with SMA and RSI next to a full-height AI-scored news view](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/tmux.png)

Two things the instances share. The config file is one: the last pane to
save settings wins, so set things up once and let the other panes just
read it. Your AlphaAI key's rate budget is the other: every pane showing
news or insider data spends requests from the same per-key allowance, so
on a free key keep an eye on how many such panes you open.

## Data sources

**Prices**

- `yahoo`: no API key, quote and candle history in one request, roughly
  15 minutes delayed. Crypto and FX tickers work as `BTC-USD`, `EURUSD=X`.
- `finnhub`: needs a key (free at [finnhub.io](https://finnhub.io)).
  Real-time-ish quotes; historical candles are premium-only there, so charts
  build up from quotes collected during the session and reset on restart.
  Range/interval switching with `t` does not apply to that synthetic
  history, and candles degrade to flat marks.
  Free tier is 60 req/min: keep `tickers x (60 / --every)` under 60.
  Crypto needs exchange-prefixed symbols (`BINANCE:BTCUSDT`).
- `alpaca`: needs a key id and secret (free at
  [alpaca.markets](https://alpaca.markets)). Realtime quotes from the IEX
  feed plus real historical bars, so charts are complete right after start
  instead of growing over the session. Crypto works in the usual `BTC-USD`
  form. Getting free keys:
  1. Sign up at [alpaca.markets](https://alpaca.markets). Email is enough;
     market data and paper trading need no KYC.
  2. The free Basic data plan is enabled by default.
  3. In the dashboard switch the environment to Paper (fine for data), then
     Home > API Keys > Generate. Copy the Key ID and the Secret; the secret
     is shown only once.
  4. Paste both in the settings screen (`s`) or export `APCA_API_KEY_ID`
     and `APCA_API_SECRET_KEY`.

  Free plan notes: the IEX feed is realtime but thin (roughly 2 to 3 percent
  of market volume, so charts of illiquid names can be sparse), and the API
  allows 200 requests/min. The app makes 2 requests per ticker per poll:
  keep `tickers x 2 x (60 / --every)` under 200. `ALPACA_FEED=sip` needs a
  paid data plan; `ALPACA_FEED=delayed_sip` gives the full market with a
  15 minute delay.

**News, sentiment, insider**

- [AlphaAI](https://alphai.io): AI-enriched financial news feed. Every
  article carries validated tickers, a category, a deterministic 1 to 10
  relevance score and a full per-ticker AI analysis (sentiment, price
  impact, confidence, novelty, actionability); insider rows are generated
  from SEC EDGAR Form 4 filings, one row per economic event. The free tier
  (no card) allows 20 requests/min and 100/day. The app is careful with
  that budget: it fetches only what the visible view needs (the trending
  scope is one extra request), caches each response for 5 minutes, loads
  further pages only when you ask for them, and the article card reuses
  data already fetched with the list. Feeds page 10 articles at a time (50
  on Pro keys, detected automatically). Paging back past your plan's
  archive horizon (30 days on Free, 90 on Basic) shows an upgrade hint
  instead of older articles. Full API reference:
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
news_open = "alphai"  # where enter opens news: "alphai" or "original"

[keys]
alphai = "ak_live_..."
finnhub = ""
alpaca_key_id = ""
alpaca_secret = ""
```

## Architecture

```
src/
  domain.rs      Quote, Candle, TickerData, Range/Interval
  config.rs      config file load/save (CLI > env > file > defaults)
  source/        DataSource trait + implementations
    yahoo.rs     Yahoo v8 chart endpoint (quote + history in one call)
    finnhub.rs   Finnhub /quote with synthetic session history
    alpaca.rs    snapshot + real historical bars (IEX/SIP feeds, crypto)
  alphai.rs      AlphaAI API client + demand-driven fetch task (TTL cache)
  indicators.rs  SMA and RSI (Wilder smoothing)
  poller.rs      fetches all symbols concurrently on a timer -> mpsc channel
  app.rs         App state + key handling + event loop + settings logic
  ui/            View trait + implementations
    table.rs     watchlist table
    chart.rs     candlestick + line chart, SMA overlays, RSI panel
    split.rs     table + chart
    news.rs      article list + sentiment rollup + detail pane
    insider.rs   Form 4 rollup + filing list
    article.rs   modal full-article card (AI analysis, context)
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

Issues and PRs are welcome.

## License

MIT. Not investment advice; data comes from third-party sources and can be
delayed or wrong. Respect the terms of the data providers you enable.
