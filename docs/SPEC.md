# wherebox-cli spec (v0.1, draft for sign-off)

Personal inventory: everything owned, where it is, who owns it, what it is
worth, and the paperwork behind it. One local fsqlite file, one CLI, no
server, no network.

## Contract with Homebox

Homebox's CSV import is the interop contract (its internal schema churns: 28
SQLite migrations). Every item field below maps 1:1 to a documented column in
<https://github.com/sysadminsmedia/homebox/blob/main/docs/src/content/docs/en/advanced/import-csv.mdx>.
`wherebox export --format homebox` -> Homebox imports it; `wherebox import
--format homebox` reads a Homebox CSV export back. `HB.import_ref` = our
stable item id, so re-imports update instead of duplicating. Fields Homebox
lacks travel as `HB.field.<Name>` custom fields. Attachments do not travel in
CSV (Homebox limitation).

## Kept from biomarker-cli

fsqlite store + migrations, encryption at rest (age / SSH key / keyfile),
profiles (separate DBs), config, audit log, output formats
(table/json/jsonl/csv/tsv/yaml/toon/html), versioned JSON envelope
(`wherebox/v1`), import/export framework (incl. mapping files and sheet
import), shell completions, man pages, doctor. Removed: markers, units,
ranges, trend/flag/diff, person/sex/age.

## Schema

| Table | Columns | Homebox column |
|---|---|---|
| `items` | `id` (ulid; = import_ref) | `HB.import_ref` |
| | `asset_id` (`000-000`, auto-assigned) | `HB.asset_id` |
| | `name`, `description` | `HB.name`, `HB.description` |
| | `quantity` (int, default 1) | `HB.quantity` |
| | `location_id` -> locations | `HB.location` (full path `A / B / C`) |
| | `parent_item_id` (item inside item) | not in CSV; noted in `HB.notes` on export |
| | `manufacturer`, `model_number`, `serial_number` | `HB.manufacturer`, `HB.model_number`, `HB.serial_number` |
| | `purchase_from`, `purchase_price` (USD cents), `purchase_time` (date) | `HB.purchase_from`, `HB.purchase_price`, `HB.purchase_time` |
| | `insured` (bool) | `HB.insured` |
| | `lifetime_warranty`, `warranty_expires`, `warranty_details` | `HB.lifetime_warranty`, `HB.warranty_expires`, `HB.warranty_details` |
| | `sold_to`, `sold_time`, `sold_price`, `sold_notes` | `HB.sold_*` |
| | `notes` (<=1000 chars) | `HB.notes` |
| | `owner_entity` (person / trust / LLC id) | `HB.field.Owner Entity` |
| | `replacement_value` (USD cents), `valued_at` (date) | `HB.field.Replacement Value`, `HB.field.Valued At` |
| | `status`: owned, lent, stored, disposed, sold, lost, gifted | `HB.field.Status` |
| | `created_at`, `updated_at` | - |
| `locations` | `id`, `name`, `parent_id`, `kind` (property, room, container, shelf, fridge, freezer, pantry), `address` (property only) | path segments of `HB.location` |
| `labels`, `item_labels` | `name` | `HB.label` (comma list) |
| `item_fields` | `item_id`, `name`, `value` | `HB.field.<name>` |
| `attachments` | `item_id`, `kind` (photo, receipt, manual, warranty, other), `path`, `sha256`, `primary` | not in CSV |
| `sources` | `item_id`, `source` (amazon-email, manual, photo, ...), `ref` (order / message id) | `HB.field.Source` |
| `events` | append-only: created, moved, edited, status change, valued | - |

Additional tables for consumables (food, household):

| Table | Columns | Homebox column |
|---|---|---|
| `items` (more) | `kind_preset` (see Presets), shelf-life type (custom field `Shelf Life`: a type name, or `none` for never perishable; see Shelf life), `unit` (ea, g, kg, ml, l, lb, oz, pack), `par_level` (keep at least N on hand), `reorder_qty`, `preferred_vendor`, `product_ref` (UPC/ASIN) | `HB.field.Preset`, `HB.field.Unit`, `HB.field.Par Level`, `HB.field.Reorder Qty`, `HB.field.Vendor`, `HB.field.Product Ref` |
| `lots` | `item_id`, `qty`, `acquired_at`, `opened_at`, `best_by` (the printed best-by date, perishables only: an override), `frozen_at`, `confirmed_at` (last `lot confirm`), `location_id` (fridge / freezer / pantry shelf), `status` (sealed, open, consumed, discarded), `unit_price`, `source_ref` (the purchase row, unique), `vendor` (a store id or any text; see Stores and sourcing); `expires_at` is retired (always empty: expiry is computed, see Shelf life) | flattened on export: one HB row per open lot, `HB.field.Best By`, `HB.field.Expires` (computed, export only), `HB.field.Lot Price`, `HB.field.Lot Ref` |
| `consumption` | `lot_id`, `qty`, `at`, `reason` (eaten, used, spoiled, discarded) | - |

Each property is its own wherebox: a separate profile and database
(`wherebox --profile lake-house ...`) with its own location tree (rooms,
fridge, freezer, pantry, closets). Profiles are declared in `[profiles.NAME]`
config tables or found as databases in the profiles directory; there are no
built-ins. `rented = true` marks a rental: its insurance report covers
contents only. `wherebox report
insurance --all-profiles` rolls them up. Attachments live in
`<data dir>/attachments/<item id>/`, content-addressed, encrypted with the DB
key when encryption is on.

## Commands

