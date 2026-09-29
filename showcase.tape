# Shoots the Ratatui showcase media: two stills and a short gif. Run
# `vhs showcase.tape` from the repo root after `cargo build --release`, with
# an AlphAI key configured. It records the binary in target/release, not an
# installed alphai-tui, so the media shows the tree it was built from.
#
# One take is one launch: one Yahoo request per ticker (the poll interval
# is parked at 900s, Yahoo rate-limits by IP) plus a handful of AlphAI
# requests for the news feed and the calendar.
#
# The showcase guide asks for 800 to 1200 pixels of width and a readable
# dark background, so the frame is 1200 wide at font size 14, which gives a
# 137 column terminal: wide enough for the News chart layout (120+). The
# app and the terminal both wear dracula.
#
# Framerate and the idle seconds after each Screenshot are load-bearing,
# see shots.tape: a still lands a few frames late otherwise.

Output assets/showcase.gif

Set Theme "Dracula"
Set FontSize 14
Set Width 1200
Set Height 680
Set Padding 12
Set Framerate 12
Set TypingSpeed 20ms

Hide
Type "clear"
Enter
# A throwaway config in a private directory: the AlphAI key and nothing
# else from the real config (a watchlist and holdings are private, and
# this media is published). umask keeps the key file 0600 from the moment
# it exists.
Type `umask 077; SHOWCASE=$(mktemp -d)`
Enter
Type `printf '[ui]\nborders = "none"\n\n[keys]\n' > $SHOWCASE/config.toml`
Enter
Type `grep -m1 ^alphai.= ~/.config/alphai-tui/config.toml >> $SHOWCASE/config.toml`
Enter
Type "clear"
Enter
Type "env -u NO_COLOR ./target/release/alphai-tui --config $SHOWCASE/config.toml --theme dracula --source yahoo --every 900 --range 5d --interval 15m NVDA AVGO AAPL MSFT META TSLA AMZN"
Enter
Sleep 12s
Show
Sleep 1s

# 1:Split, the first impression: watchlist, chart with the news marks, feed.
Screenshot assets/showcasesplit.png
Sleep 5s

# 2:News in the chart layout: the card on the right, the chart over the
# list, the selected article highlighted on the chart. Walking the list
# moves the highlight, which is what the gif is for.
Type "2"
Sleep 6s
Down
Sleep 2s
Down
Sleep 2s
Down
Sleep 3s
Screenshot assets/showcasenews.png
Sleep 5s
Down
Sleep 2s
Down
Sleep 3s

Hide
Type "q"
Type `rm -rf "$SHOWCASE"`
Enter
