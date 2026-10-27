//! Consumables: lots, FIFO consumption (`use`/`eat`/`toss`) and `expiring`.

mod support;

use serde_json::Value;
use support::{rows, Env};

fn lots(e: &Env, item: &str) -> Vec<Value> {
    e.json(&["show", item])["data"]["lots"].as_array().unwrap().clone()
}

fn qty_status(l: &[Value]) -> Vec<(f64, String)> {
    l.iter().map(|l| (l["qty"].as_f64().unwrap(), l["status"].as_str().unwrap().to_string())).collect()
}

/// Eggs with three lots bought on different days (added out of order).
fn eggs() -> Env {
    let e = Env::pinned();
    e.run(&[
        "add",
        "Eggs",
        "--at",
        "Home / Kitchen / Fridge",
        "--unit",
        "ea",
        "--lot-qty",
        "6",
        "--acquired",
        "2025-12-20",
        "--best-by",
        "2026-01-20",
    ]);
    e.run(&["lot", "add", "Eggs", "12", "--acquired", "2025-12-30", "--best-by", "2026-02-01"]);
    e.run(&["lot", "add", "Eggs", "12", "--acquired", "2025-12-25", "--best-by", "2026-01-25"]);
    e
}

#[test]
fn lot_fifo_consumption() {
    let e = eggs();
    assert_eq!(e.json(&["show", "Eggs"])["data"]["quantity"], 30);
    // FIFO = oldest acquired first: 12-20 (6), 12-25 (12), 12-30 (12).
    let v = e.json(&["eat", "Eggs", "8"]);
    let taken: Vec<(f64, f64, String)> = v["data"]["lots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["taken"].as_f64().unwrap(), t["left"].as_f64().unwrap(), t["status"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(taken, [(6.0, 0.0, "consumed".to_string()), (2.0, 10.0, "open".to_string())]);
    assert_eq!(v["data"]["on_hand"], 22.0);
    assert_eq!(
        qty_status(&lots(&e, "Eggs")),
        [(0.0, "consumed".into()), (10.0, "open".into()), (12.0, "sealed".into())]
    );
    assert_eq!(e.json(&["show", "Eggs"])["data"]["quantity"], 22);
    // The consumption log keeps every draw with its reason.
    let log = e.json(&["show", "Eggs"])["data"]["consumption"].clone();
    let reasons: Vec<(&str, f64)> =
        log.as_array().unwrap().iter().map(|c| (c["reason"].as_str().unwrap(), c["qty"].as_f64().unwrap())).collect();
    assert_eq!(reasons, [("eaten", 6.0), ("eaten", 2.0)]);

    // use spans lots too; fractions are fine.
    e.run(&["use", "Eggs", "10.5"]);
    assert_eq!(
        qty_status(&lots(&e, "Eggs")),
        [(0.0, "consumed".into()), (0.0, "consumed".into()), (11.5, "open".into())]
    );
    // toss without a quantity throws out the whole oldest active lot.
    let v = e.json(&["toss", "Eggs", "--spoiled"]);
    assert_eq!(v["data"]["reason"], "spoiled");
    assert_eq!(qty_status(&lots(&e, "Eggs"))[2], (0.0, "discarded".into()));
    assert_eq!(e.json(&["show", "Eggs"])["data"]["quantity"], 0);
    // Nothing left.
    let err = e.fail(&["eat", "Eggs"], 4);
    assert!(err.contains("no Eggs left"), "{err}");
    let kinds: Vec<String> =
        rows(&e.json(&["history", "Eggs"])).iter().map(|r| r["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["created", "lot_added", "lot_added", "eaten", "used", "spoiled"]);
}

#[test]
fn more_than_on_hand_fails_and_writes_nothing() {
    let e = eggs();
    let err = e.fail(&["eat", "Eggs", "31"], 4);
    assert!(err.contains("only 30 on hand"), "{err}");
    assert_eq!(e.json(&["show", "Eggs"])["data"]["quantity"], 30);
    assert!(e.json(&["show", "Eggs"])["data"]["consumption"].as_array().unwrap().is_empty());
}

#[test]
fn a_specific_lot_can_be_targeted() {
    let e = eggs();
    let newest = lots(&e, "Eggs")[2]["id"].as_str().unwrap().to_string();
    e.run(&["eat", "Eggs", "3", "--lot", &newest]);
    assert_eq!(qty_status(&lots(&e, "Eggs"))[2], (9.0, "open".into()));
    assert_eq!(qty_status(&lots(&e, "Eggs"))[0], (6.0, "sealed".into()), "FIFO lot untouched");
    let err = e.fail(&["eat", "Eggs", "10", "--lot", &newest], 4);
    assert!(err.contains("only 9 on hand"), "{err}");
}

#[test]
fn items_without_lots_count_down() {
    let e = Env::pinned();
    e.run(&["add", "Paper Towels", "--qty", "6", "--par", "4"]);
    e.run(&["use", "Paper Towels", "2"]);
    assert_eq!(e.json(&["show", "Paper Towels"])["data"]["quantity"], 4);
    e.fail(&["use", "Paper Towels", "5"], 4);
    e.fail(&["use", "Paper Towels", "0.5"], 4);
}

#[test]
fn expiring_lists_perishables_by_computed_expiry_freezer_included() {
    // "today" is 2026-01-01 (pinned).
    let e = Env::pinned();
    // yogurt: 1 week in the fridge from today
    e.run(&["add", "Yogurt", "--at", "Home / Kitchen / Fridge", "--lot-qty", "4"]);
    // printed best-by dates win
    e.run(&["add", "Bread", "--at", "Home / Kitchen / Pantry", "--best-by", "2025-12-30"]);
    e.run(&["add", "Salmon", "--at", "Home / Garage / Freezer", "--frozen", "2025-12-01", "--best-by", "2026-01-06"]);
    // long expired: presumed used up, off the list
    e.run(&["add", "Ribeye", "--at", "Home / Kitchen / Fridge", "--acquired", "2025-12-01"]);
    // non-perishables never expire, and cannot be given a date
    let err = e.fail(&["add", "Rice", "--at", "Pantry", "--best-by", "2027-01-01"], 4);
    assert!(err.contains("not perishable"), "{err}");
    e.run(&["add", "Rice", "--at", "Pantry", "--lot"]);
    e.run(&["add", "Hammer", "--at", "Garage"]);
    let v = e.json(&["expiring"]);
    let got: Vec<(String, i64, String)> = rows(&v)
        .iter()
        .map(|r| {
            (r["item"].as_str().unwrap().into(), r["days_left"].as_i64().unwrap(), r["state"].as_str().unwrap().into())
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Bread".into(), -2, "expired".into()),
            ("Salmon".into(), 5, "soon".into()),
            ("Yogurt".into(), 7, "soon".into())
        ]
    );
    assert_eq!(rows(&v)[0]["expiry_basis"], "best_by");
    assert_eq!(rows(&v)[2]["expiry_basis"], "shelf_life");
    assert_eq!(rows(&v)[2]["shelf_life"], "yogurt");
    assert_eq!(rows(&v)[1]["frozen_at"], "2025-12-01");
    assert_eq!(rows(&v)[1]["location"], "Home / Garage / Freezer");
    assert_eq!(e.json(&["expiring", "--within", "3d"])["count"], 1);
    assert_eq!(e.json(&["expiring", "--at", "Fridge"])["count"], 1);
    // `--at Pantry` found the existing Home / Kitchen / Pantry.
    assert_eq!(e.json(&["show", "Rice"])["data"]["location"], "Home / Kitchen / Pantry");
    assert_eq!(e.json(&["show", "Rice"])["data"]["expires_at"], serde_json::Value::Null);
    // Eaten lots drop off the list.
    e.run(&["eat", "Yogurt", "4"]);
    assert_eq!(e.json(&["expiring"])["count"], 2);
    // there is no --expires any more
    e.fail(&["lot", "add", "Rice", "1", "--expires", "2026-02-01"], 2);
}

#[test]
fn moving_into_the_freezer_rebases_the_expiry() {
    let e = Env::pinned(); // 2026-01-01
    e.run(&["add", "Chicken Thighs", "--at", "Home / Kitchen / Fridge", "--acquired", "2025-12-31"]);
    let l = &lots(&e, "Chicken Thighs")[0];
    assert_eq!(l["expires_at"], "2026-01-01", "raw poultry: 1 day in the fridge");
    assert_eq!(l["storage"], "fridge");
    let id = l["id"].as_str().unwrap().to_string();
    e.run(&["lot", "edit", &id, "--at", "Home / Kitchen / Freezer"]);
    let l = &lots(&e, "Chicken Thighs")[0];
    assert_eq!(l["frozen_at"], "2026-01-01", "frozen today");
    assert_eq!(l["expires_at"], "2026-10-01", "9 months from freezing");
    // out of the freezer: the fridge figure from purchase again
    e.run(&["lot", "edit", &id, "--at", "Home / Kitchen / Fridge"]);
    assert_eq!(lots(&e, "Chicken Thighs")[0]["expires_at"], "2026-01-01");
    // re-freezing on a later day re-bases the clock
    let later = Env { epoch: Some("1769904000".into()), ..e }; // 2026-02-01
    later.run(&["lot", "edit", &id, "--at", "Home / Kitchen / Freezer"]);
    let l = &lots(&later, "Chicken Thighs")[0];
    assert_eq!((l["frozen_at"].as_str(), l["expires_at"].as_str()), (Some("2026-02-01"), Some("2026-11-01")));
    // moving the item moves the lots that live with it
    later.run(&["add", "Ground Beef", "--at", "Home / Kitchen / Fridge"]);
    later.run(&["mv", "Ground Beef", "--to", "Home / Kitchen / Freezer"]);
    let l = &lots(&later, "Ground Beef")[0];
    assert_eq!((l["frozen_at"].as_str(), l["expires_at"].as_str()), (Some("2026-02-01"), Some("2026-05-01")));
}

#[test]
fn lot_edit_and_rm() {
    let e = eggs();
    let id = lots(&e, "Eggs")[0]["id"].as_str().unwrap().to_string();
    e.run(&["lot", "edit", &id[16..], "--opened", "2025-12-31", "--at", "Home / Kitchen / Counter"]);
    let l = &lots(&e, "Eggs")[0];
    assert_eq!(l["status"], "open");
    assert_eq!(l["location"], "Home / Kitchen / Counter");
    e.run(&["lot", "rm", &id]);
    assert_eq!(e.json(&["show", "Eggs"])["data"]["quantity"], 24);
    assert_eq!(e.json(&["lot", "ls"])["count"], 2);
}
