//! Kind presets: `preset ls|show`, the shelf-life type a food preset names,
//! and the expiry computed from it by storage (fridge vs freezer).

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
    assert_eq!(row(&v, "name", "meat.raw")["shelf_life_type"], "raw-beef");
    assert_eq!(row(&v, "name", "pantry")["shelf_life_type"], serde_json::Value::Null);
    let s = e.json(&["preset", "show", "smoke-detector"]);
    assert_eq!(s["data"]["intervals"]["replace"], "10y");
    assert_eq!(
        s["data"]["sources"][0]["url"],
        "https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms"
    );
    let table = e.run(&["preset", "show", "poultry.raw"]);
    assert!(table.contains("raw-chicken") && table.contains("shelf-life show"), "{table}");
    let err = e.fail(&["preset", "show", "nope"], 3);
    assert!(err.contains("preset ls"), "{err}");
}

#[test]
fn preset_types_give_computed_expiry_by_storage() {
    let e = Env::pinned(); // today = 2026-01-01
    e.run(&["add", "Ribeye", "--preset", "meat.raw", "--at", "Rental apartment / Kitchen / Fridge"]);
    let v = lots(&e, "Ribeye");
    assert_eq!(rows(&v).len(), 1, "a preset with a shelf-life type starts a lot");
    let l = &rows(&v)[0];
    assert_eq!(
        (&l["expires_at"], &l["expiry_basis"], &l["storage"]),
        (&"2026-01-04".into(), &"computed".into(), &"fridge".into())
    );
    assert_eq!(l["shelf_life"], "raw-beef");

    // a name the types know beats the preset's default type
    e.run(&[
        "add",
        "Chicken thighs",
        "--preset",
        "poultry.raw",
        "--at",
        "Rental apartment / Kitchen / Fridge / Freezer drawer",
    ]);
    let v = e.json(&["show", "Chicken thighs"]);
    assert_eq!((&v["data"]["shelf_life"], &v["data"]["shelf_life_basis"]), (&"raw-chicken".into(), &"name".into()));
    assert_eq!(rows(&lots(&e, "Chicken thighs"))[0]["expires_at"], "2026-10-01", "freezer: 9 months");

    // lot add: the lot's location decides
    e.run(&["lot", "add", "Ribeye", "2", "--at", "Rental apartment / Garage / Chest Freezer"]);
    let v = lots(&e, "Ribeye");
    let frozen = rows(&v).iter().find(|l| l["storage"] == "freezer").expect("frozen lot").clone();
    assert_eq!(frozen["expires_at"], "2026-05-01", "freezer: 4 months");
    // a printed best-by wins
    e.run(&["lot", "add", "Ribeye", "1", "--best-by", "2026-01-02"]);
    assert!(rows(&lots(&e, "Ribeye"))
        .iter()
        .any(|l| l["expires_at"] == "2026-01-02" && l["expiry_basis"] == "printed"));
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
        "name = \"meat.raw\"\ndescription = \"house rule\"\ncategory = \"food\"\nshelf_life_type = \"ground-meat\"\n",
    )
    .unwrap();
    let v = e.json(&["preset", "show", "meat.raw"]);
    assert!(v["data"]["origin"].as_str().unwrap().ends_with("meat.raw.toml"));
    e.run(&["add", "Tri-tip", "--preset", "meat.raw", "--at", "Fridge"]);
    assert_eq!(rows(&lots(&e, "Tri-tip"))[0]["expires_at"], "2026-01-02", "ground meat: 1 day");
}
