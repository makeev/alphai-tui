#!/bin/sh
# Shoots the theme gallery in the README: the Split view once per color
# preset, each on a terminal wearing the matching palette, plus the
# borderless example. Run
# `./gallery.sh` from the repo root after `cargo build --release`, with an
# AlphAI key configured and vhs installed.
#
# A vhs tape sets its terminal theme once, so this writes one small tape
# per preset and runs them in turn. Every take is a launch of the app, so
# the whole gallery costs ten price requests per ticker: the ticker list
# is short on purpose, and the takes are spaced out, because Yahoo blocks
# an address after about ten quick requests. Point YAHOO_CHART_URL at a
# caching proxy to pay for each request once.
#
# The takes read a throwaway config holding nothing but the AlphAI key: a
# watchlist and holdings are private, and these images are published. The
# last take is the borderless example (`[ui] borders = "none"`), passed as
# a flag-free config line of its own.
set -eu

tmp=$(mktemp -d)
# vhs cannot parse an Output path with a dash in it, and mktemp's may have
# one, so the throwaway gif of each take lands next to the stills.
trap 'rm -rf "$tmp" assets/gallerytake.gif' EXIT
(
  umask 077
  printf '[keys]\n' >"$tmp/config.toml"
  grep -m1 '^alphai.=' ~/.config/alphai-tui/config.toml >>"$tmp/config.toml" || true
)

# preset, panel look, output name, vhs terminal theme. The ANSI default
# follows the terminal it runs in, so it gets a plain dark one rather than
# a palette's.
while read -r preset look name term; do
  cfg="$tmp/config-$look.toml"
  if [ ! -f "$cfg" ]; then
    (umask 077; printf '[ui]\nborders = "%s"\n\n' "$look" >"$cfg"; cat "$tmp/config.toml" >>"$cfg")
  fi
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
Type "env -u NO_COLOR ./target/release/alphai-tui --config $cfg --theme $preset --source yahoo --every 900 --range 5d --interval 15m NVDA AVGO AAPL MSFT META TSLA AMZN"
Enter
Sleep 10s
Show
Sleep 1s
Screenshot assets/$name.png
Sleep 5s
Hide
Type "q"
Sleep 1s
TAPE
  echo "gallery: $preset"
  vhs "$tmp/take.tape" >/dev/null
  sleep "${GALLERY_PAUSE:-60}"
done <<'LIST'
default rounded gallerydefault Builtin Dark
catppuccin-mocha rounded gallerycatppuccinmocha Catppuccin Mocha
catppuccin-macchiato rounded gallerycatppuccinmacchiato Catppuccin Macchiato
catppuccin-frappe rounded gallerycatppuccinfrappe Catppuccin Frappe
catppuccin-latte rounded gallerycatppuccinlatte Catppuccin Latte
dracula rounded gallerydracula Dracula
gruvbox-dark rounded gallerygruvboxdark GruvboxDark
gruvbox-light rounded gallerygruvboxlight Gruvbox Light
nord rounded gallerynord nord
dracula none borderless Dracula
LIST
