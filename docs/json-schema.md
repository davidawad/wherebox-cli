# wherebox JSON interface (`wherebox/v1`)

This is the stable machine interface for scripts and agents. It covers
`--json` / `--format json` and `--format jsonl` output, the error object, and
the process exit codes.

## Stability

* The envelope's `schema` field is `"wherebox/v1"`. Within v1, fields are
  only ever **added**. Nothing is removed, renamed, or changed in type or
  meaning. Consumers should ignore fields they don't know.
* A breaking change will bump the schema to `wherebox/v2`.
* Numbers are JSON numbers at full precision. Money is in dollars (a number
  such as `1299.99`; it is stored as whole cents). `--precision` only affects
  text output.
* Dates are `YYYY-MM-DD` strings; timestamps (`created_at`, event `at`) are
  UTC `YYYY-MM-DDTHH:MM:SSZ`, consumption times wall-clock
  `YYYY-MM-DDTHH:MM:SS` in the configured zone. `--date-format` never affects
  JSON.
* Missing values are `null`. Labels and sources are always arrays of strings;
  custom fields an object of strings.

## Envelope (`--json`)

```json
{
  "schema": "wherebox/v1",
  "kind": "items",
  "generated_at": "2026-01-01T00:00:00Z",
  "count": 2,
  "total_value": 1649.99,
  "data": [ ... ]
}
```

| field          | type            | notes |
|----------------|-----------------|-------|
| `schema`       | string          | always `wherebox/v1` |
| `kind`         | string          | what `data` contains (see below) |
| `generated_at` | string          | UTC timestamp (`$SOURCE_DATE_EPOCH` pins it for reproducible output) |
| `count`        | integer         | present when `data` is an array |
| `data`         | array \| object | the payload |
| *(extra)*      | any             | command-specific metadata, e.g. `total_value`, `query`, `item`, `within_days`, `summary`, `config_path` |

`--json` and `--format json` are the same, except for `import`, where
`--format` names the *input* format and `--json` the output.

## JSON Lines (`--format jsonl`)

JSONL output has no envelope. It prints one `data` element per line, using
the same objects as JSON. A single-object result prints as one line.

## Kinds

