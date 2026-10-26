//! `report insurance` (per property and rolled up, every format, gaps,
//! renter's view) and `report warranties`.

mod support;

use serde_json::Value;
use support::{rows, Env};

fn p(e: &Env, args: &[&str]) -> String {
    let out = e.cmd().env_remove("WHEREBOX_DB").args(args).output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn seed(e: &Env) {
    let cfg = e.path("config").join("wherebox-cli");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "[profiles.lake-house]\ntitle = \"Lake House\"\n[profiles.rental-apt]\ntitle = \"Rental apartment\"\nrented = true\n",
    )
    .unwrap();
    p(e, &["profile", "init", "lake-house"]);
    p(e, &["profile", "init", "rental-apt"]);
    let photo = e.path("tv.jpg");
    std::fs::write(&photo, b"\xff\xd8\xff photo").unwrap();
    let receipt = e.path("tv-receipt.pdf");
    std::fs::write(&receipt, b"%PDF receipt").unwrap();
    p(
        e,
        &[
            "-P",
            "lake-house",
            "add",
            "TV",
            "--at",
            "Lake House / Living Room",
            "--preset",
            "electronics",
            "--price",
            "500",
            "--value",
            "800",
            "--serial",
            "SN123",
            "--maker",
            "Sony",
            "--model",
            "X90",
            "--bought",
            "2024-05-01",
            "--from",
            "Best Buy",
            "--warranty-expires",
            "2026-02-15",
            "--owner",
            "Alex",
        ],
    );
    p(e, &["-P", "lake-house", "attach", "TV", photo.to_str().unwrap(), "--kind", "photo"]);
    p(e, &["-P", "lake-house", "attach", "TV", receipt.to_str().unwrap(), "--kind", "receipt"]);
    p(
        e,
        &[
            "-P",
            "lake-house",
            "add",
            "Drill",
            "--at",
            "Lake House / Garage",
            "--preset",
            "tool",
            "--price",
            "120",
            "--warranty-expires",
            "2027-06-01",
            "--owner",
            "Lake House LLC",
        ],
    );
    // consumables are not insured contents
    p(
        e,
        &[
            "-P",
            "lake-house",
            "add",
            "Ribeye",
            "--at",
            "Lake House / Kitchen / Fridge",
            "--preset",
            "meat.raw",
            "--price",
            "30",
        ],
    );
    p(e, &["-P", "lake-house", "add", "Old lamp", "--at", "Lake House / Attic", "--price", "40"]);
    p(e, &["-P", "lake-house", "rm", "Old lamp"]);
    p(e, &["-P", "rental-apt", "add", "Couch", "--at", "Rental apartment / Living Room", "--owner", "Alex"]);
}

fn json(e: &Env, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    serde_json::from_str(&p(e, &a)).unwrap()
}

