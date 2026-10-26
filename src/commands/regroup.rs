//! `db regroup-consumables`: migrate databases imported before repeat
//! purchases of a consumable became lots.
//!
//! Consumables (see [`crate::consumable`]) that are not gone are grouped by
//! property and product (ASIN, else product ref). In each group the item
//! that already tracks lots, else the earliest purchase, is kept: every other
//! item's lots move to it, every item without lots becomes a lot on it
//! (quantity, purchase date, unit price, and its import ref as the lot's
//! source ref, so re-importing the same file matches the lot), and the others
//! are deleted after their labels, custom fields, sources, attachments,
//! contents and history move over. The kept item takes the latest purchase's
//! date, price and vendor. A second run finds nothing to do.

use std::collections::BTreeMap;

use serde_json::json;

use crate::consumable;
use crate::context::Ctx;
use crate::db::{text, Db};
use crate::error::Result;
use crate::output::{to_record, Record, Report};
use crate::presets::Catalog;
use crate::store::{self, Item, Lot, Tree};
use crate::util::merge_tags;

pub fn run(ctx: &Ctx, dry_run: bool) -> Result<()> {
    let db = ctx.db()?;
    let catalog = Catalog::load(&ctx.resolved.config_path)?;
    db.begin()?;
    let rows = match regroup(&db, &catalog) {
        Ok(r) if dry_run => {
            db.rollback()?;
            r
        }
        Ok(r) => {
            db.commit()?;
            r
        }
        Err(e) => {
            let _ = db.rollback();
            return Err(e);
        }
    };
    let n = |k: &str| rows.iter().filter_map(|r| r.get(k).and_then(serde_json::Value::as_u64)).sum::<u64>();
    ctx.info(&format!(
        "{}{} consumable(s) regrouped: {} item(s) merged, {} lot(s) created, {} lot(s) moved",
        if dry_run { "dry run, nothing written: " } else { "" },
        rows.len(),
        n("merged"),
        n("lots_created"),
        n("lots_moved")
    ));
    let report = Report::list("regroup", rows)
        .table_columns(&["property", "product", "name", "asset_id", "merged", "lots_created", "lots_moved"])
        .meta("dry_run", json!(dry_run));
    if dry_run {
        ctx.emit(&report)
    } else {
        ctx.emit_mutation(&report)
    }
}

fn regroup(db: &Db, catalog: &Catalog) -> Result<Vec<Record>> {
    let tree = Tree::load(db)?;
    let fields = store::all_item_fields(db)?;
    let labels = store::all_item_labels(db)?;
    let mut lots: BTreeMap<String, Vec<Lot>> = BTreeMap::new();
    for l in store::all_lots(db)? {
        lots.entry(l.item_id.clone()).or_default().push(l);
    }
    let mut groups: BTreeMap<String, (String, String, Vec<Item>)> = BTreeMap::new();
    for i in store::all_items(db)?.into_iter().filter(|i| !i.is_gone()) {
        let f = fields.get(&i.id);
        let no_labels = Vec::new();
        let kind = consumable::field(f, consumable::KIND_FIELD);
        if !consumable::is_consumable(kind, i.kind_preset.as_deref(), labels.get(&i.id).unwrap_or(&no_labels), catalog)
        {
            continue;
        }
        let Some(product) =
            consumable::product_key(consumable::field(f, consumable::ASIN_FIELD), i.product_ref.as_deref())
        else {
            continue;
        };
        let property = i.location_id.and_then(|l| tree.root_of(l)).map(|l| l.name.clone()).unwrap_or_default();
        groups
            .entry(consumable::group_key(Some(&property), &product))
            .or_insert_with(|| (property, product, Vec::new()))
            .2
            .push(i);
    }
    let has_lots = |i: &Item| lots.get(&i.id).is_some_and(|v| !v.is_empty());
    let mut out = Vec::new();
    for (property, product, mut members) in groups.into_values() {
        if members.len() == 1 && has_lots(&members[0]) {
            continue;
        }
        members.sort_by(|a, b| {
            (a.purchase_time.is_none(), &a.purchase_time, &a.id).cmp(&(
                b.purchase_time.is_none(),
                &b.purchase_time,
                &b.id,
            ))
        });
        let k = members.iter().position(has_lots).unwrap_or(0);
        let mut keeper = members[k].clone();
        let before = keeper.clone();
        let (mut created, mut moved, mut merged) = (0usize, 0usize, Vec::new());
        for m in &members {
            if has_lots(m) {
                if m.id != keeper.id {
                    db.execute("UPDATE lots SET item_id = ?1 WHERE item_id = ?2", &[text(&keeper.id), text(&m.id)])?;
                    moved += lots.get(&m.id).map_or(0, Vec::len);
                }
            } else if store::lot_by_source(db, &m.import_ref)?.is_none() {
                store::insert_lot(db, &purchase_lot(m, &keeper)?)?;
                created += 1;
            }
            if m.id == keeper.id {
                continue;
            }
            absorb(db, &keeper, m)?;
            if m.purchase_time.is_some() && m.purchase_time >= keeper.purchase_time {
                keeper.purchase_time.clone_from(&m.purchase_time);
                keeper.purchase_price = m.purchase_price.or(keeper.purchase_price);
                keeper.purchase_from = m.purchase_from.clone().or(keeper.purchase_from);
            }
            merged.push(json!({"id": m.id, "import_ref": m.import_ref, "asset_id": m.asset_id}));
        }
        if !keeper.same_content(&before) {
            keeper.updated_at = crate::util::now_iso();
            store::update_item(db, &keeper)?;
        }
        store::sync_lot_quantity(db, &keeper.id)?;
        store::add_event(
            db,
            &keeper.id,
            "regrouped",
            json!({"product": product, "merged": merged, "lots_created": created, "lots_moved": moved,
                   "via": "db regroup-consumables"}),
        )?;
        out.push(to_record(&json!({
            "property": Some(property).filter(|p| !p.is_empty()),
            "product": product,
            "id": keeper.id,
            "asset_id": keeper.asset_id,
            "name": keeper.name,
            "merged": merged.len(),
            "lots_created": created,
            "lots_moved": moved,
        })));
    }
    Ok(out)
}

