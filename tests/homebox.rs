//! Homebox CSV interop: the export matches a golden file, export -> import
//! -> export is byte-stable, and re-imports update instead of duplicating.
//!
//! Regenerate the golden with `WHEREBOX_BLESS=1 cargo test --test homebox`.

mod support;

use support::{check_golden, fixture, row, rows, Env};

/// The documented Homebox columns (import-csv doc): the special columns, then
/// the standard table in order.
const HOMEBOX_COLUMNS: &[&str] = &[
    "HB.import_ref",
    "HB.location",
    "HB.label",
    "HB.quantity",
    "HB.name",
    "HB.asset_id",
    "HB.description",
    "HB.insured",
    "HB.serial_number",
    "HB.model_number",
    "HB.manufacturer",
    "HB.notes",
    "HB.purchase_from",
    "HB.purchase_price",
    "HB.purchase_time",
    "HB.lifetime_warranty",
    "HB.warranty_expires",
    "HB.warranty_details",
    "HB.sold_to",
    "HB.sold_time",
    "HB.sold_price",
    "HB.sold_notes",
];

fn header(csv: &str) -> Vec<String> {
    let mut r = csv::Reader::from_reader(csv.as_bytes());
    r.headers().unwrap().iter().map(str::to_string).collect()
}

#[test]
fn export_matches_golden() {
    let e = Env::pinned();
    e.seed();
    let out = e.run(&["export"]);
    check_golden("homebox-export.csv", &out);
    // --format homebox is the same thing.
    assert_eq!(e.run(&["export", "--format", "homebox"]), out);
    let h = header(&out);
    assert_eq!(&h[..HOMEBOX_COLUMNS.len()], HOMEBOX_COLUMNS);
    assert!(h[HOMEBOX_COLUMNS.len()..].iter().all(|c| c.starts_with("HB.field.")), "{h:?}");
}

#[test]
fn export_import_export_is_byte_stable() {
    let a = Env::pinned();
    a.seed();
    // Add state the fixture lacks: a custom field, a source, a moved and a
    // consumed lot, a value, an item with a fractional lot elsewhere.
    a.run(&["edit", "Red Toolbox", "--field", "Drawer Count=5", "--source", "amazon-email:113-7781"]);
    a.run(&["value", "Cordless Drill", "120", "--on", "2025-06-01"]);
    a.run(&["eat", "Milk", "1.5"]);
    a.run(&[
        "add",
        "Ribeye",
        "--at",
        "Freezer",
        "--unit",
        "lb",
        "--lot-qty",
        "1.25",
        "--frozen",
        "2025-12-20",
        "--best-by",
        "2026-06-01",
    ]);
    let first = a.run(&["export"]);

    let file = a.path("export.csv");
    std::fs::write(&file, &first).unwrap();
    let b = Env::pinned();
    let v = b.json(&["import", file.to_str().unwrap()]);
    assert_eq!(v["summary"]["failed"], 0, "{v}");
    let second = b.run(&["export"]);
    assert!(first == second, "round trip changed the export:\n--- first\n{first}\n--- second\n{second}");

    // A second import of the same file changes nothing.
    let again = b.json(&["import", file.to_str().unwrap()]);
    assert_eq!(again["summary"]["created"], 0);
    assert_eq!(again["summary"]["updated"], 0);
    assert_eq!(b.run(&["export"]), first);
    // And the source database agrees it is all unchanged.
    let same = a.json(&["import", file.to_str().unwrap()]);
    assert_eq!(same["summary"]["unchanged"], same["summary"]["rows"], "{same}");
}

