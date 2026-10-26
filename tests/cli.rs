//! End-to-end tests driving the `wherebox` binary: items, locations, labels,
//! history, generic imports, output formats, config and maintenance.

mod support;

use predicates::prelude::*;
use serde_json::{json, Value};
use support::{row, rows, Env};

fn names(v: &Value) -> Vec<String> {
    rows(v).iter().map(|r| r["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn init_and_check() {
    let e = Env::new();
    let v = e.json(&["db", "init"]);
    assert_eq!(v["data"]["created"], true);
    assert_eq!(v["data"]["encrypted"], true);
    assert_eq!(e.json(&["db", "check"])["data"]["ok"], true);
    assert_eq!(e.json(&["ls"])["count"], 0);
}

#[test]
fn source_date_epoch_pins_generated_at() {
    let e = Env::pinned();
    let v = e.json(&["ls"]);
    assert_eq!(v["generated_at"], "2026-01-01T00:00:00Z");
}

#[test]
fn add_show_and_asset_ids() {
    let e = Env::pinned();
    let v = e.json(&[
        "add",
        "Cordless Drill",
        "--at",
        "Lake House / Garage / Shelf 2",
        "--price",
        "$1,149.50",
        "--bought",
        "2024-05-01",
        "--from",
        "Home Depot",
        "--serial",
        "DR-1",
        "--model",
        "DCD777",
        "--maker",
        "DeWalt",
        "--owner",
        "Family Trust",
        "--label",
        "tools,power",
        "--insured",
        "--warranty-expires",
        "2027-05-01",
        "--field",
        "Color=yellow",
        "--source",
        "amazon-email:113-55",
    ]);
    let d = &v["data"];
    assert_eq!(d["asset_id"], "000-001");
    assert_eq!(d["location"], "Lake House / Garage / Shelf 2");
    assert_eq!(d["purchase_price"], 1149.5);
    assert_eq!(d["insured"], true);
    assert_eq!(d["labels"], json!(["power", "tools"]));
    assert_eq!(d["fields"], json!({"Color": "yellow"}));
    assert_eq!(d["sources"], json!(["amazon-email:113-55"]));
    assert_eq!(d["status"], "owned");
    assert_eq!(d["id"].as_str().unwrap().len(), 26);
    let v = e.json(&["add", "Ladder", "--qty", "2"]);
    assert_eq!(v["data"]["asset_id"], "000-002");
    assert_eq!(v["data"]["sources"], json!(["manual"]));
    // Resolution by asset id, id prefix, exact name and unique name part.
    let id = e.json(&["show", "000-001"])["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(e.json(&["show", &id[..12]])["data"]["name"], "Cordless Drill");
    assert_eq!(e.json(&["show", "cordless drill"])["data"]["asset_id"], "000-001");
    assert_eq!(e.json(&["show", "drill"])["data"]["asset_id"], "000-001");
    e.fail(&["show", "nothing like it"], 3);
    // Table show is a key/value list with summaries.
    let t = e.run(&["show", "Ladder"]);
    assert!(t.contains("quantity           2"), "{t}");
    // Bad values are invalid data (exit 4); bad flags are usage errors (exit 2).
    e.fail(&["add", "X", "--price", "cheap"], 4);
    e.fail(&["add", "X", "--status", "eaten"], 4);
    e.fail(&["add", "X", "--qty", "1.5"], 4);
    e.fail(&["add", "X", "--field", "Owner Entity=me"], 2);
    e.fail(&["add", "X", "--bogus"], 2);
    let long = "n".repeat(1001);
    e.fail(&["add", "X", "--notes", &long], 4);
}

#[test]
fn edit_mv_and_history() {
    let e = Env::pinned();
    e.run(&["add", "Drill", "--at", "House / Garage", "--label", "tools", "--price", "100"]);
    e.run(&["add", "Toolbox", "--at", "House / Garage"]);
    e.run(&[
        "edit",
        "Drill",
        "--name",
        "Cordless Drill",
        "--serial",
        "SN9",
        "--label",
        "power",
        "--unlabel",
        "tools",
        "--inside",
        "Toolbox",
        "--field",
        "Color=red",
    ]);
    let d = e.json(&["show", "Cordless Drill"])["data"].clone();
    assert_eq!(d["serial_number"], "SN9");
    assert_eq!(d["labels"], json!(["power"]));
    assert_eq!(d["inside"], "000-002 Toolbox");
    e.run(&["edit", "Cordless Drill", "--clear", "serial_number", "--field", "Color="]);
    let d = e.json(&["show", "Cordless Drill"])["data"].clone();
    assert!(d["serial_number"].is_null());
    assert_eq!(d["fields"], json!({}));
    e.fail(&["edit", "Toolbox", "--inside", "Cordless Drill"], 4);
    e.fail(&["edit", "Toolbox", "--clear", "name"], 2);
    // mv creates the destination and moves several items at once.
    let v = e.json(&["mv", "Cordless Drill", "Toolbox", "--to", "House / Basement / Shelf"]);
    assert_eq!(v["count"], 2);
    assert_eq!(e.json(&["show", "Toolbox"])["data"]["location"], "House / Basement / Shelf");
    // `--to Garage` finds the existing House / Garage.
    e.run(&["mv", "Toolbox", "--to", "Garage"]);
    assert_eq!(e.json(&["show", "Toolbox"])["data"]["location"], "House / Garage");
    e.run(&["value", "Cordless Drill", "80", "--on", "2025-12-01", "--note", "eBay comps"]);
    let h = e.json(&["history", "Cordless Drill"]);
    let kinds: Vec<&str> = rows(&h).iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["created", "edited", "edited", "edited", "edited", "edited", "moved", "valued"], "{h}");
    let moved = row(&h, "kind", "moved");
    assert_eq!(moved["detail"]["from"], "House / Garage");
    assert_eq!(moved["detail"]["to"], "House / Basement / Shelf");
    assert_eq!(row(&h, "kind", "valued")["detail"]["value"], 80.0);
    let d = e.json(&["show", "Cordless Drill"])["data"].clone();
    assert_eq!((d["value"].clone(), d["valued_at"].clone()), (json!(80.0), json!("2025-12-01")));
}

#[test]
fn rm_statuses_and_purge() {
    let e = Env::pinned();
    e.run(&["add", "Old TV", "--price", "500"]);
    e.run(&["add", "Lamp"]);
    e.run(&["add", "Ring", "--value", "2000"]);
    e.run(&["rm", "Old TV", "--as", "sold", "--to", "Neighbor", "--price", "75", "--date", "2025-11-02"]);
    let tv = e.json(&["show", "Old TV"])["data"].clone();
    assert_eq!(
        (tv["status"].clone(), tv["sold_to"].clone(), tv["sold_price"].clone()),
        (json!("sold"), json!("Neighbor"), json!(75.0))
    );
    assert_eq!(tv["sold_time"], "2025-11-02");
    e.run(&["rm", "Lamp"]);
    e.run(&["rm", "Ring", "--as", "lost"]);
    e.fail(&["rm", "Ring", "--as", "owned"], 4);
    e.fail(&["rm", "Lamp", "--as", "gifted", "--price", "3"], 2);
    assert_eq!(e.json(&["ls"])["count"], 0);
    assert_eq!(e.json(&["ls", "--all"])["count"], 3);
    assert_eq!(names(&e.json(&["ls", "--status", "disposed"])), ["Lamp"]);
    let kinds: Vec<String> =
        rows(&e.json(&["history", "Ring"])).iter().map(|r| r["kind"].as_str().unwrap().into()).collect();
    assert_eq!(kinds, ["created", "status", "gone"]);
    e.run(&["rm", "Lamp", "--purge"]);
    e.fail(&["show", "Lamp"], 3);
    assert!(e.json(&["history", "Ring"])["count"].as_u64().unwrap() > 0);
}

#[test]
fn ls_filters_sort_and_find() {
    let e = Env::pinned();
    e.run(&[
        "add",
        "Drill",
        "--at",
        "A House / Garage",
        "--label",
        "tools",
        "--owner",
        "Trust",
        "--price",
        "100",
        "--warranty-expires",
        "2026-03-01",
    ]);
    e.run(&[
        "add",
        "Saw",
        "--at",
        "A House / Garage / Shelf",
        "--label",
        "tools",
        "--price",
        "300",
        "--serial",
        "SAW-77",
    ]);
    e.run(&["add", "Sofa", "--at", "A House / Den", "--owner", "LLC", "--value", "900", "--lifetime-warranty"]);
    // With one property, new paths go under it; a second property is added explicitly.
    e.run(&["loc", "add", "B Cabin"]);
    e.run(&[
        "add",
        "Kayak",
        "--at",
        "B Cabin / Shed",
        "--description",
        "Red tandem kayak",
        "--warranty-expires",
        "2030-01-01",
    ]);
    assert_eq!(names(&e.json(&["ls"])), ["Sofa", "Drill", "Saw", "Kayak"], "by location path");
    assert_eq!(names(&e.json(&["ls", "--at", "Garage"])), ["Drill", "Saw"], "subtree");
    assert_eq!(names(&e.json(&["ls", "--label", "tools", "--owner", "trust"])), ["Drill"]);
    assert_eq!(names(&e.json(&["ls", "--sort", "value", "-n", "2"])), ["Sofa", "Saw"]);
    assert_eq!(names(&e.json(&["ls", "--sort", "name", "-r"])), ["Sofa", "Saw", "Kayak", "Drill"]);
    assert_eq!(
        names(&e.json(&["ls", "--warranty-before", "2027-01-01"])),
        ["Drill"],
        "lifetime warranties never expire"
    );
    let v = e.json(&["ls"]);
    assert_eq!(v["total_value"], 1300.0);
    assert_eq!(names(&e.json(&["find", "saw-7"])), ["Saw"], "serial");
    assert_eq!(names(&e.json(&["find", "TANDEM"])), ["Kayak"], "description");
    e.run(&["find", "zzz"]);
    e.fail(&["ls", "--status", "eaten"], 4);
    e.fail(&["ls", "--at", "Nowhere"], 3);
}

#[test]
fn location_tree() {
    let e = Env::pinned();
    e.run(&["loc", "add", "Lake House", "--address", "1 Shore Rd"]);
    e.run(&["loc", "add", "Lake House / Kitchen / Pantry", "--kind", "shelf"]);
    e.fail(&["loc", "add", "Lake House / Kitchen"], 4);
    e.fail(&["loc", "add", "X", "--kind", "planet"], 4);
    e.run(&["add", "Pot", "--at", "Kitchen", "--price", "40"]);
    e.run(&["add", "Flour", "--at", "Pantry", "--price", "5"]);
    e.run(&["add", "Loose thing"]);
    let v = e.json(&["loc", "ls"]);
    let tree: Vec<(&str, &str, u64)> = rows(&v)
        .iter()
        .map(|r| (r["tree"].as_str().unwrap(), r["kind"].as_str().unwrap_or("-"), r["items"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        tree,
        [
            ("Lake House", "property", 2),
            ("  Kitchen", "room", 2),
            ("    Pantry", "shelf", 1),
            ("(no location)", "-", 1)
        ]
    );
    assert_eq!(row(&v, "path", "Lake House")["value"], 45.0);
    assert_eq!(row(&v, "path", "Lake House")["address"], "1 Shore Rd");
    let t = e.run(&["loc", "ls"]);
    assert!(t.lines().any(|l| l.starts_with("    Pantry ") && l.contains(" shelf ")), "{t}");
    // Move a subtree (no cycles), rename, edit, remove only when empty.
    e.run(&["loc", "add", "Cabin"]);
    e.run(&["loc", "mv", "Kitchen", "--to", "Cabin", "--rename", "Galley"]);
    assert_eq!(e.json(&["show", "Flour"])["data"]["location"], "Cabin / Galley / Pantry");
    e.fail(&["loc", "mv", "Cabin", "--to", "Pantry"], 4);
    e.run(&["loc", "mv", "Pantry", "--to", "/"]);
    assert_eq!(e.json(&["show", "Flour"])["data"]["location"], "Pantry");
    e.run(&["loc", "edit", "Pantry", "--kind", "room", "--notes", "under the stairs"]);
    e.fail(&["loc", "rm", "Galley"], 4);
    e.run(&["mv", "Pot", "--to", "Cabin"]);
    e.run(&["loc", "rm", "Galley"]);
    e.fail(&["loc", "rm", "Galley"], 3);
}

#[test]
fn labels() {
    let e = Env::pinned();
    e.run(&["label", "add", "insured", "--description", "on the rider"]);
    e.fail(&["label", "add", "Insured"], 4);
    e.run(&["add", "Ring", "--label", "insured,jewelry"]);
    e.run(&["add", "Watch", "--label", "insured"]);
    let v = e.json(&["label", "ls"]);
    assert_eq!(row(&v, "name", "insured")["items"], 2);
    assert_eq!(row(&v, "name", "jewelry")["items"], 1);
    e.run(&["label", "rm", "insured"]);
    assert_eq!(e.json(&["show", "Ring"])["data"]["labels"], json!(["jewelry"]));
    e.fail(&["label", "rm", "insured"], 3);
}

#[test]
fn attachments_round_trip_and_purge() {
    let e = Env::pinned();
    e.run(&["add", "Fridge", "--at", "Home / Kitchen"]);
    let photo = e.path("fridge.jpg");
    std::fs::write(&photo, b"\xff\xd8 not really a jpeg").unwrap();
    let v = e.json(&["attach", "Fridge", photo.to_str().unwrap(), "--primary"]);
    assert_eq!(v["data"]["kind"], "photo");
    assert_eq!(v["data"]["encrypted"], true);
    e.fail(&["attach", "Fridge", photo.to_str().unwrap()], 4);
    let manual = e.path("manual.pdf");
    std::fs::write(&manual, b"%PDF manual").unwrap();
    e.run(&["attach", "Fridge", manual.to_str().unwrap(), "--kind", "manual"]);
    e.fail(&["attach", "Fridge", manual.to_str().unwrap(), "--kind", "napkin"], 4);
    assert_eq!(e.json(&["attachment", "ls", "Fridge"])["count"], 2);
    let back = e.path("back.pdf");
    e.run(&["attachment", "get", "2", back.to_str().unwrap()]);
    assert_eq!(std::fs::read(&back).unwrap(), b"%PDF manual");
    let show = e.json(&["show", "Fridge"])["data"].clone();
    assert_eq!(show["attachments"].as_array().unwrap().len(), 2);
    assert_eq!(show["attachments"][0]["primary"], true);
    // Files live under <db dir>/attachments/<item id>/ and go with the item.
    let dir = e.path("attachments").join(show["id"].as_str().unwrap());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    e.run(&["attachment", "rm", "1"]);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    e.run(&["rm", "Fridge", "--purge"]);
    assert!(!dir.exists());
}

#[test]
fn generic_csv_import_with_mapping() {
    let e = Env::pinned();
    let csv = e.path("stuff.csv");
    std::fs::write(
        &csv,
        "Item,Qty,Where,Cost,Bought,Brand,Tags,Colour,Notes\n\
         Drill,1,House / Garage,149.99,05/01/2024,DeWalt,tools;power,yellow,\n\
         Sofa,1,House / Den,900,11/24/2023,IKEA,,grey,\"big, comfy\"\n",
    )
    .unwrap();
    let map = e.path("map.toml");
    std::fs::write(
        &map,
        "date_format = \"%m/%d/%Y\"\n[columns]\n\"field.Color\" = \"Colour\"\n[defaults]\nowner_entity = \"Trust\"\n",
    )
    .unwrap();
    let v = e.json(&["import", csv.to_str().unwrap(), "--mapping", map.to_str().unwrap(), "--label", "imported"]);
    assert_eq!(v["format"], "csv");
    assert_eq!(v["summary"]["created"], 2, "{v}");
    let d = e.json(&["show", "Drill"])["data"].clone();
    assert_eq!(d["purchase_time"], "2024-05-01");
    assert_eq!(d["manufacturer"], "DeWalt");
    assert_eq!(d["labels"], json!(["imported", "power", "tools"]));
    assert_eq!(d["fields"], json!({"Color": "yellow"}));
    assert_eq!(d["owner_entity"], "Trust");
    assert_eq!(e.json(&["show", "Sofa"])["data"]["notes"], "big, comfy");
    // --map on the command line; a mapped column that does not exist is a usage error.
    e.fail(&["import", csv.to_str().unwrap(), "--map", "name=Nope"], 2);
    e.fail(&["import", csv.to_str().unwrap(), "--map", "flavour=Item"], 2);
    // --at for rows without a location.
    let bare = e.path("bare.csv");
    std::fs::write(&bare, "name\nBroom\n").unwrap();
    e.run(&["import", bare.to_str().unwrap(), "--at", "Garage"]);
    assert_eq!(e.json(&["show", "Broom"])["data"]["location"], "House / Garage");
}

#[test]
fn native_json_jsonl_and_csv_round_trip() {
    let e = Env::pinned();
    e.seed();
    e.run(&["edit", "Red Toolbox", "--source", "amazon-email:9"]);
    for format in ["json", "jsonl", "csv"] {
        let out = e.run(&["export", "-f", format]);
        let file = e.path(&format!("export.{format}"));
        std::fs::write(&file, &out).unwrap();
        let other = Env::pinned();
        let v = other.json(&["import", file.to_str().unwrap()]);
        assert_eq!(v["summary"]["created"], 7, "{format}: {v}");
        assert_eq!(other.run(&["export", "-f", format]), out, "{format} round trip");
        assert_eq!(other.run(&["export"]), e.run(&["export"]), "{format} -> homebox agrees");
        // And it re-imports as unchanged.
        assert_eq!(other.json(&["import", file.to_str().unwrap()])["summary"]["unchanged"], 7, "{format}");
    }
}

#[test]
fn spreadsheet_import() {
    let e = Env::pinned();
    let path = e.path("inventory.xlsx");
    let mut wb = rust_xlsxwriter::Workbook::new();
    let ws = wb.add_worksheet().set_name("Stuff").unwrap();
    ws.write_string(0, 0, "My inventory").unwrap();
    for (c, h) in ["Name", "Location", "Price", "Purchase Date"].iter().enumerate() {
        ws.write_string(2, c as u16, *h).unwrap();
    }
    ws.write_string(3, 0, "Drill").unwrap();
    ws.write_string(3, 1, "House / Garage").unwrap();
    ws.write_number(3, 2, 149.5).unwrap();
    ws.write_string(3, 3, "2024-05-01").unwrap();
    wb.save(&path).unwrap();
    let v = e.json(&["import", path.to_str().unwrap(), "--header-row", "3"]);
    assert_eq!(v["format"], "spreadsheet");
    assert_eq!(v["summary"]["created"], 1, "{v}");
    let d = e.json(&["show", "Drill"])["data"].clone();
    assert_eq!((d["purchase_price"].clone(), d["purchase_time"].clone()), (json!(149.5), json!("2024-05-01")));
}

#[test]
fn output_formats() {
    let e = Env::pinned();
    e.run(&["add", "Drill", "--at", "House / Garage", "--price", "101.256", "--notes", "a, b"]);
    let table = e.run(&["ls"]);
    assert!(table.starts_with("ASSET_ID"), "{table}");
    assert!(table.contains("101.26"));
    let csv = e.run(&["ls", "-f", "csv", "--columns", "name,value,notes", "--precision", "1"]);
    assert_eq!(csv, "name,value,notes\nDrill,101.3,\"a, b\"\n");
    let tsv = e.run(&["ls", "-f", "tsv", "--columns", "name,location", "--no-header"]);
    assert_eq!(tsv, "Drill\tHouse / Garage\n");
    let semi = e.run(&["ls", "-f", "csv", "--columns", "name,serial_number", "--delimiter", ";", "--null", "NA"]);
    assert_eq!(semi, "name;serial_number\nDrill;NA\n");
    let jl = e.run(&["ls", "-f", "jsonl"]);
    let v: Value = serde_json::from_str(jl.trim()).unwrap();
    assert_eq!(v["name"], "Drill");
    let out = e.path("q.json");
    e.run(&["ls", "--json", "-o", out.to_str().unwrap()]);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    assert_eq!(v["count"], 1);
    // --json wins over --format; -f json is the same envelope.
    let a: Value = serde_json::from_str(&e.run(&["ls", "-f", "csv", "--json"])).unwrap();
    assert_eq!(a["kind"], "items");
    // Mutations print nothing on stdout in table mode, the record otherwise.
    assert_eq!(e.run(&["add", "Saw"]), "");
}

#[test]
fn layered_config_with_sources() {
    let e = Env::new();
    let cfg = e.path("custom.toml");
    std::fs::write(&cfg, "format = \"csv\"\nprecision = 4\nexpiring_within = \"3d\"\n[csv]\ndelimiter = \";\"\n[profiles.cabin]\ntitle = \"Cabin\"\n").unwrap();
    let out = e
        .cmd()
        .env("WHEREBOX_CONFIG", &cfg)
        .env("WHEREBOX_PRECISION", "3")
        .args(["config", "show", "--effective", "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let get = |k: &str| rows(&v).iter().find(|r| r["key"] == k).unwrap().clone();
    assert_eq!(get("format")["source"], "flag");
    assert_eq!(get("precision")["value"], "3");
    assert_eq!(get("precision")["source"], "env");
    assert_eq!(get("csv_delimiter")["value"], ";");
    assert_eq!(get("csv_delimiter")["source"], "file");
    assert_eq!(get("expiring_within")["value"], "3d");
    assert_eq!(get("color")["source"], "env"); // NO_COLOR
    assert_eq!(get("profile")["source"], "default");
    assert_eq!(v["config_path"], cfg.to_str().unwrap());

    // config set writes the XDG file and is picked up afterwards
    e.run(&["config", "set", "format", "json"]);
    let path = e.run(&["config", "path", "-f", "tsv", "--no-header", "--columns", "path"]);
    let want = e.path("config").join("wherebox-cli").join("config.toml");
    assert_eq!(path.trim(), want.to_str().unwrap());
    let v: Value = serde_json::from_str(&e.run(&["ls"])).unwrap();
    assert_eq!(v["kind"], "items");
    e.cmd().args(["config", "set", "format", "xml"]).assert().code(7);
    e.cmd().args(["config", "set", "nope", "1"]).assert().code(7);
    e.cmd().args(["config", "set", "profile", "lake house"]).assert().code(7);
    e.cmd().args(["config", "set", "expiring_within", "soon"]).assert().code(7);
    e.run(&["config", "unset", "format"]);
    assert!(e.run(&["ls"]).is_empty());
    // a missing explicit config file is a config error
    e.cmd().args(["--config", e.path("missing.toml").to_str().unwrap(), "ls"]).assert().code(7);
}

#[test]
fn db_maintenance() {
    let e = Env::pinned();
    e.seed();
    let p = e.json(&["db", "path"]);
    assert_eq!(p["data"]["path"], e.db().to_str().unwrap());
    let m = e.json(&["db", "migrate", "--status"]);
    assert!(rows(&m).iter().all(|r| r["applied"] == true));
    let backup = e.path("backup.db");
    e.run(&["db", "backup", backup.to_str().unwrap()]);
    e.run(&["db", "vacuum"]);
    assert_eq!(e.json(&["db", "check"])["data"]["ok"], true);
    // the backup is a working database
    let out = e.cmd().env("WHEREBOX_DB", &backup).args(["ls", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["count"], 6);
    let info = e.json(&["db", "info"]);
    assert_eq!(info["data"]["items"], 7);
    assert_eq!(info["data"]["lots"], 2);
    assert_eq!(info["data"]["locations"], 6);
}

#[test]
fn errors_are_machine_readable() {
    let e = Env::new();
    e.cmd().args(["ls", "--bogus"]).assert().code(2);
    let out = e.cmd().args(["show", "ghost", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["kind"], "error");
    assert_eq!(err["error"]["kind"], "not_found");
    // format from env still yields machine-readable errors
    let out = e.cmd().env("WHEREBOX_FORMAT", "json").args(["show", "ghost"]).output().unwrap();
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"]["code"], 3);
    e.cmd().args(["show", "ghost"]).assert().code(3).stderr(predicate::str::contains("no item 'ghost'"));
}

#[test]
fn color_follows_the_setting() {
    let e = Env::pinned();
    e.run(&["add", "Milk", "--best-by", "2025-12-30"]);
    let colored = e.run(&["expiring", "--color", "always"]);
    assert!(colored.contains("\x1b[31mexpired\x1b[0m"), "{colored}");
    assert!(!e.run(&["expiring"]).contains('\x1b'));
}

#[test]
fn completions_and_man() {
    let e = Env::new();
    for shell in ["bash", "zsh", "fish", "nushell", "elvish", "powershell"] {
        let out = e.run(&["completions", shell]);
        assert!(out.contains("wherebox"), "{shell}");
    }
    let man = e.run(&["man"]);
    assert!(man.contains(".TH wherebox"));
    let dir = e.path("man");
    e.run(&["man", "--dir", dir.to_str().unwrap()]);
    for page in ["wherebox-import.1", "wherebox-loc-add.1", "wherebox-eat.1", "wherebox-profile-init.1"] {
        assert!(dir.join(page).exists(), "{page}");
    }
}
