# Changelog

Notable changes in every released version, newest first. Versions follow
the crates.io releases; from 0.7.0 on each one is also a git tag and a
GitHub release with prebuilt binaries. The Homebrew tap, the AUR and the
apt repository joined later, and carry every version since.

## 0.17.1 - 2026-09-08

- The first refresh of a feed no longer lifts an old article to the top of it.
  That first check asks the server for the newest rows by arrival, and a page
  cut that way ends at a different article than the page ordered by
  publication, so the two disagree at the bottom edge. What sits below that
  edge is old news the first page cut off, and it was landing above everything
  else: a ticker feed whose twenty rows spanned two weeks showed a twelve day
  old article above one published an hour ago. Those rows now stay out, and
  paging down still reaches them in their own place. Later refreshes are
  unchanged, so an article that reaches the feed behind its own publish time
  still goes to the top carrying the new row marker, which is the reason to
  refresh by arrival at all.

## 0.17.0 - 2026-09-06

- Bare mode: `z` hides the header and the footer and gives both rows to the
  view, `--bare` starts that way and `[ui] bare = true` makes it the
  default. A tmux pane carries its own status bar and its own idea of which
  window it is, so the app's chrome is two rows it can put to better use.
  The quote rail stays, so a bare pane still names its ticker and its price,
  and the frame titles still name the view.
- The `[keybindings]` example in the README moves off `z`, which now has a
  default binding of its own.

## 0.16.0 - 2026-09-05

- New quote rail: one line under the tabs, in every view, with the selected
  ticker's price and change, where that price sits between the session low
  and high, the state of the US market and the rest of the watchlist as
  percentages. Until now only the Split, Table and Chart views showed a
  price at all, so in News, Insider and Earnings the ticker was a name in a
  frame title, moving between tickers was a blind jump, and the pulse that
  marks a fresh price was invisible. The line drops its parts one at a time
  as the terminal narrows and keeps the symbol and the price to the last;
  `[ui] quote_rail = false` turns it off, and a terminal under 12 rows
  keeps the row for the view.
- The rail says what the US market is doing (pre, live, post or closed) and
  how long until the next bell, holidays and Good Friday included, so a
  price that has not moved in hours reads as a closed market rather than as
  a broken feed. Crypto is marked as trading around the clock instead.
- A source whose prices are delayed (Yahoo, or Alpaca on
  `ALPACA_FEED=delayed_sip`) now says so on the rail.
- The app checks the poll budget: a watchlist and interval that together
  exceed the data plan's requests a minute warn on startup and under the
  interval in the settings screen, naming an interval that fits. Going over
  the ceiling turned tickers into `error` rows with nothing explaining why.

## 0.15.0 - 2026-09-02

- New Earnings view (`6`): AlphaAI's structured read of the selected
  ticker's own earnings filing. The verdict and the reason for it, the
  metric table with prior quarter, prior year and both changes, segments,
  the outlook, concerns, what to watch and the analysis. Every figure was
  checked against the filing text before publication and prints as the
  filing wrote it, with units shortened and nothing rounded into a new
  number. American 8-K item 2.02 filings and foreign private issuers' 6-K
  releases are both covered, and older quarters continue below the newest
  read. The metric table is banded row by row and led by dots from each
  name across to its figures, and it is only as wide as its own content,
  so a wide terminal does not leave half a screen between a name and its
  numbers. Paragraphs stop at a readable width for the same reason.
- A ticker with no read yet shows the date of its next report when the
  company has confirmed one, instead of an empty screen.
- The bottom line of the view carries the next US macro releases (CPI, the
  jobs report, FOMC decisions and the rest), with estimated dates named as
  estimated.
- News feed: the earnings filing itself is now marked `8-K` or `6-K` in
  place of its category, which tells it apart from the coverage around it,
  and its article card carries a short form of the read once the Earnings
  view has loaded one. The card still makes no requests of its own.
- New `--earnings TICKER` flag prints the latest read to stdout and exits,
  for a pipe or a tmux pane.
- The view tabs in the header shrink to their hotkeys on narrow terminals,
  so the clock and the chart interval stay visible.
