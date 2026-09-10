# Records assets/demo.gif for the README. Run `vhs demo.tape` from the
# repo root with alphai-tui installed and an AlphaAI key configured; one
# take costs one price request per ticker (the poll interval is parked at
# 900s: Yahoo rate-limits by IP) and about eight AlphaAI requests.
#
# The app wears catppuccin-mocha and the terminal wears the matching
# theme: the app only paints foreground colors, so the two have to agree.
# Prices come from the keyless source, which is what a first run gets.

Output assets/demo.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1200
Set Height 700
Set Padding 12
Set Framerate 12
Set TypingSpeed 40ms

Type "alphai-tui --theme catppuccin-mocha --source yahoo --every 900 NVDA AVGO AAPL META BTC-USD"
Enter
Sleep 6s

# News: the scored list next to the full AI analysis of the story
Type "2"
Sleep 4.5s
Down
Sleep 1.5s
Down
Sleep 3s

# f cycles the scope: market-wide, with reprints collapsed
Type "f"
Sleep 4.5s
Type "f"
Sleep 3s
Type "f"
Sleep 1.5s

# Insider: a year of SEC Form 4 filings for AVGO, chart and stream
Right
Sleep 500ms
Type "5"
Sleep 4s
Type "g"
Sleep 3.5s

# Earnings: the last filing, read line by line
Type "6"
Sleep 1s
Left
Sleep 5s
PageDown
Sleep 3s

# Summary: the whole watchlist as charts at once
Type "7"
Sleep 4s

# Chart: candles with the moving averages, volume and RSI; t cycles ranges
Type "4"
Sleep 3s
Type "t"
Sleep 3.5s

# Back to the split view for the closing shot (no quit: the gif loops on
# the live dashboard, not on an empty shell)
Type "1"
Sleep 4s
