//! Receipt intake with synthetic Sprouts-style and ALDI-style receipts
//! (tests/fixtures/receipts): weight lines, multi-quantity, coupons, voids
//! and fees; chain and store detection; a PLU matched against a store
//! product; dry runs; double imports; review files; JSON round trips; and
//! OCR through a stand-in `tesseract` (the real one is never needed).

mod support;

use serde_json::Value;
use support::{fixture, row, rows, Env};

fn receipt(name: &str) -> String {
    fixture("receipts").join(name).to_str().unwrap().to_string()
}

/// A database with two Sprouts stores, an ALDI, and a Sprouts product for
/// organic bananas (PLU 94011) linked to an item.
fn stocked() -> Env {
    let e = Env::pinned();
    e.run(&["store", "add", "sprouts-north", "--chain", "Sprouts", "--address", "100 Example Blvd, Anytown AZ"]);
    e.run(&["store", "add", "sprouts-south", "--chain", "Sprouts", "--address", "9 Other St, Anytown AZ"]);
    e.run(&["store", "add", "aldi-main", "--chain", "ALDI", "--address", "1234 Example Rd"]);
    e.run(&["add", "Organic Bananas", "--unit", "lb"]);
    e.run(&[
        "source",
        "add",
        "bananas",
        "--chain",
        "Sprouts",
        "--name",
        "Organic Bananas",
        "--plu",
        "94011",
        "--item",
        "Organic Bananas",
        "--price",
        "0.79",
        "--seen",
        "2026-01-01",
    ]);
    e
}

/// Line `n` of a review file.
fn line(r: &mut Value, n: u64) -> &mut Value {
    r["lines"].as_array_mut().unwrap().iter_mut().find(|l| l["n"] == n).unwrap()
}

fn import(e: &Env, args: &[&str]) -> Value {
    e.json(&[&["receipt", "import"], args].concat())
}

#[test]
fn sprouts_receipt_becomes_lots_with_store_price_and_date() {
    let e = stocked();
    let v = import(&e, &[&receipt("sprouts.txt"), "--yes"]);
    let head = &v["receipts"][0];
    assert_eq!(head["chain"], "Sprouts");
    assert_eq!(head["store"], "sprouts-north", "picked by the address in the header");
    assert_eq!((head["date"].as_str(), head["time"].as_str()), (Some("2026-10-14"), Some("17:42")));
    assert_eq!(head["total"], 22.81);
    assert_eq!(head["skipped"][0]["why"], "fee");
    // the PLU matched the chain's product, so its item
    let plu = rows(&v).iter().find(|r| r["n"] == 11).unwrap();
    assert_eq!(
        (plu["tier"].as_str(), plu["action"].as_str(), plu["item"].as_str()),
        (Some("code"), Some("add"), Some("Organic Bananas"))
    );
    // a weight line, a coupon, a void
    let bananas = rows(&v).iter().find(|r| r["n"] == 7).unwrap();
    assert_eq!(
        (bananas["qty"].as_f64(), bananas["unit"].as_str(), bananas["total"].as_f64()),
        (Some(2.12), Some("lb"), Some(2.1))
    );
    let yogurt = rows(&v).iter().find(|r| r["n"] == 12).unwrap();
    assert_eq!((yogurt["total"].as_f64(), yogurt["discount"].as_f64()), (Some(4.49), Some(1.0)));
    let walnuts = rows(&v).iter().find(|r| r["n"] == 14).unwrap();
    assert_eq!((walnuts["action"].as_str(), walnuts["lot"].is_null()), (Some("void"), true));
    let avocado = rows(&v).iter().find(|r| r["n"] == 9).unwrap();
    assert_eq!((avocado["qty"].as_f64(), avocado["item"].as_str()), (Some(3.0), Some("Hass Avocado")));
    // lots: date, unit price, vendor, source ref
    let lots = e.json(&["lot", "ls", "Organic Bananas"]);
    let ls = rows(&lots);
    assert_eq!(ls.len(), 2, "{lots}");
    assert!(ls.iter().all(|l| l["acquired_at"] == "2026-10-14" && l["vendor"] == "sprouts-north"));
    let id = head["receipt_id"].as_str().unwrap();
    assert!(ls.iter().all(|l| l["source_ref"].as_str().unwrap().starts_with(&format!("receipt:{id}:"))));
    let weighed = ls.iter().find(|l| l["qty"] == 2.12).unwrap();
    assert_eq!(weighed["unit_price"], 0.99);
    // the store product: the receipt's price and date (newer than its own)
    let src = e.json(&["source", "ls", "bananas"]);
    let p = &rows(&src)[0];
    assert_eq!((p["plu"].as_str(), p["price_seen"].as_str()), (Some("94011"), Some("2026-10-14")));
    // new items, with a shelf-life type from the lexicon, and new store products
    let chicken = e.json(&["show", "Boneless Skinless Chicken Breast"]);
    assert_eq!(chicken["data"]["shelf_life"], "raw-poultry");
    assert_eq!(chicken["data"]["unit"], "lb");
    let src = e.json(&["source", "ls", "--store", "sprouts-north"]);
    let y = row(&src, "food", "Greek Yogurt Plain");
    assert_eq!(
        (y["size"].as_str(), y["price"].as_f64(), y["section"].as_str()),
        (Some("32 oz"), Some(5.49), Some("dairy"))
    );
    // the receipt record
    let r = e.json(&["receipt", "ls"]);
    assert_eq!(rows(&r).len(), 1);
    assert_eq!(rows(&r)[0]["lots"], 6 - 1, "5 kept lines, the void left out");
    let text = e.run(&["receipt", "show", id, "--text"]);
    assert!(text.starts_with("SPROUTS FARMERS MARKET\n"));
    let shown = e.json(&["receipt", "show", &id[id.len() - 8..]]);
    assert_eq!(shown["lots"].as_array().unwrap().len(), 5);
}

