$ wherebox import examples/homebox.csv
6 row(s): 6 created, 0 updated, 0 unchanged, 0 failed; 7 location(s) and 6 label(s) new, 0 lot(s)

$ wherebox loc ls
TREE           KIND       ITEMS  VALUE
─────────────  ─────────  ─────  ────────
Lake House     property       6  11486.99
  Bedroom      room           1   6800.00
    Closet     container      1   6800.00
  Garage       room           2    238.99
    Shelf 2    container      1    149.99
  Kitchen      room           1   1649.00
  Living Room  room           2   2799.00

$ wherebox add "Pressure Washer" --at Garage --price 329 --bought 2023-04-12 \
    --maker Ryobi --label tools,outdoor
added 000-007 Pressure Washer at Lake House / Garage

$ wherebox ls --label tools
ASSET_ID  NAME             ON_HAND  LOCATION                       STATUS  VALUE   LABELS
────────  ───────────────  ───────  ─────────────────────────────  ──────  ──────  ─────────────
000-007   Pressure Washer        1  Lake House / Garage            owned   329.00  outdoor,tools
000-002   Rolling Toolbox        1  Lake House / Garage            owned    89.00  tools
000-001   Cordless Drill         1  Lake House / Garage / Shelf 2  owned   149.99  power,tools

$ wherebox mv "Pressure Washer" --to "Lake House / Workshop"
created location Lake House / Workshop
moved 1 item(s) to Lake House / Workshop

$ wherebox show washer --columns asset_id,name,location,purchase_price,labels
asset_id        000-007
name            Pressure Washer
location        Lake House / Workshop
labels          outdoor,tools
purchase_price  329.00

$ wherebox history washer --columns kind,detail
KIND     DETAIL
───────  ──────────────────────────────────────────────────────────────────────
created  {"location":"Lake House / Garage","via":"add"}
moved    {"from":"Lake House / Garage","to":"Lake House / Workshop","via":"mv"}

$ wherebox ls --warranty-before 2026-06-01 --columns asset_id,name,warranty_expires
ASSET_ID  NAME          WARRANTY_EXPIRES
────────  ────────────  ────────────────
000-006   Refrigerator  2026-03-02
000-003   OLED TV, 65"  2025-11-24

$ sh examples/pantry.sh
created location Lake House / Kitchen / Fridge
added 000-008 Milk at Lake House / Kitchen / Fridge
expires 2026-01-04 (milk: 7 days in the fridge from purchase on 2025-12-28; see `wherebox shelf-life ls`)
added a lot of Milk (1 l, expires 2026-01-07)
milk: 7 days in the fridge from purchase on 2025-12-31
created location Lake House / Garage / Freezer
added 000-009 Chicken Thighs at Lake House / Garage / Freezer
expires 2026-08-15 (raw-poultry: 9 months in the freezer from freezing on 2025-11-15; see `wherebox shelf-life ls`)
created location Lake House / Kitchen / Pantry
added 000-010 Sourdough at Lake House / Kitchen / Pantry
added 000-011 Paper Towels at Lake House / Kitchen / Pantry

$ wherebox eat Milk 1.5
eaten 1.5 l of Milk (1.5 l left)

$ wherebox expiring --within 14d
DUE         DAYS_LEFT  STATE    ITEM       QTY   UNIT  LOCATION                       EXPIRY_BASIS
──────────  ─────────  ───────  ─────────  ────  ────  ─────────────────────────────  ────────────
2025-12-31         -1  expired  Sourdough  1.00  ea    Lake House / Kitchen / Pantry  best_by
2026-01-04          3  soon     Milk       0.50  l     Lake House / Kitchen / Fridge  shelf_life
2026-01-07          6  soon     Milk       1.00  l     Lake House / Kitchen / Fridge  shelf_life

$ wherebox add Ribeye --preset meat.raw --at "Lake House / Kitchen / Fridge"
added 000-012 Ribeye at Lake House / Kitchen / Fridge
expires 2026-01-04 (raw-meat: 3 days in the fridge from purchase on 2026-01-01; see `wherebox shelf-life ls`)

$ wherebox shelf-life show milk --columns name,group,fridge,freezer,opened,origin,items
name     milk
group    dairy
fridge   7 days
freezer  3 months
opened   7 days
origin   built-in
items    Milk

$ wherebox par Milk 2
par level of Milk: 2 l

$ wherebox shopping --checklist
- [ ] Milk: 0.5 l (below par: 1.5 of 2 l on hand)

$ wherebox report warranties --expiring 1y --columns warranty_expires,days_left,name,location
WARRANTY_EXPIRES  DAYS_LEFT  NAME          LOCATION
────────────────  ─────────  ────────────  ────────────────────
2026-03-02               60  Refrigerator  Lake House / Kitchen

$ wherebox export --label electronics
HB.import_ref,HB.location,HB.label,HB.quantity,HB.name,HB.asset_id,HB.description,HB.insured,HB.serial_number,HB.model_number,HB.manufacturer,HB.notes,HB.purchase_from,HB.purchase_price,HB.purchase_time,HB.lifetime_warranty,HB.warranty_expires,HB.warranty_details,HB.sold_to,HB.sold_time,HB.sold_price,HB.sold_notes,HB.field.Owner Entity,HB.field.Replacement Value,HB.field.Status
01JAEXAMP1E000000000000003,Lake House / Living Room,electronics,1,"OLED TV, 65""",000-003,Wall mounted,true,LG65-9911,OLED65C3,LG,,Costco,1899.99,2023-11-24,false,2025-11-24,,,,,,Example Family Trust,1500.00,owned