```
wherebox add <name> --at "Lake House / Garage" [--qty --price --bought --from --serial --model --maker --owner --label --shelf-life T ...]
wherebox edit <item> [same flags]
wherebox mv <item>... --to <location>
wherebox rm <item>            # status change (disposed/sold/gifted); hard delete only with --purge
wherebox show <item>          # all fields, attachments, history
wherebox ls [--at <loc>] [--label] [--owner] [--status] [--warranty-before <date>]
wherebox find <text>          # name/description/serial/notes
wherebox loc add|ls|mv|rm     # location tree; `loc ls` prints the tree with item counts and value
wherebox label add|ls|rm
wherebox attach <item> <file> [--kind receipt]
wherebox history <item>
wherebox value <item> <amount> [--at <date>]
wherebox import <file> [--format homebox|csv|json|jsonl] [--mapping m.toml] [--dry-run]
wherebox export [--format homebox|csv|json|jsonl] [--at <loc>] [--owner]
wherebox preset ls|show <name>
wherebox par <item> <n> [--reorder Q]
wherebox shelf-life ls | show <type> | set <type> [--pantry 5d --fridge 7d --freezer 6m --opened 3d --synonym S...] | unset <type>
wherebox lot add|ls|edit|rm|confirm <item> [--qty N] [--vendor STORE]
wherebox shopping [--profile P | --all-profiles] [--within 7d] [--since 18] [--checklist] [--include-list L]
wherebox list new|ls|show|rm|add|edit|drop|check|shop|pin|unpin|explain|import   # see Lists and matching
wherebox match <query> | cohort new|ls|show|add|rm
wherebox store add|ls|show|edit|rm|layout|use   # see Stores and sourcing
wherebox source add|ls|edit|rm
wherebox report insurance [--profile P | --all-profiles] [--owner O] [--min-value N] [--bought-after DATE] [--format csv|xlsx|pdf|html]
wherebox db regroup-consumables [--dry-run]
wherebox report warranties [--expiring 90d] [--all-profiles] [--include-expired]
wherebox doctor | key | profile | config | completions
```

Every command takes `--json` (`wherebox/v1` envelope) for agents.

## Presets

A preset is a TOML file (`presets/kinds/*.toml`, user-overridable in the
config dir) that says which dates and fields a kind of item tracks, sensible
defaults, and its alerts. `wherebox add "Ribeye" --preset meat.raw --at
"Fridge"` fills in the expiry from the preset. `wherebox preset ls` lists
them, and `wherebox preset show meat.raw` answers "what does this kind
track". Shelf lives are not preset values: they belong to shelf-life
types (see Shelf life); a food preset names its default type
(`shelf_life = "raw-meat"`). Starting set:

| Preset | Tracks | Defaults / alerts |
|---|---|---|
| `meat.raw`, `meat.ground`, `poultry.raw`, `seafood.raw` | computed expiry, frozen_at | default type `raw-meat`, `ground-meat`, `raw-poultry`, `raw-fish` |
| `dairy`, `eggs`, `produce`, `bread`, `leftovers` | computed expiry, opened_at | type by name (`milk`, `yogurt`, `banana`...); `eggs`, `bread`, `cooked-leftovers` by default |
| `frozen` | frozen_at, computed expiry | default type `frozen-meal` |
| `pantry` (dry, canned), `condiment` | opened_at | pantry: non-perishable; condiment: `condiment-opened` (expires once opened) |
| `beverage`, `alcohol` | - | non-perishable (by name: `milk`, `juice-fresh`) |
| `medicine`, `supplement`, `first-aid` | - | non-perishable; restock by cadence |
| `household`, `toiletry` | none (par level only) | restock |
| `filter` (HVAC, water, fridge) | installed_at, replace interval | 1-6 months |
| `battery`, `smoke-detector`, `co-detector`, `fire-extinguisher` | installed_at, replace or inspect interval, expiration | detectors 10 / 7 yrs; extinguisher yearly check |
| `appliance`, `electronics`, `tool`, `furniture` | warranty_expires, serial, service interval | warranty alert |
| `vehicle`, `generator`, `hvac`, `water-heater` | warranty, service interval, registration/inspection | service due |
| `firearm`, `jewelry`, `art`, `collectible` | serial, appraisal date, replacement value | reappraise every 3-5 yrs (insurance scheduling) |
| `document` (passport, license, insurance policy) | expiration (custom field) | - |
| `seasonal` (paint, sealant, fuel, chemicals) | opened_at, hazmat flag | non-perishable |
| `plant`, `pet-supply` | care interval | - |

Food presets name a default shelf-life type (`meat.raw` -> `raw-meat`,
`meat.ground` -> `ground-meat`, `poultry.raw` -> `raw-poultry`,
`seafood.raw` -> `raw-fish`, `eggs`, `bread`, `leftovers` ->
`cooked-leftovers`, `condiment` -> `condiment-opened`, `frozen` ->
`frozen-meal`); `pantry` (dry and canned goods), `beverage`, `alcohol`,
`medicine`, `supplement`, `household`, `toiletry` and the durable kinds name
none: they never expire. A user preset file with the old `[shelf_life]`
table still loads; the table acts as an inline type named after the preset.
Every default a built-in preset sets cites a source (NFPA, NFPA 10, ENERGY
STAR, or this spec for alert lead times); where no single figure can be cited
(CO alarms, vehicle service, warranty terms, reappraisal intervals) the value
is left unset and the preset's notes say why. Details: docs/presets.md.

## Groceries and shopping

- `wherebox use <item> [qty]` / `wherebox eat` / `wherebox toss` log
  consumption against the oldest lot first.
- `wherebox expiring [--within 3d] [--at L]` lists perishable lots whose
  computed expiry falls within the window (freezer included), and lots that
  expired within the same window back (older expired lots are presumed used
  up and drop off, unless confirmed after their expiry). Non-perishables
  never appear. `qty` shows the lot's logged quantity; `expiry_basis` says
  where the date came from.
