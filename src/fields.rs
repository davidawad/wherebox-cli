//! The item field registry: every scalar item column, its native name (JSON,
//! CSV, `--field`-style keys), its Homebox CSV column, and how its text form
//! is parsed and printed.
//!
//! Commands, import and export all go through [`get`] / [`set`], so a value
//! has exactly one canonical text form everywhere: money `1299.99`, dates
//! `YYYY-MM-DD`, booleans `true`/`false`, quantities without a trailing `.0`.

use crate::error::{AppError, Result};
use crate::store::{check_choice, Item, STATUSES};
use crate::util::{fmt_money, fmt_qty, parse_bool, parse_day, parse_money, parse_qty, Tz};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// USD cents, written as a decimal amount.
    Money,
    Date,
    Bool,
    Int,
    /// A non-negative decimal (par levels, reorder quantities).
    Real,
    Status,
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// Native name (item JSON key, native CSV column, mapping-file field).
    pub key: &'static str,
    /// Homebox CSV column.
    pub homebox: &'static str,
    pub kind: Kind,
}

const fn f(key: &'static str, homebox: &'static str, kind: Kind) -> Field {
    Field { key, homebox, kind }
}

/// Scalar item fields in Homebox column order: the documented standard
/// columns first, then the `HB.field.*` custom fields wherebox adds.
pub const FIELDS: &[Field] = &[
    f("quantity", "HB.quantity", Kind::Int),
    f("name", "HB.name", Kind::Text),
    f("asset_id", "HB.asset_id", Kind::Text),
    f("description", "HB.description", Kind::Text),
    f("insured", "HB.insured", Kind::Bool),
    f("serial_number", "HB.serial_number", Kind::Text),
    f("model_number", "HB.model_number", Kind::Text),
    f("manufacturer", "HB.manufacturer", Kind::Text),
    f("notes", "HB.notes", Kind::Text),
    f("purchase_from", "HB.purchase_from", Kind::Text),
    f("purchase_price", "HB.purchase_price", Kind::Money),
    f("purchase_time", "HB.purchase_time", Kind::Date),
    f("lifetime_warranty", "HB.lifetime_warranty", Kind::Bool),
    f("warranty_expires", "HB.warranty_expires", Kind::Date),
    f("warranty_details", "HB.warranty_details", Kind::Text),
    f("sold_to", "HB.sold_to", Kind::Text),
    f("sold_time", "HB.sold_time", Kind::Date),
    f("sold_price", "HB.sold_price", Kind::Money),
    f("sold_notes", "HB.sold_notes", Kind::Text),
    f("owner_entity", "HB.field.Owner Entity", Kind::Text),
    f("replacement_value", "HB.field.Replacement Value", Kind::Money),
    f("valued_at", "HB.field.Valued At", Kind::Date),
    f("status", "HB.field.Status", Kind::Status),
    f("kind_preset", "HB.field.Preset", Kind::Text),
    f("unit", "HB.field.Unit", Kind::Text),
    f("par_level", "HB.field.Par Level", Kind::Real),
    f("reorder_qty", "HB.field.Reorder Qty", Kind::Real),
    f("preferred_vendor", "HB.field.Vendor", Kind::Text),
    f("product_ref", "HB.field.Product Ref", Kind::Text),
];

/// Number of documented Homebox standard columns at the start of [`FIELDS`].
pub const HOMEBOX_STANDARD: usize = 19;

pub fn by_key(key: &str) -> Option<&'static Field> {
    let k = key.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    FIELDS.iter().find(|f| f.key == k)
}

pub fn by_homebox(col: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.homebox.eq_ignore_ascii_case(col.trim()))
}

/// Canonical text of a field (`None` when unset).
pub fn get(i: &Item, key: &str) -> Option<String> {
    let s = |v: &Option<String>| v.clone();
    let m = |v: Option<i64>| v.map(fmt_money);
    let b = |v: bool| Some(v.to_string());
    let r = |v: Option<f64>| v.map(fmt_qty);
    match key {
        "quantity" => Some(i.quantity.to_string()),
        "name" => Some(i.name.clone()),
        "asset_id" => s(&i.asset_id),
        "description" => s(&i.description),
        "insured" => b(i.insured),
        "serial_number" => s(&i.serial_number),
        "model_number" => s(&i.model_number),
        "manufacturer" => s(&i.manufacturer),
        "notes" => s(&i.notes),
        "purchase_from" => s(&i.purchase_from),
        "purchase_price" => m(i.purchase_price),
        "purchase_time" => s(&i.purchase_time),
        "lifetime_warranty" => b(i.lifetime_warranty),
        "warranty_expires" => s(&i.warranty_expires),
        "warranty_details" => s(&i.warranty_details),
        "sold_to" => s(&i.sold_to),
        "sold_time" => s(&i.sold_time),
        "sold_price" => m(i.sold_price),
        "sold_notes" => s(&i.sold_notes),
        "owner_entity" => s(&i.owner_entity),
        "replacement_value" => m(i.replacement_value),
        "valued_at" => s(&i.valued_at),
        "status" => Some(i.status.clone()),
        "kind_preset" => s(&i.kind_preset),
        "unit" => s(&i.unit),
        "par_level" => r(i.par_level),
        "reorder_qty" => r(i.reorder_qty),
        "preferred_vendor" => s(&i.preferred_vendor),
        "product_ref" => s(&i.product_ref),
        _ => None,
    }
}

/// How to read user/imported text.
pub struct Parse<'a> {
    pub tz: &'a Tz,
    /// Extra strftime formats accepted for dates.
    pub date_formats: &'a [String],
}

