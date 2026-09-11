# Records assets/themes.gif: the } key walking through the color presets
# live. Run `vhs themes.tape` from the repo root with alphai-tui installed
# and an AlphaAI key configured; one take costs one price request per ticker
# (the poll interval is parked at 900s: Yahoo rate-limits by IP) and a
# couple of AlphaAI requests. It runs against a throwaway config carrying
# nothing but the key: a watchlist and a position are private, and this
# recording is published.

Output assets/themes.gif

Set Theme "Catppuccin Mocha"
Set FontSize 14
Set Width 1200
Set Height 700
Set Padding 12
Set Framerate 12
Set TypingSpeed 40ms

Hide
Type `mkdir -p /tmp/alphaishot/alphai-tui`
Enter
Type `printf '[keys]\n' > /tmp/alphaishot/alphai-tui/config.toml`
Enter
Type `grep -m1 ^alphai.= ~/.config/alphai-tui/config.toml >> /tmp/alphaishot/alphai-tui/config.toml`
Enter
Type `export XDG_CONFIG_HOME=/tmp/alphaishot; clear`
Enter
Show
Sleep 500ms
Type "alphai-tui --theme catppuccin-mocha --source yahoo --every 900 NVDA AVGO AAPL META BTC-USD"
Enter
Sleep 6s

# } walks the presets, { walks back; the terminal keeps its own background,
# so only the app's colors move. (These were p and P until 0.22.0, when p
# became the position prompt.)
Type "}"
Sleep 2.5s
Type "}"
Sleep 2.5s
Type "}"
Sleep 2.5s
Type "}"
Sleep 2.5s
Type "}"
Sleep 2.5s
Type "{"
Sleep 2s
Type "{"
Sleep 3s

# Off camera: quit and take the throwaway config away again.
Hide
Type "q"
Sleep 1s
Type `rm -rf /tmp/alphaishot`
Enter
Sleep 1s
