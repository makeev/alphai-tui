# Changelog

Notable changes in every released version, newest first. Versions follow
the crates.io releases; from 0.7.0 on each one is also a git tag and a
GitHub release with prebuilt binaries. The Homebrew tap, the AUR and the
apt repository joined later, and carry every version since.

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