#[test]
fn import_maps_every_homebox_feature() {
    let e = Env::pinned();
    let v = e.json(&["import", fixture("homebox-import.csv").to_str().unwrap(), "--dry-run"]);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["summary"]["created"], 7, "{v}");
    assert_eq!(v["summary"]["lots"], 2);
    assert_eq!(e.json(&["ls", "--all"])["count"], 0, "dry run wrote nothing");

    e.seed();
    let drill = e.json(&["show", "000-001"])["data"].clone();
    assert_eq!(drill["id"], "01J9W000000000000000000001");
    assert_eq!(drill["location"], "Lake House / Garage / Shelf 2");
    assert_eq!(drill["labels"], serde_json::json!(["power", "tools"]));
    assert_eq!(drill["purchase_price"], 149.99);
    assert_eq!(drill["insured"], true);
    assert_eq!(drill["owner_entity"], "Awad Family Trust");
    assert_eq!(drill["fields"]["Color"], "yellow");
    let tv = e.json(&["show", "000-004"])["data"].clone();
    assert_eq!(tv["name"], "OLED TV, 65\"");
    assert_eq!(tv["purchase_price"], 1899.99);
    assert_eq!(tv["replacement_value"], 1500.0);
    assert_eq!(tv["value"], 1500.0);
    // The parent marker in the notes becomes the item-in-item link.
    let sockets = e.json(&["show", "Socket Set"])["data"].clone();
    assert_eq!(sockets["notes"], "Metric and SAE");
    assert_eq!(sockets["parent_item_id"], "01J9W000000000000000000002");
    // Lot rows fold into one item.
    let milk = e.json(&["show", "Milk"])["data"].clone();
    assert_eq!(milk["quantity"], 3, "ceil(1.5 + 1)");
    assert_eq!(milk["lots"].as_array().unwrap().len(), 2);
    assert_eq!(milk["lots"][0]["qty"], 1.5);
    assert_eq!(milk["lots"][0]["status"], "open");
    // Sold items are kept but hidden from ls by default.
    assert_eq!(e.json(&["ls"])["count"], 6);
    assert_eq!(e.json(&["ls", "--status", "sold"])["data"][0]["sold_price"], 75.0);
    // The location tree was built from the paths.
    let locs = e.json(&["loc", "ls", "--flat"]);
    let paths: Vec<&str> = rows(&locs).iter().map(|r| r["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"Lake House / Kitchen / Fridge"), "{paths:?}");
    assert_eq!(row(&locs, "path", "Lake House")["kind"], "property");
    assert_eq!(row(&locs, "path", "Lake House / Garage")["kind"], "room");
}

#[test]
fn reimport_updates_by_import_ref() {
    let e = Env::pinned();
    e.seed();
    let csv = e.path("update.csv");
    std::fs::write(
        &csv,
        "HB.import_ref,HB.name,HB.location,HB.purchase_price,HB.label\n\
         01J9W000000000000000000001,Cordless Drill,Lake House / Workshop,139.00,tools\n\
         amazon:113-1:1,USB-C Charger,Lake House / Office,19.99,\n",
    )
    .unwrap();
    let v = e.json(&["import", csv.to_str().unwrap()]);
    assert_eq!(v["summary"]["updated"], 1, "{v}");
    assert_eq!(v["summary"]["created"], 1);
    let drill = e.json(&["show", "000-001"])["data"].clone();
    assert_eq!(drill["location"], "Lake House / Workshop");
    assert_eq!(drill["purchase_price"], 139.0);
    assert_eq!(drill["labels"], serde_json::json!(["tools"]));
    assert_eq!(drill["serial_number"], "DCD777-0042", "columns not in the file are kept");
    let kinds: Vec<String> =
        rows(&e.json(&["history", "000-001"])).iter().map(|r| r["kind"].as_str().unwrap().to_string()).collect();
    assert!(kinds.contains(&"moved".to_string()) && kinds.contains(&"edited".to_string()), "{kinds:?}");
    // A foreign import ref (receipt intake) is kept as the item's ref.
    let charger = e.json(&["show", "amazon:113-1:1"])["data"].clone();
    assert_eq!(charger["import_ref"], "amazon:113-1:1");
    assert_eq!(charger["asset_id"], "000-008");
    // Importing it again matches the same item.
    assert_eq!(e.json(&["import", csv.to_str().unwrap()])["summary"]["unchanged"], 2);
}

#[test]
fn rows_without_import_ref_and_tsv() {
    let e = Env::new();
    let tsv = e.path("items.tsv");
    std::fs::write(&tsv, "HB.name\tHB.location\tHB.quantity\nLadder\tHouse / Garage\t1\nRake\tHouse / Garage\t2\n")
        .unwrap();
    let v = e.json(&["import", tsv.to_str().unwrap()]);
    assert_eq!(v["format"], "homebox");
    assert_eq!(v["summary"]["created"], 2);
    // Without refs Homebox (and wherebox) cannot dedupe: a second import duplicates.
    e.run(&["import", tsv.to_str().unwrap()]);
    assert_eq!(e.json(&["ls"])["count"], 4);
    // HB.labels (Homebox's own export spelling) and unknown columns.
    let csv = e.path("hb.csv");
    std::fs::write(&csv, "HB.name,HB.labels,HB.archived,HB.url\nShovel,\"garden,tools\",false,\n").unwrap();
    let v = e.json(&["import", csv.to_str().unwrap()]);
    assert_eq!(v["ignored_columns"], serde_json::json!(["HB.archived", "HB.url"]));
    assert_eq!(e.json(&["show", "Shovel"])["data"]["labels"], serde_json::json!(["garden", "tools"]));
}

#[test]
fn bad_rows_abort_or_are_skipped() {
    let e = Env::new();
    let csv = e.path("bad.csv");
    std::fs::write(&csv, "HB.name,HB.purchase_price,HB.purchase_time\nGood,10,2024-01-01\nBad,ten,2024-01-01\n")
        .unwrap();
    let err = e.fail(&["import", csv.to_str().unwrap()], 4);
    assert!(err.contains("line 3") && err.contains("purchase_price"), "{err}");
    assert_eq!(e.json(&["ls"])["count"], 0, "an aborted import writes nothing");
    let v = e.json(&["import", csv.to_str().unwrap(), "--skip-invalid"]);
    assert_eq!(v["summary"]["created"], 1);
    assert_eq!(v["summary"]["failed"], 1);
}

/// A Homebox CSV whose HB.label uses ';' (seen in an agent-made file) gets
/// two labels, not one "a;b", and the import warns naming the row.
#[test]
fn semicolon_labels_are_split_with_a_warning() {
    let e = Env::new();
    let f = e.path("agent.csv");
    std::fs::write(
        &f,
        "HB.location,HB.label,HB.name\nHouse / Garage,tools;garage,Drill\nHouse / Garage,\"tools, garage\",Saw\n",
    )
    .unwrap();
    let out = e.out(&["import", f.to_str().unwrap(), "--input-format", "homebox"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("warning: line 2: HB.label \"tools;garage\""), "{err}");
    assert!(!err.contains("line 3"), "comma-separated labels are fine: {err}");
    let drill = e.json(&["show", "Drill"]);
    assert_eq!(drill["data"]["labels"], serde_json::json!(["garage", "tools"]));
    let labels = e.json(&["label", "ls"]);
    assert_eq!(rows(&labels).len(), 2, "{labels}");
}