| command | kind | `data` |
|---------|------|--------|
| `ls`, `find` | `items` | array of [Item](#item); meta `total_value` |
| `show` | `item` | Item + `inside`, `contains`, `lots`, `consumption`, `attachments`, `history` |
| `add`, `edit`, `rm`, `value` | `item` | Item |
| `rm --purge` | `purged` | `{id, name}` |
| `mv` | `moved` | array of Item |
| `history` | `history` | array of `{at, kind, detail}`; meta `item` |
| `loc ls` | `locations` | array of `{id, path, name, kind, address, notes, tree, depth, items_here, items, value}`; meta `total_value` |
| `loc add` / `mv` / `edit` | `location` | `{id, path, name, kind, address, notes}` |
| `label ls` / `add` | `labels` / `label` | `{name, items, description}` |
| `attach`, `attachment ls` | `attachment` / `attachments` | [Attachment](#attachment) |
| `attachment get` | `attachment_get` | `{id, dest, bytes}` |
| `lot add` / `edit` / `ls` | `lot` / `lots` | [Lot](#lot) + `item`, `asset_id`, `item_id` |
| `use`, `eat`, `toss` | `consumption` | `{item, item_id, reason, qty, unit, on_hand, lots: [{lot, taken, left, status}]}` |
| `lot confirm` | `confirmed` | `{item, item_id, confirmed_on, qty, unit}` + the item's `shelf_life`, `shelf_life_basis`, `on_hand`, `on_hand_basis`, `expires_at`, `expiry_basis` |
| `expiring` | `expiring` | array of `{item, asset_id, lot, qty, unit, due, expires_at, expiry_basis, expiry_detail, shelf_life, days_left, state, on_hand_basis, used_up, storage, frozen_at, location}` (perishables only; `due` = `expires_at`, computed); meta `within_days` |
| `shelf-life ls` / `show` / `set` / `unset` | `shelf_life` | `{name, group, pantry, fridge, freezer, opened, origin, days: {pantry, fridge, freezer, opened}}` (durations as text such as `7 days`; `days` numeric; `show` adds `synonyms`, `summary`, `items`) |
| `export -f json\|jsonl\|yaml\|toon` | `export` | array of Item + `lots` (all lots, any status); meta `profile`; json/yaml/toon also `stores` (every Store column), `chain_layouts` (`[{chain, layout}]`), `store_products` (every column, `price` in dollars) and `receipts` (every column, money in dollars, `image` base64 or null), which `import` reads back |
| `import` | `import` | array of `{line, action, id, asset_id, name, location, lots, error}`; meta `format`, `dry_run`, `batch`, `summary`, `stores`, `receipts` (how many were read from the envelope), `ignored_columns`, `warnings` |
| `profile ls` / `show` / `init` | `profiles` / `profile` / `profile_init` | `{name, title, address, rented, db_path, exists, active, source}` (+ `items`, `locations`, `value` in `show`) |
| `preset ls` | `presets` | array of `{name, category, description, tracks, shelf_life, intervals, alert_days, origin}` (`shelf_life`: the default shelf-life type; `intervals`: text such as `replace every 10 years`) |
| `preset show` | `preset` | the preset as in its TOML file: `{name, description, category, tracks, alert_days, notes, shelf_life (a type name, or an inline table in old user files), intervals: {replace, inspect, service, appraise, warranty}, defaults: {unit, labels, fields}, sources: [{covers, cite, url}], origin}` |
| `par` | `item` | Item |
| `shopping` | `shopping` | array of `{item, qty, unit, vendor, product_ref, reason, detail, last_bought, cadence_days, overdue_days, location, profile, asset_id}`; `reason` ∈ `below par, expiring, reorder, list` (`list`: from `--include-list`; the last three fields are set on reorders); meta `within_days`, `dropped` (recurring items last bought before `--since`) |
| `list new` | `list` | `{slug, name, type, profile, notes, threshold, food, entries, path}` (`food`: only food matches by attribute or name) |
| `list ls` | `lists` | array of the list header + `open`, `fulfilled` (wishlists; else null) |
| `list show`, `list import` | `list` | array of entries `{n, name, qty, unit, optional, note, matcher, asin, sku, upc, product_ref, url, any, label, preset, at, cohort, exclude, pins, fuzzy, threshold}` (matcher fields are arrays; `matcher` a one-line summary); meta `list` (the header), `skipped` (import) |
| `list add` / `edit` / `pin` / `unpin` | `list_entry` | one entry + `list` (slug) |
| `list drop`, `list rm`, `cohort rm` | `removed` | the entry; for files `{path, trashed_to}` |
| `list check` | `list_check` | array of `{profile, n, entry, want, unit, have, on_hand_basis, used_up, expired, state, status, optional, ambiguous, matched, matches, note}`; `have` counts estimated on hand (`on_hand_basis` `estimated` when a matched lot was presumed used up, `used_up` how much; the table shows `~6`), `expired` lots confirmed on hand after their expiry; `state` ∈ `have short missing ambiguous unit-mismatch` (wishlists: `open fulfilled`), `status` the text shown (`short (have ~0.5 of 2)`, `missing (~1.9 l presumed used up)`, `missing (lent out)`); each of `matches` is `{item, item_id, asset_id, location, status, tier, score, via, qty, on_hand, item_unit, lent, expired, basis, on_hand_basis, used_up}` with `tier` ∈ `identifier url pinned attribute name`, `qty` in the entry's unit (null: unit mismatch), `basis` ∈ `unit, size from name, unit mismatch`; meta `list`, `threshold`, `ready`, `verdict` (`ready` / `missing N of M`), `verdicts` (`{profile: {ready, verdict, store}}`). Every row also carries the sourcing fields `{store, store_name, source_tier, source_link, source_id, source_at, product, brand, size, price, url, section, aisle, source_note, packs, buy}`, set on `missing`, `short` and `open` rows (null otherwise; `product` null and `buy` `no source at <store>` when nothing is linked): `source_tier` ∈ `store chain other`, `aisle` the product's or its section's in the layout, `packs` (`2 x 8 oz`) when the product's size converts |
| `list shop` | `shopping` | rows as `shopping`, `reason` = `list`, in walking order, plus `step`, `group` (section title, or `Ask / unsorted`) and the sourcing fields below (`vendor` = where the chosen product is); meta `list`, `route` (`[{profile, store, store_name, layout_source, steps}]`, `layout_source` ∈ `store chain default`) |
| `store add` / `edit` / `show`, `store ls` | `store` / `stores` | `{id, chain, name, address, website, notes, layout, layout_source, products, profiles}`; `store show` adds meta `products` (Store product records) |
| `store layout` | `store_layout` | array of `{n, section, title, aisles}`; meta `layout`, `layout_source`, `store`, `chain` |
| `store use` | `profile_stores` | `{profile, stores}` |
| `source add` / `edit`, `source ls` | `store_product` / `store_products` | `{id, food, store, store_name, chain, name, brand, size, price, price_seen, url, sku, plu, upc, section, aisle, notes, item_id, at}` (`at`: the store's name, or the chain); `source add` adds `section_guessed`, `section_basis` (`category url name food`); `source ls FOOD` adds `link` (`name item head noun`) |
| `receipt import`, `receipt apply` | `receipt_import` | array of lines `{file, n, text, name, code, code_kind, qty, unit, size, unit_price, total, discount, voided, action, item, item_id, tier, confidence, shelf_life, lot, lot_qty, lot_unit}` (`action` ∈ `add new skip void`, `tier` ∈ `code product food item new edited`, money in dollars, `lot` set when a lot was written); meta `dry_run`, `receipts` (`[{file, kind, receipt_id, fingerprint, chain, store, date, time, subtotal, tax, total, review_file, lots, items_created, products_created, products_updated, below_threshold, skipped: [{n, text, why, amount}], warnings}]`) |
| `receipt ls`, `receipt show` | `receipts` / `receipt` | `{id, date, time, store, chain, subtotal, tax, total, lines, lots, file, image, fingerprint, created_at}`; `show` adds meta `lots` (`[{line, lot, item, asset_id, item_id, qty, unit, unit_price, status, acquired_at, vendor}]`) |
| `list explain` | `explain` | array of `{item, asset_id, item_id, location, identifier, url, pinned, attribute, name, name_score, excluded, location_ok, food, tier, score, via, result}` (`result` `not food (food list)` when only the food gate kept it out) (each tier: what hit, else null); meta `entry`, `threshold`, `ambiguous` |
| `match` | `match` | array of `{item, asset_id, item_id, tier, score, via, on_hand, unit, expired, location, status, profile}`; meta `query`, `threshold`, `ambiguous` |
| `cohort new` / `show` / `add`, `cohort ls` | `cohort` / `cohorts` | `{slug, name, notes, matcher, includes, path}` + the matcher fields |
| `report insurance` | `insurance` | `{header, properties: [{name, profile, address, rented, coverage, rooms: [{room, items, subtotal}], total}], grand_total, flagged, gap_counts: {gap: n}, gaps: [{property, location, name, asset_id, gaps}]}`; each of `items` is `{section, property, coverage, room, location, name, asset_id, quantity, manufacturer, model_number, serial_number, purchase_time, purchase_from, purchase_price, replacement_value, unit_value, line_value, value_basis, flag, photo, photo_paths, receipt, receipt_paths, owner_entity, gaps, profile}`; meta `header`. `--format csv\|tsv\|jsonl` stream the same item records plus `room subtotal`, `property total`, `grand total` (with `line_value`) and `gap` rows (the `section` column) |
| `report warranties` | `warranties` | array of `{warranty_expires, days_left, state, name, asset_id, location, manufacturer, model_number, serial_number, purchase_time, purchase_from, warranty_details, receipt, profile}`; meta `within_days` |
| `*  rm` | `removed` | what was removed |
| `config show` | `config` | `{key, value, source}`; `source` ∈ `default, file, env, flag` |
| `config keys` / `path` / `set` / `unset` | `config_keys` / `config_path` / `config_set` / `config_unset` | |
| `db path/init/migrate/backup/vacuum/check/info/encrypt/rekey` | `db_path` / `db_init` / `migrations` / `db_backup` / `db_vacuum` / `db_check` / `db_info` / `db_encrypt` / `db_rekey` | |
| `db regroup-consumables` | `regroup` | array of `{property, product, id, asset_id, name, merged, lots_created, lots_moved}`, one per consumable regrouped; meta `dry_run` |
| `key status`, `doctor`, `audit log` | `key_status`, `doctor`, `audit_log` | |

Mutating commands print nothing on stdout in table mode (the status line goes
to stderr); with any other format they print the record shown above.

### Item

| field | type | notes |
|-------|------|-------|
| `id` | string | ULID, stable forever |
| `asset_id` | string | `000-001`, assigned on creation |
| `name`, `description` | string | |
| `quantity` | integer | for items with lots: what is left on hand, rounded up |
| `location` | string \| null | full path, `Lake House / Garage / Shelf 2` |
| `status` | string | `owned lent stored disposed sold lost gifted` |
| `value` | number \| null | replacement value, else purchase price |
| `labels` | array of string | sorted |
| `owner_entity`, `manufacturer`, `model_number`, `serial_number`, `purchase_from` | string \| null | |
| `purchase_price`, `replacement_value`, `sold_price` | number \| null | dollars |
| `purchase_time`, `valued_at`, `warranty_expires`, `sold_time` | string \| null | `YYYY-MM-DD` |
| `insured`, `lifetime_warranty` | bool | |
| `warranty_details`, `sold_to`, `sold_notes`, `notes` | string \| null | |
| `kind_preset`, `unit`, `preferred_vendor`, `product_ref` | string \| null | |
| `par_level`, `reorder_qty` | number \| null | |
| `parent_item_id` | string \| null | the item this one is stored inside |
| `import_ref` | string | `HB.import_ref`; the id unless imported with another ref |
| `fields` | object | custom fields |
| `sources` | array of string | `source` or `source:ref` |
| `created_at`, `updated_at` | string | UTC timestamps |
| `shelf_life`, `shelf_life_basis` | string \| null | the shelf-life type; how it was found: `field` (`--shelf-life`), `name`, `preset`. Null: non-perishable |
| `on_hand`, `on_hand_basis` | number, string | estimated on hand; `logged`, or `estimated` when a lot was presumed used up (text output shows `~6`). `quantity` stays the logged quantity |
| `expires_at`, `expiry_basis` | string \| null | computed: the earliest expiry of the counted lots; `best_by` (printed) or `shelf_life` |

### Lot

`{id, qty, unit, status, acquired_at, opened_at, best_by, frozen_at,
confirmed_at, location, unit_price, source_ref, vendor, expires_at, expiry_basis,
expiry_detail, storage, used_up, used_up_why, on_hand_basis}`; `status` ∈
`sealed open consumed discarded`, `location` is the lot's own place or (in
`lot ls`) the item's, `unit_price` is dollars for one unit, `source_ref` the
purchase row (import ref) the lot came from, `vendor` where it was bought (a store id or text). `best_by` is the printed date
(perishables only); `expires_at` is computed (`expiry_basis` `best_by` or
`shelf_life`, `expiry_detail` says how), `storage` is `pantry`, `fridge` or
`freezer`, `used_up` whether the lot is presumed used up (and why).

### Attachment

`{id, item_id, kind, path, filename, sha256, bytes, encrypted, primary,
created_at}`; `kind` ∈ `photo receipt manual warranty other`, `path` is
relative to the attachments directory.

## Errors

On failure, a human-readable message goes to stderr. If the effective format
is `json` or `jsonl` (from `--json`, the flag, environment, or config file),
stderr instead gets a single JSON line:

```json
{"schema": "wherebox/v1", "kind": "error", "error": {"kind": "not_found", "code": 3, "message": "no item 'ghost' (see: wherebox find ghost)"}}
```

## Exit codes

| code | kind        | meaning |
|------|-------------|---------|
| 0    |             | success |
| 1    | `general`   | unspecified failure |
| 2    | `usage`     | bad arguments or invalid flag combination |
| 3    | `not_found` | unknown item, location, label, lot, attachment |
| 4    | `invalid`   | invalid data: unparsable amount/date, unknown status, ambiguous name, more than on hand, import aborted because of invalid rows |
| 5    | `database`  | database engine error, or `db check` failed |
| 6    | `io`        | file read/write failure |
| 7    | `config`    | bad config file, key or value (including a bad profile name) |
| 8    | `key`       | encrypted database cannot be unlocked: no key available or wrong key |

`list check` also exits 3 when the list is not ready (an entry that is not
optional is short, missing, ambiguous or a unit mismatch), with its normal
output on stdout and no error object; with `--all-profiles`, when no property
is ready. Wishlists always exit 0.

## Example (shell)

```sh
wherebox ls --at Garage --json | jq -r '.data[] | "\(.asset_id)\t\(.name)\t\(.value // "")"'
wherebox expiring --within 3d --json | jq -r '.data[] | select(.state == "expired") | .item'
```

## Receipt review file (`wherebox/receipt-review/v1`)

Written by `receipt import` without `--yes` (or with `--review FILE`), read
by `receipt apply`. `{schema, receipt: {fingerprint, file, kind, chain,
store, date, time, subtotal, tax, total, keep_image, warnings, ocr_text},
lines: [{n, text, name, code, code_kind, qty, unit, size, unit_price, total,
discount, voided, tier, confidence, action, item, item_name, new_name,
shelf_life, product_id}], skipped: [{n, text, why, amount}]}`. Edit
`action` (`add` to `item`, an id, asset id or name; `new` named `new_name`
with shelf-life type `shelf_life`; `skip`) and `qty`; money is in dollars.
