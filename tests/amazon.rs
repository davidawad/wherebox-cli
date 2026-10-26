//! Real-world shaped input: a Homebox CSV made from Amazon orders. Repeat
//! purchases of a consumable are one item with a lot per purchase (and
//! `db regroup-consumables` migrates databases imported before that), the
//! shopping list learns their cadence, and the insurance report leaves them
//! out.

mod support;

use serde_json::Value;
use support::{fixture, row, rows, Env};

const TOWELS: &str = "Paper Towels 6-pack";

fn import(e: &Env) -> Value {
    e.json(&["import", fixture("amazon-orders.csv").to_str().unwrap()])
}

/// Items by name (and property), with their lots.
fn items(e: &Env) -> Vec<Value> {
    rows(&e.json(&["export", "--format", "json"])).clone()
}

fn named<'a>(all: &'a [Value], name: &str, property: &str) -> &'a Value {
    all.iter()
        .find(|i| i["name"] == name && i["location"].as_str().unwrap_or("").starts_with(property))
        .unwrap_or_else(|| panic!("no {name} at {property}"))
}

fn lot_refs(item: &Value) -> Vec<(String, String, f64, f64)> {
    item["lots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["source_ref"].as_str().unwrap_or("").to_string(),
                l["acquired_at"].as_str().unwrap_or("").to_string(),
                l["qty"].as_f64().unwrap(),
                l["unit_price"].as_f64().unwrap_or(0.0),
            )
        })
        .collect()
}

#[test]
fn consumable_purchases_are_lots_on_one_item() {
    let e = Env::pinned();
    let v = import(&e);
    let s = &v["summary"];
    assert_eq!((s["rows"].as_u64(), s["created"].as_u64()), (Some(18), Some(10)), "{s}");
    let all = items(&e);
    assert_eq!(all.len(), 10);
    let towels = named(&all, TOWELS, "Lake House");
    assert_eq!(towels["import_ref"], "amazon:111-0000001-0000001:B0TOWEL001", "the first purchase's ref");
    assert_eq!(towels["quantity"], 8);
    assert_eq!(towels["purchase_time"], "2025-09-04", "the latest purchase's fields");
    assert_eq!(towels["fields"]["Purchase Count"], "4");
    assert_eq!(
        lot_refs(towels),
        [
            ("amazon:111-0000001-0000001:B0TOWEL001".to_string(), "2025-06-01".to_string(), 2.0, 19.99),
            ("amazon:111-0000002-0000002:B0TOWEL001".into(), "2025-07-01".into(), 2.0, 19.99),
            ("amazon:111-0000003-0000003:B0TOWEL001".into(), "2025-08-05".into(), 2.0, 21.49),
            ("amazon:111-0000004-0000004:B0TOWEL001".into(), "2025-09-04".into(), 2.0, 21.49),
        ]
    );
    // The same product at another property is another item.
    assert_eq!(lot_refs(named(&all, TOWELS, "Rental apartment")).len(), 1);
    // A food label makes a consumable without the Kind field.
    assert_eq!(lot_refs(named(&all, "Olive Oil 1L", "Lake House")).len(), 2);
    // Durables stay one item per row, without lots.
    let scales: Vec<&Value> = all.iter().filter(|i| i["name"] == "Kitchen Scale").collect();
    assert_eq!(scales.len(), 2);
    assert!(scales.iter().all(|s| s["lots"].as_array().unwrap().is_empty()));
    assert_eq!(named(&all, "Dining Chair", "Lake House")["quantity"], 4);

    // Re-importing the same file changes nothing.
    let v = import(&e);
    let s = &v["summary"];
    assert_eq!((s["created"].as_u64(), s["updated"].as_u64(), s["unchanged"].as_u64()), (Some(0), Some(0), Some(10)));
    assert_eq!(items(&e).len(), 10);

    // A later order file adds a lot to the same item, and keeps what was used.
    e.run(&["use", "Coffee Pods 72ct", "1"]);
    let later = e.path("later.csv");
    let text = std::fs::read_to_string(fixture("amazon-orders.csv")).unwrap();
    let header = text.lines().next().unwrap();
    let new_row = "amazon:111-0000099-0000099:B0TOWEL001,Lake House / Pantry,\"household,consumable,amazon\",3,Paper Towels 6-pack,Bounty,Amazon,22.99,2025-12-15,,amazon-orders:111-0000099-0000099,consumable,B0TOWEL001,true,5,31";
    let coffee_row = text.lines().find(|l| l.contains("111-0000006")).unwrap();
    std::fs::write(&later, format!("{header}\n{new_row}\n{coffee_row}\n")).unwrap();
    let v = e.json(&["import", later.to_str().unwrap()]);
    let s = &v["summary"];
    assert_eq!(
        (s["created"].as_u64(), s["updated"].as_u64(), s["unchanged"].as_u64()),
        (Some(0), Some(1), Some(1)),
        "{s}"
    );
    let all = items(&e);
    let towels = named(&all, TOWELS, "Lake House");
    assert_eq!(lot_refs(towels).len(), 5);
    assert_eq!(towels["quantity"], 11);
    let coffee = named(&all, "Coffee Pods 72ct", "Lake House");
    assert_eq!(coffee["lots"][0]["status"], "consumed", "a re-imported purchase keeps its use");
}

