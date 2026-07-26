#!/usr/bin/env bash
# Shoots one screenshot per Catppuccin flavor, all framed identically, and
# stitches them into the layered preview the Catppuccin port catalog asks
# for. Run from the repo root with alphai-tui on PATH (or built in
# ./target/release) and an AlphaAI key configured:
#
#   ./shoot-themes.sh              # four shots plus assets/themes.webp
#   ./shoot-themes.sh --no-preview # shots only, skip catwalk
#
# Needs vhs (brew install vhs) and, for the preview, catwalk
# (cargo install catppuccin-catwalk).
#
# One take costs about three AlphaAI requests per flavor, so twelve in
# total, inside the free 20/min budget. The four runs go back to back on
# purpose: the news feed is live, and shots taken minutes apart would
# disagree with each other in the layered preview.
set -euo pipefail

cd "$(dirname "$0")"
out=assets/themes
mkdir -p "$out"

# The frame: the Split view, which puts the most of the palette on one
# screen (candles, sparklines, colored deltas, the RSI panel and a scored
# news feed) on a ticker with a dense feed.
symbols="AVGO NVDA AAPL BTC-USD"

# Flavor -> the matching terminal theme built into vhs. The terminal has
# to wear the same flavor as the app: the app never paints a background.
shoot() {
  local flavor="$1" vhs_theme="$2"
  # vhs quirks worth keeping: a Screenshot that shares its stem with
  # Output is silently dropped, and a dash in the Output name breaks the
  # screenshot too. Hence the fixed, boring out.gif.
  cat > "$out/tape" <<EOF
Output $out/out.gif

Set Theme "$vhs_theme"
Set FontSize 15
Set Width 1500
Set Height 800
Set Padding 16
Set TypingSpeed 30ms

Type "alphai-tui --theme catppuccin-$flavor --source yahoo $symbols"
Enter
Sleep 9s
Screenshot $out/shot-$flavor.png
EOF
  vhs "$out/tape"
  echo "  $out/shot-$flavor.png"
}

export PATH="$PWD/target/release:$PATH"
echo "shooting four flavors, about a minute:"
shoot latte "Catppuccin Latte"
shoot frappe "Catppuccin Frappe"
shoot macchiato "Catppuccin Macchiato"
shoot mocha "Catppuccin Mocha"
rm -f "$out/tape" "$out/out.gif"

if [ "${1:-}" = "--no-preview" ]; then
  exit 0
fi

# Argument order is fixed by catwalk: latte, frappe, macchiato, mocha.
catwalk "$out/shot-latte.png" "$out/shot-frappe.png" \
        "$out/shot-macchiato.png" "$out/shot-mocha.png" \
        --ext png --output assets/themes.webp
echo "preview: assets/themes.webp"
