#!/usr/bin/env sh
# Replay the README sample session against the synthetic data in examples/ and
# print it as a transcript: each command after a `$ ` prompt, then its real
# output. Uses whichever `wherebox` is first on PATH and a throwaway database.
#
#   scripts/readme-session.sh [full]
#
# `full` (the default and only session) is embedded in README.md and stored in
# docs/readme-session.txt. Set WHEREBOX_COLOR=always to keep ANSI colours.
# tests/readme.rs fails when docs/readme-session.txt drifts from this output;
# scripts/readme-samples.sh regenerates it, the screenshot and README.md.
set -eu

session=${1:-full}
case "$session" in
  full) ;;
  *)
    echo "usage: $0 [full]" >&2
    exit 2
    ;;
esac

repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Isolate from the caller's config and data, and pin everything that would
# otherwise vary between runs (time zone, `generated_at`).
unset WHEREBOX_CONFIG WHEREBOX_PROFILE WHEREBOX_FORMAT WHEREBOX_PRECISION \
  WHEREBOX_DATE_FORMAT WHEREBOX_EXPIRING_WITHIN WHEREBOX_NULL WHEREBOX_QUIET \
  WHEREBOX_VERBOSE NO_COLOR
export HOME="$tmp" XDG_CONFIG_HOME="$tmp/config" XDG_DATA_HOME="$tmp/data"
export WHEREBOX_DB="$tmp/wherebox.db" WHEREBOX_TZ=UTC SOURCE_DATE_EPOCH=1767225600
export WHEREBOX_COLOR="${WHEREBOX_COLOR:-never}"
# The database is encrypted; use a fixed throwaway test key from the
# environment (not the caller's SSH key).
export WHEREBOX_KEY_SOURCE=env
export WHEREBOX_KEY=raw:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
cd "$repo"

# One command per line (after a `-`). A line ending in `\` continues on the
# next one, as in an interactive shell.
first=1
replay() {
  cmd=
  while IFS= read -r line; do
    if [ -z "$cmd" ]; then
      line=${line#* }
      shown="\$ $line"
    else
      shown="$shown
$line"
    fi
    cmd="$cmd$line"
    case "$line" in *\\)
      cmd="${cmd%\\}"
      continue
      ;;
    esac
    [ "$first" = 1 ] || echo
    first=0
    printf '%s\n' "$shown"
    eval "$cmd" 2>&1 < /dev/null
    cmd=
  done
}

replay << 'EOF'
- wherebox import examples/homebox.csv
- wherebox loc ls
- wherebox add "Pressure Washer" --at Garage --price 329 --bought 2023-04-12 \
    --maker Ryobi --label tools,outdoor
- wherebox ls --label tools
- wherebox mv "Pressure Washer" --to "Lake House / Workshop"
- wherebox show washer --columns asset_id,name,location,purchase_price,labels
- wherebox history washer --columns kind,detail
- wherebox ls --warranty-before 2026-06-01 --columns asset_id,name,warranty_expires
- sh examples/pantry.sh
- wherebox eat Milk 1.5
- wherebox expiring --within 14d
- wherebox add Ribeye --preset meat.raw --at "Lake House / Kitchen / Fridge"
- wherebox shelf-life show milk --columns name,group,fridge,freezer,opened,origin,items
- wherebox par Milk 2
- wherebox shopping --checklist
- wherebox report warranties --expiring 1y --columns warranty_expires,days_left,name,location
- wherebox export --label electronics
EOF