#[test]
fn insurance_rolls_up_every_property_with_totals_and_gaps() {
    let e = Env::pinned();
    seed(&e);
    let v = json(&e, &["report", "insurance", "--all-profiles"]);
    let d = &v["data"];
    let props: Vec<&str> = d["properties"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(props, ["Lake House", "Rental apartment"]);
    let ll = &d["properties"][0];
    assert_eq!(ll["rented"], false);
    assert_eq!(ll["total"], 920.0, "TV 800 (replacement) + drill 120 (purchase price)");
    let rooms: Vec<&str> = ll["rooms"].as_array().unwrap().iter().map(|r| r["room"].as_str().unwrap()).collect();
    assert_eq!(rooms, ["Garage", "Living Room"], "no Kitchen (food) and no Attic (disposed)");
    let drill = &ll["rooms"][0]["items"][0];
    assert_eq!((drill["unit_value"].as_f64(), drill["value_basis"].as_str()), (Some(120.0), Some("purchase price")));
    assert_eq!(drill["line_value"], 120.0);
    let tv = &ll["rooms"][1]["items"][0];
    assert_eq!((tv["photo"].as_str(), tv["receipt"].as_str()), (Some("y"), Some("y")));
    assert!(tv["photo_paths"].as_str().unwrap().contains("attachments"));
    assert_eq!(tv["location"], "Lake House / Living Room");
    let rental = &d["properties"][1];
    assert_eq!((rental["rented"].as_bool(), rental["coverage"].as_str()), (Some(true), Some("contents only")));
    assert_eq!(d["grand_total"], 920.0);
    assert_eq!(d["flagged"], 1);
    assert!(d["header"].to_string().contains("contents only"));
    let gaps: Vec<(String, String)> = d["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["name"].as_str().unwrap().to_string(),
                g["gaps"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect::<Vec<_>>().join(", "),
            )
        })
        .collect();
    assert_eq!(
        gaps,
        [
            ("Drill".to_string(), "no photo, no receipt, no serial".to_string()),
            ("Couch".to_string(), "no value, no photo, no receipt".to_string())
        ]
    );
    // owner filter
    let v = json(&e, &["report", "insurance", "--all-profiles", "--owner", "alex"]);
    assert_eq!(v["data"]["grand_total"], 800.0);
    // one property
    let v = json(&e, &["-P", "rental-apt", "report", "insurance"]);
    assert_eq!(v["data"]["properties"].as_array().unwrap().len(), 1);
}

#[test]
fn insurance_formats() {
    let e = Env::pinned();
    seed(&e);
    let csv = p(&e, &["report", "insurance", "--all-profiles", "--format", "csv"]);
    let mut r = csv::Reader::from_reader(csv.as_bytes());
    let sections: Vec<String> = r.records().map(|x| x.unwrap()[0].to_string()).collect();
    assert_eq!(
        sections,
        [
            "item",
            "room subtotal",
            "item",
            "room subtotal",
            "property total",
            "item",
            "room subtotal",
            "property total",
            "grand total",
            "gap",
            "gap"
        ]
    );
    assert!(csv.contains("contents only (rented)"));

    let table = p(&e, &["report", "insurance", "--all-profiles"]);
    assert!(table.contains("Renter's view: Rental apartment rented; this report covers contents only"), "{table}");
    assert!(table.contains("Gaps (2):"), "{table}");

    let html = e.path("report.html");
    p(&e, &["report", "insurance", "--all-profiles", "--format", "html", "-o", html.to_str().unwrap()]);
    let h = std::fs::read_to_string(&html).unwrap();
    assert!(h.starts_with("<!DOCTYPE html>"));
    assert!(h.contains("Rented property: contents only.") && h.contains("Grand total: $920.00"), "{h}");
    assert!(h.contains("Subtotal: Living Room") && h.contains("$800.00"));

    let xlsx = e.path("report.xlsx");
    p(&e, &["report", "insurance", "--all-profiles", "--format", "xlsx", "-o", xlsx.to_str().unwrap()]);
    let bytes = std::fs::read(&xlsx).unwrap();
    assert!(bytes.starts_with(b"PK"), "a zip container");

    let pdf = e.path("report.pdf");
    p(&e, &["report", "insurance", "--all-profiles", "--format", "pdf", "-o", pdf.to_str().unwrap()]);
    let bytes = std::fs::read(&pdf).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Grand total: $920.00") && text.contains("contents only"), "pdf text");

    // xlsx/pdf elsewhere is a usage error
    let out = e.cmd().args(["ls", "--format", "pdf"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn warranties_expiring() {
    let e = Env::pinned(); // today = 2026-01-01
    seed(&e);
    let v = json(&e, &["report", "warranties", "--all-profiles"]);
    let names: Vec<&str> = rows(&v).iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["TV"]);
    assert_eq!(rows(&v)[0]["days_left"], 45);
    assert_eq!(rows(&v)[0]["receipt"], true);
    let v = json(&e, &["report", "warranties", "--all-profiles", "--expiring", "2y"]);
    assert_eq!(rows(&v).len(), 2);
    let v = json(&e, &["-P", "lake-house", "report", "warranties", "--expiring", "30d"]);
    assert!(rows(&v).is_empty());
}
