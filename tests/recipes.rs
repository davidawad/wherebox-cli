//! Recipe matching quality with synthetic Amazon-style items: food lists
//! only match food, the entry's head noun must name the product (not a
//! flavour or modifier), synonyms widen food nouns, and volume, mass and
//! pieces convert through densities and piece weights.

mod support;

use serde_json::Value;
use support::{row, rows, Env};

/// A kitchen and a bathroom stocked from (synthetic) Amazon orders; today
/// is 2026-01-01.
fn stocked() -> Env {
    let e = Env::pinned();
    let add = |args: &[&str]| e.run(&[&["add"], args].concat());
    add(&[
        "Lost Cherry Eau de Parfum Spray, 1.7 oz",
        "--at",
        "Home / Bathroom",
        "--label",
        "amazon",
        "--field",
        "ASIN=B0PERFUME1",
    ]);
    add(&["Berry Trail Mix, 26 oz", "--at", "Home / Kitchen / Pantry", "--label", "amazon,grocery", "--lot"]);
    add(&["Ginger Root Capsules 550 mg, 100 Count", "--at", "Home / Kitchen / Pantry", "--label", "amazon", "--lot"]);
    add(&["Chili Garlic Sauce, 8 oz", "--at", "Home / Kitchen / Pantry", "--label", "amazon", "--lot"]);
    add(&["Hemp Seed Organic, 12 Ounce", "--at", "Home / Kitchen / Pantry", "--label", "amazon", "--lot"]);
    e
}

/// `list check --json`, whatever its exit code.
fn check(e: &Env, list: &str) -> Value {
    serde_json::from_slice(&e.out(&["list", "check", list, "--json"]).stdout).unwrap()
}

