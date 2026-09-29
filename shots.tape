# Shoots every still the README uses, in one run of the app. Run
# `vhs shots.tape` from the repo root after `cargo build --release`, with
# an AlphAI key configured.
#
# One take is one launch, which is one price request per ticker (the poll
# interval is parked at 900s on purpose: Yahoo rate-limits by IP, and a
# second poll would double the bill) plus about eighteen AlphAI requests
# for the feeds, the earnings reads and the macro calendar. The company
# dates are fetched one at a time while Calendar is open.
#
# The app wears dracula and so does the terminal: the gutters between the
# tinted panels are the terminal's own background, so the two have to
# agree. 2400x1300 at font size 28 gives a 137x37 terminal, wide enough for
# the chart-over-list news layout (120 columns) and sharp on a retina
# README. The window is 5d / 15m rather than daily bars: news marks and
# the pre-market and after-hours shading need intraday candles.
#
# Two vhs quirks are load-bearing here. A path with a dash or a slash in it
# fails to parse, so the outputs are plain names under assets/. And a
# Screenshot lands a few frames late, further behind the more pixels each
# frame costs, so the framerate is low (the gif is a throwaway) and every
# shot is followed by five idle seconds. Without them the split shot comes
# out showing the news view.

Output assets/out.gif

Set Theme "Dracula"
Set FontSize 28
Set Width 2400
Set Height 1300
Set Padding 20
Set Framerate 4
Set TypingSpeed 20ms

Hide
# The throwaway config below holds an API key: keep it private from the
# moment it exists.
Type "umask 077"
Enter
Type "clear"
Enter
# The Portfolio view (and the two extra columns in the table) need
# holdings, which live in the config. The take builds a throwaway one out
# of the AlphAI key and three invented holdings, and nothing else: a real
# watchlist and real positions are private, and these shots are published.
# Every ticker here is already on the watchlist, so they cost no extra
# requests.
Type `printf '[ui]\nborders = "none"\n\n[keys]\n' > /tmp/alphaishot.toml`
Enter
Type `grep -m1 ^alphai.= ~/.config/alphai-tui/config.toml >> /tmp/alphaishot.toml`
Enter
Type `printf '\n[[positions]]\nsymbol = "NVDA"\nqty = 120\navg_price = 118.40\n\n[[positions]]\nsymbol = "AAPL"\nqty = 40\navg_price = 232.10\n\n[[positions]]\nsymbol = "MSFT"\nqty = 12\navg_price = 501.30\n' >> /tmp/alphaishot.toml`
Enter
Type `clear`
Enter
Type "env -u NO_COLOR ./target/release/alphai-tui --config /tmp/alphaishot.toml --theme dracula --source yahoo --every 900 --range 5d --interval 15m NVDA AVGO AAPL MSFT META TSLA AMZN GOOGL MU COST BTC-USD"
Enter
Sleep 14s
Show
Sleep 1s

# 1:Split, the default view: the chart across the top, the watchlist and
# the scored feed side by side under it.
Screenshot assets/split.png
Sleep 5s

# 2:News for the selected ticker: the chart over the list, the full
# analysis card beside them, the selected story marked on the chart.
Type "2"
Sleep 7s
Down
Sleep 3s
Screenshot assets/news.png
Sleep 5s

# v reads the story in place: the chart stays, the card takes the list's
# room.
Type "v"
Sleep 3s
Screenshot assets/article.png
Sleep 5s
Escape
Sleep 2s

# f cycles the scope: the whole market with reprints collapsed, then the
# 48-hour trending top 10, then back to the ticker.
Type "f"
Sleep 7s
Screenshot assets/market.png
Sleep 5s
Type "f"
Sleep 6s
Screenshot assets/trending.png
Sleep 5s
Type "f"
Sleep 3s

# 3:Table, the whole watchlist with sparklines, the extended column and,
# for the three held tickers, what they are worth and what they made.
Type "3"
Sleep 3s
Screenshot assets/table.png
Sleep 5s

# 4:Chart, 15-minute candles across the pre-market, regular and
# after-hours sessions, with both moving averages, volume and RSI. The
# visible bars keep their interval, even if the window is wider than the
# terminal. Extended hours are enabled by default.
Type "4"
Sleep 4s
Screenshot assets/chart.png
Sleep 5s

# 5:Insider, on AVGO, which files Form 4s at a steady clip. g opens the
# window to twelve months and - drops the trade-size filter, which is what
# a year of one company's filings actually looks like.
Type "5"
Sleep 2s
Right
Sleep 8s
Type "g"
Sleep 3s
Type "-"
Sleep 6s
Screenshot assets/insider.png
Sleep 5s

# 6:Earnings, back on NVDA.
Type "6"
Sleep 2s
Left
Sleep 8s
Screenshot assets/earnings.png
Sleep 5s

# 7:Summary, the whole watchlist as charts at once.
Type "7"
Sleep 4s
Screenshot assets/summary.png
Sleep 5s

# 8:Portfolio, the holdings from the throwaway config above.
Type "8"
Sleep 3s
Screenshot assets/portfolio.png
Sleep 5s

# 9:Calendar, US macro releases beside confirmed watchlist report dates.
# Allow the paced date sweep to finish; the NVDA read above is reused.
Type "9"
Sleep 50s
Screenshot assets/calendar.png
Sleep 5s

# The help overlay, every action with the keys it is bound to.
Type "?"
Sleep 3s
Screenshot assets/help.png
Sleep 5s
Escape
Sleep 1s

Hide
Type "q"
Sleep 2s
Type `rm -f /tmp/alphaishot.toml`
Enter
Sleep 1s
