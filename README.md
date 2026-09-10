# alphai-tui

[![CI](https://github.com/makeev/alphai-tui/actions/workflows/ci.yml/badge.svg)](https://github.com/makeev/alphai-tui/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/alphai-tui.svg)](https://crates.io/crates/alphai-tui)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/makeev/alphai-tui/blob/main/LICENSE)

Terminal dashboard for watching stocks: live quotes and charts next to
AI-scored financial news, sentiment and SEC Form 4 insider activity.
Built in Rust with [ratatui](https://ratatui.rs).

![alphai-tui demo: news list with the AI analysis card, market and trending scopes, SEC Form 4 insider stream, candlestick chart with SMA and RSI, split dashboard](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/demo.gif)

## What you get

- **The quote rail**: one line under the tabs, present in every view, with
  the selected ticker's price, its change on the day, where that price sits
  between the session low and high, what the US market is doing right now
  (pre, live, post or closed, and how long until the next bell) and the
  rest of the watchlist as percentages. The News, Insider and Earnings
  views are no longer a ticker name with no price attached, and moving
  between tickers with the arrow keys is not a blind jump. Parts drop one
  at a time as the terminal narrows, the symbol and the price surviving to
  the last; `[ui] quote_rail = false` turns the line off, and a terminal
  under 12 rows gives the row back to the view. A delayed source (Yahoo, or
  `ALPACA_FEED=delayed_sip`) says so on the rail instead of passing a
  15 minute old price off as live. After the closing bell the rail also
  carries the extended-hours print (`AH`, or `PRE` before the open) with
  its move measured against the close, so news breaking outside the session
  is not read next to a price frozen at 16:00. Where the source reports
  them, the year's range and the day's volume follow at the end of the
  line, first to go as the terminal narrows.
- **Split** (the default view): watchlist and chart side by side in the top
  half, the news feed in the bottom half (hidden on very small terminals).
- **News**: enriched articles for the selected ticker, the whole market or
  the 48-hour trending top 10 (`f` cycles the three scopes), shown as a
  list next to a full article card with the complete AI analysis: price
  impact prediction with confidence, relevance and novelty scores,
  actionability, background context, key entities and a contrarian view.
  The feed shows articles with a relevance score of 7 and up by default;
  `+` and `-` move that bar live (1 to 10, filtered server-side, the block
  title shows the active value) and `[ui] news_min_score` sets the startup
  default. Articles fresher than 15 minutes light up their age in the
  accent color, and rows that appeared since you last looked at the feed
  carry a `●` marker that goes out once the cursor rests on them (insider
  rows get the same treatment). Arrivals are placed at the top of the list
  even when the rows below them are newer: an article reaches the feed a
  while after it was published (a Form 4 days after its trade), so at its
  publish position it would land below the fold and never be seen. `x`
  flips the layout between side-by-side and list-over-card, `v` expands the
  card to full screen, PgUp/PgDn scroll it.
  On terminals narrower than 90 columns the side layout gives the whole
  width to the list and `v` remains the way to read the card. Pressing
  down on the last row loads the next page of the feed; the page size
  adapts to your plan automatically (20 per page, 50 on Pro keys). Market
  and trending scopes collapse syndicated reprints to one row per story
  and show how many outlets carry it (`×7`). Enter opens the article page
  on alphai.io; a settings toggle switches that to the original source
  site. A 7-day bullish/bearish rollup tops the ticker scope.

  ![alphai-tui news view: article list next to the full AI analysis card with price impact, trading value, context and a contrarian view](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/news.png)

- **Table**: watchlist with price, change, day range and unicode sparklines.
- **Chart**: candlestick chart of the selected ticker at half-block
  resolution, with a previous-close reference line, 20/100 moving average
  overlays, a volume panel and an RSI(14) panel. The overlays thread through
  the candles as thin braille lines and average simple or exponential (`e`,
  or `[chart] ma_type`); the volume bars sit in their candles' own columns
  and take their color, so a move and the volume behind it read together.
  `c` switches to the classic Braille line chart, `m`, `i` and `b` toggle
  the indicators and panels, `t` cycles interval presets on the fly. A
  short terminal drops the panels rather than squeezing the price chart,
  and sources without volume (finnhub) simply have no volume panel.
  The client quietly fetches extra history beyond the visible window, so
  the average and RSI lines are fully drawn from the first candle on screen
  instead of waiting a hundred candles to warm up. Like a trading
  terminal, the chart keeps a margin right of the newest candle (20% of
  the plot; `[chart] right_margin_pct` resizes it, 0 turns it off) with a
  last-price marker line and tag in it. Every time a poll changes the
  price, the marker and the title price pulse in the tick's color for a
  moment, and the newest candle folds the live price into its close, so
  the candle and the marker never disagree and a live market is visible
  at a glance; the poll interval is `--every` / the `Poll every`
  settings row.
- **Insider**: SEC Form 4 activity for the selected ticker. A 12-month
  rollup (buys vs sells, dollar volumes, share of pre-arranged 10b5-1 plan
  trades, most active insiders with their event counts) sits above a trades
  chart and the stream of filing events. The chart mirrors the insider
  trades page on alphai.io: every event in the window is a triangle placed
  by date on a log dollar scale (`▲` buy, `▼` sale, a hollow `▽` for shares
  sold back to the issuer, dimmed when the trade ran under a 10b5-1 plan),
  with weekly buy/sell dollar bars underneath and month marks along the
  axis. The mark of the filing selected in the list renders inverted, so
  the list and the chart always point at each other. `g` cycles the chart
  window: 3 months, 12 months, off (`[ui] insider_chart` sets the startup
  value); on low terminals the bars drop first and the whole panel yields
  before the list would starve. The chart plots every event the API knows
  in the window, unaffected by the score filter below. Each filing row
  shows the trade side straight from the
  filing (a buy/sell glyph; a sale back to the issuer stays neutral instead
  of reading as a market sale), a `D`/`I` marker for direct or indirect
  ownership, a `p` flag on pre-arranged 10b5-1 plan trades and the total
  trade value. The article card breaks the event down further: shares, the
  value-weighted average price, the SEC transaction code, who traded and
  their role, and the transaction date; the detail pane below the list adds
  what the chart data knows about the selected filing: the share of the
  insider's stake the event moved, how many tranches the filing folded
  into it, and a late-filing flag. Insider rows are scored from the
  trade size, so `+` and `-` filter the stream by dollar value
  (`[ui] insider_min_score` sets the startup default; 7 keeps roughly the
  $10M+ trades). The stream pages like the news feed: down on the last row
  loads more.

  ![alphai-tui insider view: the Form 4 trades chart, a log-scale scatter of sales over weekly dollar bars, above the filing stream with the 12-month rollup on top](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/insider.png)

- **Earnings**: AlphaAI's structured read of the selected ticker's own
  earnings filing, the way a company reports it: the verdict and why, the
  metric table with prior quarter, prior year and both changes, segments,
  the outlook, concerns, what to watch and several paragraphs of analysis.
  Every figure in it was checked against the filing text before it was
  published, and it prints exactly as the filing wrote it, units shortened
  and nothing rounded into a new number. Both American and foreign filings
  are covered (an 8-K item 2.02, or a foreign private issuer's 6-K, which
  reports half years and its own currency). The metric table is banded row
  by row and led by dots from each name across to its figures, and it stays
  as wide as its own content however wide the terminal is. `←` `→` walk the
  watchlist, `↑` `↓` scroll, and older quarters continue below the newest
  read.
  When a company has not reported since AlphaAI began reading filings, the
  view says so and gives the date of its next report when the company has
  confirmed one. The bottom line carries the next couple of US macro
  releases (CPI, the jobs report, an FOMC decision and the rest), which is
  the other half of what moves a price you are about to read about.
  Filings show up in the News feed too: the row for the filing itself is
  marked `8-K` or `6-K` rather than `earnings`, which tells it apart from
  the coverage around it, and its article card carries a short form of the
  read once the Earnings view has loaded it.
- In-app settings (`s`): pick the price source, paste API keys once, set
  the poll interval (applies immediately) and choose where Enter opens
  news articles. Everything is saved to a config file, so after the first
  run a bare `alphai-tui` is enough.

Prices work with no key at all (Yahoo). News, sentiment and insider views
use the [AlphaAI](https://alphai.io?utm_source=alphai-tui&utm_medium=referral) API and need a free key.

## Install

Homebrew (macOS and Linux):

```sh
brew install makeev/tap/alphai-tui
```

Arch Linux, from the AUR:

```sh
paru -S alphai-tui-bin
```

Debian and Ubuntu, from the apt repository (amd64 and arm64, Ubuntu 22.04 and
Debian 12 or newer):

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://makeev.github.io/alphai-tui-apt/alphai-tui.gpg \
  -o /etc/apt/keyrings/alphai-tui.gpg
echo "deb [signed-by=/etc/apt/keyrings/alphai-tui.gpg] https://makeev.github.io/alphai-tui-apt stable main" \
  | sudo tee /etc/apt/sources.list.d/alphai-tui.list
sudo apt update && sudo apt install alphai-tui
```

Single `.deb` files, for an install without the repository, are linked from
[the repository landing page](https://makeev.github.io/alphai-tui-apt/).

Prebuilt binaries for macOS, Linux and Windows, no Rust needed:

```sh
curl -LsSf https://github.com/makeev/alphai-tui/releases/latest/download/alphai-tui-installer.sh | sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/makeev/alphai-tui/releases/latest/download/alphai-tui-installer.ps1 | iex"
```

Archives for every platform, with checksums, live on the
[releases page](https://github.com/makeev/alphai-tui/releases).

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
AlphaAI key (get one free at [alphai.io](https://alphai.io?utm_source=alphai-tui&utm_medium=referral), Account >
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
| `-e, --every` | `15` | Poll interval, seconds (also a settings row, applied live) |
| `-r, --range` | `1d` | History window: `1d 5d 1mo 3mo 6mo 1y 2y` |
| `-i, --interval` | `5m` | Candle size: `1m 2m 5m 15m 30m 60m 1d` |
| `--theme` | `default` | Color preset, e.g. `catppuccin-mocha` (also a key and a settings row) |
| `--bare` | off | Start without the header and footer, for a tmux pane (`z` toggles it live) |
| `--once` | | Print quotes to stdout and exit |
| `--earnings TICKER` | | Print the latest earnings read to stdout and exit (needs an AlphaAI key; one request) |
| `--config` | | Use an alternate config file (Save writes back to it) |

`-r` and `-i` set the startup window; the `t` key cycles the preset
combinations (configurable as `[chart] presets`) for the session without
persisting them.

CLI arguments win over the config file; the config file wins over built-in
defaults. API keys can also come from env vars, which win over the config:
`ALPHAI_API_KEY`, `FINNHUB_API_KEY`, `APCA_API_KEY_ID`, `APCA_API_SECRET_KEY`.

## Keys

| Key | Where | Action |
|-----|-------|--------|
| `Tab` / `1`..`6` | everywhere | switch view |
| `↑` `↓` / `j` `k` | table, chart, split | select ticker |
| `↑` `↓` / `j` `k` | news, insider | scroll articles |
| `↑` `↓` / `j` `k` | earnings | scroll the read |
| `←` `→` / `h` `l` | news, insider, earnings | switch ticker |
| `Enter` / `o` | news, insider | open article in browser |
| `Enter` / `o` | earnings | open the read on alphai.io |
| `v` | news, insider | fullscreen article card; scroll with `↑` `↓`, `Esc` closes |
| `x` | news | flip the list/card layout: side-by-side or stacked |
| `PgUp` `PgDn` | news | scroll the article card pane |
| `PgUp` `PgDn` | earnings | page through the read |
| `↓` / `j` on the last row | news, insider | load the next page of the feed |
| `f` | news, split | cycle news scope: selected ticker, whole market, trending |
| `+` / `-` | news, insider, split | raise / lower the visible feed's score filter (news: relevance, starts at 7; insider: trade size, starts at 4) |
| `g` | insider | cycle the trades chart window: 3 months, 12 months, off |
| `c` | chart, split | toggle candlestick / line chart |
| `m` | chart, split | toggle the two moving average overlays |
| `e` | chart, split | average them simple (SMA) or exponential (EMA) |
| `i` | chart, split | toggle the RSI(14) panel |
| `b` | chart, split | toggle the volume panel |
| `t` / `T` | everywhere | cycle candle interval presets forward / back (each interval with a matching history window; the list is configurable as `[chart] presets`) |
| `r` | everywhere | refresh prices and the visible news view |
| `z` | everywhere | bare mode: hide the header and footer, giving both rows to the view |
| `p` / `P` | everywhere | next / previous color preset (session-only until Save) |
| `s` | everywhere | settings |
| `?` | everywhere | help overlay: every action with its current keys |
| `q` / `Esc` / `Ctrl-C` | everywhere | quit |

## A tmux workspace

alphai-tui is a single self-contained process, so a terminal multiplexer
(tmux, zellij, screen, or your terminal's own splits) turns it into a
custom trading workspace: run one instance per pane and switch each pane
to the view you want with `1`..`5`.

```sh
tmux new-session -d -s market 'alphai-tui --bare CRWV'
tmux split-window  -h 'alphai-tui --bare AAPL'      # news pane on the right
tmux split-window -v -t market:0.0 'alphai-tui --bare NVDA'
tmux split-window -v -t market:0.1 'alphai-tui --bare NBIS'
tmux attach -t market
```

`--bare` drops the header and the key hints, which a pane with tmux's own
status bar has little use for, and hands both rows to the view; `z` toggles
it in a running instance and `[ui] bare = true` makes it the default. The
quote rail stays, so a bare pane still names its ticker and its price.

Press `4` in the chart panes and `2` in the news pane, and you get a wall
of charts next to a live scored feed:

![four alphai-tui instances in tmux panes: three candlestick charts with SMA and RSI next to a full-height AI-scored news view](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/tmux.png)

Two things the instances share. The config file is one: the last pane to
save settings wins, so set things up once and let the other panes just
read it. Your AlphaAI key's rate budget is the other: every pane showing
news or insider data spends requests from the same per-key allowance, so
on a free key keep an eye on how many such panes you open.

The same trick turns the terminal into a full trading desk with an AI
analyst on staff. Run an agent such as
[Claude Code](https://claude.com/claude-code) in the pane next to
alphai-tui and connect it to the
[AlphaAI MCP server](https://alphai.io/mcp?utm_source=alphai-tui&utm_medium=referral), which serves the same news,
sentiment and insider data as the dashboard. You watch the tape on one
side while the agent digs through whatever the tape surfaces: ask it for
the last insider sells and the news that moved the stock this week, and
get a sourced brief without leaving the terminal.

![Claude Code next to alphai-tui in tmux: the agent summarizes CRWV insider selling and the week's dominant story while the dashboard shows the candlestick chart and the scored news feed](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/agent.png)

## Data sources

**Prices**

- `yahoo`: no API key, quote and candle history in one request, roughly
  15 minutes delayed. Crypto and FX tickers work as `BTC-USD`, `EURUSD=X`.
  The only source here that reports extended-hours prices, the 52 week
  range and full market volume, all in the same request as the price.
- `finnhub`: needs a key (free at [finnhub.io](https://finnhub.io)).
  Real-time-ish quotes; historical candles are premium-only there, so charts
  build up from quotes collected during the session and reset on restart.
  Range/interval switching with `t` does not apply to that synthetic
  history, and candles degrade to flat marks.
  Free tier is 60 req/min, one request per ticker per poll; the app warns
  on startup and in the settings screen when the watchlist and the poll
  interval together go over that.
  Crypto needs exchange-prefixed symbols (`BINANCE:BTCUSDT`).
  Its quote endpoint covers the regular session only, so no extended-hours
  price, 52 week range or volume.
- `alpaca`: needs a key id and secret (free at
  [alpaca.markets](https://alpaca.markets)). Realtime quotes from the IEX
  feed plus real historical bars, so charts are complete right after start
  instead of growing over the session. Crypto works in the usual `BTC-USD`
  form. IEX is one exchange rather than the whole tape, so on the free feed
  there are no pre or post market prints and no volume figure: a few
  percent of the day's shares would read as the day's volume. Both appear
  on `ALPACA_FEED=sip` or `delayed_sip`. Getting free keys:
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
  allows 200 requests/min. The app makes 2 requests per ticker per poll and
  warns, on startup and under the interval in the settings screen, when the
  watchlist and the interval together go over the ceiling, naming an
  interval that fits (going over turns tickers into `error` rows).
  `ALPACA_FEED=sip` needs a paid data plan; `ALPACA_FEED=delayed_sip` gives
  the full market with a 15 minute delay.

**News, sentiment, insider**

- [AlphaAI](https://alphai.io?utm_source=alphai-tui&utm_medium=referral): AI-enriched financial news feed. Every
  article carries validated tickers, a category, a deterministic 1 to 10
  relevance score and a full per-ticker AI analysis (sentiment, price
  impact, confidence, novelty, actionability); insider rows are generated
  from SEC EDGAR Form 4 filings, one row per economic event. The free tier
  (no card) allows 20 requests/min and 100/day. The app is careful with
  that budget: it fetches only what the visible view needs (the trending
  scope is one extra request), caches each response for 5 minutes
  (`[ui] alphai_ttl_secs` in the config changes that), loads
  further pages only when you ask for them, and the article card reuses
  data already fetched with the list. The refresh at the end of that cache
  window asks the server what has arrived since the previous check rather
  than re-reading the newest page, which costs the same single request,
  keeps the pages you loaded and the row you are on, and is the only way to
  see an article that entered the feed behind its own publish time. The
  relevance filter is applied by the server, so filtered-out articles never
  occupy page slots; moving it with `+`/`-` refetches the visible feed, one
  request per press at most.
  The Insider view's rollup and trades chart arrive as one bundle
  alongside the feed's first page and live in the same cache, so the
  chart costs no extra requests and the `g` window switch is free.
  The Earnings view costs one request per ticker, made only while that
  view is on screen and cached for an hour, because a read is published
  once a quarter and never changes afterwards. That single response
  carries the whole history of reads for the ticker and its next
  confirmed report date, which is also what fills the read shown in the
  News card, so opening the card still costs nothing. The macro calendar
  behind the bottom line of that view is one request for the whole market,
  cached for six hours; if it fails there is simply no line.
  Feeds page 20 articles at a time, the most every plan allows (50 on Pro
  keys, detected automatically). Paging back past your plan's
  archive horizon (30 days on Free, 90 on Basic) shows an upgrade hint
  instead of older articles. Full API reference:
  [alphai.io/developers](https://alphai.io/developers?utm_source=alphai-tui&utm_medium=referral).

Ticker forms follow the US/Yahoo convention (`AAPL`, `BTC-USD`, `VOD.L`),
which is also what AlphaAI uses. Finnhub-specific symbols like
`BINANCE:BTCUSDT` will not have news attached.

## Configuration

`~/.config/alphai-tui/config.toml` on Linux and macOS (`%APPDATA%` on
Windows), created by the settings screen with mode 0600 since it can hold
keys; `--config PATH` points at a different file. Saving the settings also
persists the watchlist on screen. Every key is optional. A misspelled value
in the `[ui]`, `[chart]`, `[theme]` or `[keybindings]` sections prints a
warning on startup and keeps that entry's default; only a TOML syntax error
makes the whole file fall back to defaults. The `[ui]` and `[chart]` sections set startup
defaults; the session keys (`x`, `f`, `g`, `+`, `-`, `c`, `m`, `i`, `b`, `e`, `t`) still
change everything live without persisting it:

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

[ui]
default_view = "split"    # split | news | table | chart | insider | earnings
quote_rail = true         # the price line under the tabs
bare = false              # start with no header and no footer (--bare, z)
news_layout = "side"      # side | stacked
news_scope = "ticker"     # ticker | market | trending
borders = "rounded"       # panel frames: rounded | plain
news_min_score = 7        # minimum relevance score in news feeds, 1 to 10
insider_min_score = 4     # insider feed filter; the score tracks trade size
insider_chart = "3m"      # insider trades chart window at start: 3m | 12m | off
alphai_ttl_secs = 300     # news/sentiment/insider cache lifetime, 30 to 86400

[chart]
style = "candles"         # candles | line
sma = true                # moving average overlays visible at start
ma_type = "sma"           # sma | ema, both using the periods below
rsi = true                # RSI panel visible at start
volume = true             # volume panel visible at start
sma_fast = 20             # 2 to 250
sma_slow = 100            # 2 to 250; also sizes the history warm-up
rsi_period = 14           # 2 to 100
right_margin_pct = 20     # free space right of the newest candle, 0 to 50
presets = [               # the combos the t and T keys cycle
  ["1d", "5m"],
  ["5d", "15m"],
  ["1mo", "60m"],
  ["6mo", "1d"],
  ["1y", "1d"],
]
```

### Colors

`[theme] preset` swaps in a ready-made palette:

```toml
[theme]
preset = "catppuccin-mocha"
```

![alphai-tui cycling through its color presets with the p key: catppuccin mocha, macchiato and frappe, dracula, gruvbox and nord](https://raw.githubusercontent.com/makeev/alphai-tui/main/assets/themes.gif)

`p` and `P` walk the presets live, `--theme catppuccin-mocha` picks one
for a single run, and the Theme row in the settings screen (`s`) does
both: `←` `→` cycle it with a live preview, Save writes it here.

Available: `default`, `catppuccin-mocha`, `catppuccin-macchiato`,
`catppuccin-frappe`, `catppuccin-latte`, `dracula`, `gruvbox-dark`,
`gruvbox-light`, `nord`. Presets are written in hex, so they want a
terminal with 24-bit color; `default` uses ANSI names and follows whatever
palette the terminal itself is set to. They only set foreground colors,
which leaves a transparent or blurred terminal background alone. The two
light ones (`catppuccin-latte`, `gruvbox-light`) expect a light terminal
background.

Every color the views draw comes from a named slot, and the optional
`[theme]` table recolors any of them, over the preset when there is one.
Values are ANSI color names (case-insensitive, `light-blue`, `grey`),
`#RRGGBB` hex, or an ANSI-256 index written as a string like `"245"`. A
bad color, a misspelled slot or an unknown preset prints a warning on
startup and keeps the default; it never breaks the config file. The
defaults are the values shown:

```toml
[theme]
accent = "cyan"          # header title, active tab, overlay borders, headings
accent_text = "black"    # text on the active tab
up = "green"             # price up: candles, deltas, sparklines
down = "red"             # price down
flat = "gray"            # unchanged / no data
pos = "green"            # bullish sentiment, insider buys
neg = "red"              # bearish sentiment, insider sells
error = "red"            # error messages
warn = "yellow"          # notices and the editing highlight
score_high = "yellow"    # relevance score 8 to 10
sma_fast = "yellow"      # fast moving average overlay
sma_slow = "magenta"     # slow moving average overlay
rsi_line = "cyan"        # RSI line
ref_line = "darkgray"    # previous close and RSI 30/70 reference lines
border = "reset"         # panel frames; reset keeps the terminal's foreground
```

### Custom keybindings

The optional `[keybindings]` table rebinds any action. An action you list
replaces its default keys entirely; actions you leave out keep theirs. The
value is one key or a list of keys:

```toml
[keybindings]
quit = "ctrl-q"
open = ["enter", "w"]
next_preset = "]"
prev_preset = "["
```

A key is written as `[ctrl-][alt-][shift-]<base>`, where base is a single
character or one of the named keys: `esc`, `enter`, `tab`, `backtab`,
`space`, `up`, `down`, `left`, `right`, `home`, `end`, `pgup`, `pgdn`,
`backspace`, `delete`, `insert`, `f1` to `f12`. `shift-` plus a letter
means the uppercase letter (`shift-t` equals `T`), and `shift-tab` equals
`backtab`.

The actions: `quit`, `next_view`, `prev_view`, `settings`, `help`,
`refresh`, `up`, `down`, `left`, `right`, `page_up`, `page_down`, `open`,
`card`, `cycle_scope`, `cycle_layout`, `score_up`, `score_down`,
`insider_chart`, `chart_style`, `toggle_sma`, `toggle_rsi`,
`toggle_volume`, `ma_type`, `next_preset`, `prev_preset`, `next_theme`,
`prev_theme`.
The `?` help overlay shows this list with the current keys next to it.

Reserved and never remappable: `ctrl-c` (force quit), `esc`, the digits
`1` to `9` (view hotkeys), and the keys inside the settings form. A bad or
reserved key, an unknown action, or a key claimed by two actions prints a
warning on startup and falls back safely; the footer always shows the keys
that are actually bound.

### Not configurable on purpose

The AlphaAI response cache (5 minutes), the feed page sizes, the 2 second
poll floor and the chart warm-up factors are fixed. They keep the app a
fair citizen of the free API tiers, and a config knob for them would turn
an innocent-looking file into an abuse vector. `ALPACA_FEED`,
`ALPHAI_API_URL` and `ALPACA_DATA_URL` stay env-only debug overrides for
the same reason.

## Architecture

```
src/
  domain.rs      Quote, Candle, TickerData, Range/Interval
  config.rs      config file load/save (CLI > env > file > defaults)
  source/        DataSource trait + implementations
    registry.rs  the one place a new source registers; CLI, settings and keys derive from it
    http.rs      shared client builder, JSON fetching and error helpers
    yahoo.rs     Yahoo v8 chart endpoint (quote + history in one call)
    finnhub.rs   Finnhub /quote with synthetic session history
    alpaca.rs    snapshot + real historical bars (IEX/SIP feeds, crypto)
  alphai.rs      AlphaAI API client + demand-driven fetch task (TTL cache)
  keymap.rs      semantic actions + the key table (footer hints derive from it)
  theme.rs       semantic color palette ([theme] overrides)
  indicators.rs  SMA, EMA and RSI (Wilder smoothing)
  poller.rs      fetches all symbols concurrently on a timer -> mpsc channel
  app.rs         App state, event loop, key handling
    app/feeds.rs     feed cache and every AlphaAI request-budget guard
    app/settings.rs  settings overlay state, rows derived from the registry
  ui/            View trait + implementations
    table.rs     watchlist table
    chart.rs     candlestick + line chart, SMA overlays, volume and RSI panels
    split.rs     table + chart
    news.rs      article list + sentiment rollup + detail pane
    insider.rs   Form 4 rollup + filing list
    insider_chart.rs  log-scale trades scatter + weekly dollar bars
    earnings.rs  structured earnings read + the schedule line
    article.rs   modal full-article card (AI analysis, context)
    settings.rs  modal settings overlay
```

Data flows one way: background tasks (price poller, AlphaAI fetcher) push
events over an mpsc channel into `App::apply`; views are stateless renderers
over `&mut App`. The UI never blocks on the network, and every AlphaAI
request-budget guard lives in one file (`app/feeds.rs`).

### Adding a price source

1. Implement `source::DataSource` (one async `fetch` returning quote plus
   candles) in a new module under `src/source/`. The helpers in
   `source/http.rs` cover the client, JSON fetching and error plumbing.
2. Append one `SourceInfo` entry to `source/registry.rs`: id, aliases, a
   settings hint, the key fields it needs and a constructor. The `--source`
   help and error list, the settings screen rows and picker cycle, config
   `[keys]` persistence and env-var overrides all derive from that entry,
   and the registry tests check it.
3. Describe the source in this README.

### Adding a view

1. Implement `ui::View` as a unit struct in a new module under `src/ui/`: a
   stateless `render` over `&mut App`, a new `ViewId` variant, a footer hint
   line, and the capability methods (`feed_shown`, `navigates_articles`,
   `has_chart_panel`) that opt into the shared key handling and the
   demand-driven AlphaAI fetch. Views never fetch anything themselves.
2. Add it to `ui::VIEWS`. Order in that array defines the tab cycle and the
   `1`..`9` hotkeys; the header pills and the footer hints derive from it.

## Development

```sh
cargo test          # unit + TestBackend rendering tests
cargo clippy --all-targets
cargo fmt --all -- --check
cargo run -- --once AAPL             # network smoke test without a TTY
ALPHAI_API_KEY=ak_live_... cargo test live_api -- --ignored   # live API smoke
```

CI runs the first three on every push and pull request, on Linux, macOS
and Windows. What changed in each release is in
[CHANGELOG.md](CHANGELOG.md).

Issues and PRs are welcome.

## License

MIT. Not investment advice; data comes from third-party sources and can be
delayed or wrong. Respect the terms of the data providers you enable.
