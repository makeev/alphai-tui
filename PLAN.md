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

## Phase 2 — Alpha Vantage source ⏳

Goal: second polling source to prove the `DataSource` abstraction; useful for
tickers/fields Yahoo lacks.

- New module `src/source/alphavantage.rs`, register as
  `"alphavantage" | "av"` in `make_source`.
- API key from `ALPHAVANTAGE_API_KEY` env (later: config file, Phase 6).
  Missing key → hard error at startup with a clear message.
- Endpoints: `GLOBAL_QUOTE` (price + prev close) and `TIME_SERIES_INTRADAY`
  (candles; intervals 1/5/15/30/60min) or `TIME_SERIES_DAILY` for
  `Interval::D1`. Two HTTP calls per symbol per cycle — mind the mapping in
  one `fetch`.
- **Rate limits are the design constraint**: free tier is 25 requests/day.
  Fetch history once at startup and only poll `GLOBAL_QUOTE` afterwards;
  enforce a minimum poll interval (e.g. refuse `--every < 60` for this
  source, or auto-clamp with a footer warning). Surface remaining-quota
  errors (AV returns them as JSON "Note"/"Information" fields with HTTP 200 —
  must be detected in the body, not the status code).
- Acceptance: `cargo run -- --once -s av AAPL` prints a quote; TUI shows data
  with `-s av`; a bogus key shows a readable error, not a JSON parse failure.

## Phase 3 — IBKR source ⏳

Goal: realtime-quality data from Interactive Brokers via a running
TWS/IB Gateway.

- Candidate crate: `ibapi` (evaluate current state first; fallback is the
  Client Portal Web API over plain HTTPS, which would reuse reqwest).
- Polling first (fits the existing `fetch` contract): snapshot market data +
  historical bars mapped to `Range`/`Interval`.
- Connection params (host, port 7496 live / 7497 paper, client id) — CLI
  flags now, config file later (Phase 6).
- Requires market-data subscriptions on the IBKR account; errors from missing
  subscriptions must render readably in the footer.
- Acceptance: with Gateway running, `-s ibkr` shows live quotes; with Gateway
  down, a clear "cannot connect to TWS/Gateway at host:port" error.

## Phase 4 — streaming ⏳ (after Phase 3)

Goal: push-based updates for sources that support them (IBKR), without
touching the UI.

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
  (alphavantage key, ibkr host/port/client-id).
- Precedence: CLI args > config > built-in defaults. Symbols on the CLI
  replace the config watchlist entirely (no merging).
- Acceptance: bare `tr-monitor` runs from config; `--once` still works with
  no config file present.

## Backlog (unordered ideas, promote to a phase when picked)

- Multi-chart grid view (small chart per ticker).
- Runtime watchlist editing (`a` to add / `d` to remove a ticker).
- Table sorting (by Δ%, by symbol).
- Pre/post-market prices where the source provides them.
- Price-level alerts (bell / desktop notification when a threshold crosses).
- FX/crypto conveniences (Yahoo already handles `BTC-USD`, `EURUSD=X`).
