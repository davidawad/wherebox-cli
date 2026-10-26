# Kind presets

A preset says what a kind of item tracks, which defaults it fills in, how
early to warn, and where each default comes from. `wherebox preset ls` lists
them; `wherebox preset show NAME` prints one with its sources.

```sh
wherebox add Ribeye --preset meat.raw --at "Kitchen / Fridge"
# meat.raw: use by 2026-01-04 (3-5 days in the fridge from purchase on 2026-01-01)
wherebox lot add Ribeye 2 --at "Garage / Chest Freezer"     # use by +4 months, frozen today
```

## What a preset fills in

* **Lot dates** (`[shelf_life]`). `add --preset` with a shelf-life preset
  starts a lot (acquired today unless `--acquired`); `lot add` dates new lots
  of an item with a preset. Where the lot is kept decides the figure: the
  nearest segment of the lot's location (else the item's) named like a
  freezer (`Freezer`, `Chest Freezer`, `Freezer drawer`), a fridge (`Fridge`,
  `Refrigerator`) or a pantry (`Pantry`, `Cupboard`, `Larder`). An unknown
  place uses the fridge figure, else the pantry, else the freezer. In the
  freezer the clock starts at `frozen_at` (set to the acquired date when
  missing). An opened lot is capped by the `opened` figure. A range such as
  `3-5d` fills with its shorter end. Dates given on the command line win.
* **Service dates** (`[intervals]`) as custom fields, counted from the item's
  `Installed` field, else its purchase date: `Replace By` (not rolled
  forward, so an overdue replacement shows as past), `Next Inspection`,
  `Next Service` and `Reappraise By` (the first one on or after today). A
  `warranty` term sets `warranty_expires`. Nothing is guessed without a base
  date.
* **Defaults** (`[defaults]`): `unit`, `labels`, custom `fields` for new items.
* **Alerts**: `alert_days` widens `wherebox expiring` for the preset's items
  (medicine 30 days, documents 90) unless `--within` is given.
* **Reports**: presets with category `food` or `consumable` are left out of
  the insurance report; presets that track `serial` make a missing serial
  number a gap.

## Sources

Every value a built-in preset sets is covered by a `[[sources]]` entry (a
unit test checks this). Values without a citable source are left unset and
explained in `notes`.

| preset | value | source |
|---|---|---|
| food (`meat.raw`, `meat.ground`, `poultry.raw`, `seafood.raw`, `eggs`, `bread`, `leftovers`, `frozen`, `pantry`, `condiment`) | fridge, freezer and pantry shelf life | USDA FSIS FoodKeeper data, version 128 (<https://catalog.data.gov/dataset/fsis-foodkeeper-data>); each preset names the FoodKeeper product ids it uses |
| `frozen`, `condiment` (notes) | frozen food safe indefinitely; opened mayonnaise 2 months | USDA FSIS Cold Food Storage Chart (<https://www.foodsafety.gov/food-safety-charts/cold-food-storage-charts>) |
| `smoke-detector` | replace every 10 years, test monthly | NFPA (<https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms>) |
| `fire-extinguisher` | inspect monthly, maintain yearly | NFPA 10 (<https://www.nfpa.org/codes-and-standards/nfpa-10-standard-development/10>) |
| `filter` | check monthly, replace at least every 3 months (HVAC air filters) | ENERGY STAR Maintenance Checklist (<https://www.energystar.gov/saveathome/heating-cooling/maintenance-checklist>) |
| `hvac` | yearly professional check-up | ENERGY STAR Maintenance Checklist |
| `medicine`, `supplement`, `first-aid` (30), `document` (90), `appliance`, `electronics`, `tool`, `furniture` (90) | `alert_days` | wherebox policy (docs/SPEC.md), not an external fact |

Left unset for lack of a single citable figure: dairy, produce, beverage and
alcohol shelf lives (FoodKeeper varies by product; the notes quote examples),
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
alert_days = 2                    # optional
notes = "free text"               # optional

[shelf_life]                      # optional
date = "use_by"                   # which lot date to fill: use_by (expires_at) or best_by
fridge = "3-5d"                   # d, w, m, y; a range fills with its shorter end
freezer = "4-12m"
pantry = "1y"
opened = "2m"                     # after opening

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
covers = "fridge, freezer"
cite = "USDA FSIS FoodKeeper data, version 128: Beef steaks (#40)"
url = "https://catalog.data.gov/dataset/fsis-foodkeeper-data"
```

`tracks` is descriptive (shown by `preset show`, used for `serial` in the
insurance report): `use_by best_by opened_at frozen_at expiration
installed_at replace inspection service warranty serial appraisal
replacement_value registration shelf_life hazmat care par_level`.
