# Kind presets

A preset says what a kind of item tracks, which defaults it fills in, how
early to warn, and where each default comes from. `wherebox preset ls` lists
them; `wherebox preset show NAME` prints one with its sources.

```sh
wherebox add Ribeye --preset meat.raw --at "Kitchen / Fridge"
# raw-beef (name): expires 2026-01-04 (fridge 3 days from 2026-01-01 (acquired); ...)
wherebox lot add Ribeye 2 --at "Garage / Chest Freezer"     # expires in 4 months
```

## What a preset fills in

* **A default shelf-life type** (`shelf_life_type`). Presets store no lot
  dates: expiry is computed from the item's shelf-life type and where each
  lot is kept (docs/SPEC.md, "Shelf life"; `wherebox shelf-life ls`). A
  food preset's type applies when the item's name does not classify it and
  it has no `--shelf-life`; `add --preset` with such a preset starts a lot
  (acquired today unless `--acquired`). Non-food presets (household,
  toiletry, medicine, supplement, tools...) are never perishable.
* **Service dates** (`[intervals]`) as custom fields, counted from the item's
  `Installed` field, else its purchase date: `Replace By` (not rolled
  forward, so an overdue replacement shows as past), `Next Inspection`,
  `Next Service` and `Reappraise By` (the first one on or after today). A
  `warranty` term sets `warranty_expires`. Nothing is guessed without a base
  date.
* **Defaults** (`[defaults]`): `unit`, `labels`, custom `fields` for new items.
* **Alerts**: `alert_days` widens `wherebox expiring` for the preset's items
  unless `--within` is given (only perishables expire, so this matters for
  food presets).
* **Cadence**: a `[cadence]` table in `config.toml` (`household = "60d"`,
  by preset name or category) sets how long a non-perishable lot is
  presumed on hand when the item has no learned reorder cadence.
* **Reports**: presets with category `food` or `consumable` are left out of
  the insurance report; presets that track `serial` make a missing serial
  number a gap.

## Sources

Every value a built-in preset sets is covered by a `[[sources]]` entry (a
unit test checks this). Values without a citable source are left unset and
explained in `notes`.

| preset | value | source |
|---|---|---|
| food (`meat.raw`, `meat.ground`, `poultry.raw`, `seafood.raw`, `eggs`, `bread`, `leftovers`, `frozen`, `condiment`) | `shelf_life_type` | the figures are the type's: `shelf-life/types.toml` cites USDA FSIS FoodKeeper and the FDA / FoodSafety.gov Cold Food Storage Chart per group (`wherebox shelf-life show TYPE`) |
| `frozen`, `condiment` (notes) | frozen food safe indefinitely; opened mayonnaise 2 months | USDA FSIS Cold Food Storage Chart (<https://www.foodsafety.gov/food-safety-charts/cold-food-storage-charts>) |
| `smoke-detector` | replace every 10 years, test monthly | NFPA (<https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms>) |
| `fire-extinguisher` | inspect monthly, maintain yearly | NFPA 10 (<https://www.nfpa.org/codes-and-standards/nfpa-10-standard-development/10>) |
| `filter` | check monthly, replace at least every 3 months (HVAC air filters) | ENERGY STAR Maintenance Checklist (<https://www.energystar.gov/saveathome/heating-cooling/maintenance-checklist>) |
| `hvac` | yearly professional check-up | ENERGY STAR Maintenance Checklist |
| `medicine`, `supplement`, `first-aid` (30), `document` (90), `appliance`, `electronics`, `tool`, `furniture` (90) | `alert_days` | wherebox policy (docs/SPEC.md), not an external fact |

No type for `dairy`, `produce`, `beverage`, `alcohol` and `pantry` (one
figure cannot cover them: dairy and produce items classify by name; dry and
canned goods never expire),
CO alarm replacement (maker's schedule), vehicle/generator/water-heater
service, warranty terms, and reappraisal intervals for valuables.

## File format

Built-in presets live in `presets/kinds/NAME.toml` and are compiled in. A
file in `<config dir>/presets/kinds/` (next to `config.toml`) with the same
`name` replaces a built-in preset; other names add presets.

```toml
name = "meat.raw"                 # lowercase letters, digits, '.', '-', '_'
description = "Raw beef, veal, lamb and pork"
category = "food"                 # food consumable safety durable valuable document other
tracks = ["use_by", "frozen_at"]  # see below
shelf_life_type = "raw-beef"      # optional (food): see `wherebox shelf-life ls`
alert_days = 2                    # optional
notes = "free text"               # optional

[intervals]                       # optional
replace = "10y"
inspect = "1m"
service = "1y"
appraise = "3y"
warranty = "2y"

[defaults]                        # optional
unit = "lb"
labels = ["food"]
fields = { Hazmat = "yes" }

[[sources]]                       # one per source; `covers` names the values it backs
covers = "replace"
cite = "NFPA: replace smoke alarms every 10 years"
url = "https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms"
```

A `[shelf_life]` table (preset shelf-life figures before schema v3) is
accepted and ignored.

`tracks` is descriptive (shown by `preset show`, used for `serial` in the
insurance report): `use_by best_by opened_at frozen_at expiration
installed_at replace inspection service warranty serial appraisal
replacement_value registration shelf_life hazmat care par_level`.
