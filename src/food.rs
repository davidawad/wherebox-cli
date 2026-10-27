//! Is it food? Signals from labels and product names, shared by shelf-life
//! classification (only food gets a shelf-life type by name) and recipe
//! matching; and the food lexicon of recipe matching (`data/food.toml`):
//! food nouns, synonyms, densities and piece weights.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::matching::{self, Dim};
use crate::shelf_life::Types;

/// The built-in lexicon.
pub const DATA: &str = include_str!("../data/food.toml");

/// Labels that say an item is food (including Amazon import categories).
pub const FOOD_LABELS: &[&str] = &[
    "food",
    "grocery",
    "groceries",
    "grocery & gourmet food",
    "produce",
    "pantry",
    "snack",
    "snacks",
    "beverage",
    "beverages",
    "dairy",
    "meat",
    "seafood",
    "frozen",
    "bakery",
    "fruit",
    "vegetables",
    "spices",
    "baking",
];

/// Labels that say an item is not food, whatever its name.
pub const NON_FOOD_LABELS: &[&str] = &[
    "household",
    "cleaning",
    "toiletry",
    "toiletries",
    "personal care",
    "beauty",
    "cosmetics",
    "fragrance",
    "health",
    "health & household",
    "medicine",
    "supplement",
    "supplements",
    "vitamins",
    "pet",
    "pet supplies",
    "office",
    "electronics",
    "clothing",
    "apparel",
    "tools",
    "paper",
    "durable",
];

/// Name words (normalized, see [`crate::matching::name_tokens`]) of things
/// that are not food: fragrance, cosmetics, supplements, household goods.
pub const NON_FOOD_WORDS: &[&str] = &[
    "spray",
    "edp",
    "edt",
    "parfum",
    "perfume",
    "cologne",
    "fragrance",
    "toilette",
    "candle",
    "soap",
    "shampoo",
    "conditioner",
    "lotion",
    "moisturizer",
    "serum",
    "deodorant",
    "toothpaste",
    "mouthwash",
    "sunscreen",
    "spf",
    "lipstick",
    "balm",
    "mascara",
    "cosmetic",
    "makeup",
    "capsule",
    "softgel",
    "tablet",
    "caplet",
    "supplement",
    "vitamin",
    "multivitamin",
    "detergent",
    "cleaner",
    "bleach",
    "wipe",
    "diaper",
    "litter",
    "incense",
    "scented",
    "scent",
    "diffuser",
    "charger",
    "cable",
];

fn has_label(labels: &[String], set: &[&str]) -> bool {
    labels.iter().any(|l| set.contains(&l.trim().to_lowercase().as_str()))
}

/// Does a label say food?
pub fn food_label(labels: &[String]) -> bool {
    has_label(labels, FOOD_LABELS)
}

/// Does a label say not food (and no label says food)?
pub fn non_food_label(labels: &[String]) -> bool {
    has_label(labels, NON_FOOD_LABELS) && !food_label(labels)
}

/// Does the product name say it is not food (`Eau de Parfum Spray`,
/// `Ginger Root Capsules`)?
pub fn non_food_name(name: &str) -> bool {
    crate::matching::name_tokens(name).iter().any(|t| NON_FOOD_WORDS.contains(&t.as_str()))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(default)]
    nouns: Vec<String>,
    #[serde(default)]
    synonyms: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    density: BTreeMap<String, f64>,
    #[serde(default)]
    pieces: BTreeMap<String, BTreeMap<String, f64>>,
}

fn data() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| toml::from_str(DATA).unwrap_or_else(|e| panic!("data/food.toml: {e}")))
}

/// How much one unit of a food weighs: its density and piece weights.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub key: String,
    /// Grams per millilitre.
    pub density: Option<f64>,
    /// Grams per piece, by unit (`clove`, `head`, `ea`).
    pub pieces: BTreeMap<String, f64>,
}

