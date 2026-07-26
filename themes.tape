# Records assets/themes.gif: the p key walking through the color presets
# live. Run `vhs themes.tape` from the repo root with alphai-tui installed
# and an AlphaAI key configured; one take costs about three API requests.

Output assets/themes.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1200
Set Height 700
Set Padding 12
Set TypingSpeed 40ms

Type "alphai-tui --theme catppuccin-mocha --source yahoo AVGO NVDA AAPL BTC-USD"
Enter
Sleep 6s

# p walks the presets, P walks back; the terminal keeps its own background,
# so only the app's colors move.
Type "p"
Sleep 2.5s
Type "p"
Sleep 2.5s
Type "p"
Sleep 2.5s
Type "p"
Sleep 2.5s
Type "p"
Sleep 2.5s
Type "P"
Sleep 2s
Type "P"
Sleep 3s
