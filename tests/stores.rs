//! Store sourcing with synthetic stores of two chains: a profile shopping at
//! chain A and one shopping at chain B get different products for the same
//! entry, `list shop` walks the store's layout, sections are guessed from
//! category text, URLs and names, and stores travel through JSON export.

mod support;

use serde_json::Value;
use support::{row, rows, Env};

/// Run with `-P profile` and `--json`, whatever the exit code.
fn json_any(e: &Env, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let out = e.out(&a);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|err| panic!("{a:?}: {err}\n{}", String::from_utf8_lossy(&out.stderr)))
}

/// Two properties: the Beach House shops at Sunfield Market (chain A), the
/// Lake House at Penny Grocer (chain B). Both databases know both chains.
fn stocked() -> Env {
    let e = Env::pinned();
    for (p, title) in [("beach-house", "Beach House"), ("lake-house", "Lake House")] {
        let run = |args: &[&str]| e.run(&[&["-P", p], args].concat());
        run(&["profile", "init", "--title", title]);
        run(&[
            "store",
            "add",
            "sunfield-north",
            "--chain",
            "Sunfield Market",
            "--name",
            "Sunfield North",
            "--address",
            "1 Example Ave",
        ]);
        run(&["store", "add", "sunfield-south", "--chain", "Sunfield Market", "--name", "Sunfield South"]);
        run(&["store", "add", "penny-main", "--chain", "Penny Grocer", "--name", "Penny Main St"]);
        run(&[
            "store",
            "layout",
            "--chain",
            "Sunfield Market",
            "produce=1, bulk=2, bakery, pantry=3-7, dairy=11, frozen=12-13, checkout",
        ]);
        // chain A: a chain-wide row and one store's own (bulk bins) row
        run(&[
            "source",
            "add",
            "walnuts",
            "--chain",
            "Sunfield Market",
            "--name",
            "Walnut Halves & Pieces",
            "--size",
            "16 oz",
            "--aisle",
            "5",
        ]);
        run(&[
            "source",
            "add",
            "walnuts",
            "--store",
            "sunfield-north",
            "--name",
            "Raw Walnuts (bulk bins)",
            "--section",
            "bulk",
            "--aisle",
            "bins 2",
            "--note",
            "top row by the almonds",
            "--price",
            "8.99",
        ]);
        // chain B
        run(&[
            "source",
            "add",
            "walnuts",
            "--chain",
            "Penny Grocer",
            "--name",
            "Southern Grove Chopped Walnuts",
            "--size",
            "8 oz",
            "--section",
            "baking",
            "--aisle",
            "9",
        ]);
        run(&["source", "add", "limes", "--chain", "Sunfield Market", "--name", "Organic Limes", "--plu", "94048"]);
        run(&[
            "source",
            "add",
            "milk",
            "--chain",
            "Sunfield Market",
            "--name",
            "Whole Milk, Half Gallon",
            "--category",
            "Dairy & Eggs > Milk",
        ]);
        run(&["source", "add", "oats", "--chain", "Sunfield Market", "--name", "Rolled Oats", "--aisle", "3"]);
        run(&["source", "add", "honey", "--chain", "Penny Grocer", "--name", "Clover Honey"]);
    }
    e.run(&["-P", "beach-house", "store", "use", "sunfield-north", "penny-main"]);
    e.run(&["-P", "lake-house", "store", "use", "Penny Main St"]);
    // the recipe needs five things; walnuts are short at the Beach House
    e.run(&[
        "-P",
        "beach-house",
        "add",
        "Walnut Halves 16 oz",
        "--at",
        "Kitchen / Pantry",
        "--unit",
        "g",
        "--lot",
        "--lot-qty",
        "100",
    ]);
    e.run(&["list", "new", "Nutty pudding", "--type", "recipe"]);
    for (entry, qty, unit) in [
        ("walnuts", "300", "g"),
        ("limes", "2", "ea"),
        ("milk", "1", "l"),
        ("oats", "100", "g"),
        ("cardamom", "1", "tsp"),
    ] {
        e.run(&["list", "add", "nutty-pudding", entry, "--qty", qty, "--unit", unit]);
    }
    e
}

