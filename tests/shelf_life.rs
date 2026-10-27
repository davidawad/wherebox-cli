//! Computed shelf life and estimated on hand (docs/SPEC.md, "Shelf life" and
//! "Estimated on hand"): shelf-life types and their config, expiry computed
//! by storage, non-perishables without expiry, lots presumed used up, and
//! explicit logs and `lot confirm` winning over the presumption.

mod support;

use serde_json::Value;
use support::{row, rows, Env};

fn config(e: &Env) -> std::path::PathBuf {
    e.path("config").join("wherebox-cli").join("config.toml")
}

fn lots(e: &Env, item: &str) -> Vec<Value> {
    rows(&e.json(&["lot", "ls", item])).to_vec()
}

#[test]
fn types_list_show_set_and_reset() {
    let e = Env::pinned();
    let v = e.json(&["shelf-life", "ls"]);
    assert!(rows(&v).len() >= 60, "{} types", rows(&v).len());
    assert_eq!(row(&v, "name", "milk")["fridge"], "1 week");
    assert_eq!(row(&v, "name", "milk")["origin"], "built-in");
    assert_eq!(row(&v, "name", "opened-condiment")["pantry"], Value::Null);
    let show = e.json(&["shelf-life", "show", "raw-chicken"]);
    assert!(show["data"]["source"].as_str().unwrap().contains("FoodSafety.gov"), "{show}");
    assert!(show["data"]["synonyms"].as_array().unwrap().iter().any(|s| s == "chicken breast"));
    e.fail(&["shelf-life", "show", "nope"], 3);

    // override a built-in figure; `none` removes one
    e.run(&["shelf-life", "set", "milk", "--fridge", "5d", "--freezer", "none"]);
    let text = std::fs::read_to_string(config(&e)).unwrap();
    assert!(text.contains("[shelf_life.milk]") && text.contains("fridge = \"5d\""), "{text}");
    let milk = e.json(&["shelf-life", "show", "milk"]);
    assert_eq!((&milk["data"]["fridge"], &milk["data"]["freezer"]), (&"5d".into(), &Value::Null));
    assert_eq!(milk["data"]["origin"], "built-in+config");
    e.run(&["add", "Whole Milk", "--at", "Kitchen / Fridge", "--lot"]);
    assert_eq!(lots(&e, "Whole Milk")[0]["expires_at"], "2026-01-06");

    // a new type, classified by its synonym
    e.run(&["shelf-life", "set", "kombucha", "--fridge", "1m", "--synonym", "booch", "--group", "prepared"]);
    e.run(&["add", "Ginger Booch", "--at", "Kitchen / Fridge", "--lot"]);
    let v = e.json(&["show", "Ginger Booch"]);
    assert_eq!((&v["data"]["shelf_life"], &v["data"]["shelf_life_basis"]), (&"kombucha".into(), &"name".into()));
    assert_eq!(v["data"]["expires_at"], "2026-02-01");
    // bad values are refused and leave the file alone
    let before = std::fs::read_to_string(config(&e)).unwrap();
    e.fail(&["shelf-life", "set", "kombucha", "--fridge", "soon"], 7);
    e.fail(&["shelf-life", "set", "Bad Name", "--fridge", "1d"], 4);
    e.fail(&["shelf-life", "set", "empty", "--description", "no figures"], 7);
    assert_eq!(std::fs::read_to_string(config(&e)).unwrap(), before);

    // settings in the same file still work
    e.run(&["config", "set", "precision", "1"]);
    e.run(&["shelf-life", "reset", "milk"]);
    assert_eq!(e.json(&["shelf-life", "show", "milk"])["data"]["origin"], "built-in");
    e.run(&["shelf-life", "reset", "kombucha"]);
    e.fail(&["shelf-life", "show", "kombucha"], 3);
    e.fail(&["shelf-life", "reset", "kombucha"], 3);
    assert!(std::fs::read_to_string(config(&e)).unwrap().contains("precision"));
}

