# Shoots every still the README uses, in one run of the app. Run
# `vhs shots.tape` from the repo root with alphai-tui built in
# ./target/release (or on PATH) and an AlphaAI key configured.
#
# One take is one launch, which is one price request per ticker (the poll
# interval is parked at 900s on purpose: Yahoo rate-limits by IP, and a
# second poll would double the bill) plus about eight AlphaAI requests for
# the feeds, the earnings read and the macro calendar.
#
# The app wears catppuccin-mocha and so does the terminal: the app paints
# foreground colors only, so the two have to agree. 2400x1300 at font size
# 28 gives a 137x37 terminal, wide enough for the side-by-side news layout
# and sharp on a retina README.
#
# Two vhs quirks are load-bearing here. A path with a dash or a slash in it
# fails to parse, so the outputs are plain names under assets/. And a
# Screenshot lands a few frames late, further behind the more pixels each
# frame costs, so the framerate is low (the gif is a throwaway) and every
# shot is followed by five idle seconds. Without them the split shot comes
# out showing the news view.

Output assets/out.gif

Set Theme "Catppuccin Mocha"
Set FontSize 28
Set Width 2400
Set Height 1300
Set Padding 20
Set Framerate 4
Set TypingSpeed 20ms

Hide
Type "clear"
Enter
Type "alphai-tui --theme catppuccin-mocha --source yahoo --every 900 NVDA AVGO AAPL MSFT META TSLA AMZN GOOGL BTC-USD"
Enter
Sleep 14s
Show
Sleep 1s

# 1:Split, the default view: watchlist and chart over the scored feed.
Screenshot assets/split.png
Sleep 5s

# 2:News for the selected ticker, list next to the full analysis card.
Type "2"
Sleep 7s
Down
Sleep 3s
Screenshot assets/news.png
Sleep 5s

# v blows the card up to full screen.
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

# 3:Table, the whole watchlist with sparklines and the extended column.
Type "3"
Sleep 3s
Screenshot assets/table.png
Sleep 5s

# 4:Chart, with E adding the pre and post market candles.
Type "4"
Sleep 3s
Type "E"
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