#[test]
fn homebox_export_has_a_row_per_active_lot_and_round_trips() {
    let a = Env::pinned();
    import(&a);
    let out = a.run(&["export"]);
    let towel_rows: Vec<&str> =
        out.lines().filter(|l| l.starts_with("amazon:111-0000001-0000001:B0TOWEL001#lot:")).collect();
    assert_eq!(towel_rows.len(), 4);
    assert!(out.lines().next().unwrap().contains("HB.field.Lot Price,HB.field.Lot Ref"), "{out}");
    let file = a.path("export.csv");
    std::fs::write(&file, &out).unwrap();
    let b = Env::pinned();
    b.run(&["import", file.to_str().unwrap()]);
    assert_eq!(b.run(&["export"]), out);
}

#[test]
fn shopping_learns_reorders_from_lots() {
    let e = Env::pinned(); // today = 2026-01-01
    import(&e);
    let v = e.json(&["shopping"]);
    let got: Vec<(String, String, i64, i64)> = rows(&v)
        .iter()
        .map(|r| {
            (
                format!("{} @ {}", r["item"].as_str().unwrap(), r["location"].as_str().unwrap()),
                r["last_bought"].as_str().unwrap().to_string(),
                r["cadence_days"].as_i64().unwrap(),
                r["overdue_days"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Coffee Pods 72ct @ Lake House / Kitchen".to_string(), "2025-12-20".to_string(), 10, 2),
            ("Olive Oil 1L @ Lake House / Kitchen".into(), "2025-10-15".into(), 60, 18),
            ("Paper Towels 6-pack @ Lake House / Pantry".into(), "2025-09-04".into(), 30, 89),
            ("Paper Towels 6-pack @ Rental apartment / Kitchen".into(), "2025-11-01".into(), 31, 30),
        ]
    );
    let coffee = row(&v, "item", "Coffee Pods 72ct");
    assert!(coffee["detail"].as_str().unwrap().contains("median of purchases"), "{coffee}");
    assert!(row(&v, "item", "Olive Oil 1L")["detail"].as_str().unwrap().contains("Avg Interval Days"));
    // printer ink was last bought in 2023: dropped, unless the cutoff reaches back
    assert_eq!(v["dropped"], 1);
    assert!(!rows(&v).iter().any(|r| r["item"] == "Printer Ink XL"));
    let v = e.json(&["shopping", "--since", "4y"]);
    assert_eq!(v["dropped"], 0);
    assert!(rows(&v).iter().any(|r| r["item"] == "Printer Ink XL"));
    // a fresh purchase inside the cadence stocks the coffee
    e.run(&["lot", "add", "Coffee Pods 72ct", "1"]);
    assert!(!rows(&e.json(&["shopping"])).iter().any(|r| r["item"] == "Coffee Pods 72ct"));
}

#[test]
fn insurance_leaves_out_consumables_and_values_lines() {
    let e = Env::pinned();
    import(&e);
    let v = e.json(&["report", "insurance"]);
    let d = &v["data"];
    let lines: Vec<(String, f64, f64)> = d["properties"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["rooms"].as_array().unwrap().iter())
        .flat_map(|r| r["items"].as_array().unwrap().iter())
        .map(|i| {
            (
                i["name"].as_str().unwrap().to_string(),
                i["unit_value"].as_f64().unwrap(),
                i["line_value"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            ("Dining Chair".to_string(), 120.0, 480.0),
            ("Cordless Vacuum".into(), 299.99, 299.99),
            ("Kitchen Scale".into(), 24.99, 24.99),
            ("Kitchen Scale".into(), 24.99, 24.99),
            ("Stand Mixer".into(), 249.0, 249.0),
        ]
    );
    assert_eq!(d["grand_total"], 1078.97);
    assert_eq!(d["gap_counts"]["verify still owned"], 1);
    let mixer = d["gaps"].as_array().unwrap().iter().find(|g| g["name"] == "Stand Mixer").unwrap();
    assert!(mixer["gaps"].as_array().unwrap().iter().any(|g| g == "verify still owned"), "{mixer}");
    assert!(d["header"].to_string().contains("Verify still owned: 1 item(s) bought more than 10 years ago"));

    let total = |args: &[&str]| {
        let mut a = vec!["report", "insurance"];
        a.extend_from_slice(args);
        e.json(&a)["data"]["grand_total"].as_f64().unwrap()
    };
    assert_eq!(total(&["--min-value", "250"]), 779.99, "chairs (line 480) and the vacuum");
    assert_eq!(total(&["--bought-after", "2024-01-01"]), 349.97, "the vacuum and both scales");
    // the mixer, once sold, is no longer a gap
    e.run(&["edit", "Stand Mixer", "--status", "sold"]);
    assert!(e.json(&["report", "insurance"])["data"]["gap_counts"]["verify still owned"].is_null());
    let table = e.run(&["report", "insurance"]);
    assert!(table.contains("UNIT_VALUE") && table.contains("LINE_VALUE"), "{table}");
}

#[test]
fn regroup_migrates_one_item_per_purchase() {
    let e = Env::pinned();
    // As imported before: one item per order line.
    for (r, day, price) in [("A", "2025-10-01", "9.99"), ("B", "2025-11-01", "10.49"), ("C", "2025-12-01", "10.49")] {
        e.run(&[
            "add",
            "Dish Soap",
            "--at",
            "Lake House / Kitchen",
            "--qty",
            "2",
            "--bought",
            day,
            "--price",
            price,
            "--import-ref",
            &format!("amazon:{r}:B0SOAP0001"),
            "--field",
            "ASIN=B0SOAP0001",
            "--field",
            "Kind=consumable",
            "--source",
            &format!("amazon-orders:{r}"),
        ]);
    }
    e.run(&["add", "Toaster", "--at", "Lake House / Kitchen", "--field", "ASIN=B0TOAST001", "--price", "40"]);
    let count = |e: &Env| items(e).len();
    let v = e.json(&["db", "regroup-consumables", "--dry-run"]);
    assert_eq!(rows(&v).len(), 1);
    assert_eq!((rows(&v)[0]["merged"].as_u64(), rows(&v)[0]["lots_created"].as_u64()), (Some(2), Some(3)));
    assert_eq!(count(&e), 4, "dry run writes nothing");

    let v = e.json(&["db", "regroup-consumables"]);
    assert_eq!(rows(&v).len(), 1);
    let all = items(&e);
    assert_eq!(all.len(), 2);
    let soap = named(&all, "Dish Soap", "Lake House");
    assert_eq!(soap["import_ref"], "amazon:A:B0SOAP0001");
    assert_eq!(soap["quantity"], 6);
    assert_eq!(soap["purchase_time"], "2025-12-01");
    assert_eq!(soap["sources"].as_array().unwrap().len(), 3);
    assert_eq!(
        lot_refs(soap),
        [
            ("amazon:A:B0SOAP0001".to_string(), "2025-10-01".to_string(), 2.0, 9.99),
            ("amazon:B:B0SOAP0001".into(), "2025-11-01".into(), 2.0, 10.49),
            ("amazon:C:B0SOAP0001".into(), "2025-12-01".into(), 2.0, 10.49),
        ]
    );
    let kinds: Vec<String> =
        rows(&e.json(&["history", "Dish Soap"])).iter().map(|r| r["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds.iter().filter(|k| *k == "created").count(), 3, "the merged items' history moves over");
    assert!(kinds.iter().any(|k| k == "merged") && kinds.iter().any(|k| k == "regrouped"), "{kinds:?}");

    // Idempotent: nothing left to do.
    assert!(rows(&e.json(&["db", "regroup-consumables"])).is_empty());
    // And a re-import of the same purchases matches the lots.
    let csv = e.path("orders.csv");
    let mut text = String::from("HB.import_ref,HB.location,HB.quantity,HB.name,HB.purchase_price,HB.purchase_time,HB.field.Kind,HB.field.ASIN\n");
    for (r, day, price) in [("A", "2025-10-01", "9.99"), ("B", "2025-11-01", "10.49"), ("C", "2025-12-01", "10.49")] {
        text.push_str(&format!(
            "amazon:{r}:B0SOAP0001,Lake House / Kitchen,2,Dish Soap,{price},{day},consumable,B0SOAP0001\n"
        ));
    }
    std::fs::write(&csv, text).unwrap();
    let v = e.json(&["import", csv.to_str().unwrap()]);
    assert_eq!(v["summary"]["created"], 0);
    assert_eq!(lot_refs(named(&items(&e), "Dish Soap", "Lake House")).len(), 3);
}
