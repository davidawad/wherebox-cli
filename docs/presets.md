# Kind presets

A preset says what a kind of item tracks, which defaults it fills in, how
early to warn, and where each default comes from. `wherebox preset ls` lists
them; `wherebox preset show NAME` prints one with its sources.

```sh
wherebox add Ribeye --preset meat.raw --at "Kitchen / Fridge"
# expires 2026-01-04 (raw-meat: 3 days in the fridge from purchase on 2026-01-01)
wherebox lot add Ribeye 2 --at "Garage / Chest Freezer"     # expires in 4 months
```

## What a preset fills in

* **A default shelf-life type** (`shelf_life = "raw-meat"`). Shelf lives
  are not preset values: they belong to shelf-life types (`wherebox
  shelf-life ls`, `data/shelf_life.toml`, docs/SPEC.md "Shelf life and on
  hand"). An item of a food preset whose name names no better type (`Ribeye`
  is `raw-meat` by name anyway) gets the preset's type; `add --preset` then
  starts a lot so its expiry can be computed. Expiry is computed from where
  the lot is kept now, never stored. Presets of non-perishables (`pantry`,
  `beverage`, `medicine`, `supplement`, `household`...) name no type: their
  items never expire. A user preset file with the old inline `[shelf_life]`
  table (`fridge`, `freezer`, `pantry`, `opened`; `date` is ignored) still
  loads: the table becomes a shelf-life type named after the preset.
* **Service dates** (`[intervals]`) as custom fields, counted from the item's
  `Installed` field, else its purchase date: `Replace By` (not rolled
  forward, so an overdue replacement shows as past), `Next Inspection`,
  `Next Service` and `Reappraise By` (the first one on or after today). A
  `warranty` term sets `warranty_expires`. Nothing is guessed without a base
  date.
* **Defaults** (`[defaults]`): `unit`, `labels`, custom `fields` for new items.
* **Alerts**: `alert_days` widens `wherebox expiring` for the preset's
  (perishable) items unless `--within` is given.
* **Reports**: presets with category `food` or `consumable` are left out of
  the insurance report; presets that track `serial` make a missing serial
  number a gap.

## Sources

Every value a built-in preset sets is covered by a `[[sources]]` entry (a
unit test checks this). Values without a citable source are left unset and
explained in `notes`.

| preset | value | source |
|---|---|---|
| food (`meat.raw`, `meat.ground`, `poultry.raw`, `seafood.raw`, `eggs`, `bread`, `leftovers`, `frozen`, `condiment`) | the default shelf-life type; the type's figures cite USDA FSIS FoodKeeper data, version 128 (<https://catalog.data.gov/dataset/fsis-foodkeeper-data>) and the Cold Food Storage Chart per group in `data/shelf_life.toml` | |
| `frozen`, `condiment` (notes) | frozen food safe indefinitely; opened mayonnaise 2 months | USDA FSIS Cold Food Storage Chart (<https://www.foodsafety.gov/food-safety-charts/cold-food-storage-charts>) |
| `smoke-detector` | replace every 10 years, test monthly | NFPA (<https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms>) |
| `fire-extinguisher` | inspect monthly, maintain yearly | NFPA 10 (<https://www.nfpa.org/codes-and-standards/nfpa-10-standard-development/10>) |
| `filter` | check monthly, replace at least every 3 months (HVAC air filters) | ENERGY STAR Maintenance Checklist (<https://www.energystar.gov/saveathome/heating-cooling/maintenance-checklist>) |
| `hvac` | yearly professional check-up | ENERGY STAR Maintenance Checklist |
| `appliance`, `electronics`, `tool`, `furniture` (90) | `alert_days` | wherebox policy (docs/SPEC.md), not an external fact |

Dairy and produce presets name no single type: their items get one by name
(`milk`, `yogurt`, `banana`, `spinach`...). Left unset for lack of a single
citable figure: CO alarm replacement (maker's schedule),
vehicle/generator/water-heater service, warranty terms, and reappraisal
intervals for valuables.

## File format

Built-in presets live in `presets/kinds/NAME.toml` and are compiled in. A
file in `<config dir>/presets/kinds/` (next to `config.toml`) with the same
`name` replaces a built-in preset; other names add presets.

```toml
name = "meat.raw"                 # lowercase letters, digits, '.', '-', '_'
description = "Raw beef, veal, lamb and pork"
category = "food"                 # food consumable safety durable valuable document other
tracks = ["shelf_life", "frozen_at"]  # see below
alert_days = 2                    # optional
notes = "free text"               # optional

shelf_life = "raw-meat"           # optional: the default shelf-life type of its items

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