#[test]
fn expiry_follows_storage_opening_freezing_and_thawing() {
    let e = Env::pinned(); // today = 2026-01-01
    e.run(&["loc", "add", "Home / Garage / Big box", "--kind", "freezer"]);
    // pantry: bananas keep 2 days
    e.run(&["add", "Bananas", "--at", "Home / Kitchen / Counter", "--lot", "--acquired", "2025-12-31"]);
    let l = &lots(&e, "Bananas")[0];
    assert_eq!(
        (&l["storage"], &l["expires_at"], &l["expiry_basis"]),
        (&"pantry".into(), &"2026-01-02".into(), &"computed".into())
    );
    // fridge: deli turkey keeps 2 weeks sealed, 3 days once opened
    e.run(&["add", "Deli Turkey", "--at", "Home / Kitchen / Fridge", "--lot", "--acquired", "2025-12-30"]);
    let id = lots(&e, "Deli Turkey")[0]["id"].as_str().unwrap().to_string();
    assert_eq!(lots(&e, "Deli Turkey")[0]["expires_at"], "2026-01-13");
    e.run(&["lot", "edit", &id, "--opened", "2026-01-01"]);
    assert_eq!(lots(&e, "Deli Turkey")[0]["expires_at"], "2026-01-04");
    // ground beef: a day in the fridge; into the freezer (a location kind) re-bases on today
    e.run(&["add", "Ground Beef", "--at", "Home / Kitchen / Fridge", "--lot", "--acquired", "2025-12-20"]);
    let gb = lots(&e, "Ground Beef")[0]["id"].as_str().unwrap().to_string();
    assert_eq!(lots(&e, "Ground Beef")[0]["expires_at"], "2025-12-21");
    e.run(&["lot", "edit", &gb, "--at", "Home / Garage / Big box"]);
    let l = &lots(&e, "Ground Beef")[0];
    assert_eq!(
        (&l["storage"], &l["frozen_at"], &l["expires_at"]),
        (&"freezer".into(), &"2026-01-01".into(), &"2026-04-01".into())
    );
    // out again: thawed today, a day in the fridge
    e.run(&["lot", "edit", &gb, "--at", "Home / Kitchen / Fridge"]);
    let l = &lots(&e, "Ground Beef")[0];
    assert_eq!((&l["thawed_at"], &l["expires_at"]), (&"2026-01-01".into(), &"2026-01-02".into()));
    // re-frozen by moving the item (the lot is where the item is, so it follows)
    e.run(&["edit", "Ground Beef", "--at", "Home / Garage / Big box"]);
    let l = &lots(&e, "Ground Beef")[0];
    assert_eq!(
        (&l["storage"], &l["thawed_at"], &l["frozen_at"]),
        (&"freezer".into(), &Value::Null, &"2026-01-01".into())
    );
    // a lot in a place of its own does not follow the item
    e.run(&["lot", "edit", &gb, "--at", "Home / Garage / Chest Freezer"]);
    e.run(&["mv", "Ground Beef", "--to", "Home / Kitchen / Fridge"]);
    assert_eq!(lots(&e, "Ground Beef")[0]["storage"], "freezer");
    // a printed best-by wins over the computation
    e.run(&["lot", "add", "Bananas", "3", "--best-by", "2026-01-20"]);
    assert!(lots(&e, "Bananas").iter().any(|l| l["expires_at"] == "2026-01-20" && l["expiry_basis"] == "printed"));
    // expiring lists perishables only
    let v = e.json(&["expiring", "--within", "3d"]);
    let names: Vec<&str> = rows(&v).iter().map(|r| r["item"].as_str().unwrap()).collect();
    assert_eq!(names, ["Bananas", "Deli Turkey"], "{v}");
}

