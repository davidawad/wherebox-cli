//! `par` and `shopping`: below par, expiring par items, learned reorders.

mod support;

use support::{row, rows, Env};

fn seed(e: &Env) {
    // today = 2026-01-01
    e.run(&[
        "add",
        "Milk",
        "--unit",
        "l",
        "--lot",
        "--qty",
        "1",
        "--at",
        "Kitchen / Fridge",
        "--vendor",
        "Whole Foods",
    ]);
    e.run(&["par", "Milk", "2"]);
    e.run(&["add", "Eggs", "--lot", "--qty", "6", "--best-by", "2026-01-03", "--at", "Kitchen / Fridge"]);
    e.run(&["par", "Eggs", "1", "--reorder", "12"]);
    // coffee bought every ~9 days, last on 2025-12-19
    e.run(&["add", "Coffee beans", "--lot", "--acquired", "2025-12-01", "--at", "Kitchen / Pantry"]);
    for d in ["2025-12-10", "2025-12-19"] {
        e.run(&["lot", "add", "Coffee beans", "1", "--acquired", d]);
    }
    e.run(&["use", "Coffee beans", "2"]);
    // a recurring field (as HB.field.Recurring imports it)
    e.run(&["add", "Furnace filter", "--bought", "2025-09-15", "--field", "Recurring=3m"]);
    // plenty on hand: not listed
    e.run(&["add", "Rice", "--unit", "kg", "--qty", "5", "--par", "1"]);
}

#[test]
fn lists_below_par_expiring_and_reorders() {
    let e = Env::pinned();
    seed(&e);
    let v = e.json(&["shopping"]);
    let reasons: Vec<(&str, &str)> =
        rows(&v).iter().map(|r| (r["item"].as_str().unwrap(), r["reason"].as_str().unwrap())).collect();
    assert_eq!(
        reasons,
        [("Milk", "below par"), ("Eggs", "expiring"), ("Coffee beans", "reorder"), ("Furnace filter", "reorder")]
    );
    assert_eq!(row(&v, "item", "Milk")["qty"], 1.0);
    assert_eq!(row(&v, "item", "Milk")["vendor"], "Whole Foods");
    assert_eq!(row(&v, "item", "Eggs")["qty"], 12.0);
    assert!(row(&v, "item", "Coffee beans")["detail"].as_str().unwrap().contains("every ~9 days"));
    // a shorter window drops the eggs
    let v = e.json(&["shopping", "--within", "1d"]);
    assert!(!rows(&v).iter().any(|r| r["item"] == "Eggs"));
    // clearing the par level
    e.run(&["par", "Milk", "none"]);
    assert_eq!(e.json(&["show", "Milk"])["data"]["par_level"], serde_json::Value::Null);
}

#[test]
fn checklist_and_csv() {
    let e = Env::pinned();
    seed(&e);
    let md = e.run(&["shopping", "--checklist"]);
    assert!(
        md.lines().next().unwrap().starts_with("- [ ] Milk: 1 l @ Whole Foods (below par: 1 of 2 l on hand)"),
        "{md}"
    );
    assert_eq!(md.lines().filter(|l| l.starts_with("- [ ] ")).count(), 4);
    let csv = e.run(&["shopping", "--format", "csv"]);
    assert!(csv.starts_with("item,qty,unit,vendor,product_ref,reason,detail,last_bought,cadence_days,overdue_days,location,profile,asset_id\n"), "{csv}");
    assert!(csv.contains("Eggs,12.00,ea,"), "{csv}");
}

#[test]
fn all_profiles() {
    let e = Env::pinned();
    let p = |args: &[&str]| {
        let out = e.cmd().env_remove("WHEREBOX_DB").args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    p(&["profile", "init", "rental-apt", "--title", "Rental apartment"]);
    p(&["profile", "init", "lake-house", "--title", "Lake House"]);
    p(&["-P", "rental-apt", "add", "Paper towels", "--qty", "0", "--par", "2"]);
    p(&["-P", "lake-house", "add", "Dish soap", "--qty", "1", "--par", "2"]);
    let v: serde_json::Value = serde_json::from_str(&p(&["shopping", "--all-profiles", "--json"])).unwrap();
    let got: Vec<(&str, &str)> =
        rows(&v).iter().map(|r| (r["profile"].as_str().unwrap(), r["item"].as_str().unwrap())).collect();
    assert_eq!(got, [("lake-house", "Dish soap"), ("rental-apt", "Paper towels")]);
    let md = p(&["shopping", "--all-profiles", "--checklist"]);
    assert!(md.contains("## lake-house\n\n- [ ] Dish soap") && md.contains("## rental-apt"), "{md}");
    let out =
        e.cmd().env_remove("WHEREBOX_DB").args(["-P", "rental-apt", "shopping", "--all-profiles"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "--profile and --all-profiles conflict");
}