#[test]
fn aldi_receipt_price_you_pay_voids_and_fees() {
    let e = stocked();
    let v = import(&e, &[&receipt("aldi.txt"), "--yes", "--min-confidence", "0.3"]);
    let head = &v["receipts"][0];
    assert_eq!((head["chain"].as_str(), head["store"].as_str()), (Some("ALDI"), Some("aldi-main")));
    assert_eq!(head["lots"], 4);
    let eggs = rows(&v).iter().find(|r| r["n"] == 9).unwrap();
    assert_eq!(
        (eggs["total"].as_f64(), eggs["discount"].as_f64(), eggs["item"].as_str()),
        (Some(3.69), Some(0.5), Some("Large Brown Eggs"))
    );
    let spinach = rows(&v).iter().find(|r| r["n"] == 8).unwrap();
    assert_eq!((spinach["qty"].as_f64(), spinach["unit_price"].as_f64()), (Some(2.0), Some(1.29)));
    assert_eq!(rows(&v).iter().find(|r| r["n"] == 12).unwrap()["action"], "void");
    // ALDI item codes are SKUs on the new store products
    let src = e.json(&["source", "ls", "--store", "aldi-main"]);
    assert!(rows(&src).iter().any(|p| p["sku"] == "702345"), "{src}");
    // a second ALDI receipt finds the product by SKU
    let again = "ALDI\n1234 Example Rd\n11/02/2026 10:00\n702345 ORG BABY SPINACH 1.29 FA\nTOTAL 1.29\n";
    std::fs::write(e.path("aldi2.txt"), again).unwrap();
    let v = import(&e, &[e.path("aldi2.txt").to_str().unwrap(), "--yes"]);
    let l = &rows(&v)[0];
    assert_eq!((l["tier"].as_str(), l["action"].as_str()), (Some("code"), Some("add")));
}