- A consumable bought repeatedly is one item with a lot per purchase. On
  import, consumable rows (`HB.field.Kind` = consumable, a food/consumable
  preset or label) sharing `HB.field.ASIN` (else a product ref) at one
  property collapse into one item; each row becomes a lot (qty, acquired =
  purchase date, unit price, the row's import ref as `source_ref`, so
  re-imports are idempotent). Durables stay one item per row. `wherebox db
  regroup-consumables [--dry-run]` migrates existing databases in place and
  records `merged`/`regrouped` events.
- `wherebox shopping [--profile rental-apt]` generates the list: every item
  whose estimated on hand (see Shelf life) is below `par_level`, anything
  expiring (computed expiry) that has a par, plus learned
  reorders: with 3+ purchases (lot dates) the cadence is the median gap; due
  when that has passed since the last purchase and no unused lot was
  acquired within the last cadence (lots presumed used up don't count).
  Without that history a `Recurring`
  custom field gives it (`9d`, `monthly`, or `yes`/`true` with `Avg Interval
  Days`). Rows show last bought, cadence and days overdue. Items last bought
  before `--since` months (default 18) are dropped, not due. Output as text,
  a checklist (`--checklist`, Markdown), or a cart-ready CSV (`-f csv`).
  `wherebox par <item> <n>` sets the par level.
- Receipt intake (Amazon, Whole Foods, grocery emails) adds lots
  automatically, so the inventory restocks itself when you buy.

## Shelf life and on hand

Every purchased lot used to count as on hand forever. Instead, perishables
get a computed expiry, and every consumable lot is presumed used up after a
while unless logs say otherwise.

### Shelf-life types

A shelf-life type (`banana`, `berries`, `leafy-greens`, `milk`,
`cheese-hard`, `raw-poultry`, `ground-meat`, `bread`, `cooked-leftovers`,
`condiment-opened`, `canned-food-opened`, ...; about 60 common fruit,
vegetable, dairy, egg, meat, seafood, bakery and prepared types ship) says
how long food keeps per storage:

```toml
[raw-poultry]
group = "meat"              # fruit, vegetable, herb, dairy, egg, meat, seafood, bakery, prepared, condiment, other
pantry = ...                # absent: not recommended there
fridge = "1d"               # d, w, m, y; a range (3-5d) uses its shorter end
freezer = "9m"
opened = ...                # after opening (any storage); absent: opening changes nothing
synonyms = ["chicken breast", "chicken thighs", "turkey breast"]
```

The defaults are an embedded data file (`data/shelf_life.toml`), with each
group citing USDA FSIS FoodKeeper and the FDA/FoodSafety.gov cold storage
chart, using the conservative end of every range. A test checks that it
loads, every value parses, every type has at least one storage, and type
names and synonyms are unique. Config overrides and extends it:
`[shelf_life.<type>]` tables in `config.toml` replace the given fields of a
built-in type (`[shelf_life.milk] fridge = "5d"`) or define a new type
(`[shelf_life.kombucha] fridge = "30d"`; a new type needs at least one
storage). `wherebox shelf-life ls` lists them (origin built-in, config or
overridden), `shelf-life show <type>` prints one (with the items classified
as it in the current profile), `shelf-life set <type> --fridge 7d --freezer
6m [--pantry --opened --synonym S --group G]` writes the config table
(`--fridge none` removes a value), `shelf-life unset <type>` drops it.

### Which items are perishable

An item's type is, in order: its `Shelf Life` custom field (set by `add` /
`edit --shelf-life <type>`; `--shelf-life none` marks it never perishable;
an unknown type is an error), else, for consumables (a consumable per
Groceries, or an item with lots), the best type whose name or synonym
matches the item's name (the name-matching ladder below: the type phrase's
head noun must be the head noun of a segment of the item name, so "Chili
Garlic Sauce" is not garlic and "Berry Trail Mix" is not berries; score >=
0.85), else the default type of its preset. Durables, and consumables
without a type, are non-perishable: canned goods unopened, dry goods,
cleaning supplies, toiletries, supplements, medicine, paper goods. A
consumable whose preset category is `consumable` (not `food`) is never
auto-classified.

### Computed expiry

Expiry is computed when read, never stored. For an active lot of a typed
item:

- storage: the nearest location of the lot (else the item) whose kind is
  `fridge`, `freezer` or `pantry` or whose name says so (`Freezer`, `Chest
  Freezer`, `Fridge`, `Refrigerator`, `Pantry`, `Cupboard`, `Larder`);
  default pantry.
- base and days: in the freezer, `frozen_at` (else `acquired_at`) plus the
  freezer days; otherwise `acquired_at` plus the storage days. A storage the
  type has no value for (not recommended) uses the shortest of its pantry and
  fridge values, else its freezer value, and says so in the basis. An opened
  lot (`opened_at`) of a type with an `opened` value expires at the earlier
  of `opened_at` + opened days and the date above.
- a printed best-by on the lot (`lot add|edit --best-by`, perishables
  only) wins over the computed date.
- moving a lot into a freezer (`lot edit --at`, or moving the item a lot
  inherits its place from) sets `frozen_at` to that day, so re-freezing
  re-bases the freezer clock.

A lot of a type without a value for its state (an unopened lot of
`canned-food-opened`) has no expiry. `expiry_basis` is `best_by` or
`shelf_life` (null without expiry); `expiry_detail` says e.g. "raw-poultry:
1 day in the fridge from purchase on 2026-01-01". There is no `--expires`
on items or lots: non-perishables cannot be given an expiry (`--best-by` on
one is an error; imports drop it with a warning). Schema migration 3
retires `lots.expires_at`: for non-perishable items every stored
`expires_at`/`best_by` is dropped; for perishables a stored date is kept as
the printed best-by only when it differs from what the item's preset would
have filled in (an explicit date), and dropped otherwise.

### Estimated on hand

For every consumable (perishable or not), an active lot is **presumed used
up** when

