# PLAN.md — roadmap

Phased roadmap for **tr-monitor** — a Rust/ratatui terminal TUI for watching
ticker prices with pluggable data sources and pluggable views.
Status marks: ✅ shipped · 🔨 in progress · ⏳ planned · ❌ dropped.

Details of a non-trivial phase live in a `docs/` plan file (frontmatter
`status: plan`) linked from the phase. When a phase fully ships: mark it ✅
here and **delete** its docs/ plan file — git history is the archive.

## Context for agents

- Build/test: `cargo build`, `cargo test` (4 TestBackend rendering tests in
  `src/ui/tests.rs`). Run: `cargo run --release -- AAPL MSFT NVDA`.
  Data-path smoke test without a TTY: `cargo run -- --once AAPL`.
- Architecture and extension points (how to add a source / a view) are in
  `README.md`. Short version: a data source implements
  `source::DataSource` (`src/source/mod.rs`) and gets registered in
  `make_source`; a view implements `ui::View` (`src/ui/mod.rs`) and gets an
  entry in the `ui::VIEWS` array (array order = `1`–`9` hotkeys).
- All UI state lives in `app::App`; views are stateless renderers. Data flows
  one way: poller task (tokio) → `SourceEvent` over an unbounded mpsc channel
  → `App::apply` → render loop (sync, 100 ms tick, `crossterm::event::poll`).
- The UI never blocks on the network and never talks to a source directly —
  keep it that way when adding sources/views.

## Phase 1 — core TUI + Yahoo source ✅ (v0.1, 2026-07-10)

What shipped:

- Domain model (`src/domain.rs`): `Quote`, `Candle` (full OHLCV — open/high/
  low/volume are intentionally unused until the candlestick view),
  `TickerData`, `Range` (1d…1y) and `Interval` (1m…1d) as clap `ValueEnum`s.
- `DataSource` trait (async `fetch(symbol, range, interval) -> TickerData`)
  + Yahoo implementation (`src/source/yahoo.rs`) on the v8 chart endpoint:
  one keyless HTTP request per symbol returns both the latest price and the
  candle history. Quotes are ~15 min delayed. Needs a browser-like
  `User-Agent` or Yahoo intermittently rejects requests.
- Poller (`src/poller.rs`): fetches all symbols concurrently (`JoinSet`) every
  `--every` seconds (default 15); `r` key triggers an immediate cycle via
  `tokio::sync::Notify`. Per-symbol errors flow as `SourceEvent::Error` and
  render in the footer + as `error` rows — one bad ticker never breaks the rest.
- Three views: Table (price, Δ, Δ%, lo–hi, unicode sparkline), Chart (Braille
  line chart of closes + dotted previous-close reference line, green/red by
  direction), Split (table 45% + chart 55%).
- Keys: `Tab`/`Shift-Tab`/`1`–`9` views, `↑↓`/`jk` selection, `r` refresh,
  `q`/`Esc`/`Ctrl-C` quit.
- `--once` flag: print quotes to stdout and exit — scripting + data-path
  smoke test.
- Tests: 4 rendering tests over ratatui `TestBackend` (table content, chart
  Braille cells, split view, placeholder/error rendering must not panic).

Gotchas discovered (also in `issues/` if they recur):

- reqwest 0.13 renamed the TLS feature `rustls-tls` → `rustls`, and `.query()`
  now needs the `query` feature. We use
  `--no-default-features --features json,rustls,query,gzip,http2`.
- ratatui is 0.30 (split into ratatui-core/-widgets); crossterm 0.29 is a
  direct dependency for the event loop.
- A pty from `script(1)` has zero window size — the TUI renders nothing there.
  Verify rendering via the `TestBackend` tests, not via `script`.

## Phase 2 — Finnhub source ✅ (2026-07-10)

Second polling source; proved the `DataSource` plugin contract.

- `src/source/finnhub.rs`, registered as `"finnhub" | "fh"`; API key from
  `FINNHUB_API_KEY` env (never in the repo).
