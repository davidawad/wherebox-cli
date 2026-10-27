//! Homebox CSV, the interop contract.
//!
//! Columns follow Homebox's import-csv documentation exactly:
//! `HB.import_ref`, `HB.location` (nested `A / B / C` paths), `HB.label`
//! (comma list), the standard `HB.*` columns, and `HB.field.<Name>` custom
//! fields. Fields Homebox lacks travel as custom fields (see
//! [`crate::fields::FIELDS`]), plus:
//!
//! * `HB.field.Source`: the item's sources, `source:ref; source:ref`;
//! * lots: one row per active lot, with import ref `<item ref>#lot:<lot id>`,
//!   `HB.quantity` the lot's (whole) quantity and `HB.field.Lot Qty`,
//!   `Acquired`, `Opened`, `Expires`, `Best By`, `Frozen`, `Lot Status`,
//!   `Lot Price` (one unit) and `Lot Ref` (the purchase it came from). Item
//!   columns repeat on every lot row; the asset id only on the first;
//! * the item an item is stored inside: a last line `Inside: <name>
//!   (wherebox:<id>)` appended to `HB.notes`.
//!
//! Export -> import -> export is byte-stable.

use std::collections::{BTreeMap, HashMap};

use crate::commands::import::{Incoming, IncomingLot, RawRow};
use crate::commands::items::Lookups;
use crate::error::{AppError, Result};
use crate::fields::{self, FIELDS, HOMEBOX_STANDARD};
use crate::store::{Item, Lot, Source};
use crate::util::fmt_qty;

pub const IMPORT_REF: &str = "HB.import_ref";
pub const LOCATION: &str = "HB.location";
pub const LABEL: &str = "HB.label";
/// Homebox's own exports name the label column `HB.labels`; accepted on import.
pub const LABELS_ALT: &str = "HB.labels";
pub const FIELD_PREFIX: &str = "HB.field.";
pub const SOURCE: &str = "Source";
/// Lot columns, in export order.
pub const LOT_FIELDS: &[&str] =
    &["Lot Qty", "Acquired", "Opened", "Expires", "Best By", "Frozen", "Lot Status", "Lot Price", "Lot Ref"];
pub const LOT_SEP: &str = "#lot:";
const INSIDE_PREFIX: &str = "Inside: ";
const INSIDE_TAG: &str = "(wherebox:";

/// Names of `HB.field.*` columns wherebox reads into its own fields.
pub fn is_reserved_field(name: &str) -> bool {
    let n = name.trim();
    FIELDS
        .iter()
        .filter_map(|f| f.homebox.strip_prefix(FIELD_PREFIX))
        .chain([SOURCE])
        .chain(LOT_FIELDS.iter().copied())
        .any(|r| r.eq_ignore_ascii_case(n))
}

/// Does this header row look like Homebox CSV?
pub fn is_homebox_header<S: AsRef<str>>(headers: &[S]) -> bool {
    headers.iter().any(|h| h.as_ref().trim().starts_with("HB."))
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn notes_with_parent(i: &Item, parent: Option<&Item>) -> Option<String> {
    let tag = parent.map(|p| format!("{INSIDE_PREFIX}{} {INSIDE_TAG}{})", p.name, p.id));
    match (i.notes.as_deref(), tag) {
        (n, None) => n.map(str::to_string),
        (None, Some(t)) => Some(t),
        (Some(n), Some(t)) => Some(format!("{n}\n{t}")),
    }
}

/// Split the `Inside: ...` line off imported notes: (notes, parent id).
pub fn split_parent(notes: &str) -> (Option<String>, Option<String>) {
    let (head, last) = notes.rsplit_once('\n').map_or(("", notes), |(h, l)| (h, l));
    let parent = last
        .trim()
        .strip_prefix(INSIDE_PREFIX)
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|rest| rest.rsplit_once(INSIDE_TAG))
        .map(|(_, id)| id.trim().to_string())
        .filter(|id| !id.is_empty());
    match parent {
        Some(p) => (Some(head.to_string()).filter(|h| !h.trim().is_empty()), Some(p)),
        None => (Some(notes.to_string()).filter(|n| !n.trim().is_empty()), None),
    }
}

fn lot_quantity(q: f64) -> String {
    if q.fract() == 0.0 {
        fmt_qty(q)
    } else {
        fmt_qty(q.ceil())
    }
}