#[test]
fn dry_run_writes_nothing() {
    let e = stocked();
    let before = e.json(&["export", "-f", "json"]);
    let v = import(&e, &[&receipt("sprouts.txt"), "--yes", "--dry-run"]);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["receipts"][0]["lots"], 5);
    assert!(rows(&e.json(&["receipt", "ls"])).is_empty());
    let after = e.json(&["export", "-f", "json"]);
    assert_eq!(before["data"], after["data"]);
    assert_eq!(before["store_products"], after["store_products"]);
    // a dry run without --yes writes no review file either
    e.cmd()
        .current_dir(e.dir.path())
        .args(["receipt", "import", &receipt("sprouts.txt"), "--dry-run"])
        .assert()
        .success();
    assert!(!e.path("sprouts.review.json").exists());
}

#[test]
fn double_import_is_refused_unless_forced() {
    let e = stocked();
    import(&e, &[&receipt("sprouts.txt"), "--yes"]);
    let err = e.fail(&["receipt", "import", &receipt("sprouts.txt"), "--yes"], 4);
    assert!(err.contains("already imported") && err.contains("--force"), "{err}");
    assert_eq!(rows(&e.json(&["receipt", "ls"])).len(), 1);
    import(&e, &[&receipt("sprouts.txt"), "--yes", "--force"]);
    assert_eq!(rows(&e.json(&["receipt", "ls"])).len(), 2);
    assert_eq!(rows(&e.json(&["lot", "ls", "Organic Bananas"])).len(), 4);
}

#[test]
fn review_file_round_trip_and_apply() {
    let e = stocked();
    e.run(&["add", "Greek Yogurt"]);
    let review = e.path("r.json");
    let v = import(&e, &[&receipt("sprouts.txt"), "--review", review.to_str().unwrap()]);
    assert_eq!(v["receipts"][0]["lots"], 0, "nothing imported");
    assert!(rows(&e.json(&["receipt", "ls"])).is_empty());
    let mut r: Value = serde_json::from_str(&std::fs::read_to_string(&review).unwrap()).unwrap();
    assert_eq!(r["schema"], "wherebox/receipt-review/v1");
    assert!(r["receipt"]["ocr_text"].as_str().unwrap().contains("PAPER BAG"));
    // skip the chicken, put the yogurt on the existing item (2 tubs), rename the avocados
    line(&mut r, 16)["action"] = "skip".into();
    line(&mut r, 12)["action"] = "add".into();
    line(&mut r, 12)["item"] = "Greek Yogurt".into();
    line(&mut r, 12)["qty"] = 2.into();
    line(&mut r, 9)["action"] = "new".into();
    line(&mut r, 9)["new_name"] = "Avocados".into();
    std::fs::write(&review, serde_json::to_string_pretty(&r).unwrap()).unwrap();
    let v = e.json(&["receipt", "apply", review.to_str().unwrap()]);
    assert_eq!(v["receipts"][0]["lots"], 4);
    let yogurt = e.json(&["lot", "ls", "Greek Yogurt"]);
    assert_eq!(rows(&yogurt)[0]["qty"], 2.0);
    assert_eq!(rows(&yogurt)[0]["unit_price"], 2.25, "4.49 for 2");
    assert!(e.out(&["show", "Boneless Skinless Chicken Breast"]).status.code() == Some(3), "skipped: no item");
    assert!(e.out(&["show", "Avocados"]).status.success());
    // the same review again is the same receipt
    let err = e.fail(&["receipt", "apply", review.to_str().unwrap()], 4);
    assert!(err.contains("already imported"), "{err}");
    // a bad edit is refused before anything is written
    line(&mut r, 7)["action"] = "maybe".into();
    std::fs::write(&review, serde_json::to_string(&r).unwrap()).unwrap();
    e.fail(&["receipt", "apply", review.to_str().unwrap(), "--force"], 4);
}

