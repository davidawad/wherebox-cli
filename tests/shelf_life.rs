//! Shelf-life types (`shelf-life ls|show|set|unset`, `--shelf-life`),
//! computed expiry, and estimated on hand (`lot confirm`, `ls`, `list
//! check`, `shopping`) with synthetic data.

mod support;

use serde_json::Value;
use support::{row, rows, Env};

const TUNA: &str = "Albacore Tuna in Water, 5 oz Can";

/// `list check --json`, whatever its exit code (3: not ready).
fn check(e: &Env, list: &str) -> Value {
    serde_json::from_slice(&e.out(&["list", "check", list, "--json"]).stdout).unwrap()
}

/// Twelve purchases of six cans of tuna over two years (every ~2 months);
/// "today" is 2026-01-01.
fn tuna() -> Env {
    let e = Env::pinned();
    e.run(&[
        "add",
        TUNA,
        "--at",
        "Home / Kitchen / Pantry",
        "--unit",
        "can",
        "--label",
        "food",
        "--lot-qty",
        "6",
        "--acquired",
        "2024-01-10",
    ]);
    for d in [
        "2024-03-12",
        "2024-05-10",
        "2024-07-11",
        "2024-09-09",
        "2024-11-12",
        "2025-01-10",
        "2025-03-11",
        "2025-05-12",
        "2025-07-10",
        "2025-09-10",
        "2025-11-12",
    ] {
        e.run(&["lot", "add", TUNA, "6", "--acquired", d]);
    }
    e
}

#[test]
fn types_list_show_and_filter() {
    let e = Env::pinned();
    let v = e.json(&["shelf-life", "ls"]);
    assert!(rows(&v).len() >= 60, "{} types", rows(&v).len());
    let milk = row(&v, "name", "milk");
    assert_eq!((milk["fridge"].as_str(), milk["pantry"].as_str()), (Some("7 days"), None));
    assert_eq!(milk["origin"], "built-in");
    let dairy = e.json(&["shelf-life", "ls", "--group", "dairy"]);
    assert!(rows(&dairy).iter().all(|r| r["group"] == "dairy"));
    let s = e.json(&["shelf-life", "show", "raw-poultry"]);
    assert_eq!(s["data"]["days"]["fridge"], 1.0);
    assert!(s["data"]["synonyms"].as_array().unwrap().iter().any(|x| x == "chicken breast"));
    e.fail(&["shelf-life", "show", "nope"], 3);
    e.fail(&["shelf-life", "ls", "--group", "nope"], 2);
}

#[test]
fn config_overrides_and_new_types() {
    let e = Env::pinned();
    // a new type, with a synonym
    e.run(&["shelf-life", "set", "kombucha", "--fridge", "30d", "--group", "prepared", "--synonym", "booch"]);
    let cfg = std::fs::read_to_string(e.path("config").join("wherebox-cli").join("config.toml")).unwrap();
    assert!(cfg.contains("[shelf_life.kombucha]") && cfg.contains("fridge = \"30d\""), "{cfg}");
    let s = e.json(&["shelf-life", "show", "kombucha"]);
    assert_eq!((s["data"]["origin"].as_str(), s["data"]["fridge"].as_str()), (Some("config"), Some("30 days")));
    e.run(&["add", "Ginger Booch", "--at", "Home / Kitchen / Fridge"]);
    let item = e.json(&["show", "Ginger Booch"]);
    assert_eq!(
        (item["data"]["shelf_life"].as_str(), item["data"]["shelf_life_basis"].as_str()),
        (Some("kombucha"), Some("name"))
    );
    assert_eq!(item["data"]["expires_at"], "2026-01-31");
    assert_eq!(e.json(&["shelf-life", "show", "kombucha"])["data"]["items"][0], "Ginger Booch");
    // override a built-in value; the rest stays
    e.run(&["shelf-life", "set", "milk", "--fridge", "5d"]);
    let v = e.json(&["shelf-life", "ls"]);
    let milk = row(&v, "name", "milk");
    assert_eq!((milk["fridge"].as_str(), milk["freezer"].as_str()), (Some("5 days"), Some("3 months")));
    assert!(milk["origin"].as_str().unwrap().contains("overridden"));
    // `none` removes a built-in value
    e.run(&["shelf-life", "set", "milk", "--freezer", "none"]);
    assert_eq!(row(&e.json(&["shelf-life", "ls"]), "name", "milk")["freezer"], Value::Null);
    e.run(&["shelf-life", "unset", "milk"]);
    assert_eq!(row(&e.json(&["shelf-life", "ls"]), "name", "milk")["fridge"], "7 days");
    // bad values never reach the file
    e.fail(&["shelf-life", "set", "milk", "--fridge", "soon"], 7);
    e.fail(&["shelf-life", "set", "empty", "--group", "fruit"], 7);
    // settings and shelf-life tables live together
    e.run(&["config", "set", "precision", "3"]);
    assert_eq!(e.json(&["shelf-life", "show", "kombucha"])["data"]["fridge"], "30 days");
}