/// Header and rows (cells in header order) for `items`.
pub fn export_rows(items: &[Item], lk: &Lookups, lots: &[Lot], all: &[Item]) -> (Vec<String>, Vec<Vec<String>>) {
    let by_id: HashMap<&str, &Item> = all.iter().map(|i| (i.id.as_str(), i)).collect();
    let mut rows: Vec<BTreeMap<String, String>> = Vec::new();
    for i in items {
        let mut base: BTreeMap<String, String> = BTreeMap::new();
        let mut put = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                base.insert(k.to_string(), v);
            }
        };
        put(IMPORT_REF, Some(i.import_ref.clone()));
        put(LOCATION, lk.tree.opt_path(i.location_id));
        put(LABEL, Some(lk.labels_of(&i.id).join(",")));
        for f in FIELDS {
            let v = if f.key == "notes" {
                notes_with_parent(i, i.parent_item_id.as_deref().and_then(|p| by_id.get(p).copied()))
            } else {
                fields::get(i, f.key)
            };
            put(f.homebox, v);
        }
        put(
            &format!("{FIELD_PREFIX}{SOURCE}"),
            Some(lk.sources_of(&i.id).iter().map(Source::display).collect::<Vec<_>>().join("; ")),
        );
        for (n, v) in lk.fields_of(&i.id) {
            put(&format!("{FIELD_PREFIX}{n}"), Some(v));
        }
        let active: Vec<&Lot> = lots.iter().filter(|l| l.item_id == i.id && l.is_active()).collect();
        if active.is_empty() {
            rows.push(base);
            continue;
        }
        for (n, l) in active.iter().enumerate() {
            let mut r = base.clone();
            if n > 0 {
                r.remove("HB.asset_id");
            }
            r.insert(IMPORT_REF.into(), format!("{}{LOT_SEP}{}", i.import_ref, l.id));
            r.insert("HB.quantity".into(), lot_quantity(l.qty));
            match lk.tree.opt_path(l.location_id.or(i.location_id)) {
                Some(p) => r.insert(LOCATION.into(), p),
                None => r.remove(LOCATION),
            };
            let lot_vals = [
                Some(fmt_qty(l.qty)),
                l.acquired_at.clone(),
                l.opened_at.clone(),
                lk.est.lot(l).and_then(|v| v.expiry.as_ref()).map(|e| e.date.to_string()),
                l.best_by.clone(),
                l.frozen_at.clone(),
                Some(l.status.clone()),
                l.unit_price.map(crate::util::fmt_money),
                l.source_ref.clone(),
            ];
            for (name, v) in LOT_FIELDS.iter().zip(lot_vals) {
                if let Some(v) = v {
                    r.insert(format!("{FIELD_PREFIX}{name}"), v);
                }
            }
            rows.push(r);
        }
    }
    let mut header: Vec<String> = [IMPORT_REF, LOCATION, LABEL].iter().map(|s| s.to_string()).collect();
    header.extend(FIELDS[..HOMEBOX_STANDARD].iter().map(|f| f.homebox.to_string()));
    let known: Vec<String> = FIELDS[HOMEBOX_STANDARD..]
        .iter()
        .map(|f| f.homebox.to_string())
        .chain(std::iter::once(SOURCE).chain(LOT_FIELDS.iter().copied()).map(|n| format!("{FIELD_PREFIX}{n}")))
        .collect();
    let present = |c: &String| rows.iter().any(|r| r.contains_key(c));
    header.extend(known.iter().filter(|c| present(c)).cloned());
    let mut custom: Vec<String> = rows
        .iter()
        .flat_map(|r| r.keys())
        .filter(|k| k.starts_with(FIELD_PREFIX) && !known.contains(k))
        .cloned()
        .collect();
    custom.sort_by_key(|c| (c.to_lowercase(), c.clone()));
    custom.dedup();
    header.extend(custom);
    let cells = rows.iter().map(|r| header.iter().map(|h| r.get(h).cloned().unwrap_or_default()).collect()).collect();
    (header, cells)
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Labels of an `HB.label` cell: Homebox separates them with `,`; `;` (seen
/// in hand- and agent-made files) is accepted too.
pub fn split_labels(v: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in v.split([',', ';']).map(str::trim).filter(|l| !l.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(l)) {
            out.push(l.to_string());
        }
    }
    out
}

