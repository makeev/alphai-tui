#!/bin/sh
# Shoots the theme gallery in the README: the Split view once per color
# preset, each on a terminal wearing the matching palette. Run
# `./gallery.sh` from the repo root after `cargo build --release`, with an
# AlphAI key configured and vhs installed.
#
# A vhs tape sets its terminal theme once, so this writes one small tape
# per preset and runs them in turn. Every take is a launch of the app, so
# the whole gallery costs nine price requests per ticker: the ticker list
# is short on purpose, and the takes are spaced out, because Yahoo blocks
# an address after about ten quick requests. Point YAHOO_CHART_URL at a
# caching proxy to pay for each request once.
#
# The takes read a throwaway config holding the AlphAI key and the panel
# look, nothing else: a watchlist and holdings are private, and these
# images are published.
set -eu

tmp=$(mktemp -d)
# vhs cannot parse an Output path with a dash in it, and mktemp's may have
# one, so the throwaway gif of each take lands next to the stills.
trap 'rm -rf "$tmp" assets/gallerytake.gif' EXIT
(
  umask 077
  printf '[ui]\nborders = "none"\n\n[keys]\n' >"$tmp/config.toml"
  grep -m1 '^alphai.=' ~/.config/alphai-tui/config.toml >>"$tmp/config.toml" || true
)

# preset, vhs terminal theme. The ANSI default follows the terminal it
# runs in, so it gets a plain dark one rather than a palette's.
while read -r preset term; do
  name=$(printf '%s' "$preset" | tr -d -)
  cat >"$tmp/take.tape" <<TAPE
Output assets/gallerytake.gif
Set Theme "$term"
Set FontSize 14
Set Width 1200
Set Height 680
Set Padding 12
Set Framerate 4
Hide
Type "clear"
Enter
Type "env -u NO_COLOR ./target/release/alphai-tui --config $tmp/config.toml --theme $preset --source yahoo --every 900 --range 5d --interval 15m NVDA AVGO AAPL MSFT META TSLA AMZN"
Enter
Sleep 10s
Show
Sleep 1s
Screenshot assets/gallery$name.png
Sleep 5s
Hide
Type "q"
Sleep 1s
TAPE
  echo "gallery: $preset"
  vhs "$tmp/take.tape" >/dev/null
  sleep "${GALLERY_PAUSE:-60}"
done <<'LIST'
default Builtin Dark
catppuccin-mocha Catppuccin Mocha
catppuccin-macchiato Catppuccin Macchiato
catppuccin-frappe Catppuccin Frappe
catppuccin-latte Catppuccin Latte
dracula Dracula
gruvbox-dark GruvboxDark
gruvbox-light Gruvbox Light
nord nord
LIST
