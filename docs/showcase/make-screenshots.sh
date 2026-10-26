#!/bin/sh
# Regenerate docs/showcase/*.png from the synthetic data in examples/.
# Needs: wherebox on PATH (cargo install --path .), ImageMagick (magick),
# and a monospace font with box-drawing glyphs (FONT, default macOS Menlo.ttc;
# on Linux try FONT=/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf). Never reads a real profile: it
# runs in a throwaway HOME with a throwaway test key.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
out="$root/docs/showcase"
font=${FONT:-/System/Library/Fonts/Menlo.ttc}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

w() {
  env -i HOME="$tmp" XDG_CONFIG_HOME="$tmp/config" XDG_DATA_HOME="$tmp/data" \
    WHEREBOX_DB="$tmp/demo.db" WHEREBOX_TZ=UTC WHEREBOX_KEY_SOURCE=env \
    WHEREBOX_KEY=raw:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f \
    NO_COLOR=1 SOURCE_DATE_EPOCH=1767225600 PATH="$PATH" wherebox "$@"
}

w import "$root/examples/homebox.csv" > /dev/null
{
  echo '$ wherebox loc ls'
  w loc ls
  echo
  echo '$ wherebox ls --columns name,location,value'
  w ls --columns name,location,value
} | magick -background '#1e1e2e' -fill '#cdd6f4' -font "$font" -pointsize 18 \
  label:@- -bordercolor '#1e1e2e' -border 24 -strip PNG8:"$out/tour.png"