- (a) it has an expiry and that date has passed, or
- (b) it has none and its age exceeds the item's reorder cadence: the median
  gap between purchases (lot acquired dates, with 3+ purchases), else the
  `Recurring` field, else the `[reorder_cadence]` config table by preset
  name, then preset category, then `default` (built-in 90d).

Age counts from the lot's last evidence: the latest of `acquired_at`, its
last logged consumption and `confirmed_at`. An expired lot confirmed after
its expiry is on hand (but expired). Explicit `use`/`eat`/`toss` logs always
win (they change the logged quantity and status); `wherebox lot confirm
<item> [--qty N] [--lot L]` asserts what is on hand today: it sets
`confirmed_at` on the item's active lots (newest first), and with `--qty`
keeps only N (trimming the newest lots to N and logging the rest as `used`
with the note "not found at lot confirm"; more than is logged is an error:
use `lot add`).

`on_hand` is the logged quantity of active lots minus presumed-used-up
ones; `on_hand_basis` is `logged` when nothing was presumed, else
`estimated`. Durables and items without lots report their quantity
(`logged`). Text output shows estimated quantities as `~2`. JSON for
`ls`/`show`/`lot ls` items and lots carries `on_hand`, `on_hand_basis`,
`expires_at` (computed: the earliest of the item's counted lots),
`expiry_basis` and `shelf_life` (the type); lots also carry `used_up`
(presumed) and `expiry_detail`. `list check` counts estimated on hand (rows
and matches carry `on_hand_basis`, the table shows `~`), `shopping` uses it
for par checks, and `expiring` lists only perishables by computed date.
`items.quantity` keeps the logged quantity.

```toml
[reorder_cadence]   # config.toml
default = "90d"
supplement = "60d"  # preset name
consumable = "120d" # preset category
```

## Lists and matching

Lists answer "do we have what this needs, here?": a recipe ("nutty pudding
at the Beach House"), a kit or packing list, a wishlist. They are cross-property
(one recipe is checked against any profile), so they live outside the
per-profile databases, as hand-editable, git-able TOML:

- `<config dir>/lists/<slug>.toml`: one list per file.
- `<config dir>/cohorts/<slug>.toml`: one cohort (a named, reusable matcher)
  per file.
- `list rm` / `cohort rm` move the file to `lists/.trash/` (`cohorts/.trash/`,
  timestamped when the name is taken); nothing is ever deleted.

CLI edits go through `toml_edit`, so comments, key order and formatting of
the rest of the file survive. The slug is `slugify(name)`; a list is named on
the command line by slug or name.

### List file

```toml
name = "Nutty pudding"
type = "recipe"          # recipe, kit, packing, wishlist, or any other string
profile = "beach-house"       # optional default profile for check/shop/pin/explain
notes = "serves 6"
food = true              # only food is a candidate (default: true for recipe, false otherwise)

[[entry]]
name = "walnuts"         # display name; also the fuzzy name (see below)
qty = 200                # default 1
unit = "g"               # default ea
note = "chopped"
optional = false
cohort = "nuts"          # string or array; matcher fields below
exclude = ["walnut oil"]
pins = ["asin:B0WALNUT01"]
```

Types with behaviour: `recipe` (consumables; expired and presumed used-up lots don't count; a food list),
`kit` and `packing` (lent items don't count: they are not here to pack),
`wishlist` (see below). Any other string is a custom type with recipe
semantics (not a food list unless `food = true`).

**Food lists** (`food = true`, the default for `recipe`; `list new --food
true|false` or the `food` key sets it per list): only food items are
candidates at the attribute and name tiers (labels, presets, cohorts, fuzzy
names); identifiers, URLs and pins still match anything. An item is food
unless its `Kind` is `durable`, its preset's category is not `food`, a label
says otherwise (`household`, `cleaning`, `toiletries`, `personal care`,
`beauty`, `cosmetics`, `fragrance`, `health`, `medicine`, `supplements`,
`vitamins`, `pet supplies`, `office`, `electronics`, `clothing`, `tools`,
`paper`, `durable`, and no food label) or its name does (`spray`, `edp`,
`parfum`, `cologne`, `candle`, `soap`, `shampoo`, `lotion`, `serum`,
`capsule`, `softgel`, `tablet`, `supplement`, `vitamin`, `detergent`,
`cleaner`, `wipe`...); and it is food when its preset is a `food` preset, a
label (`food`, `grocery`, `grocery & gourmet food`, `produce`, `pantry`,
`snacks`, `beverages`, `dairy`, `meat`, `seafood`, `frozen`, `bakery`...) or
`Kind = food` says so, it has a shelf-life type, it (or a lot) is kept in a
fridge, freezer or pantry, or its name's head noun is a food noun (the food
lexicon below, or a shelf-life type). Anything else is not a candidate. `list
explain` shows each candidate's `food` verdict.

### Matchers

An entry (and a cohort) is a matcher. Every field takes a string or an array:

| field | tier | effect |
|---|---|---|
| `asin`, `sku`, `upc` (alias `gtin`), `product_ref` | a, identifier | widen: an item whose identifiers contain it |
| `url` | b, url | widen: Amazon URLs give their ASIN (tier b, compared as an identifier); other URLs compare host + path (+ non-tracking query) with URLs on the item |
| `pins` | c, pinned | widen: confirmed matches written by `list pin` (`asin:B0..`, `upc:..`, `ref:..`, or `item:<profile>:<id>`) |
| `label`, `preset` (alias `kind`) | d, attribute | widen: any item with that label / kind preset |
| `cohort` | as matched | widen: everything the cohort matches (its own fields, recursively) |
| `name` (the entry's name), `any` | e, name | widen: fuzzy name match at or above the threshold |
| `at` (alias `location`) | - | narrow: only items (and, for consumables, lots) at a location whose path contains these segments in order (`Kitchen / Freezer` matches `Beach House / Kitchen / Freezer`) |
| `exclude` | - | names; an item whose name fuzzily matches one is never a match. Exclude always wins |
| `fuzzy` | - | `true`/`false`: whether the entry's own name is a fuzzy matcher. Default: only when the entry has no other selector (`any` and `pins` don't count) |
| `threshold` | - | fuzzy threshold for this entry (else the list's `threshold`, else the `match_threshold` setting, default 0.82) |

A cohort file has `name`, `notes` and the same matcher fields (no own-name
matching: a cohort matches by `any`, identifiers, labels...). `cohort =` in a
cohort includes other cohorts; a cycle (`a -> b -> a`) is an error naming the
path. A cohort's `at` and `exclude` apply to what it matches; the including
entry's `at` and `exclude` apply on top.

Identifiers on an item: `product_ref`, `model_number`, `serial_number` and the
custom fields ASIN, SKU, UPC, GTIN, EAN, ISBN, MPN, Product Ref, Item Number.
Both sides are normalized: upper case, only letters and digits (dashes,
spaces, dots dropped); digit strings that are GTINs (8, 12, 13 or 14 digits,
or an 11-digit UPC without its check digit, which is computed) compare as
GTIN-14 (left-padded with zeros), so a UPC-A matches the same EAN-13.
Identifiers shorter than 4 characters never match. URLs on an item: custom
field values and http(s) URLs in the notes or description.

URL normalization: lower-case scheme and host, `www.`/`smile.`/`m.` dropped,
fragment dropped, tracking query parameters (`utm_*`, `ref`, `ref_`, `tag`,
`psc`, `th`, `gclid`, `fbclid`, `_encoding`, `pd_rd_*`, `pf_rd_*`, `qid`,
`sr`, `keywords`, `crid`, `sprefix`, `content-id`, `smid`, `linkCode`, `spm`)
dropped, the rest sorted, trailing `/` dropped. Amazon hosts (`amazon.*`,
`amzn.*`): the ASIN from `/dp/<ASIN>`, `/gp/product/<ASIN>`, `/gp/aw/d/<ASIN>`,
`/o/ASIN/<ASIN>` or `/exec/obidos/ASIN/<ASIN>`. No network: short links
(`a.co`, `amzn.to`) only compare as URLs.

Head nouns: a product name is cut into segments at commas, parentheses,
`/`, ` - ` and the words `in`, `with`, `for`, `from`, `by` (`Tuna in Water,
5 oz Can` -> `tuna`, `water`, `5 oz can`); a segment's head noun is its last
normalized word that is not a modifier (`organic`, `frozen`, `raw`,
`whole`, `wild`, `boneless`...), packaging (`bag`, `head`, `bunch`, `jar`,
`can`, `loaf`...) or a cut (`halves`, `slices`, `root`, `cloves`,
`leaves`...), except that a food form (`sauce`, `paste`, `cereal`, `mix`,
`chips`, `bar`, `powder`, `juice`, `oil`, `tea`, `candy`, `seasoning`,
`dressing`, `syrup`, `cookies`...) is the head wherever it sits, since
brands often put it first or mid-name. `Hemp Seed Organic, 12 Ounce` is
seeds, `Walnut Halves` walnuts, `Chili Garlic Sauce` and `Huy Fong Sauce
Chili Garlic` sauces. A product whose segment heads (outside `in`/`with`...
segments) include a food form is that form: a phrase whose head noun is
another word is at most a modifier of it, even in a segment of its own
(`blueberry` in `Chex Cereal, Blueberry`; `garlic` in `Garlic Powder`, which
does match `garlic powder`).

Fuzzy names: lower case; punctuation to spaces; pack and size tokens dropped
(`12-pack`, `pack of 6`, `5oz`, `16 fl oz`, `2 lb`, `x3`, bare numbers);
marketing and preparation stopwords dropped (`premium`, `value`, `family
size`, `bulk`, `new`, `brand`, `chopped`, `diced`...; `organic` and `whole`
are kept, they change the product); words singularized (`berries` -> `berry`, `walnuts` ->
`walnut`). Similarity is token-set based: each query token is matched to the
most similar candidate token (strsim Jaro-Winkler, counted when >= 0.94), and
`score = 0.9 * query coverage + 0.1 * candidate coverage`, so a query fully
contained in a long Amazon title scores >= 0.9 and an exact name 1.0. The
query's head noun must be the head noun of a segment of the candidate: a
shared modifier alone is not enough, and a compound product in which the
query's noun is only a flavour or modifier (`garlic` in `Chili Garlic
Sauce`, `berry` in `Berry Trail Mix`, `cherry` in `Lost Cherry Eau de Parfum
Spray`) scores at most 0.5. Synonyms widen food nouns only where the synonym is
the product's head noun (not a flavour of a cereal or bar): an entry named like a
lexicon key or a shelf-life type also matches candidates whose head noun is
one of its synonyms (`berries`: blueberries, raspberries, strawberries,
blackberries; `nut milk`: almond, macadamia, cashew milk; `citrus`: lime,
lemon, orange...), at 0.95 of that head match, and the match says so (`name
"berries" (as blueberry)`). Words compare equal when identical or, both six
letters or longer, Jaro-Winkler >= 0.94.
Candidates are ranked per query (`name` and each `any`). The match is
**ambiguous** when the best two distinct candidates (different normalized
names) are within 0.03 of each other and they don't both contain every query
token (two walnut products that both contain "walnut" are both counted;
"dark chocolate chips" between "Milk Chocolate Chips" and "Semi-Sweet
Chocolate Chips", 0.65 and 0.64, is ambiguous at a threshold of 0.6: pin
one, or add an `exclude`; a "Dark Chocolate Bar" is not chips at all). Ambiguity only
matters when the entry has no match above tier e.

Every match reports its tier (`identifier`, `url`, `pinned`, `attribute`,
`name`; ladder order a-e, highest confidence first; an item matched several
ways reports the best) and score (1.0 above tier e, the similarity at tier
e), and what it matched (`via`, e.g. `cohort nuts: any "pecans"`).

### Have-check

Per matched item: durables (no lots) count `quantity` when owned, stored or
lent (kit/packing: owned or stored). Consumables count their unconsumed lots
(sealed, open) whose location satisfies `at`, by estimated on hand (see
Shelf life): lots presumed used up don't count (reported as `used_up`, the
row's `on_hand_basis` is `estimated` and the table shows `~2`); lots past
their computed expiry but confirmed after it are reported separately
(`expired`) and don't count. Quantities convert
between compatible units: mass `g kg oz lb`, volume `ml l tsp tbsp cup`
(`fl oz` is volume, `oz` mass), and count: `ea` (`each`, `ct`, `count`, `pc`,
`pcs`, `piece`), `can`, `jar`, `bottle`, `box`, `bag`, `pack` (`pk`),
`packet`, `tube`, `capsule` are one dimension and compare one for one (`1 can
tuna` is had by one item counted in `ea`). Pack counts: when the entry's unit
is an inner count unit (any count unit but `ea`/`pack`, e.g. `can`,
`capsule`) and the item is counted in `ea` or `pack`, a pack count in the
item's name multiplies it: `12-Pack`, `12 pack`, `12pk`, `Pack of 12`, `90
Count`, `12 ct` (the last pack phrase times the last count phrase, so `24
Count (Pack of 4)` is 96). One `Albacore Tuna, 5 oz Can, 12-Pack` in `ea`
has 12 cans; one `Fish Oil, 90 Count` has 90 capsules. An entry in `ea` or
`pack` is never multiplied (one 12-pack is one `ea`), nor is an item counted
in the inner unit itself. An item counted in a count unit whose name states a
size (`Walnut Halves 16 oz`) counts as that many of the size per unit for a
mass or volume entry. Common foods convert further through the food lexicon
(`data/food.toml`, from USDA FoodData Central portion weights), found by the
entry's head noun, else the item's: a density (g per ml) turns volume into
mass and back (`1 tbsp hemp seeds` against a 12 oz bag: 10 g a tablespoon,
basis `density`), and piece weights turn count-like units into mass (`1
clove garlic` is 3 g; a `garlic, 1 head` item counted `ea` is a 30 g head,
about 10 cloves, a `garlic 3 lb bag` 1360 g; `2 limes`, `10 cherries` and
`3 brazil nuts` count against bags by weight; basis `piece weight`). Any
other mismatch, an unknown density or piece weight included, is **unit
mismatch**: unknown, not missing.

Entry status: `have` (enough), `short` (have x of y), `missing`, `ambiguous`,
`unit-mismatch`. The list is **ready** when every non-optional entry is
`have`; the verdict is `ready` or `missing N of M` (N non-optional entries not
`have`, of M non-optional). `list check` exits 0 when ready and 3 when not
(errors keep their own codes and print to stderr), so it scripts:
`wherebox list check nutty-pudding -P beach-house -q && echo go`. With
`--all-profiles` there is a verdict per profile and the exit is 0 when some
property is ready.

Wishlists: each entry is `open` or `fulfilled` (`already own`: a matching
item is owned, stored or lent, listed so you don't buy it twice). `list ls`
shows open/fulfilled counts (against the list's or current profile); `list
check` on a wishlist always exits 0.

### Commands

```
wherebox list new <name> --type <t> [--profile P] [--notes N] [--food BOOL]
wherebox list ls | show <l> | rm <l>
wherebox list add <l> <entry> [--qty --unit --note --asin --sku --upc --product-ref --url --label --preset --at --cohort --any N... --exclude N... --optional --fuzzy B --threshold T]
wherebox list edit <l> <entry> [same flags] [--name N] [--required] [--clear FIELD]
wherebox list drop <l> <entry>
wherebox list check <l> [-P P | --all-profiles] [--threshold T] [--store S]
wherebox list shop <l> [-P P | --all-profiles] [--store S]
wherebox list pin <l> <entry> <item> [-P P] | unpin <l> <entry> [<pin>|--all]
wherebox list explain <l> <entry> [-P P]
wherebox list import <l> <file>
wherebox match <query> [-P P] [--asin --sku --upc --url --label --preset --at --cohort --any --exclude --threshold]
wherebox cohort new <name> [matcher flags] | ls | show <c> | add <c> [matcher flags] | rm <c>
wherebox shopping --include-list <l>...
```

`wherebox list` was an alias of `ls` before lists existed; it now names this
command group.

`<entry>` is the entry's name (case-insensitive) or its 1-based position
(`3` or `#3`). Profile for `check`/`shop`/`pin`/`explain`: `-P`, else the
list's `profile`, else the profile in use. `list pin` stores the item's
identifier when it has one (ASIN field, else product ref, else UPC/GTIN
field), else `item:<profile>:<id>`. `list explain` shows every candidate
(any tier hit, or a name score >= 0.5) with each tier's verdict, the
exclusion and location verdicts, and the final tier. `list shop` emits
shortfall rows in the `shopping` shape (`reason` = `list`, detail `<list>:
have x of y`); missing and short entries are listed, ambiguous and
unit-mismatch ones are left out with a note; on a wishlist every open entry.
`shopping --include-list` merges them into the shopping list per profile.

`list import` reads plain text, one entry per line: `[qty] [unit] name`
(`200 g walnuts`, `1 1/2 cup milk`, `½ tsp salt`, `3 eggs`, `200g sugar`),
bullets (`-`, `*`, `- [ ]`, `1.`) and `#` comment lines skipped, text in
parentheses or after a comma becomes the note. A line that is a URL becomes
a `url` matcher: indented, on the entry above; else its own entry (named from
the URL's slug or ASIN). No network.

## Stores and sourcing

Most food is bought at physical grocery chains whose catalogs are on their
own websites, not on Amazon. Stores and their products live in each
property's database (a profile's stores are the ones near that property),
so `list check` and `list shop` can say, for what is missing, which product
to buy at the store that serves the profile, where it sits in that store,
and in what order to walk the aisles. No network: URLs and category text
are stored and parsed, never fetched.

### Stores

| Table | Columns |
|---|---|
| `stores` | `id` (a slug, e.g. `sprouts-north`), `chain` (free text, e.g. `Sprouts`; compared case-insensitively), `name` (default: the chain), `address`, `website`, `notes`, `layout` (optional, see Layouts), `created_at`, `updated_at` |
| `chain_layouts` | `chain` (lower case), `layout`: the default layout of every store of the chain |
| `store_products` | `id` (integer), `food` (the canonical food it is sourced for, as typed) and `food_key` (its normalized name), `item_id` (optional link to an item), `store_id` or `chain` (one row covers one store, or every store of a chain; at least one is set), `name` (the store's product name), `brand`, `size` (free text, `16 oz`, `1 lb bag`), `price` (USD cents) and `price_seen` (date), `url`, `sku`, `plu`, `upc`, `section`, `aisle` (free text), `notes`, `created_at`, `updated_at` |
| `lots` (more) | `vendor`: where the lot was bought, a store id (see `store ls`) or any text |

A profile's stores are an ordered list in its config table; the first is
its default store:

```toml
[profiles.beach-house]
title = "Beach House"
stores = ["sprouts-north", "aldi-main"]
```

`wherebox store use <store>... [-P P]` writes that list (through
`toml_edit`: the rest of the file is kept). The store in use for a command
is `--store S`, else the profile's first store; without a profile (or with
no `stores`) there is no default store and any store's product is offered.
A configured store id missing from the database is a warning, not an
error.

```
wherebox store add <id> --chain C [--name N] [--address A] [--website URL] [--notes T] [--layout L]
wherebox store ls [--chain C]
wherebox store show <store>
wherebox store edit <store> [--chain --name --address --website --notes --layout L] [--clear FIELD]
wherebox store rm <store> [--force]          # --force also removes the store's own products
wherebox store layout [<store> | --chain C] [L] [--clear]   # show, or set, a layout
wherebox store use <store>... [-P P]         # the profile's stores, first = default
```

`<store>` is a store id, or a unique (case-insensitive) store name. `store
rm` refuses while store products reference the store, unless `--force`.

### Store products (sourcing)

```
wherebox source add <food> (--store S | --chain C) [--name N] [--brand B] [--size "16 oz"]
    [--price 4.99 [--seen DATE]] [--url URL] [--sku --plu --upc] [--section S] [--aisle A]
    [--note T] [--category TEXT] [--item ITEM]
wherebox source ls [food] [--store S | --chain C]
wherebox source edit <id> [same flags] [--clear FIELD]
wherebox source rm <id>...
```

`<food>` names the canonical food: a list entry name (`walnuts`), a food
lexicon noun, or anything else; `--item` also links it to an item. Many
products per food: one per chain at least, one per store where a store
differs. `--name` defaults to the food. `--seen` defaults to today when a
price is given. `--category` is the store website's category text
(breadcrumbs such as `Pantry > Nut Butters`); it is only used to guess the
section, and not stored.

An entry is linked to a product (strongest first):

1. **name**: the product's `food_key` is the entry's normalized name (or
   one of its `any` names), see Fuzzy names;
2. **item**: the product's `item_id` is one of the entry's matches;
3. **head noun**: the entry's name names the food (or the food names the
   entry) by head noun at a head score of 0.85 or more and not as a
   compound (`walnuts` and `walnut halves`; not `walnuts` and `walnut
   oil`).

`source ls <food>` lists the products linked to `<food>` that way.

The product chosen for an entry at a store: among the linked products,
the one at **that store** (`store_id`), else one for **its chain** (a
`chain` row, or a row of another store of the same chain), else **another
store**: the profile's other stores in order, then any store. Ties go to
the stronger link, then the lowest id. The tier is reported as
`source_tier` = `store`, `chain` or `other`.

### Sections and aisles

Sections are a fixed vocabulary, aliases in parentheses:

`produce`, `bulk`, `meat`, `seafood`, `deli`, `bakery`, `dairy`, `eggs`,
`frozen`, `pantry` (`dry goods`, `pantry/dry goods`, `dry`), `canned`,
`baking`, `spices` (`spice`, `herbs & spices`), `oils-vinegar` (`oils`,
`oil`, `vinegar`, `oils/vinegar`), `snacks`, `beverages` (`drinks`),
`supplements` (`vitamins`, `supplements/vitamins`), `household`,
`health-beauty` (`health/beauty`, `health`, `beauty`), `checkout`,
`special-buys` (`center`, `center/special-buys`, `specials`).

Input is lower-cased with spaces, `/` and `&` read as `-`; any other value
is a **custom** section (stored as its slug, `hot-bar`), reported with
`custom: true`. An aisle is free text (`7`, `12B`, `bins 3-4`); its first
number orders products within a section.

**Section guess**: `source add` without `--section` guesses one from, in
order, `--category`, the words of the URL's path, the product name and the
food, with a fixed table (deterministic, no network):

1. phrases first: `peanut butter`, `almond butter`, `nut butter`, `coconut
   milk` -> pantry or canned, `olive oil` -> oils-vinegar, `baking soda`,
   `baking powder` -> baking, `trail mix` -> snacks...;
2. the modifiers `frozen` -> frozen, `canned`/`tinned` -> canned, `bulk`
   -> bulk;
3. for a product name or food, its head noun (see Head nouns: `Chicken
   Broth` is broth, pantry); for category and URL text, any word;
4. word table (in this priority): spices (`spice`, `seasoning`,
   `cinnamon`, `paprika`, `cumin`, `salt`...), oils-vinegar (`oil`,
   `vinegar`), baking (`flour`, `sugar`, `yeast`, `cocoa`), supplements
   (`supplement`, `vitamin`, `capsule`), health-beauty (`shampoo`, `soap`,
   `lotion`, `toothpaste`...), household (`detergent`, `cleaner`, `paper`
   towels, `foil`...), beverages (`juice`, `soda`, `coffee`, `tea`,
   `kombucha`...), snacks (`snack`, `chip`, `cracker`, `pretzel`, `popcorn`,
   `bar`), bakery (`bread`, `bagel`, `tortilla`, `muffin`), deli (`deli`,
   `hummus`, `salami`), eggs (`egg`), dairy (`milk`, `cheese`, `yogurt`,
   `butter`, `cream`), seafood (`fish`, `salmon`, `shrimp`, `cod`...), meat
   (`beef`, `chicken`, `pork`, `turkey`, `bacon`, `sausage`...), produce
   (`fruit`, `vegetable`, `banana`, `lime`, `lemon`, `avocado`, `garlic`,
   `onion`, `berry`...), pantry (`rice`, `pasta`, `bean`, `oat`, `cereal`,
   `walnut`, `almond`, `seed`, `sauce`, `honey`, `broth`...), and the
   section names and aliases themselves.

The guessed section is stored like a given one and reported with
`section_guessed` and its basis (`category`, `url`, `name`, `food`). No
match leaves the section empty.

### Layouts

A layout is the order of sections from the entrance to the checkout,
optionally with the aisles of each section. Written on the command line as
a comma list, `section[=aisles]`:

```
produce, bulk=1-2, bakery, deli, meat, seafood, pantry=3-7, canned=5, snacks=8, dairy=11, frozen=12-13, checkout
```

A store's layout is its own, else its chain's (`store layout --chain
Sprouts L`, set once for every store of the chain), else the **generic
default**: produce, bulk, bakery, deli, meat, seafood, pantry, canned,
baking, spices, oils-vinegar, snacks, beverages, supplements,
health-beauty, household, special-buys, eggs, dairy, frozen, checkout
(produce first, cold and frozen last, checkout at the end). `store show`
and `store layout` report which one applies (`layout_source` = `store`,
`chain`, `default`). A layout's aisles are shown for a product that has no
aisle of its own.

### List check and list shop

`list check <l> [-P P | --all-profiles] [--store S]`: every `missing` or
`short` entry (and every `open` wishlist entry) carries the chosen product
for the profile's store: `store`, `store_name`, `source_tier`,
`source_id`, `product`, `brand`, `size`, `price`, `url`, `section`,
`aisle`, `source_note`, and `buy` (the text column: `Sprouts North: Raw
Walnut Halves (bulk, aisle 2; top bins)`, or `no source at Sprouts North`
when no product is linked; `no source` without a store). Other entries
have these fields null. With `--all-profiles` each profile uses its own
store (`--store` applies to all).

`list shop <l> [-P P | --all-profiles] [--store S]` prints the walk: the
missing and short entries (ambiguous and unit-mismatch ones are left out
with a note, as before), grouped by section in the store's layout order,
then by aisle number (no number last), then by name, numbered 1..n across
the walk:

```
Nutty pudding at Sprouts North (sprouts-north), layout: chain Sprouts
Produce
   1. 2 ea      Organic Limes                 aisle 1
Bulk
   2. 200 g     Raw Walnut Halves             aisle 2   top bins
Ask / unsorted
   3. 1 l       milk                          no source at Sprouts North
```

The quantity is what to buy (want, or want - have); when the product's
size converts to the entry's unit, the packs to take are added (`200 g`
of a `16 oz` bag is `1 x 16 oz`). Sections not in the layout come before
`checkout` (else last), in name order. Entries with no product, or a
product with no section, go in a final `ask / unsorted` group. JSON keeps
the `shopping` row shape (`list shop` rows still merge into `shopping
--include-list`), with `step`, `section`, `aisle`, `product`, `packs`,
`store`, `source_tier`, `source_note` added and `vendor` the store's name
when a product was found; `route` in the envelope gives the store and the
layout source per profile.

### Export and import

`export --format json` (and yaml, toon) carries `stores`, `chain_layouts`
and `store_products` arrays next to `data`; `import` of such an envelope
creates or updates them (stores by id, chain layouts by chain, products by
id when the id is free or the same product, else as new rows) in the same
transaction as the items, `--dry-run` included. Lots export and import
their `vendor`. Homebox CSV is unchanged (stores do not travel).

## Insurance report

Per property and/or per owner entity: every owned item with location path,
quantity, maker/model/serial, purchase date/vendor/price, unit value
(replacement value, falling back to purchase price, flagged) and line value
(unit x quantity), photo and receipt present (y/n + paths), subtotal per room,
total per property, a grand total, and a gaps section with counts (no value,
no photo, no receipt, no serial on serialized categories: presets that track
`serial`; "verify still owned" for items bought 10+ years ago still marked
owned). `--min-value` and `--bought-after` filter lines. CSV, XLSX, HTML, PDF
(written in-process with the `pdf-writer` crate, no external binaries).
"Owned durable" = not gone and not a consumable (no lots; no `Kind` =
consumable field, food/consumable preset, or consumable/food label). A rented
property's header says "contents only".

## Intake (later beads)

- Amazon/Gmail receipts: an agent writes a Homebox-format CSV
  with `HB.import_ref = amazon:<order>:<line>`; review with `wherebox import
  --format homebox --dry-run`, then import.
- Photo-to-items.
- Shopping: future purchases flow in through the same receipt path.

Out of scope: any server or sync service. larder (food CLI, init only) is folded into wherebox.