#[test]
fn non_interactive_without_yes_writes_a_review_next_to_the_cwd() {
    let e = stocked();
    let out = e.cmd().current_dir(e.dir.path()).args(["receipt", "import", &receipt("aldi.txt")]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("receipt apply"));
    let r: Value = serde_json::from_str(&std::fs::read_to_string(e.path("aldi.review.json")).unwrap()).unwrap();
    assert_eq!(r["receipt"]["chain"], "ALDI");
    assert!(rows(&e.json(&["receipt", "ls"])).is_empty());
}

#[test]
fn receipts_round_trip_through_json_export() {
    let e = stocked();
    let v = import(&e, &[&receipt("sprouts.txt"), "--yes"]);
    let id = v["receipts"][0]["receipt_id"].as_str().unwrap().to_string();
    let export = e.path("export.json");
    e.run(&["export", "-f", "json", "-o", export.to_str().unwrap()]);
    let f = Env::pinned();
    let out = f.json(&["import", export.to_str().unwrap()]);
    assert_eq!(out["receipts"], 1);
    let ls = f.json(&["receipt", "ls"]);
    assert_eq!((rows(&ls)[0]["id"].as_str(), rows(&ls)[0]["lots"].as_u64()), (Some(id.as_str()), Some(5)));
    assert_eq!(f.run(&["receipt", "show", &id, "--text"]), e.run(&["receipt", "show", &id, "--text"]));
    // and the copy refuses the same receipt again
    f.fail(&["receipt", "import", &receipt("sprouts.txt"), "--yes", "--store", "sprouts-north"], 4);
}

#[test]
fn missing_ocr_tool_names_the_package() {
    let e = stocked();
    let png = e.path("receipt.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\nnot really").unwrap();
    std::fs::create_dir_all(e.path("nobin")).unwrap();
    let out = e
        .cmd()
        .env("PATH", e.path("nobin"))
        .args(["receipt", "import", png.to_str().unwrap(), "--yes"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(6));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("tesseract")
            && err.contains("apt install tesseract-ocr")
            && err.contains("brew install tesseract"),
        "{err}"
    );
    let pdf = e.path("receipt.pdf");
    std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
    let out = e
        .cmd()
        .env("PATH", e.path("nobin"))
        .args(["receipt", "import", pdf.to_str().unwrap(), "--yes"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&out.stderr).contains("poppler-utils"));
}

/// The system OCR path, with a stand-in `tesseract` that prints a receipt;
/// `--keep-image` keeps the original file, `--extract-image` gives it back.
#[cfg(unix)]
#[test]
fn images_go_through_tesseract_and_can_be_kept() {
    use std::os::unix::fs::PermissionsExt;
    let e = stocked();
    let bin = e.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("tesseract");
    std::fs::write(&tool, format!("#!/bin/sh\n[ \"$2\" = stdout ] || exit 9\ncat '{}'\n", receipt("aldi.txt")))
        .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let png = e.path("aldi.png");
    let bytes = b"\x89PNG\r\n\x1a\nsynthetic image bytes".to_vec();
    std::fs::write(&png, &bytes).unwrap();
    let out = e
        .cmd()
        .env("PATH", &path)
        .args(["receipt", "import", png.to_str().unwrap(), "--yes", "--keep-image", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let head = &v["receipts"][0];
    assert_eq!((head["kind"].as_str(), head["chain"].as_str()), (Some("image"), Some("ALDI")));
    let id = head["receipt_id"].as_str().unwrap();
    let back = e.path("back.png");
    e.run(&["receipt", "show", id, "--extract-image", back.to_str().unwrap()]);
    assert_eq!(std::fs::read(&back).unwrap(), bytes);
    // without --keep-image only the text and the path are kept
    let out = e
        .cmd()
        .env("PATH", &path)
        .args(["receipt", "import", png.to_str().unwrap(), "--yes", "--force", "--no-path", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = v["receipts"][0]["receipt_id"].as_str().unwrap();
    e.fail(&["receipt", "show", id, "--extract-image", back.to_str().unwrap()], 3);
    let shown = e.json(&["receipt", "show", id]);
    assert!(shown["data"]["file"].is_null());
}