- Free tier reality: `/quote` is real-time-ish and free (60 req/min), but
  **historical candles are premium-only** (`/stock/candle` → 403). So the
  source synthesizes history: each poll appends a tick-candle to an in-memory
  per-symbol series (`push_tick`, capped at 600, same-timestamp ticks update
  the last candle). Charts grow over the session, reset on restart.
- Unknown symbols come back as HTTP 200 with all-zero fields — detected and
  turned into a readable error. 429 → readable rate-limit hint.
- Crypto needs exchange-prefixed symbols (`BINANCE:BTCUSDT`).
- Unit tests for `push_tick` (dedupe + cap) in the module.

## Phase 3 — Alpaca source ⏳

Goal: the primary real-time source. Decision (July 2026): **IBKR dropped** —
TWS/Gateway must stay running and the session drops on phone login; Alpaca
free tier gives real-time IEX websocket + REST with just an API key from a
paper account, no running terminal. (Polygon/"Massive" starts at ~$79/mo —
only if historical data becomes a need.)

- REST first (fits the `fetch` contract): latest trade/quote + bars
  (`/v2/stocks/{symbol}/bars`) mapped to `Range`/`Interval` — unlike Finnhub,
  Alpaca's free tier does include historical bars (IEX feed).
- Keys from `ALPACA_API_KEY_ID` / `ALPACA_API_SECRET_KEY` env.
- Register as `"alpaca"`; data feed param must be `iex` (free) not `sip`.
- Acceptance: `--once -s alpaca AAPL` prints a quote; TUI charts show real
  history; missing keys → readable startup error.

## Phase 4 — streaming ⏳ (after Phase 3)

Goal: push-based updates for sources that support them (Alpaca IEX websocket;
later crypto exchange websockets), without touching the UI.

- Extend `DataSource` with a default-None method, e.g.
  `fn watch(&self, symbols) -> Option<BoxStream<SourceEvent>>`.
- Poller: if `watch()` is Some — forward stream events into the same mpsc
  channel (history still fetched via `fetch` at startup and on `r`);
  if None — current polling loop, unchanged.
- Likely needs a lighter event (`SourceEvent::Quote`) that updates price only,
  merged into existing `TickerData` in `App::apply`, so ticks don't wipe
  candle history.
- Acceptance: UI code untouched except `App::apply`; Yahoo path behavior
  identical.

## Phase 5 — candlestick view ⏳

Goal: proper OHLC candles; the domain model already carries the data.

- ratatui has no native candlestick widget — render manually (Canvas widget
  or direct Buffer painting: wick = `│`, body = `█`/`░` colored green/red).
- New unit struct in `src/ui/candles.rs`, one entry in `ui::VIEWS` — this is
  also the dogfood test that the View plugin contract holds.
- Downsample candles to the available width (like the table sparkline does).
- Acceptance: a TestBackend test asserting candle glyphs render; view
  reachable via hotkey `4`.

## Phase 6 — config file ⏳

Goal: run bare `tr-monitor` with a saved setup.

- `~/.config/tr-monitor/config.toml` (`directories` or `dirs` crate): default
  watchlist, source, `every`, `range`/`interval`, per-source blocks
  (finnhub key, alpaca key id/secret) — env vars keep working and win over
  the config file.
- Precedence: CLI args > config > built-in defaults. Symbols on the CLI
  replace the config watchlist entirely (no merging).
- Acceptance: bare `tr-monitor` runs from config; `--once` still works with
  no config file present.

## Backlog (unordered ideas, promote to a phase when picked)

- Crypto via free exchange websockets (Binance/Coinbase) as a separate
  source — decided direction for crypto tickers, promote when picked.
- Alpha Vantage source — demoted from the original Phase 2: free tier is
  25 req/day (unusable for monitoring) and Finnhub already proved the
  plugin contract. Only worth it if some AV-specific data is needed.
- Multi-chart grid view (small chart per ticker).
- Runtime watchlist editing (`a` to add / `d` to remove a ticker).
- Table sorting (by Δ%, by symbol).
- Pre/post-market prices where the source provides them.
- Price-level alerts (bell / desktop notification when a threshold crosses).
- FX/crypto conveniences (Yahoo already handles `BTC-USD`, `EURUSD=X`).
