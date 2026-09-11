# Changelog

Notable changes in every released version, newest first. Versions follow
the crates.io releases; from 0.7.0 on each one is also a git tag and a
GitHub release with prebuilt binaries. The Homebrew tap, the AUR and the
apt repository joined later, and carry every version since.

## 0.21.0 - 2026-09-11

- The price chart marks the ticker's news on the candles it was published
  in: `▲` or `▼` for the AI sentiment call, `◆` when it is neutral, dimmer
  or brighter with the relevance score, and the freshest of them named on
  the chart's bottom border. The scored news was the one thing here no
  other terminal stock tool has, and it lived in a view of its own, so a
  chart-first reader never saw it; now the move and its reason share a
  column. `n` toggles the marks, `[chart] news_markers = false` starts
  without them, and the action is rebindable as `news_markers`.
- The marks cost no API request. They are drawn from the news bundle the
  Split and News views already keep fresh for the selected ticker, which
  also means they are absent until one of those views has loaded it.
- Rows older than the first candle on screen are dropped rather than
  pinned to the left edge, and a candle that carried several stories keeps
  the highest-scoring one, so a busy day stays readable.
- The Split view's footer drops its `s settings` hint to stay inside 110
  columns, the way the News footer already does. `?` still lists it.
- `--json` prints the one-shot quote run as a JSON array instead of a text
  table, for status bars and cron jobs: price, the move from the previous
  close, the extended print with its own move from the regular close, the
  day and 52 week ranges, volume and the source. It implies `--once`.
  Absent figures are left out rather than sent as null, a symbol that
  failed gets a row carrying its error, and warnings stay on stderr so
  stdout is a valid document.

## 0.20.0 - 2026-09-10

- `E` draws the pre and post market candles on the chart, not just the
  extended price beside it. `[chart] extended_hours = true` starts that
  way. Yahoo is the only source that can answer it: Alpaca's free IEX feed
  carries no extended-hours bars at all, and Finnhub has no candle history
  to extend.
- The day range on the quote rail now comes from the source's own figure
  for the regular session rather than being folded out of the candles,
  which stopped being the same number once the extended ones could be
  drawn.
- A Summary view (`7`): the whole watchlist as small charts at once, in a
  grid sized to the terminal. The Table view already lists every ticker,
  but a sparkline squeezed into one row answers up or down and nothing
  else; this spends real rows on each name, so one glance covers the shape
  of the session across the watchlist. Cards keep a sane height rather
  than stretching to fill a tall terminal, where the session's own jitter
  would read as noise instead of a path. `↑` `↓` move between cards and
  page the grid when the watchlist outgrows the screen.

- The watchlist is editable while the app runs. `a` opens a prompt, the
  typed ticker joins the list and is polled from the next tick; `d` removes
  the selected one. Until now the watchlist could only be set with command
  line arguments or by hand-editing the config, so following a name someone
  mentioned meant quitting first. Both keys are rebindable as `add_ticker`
  and `remove_ticker`, and both are session-only like every other runtime
  change: Save in the settings screen writes the watchlist to the config.
- The last ticker cannot be removed. Every view is scoped to a selected
  ticker, so an empty watchlist needs empty states before it can be
  reached.

- Extended-hours prices. After the closing bell the quote rail carries the
  late print as its own zone, labelled `AH` (or `PRE` before the open), with
  the move measured against the close the way a broker screen reads it. The
  headline price stays the regular close, so the two facts no longer
  overwrite each other: on 9 September AAPL closed down 0.28% and traded up
  0.68% after hours, and only the first of those was visible. This is the
  half that was missing from the news views, where a filing lands at 20:00
  and the price beside it could not move until the next morning. The
  watchlist table grows an `Ext Δ%` column when any row has a late print,
  and gives the width back during the session, when there is nothing to
  show.
- The year's range and the day's volume at the end of the rail, on sources
  that report them. Like every other zone they are dropped first as the
  terminal narrows.
- All of it rides in the request each source already makes, so a poll still
  costs exactly what it did.
- Sources differ in what they can answer, and now say so rather than
  guessing. Yahoo reports all three. Alpaca's free IEX feed is one exchange
  rather than the whole tape: it has no extended-hours prints, and its
  share count is a few percent of the day's volume, so that figure is no
  longer shown as the volume (it returns on `ALPACA_FEED=sip`). Finnhub's
  quote endpoint is regular session only.

## 0.17.2 - 2026-09-10

- Prices no longer drop out when the source refuses a request. Alpaca's data
  edge turns down roughly one request in seven with a bare "too many
  requests", and it does so no matter how slowly you ask: twenty requests
  sent as fast as the socket allowed were refused twice, and the same twenty
  paced at two per second were refused four times. Nothing was over any
  published limit. One refusal used to be enough to leave a ticker showing an
  error until the next poll, and to make `--once` print nothing but the
  error; watching two symbols failed that way half the time. A refused
  request is now retried, briefly and up to twice, which brings the same
  check to twenty nine runs in thirty. Gateway errors are retried the same
  way. A wrong key, an unknown symbol and a source that is genuinely down
  still answer immediately, as before.

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
