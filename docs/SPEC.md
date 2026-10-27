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
| `locations` | `id`, `name`, `parent_id`, `kind` (property, room, container, shelf), `address` (property only) | path segments of `HB.location` |
| `labels`, `item_labels` | `name` | `HB.label` (comma list) |
| `item_fields` | `item_id`, `name`, `value` | `HB.field.<name>` |
| `attachments` | `item_id`, `kind` (photo, receipt, manual, warranty, other), `path`, `sha256`, `primary` | not in CSV |
| `sources` | `item_id`, `source` (amazon-email, manual, photo, ...), `ref` (order / message id) | `HB.field.Source` |
| `events` | append-only: created, moved, edited, status change, valued | - |

Additional tables for consumables (food, household):

| Table | Columns | Homebox column |
|---|---|---|
| `items` (more) | `kind_preset` (see Presets), `unit` (ea, g, kg, ml, l, lb, oz, pack), `par_level` (keep at least N on hand), `reorder_qty`, `preferred_vendor`, `product_ref` (UPC/ASIN), `shelf_life` (a shelf-life type, `none` = never perishable, unset = classify by name; see Shelf life) | `HB.field.Preset`, `HB.field.Unit`, `HB.field.Par Level`, `HB.field.Reorder Qty`, `HB.field.Vendor`, `HB.field.Product Ref`, `HB.field.Shelf Life` |
| `lots` | `item_id`, `qty`, `acquired_at`, `opened_at`, `best_by` (printed best-by override, perishables only), `frozen_at`, `thawed_at`, `confirmed_at` (last `lot confirm`), `location_id` (fridge / freezer / pantry shelf), `status` (sealed, open, consumed, discarded), `unit_price`, `source_ref` (the purchase row, unique). `expires_at` is a legacy column, always empty since schema v3: expiry is computed, never stored | flattened on export: one HB row per open lot, `HB.field.Best By`, `HB.field.Lot Price`, `HB.field.Lot Ref` |
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
wherebox add <name> --at "Lake House / Garage" [--qty --price --bought --from --serial --model --maker --owner --label ...]
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
wherebox shelf-life ls | show <type> | set <type> [--pantry D] [--fridge D] [--freezer D] [--opened D] [--synonym S]... | reset <type>
wherebox lot add|ls|edit|rm|confirm
wherebox par <item> <n> [--reorder Q]
wherebox shopping [--profile P | --all-profiles] [--within 7d] [--since 18] [--checklist] [--include-list L]
wherebox list new|ls|show|rm|add|edit|drop|check|shop|pin|unpin|explain|import   # see Lists and matching
wherebox match <query> | cohort new|ls|show|add|rm
wherebox report insurance [--profile P | --all-profiles] [--owner O] [--min-value N] [--bought-after DATE] [--format csv|xlsx|pdf|html]
wherebox db regroup-consumables [--dry-run]
wherebox report warranties [--expiring 90d] [--all-profiles] [--include-expired]
wherebox doctor | key | profile | config | completions
```

Every command takes `--json` (`wherebox/v1` envelope) for agents.

## Presets

A preset is a TOML file (`presets/kinds/*.toml`, user-overridable in the
config dir) that says which dates and fields a kind of item tracks, sensible
defaults, and its alerts. A food preset may name a default shelf-life type
(`shelf_life_type = "raw-beef"`); presets never store lot dates (see Shelf
life). `wherebox preset ls` lists
them, and `wherebox preset show meat.raw` answers "what does this kind
track". Starting set:

| Preset | Tracks | Defaults / alerts |
|---|---|---|
| `meat.raw`, `poultry.raw`, `seafood.raw` | use-by, frozen_at | fridge 1-5 days, freezer 4-12 months by cut (USDA FoodKeeper) |
| `dairy`, `eggs`, `produce`, `bread`, `leftovers` | best-by, opened_at | shelf life after opening |
| `frozen` | frozen_at, best-by | months |
| `pantry` (dry, canned), `condiment` | best-by, opened_at | months-years; opened shortens it |
| `beverage`, `alcohol` | best-by (beer, wine) | none for spirits |
| `medicine`, `supplement`, `first-aid` | expiration | alert 30 days before |
| `household`, `toiletry` | none (par level only) | restock |
| `filter` (HVAC, water, fridge) | installed_at, replace interval | 1-6 months |
| `battery`, `smoke-detector`, `co-detector`, `fire-extinguisher` | installed_at, replace or inspect interval, expiration | detectors 10 / 7 yrs; extinguisher yearly check |
| `appliance`, `electronics`, `tool`, `furniture` | warranty_expires, serial, service interval | warranty alert |
| `vehicle`, `generator`, `hvac`, `water-heater` | warranty, service interval, registration/inspection | service due |
| `firearm`, `jewelry`, `art`, `collectible` | serial, appraisal date, replacement value | reappraise every 3-5 yrs (insurance scheduling) |
| `document` (passport, license, insurance policy) | expiration | alert 90 days before |
| `seasonal` (paint, sealant, fuel, chemicals) | opened_at, shelf life, hazmat flag | shelf life |
| `plant`, `pet-supply` | care interval | - |

Shelf-life figures live in shelf-life types (below), not presets. Every
default a built-in preset sets cites a source (NFPA, NFPA 10, ENERGY STAR, or
this spec for alert lead times); where no single figure can be cited (CO
alarms, vehicle service, warranty terms, reappraisal intervals) the value is
left unset and the preset's notes say why. Details: docs/presets.md.

## Shelf life

Only perishables expire. Durables and non-perishable consumables (unopened
canned goods, dry goods, cleaning supplies, toiletries, supplements,
medicine, paper goods) have no expiry, and there is no way to give them one.

**Types.** A shelf-life type (`banana`, `milk`, `raw-chicken`,
`cheese-hard`, `opened-canned-food`, ...; about 60 built in, covering common
fruit, vegetables, dairy, eggs, meat, poultry, seafood, bakery, prepared
food and opened condiments) has a description, a group, days per storage
(`pantry`, `fridge`, `freezer`; any may be absent = storing it there is not
recommended), an `opened` figure where opening shortens it, and synonyms for
classification. The defaults ship as an embedded data file
(`shelf-life/types.toml`), conservative (the short end of every published
range), with a comment per group citing USDA FSIS FoodKeeper and the
FDA/FoodSafety.gov cold-storage chart. Durations: `5d`, `2w`, `6m`, `1y`.
A type with only an `opened` figure (`opened-canned-food`,
`opened-condiment`) is shelf-stable until opened.

**Config.** `[shelf_life.<type>]` tables in `config.toml` override any field
of a built-in type (`fridge = "7d"`; `fridge = "none"` removes a figure) or
add a new type (it needs at least one figure). `wherebox shelf-life set
<type> --fridge 7d --freezer 6m` writes that table (with `toml_edit`, the rest
of the file untouched); `shelf-life reset <type>` removes it. `shelf-life
ls` lists every type with its figures and origin (built-in, config, or
built-in+config); `shelf-life show <type>` also lists synonyms and the
source.

**Item -> type.** An item's type is, in order: its `shelf_life` field (set
with `add/edit --shelf-life <type>`, or `none` to say it never perishes;
`edit --clear shelf_life` returns to automatic); nothing for an item marked
`Kind = durable` or with a non-food preset (household, toiletry, medicine,
supplement, tools...); else the best fuzzy match of the item's name against
type names and synonyms (the matching ladder's name tier, tokens as in
"Fuzzy names"), when it scores at least 0.9 and the name has no
shelf-stable marker (`canned`, `can`, `dried`, `dry`, `powder`, `chips`,
`jerky`, `freeze dried`, `shelf stable`, `uht`, `jar`, `jarred`,
`pickled`); else the preset's `shelf_life_type`. Anything left unclassified
is non-perishable. `show` and JSON report the type and how it was found
(`shelf_life_basis`: explicit, name, preset).

**Storage.** Where a lot is kept: the nearest location (the lot's, else its
item's, walking up the tree) whose kind is `fridge`, `freezer` or `pantry`
(location kinds), or whose name says so (`Freezer`, `Chest Freezer`, `Deep
freeze`; `Fridge`, `Refrigerator`; `Pantry`, `Cupboard`, `Larder`).
Default: pantry.

**Expiry is computed, never stored.**

```
expires = base + figure
  in the freezer (and the type has a freezer figure): frozen_at (else acquired_at) + freezer
  else opened (opened_at) and the type has an opened figure: opened_at + opened
  else: (thawed_at if later than acquired_at, else acquired_at) + figure for the storage
```

When the type has no figure for the lot's storage, the shortest of its
pantry/fridge figures applies (conservative), else the freezer figure; a
type with no storage figure at all never expires unopened. A printed best-by
date on the lot (`lot add|edit --best-by`, perishables only) wins over the
computation (`expiry_basis` = `printed`, else `computed`). Moving a lot (or
its item) into a freezer sets `frozen_at` to that day (re-basing the clock;
re-freezing re-bases again) and clears `thawed_at`; moving it out of a
freezer sets `thawed_at`. `--best-by`/`Best By` on a non-perishable is
rejected (`add`, `lot add`, `lot edit`) or dropped with a warning (import);
there is no `--expires`.

Migration (schema v3): lots of non-perishable items lose stored `expires_at`
and `best_by`; lots of perishables keep a stored date only as a printed
best-by override, and only when it differs from what the item's preset
would have filled in (so preset-computed dates are dropped and recomputed);
`expires_at` is emptied everywhere. Classification during the migration
uses the built-in types (config overrides are not visible to it).

## Estimated on hand

Every purchase lot of a consumable used to count as on hand forever. Now
each active lot (sealed or open, qty > 0) is either counted or **presumed
used up**:

- a lot with a computed (or printed) expiry: presumed used up once it is
  past it;
- any other lot (non-perishable): presumed used up once it is older than the
  item's reorder cadence, counted from the latest of `acquired_at`, its last
  logged use and `confirmed_at`. The cadence is the learned one (as in
  `shopping`: median gap of 3+ purchase dates, else the `Recurring` field)
  or else a default: the `[cadence]` config table by preset name, then by
  preset category (`food`, `consumable`, ...), then the `default_cadence`
  setting (`90d`).

Explicit logs always win: `use`/`eat`/`toss` change logged quantities, and
any logged use of a lot restarts its cadence clock. `wherebox lot confirm
<item> [--qty N] [--lot L]` asserts what is on hand today: every active lot
(or lot L) gets `confirmed_at` = today, which restarts its cadence clock
(a perishable past its expiry stays expired: toss it or give it a printed
`--best-by`); with `--qty N` the oldest lots are drawn down (reason `used`,
note `lot confirm`) so that N remains.

An item's `on_hand` is the sum of its counted lots; `on_hand_basis` is
`logged` when no active lot is presumed used up, else `estimated`. Items
without lots: `quantity`, basis `logged`. Text output marks estimates with a
tilde (`~2`); `ls`, `show`, `list check`, `shopping` and `expiring` use the
estimate. JSON of items, lots and list matches carries `on_hand`,
`on_hand_basis`, `expires_at` (computed; null for non-perishables),
`expiry_basis` (`printed`, `computed`, or null), plus `shelf_life`,
`storage` and `presumed_used_up` on lots.

## Groceries and shopping

- `wherebox use <item> [qty]` / `wherebox eat` / `wherebox toss` log
  consumption against the oldest lot first.
- `wherebox expiring [--within 3d]` lists perishable lots (computed
  expiry, freezer included) that expire within the window or already have:
  what to eat or toss. Non-perishables never appear.
- A consumable bought repeatedly is one item with a lot per purchase. On
  import, consumable rows (`HB.field.Kind` = consumable, a food/consumable
  preset or label) sharing `HB.field.ASIN` (else a product ref) at one
  property collapse into one item; each row becomes a lot (qty, acquired =
  purchase date, unit price, the row's import ref as `source_ref`, so
  re-imports are idempotent). Durables stay one item per row. `wherebox db
  regroup-consumables [--dry-run]` migrates existing databases in place and
  records `merged`/`regrouped` events.
- `wherebox shopping [--profile rental-apt]` generates the list: every item
  whose estimated on hand is below `par_level`, anything expiring that has a
  par, plus learned
  reorders: with 3+ purchases (lot dates) the cadence is the median gap; due
  when that has passed since the last purchase and no unused lot was
  acquired within the last cadence. Without that history a `Recurring`
  custom field gives it (`9d`, `monthly`, or `yes`/`true` with `Avg Interval
  Days`). Rows show last bought, cadence and days overdue. Items last bought
  before `--since` months (default 18) are dropped, not due. Output as text,
  a checklist (`--checklist`, Markdown), or a cart-ready CSV (`-f csv`).
  `wherebox par <item> <n>` sets the par level.
- Receipt intake (Amazon, Whole Foods, grocery emails) adds lots
  automatically, so the inventory restocks itself when you buy.

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

Types with behaviour: `recipe` (consumables; expired lots don't count),
`kit` and `packing` (lent items don't count: they are not here to pack),
`wishlist` (see below). Any other string is a custom type with recipe
semantics.

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

Fuzzy names: lower case; punctuation to spaces; pack and size tokens dropped
(`12-pack`, `pack of 6`, `5oz`, `16 fl oz`, `2 lb`, `x3`, bare numbers);
marketing and preparation stopwords dropped (`premium`, `value`, `family
size`, `bulk`, `new`, `brand`, `chopped`, `diced`...; `organic` and `whole`
are kept, they change the product); words singularized (`berries` -> `berry`, `walnuts` ->
`walnut`). Similarity is token-set based: each query token is matched to the
most similar candidate token (strsim Jaro-Winkler, counted when >= 0.94), and
`score = 0.9 * query coverage + 0.1 * candidate coverage`, so a query fully
contained in a long Amazon title scores >= 0.9 and an exact name 1.0.
Candidates are ranked per query (`name` and each `any`). The match is
**ambiguous** when the best two distinct candidates (different normalized
names) are within 0.03 of each other and they don't both contain every query
token (two walnut products that both contain "walnut" are both counted;
"dark chocolate chips" between "Dark Chocolate Bar" and "Dark Chocolate
Chunks", both 0.65, is ambiguous at a threshold of 0.6: pin one, or add an
`exclude`). Ambiguity only
matters when the entry has no match above tier e.

Every match reports its tier (`identifier`, `url`, `pinned`, `attribute`,
`name`; ladder order a-e, highest confidence first; an item matched several
ways reports the best) and score (1.0 above tier e, the similarity at tier
e), and what it matched (`via`, e.g. `cohort nuts: any "pecans"`).

### Have-check

Per matched item: durables (no lots) count `quantity` when owned, stored or
lent (kit/packing: owned or stored). Consumables count their unconsumed lots
(sealed, open) whose location satisfies `at` and that are not presumed used
up (see Estimated on hand); perishable lots past their computed expiry are
reported separately (`expired`) and don't count. A have count that rests on
an estimate shows as `~N` and the match's `on_hand_basis` is `estimated`. Quantities convert
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
mass or volume entry. Any other mismatch is **unit mismatch**: unknown, not
missing.

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
wherebox list new <name> --type <t> [--profile P] [--notes N]
wherebox list ls | show <l> | rm <l>
wherebox list add <l> <entry> [--qty --unit --note --asin --sku --upc --product-ref --url --label --preset --at --cohort --any N... --exclude N... --optional --fuzzy B --threshold T]
wherebox list edit <l> <entry> [same flags] [--name N] [--required] [--clear FIELD]
wherebox list drop <l> <entry>
wherebox list check <l> [-P P | --all-profiles] [--threshold T]
wherebox list shop <l> [-P P | --all-profiles]
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