/// Turn one Homebox CSV row into an [`Incoming`] item (plus at most one lot).
pub fn parse_row(line: usize, row: &RawRow) -> Result<Incoming> {
    let mut inc = Incoming { line, ..Default::default() };
    let mut lot_vals: HashMap<&'static str, String> = HashMap::new();
    for (col, val) in row {
        let col = col.trim();
        let v = val.trim();
        let present = |v: &str| Some(v.to_string()).filter(|v| !v.is_empty());
        if col == IMPORT_REF {
            if let Some(r) = present(v) {
                match r.rsplit_once(LOT_SEP) {
                    Some((item, lot)) => {
                        inc.key = Some(item.trim().to_string());
                        lot_vals.insert("id", lot.trim().to_string());
                    }
                    None => inc.key = Some(r),
                }
            }
        } else if col == LOCATION {
            inc.location = Some(present(v));
        } else if col == LABEL || col == LABELS_ALT {
            if v.contains(';') {
                inc.warnings.push(format!(
                    "line {line}: {col} \"{v}\" separates labels with ';' (Homebox uses ','); split on both"
                ));
            }
            inc.labels = Some(split_labels(v));
        } else if let Some(name) = col.strip_prefix(FIELD_PREFIX) {
            // Empty custom-field cells are ignored, as in Homebox.
            let Some(v) = present(v) else { continue };
            if let Some(f) = fields::by_homebox(col) {
                inc.values.push((f.key, Some(v)));
            } else if name.trim().eq_ignore_ascii_case(SOURCE) {
                inc.sources = Some(v.split(';').filter_map(Source::parse).collect());
            } else if let Some(lf) = LOT_FIELDS.iter().find(|l| l.eq_ignore_ascii_case(name.trim())) {
                lot_vals.insert(lf, v);
            } else {
                inc.custom.push((name.trim().to_string(), Some(v)));
            }
        } else if let Some(f) = fields::by_homebox(col) {
            if f.key == "notes" {
                let (notes, parent) = split_parent(v);
                inc.values.push(("notes", notes));
                inc.parent = Some(parent);
            } else {
                inc.values.push((f.key, present(v)));
            }
        } else {
            inc.ignored.push(col.to_string());
        }
    }
    let is_lot = lot_vals.contains_key("id") || LOT_FIELDS.iter().any(|f| lot_vals.contains_key(f));
    if is_lot {
        let qty_text = lot_vals
            .get("Lot Qty")
            .cloned()
            .or_else(|| inc.value("quantity").map(str::to_string))
            .unwrap_or_else(|| "1".into());
        let get = |k: &str| lot_vals.get(k).cloned();
        inc.lots.push(IncomingLot {
            id: get("id"),
            qty: crate::util::parse_qty(&qty_text).map_err(|e| e.context("lot quantity"))?,
            acquired_at: get("Acquired"),
            opened_at: get("Opened"),
            expires_at: get("Expires"),
            best_by: get("Best By"),
            frozen_at: get("Frozen"),
            status: get("Lot Status"),
            location: inc.location.clone().flatten(),
            unit_price: get("Lot Price"),
            source_ref: get("Lot Ref"),
        });
        // HB.quantity on a lot row is the lot's, not the item's.
        inc.values.retain(|(k, _)| *k != "quantity");
    }
    if inc.values.iter().all(|(k, _)| *k != "name") && inc.key.is_none() {
        return Err(AppError::invalid("row has neither HB.name nor HB.import_ref"));
    }
    Ok(inc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_marker_round_trips() {
        assert_eq!(split_parent("plain"), (Some("plain".into()), None));
        assert_eq!(split_parent("a\nInside: Big Box (wherebox:01ABC)"), (Some("a".into()), Some("01ABC".into())));
        assert_eq!(split_parent("Inside: Box (x) (wherebox:01ABC)"), (None, Some("01ABC".into())));
        assert!(is_reserved_field("owner entity"));
        assert!(is_reserved_field("Expires"));
        assert!(!is_reserved_field("Color"));
    }

    #[test]
    fn labels_split_on_comma_and_semicolon_with_a_warning() {
        assert_eq!(split_labels("a, b,,c"), ["a", "b", "c"]);
        assert_eq!(split_labels("a;b; A"), ["a", "b"]);
        let row: RawRow = vec![("HB.name".into(), "Drill".into()), ("HB.label".into(), "tools;garage".into())];
        let inc = parse_row(7, &row).unwrap();
        assert_eq!(inc.labels.as_deref(), Some(&["tools".to_string(), "garage".to_string()][..]));
        assert_eq!(inc.warnings.len(), 1);
        assert!(inc.warnings[0].starts_with("line 7: HB.label"), "{:?}", inc.warnings);
        let ok: RawRow = vec![("HB.name".into(), "Saw".into()), ("HB.label".into(), "tools,garage".into())];
        assert!(parse_row(8, &ok).unwrap().warnings.is_empty());
    }
}