fn matched(v: &Value, entry: &str) -> Vec<String> {
    row(v, "entry", entry)["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["item"].as_str().unwrap().to_string())
        .collect()
}

fn recipe(e: &Env, lines: &str) {
    e.run(&["list", "new", "Smoothie bowl", "--type", "recipe"]);
    let f = e.path("smoothie.txt");
    std::fs::write(&f, lines).unwrap();
    e.run(&["list", "import", "smoothie-bowl", f.to_str().unwrap()]);
}

#[test]
fn observed_false_matches_are_gone() {
    let e = stocked();
    recipe(&e, "10 cherries\n1 cup berries\n3 g ginger root\n1 clove garlic\n1 tbsp hemp seeds\n");
    let v = check(&e, "smoothie-bowl");
    // a perfume is never a recipe candidate (and "cherry" only flavours it)
    assert!(matched(&v, "cherries").is_empty(), "{v}");
    assert_eq!(row(&v, "entry", "cherries")["state"], "missing");
    // berries flavour a trail mix: not berries
    assert!(matched(&v, "berries").is_empty(), "{v}");
    assert_eq!(row(&v, "entry", "berries")["state"], "missing", "not unit-mismatch");
    // supplement capsules are not ginger root
    assert!(matched(&v, "ginger root").is_empty(), "{v}");
    // a chili garlic sauce is not garlic
    assert!(matched(&v, "garlic").is_empty(), "{v}");
    // hemp seeds by the tablespoon count against a 12 oz bag: 340 g / 10 g
    let hemp = row(&v, "entry", "hemp seeds");
    assert_eq!(hemp["state"], "have", "{hemp}");
    assert_eq!(hemp["matches"][0]["basis"], "density");
    assert!((hemp["have"].as_f64().unwrap() - 33.85).abs() < 0.1, "{hemp}");

    // why: explain says so
    let x = e.json(&["list", "explain", "smoothie-bowl", "cherries"]);
    let perfume = row(&x, "item", "Lost Cherry Eau de Parfum Spray, 1.7 oz");
    assert_eq!(perfume["food"], false);
    assert!(perfume["name_score"].as_f64().unwrap() <= 0.5, "{perfume}");
    let x = e.json(&["list", "explain", "smoothie-bowl", "garlic"]);
    let sauce = row(&x, "item", "Chili Garlic Sauce, 8 oz");
    assert_eq!((sauce["food"].as_bool(), sauce["result"].as_str()), (Some(true), Some("below threshold")));
}

#[test]
fn the_real_thing_matches() {
    let e = stocked();
    let add = |args: &[&str]| e.run(&[&["add"], args].concat());
    add(&["Frozen Organic Wild Blueberries, 3 lb", "--at", "Home / Kitchen / Freezer", "--label", "amazon"]);
    add(&["Fresh Garlic, 1 Head", "--at", "Home / Kitchen / Pantry", "--label", "amazon"]);
    add(&["Garlic, 3 lb Bag", "--at", "Home / Kitchen / Pantry", "--unit", "lb", "--lot", "--lot-qty", "3"]);
    add(&["Bing Cherries, 2 lb", "--at", "Home / Kitchen / Fridge"]);
    recipe(&e, "10 cherries\n300 g berries\n1 clove garlic\n1 tbsp hemp seeds\n2 limes\n");
    let v = check(&e, "smoothie-bowl");
    // frozen blueberries satisfy "berries" (a synonym), the trail mix doesn't
    assert_eq!(matched(&v, "berries"), ["Frozen Organic Wild Blueberries, 3 lb"]);
    let berries = row(&v, "entry", "berries");
    assert_eq!(berries["state"], "have");
    assert!(berries["matches"][0]["via"].as_str().unwrap().contains("as blueberry"), "{berries}");
    // a garlic head is ~10 cloves; a 3 lb bag is many more; the sauce is out
    let garlic = row(&v, "entry", "garlic");
    assert_eq!(garlic["state"], "have", "{garlic}");
    let names = matched(&v, "garlic");
    assert!(names.contains(&"Fresh Garlic, 1 Head".to_string()) && names.contains(&"Garlic, 3 lb Bag".to_string()));
    assert!(!names.contains(&"Chili Garlic Sauce, 8 oz".to_string()));
    let head = garlic["matches"].as_array().unwrap().iter().find(|m| m["item"] == "Fresh Garlic, 1 Head").unwrap();
    assert_eq!((head["qty"].as_f64(), head["basis"].as_str()), (Some(10.0), Some("piece weight")));
    // ten cherries out of a 2 lb bag (8 g each)
    let cherries = row(&v, "entry", "cherries");
    assert_eq!(
        (cherries["state"].as_str(), matched(&v, "cherries")),
        (Some("have"), vec!["Bing Cherries, 2 lb".to_string()])
    );
    // no limes: missing; the hemp bag still counts
    assert_eq!(row(&v, "entry", "limes")["state"], "missing");
    assert_eq!(row(&v, "entry", "hemp seeds")["state"], "have");
}

#[test]
fn identifiers_and_pins_still_reach_non_food() {
    let e = stocked();
    e.run(&["list", "new", "Gift basket", "--type", "recipe"]);
    e.run(&["list", "add", "gift-basket", "cherry perfume", "--asin", "B0PERFUME1"]);
    e.run(&["list", "add", "gift-basket", "ginger pills", "--label", "amazon"]);
    let v = check(&e, "gift-basket");
    assert_eq!(matched(&v, "cherry perfume"), ["Lost Cherry Eau de Parfum Spray, 1.7 oz"]);
    assert_eq!(row(&v, "entry", "cherry perfume")["matches"][0]["tier"], "identifier");
    // a label is an attribute: non-food items stay out of a food list
    assert!(!matched(&v, "ginger pills").iter().any(|n| n.contains("Capsules")), "{v}");
    e.run(&["list", "pin", "gift-basket", "ginger pills", "Ginger Root Capsules 550 mg, 100 Count"]);
    let v = check(&e, "gift-basket");
    assert!(matched(&v, "ginger pills").iter().any(|n| n.contains("Capsules")), "{v}");
    // the gating is a list property: food = false lets attributes through
    e.run(&["list", "new", "Amazon haul", "--type", "recipe", "--food", "false"]);
    let shown = e.json(&["list", "ls"]);
    assert_eq!(row(&shown, "slug", "amazon-haul")["food"], false);
    assert_eq!(row(&shown, "slug", "gift-basket")["food"], true);
    e.run(&["list", "add", "amazon-haul", "stuff", "--label", "amazon"]);
    let v = check(&e, "amazon-haul");
    assert_eq!(matched(&v, "stuff").len(), 5, "{v}");
    // kits are not food lists
    e.run(&["list", "new", "Bathroom kit", "--type", "kit"]);
    e.run(&["list", "add", "bathroom-kit", "perfume", "--label", "amazon", "--at", "Bathroom"]);
    assert_eq!(rows(&check(&e, "bathroom-kit"))[0]["state"], "have");
}