#[test]
fn non_perishables_never_expire() {
    let e = Env::pinned();
    e.run(&["add", "Dish Soap", "--preset", "household", "--lot", "--qty", "2"]);
    e.run(&["add", "Canned Tuna", "--lot", "--qty", "12", "--label", "food"]);
    for (args, what) in [
        (vec!["add", "Paper Towels", "--best-by", "2027-01-01"], "add"),
        (vec!["lot", "add", "Canned Tuna", "1", "--best-by", "2027-01-01"], "lot add"),
    ] {
        let err = e.fail(&args, 4);
        assert!(err.contains("not perishable"), "{what}: {err}");
    }
    let id = lots(&e, "Dish Soap")[0]["id"].as_str().unwrap().to_string();
    e.fail(&["lot", "edit", &id, "--best-by", "2027-01-01"], 4);
    e.fail(&["lot", "add", "Canned Tuna", "1", "--expires", "2027-01-01"], 2);
    for item in ["Dish Soap", "Canned Tuna"] {
        let v = e.json(&["show", item]);
        assert_eq!((&v["data"]["shelf_life"], &v["data"]["expires_at"]), (&Value::Null, &Value::Null), "{item}");
    }
    assert_eq!(e.json(&["expiring", "--within", "10y"])["count"], 0);
    // `--shelf-life` makes anything perishable; `none` makes anything not
    e.run(&["edit", "Canned Tuna", "--shelf-life", "opened-canned-food"]);
    e.run(&["lot", "edit", lots(&e, "Canned Tuna")[0]["id"].as_str().unwrap(), "--opened", "2026-01-01"]);
    assert_eq!(e.json(&["show", "Canned Tuna"])["data"]["expires_at"], "2026-01-04");
    e.fail(&["edit", "Canned Tuna", "--shelf-life", "no-such-type"], 3);
    e.run(&["add", "Ripe Bananas", "--lot", "--shelf-life", "none"]);
    assert_eq!(e.json(&["show", "Ripe Bananas"])["data"]["expires_at"], Value::Null);
    e.run(&["edit", "Ripe Bananas", "--clear", "shelf_life"]);
    assert_eq!(e.json(&["show", "Ripe Bananas"])["data"]["shelf_life"], "banana");
}

#[test]
fn explicit_logs_and_confirmations_win() {
    let e = Env::pinned(); // today = 2026-01-01; default cadence 90 days
    e.run(&["add", "Dish Soap", "--preset", "household", "--lot", "--qty", "2", "--acquired", "2025-09-01"]);
    let v = e.json(&["show", "Dish Soap"]);
    assert_eq!((&v["data"]["on_hand"], &v["data"]["on_hand_basis"]), (&0.into(), &"estimated".into()));
    assert_eq!(v["data"]["quantity"], 2, "the logged quantity is kept");
    assert!(e.run(&["ls"]).contains("~0"));
    // `lot confirm` says it is still there
    e.run(&["lot", "confirm", "Dish Soap"]);
    let v = e.json(&["show", "Dish Soap"]);
    assert_eq!((&v["data"]["on_hand"], &v["data"]["on_hand_basis"]), (&2.into(), &"logged".into()));
    // with --qty the oldest lots are drawn down so N remains
    e.run(&["lot", "add", "Dish Soap", "3", "--acquired", "2025-12-15"]);
    let v = e.json(&["lot", "confirm", "Dish Soap", "--qty", "4"]);
    assert_eq!(v["on_hand"], 4.0, "{v}");
    let log = e.json(&["show", "Dish Soap"])["data"]["consumption"].clone();
    assert_eq!((&log[0]["qty"], &log[0]["note"]), (&1.0.into(), &"lot confirm".into()));
    e.fail(&["lot", "confirm", "Dish Soap", "--qty", "9"], 4);
    // a logged use restarts a lot's clock too
    e.run(&["add", "Sponges", "--preset", "household", "--lot", "--qty", "6", "--acquired", "2025-08-01"]);
    assert_eq!(e.json(&["show", "Sponges"])["data"]["on_hand"], 0);
    e.run(&["use", "Sponges", "1", "--on", "2025-12-30"]);
    assert_eq!(e.json(&["show", "Sponges"])["data"]["on_hand"], 5);
    // a perishable past its expiry stays expired, confirmed or not
    e.run(&["add", "Strawberries", "--at", "Fridge", "--lot", "--acquired", "2025-12-25"]);
    e.run(&["lot", "confirm", "Strawberries"]);
    let v = e.json(&["show", "Strawberries"]);
    assert_eq!((&v["data"]["on_hand"], &v["data"]["on_hand_basis"]), (&0.into(), &"estimated".into()));
    // and toss logs it gone
    e.run(&["toss", "Strawberries"]);
    assert_eq!(e.json(&["show", "Strawberries"])["data"]["on_hand_basis"], "logged");
    // the default cadence is a setting; [cadence] sets it per preset or category
    std::fs::write(config(&e), "default_cadence = \"30d\"\n[cadence]\nhousehold = \"1y\"\n").unwrap();
    e.run(&["add", "Hand Soap", "--preset", "toiletry", "--lot", "--acquired", "2025-11-15"]);
    e.run(&["add", "Trash Bags", "--preset", "household", "--lot", "--acquired", "2025-06-01"]);
    assert_eq!(e.json(&["show", "Hand Soap"])["data"]["on_hand_basis"], "estimated");
    assert_eq!(e.json(&["show", "Trash Bags"])["data"]["on_hand_basis"], "logged");
}

