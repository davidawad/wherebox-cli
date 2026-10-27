#!/usr/bin/env sh
# A small pantry with dated lots, for trying `eat`, `toss` and `expiring`.
# Expiry is computed from each item's shelf-life type (`wherebox shelf-life
# ls`) and where it is kept; a printed best-by date wins.
#   sh examples/pantry.sh && wherebox expiring --within 14d
set -eu
wherebox add Milk --at "Lake House / Kitchen / Fridge" --unit l --lot-qty 2 \
  --acquired 2025-12-28 --label food
wherebox lot add Milk 1 --acquired 2025-12-31
wherebox add "Chicken Thighs" --at "Lake House / Garage / Freezer" --unit lb \
  --lot-qty 3 --frozen 2025-11-15 --label food
wherebox add Sourdough --at "Lake House / Kitchen / Pantry" --best-by 2025-12-31 --label food
wherebox add "Paper Towels" --at "Lake House / Kitchen / Pantry" --qty 6 --par 4
