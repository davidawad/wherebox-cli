//! Consumables versus durables, and which purchases are the same product.
//!
//! An item is a consumable when its `Kind` custom field says so
//! (`HB.field.Kind`: `consumable` or `food`; `durable` says it is not), else
//! when its preset is a food or consumable kind, else when it carries a
//! `consumable` or `food` label. Repeat purchases of one consumable are one
//! item with a lot per purchase; purchases are the same product when they
//! share an ASIN (`HB.field.ASIN`), else a product ref, at the same property
//! (top-level location).

use std::collections::BTreeMap;

use crate::presets::Catalog;

/// Custom field naming the kind: `consumable`, `food` or `durable`.
pub const KIND_FIELD: &str = "Kind";
/// Custom field with an Amazon ASIN.
pub const ASIN_FIELD: &str = "ASIN";
/// Labels that mark an item as a consumable.
pub const LABELS: &[&str] = &["consumable", "consumables", "food"];

/// A custom field by name, ignoring case.
pub fn field<'a>(fields: Option<&'a BTreeMap<String, String>>, name: &str) -> Option<&'a str> {
    fields?.iter().find(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Is an item with this `Kind` field, preset and labels a consumable?
pub fn is_consumable(kind: Option<&str>, preset: Option<&str>, labels: &[String], catalog: &Catalog) -> bool {
    match kind.map(|k| k.trim().to_ascii_lowercase()).as_deref() {
        Some("consumable" | "consumables" | "food") => return true,
        Some("durable") => return false,
        _ => {}
    }
    preset.and_then(|p| catalog.get(p)).is_some_and(|p| p.is_consumable())
        || labels.iter().any(|l| LABELS.contains(&l.trim().to_ascii_lowercase().as_str()))
}

/// The product a purchase is of: its ASIN, else its product ref (trimmed,
/// upper case so `b0abc` and `B0ABC` match).
pub fn product_key(asin: Option<&str>, product_ref: Option<&str>) -> Option<String> {
    asin.or(product_ref).map(|k| k.trim().to_ascii_uppercase()).filter(|k| !k.is_empty())
}

/// Group key of a consumable purchase: property (lower case, may be empty) and product.
pub fn group_key(property: Option<&str>, product: &str) -> String {
    format!("{}\u{1f}{product}", property.unwrap_or("").trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_field_wins_then_preset_then_labels() {
        let c = Catalog::builtin();
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_consumable(Some("Consumable"), None, &[], &c));
        assert!(!is_consumable(Some("durable"), None, &l(&["food"]), &c));
        assert!(is_consumable(None, None, &l(&["grocery", "Food"]), &c));
        assert!(is_consumable(None, Some("meat.raw"), &[], &c));
        assert!(!is_consumable(None, Some("tool"), &l(&["amazon"]), &c));
        assert_eq!(product_key(Some(" b0abc "), Some("x")), Some("B0ABC".into()));
        assert_eq!(product_key(None, Some("")), None);
        let f = BTreeMap::from([("asin".to_string(), "B0".to_string())]);
        assert_eq!(field(Some(&f), "ASIN"), Some("B0"));
    }
}