#[test]
fn twelve_tuna_purchases_leave_only_the_latest_on_hand() {
    let e = Env::pinned(); // today = 2026-01-01
    e.run(&["add", "Canned Tuna", "--at", "Home / Kitchen / Pantry", "--unit", "can", "--label", "food", "--par", "6"]);
    // 12 purchases of 12 cans over two years, about every 65 days, never logged as eaten
    let mut d = chrono::NaiveDate::from_ymd_opt(2024, 1, 10).unwrap();
    for _ in 0..12 {
        e.run(&["lot", "add", "Canned Tuna", "12", "--acquired", &d.to_string()]);
        d += chrono::Duration::days(65);
    }
    let v = e.json(&["show", "Canned Tuna"]);
    assert_eq!(v["data"]["quantity"], 144, "logged: every can ever bought");
    assert_eq!((&v["data"]["on_hand"], &v["data"]["on_hand_basis"]), (&12.into(), &"estimated".into()));
    let presumed = v["data"]["lots"].as_array().unwrap().iter().filter(|l| l["presumed_used_up"] == true).count();
    assert_eq!(presumed, 11);
    // list check counts only the recent lot
    e.run(&["list", "new", "tuna-salad"]);
    e.run(&["list", "add", "tuna-salad", "canned tuna", "--qty", "24", "--unit", "can"]);
    // (exit 3: the list is not ready)
    let out = e.out(&["list", "check", "tuna-salad", "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = &rows(&v)[0];
    assert_eq!((&r["have"], &r["state"], &r["on_hand_basis"]), (&12.0.into(), &"short".into(), &"estimated".into()));
    let text = String::from_utf8(e.out(&["list", "check", "tuna-salad"]).stdout).unwrap();
    assert!(text.contains("~12"), "{text}");
    // shopping: 12 on hand (estimated) is above par
    assert!(rows(&e.json(&["shopping"])).iter().all(|r| r["reason"] != "below par"));
    e.run(&["eat", "Canned Tuna", "8"]);
    let v = e.json(&["shopping"]);
    let line = rows(&v).iter().find(|r| r["item"] == "Canned Tuna").expect("below par").clone();
    assert_eq!(line["reason"], "below par");
    assert!(line["detail"].as_str().unwrap().starts_with("~4 of 6"), "{line}");
    assert!(e.run(&["ls"]).contains("~4"));
}

#[test]
fn import_drops_printed_dates_of_non_perishables() {
    let e = Env::pinned();
    let csv = e.path("in.csv");
    std::fs::write(
        &csv,
        "HB.name,HB.location,HB.field.Kind,HB.field.Lot Qty,HB.field.Expires,HB.field.Best By\n\
         Canned Beans,Pantry,consumable,4,2028-01-01,\n\
         Whole Milk,Fridge,food,1,,2026-01-09\n",
    )
    .unwrap();
    let v = e.json(&["import", csv.to_str().unwrap()]);
    let warnings = v["warnings"].to_string();
    assert!(warnings.contains("Canned Beans") && warnings.contains("not perishable"), "{v}");
    assert_eq!(lots(&e, "Canned Beans")[0]["best_by"], Value::Null);
    assert_eq!(lots(&e, "Whole Milk")[0]["expires_at"], "2026-01-09");
}