/// The lot an item without lots stands for: one purchase.
fn purchase_lot(m: &Item, keeper: &Item) -> Result<Lot> {
    Ok(Lot {
        id: crate::ulid::new()?,
        item_id: keeper.id.clone(),
        qty: m.quantity as f64,
        acquired_at: m.purchase_time.clone(),
        location_id: m.location_id.filter(|l| Some(*l) != keeper.location_id),
        status: if m.quantity > 0 { "sealed" } else { "consumed" }.into(),
        created_at: crate::util::now_iso(),
        unit_price: m.purchase_price,
        source_ref: Some(m.import_ref.clone()),
        ..Default::default()
    })
}

/// Move what `m` has (labels, fields the keeper lacks, sources, attachments,
/// contents, history) to `keeper`, then delete `m`.
fn absorb(db: &Db, keeper: &Item, m: &Item) -> Result<()> {
    let have = store::item_labels(db, &keeper.id)?;
    store::set_item_labels(db, &keeper.id, &merge_tags(&have, &store::item_labels(db, &m.id)?, &[]))?;
    let kf = store::item_fields(db, &keeper.id)?;
    for (n, v) in store::item_fields(db, &m.id)? {
        if !kf.keys().any(|k| k.eq_ignore_ascii_case(&n)) {
            store::set_item_field(db, &keeper.id, &n, Some(&v))?;
        }
    }
    let ks = store::item_sources(db, &keeper.id)?;
    for s in store::item_sources(db, &m.id)?.iter().filter(|s| !ks.contains(s)) {
        store::add_source(db, &keeper.id, s)?;
    }
    let shas: Vec<String> = store::attachments(db, Some(&keeper.id))?.into_iter().map(|a| a.sha256).collect();
    for a in store::attachments(db, Some(&m.id))?.iter().filter(|a| !shas.contains(&a.sha256)) {
        db.execute("UPDATE attachments SET item_id = ?1 WHERE id = ?2", &[text(&keeper.id), crate::db::int(a.id)])?;
    }
    db.execute("UPDATE items SET parent_item_id = ?1 WHERE parent_item_id = ?2", &[text(&keeper.id), text(&m.id)])?;
    db.execute("UPDATE events SET item_id = ?1 WHERE item_id = ?2", &[text(&keeper.id), text(&m.id)])?;
    store::add_event(
        db,
        &keeper.id,
        "merged",
        json!({"from": m.id, "import_ref": m.import_ref, "asset_id": m.asset_id, "via": "db regroup-consumables"}),
    )?;
    store::delete_item(db, &m.id)
}
