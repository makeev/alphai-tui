# Records assets/tmux.png, the four pane workspace in the README. Run
# `vhs tmux.tape` from the repo root with alphai-tui built in
# ./target/release (or on PATH), tmux installed and an AlphAI key
# configured. One take is four launches of one ticker each, so four price
# requests, plus the AlphAI feed each pane asks for.
#
# Panes start with --bare because the tmux status bar already carries what
# the app header would say. The README prints the same session without the
# send-keys line: there a person presses 4 and 2 themselves.
#
# Panes are addressed by direction rather than by index, because tmux
# renumbers indices by position after every split. The session runs on its
# own socket with an empty config ($T), so the shot carries the stock tmux
# status bar and not whatever the machine doing the shooting wears, and its
# status-right is replaced with a clock so the hostname stays out of a
# public image. The panes read a throwaway config holding nothing but the
# AlphAI key, for the same reason: a watchlist and a position are private
# and this shot is published.

Output assets/out.gif

Set Theme "Catppuccin Mocha"
Set FontSize 24
Set Width 2400
Set Height 1500
Set Padding 20
Set Framerate 4
Set TypingSpeed 10ms

Hide
Type "unset TMUX; clear"
Enter
Type `mkdir -p /tmp/alphaishot/alphai-tui`
Enter
Type `printf '[keys]\n' > /tmp/alphaishot/alphai-tui/config.toml`
Enter
Type `grep -m1 ^alphai.= ~/.config/alphai-tui/config.toml >> /tmp/alphaishot/alphai-tui/config.toml`
Enter
Type `export XDG_CONFIG_HOME=/tmp/alphaishot; clear`
Enter
Type "T='tmux -L alphaishot -f /dev/null'"
Enter
Type "A='alphai-tui --bare -e 900 --theme catppuccin-mocha'"
Enter
Sleep 1s
Type '$T new-session -d -s market "$A NVDA"'
Enter
Type '$T split-window -h -t market "$A AAPL"'
Enter
Type '$T select-pane -t market -L'
Enter
Type '$T split-window -v -t market "$A AVGO"'
Enter
Type '$T split-window -v -t market "$A TSLA"'
Enter

# The stock status-right prints the hostname of whatever machine shot this.
Type '$T set -t market status-right " %H:%M "'
Enter
Sleep 12s

# The three chart panes go to the chart view, the tall one to the news feed.
Type '$T send-keys -t market 4; $T select-pane -t market -U; $T send-keys -t market 4'
Enter
Type '$T select-pane -t market -U; $T send-keys -t market 4; $T select-pane -t market -R; $T send-keys -t market 2'
Enter
Sleep 2s
Type '$T attach -t market'
Enter
Sleep 10s
Show
Sleep 2s
Screenshot assets/tmux.png
Sleep 6s
Hide
Ctrl+B
Type "d"
Sleep 1s
Type '$T kill-server'
Enter
Sleep 1s
Type `rm -rf /tmp/alphaishot`
Enter
Sleep 1s