#[test]
fn shelf_life_flag_sets_or_clears_the_type() {
    let e = Env::pinned();
    e.run(&["add", "Mystery leftovers", "--at", "Home / Kitchen / Fridge", "--shelf-life", "cooked-leftovers"]);
    let v = e.json(&["show", "Mystery leftovers"]);
    assert_eq!(
        (v["data"]["shelf_life"].as_str(), v["data"]["expires_at"].as_str()),
        (Some("cooked-leftovers"), Some("2026-01-04"))
    );
    assert_eq!(v["data"]["fields"]["Shelf Life"], "cooked-leftovers");
    e.fail(&["add", "Thing", "--shelf-life", "bogus"], 3);
    // `none`: never perishable; printed dates go with it
    e.run(&["add", "Cheddar", "--at", "Home / Kitchen / Fridge", "--best-by", "2026-03-01"]);
    assert_eq!(e.json(&["show", "Cheddar"])["data"]["expiry_basis"], "best_by");
    e.run(&["edit", "Cheddar", "--shelf-life", "none"]);
    let v = e.json(&["show", "Cheddar"]);
    assert_eq!((v["data"]["shelf_life"].clone(), v["data"]["expires_at"].clone()), (Value::Null, Value::Null));
    assert_eq!(v["data"]["lots"][0]["best_by"], Value::Null);
    let lot = v["data"]["lots"][0]["id"].as_str().unwrap().to_string();
    let err = e.fail(&["lot", "edit", &lot, "--best-by", "2026-03-01"], 4);
    assert!(err.contains("not perishable"), "{err}");
    // and back: by name again
    e.run(&["edit", "Cheddar", "--shelf-life", ""]);
    assert_eq!(e.json(&["show", "Cheddar"])["data"]["shelf_life"], "cheese-hard");
}

#[test]
fn only_the_recent_tuna_counts() {
    let e = tuna();
    let v = e.json(&["show", TUNA]);
    assert_eq!(v["data"]["quantity"], 72, "logged");
    assert_eq!((v["data"]["on_hand"].as_f64(), v["data"]["on_hand_basis"].as_str()), (Some(6.0), Some("estimated")));
    assert_eq!(v["data"]["shelf_life"], Value::Null, "canned tuna is not perishable");
    let lots = v["data"]["lots"].as_array().unwrap();
    assert_eq!(lots.iter().filter(|l| l["used_up"] == true).count(), 11);
    assert!(lots[0]["used_up_why"].as_str().unwrap().contains("62 day reorder cadence"), "{}", lots[0]["used_up_why"]);
    // text output marks the estimate
    let table = e.run(&["ls", "--columns", "name,on_hand"]);
    assert!(table.contains("~6"), "{table}");
    // a recipe needing 6 cans is ready; 12 is short of the estimate
    e.run(&["list", "new", "Tuna salad", "--type", "recipe"]);
    e.run(&["list", "add", "Tuna salad", "albacore tuna", "--qty", "6", "--unit", "can"]);
    let v = e.json(&["list", "check", "Tuna salad"]);
    assert_eq!(rows(&v)[0]["state"], "have");
    assert_eq!((rows(&v)[0]["have"].as_f64(), rows(&v)[0]["on_hand_basis"].as_str()), (Some(6.0), Some("estimated")));
    e.run(&["list", "edit", "Tuna salad", "1", "--qty", "12"]);
    let out = e.out(&["list", "check", "Tuna salad"]);
    assert_eq!(out.status.code(), Some(3));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("short (have ~6 of 12)"), "{text}");
    // shopping: below par by the estimate
    e.run(&["par", TUNA, "12"]);
    let v = e.json(&["shopping"]);
    let line = row(&v, "item", TUNA);
    assert_eq!((line["reason"].as_str(), line["qty"].as_f64()), (Some("below par"), Some(6.0)));
    assert!(line["detail"].as_str().unwrap().starts_with("~6 of 12"), "{}", line["detail"]);
}

