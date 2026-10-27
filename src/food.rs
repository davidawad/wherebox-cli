//! Is it food? Signals from labels and product names, shared by shelf-life
//! classification (only food gets a shelf-life type by name) and recipe
//! matching.

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
}
