//! Lists and cohorts checked against a synthetic property: Amazon-style long
//! product names, consumables with lots (one expired), a "nutty pudding"
//! recipe that is not ready and then ready, wishlists, kits, pins, explain,
//! `list shop` / `shopping --include-list` and `match`.

mod support;

use serde_json::Value;
use support::{row, rows, Env};

const BEACH_HOUSE: &[&str] = &["-P", "beach-house"];

fn p(args: &[&str]) -> Vec<String> {
    BEACH_HOUSE.iter().chain(args).map(|s| s.to_string()).collect()
}

fn run(e: &Env, args: &[&str]) -> String {
    let a = p(args);
    e.run(&a.iter().map(String::as_str).collect::<Vec<_>>())
}

fn json(e: &Env, args: &[&str]) -> Value {
    let a = p(args);
    e.json(&a.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Beach House (today = 2026-01-01).
fn seed(e: &Env) {
    run(e, &["profile", "init", "--title", "Beach House"]);
    let add = |args: &[&str]| run(e, &[&["add"], args].concat());
    add(&[
        "Diamond of California Shelled Walnut Halves & Pieces, 16 oz (Pack of 1)",
        "--at",
        "Kitchen / Pantry",
        "--lot",
        "--qty",
        "1",
        "--field",
        "ASIN=B0WALNUT01",
        "--label",
        "nuts",
    ]);
    add(&["Kirkland Signature Pecan Halves, 2 lbs", "--at", "Kitchen / Pantry", "--lot", "--qty", "1"]);
    // milk: two lots, one past its best-by date
    add(&[
        "Horizon Organic Whole Milk, Half Gallon",
        "--unit",
        "l",
        "--at",
        "Kitchen / Fridge",
        "--lot",
        "--lot-qty",
        "1.9",
        "--best-by",
        "2025-12-28",
    ]);
    add(&["Large Brown Eggs, 12 ct", "--at", "Kitchen / Fridge", "--lot", "--qty", "12", "--best-by", "2026-01-20"]);
    run(e, &["lot", "add", "Large Brown Eggs", "6", "--best-by", "2025-12-01"]);
    add(&["Domino Premium Pure Cane Granulated Sugar, 4 lb Bag", "--at", "Kitchen / Pantry", "--lot", "--qty", "1"]);
    add(&["Cocoa powder", "--unit", "jar", "--at", "Kitchen / Pantry", "--lot", "--qty", "1"]);
    add(&["Silicone baking mold", "--at", "Kitchen", "--product-ref", "0 12345 67890 5"]);
}

fn recipe(e: &Env) {
    run(e, &["list", "new", "Nutty pudding", "--type", "recipe"]);
    let text = e.path("nutty-pudding.txt");
    std::fs::write(
        &text,
        "# Nutty pudding (serves 6)\n\
         - 200 g walnuts, chopped\n\
         - 1 l milk\n\
         - 3 eggs\n\
         - 150 g sugar\n\
         - 2 tbsp cocoa powder\n\
         - 1 mold\n  https://example.com/shop/silicone-mold?utm_source=x\n",
    )
    .unwrap();
    run(e, &["list", "import", "nutty-pudding", text.to_str().unwrap()]);
    run(e, &["list", "edit", "nutty-pudding", "mold", "--fuzzy", "false", "--upc", "012345678905"]);
}

fn statuses(v: &Value) -> Vec<(String, String)> {
    rows(v)
        .iter()
        .map(|r| (r["entry"].as_str().unwrap().to_string(), r["state"].as_str().unwrap().to_string()))
        .collect()
}

#[test]
fn nutty_pudding_is_not_ready_then_ready() {
    let e = Env::pinned();
    seed(&e);
    recipe(&e);
    let list = e.path("config/wherebox-cli/lists/nutty-pudding.toml");
    let toml = std::fs::read_to_string(&list).unwrap();
    assert!(toml.contains("profile = \"beach-house\""), "{toml}");
    assert!(toml.contains("url = \"https://example.com/shop/silicone-mold?utm_source=x\""), "{toml}");

    let out = e.out(&["list", "check", "nutty-pudding", "--json"]);
    assert_eq!(out.status.code(), Some(3), "not ready exits 3");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "list_check");
    assert_eq!(v["ready"], false);
    assert_eq!(v["verdict"], "missing 2 of 6");
    let s = |n: &str, st: &str| (n.to_string(), st.to_string());
    assert_eq!(
        statuses(&v),
        [
            s("walnuts", "have"),
            s("milk", "missing"),
            s("eggs", "have"),
            s("sugar", "have"),
            s("cocoa powder", "unit-mismatch"),
            s("mold", "have"),
        ]
    );
    let walnuts = row(&v, "entry", "walnuts");
    assert_eq!(walnuts["matches"][0]["tier"], "name");
    assert_eq!(walnuts["matches"][0]["basis"], "size from name");
    assert!((walnuts["have"].as_f64().unwrap() - 453.592).abs() < 0.01, "16 oz in g: {walnuts}");
    let milk = row(&v, "entry", "milk");
    assert!((milk["expired"].as_f64().unwrap() - 1.9).abs() < 1e-9, "{milk}");
    assert!(milk["status"].as_str().unwrap().contains("expired"), "{milk}");
    let eggs = row(&v, "entry", "eggs");
    assert_eq!((eggs["have"].as_f64(), eggs["expired"].as_f64()), (Some(12.0), Some(6.0)), "the old lot doesn't count");
    let mold = row(&v, "entry", "mold");
    assert_eq!(
        (mold["matches"][0]["tier"].as_str(), mold["matches"][0]["score"].as_f64()),
        (Some("identifier"), Some(1.0))
    );

    // table output: the verdict line closes it
    let out = e.out(&["list", "check", "nutty-pudding"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.trim_end().ends_with("missing 2 of 6"), "{text}");
    assert!(text.contains("unit-mismatch"), "{text}");

    // shortfall in shopping shape, and merged into `shopping`
    let shop = json(&e, &["list", "shop", "nutty-pudding"]);
    assert_eq!(shop["kind"], "shopping");
    let milk = row(&shop, "item", "milk");
    assert_eq!(
        (milk["reason"].as_str(), milk["qty"].as_f64(), milk["unit"].as_str()),
        (Some("list"), Some(1.0), Some("l"))
    );
    assert_eq!(rows(&shop).len(), 1, "unit-mismatch entries are unknown, not bought: {shop}");
    let shopping = json(&e, &["shopping", "--include-list", "nutty-pudding"]);
    assert_eq!(row(&shopping, "item", "milk")["detail"], "Nutty pudding: missing");

    // fresh milk, cocoa counted in grams: ready
    run(&e, &["lot", "add", "Horizon Organic Whole Milk", "1.9", "--best-by", "2026-01-10"]);
    run(&e, &["list", "edit", "nutty-pudding", "cocoa powder", "--unit", "jar", "--qty", "1"]);
    let out = e.out(&["list", "check", "nutty-pudding", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!((v["ready"].as_bool(), v["verdict"].as_str()), (Some(true), Some("ready")));

    // a short optional entry doesn't block `ready`
    run(&e, &["list", "add", "nutty-pudding", "pecans", "--qty", "2", "--unit", "kg", "--optional"]);
    let v = json(&e, &["list", "check", "nutty-pudding"]);
    let pecans = row(&v, "entry", "pecans");
    assert_eq!(pecans["state"], "short");
    assert!(pecans["status"].as_str().unwrap().starts_with("short (have 0.907 of 2)"), "{pecans}");
    assert_eq!(v["verdict"], "ready");
}

#[test]
fn edits_keep_comments_and_rm_moves_to_trash() {
    let e = Env::pinned();
    seed(&e);
    let dir = e.path("config/wherebox-cli/lists");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("brunch.toml"),
        "# Sunday brunch, hand-written\nname = \"Brunch\"\ntype = \"recipe\"\n\n[[entry]]\n# from the farm stand\nname = \"eggs\"\nqty = 6\n",
    )
    .unwrap();
    run(&e, &["list", "add", "brunch", "walnuts", "--qty", "100", "--unit", "grams", "--exclude", "walnut oil"]);
    run(&e, &["list", "edit", "brunch", "eggs", "--note", "free range", "--any", "duck eggs"]);
    let text = std::fs::read_to_string(dir.join("brunch.toml")).unwrap();
    assert!(text.starts_with("# Sunday brunch, hand-written\nname = \"Brunch\""), "{text}");
    assert!(
        text.contains("# from the farm stand\nname = \"eggs\"\nqty = 6\nnote = \"free range\"\nany = \"duck eggs\"\n"),
        "{text}"
    );
    assert!(text.contains("name = \"walnuts\"\nqty = 100\nunit = \"g\"\nexclude = \"walnut oil\""), "{text}");
    // the exclusion wins over the fuzzy name
    run(&e, &["add", "Walnut Oil, 8.4 fl oz", "--at", "Kitchen / Pantry"]);
    let v = json(&e, &["list", "check", "brunch"]);
    let w = row(&v, "entry", "walnuts");
    assert_eq!(w["matches"].as_array().unwrap().len(), 1, "{w}");
    run(&e, &["list", "drop", "brunch", "2"]);
    let shown = json(&e, &["list", "show", "brunch"]);
    assert_eq!(rows(&shown).len(), 1);
    run(&e, &["list", "rm", "brunch"]);
    assert!(!dir.join("brunch.toml").exists());
    assert!(dir.join(".trash/brunch.toml").exists(), "never deleted");
    e.fail(&["list", "show", "brunch"], 3);
}

#[test]
fn cohorts_include_others_and_cycles_fail() {
    let e = Env::pinned();
    seed(&e);
    run(
        &e,
        &[
            "cohort",
            "new",
            "nuts",
            "--any",
            "pecans",
            "almonds",
            "cashews",
            "--label",
            "nuts",
            "--exclude",
            "walnut oil",
        ],
    );
    run(&e, &["cohort", "new", "baking nuts", "--cohort", "nuts"]);
    let err = e.fail(&["cohort", "add", "nuts", "--cohort", "baking-nuts"], 4);
    assert!(err.contains("cohort cycle: nuts -> baking-nuts -> nuts"), "{err}");
    let c = json(&e, &["cohort", "show", "baking-nuts"]);
    assert_eq!(c["data"]["includes"][0], "nuts");
    run(&e, &["list", "new", "Trail mix", "--type", "kit"]);
    run(&e, &["list", "add", "trail-mix", "nuts", "--qty", "500", "--unit", "g", "--cohort", "baking-nuts"]);
    let v = json(&e, &["list", "check", "trail-mix"]);
    let m = &row(&v, "entry", "nuts")["matches"];
    let tiers: Vec<(&str, &str)> =
        m.as_array().unwrap().iter().map(|x| (x["tier"].as_str().unwrap(), x["via"].as_str().unwrap())).collect();
    assert_eq!(
        tiers,
        [
            ("attribute", "cohort baking-nuts: cohort nuts: label nuts"),
            ("name", "cohort baking-nuts: cohort nuts: any \"pecans\"")
        ]
    );
    assert_eq!(row(&v, "entry", "nuts")["state"], "have", "16 oz + 2 lbs >= 500 g");
    e.fail(&["list", "add", "trail-mix", "seeds", "--cohort", "ghost"], 3);
}

#[test]
fn ambiguity_pins_and_explain() {
    let e = Env::pinned();
    seed(&e);
    run(&e, &["add", "Lindt Dark Chocolate Bar 70%", "--at", "Kitchen / Pantry"]);
    run(&e, &["add", "Ghirardelli Dark Chocolate Chunks", "--at", "Kitchen / Pantry"]);
    run(&e, &["list", "new", "Cookies"]);
    run(&e, &["list", "add", "cookies", "dark chocolate chips", "--threshold", "0.6"]);
    let out = e.out(&["list", "check", "cookies", "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let c = row(&v, "entry", "dark chocolate chips");
    assert_eq!((c["state"].as_str(), c["ambiguous"].as_bool()), (Some("ambiguous"), Some(true)), "{c}");
    let x = json(&e, &["list", "explain", "cookies", "1"]);
    assert_eq!(x["kind"], "explain");
    assert_eq!(x["ambiguous"], true);
    let bar = row(&x, "item", "Lindt Dark Chocolate Bar 70%");
    assert_eq!(bar["result"], "name");
    assert!(bar["identifier"].is_null() && bar["pinned"].is_null());
    // pinning one resolves it (no ASIN or ref: stored as profile + item id)
    run(&e, &["list", "pin", "cookies", "1", "Ghirardelli Dark Chocolate Chunks"]);
    let v = json(&e, &["list", "check", "cookies"]);
    let c = row(&v, "entry", "dark chocolate chips");
    assert_eq!(c["state"], "have", "{c}");
    assert_eq!(c["matches"][0]["tier"], "pinned");
    let shown = json(&e, &["list", "show", "cookies"]);
    assert!(rows(&shown)[0]["pins"][0].as_str().unwrap().starts_with("item:beach-house:"), "{shown}");
    // an item with an ASIN pins by ASIN
    run(&e, &["list", "add", "cookies", "walnuts"]);
    run(&e, &["list", "pin", "cookies", "walnuts", "000-001"]);
    let shown = json(&e, &["list", "show", "cookies"]);
    assert_eq!(rows(&shown)[1]["pins"][0], "asin:B0WALNUT01");
    run(&e, &["list", "unpin", "cookies", "walnuts"]);
    assert!(rows(&json(&e, &["list", "show", "cookies"]))[1]["pins"].as_array().unwrap().is_empty());
}

#[test]
fn wishlists_kits_and_match() {
    let e = Env::pinned();
    seed(&e);
    run(
        &e,
        &["add", "Sony WH-1000XM5 Wireless Noise Canceling Headphones", "--at", "Office", "--field", "ASIN=B09XS7JWHH"],
    );
    run(
        &e,
        &[
            "add",
            "REI Half Dome Tent",
            "--at",
            "Garage",
            "--status",
            "lent",
            "--notes",
            "https://www.rei.com/product/12345/half-dome-tent",
        ],
    );
    run(&e, &["list", "new", "Wants", "--type", "wishlist"]);
    run(
        &e,
        &[
            "list",
            "add",
            "wants",
            "headphones",
            "--url",
            "https://www.amazon.com/Sony-WH-1000XM5/dp/B09XS7JWHH/ref=sr_1_1?th=1",
        ],
    );
    run(&e, &["list", "add", "wants", "kayak"]);
    let v = json(&e, &["list", "check", "wants"]);
    assert_eq!(row(&v, "entry", "headphones")["state"], "fulfilled");
    assert_eq!(row(&v, "entry", "headphones")["matches"][0]["tier"], "url");
    assert_eq!(row(&v, "entry", "kayak")["state"], "open");
    assert_eq!(v["verdict"], "1 open, 1 fulfilled");
    let ls = json(&e, &["list", "ls"]);
    let w = row(&ls, "slug", "wants");
    assert_eq!((w["open"].as_u64(), w["fulfilled"].as_u64()), (Some(1), Some(1)));
    let shop = json(&e, &["list", "shop", "wants"]);
    assert_eq!(rows(&shop).len(), 1);
    assert_eq!(rows(&shop)[0]["detail"], "Wants: wishlist");

    // packing: a lent tent is not here to pack; a custom type is recipe-like (lent counts)
    run(&e, &["list", "new", "Camping", "--type", "packing"]);
    run(
        &e,
        &["list", "add", "camping", "tent", "--url", "https://rei.com/product/12345/half-dome-tent/?utm_source=mail"],
    );
    let out = e.out(&["list", "check", "camping", "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(row(&v, "entry", "tent")["status"].as_str().unwrap().contains("lent"), "{v}");
    run(&e, &["list", "new", "Camping 2", "--type", "car-trip"]);
    run(&e, &["list", "add", "camping-2", "tent"]);
    assert_eq!(e.out(&["list", "check", "camping-2"]).status.code(), Some(0));

    // ad-hoc ladder
    let m = json(&e, &["match", "walnut halves"]);
    assert_eq!(rows(&m)[0]["asset_id"], "000-001");
    let m = json(&e, &["match", "--asin", "b0walnut01"]);
    assert_eq!((rows(&m)[0]["tier"].as_str(), rows(&m).len()), (Some("identifier"), 1));
    let m = json(&e, &["match", "--upc", "01234567890"]);
    assert_eq!(rows(&m)[0]["item"], "Silicone baking mold", "11 digits: check digit computed");
    let m = json(&e, &["match", "pecans", "--at", "Fridge"]);
    assert!(rows(&m).is_empty(), "the pecans are in the pantry");
}

#[test]
fn checks_across_profiles() {
    let e = Env::pinned();
    seed(&e);
    e.run(&["-P", "lake-house", "profile", "init", "--title", "Lake House"]);
    e.run(&["-P", "lake-house", "add", "Walnuts", "--unit", "g", "--lot", "--lot-qty", "500", "--at", "Kitchen"]);
    e.run(&["list", "new", "Snack"]);
    e.run(&["list", "add", "snack", "walnuts", "--qty", "300", "--unit", "g"]);
    e.run(&["list", "add", "snack", "eggs"]);
    let out = e.out(&["list", "check", "snack", "--all-profiles", "--json"]);
    assert_eq!(out.status.code(), Some(0), "ready at one property");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["verdicts"]["beach-house"]["verdict"], "ready");
    assert_eq!(v["verdicts"]["lake-house"]["verdict"], "missing 1 of 2");
    let out = e.out(&["-P", "lake-house", "list", "check", "snack"]);
    assert_eq!(out.status.code(), Some(3));
}