#[test]
fn explicit_logs_and_confirm_win() {
    let e = tuna();
    // a use comes out of the recent lot, not a presumed-gone one
    e.run(&["use", TUNA, "2"]);
    let v = e.json(&["show", TUNA]);
    assert_eq!((v["data"]["on_hand"].as_f64(), v["data"]["quantity"].as_i64()), (Some(4.0), Some(70)));
    // confirm: 10 cans are really there (the newest lots), the rest logged as used
    let c = e.json(&["lot", "confirm", TUNA, "--qty", "10"]);
    assert_eq!((c["data"]["on_hand"].as_f64(), c["data"]["on_hand_basis"].as_str()), (Some(10.0), Some("logged")));
    let v = e.json(&["show", TUNA]);
    assert_eq!(v["data"]["quantity"], 10);
    let active: Vec<&Value> =
        v["data"]["lots"].as_array().unwrap().iter().filter(|l| l["qty"].as_f64() > Some(0.0)).collect();
    assert_eq!(active.len(), 2);
    assert!(active.iter().all(|l| l["confirmed_at"] == "2026-01-01" && l["used_up"] == false));
    let reasons = v["data"]["consumption"].as_array().unwrap();
    assert!(reasons.iter().any(|r| r["note"] == "not found at lot confirm"));
    // more than is logged: add it instead
    let err = e.fail(&["lot", "confirm", TUNA, "--qty", "11"], 4);
    assert!(err.contains("lot add"), "{err}");
    // confirming without a quantity keeps everything logged
    e.run(&["lot", "confirm", TUNA]);
    assert_eq!(e.json(&["show", TUNA])["data"]["on_hand"], 10);
}

#[test]
fn perishables_past_expiry_are_presumed_used_up() {
    let e = Env::pinned();
    e.run(&[
        "add",
        "Whole Milk",
        "--at",
        "Home / Kitchen / Fridge",
        "--unit",
        "gal",
        "--lot-qty",
        "1",
        "--acquired",
        "2025-12-20",
    ]);
    e.run(&["lot", "add", "Whole Milk", "1", "--acquired", "2025-12-30"]);
    let v = e.json(&["show", "Whole Milk"]);
    assert_eq!((v["data"]["on_hand"].as_f64(), v["data"]["on_hand_basis"].as_str()), (Some(1.0), Some("estimated")));
    assert_eq!(v["data"]["expires_at"], "2026-01-06");
    let lots = v["data"]["lots"].as_array().unwrap();
    assert!(
        lots[0]["used_up"] == true && lots[0]["used_up_why"].as_str().unwrap().contains("past its expiry 2025-12-27")
    );
    // a recipe sees only the fresh gallon
    e.run(&["list", "new", "Pancakes", "--type", "recipe"]);
    e.run(&["list", "add", "Pancakes", "milk", "--qty", "2", "--unit", "gal"]);
    let v = check(&e, "Pancakes");
    assert_eq!((rows(&v)[0]["state"].as_str(), rows(&v)[0]["have"].as_f64()), (Some("short"), Some(1.0)));
    // confirmed after its expiry: on hand, but expired (not for recipes)
    let old = lots[0]["id"].as_str().unwrap().to_string();
    e.run(&["lot", "confirm", "Whole Milk", "--lot", &old]);
    let v = e.json(&["show", "Whole Milk"]);
    assert_eq!((v["data"]["on_hand"].as_f64(), v["data"]["on_hand_basis"].as_str()), (Some(2.0), Some("logged")));
    let c = check(&e, "Pancakes");
    assert_eq!((rows(&c)[0]["have"].as_f64(), rows(&c)[0]["expired"].as_f64()), (Some(1.0), Some(1.0)));
    let exp = e.json(&["expiring", "--within", "1d"]);
    assert_eq!((rows(&exp)[0]["state"].as_str(), rows(&exp)[0]["days_left"].as_i64()), (Some("expired"), Some(-5)));
}
