# Records assets/demo.gif for the README. Run `vhs demo.tape` from the
# repo root with alphai-tui installed and an AlphaAI key configured; one
# take costs about 6 API requests (news, sentiment, trending, insider).
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
Set TypingSpeed 40ms

Type "alphai-tui --theme catppuccin-mocha --source yahoo AAPL AVGO NVDA BTC-USD"
Enter
Sleep 5.5s

# News: scored list next to the full AI analysis card
Type "2"
Sleep 4.5s
Type "j"
Sleep 1.5s
Type "j"
Sleep 2.5s

# f cycles the scope: market-wide with collapsed reprints, then trending
Type "f"
Sleep 4s
Type "f"
Sleep 3s

# Insider: SEC Form 4 rollup above the filing stream (AVGO has the flow)
Right
Sleep 800ms
Type "5"
Sleep 5s

# Chart: candles with SMA overlays and RSI, t cycles range presets
Type "4"
Sleep 3.5s
Type "t"
Sleep 3s

# Back to the split view for the closing shot (no quit: the gif ends and
# loops on the live dashboard, not on an empty shell)
Type "1"
Sleep 4s
