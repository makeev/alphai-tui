# Records assets/demo.gif for the README. Run `vhs demo.tape` from the
# repo root with alphai-tui installed and an AlphAI key configured; one
# take costs one price request per ticker (the poll interval is parked at
# 900s: Yahoo rate-limits by IP) and about eight AlphAI requests.
#
# The app wears catppuccin-mocha and the terminal wears the matching
# theme: the app only paints foreground colors, so the two have to agree.
# Prices come from the keyless source, which is what a first run gets.
#
# The Portfolio scene needs holdings, and those live in the config. The
# take builds a throwaway one (the AlphAI key, lifted out of the real
# config, and three invented holdings) and points XDG_CONFIG_HOME at it.
# Nothing else is copied on purpose: a watchlist and real positions are
# private, and this recording ends up in a public README.

Output assets/demo.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1200
Set Height 700
Set Padding 12
Set Framerate 12
Set TypingSpeed 40ms

Hide
Type `mkdir -p /tmp/alphaidemo/alphai-tui`
Enter
Type `printf '[keys]\n' > /tmp/alphaidemo/alphai-tui/config.toml`
Enter
Type `grep -m1 ^alphai.= ~/.config/alphai-tui/config.toml >> /tmp/alphaidemo/alphai-tui/config.toml`
Enter
Type `printf '\n[[positions]]\nsymbol = "NVDA"\nqty = 120\navg_price = 118.40\n\n[[positions]]\nsymbol = "AAPL"\nqty = 40\navg_price = 232.10\n\n[[positions]]\nsymbol = "META"\nqty = 15\navg_price = 702.80\n' >> /tmp/alphaidemo/alphai-tui/config.toml`
Enter
Type `export XDG_CONFIG_HOME=/tmp/alphaidemo; clear`
Enter
Show
Sleep 500ms

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

# Portfolio: the holdings, what they are worth and what they made
Type "8"
Sleep 4.5s

# Chart: candles with the moving averages, volume and RSI; t cycles ranges
Type "4"
Sleep 3s
Type "t"
Sleep 3.5s

# Back to the split view for the closing shot (no quit: the gif loops on
# the live dashboard, not on an empty shell)
Type "1"
Sleep 4s

# Off camera: quit and take the throwaway config away again.
Hide
Type "q"
Sleep 1s
Type `rm -rf /tmp/alphaidemo`
Enter
Sleep 1s