impl Facts {
    /// Grams in one `unit` (`g`, `tbsp`, `clove`, `ea`), when known.
    pub fn grams_per(&self, unit: &str) -> Option<f64> {
        let u = matching::canonical_unit(unit);
        match matching::unit_info(&u) {
            Some((Dim::Mass, f)) => Some(f),
            Some((Dim::Volume, f)) => self.density.map(|d| f * d),
            _ => self.pieces.get(&u).or_else(|| self.pieces.get(&singular_unit(&u))).copied(),
        }
    }

    /// Grams in `qty` of an item counted in `unit` named `name`: a size in
    /// the name (`12 Ounce`), else a piece word in the name (`1 Head`), else
    /// one piece (`ea`), times a pack count in the name.
    pub fn item_grams(&self, qty: f64, unit: &str, name: &str) -> Option<f64> {
        let u = matching::canonical_unit(unit);
        if !matches!(matching::unit_info(&u), Some((Dim::Count, _)) | None) {
            return self.grams_per(&u).map(|g| qty * g);
        }
        if let Some((size, su)) = matching::size_from_name(name) {
            if let Some(g) = self.grams_per(&su) {
                return Some(qty * size * g);
            }
        }
        let words: Vec<String> = name
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(singular_unit)
            .collect();
        let piece = self
            .pieces
            .iter()
            .filter(|(k, _)| k.as_str() != "ea")
            .find(|(k, _)| words.contains(k))
            .map(|(_, g)| *g)
            .or_else(|| self.grams_per(&u))
            .or_else(|| self.pieces.get("ea").copied())?;
        Some(qty * piece * matching::pack_count_from_name(name).unwrap_or(1.0))
    }
}

fn singular_unit(u: &str) -> String {
    let u = u.trim().to_lowercase();
    u.strip_suffix("es").filter(|s| s.ends_with("ch") || s.ends_with("sh")).map_or_else(
        || u.strip_suffix('s').filter(|s| s.len() > 2).map_or_else(|| u.clone(), str::to_string),
        str::to_string,
    )
}

/// The food lexicon: built-in data plus the shelf-life types (a type's name
/// expands to its synonyms, and its words name food).
#[derive(Debug, Clone, Default)]
pub struct Lexicon {
    nouns: Vec<String>,
    synonyms: Vec<(Vec<String>, Vec<Vec<String>>)>,
    facts: Vec<(Vec<String>, Facts)>,
    types: Types,
}

impl Lexicon {
    pub fn new(types: &Types) -> Self {
        let d = data();
        let tok = |s: &str| matching::name_tokens(s);
        let mut synonyms: Vec<(Vec<String>, Vec<Vec<String>>)> =
            d.synonyms.iter().map(|(k, v)| (tok(k), v.iter().map(|s| tok(s)).collect())).collect();
        for t in types.types.values() {
            let name = tok(&t.name.replace(['-', '_', '.'], " "));
            if !t.synonyms.is_empty() && !synonyms.iter().any(|(k, _)| *k == name) {
                synonyms.push((name, t.synonyms.iter().map(|s| tok(s)).collect()));
            }
        }
        let mut keys: Vec<&String> = d.density.keys().chain(d.pieces.keys()).collect();
        keys.sort();
        keys.dedup();
        let facts = keys
            .into_iter()
            .map(|k| {
                let f = Facts {
                    key: k.clone(),
                    density: d.density.get(k).copied(),
                    pieces: d.pieces.get(k).cloned().unwrap_or_default(),
                };
                (tok(k), f)
            })
            .collect();
        let nouns = d.nouns.iter().flat_map(|n| tok(n)).collect();
        Self { nouns, synonyms, facts, types: types.clone() }
    }

    /// Phrases an entry also goes by (`berries`: `blueberries`...).
    pub fn synonyms(&self, entry: &[String]) -> Vec<&Vec<String>> {
        self.synonyms.iter().filter(|(k, _)| k.as_slice() == entry).flat_map(|(_, v)| v.iter()).collect()
    }

    /// The density and piece weights of the food a name names: the best
    /// key whose head noun is the name's head noun.
    pub fn facts(&self, name: &str) -> Option<&Facts> {
        let segs = matching::segments(name);
        self.facts
            .iter()
            .map(|(k, f)| (matching::head_match(k, &segs).score, k.len(), f))
            .filter(|(s, _, _)| *s >= crate::shelf_life::CLASSIFY_THRESHOLD)
            .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
            .map(|(_, _, f)| f)
    }