#[test]
fn two_chains_give_two_products_for_one_entry() {
    let e = stocked();
    let cfg = std::fs::read_to_string(e.path("config").join("wherebox-cli").join("config.toml")).unwrap();
    assert!(
        cfg.contains("[profiles.beach-house]") && cfg.contains("stores = [\"sunfield-north\", \"penny-main\"]"),
        "{cfg}"
    );

    let beach = json_any(&e, &["-P", "beach-house", "list", "check", "nutty-pudding"]);
    let w = row(&beach, "entry", "walnuts");
    assert_eq!(w["state"], "short");
    assert_eq!((w["store"].as_str(), w["source_tier"].as_str()), (Some("sunfield-north"), Some("store")));
    assert_eq!(w["product"], "Raw Walnuts (bulk bins)", "the store's own row beats the chain row: {w}");
    assert_eq!((w["section"].as_str(), w["aisle"].as_str()), (Some("bulk"), Some("bins 2")));
    assert_eq!(w["source_note"], "top row by the almonds");
    assert_eq!(w["price"], 8.99);
    assert_eq!(w["buy"], "Sunfield North: Raw Walnuts (bulk bins) (bulk, aisle bins 2; top row by the almonds)");
    let limes = row(&beach, "entry", "limes");
    assert_eq!((limes["source_tier"].as_str(), limes["product"].as_str()), (Some("chain"), Some("Organic Limes")));
    assert_eq!(limes["section"], "produce", "guessed from the name");
    assert_eq!(limes["aisle"], "1", "the layout's aisle for the section");
    assert_eq!(row(&beach, "entry", "cardamom")["buy"], "no source at Sunfield North");
    assert!(row(&beach, "entry", "cardamom")["product"].is_null());

    // the same entry at the Lake House, which shops at chain B
    let lake = json_any(&e, &["-P", "lake-house", "list", "check", "nutty-pudding"]);
    let w = row(&lake, "entry", "walnuts");
    assert_eq!(w["state"], "missing");
    assert_eq!((w["store"].as_str(), w["source_tier"].as_str()), (Some("penny-main"), Some("chain")));
    assert_eq!(w["product"], "Southern Grove Chopped Walnuts");
    assert_eq!(w["packs"], "2 x 8 oz", "300 g of 8 oz (227 g) bags");
    // no Penny Grocer row for limes: another store's, said so
    let limes = row(&lake, "entry", "limes");
    assert_eq!((limes["source_tier"].as_str(), limes["product"].as_str()), (Some("other"), Some("Organic Limes")));
    assert!(
        limes["buy"].as_str().unwrap().starts_with("Sunfield Market (none at Penny Main St): Organic Limes"),
        "{limes}"
    );

    // --store overrides the profile's default
    let v = json_any(&e, &["-P", "beach-house", "list", "check", "nutty-pudding", "--store", "penny-main"]);
    assert_eq!(row(&v, "entry", "walnuts")["product"], "Southern Grove Chopped Walnuts");
    // another store of chain A: the chain row, not sunfield-north's own
    let v = json_any(&e, &["-P", "beach-house", "list", "check", "nutty-pudding", "--store", "sunfield-south"]);
    let w = row(&v, "entry", "walnuts");
    assert_eq!((w["product"].as_str(), w["source_tier"].as_str()), (Some("Walnut Halves & Pieces"), Some("chain")));
    assert_eq!(w["section"], "pantry");

    // every profile at its own store
    let all = json_any(&e, &["list", "check", "nutty-pudding", "--all-profiles"]);
    let by =
        |p: &str| rows(&all).iter().find(|r| r["profile"] == p && r["entry"] == "walnuts").unwrap()["product"].clone();
    assert_eq!(by("beach-house"), "Raw Walnuts (bulk bins)");
    assert_eq!(by("lake-house"), "Southern Grove Chopped Walnuts");
    assert_eq!(all["verdicts"]["lake-house"]["store"], "penny-main");

    // the table says where to buy
    let out = e.out(&["-P", "beach-house", "list", "check", "nutty-pudding"]);
    assert_eq!(out.status.code(), Some(3));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("BUY") && text.contains("no source at Sunfield North"), "{text}");
}

