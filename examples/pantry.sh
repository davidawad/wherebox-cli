#!/usr/bin/env sh
# A small pantry with dated lots, for trying `eat`, `toss` and `expiring`.
#   sh examples/pantry.sh && wherebox expiring --within 14d
set -eu
wherebox add Milk --at "Lake House / Kitchen / Fridge" --unit l --lot-qty 2 \
  --acquired 2025-12-28 --expires 2026-01-04 --label food
wherebox lot add Milk 1 --acquired 2025-12-31 --expires 2026-01-09
wherebox add "Chicken Thighs" --at "Lake House / Garage / Freezer" --unit lb \
  --lot-qty 3 --frozen 2025-11-15 --best-by 2026-02-15 --label food
wherebox add Sourdough --at "Lake House / Kitchen / Pantry" --best-by 2025-12-31 --label food
wherebox add "Paper Towels" --at "Lake House / Kitchen / Pantry" --qty 6 --par 4