    /// Does the name's head noun name food (a lexicon noun or key, or a
    /// shelf-life type)?
    pub fn names_food(&self, name: &str) -> bool {
        if non_food_name(name) {
            return false;
        }
        let segs = matching::segments(name);
        segs.iter().filter_map(|s| s.head.as_ref()).any(|h| self.nouns.contains(h))
            || self.facts(name).is_some()
            || self.types.by_name(name).is_some()
    }
}

/// What [`is_food`] looks at.
pub struct ItemSignals<'a> {
    pub name: &'a str,
    pub preset_category: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub labels: &'a [String],
    pub shelf_life: bool,
    pub food_place: bool,
}

/// Is an item food, for recipe gating? Never when its preset is a non-food
/// category, a label says not food, or its name does (fragrance, cosmetics,
/// supplements, household goods); else when its preset is food, a label or
/// its `Kind` field says food, it has a shelf-life type, it is kept in a
/// fridge, freezer or pantry, or its name's head noun names food.
pub fn is_food(s: &ItemSignals, lex: &Lexicon) -> bool {
    let kind = s.kind.map(|k| k.trim().to_ascii_lowercase());
    if kind.as_deref() == Some("durable")
        || s.preset_category.is_some_and(|c| c != "food")
        || non_food_label(s.labels)
        || non_food_name(s.name)
    {
        return false;
    }
    s.preset_category == Some("food")
        || kind.as_deref() == Some("food")
        || food_label(s.labels)
        || s.shelf_life
        || s.food_place
        || lex.names_food(s.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_labels() {
        assert!(non_food_name("Lost Cherry Eau de Parfum Spray, 1.7 oz"));
        assert!(non_food_name("Ginger Root Capsules 550mg, 100 Count"));
        assert!(!non_food_name("Organic Ginger Root, 1 lb"));
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(non_food_label(&l(&["household", "consumable"])));
        assert!(!non_food_label(&l(&["household", "food"])));
        assert!(food_label(&l(&["Grocery"])));
    }

    #[test]
    fn lexicon_loads_and_converts() {
        let lex = Lexicon::new(&Types::builtin());
        let tok = |s: &str| matching::name_tokens(s);
        let syn = lex.synonyms(&tok("berries"));
        assert!(syn.contains(&&tok("blueberries")), "{syn:?}");
        assert!(lex.synonyms(&tok("nut milk")).contains(&&tok("macadamia milk")));
        // a shelf-life type's name expands too
        assert!(lex.synonyms(&tok("citrus")).contains(&&tok("lime")));
        let hemp = lex.facts("Hemp Seed Organic, 12 Ounce").unwrap();
        assert_eq!(hemp.key, "hemp seeds");
        let tbsp = hemp.grams_per("tbsp").unwrap();
        assert!((tbsp - 10.05).abs() < 0.1, "{tbsp}");
        let bag = hemp.item_grams(1.0, "ea", "Hemp Seed Organic, 12 Ounce").unwrap();
        assert!((bag - 340.2).abs() < 0.5, "{bag}");
        let garlic = lex.facts("garlic").unwrap();
        assert_eq!(garlic.grams_per("cloves"), Some(3.0));
        assert_eq!(garlic.item_grams(1.0, "ea", "Fresh Garlic, 1 Head"), Some(30.0));
        assert!((garlic.item_grams(1.0, "lb", "Garlic, 3 lb Bag").unwrap() - 453.6).abs() < 0.1);
        assert_eq!(lex.facts("Chili Garlic Sauce"), None, "garlic is not the head noun");
        assert!(lex.names_food("Walnuts") && lex.names_food("Organic Hemp Seeds"));
        assert!(!lex.names_food("Lost Cherry Eau de Parfum Spray") && !lex.names_food("Cordless Drill"));
        // every value is positive
        let d = data();
        assert!(d.density.values().chain(d.pieces.values().flat_map(|p| p.values())).all(|v| *v > 0.0));
    }
}