#[test]
fn list_shop_walks_the_layout() {
    let e = stocked();
    let v = json_any(&e, &["-P", "beach-house", "list", "shop", "nutty-pudding"]);
    assert_eq!(v["kind"], "shopping");
    assert_eq!(v["route"][0]["store"], "sunfield-north");
    assert_eq!(v["route"][0]["layout_source"], "chain");
    let walk: Vec<(u64, String, String)> = rows(&v)
        .iter()
        .map(|r| {
            (
                r["step"].as_u64().unwrap(),
                r["group"].as_str().unwrap().to_string(),
                r["item"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let want: Vec<(u64, String, String)> = [
        (1, "Produce", "limes"),
        (2, "Bulk", "walnuts"),
        (3, "Pantry", "oats"),
        (4, "Dairy", "milk"),
        (5, "Ask / unsorted", "cardamom"),
    ]
    .iter()
    .map(|(n, g, i)| (*n, g.to_string(), i.to_string()))
    .collect();
    assert_eq!(walk, want, "{v}");
    let w = row(&v, "item", "walnuts");
    assert_eq!(w["qty"], 200.0, "short: buy the difference");
    assert_eq!(w["vendor"], "Sunfield North");
    assert_eq!(row(&v, "item", "milk")["section"], "dairy", "guessed from the category");

    // the text walk: headings in layout order, numbered steps
    let text = e.run(&["-P", "beach-house", "list", "shop", "nutty-pudding"]);
    assert!(text.starts_with("Nutty pudding at Sunfield North (sunfield-north), layout: chain\n"), "{text}");
    let pos = |s: &str| text.find(s).unwrap_or_else(|| panic!("no {s:?} in\n{text}"));
    assert!(pos("Produce\n") < pos("Bulk\n") && pos("Bulk\n") < pos("Pantry\n") && pos("Pantry\n") < pos("Dairy\n"));
    assert!(pos("Dairy\n") < pos("Ask / unsorted\n"));
    assert!(text.contains("   2. 200 g  Raw Walnuts (bulk bins)  aisle bins 2  top row by the almonds\n"), "{text}");
    assert!(text.contains("   4. 1 l    Whole Milk, Half Gallon  aisle 11\n"), "{text}");
    assert!(text.contains("   5. 1 tsp  cardamom") && text.contains("no source at Sunfield North"), "{text}");

    // a store's own layout beats the chain's: dairy first, then aisles in number order
    e.run(&[
        "-P",
        "beach-house",
        "source",
        "add",
        "honey",
        "--store",
        "sunfield-north",
        "--name",
        "Wildflower Honey",
        "--aisle",
        "7",
    ]);
    e.run(&["list", "add", "nutty-pudding", "honey"]);
    e.run(&["-P", "beach-house", "store", "layout", "sunfield-north", "dairy, pantry, produce, bulk, checkout"]);
    let v = json_any(&e, &["-P", "beach-house", "list", "shop", "nutty-pudding"]);
    assert_eq!(v["route"][0]["layout_source"], "store");
    let items: Vec<&str> = rows(&v).iter().map(|r| r["item"].as_str().unwrap()).collect();
    assert_eq!(items, ["milk", "oats", "honey", "limes", "walnuts", "cardamom"], "pantry by aisle: 3, then 7");

    // no store: the generic default layout
    let v = json_any(&e, &["-P", "lake-house", "list", "shop", "nutty-pudding", "--store", "penny-main"]);
    assert_eq!(v["route"][0]["layout_source"], "default");
    let groups: Vec<&str> = rows(&v).iter().map(|r| r["group"].as_str().unwrap()).collect();
    assert_eq!(groups.first(), Some(&"Produce"));
    assert_eq!(groups.last(), Some(&"Ask / unsorted"));
}

#[test]
fn sections_are_guessed_and_listed() {
    let e = Env::pinned();
    e.run(&["store", "add", "corner", "--chain", "Corner Grocer"]);
    let add = |args: &[&str]| e.json(&[&["source", "add"], args].concat());
    let v = add(&[
        "peanut butter",
        "--store",
        "corner",
        "--name",
        "Creamy Peanut Butter",
        "--category",
        "Pantry > Nut Butters",
    ]);
    assert_eq!(
        (v["data"]["section"].as_str(), v["data"]["section_basis"].as_str()),
        (Some("pantry"), Some("category"))
    );
    let v = add(&[
        "beans",
        "--store",
        "corner",
        "--url",
        "https://shop.example.com/pantry/canned-goods/black-beans?utm_source=x",
    ]);
    assert_eq!((v["data"]["section"].as_str(), v["data"]["section_basis"].as_str()), (Some("canned"), Some("url")));
    let v = add(&["blueberries", "--store", "corner", "--name", "Frozen Wild Blueberries"]);
    assert_eq!(v["data"]["section"], "frozen");
    let v = add(&["broth", "--store", "corner", "--name", "Chicken Broth, 32 oz"]);
    assert_eq!(v["data"]["section"], "pantry", "the head noun, not the flavour");
    let v = add(&["tamarind", "--store", "corner", "--section", "Hot Bar"]);
    assert_eq!((v["data"]["section"].as_str(), v["data"]["section_guessed"].as_bool()), (Some("hot-bar"), Some(false)));
    let v = add(&["zorp", "--store", "corner"]);
    assert!(v["data"]["section"].is_null(), "no guess: {v}");

    // products of a food, as list entries link them
    e.run(&["source", "add", "walnut halves", "--store", "corner", "--name", "Walnut Halves"]);
    e.run(&["source", "add", "walnut oil", "--store", "corner", "--name", "Walnut Oil"]);
    let v = e.json(&["source", "ls", "walnuts"]);
    assert_eq!(rows(&v).iter().map(|r| r["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Walnut Halves"]);
    assert_eq!(rows(&v)[0]["link"], "head noun");

    // removing a store that has products needs --force
    let err = e.fail(&["store", "rm", "corner"], 4);
    assert!(err.contains("--force"), "{err}");
    e.run(&["store", "rm", "corner", "--force"]);
    assert!(rows(&e.json(&["source", "ls"])).is_empty());
}

#[test]
fn lots_name_the_store_and_stores_round_trip_through_json() {
    let e = stocked();
    let p = ["-P", "beach-house"];
    let run = |args: &[&str]| e.run(&[&p[..], args].concat());
    run(&["lot", "add", "Walnut Halves 16 oz", "200", "--vendor", "Sunfield North"]);
    run(&["lot", "add", "Walnut Halves 16 oz", "100", "--vendor", "Farmers market"]);
    let lots = e.json(&[&p[..], &["lot", "ls"]].concat());
    let vendors: Vec<Value> = rows(&lots).iter().map(|l| l["vendor"].clone()).collect();
    assert!(vendors.contains(&Value::from("sunfield-north")), "a store name is stored as its id: {lots}");
    assert!(vendors.contains(&Value::from("Farmers market")));

    let export = e.run(&[&p[..], &["export", "-f", "json"]].concat());
    let v: Value = serde_json::from_str(&export).unwrap();
    assert_eq!(v["stores"].as_array().unwrap().len(), 3);
    assert_eq!(v["chain_layouts"][0]["chain"], "sunfield market");
    assert_eq!(v["store_products"].as_array().unwrap().len(), 7);
    let file = e.path("export.json");
    std::fs::write(&file, &export).unwrap();

    // into an empty database, twice: the same stores and products, no duplicates
    let before = e.json(&[&p[..], &["source", "ls"]].concat());
    for _ in 0..2 {
        e.run(&["import", file.to_str().unwrap()]);
    }
    let after = e.json(&["source", "ls"]);
    let names = |v: &Value| {
        let mut n: Vec<String> =
            rows(v).iter().map(|r| format!("{} @ {} {}", r["name"], r["at"], r["section"])).collect();
        n.sort();
        n
    };
    assert_eq!(names(&after), names(&before));
    let stores = e.json(&["store", "ls"]);
    assert_eq!(rows(&stores).len(), 3);
    assert_eq!(row(&stores, "id", "sunfield-north")["layout_source"], "chain");
    let lots = e.json(&["lot", "ls"]);
    assert!(rows(&lots).iter().any(|l| l["vendor"] == "sunfield-north"), "{lots}");
    let w = e.json(&["source", "ls", "walnuts", "--store", "sunfield-north"]);
    assert_eq!(row(&w, "name", "Raw Walnuts (bulk bins)")["price"], 8.99);
}