impl Parse<'_> {
    fn day(&self, v: &str) -> Result<String> {
        let fmts: Vec<&str> = self.date_formats.iter().map(String::as_str).collect();
        parse_day(v, &fmts, self.tz)
    }
}

/// Set a field from text; `None` or blank clears it (to the default for
/// required fields: quantity 1, status owned, booleans false).
pub fn set(i: &mut Item, key: &str, value: Option<&str>, p: &Parse) -> Result<()> {
    let field = by_key(key).ok_or_else(|| AppError::usage(format!("unknown item field '{key}'")))?;
    let v = value.map(str::trim).filter(|v| !v.is_empty());
    let ctx = |e: AppError| e.context(field.key);
    let text = || v.map(str::to_string);
    let money = || v.map(parse_money).transpose().map_err(ctx);
    let date = || v.map(|d| p.day(d)).transpose().map_err(ctx);
    let boolean = || v.map(parse_bool).transpose().map(|b| b.flatten().unwrap_or(false)).map_err(ctx);
    let real = || v.map(parse_qty).transpose().map_err(ctx);
    match field.key {
        "quantity" => {
            i.quantity = match v {
                None => 1,
                Some(q) => {
                    let n = parse_qty(q).map_err(ctx)?;
                    if n.fract() != 0.0 {
                        return Err(AppError::invalid(format!("quantity must be a whole number, got '{q}'")));
                    }
                    n as i64
                }
            }
        }
        "name" => {
            i.name = v.ok_or_else(|| AppError::invalid("an item needs a name"))?.to_string();
        }
        "asset_id" => {
            i.asset_id = match v {
                None => None,
                Some(a) => Some(
                    crate::store::parse_asset_id(a)
                        .map(crate::store::format_asset_id)
                        .ok_or_else(|| AppError::invalid(format!("bad asset id '{a}' (expected 000-000)")))?,
                ),
            }
        }
        "description" => i.description = text(),
        "insured" => i.insured = boolean()?,
        "serial_number" => i.serial_number = text(),
        "model_number" => i.model_number = text(),
        "manufacturer" => i.manufacturer = text(),
        "notes" => i.notes = text(),
        "purchase_from" => i.purchase_from = text(),
        "purchase_price" => i.purchase_price = money()?,
        "purchase_time" => i.purchase_time = date()?,
        "lifetime_warranty" => i.lifetime_warranty = boolean()?,
        "warranty_expires" => i.warranty_expires = date()?,
        "warranty_details" => i.warranty_details = text(),
        "sold_to" => i.sold_to = text(),
        "sold_time" => i.sold_time = date()?,
        "sold_price" => i.sold_price = money()?,
        "sold_notes" => i.sold_notes = text(),
        "owner_entity" => i.owner_entity = text(),
        "replacement_value" => i.replacement_value = money()?,
        "valued_at" => i.valued_at = date()?,
        "status" => i.status = v.map_or(Ok("owned".to_string()), |s| check_choice("status", s, STATUSES))?,
        "kind_preset" => i.kind_preset = text(),
        "unit" => i.unit = text(),
        "par_level" => i.par_level = real()?,
        "reorder_qty" => i.reorder_qty = real()?,
        "preferred_vendor" => i.preferred_vendor = text(),
        "product_ref" => i.product_ref = text(),
        other => return Err(AppError::usage(format!("unknown item field '{other}'"))),
    }
    Ok(())
}

/// Names of the fields that differ between two versions of an item.
pub fn changed(before: &Item, after: &Item) -> Vec<&'static str> {
    let mut v: Vec<&'static str> =
        FIELDS.iter().filter(|f| get(before, f.key) != get(after, f.key)).map(|f| f.key).collect();
    if before.location_id != after.location_id {
        v.push("location");
    }
    if before.parent_item_id != after.parent_item_id {
        v.push("parent_item_id");
    }
    if before.import_ref != after.import_ref {
        v.push("import_ref");
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_field_through_text() {
        let tz = Tz::parse("UTC").unwrap();
        let p = Parse { tz: &tz, date_formats: &[] };
        let mut i = Item::new("Drill").unwrap();
        let values = [
            ("quantity", "3"),
            ("asset_id", "000-007"),
            ("insured", "true"),
            ("purchase_price", "149.50"),
            ("purchase_time", "2024-05-01"),
            ("status", "lent"),
            ("par_level", "2.5"),
            ("serial_number", "SN-1"),
        ];
        for (k, v) in values {
            set(&mut i, k, Some(v), &p).unwrap();
            assert_eq!(get(&i, k).as_deref(), Some(v), "{k}");
        }
        set(&mut i, "insured", Some("No"), &p).unwrap();
        assert!(!i.insured);
        set(&mut i, "purchase_price", Some("$1,299"), &p).unwrap();
        assert_eq!(get(&i, "purchase_price").as_deref(), Some("1299.00"));
        set(&mut i, "status", None, &p).unwrap();
        assert_eq!(i.status, "owned");
        assert!(set(&mut i, "status", Some("eaten"), &p).is_err());
        assert!(set(&mut i, "quantity", Some("1.5"), &p).is_err());
        assert!(set(&mut i, "asset_id", Some("12a"), &p).is_err());
        assert!(FIELDS[..HOMEBOX_STANDARD].iter().all(|f| !f.homebox.starts_with("HB.field.")));
        assert!(FIELDS[HOMEBOX_STANDARD..].iter().all(|f| f.homebox.starts_with("HB.field.")));
    }
}
