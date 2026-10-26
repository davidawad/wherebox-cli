//! Kind presets: `preset ls|show`, and dates filled by `add --preset` and
//! `lot add` (fridge vs freezer by location name).

mod support;

use support::{row, rows, Env};

fn lots(e: &Env, item: &str) -> serde_json::Value {
    e.json(&["lot", "ls", item])
}

#[test]
fn ls_and_show() {
    let e = Env::pinned();
    let v = e.json(&["preset", "ls"]);
    assert!(rows(&v).len() >= 39);
    assert_eq!(row(&v, "name", "meat.raw")["category"], "food");
    assert_eq!(row(&v, "name", "meat.raw")["shelf_life"], "fridge 3-5 days, freezer 4-12 months");
    let s = e.json(&["preset", "show", "smoke-detector"]);
    assert_eq!(s["data"]["intervals"]["replace"], "10y");
    assert_eq!(
        s["data"]["sources"][0]["url"],
        "https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms"
    );
    let m = e.json(&["preset", "show", "poultry.raw"]);
    assert!(m["data"]["sources"][0]["cite"].as_str().unwrap().contains("FoodKeeper"));
    let table = e.run(&["preset", "show", "meat.raw"]);
    assert!(table.contains("FoodKeeper") && table.contains("use-by"), "{table}");
    let err = e.fail(&["preset", "show", "nope"], 3);
    assert!(err.contains("preset ls"), "{err}");
}

#[test]
fn add_and_lot_add_fill_dates_by_storage() {
    let e = Env::pinned(); // today = 2026-01-01
    e.run(&["add", "Ribeye", "--preset", "meat.raw", "--at", "Rental apartment / Kitchen / Fridge"]);
    let v = lots(&e, "Ribeye");
    assert_eq!(rows(&v).len(), 1, "a preset with a shelf life starts a lot");
    assert_eq!(rows(&v)[0]["expires_at"], "2026-01-04", "fridge: 3 days");
    assert_eq!(rows(&v)[0]["frozen_at"], serde_json::Value::Null);

    e.run(&[
        "add",
        "Chicken thighs",
        "--preset",
        "poultry.raw",
        "--at",
        "Rental apartment / Kitchen / Fridge / Freezer drawer",
    ]);
    let v = lots(&e, "Chicken thighs");
    assert_eq!(rows(&v)[0]["expires_at"], "2026-10-01", "freezer: 9 months");
    assert_eq!(rows(&v)[0]["frozen_at"], "2026-01-01");

    // lot add: the lot's location decides
    e.run(&[
        "lot",
        "add",
        "Ribeye",
        "2",
        "--at",
        "Rental apartment / Garage / Chest Freezer",
        "--acquired",
        "2026-01-01",
    ]);
    let v = lots(&e, "Ribeye");
    let frozen = rows(&v).iter().find(|l| l["frozen_at"] == "2026-01-01").expect("frozen lot");
    assert_eq!(frozen["expires_at"], "2026-05-01", "freezer: 4 months");
    // explicit dates win
    e.run(&["lot", "add", "Ribeye", "1", "--expires", "2026-01-02"]);
    assert!(rows(&lots(&e, "Ribeye")).iter().any(|l| l["expires_at"] == "2026-01-02"));
    // best-by presets
    e.run(&["add", "Sourdough", "--preset", "bread", "--at", "Rental apartment / Kitchen / Pantry"]);
    assert_eq!(rows(&lots(&e, "Sourdough"))[0]["best_by"], "2026-01-15");
    // unknown preset
    let err = e.fail(&["add", "Thing", "--preset", "nope"], 3);
    assert!(err.contains("no preset 'nope'"), "{err}");
}

#[test]
fn intervals_become_custom_fields() {
    let e = Env::pinned();
    e.run(&["add", "Hall smoke alarm", "--preset", "smoke-detector", "--bought", "2020-03-01"]);
    let v = e.json(&["show", "Hall smoke alarm"]);
    assert_eq!(v["data"]["fields"]["Replace By"], "2030-03-01");
    assert_eq!(v["data"]["fields"]["Next Inspection"], "2026-01-01", "monthly test, rolled forward to today");
    e.run(&["add", "Kitchen extinguisher", "--preset", "fire-extinguisher", "--field", "Installed=2025-06-01"]);
    let v = e.json(&["show", "Kitchen extinguisher"]);
    assert_eq!(v["data"]["fields"]["Next Service"], "2026-06-01");
    assert!(lots(&e, "Kitchen extinguisher")["data"].as_array().unwrap().is_empty(), "no lot for durables");
}

#[test]
fn user_presets_override_builtins() {
    let e = Env::pinned();
    let dir = e.path("config").join("wherebox-cli").join("presets").join("kinds");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("meat.raw.toml"),
        "name = \"meat.raw\"\ndescription = \"house rule\"\ncategory = \"food\"\n[shelf_life]\ndate = \"use_by\"\nfridge = \"2d\"\n",
    )
    .unwrap();
    let v = e.json(&["preset", "show", "meat.raw"]);
    assert!(v["data"]["origin"].as_str().unwrap().ends_with("meat.raw.toml"));
    e.run(&["add", "Steak", "--preset", "meat.raw", "--at", "Fridge"]);
    assert_eq!(rows(&lots(&e, "Steak"))[0]["expires_at"], "2026-01-03");
}

#[test]
fn alert_days_widen_expiring() {
    let e = Env::pinned();
    e.run(&["add", "Ibuprofen", "--preset", "medicine", "--expires", "2026-01-20"]);
    e.run(&["add", "Passport", "--preset", "document", "--expires", "2026-03-15"]);
    let v = e.json(&["expiring"]);
    let names: Vec<&str> = rows(&v).iter().map(|r| r["item"].as_str().unwrap()).collect();
    assert_eq!(names, ["Ibuprofen", "Passport"], "30 and 90 day alerts beat the 7 day default");
    let v = e.json(&["expiring", "--within", "7d"]);
    assert!(rows(&v).is_empty(), "an explicit window wins");
}