- Budget: one request per ticker while the Earnings view is on screen,
  cached for an hour, plus one shared macro-calendar request every six
  hours. Both are silent about failures that no retry would fix.

## 0.14.0 - 2026-08-26

- Feeds now poll for arrivals instead of refetching the published head of
  the list, so articles that reach the feed after they were published (a
  Form 4 lands days after its trade) are no longer missed. New rows are
  merged at the top of the list and carry the unseen marker.
- Polling also runs while you are scrolled down the feed. Until now a
  reader who had moved off the first row got no updates at all.
- A failed poll leaves the feed on screen with a note under the list
  instead of replacing the view with an error.
- The request budget is unchanged: one request per cache lifetime per
  visible feed.

## 0.13.0 - 2026-08-11

- Insider view: a chart panel of Form 4 events with weekly dollar bars,
  `g` cycles the window between 3 months, 12 months and off. Sales,
  purchases and buybacks get their own markers, 10b5-1 trades are dimmed.
- The selected filing is highlighted on the chart, and the detail pane
  gains the stake change, the tranche count and a late filing flag.
- The view header is now a 12 month rollup. The panel rides along with
  the feed request, so it costs no extra API calls.

## 0.12.0 - 2026-08-03

- Volume bars under the price chart, `b` toggles the panel.
- `e` switches the two moving averages between simple and exponential.

## 0.11.0 - 2026-07-26

- Nine named color presets, `--theme` at startup, `p` and `P` to cycle
  them live, and a row in the settings form.
- Rounded panel frames and a themed border color slot.
- Fix: on narrow terminals the watchlist drops columns instead of
  squeezing all of them.
- Fix: the left arrow steps back through a settings row's choices.
- Packaging: apt repository for Debian and Ubuntu.

## 0.10.2 - 2026-07-25

- Fix: feeds page at 20 rows, the widest page every AlphaAI plan allows.
  The window was half that, because it used the server default.
- Packaging: AUR package `alphai-tui-bin`, published from CI.

## 0.10.1 - 2026-07-23

- Fix: the live quote is folded into the newest candle, so the chart and
  the price in the header no longer disagree between candle closes.

## 0.10.0 - 2026-07-22

- The chart keeps a margin to the right of the last candle with a line
  and a tag at the live price. `[chart] right_margin_pct` sizes it.
- A price change flashes in the header, the price tag and the quotes
  table.
- The poll interval is editable in the settings form and applies live.

## 0.9.0 - 2026-07-21

- `[keybindings]` config section: every action can be remapped, with
  named keys and modifiers. A bad binding is a warning, not a failure.
- `?` opens a help overlay listing every action with its current keys and
  the name to use in the config file.
- Packaging: releases publish to crates.io and the Homebrew tap on their
  own, without a manual step.

## 0.8.0 - 2026-07-12

- Rows that appeared since you last looked at a feed carry a `●` marker
  that goes out once the cursor rests on them.
- `[ui] alphai_ttl_secs` sets how long news, sentiment and insider data
  stay cached.

## 0.7.0 - 2026-07-11

- `+` and `-` set the score filter of the visible feed live: relevance
  for news, trade size for insider rows. `[ui] news_min_score` and
  `[ui] insider_min_score` set the startup values.
- Insider rows read the trade from the structured Form 4 fields, so the
  side, the 10b5-1 flag and the filer's title come from the filing
  instead of the headline.
- Moving averages and reference lines draw as continuous braille lines
  over the candles.
- `[theme]` colors every semantic slot, `[ui]` and `[chart]` set startup
  defaults, and `--config PATH` picks a different config file.
- Prebuilt binaries for five platforms on every release, and `--version`.

## 0.5.0 - 2026-07-10

First public release.

- Views: quotes table, chart, news, insider and a split dashboard.
- Prices from Yahoo without a key, or from Finnhub or Alpaca with one.
- Candlestick or line chart with SMA 20 and 100, an RSI(14) panel and
  interval presets, drawn with enough warm-up history for the overlays
  to be correct from the first visible candle.
- AlphaAI news with the full analysis card, market and trending scopes,
  paging, sentiment and SEC Form 4 insider activity.
- In-app settings, a config file holding keys and the watchlist.
