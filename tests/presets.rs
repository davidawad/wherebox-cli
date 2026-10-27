//! Kind presets: `preset ls|show`, their default shelf-life types (expiry
//! computed by where the lot is kept), intervals and user overrides.

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
    assert_eq!(row(&v, "name", "meat.raw")["shelf_life"], "raw-meat");
    assert_eq!(row(&v, "name", "pantry")["shelf_life"], serde_json::Value::Null);
    let s = e.json(&["preset", "show", "smoke-detector"]);
    assert_eq!(s["data"]["intervals"]["replace"], "10y");
    assert_eq!(
        s["data"]["sources"][0]["url"],
        "https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms"
    );
    let m = e.json(&["preset", "show", "poultry.raw"]);
    assert_eq!(m["data"]["shelf_life"], "raw-poultry");
    let table = e.run(&["preset", "show", "meat.raw"]);
    assert!(table.contains("raw-meat"), "{table}");
    let err = e.fail(&["preset", "show", "nope"], 3);
    assert!(err.contains("preset ls"), "{err}");
}

#[test]
fn preset_types_compute_expiry_by_storage() {
    let e = Env::pinned(); // today = 2026-01-01
    e.run(&["add", "Ribeye", "--preset", "meat.raw", "--at", "Rental apartment / Kitchen / Fridge"]);
    let v = lots(&e, "Ribeye");
    assert_eq!(rows(&v).len(), 1, "a perishable starts a lot");
    assert_eq!(rows(&v)[0]["expires_at"], "2026-01-04", "fridge: 3 days");
    assert_eq!(rows(&v)[0]["expiry_basis"], "shelf_life");
    assert_eq!(rows(&v)[0]["best_by"], serde_json::Value::Null, "nothing stored");

    e.run(&[
        "add",
        "Chicken thighs",
        "--preset",
        "poultry.raw",
        "--at",
        "Rental apartment / Kitchen / Fridge / Freezer drawer",
    ]);
    assert_eq!(rows(&lots(&e, "Chicken thighs"))[0]["expires_at"], "2026-10-01", "freezer: 9 months");

    // lot add: the lot's location decides
    e.run(&["lot", "add", "Ribeye", "2", "--at", "Rental apartment / Garage / Chest Freezer"]);
    let v = lots(&e, "Ribeye");
    assert!(rows(&v).iter().any(|l| l["expires_at"] == "2026-05-01"), "freezer: 4 months");
    // a printed best-by wins
    e.run(&["lot", "add", "Ribeye", "1", "--best-by", "2026-01-02"]);
    assert!(rows(&lots(&e, "Ribeye"))
        .iter()
        .any(|l| l["expires_at"] == "2026-01-02" && l["expiry_basis"] == "best_by"));
    // bread in the pantry: 14 days
    e.run(&["add", "Sourdough", "--preset", "bread", "--at", "Rental apartment / Kitchen / Pantry"]);
    assert_eq!(rows(&lots(&e, "Sourdough"))[0]["expires_at"], "2026-01-15");
    // dry goods: no expiry
    e.run(&["add", "Basmati Rice", "--preset", "pantry", "--lot"]);
    assert_eq!(rows(&lots(&e, "Basmati Rice"))[0]["expires_at"], serde_json::Value::Null);
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
    // the old inline table: a shelf-life type named after the preset
    e.run(&["add", "Mystery cut", "--preset", "meat.raw", "--at", "Fridge"]);
    assert_eq!(rows(&lots(&e, "Mystery cut"))[0]["expires_at"], "2026-01-03");
    assert_eq!(e.json(&["show", "Mystery cut"])["data"]["shelf_life"], "meat.raw");
}

#[test]
fn medicine_and_documents_never_expire() {
    let e = Env::pinned();
    let err = e.fail(&["add", "Ibuprofen", "--preset", "medicine", "--best-by", "2026-01-20"], 4);
    assert!(err.contains("not perishable"), "{err}");
    e.fail(&["add", "Passport", "--preset", "document", "--expires", "2026-03-15"], 2);
    e.run(&["add", "Ibuprofen", "--preset", "medicine", "--lot"]);
    assert!(rows(&e.json(&["expiring", "--within", "1y"])).is_empty());
}
