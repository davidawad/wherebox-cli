//! Items: `add`, `edit`, `mv`, `rm`, `show`, `ls`, `find`, `history`, `value`.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use crate::cli::{AddArgs, EditArgs, FindArgs, ItemFilter, ItemFlags, ItemSort, LsArgs, MvArgs, RmArgs, ValueArgs};
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::fields::{self, Parse};
use crate::output::{to_record, Format, Record, Report};
use crate::store::{self, Item, Lot, Source, Tree, GONE};
use crate::util::{money_value, parse_date};

pub const ITEM_TABLE: &[&str] = &["asset_id", "name", "quantity", "location", "status", "value", "labels"];

/// Everything needed to describe items, loaded once per command.
pub struct Lookups {
    pub tree: Tree,
    pub labels: HashMap<String, Vec<String>>,
    pub fields: HashMap<String, BTreeMap<String, String>>,
    pub sources: HashMap<String, Vec<Source>>,
}

impl Lookups {
    pub fn load(db: &Db) -> Result<Self> {
        Ok(Self {
            tree: Tree::load(db)?,
            labels: store::all_item_labels(db)?,
            fields: store::all_item_fields(db)?,
            sources: store::all_item_sources(db)?,
        })
    }

    pub fn labels_of(&self, id: &str) -> Vec<String> {
        self.labels.get(id).cloned().unwrap_or_default()
    }

    pub fn fields_of(&self, id: &str) -> BTreeMap<String, String> {
        self.fields.get(id).cloned().unwrap_or_default()
    }

    pub fn sources_of(&self, id: &str) -> Vec<Source> {
        self.sources.get(id).cloned().unwrap_or_default()
    }
}

/// The JSON/table record of an item: native field names, money in dollars,
/// the location as a full path.
pub fn item_record(i: &Item, lk: &Lookups) -> Record {
    let money = |v: Option<i64>| json!(v.map(money_value));
    let mut r = Record::new();
    let mut put = |k: &str, v: Value| {
        r.insert(k.to_string(), v);
    };
    put("id", json!(i.id));
    put("asset_id", json!(i.asset_id));
    put("name", json!(i.name));
    put("quantity", json!(i.quantity));
    put("location", json!(lk.tree.opt_path(i.location_id)));
    put("status", json!(i.status));
    put("value", money(i.value()));
    put("labels", json!(lk.labels_of(&i.id)));
    put("description", json!(i.description));
    put("owner_entity", json!(i.owner_entity));
    put("manufacturer", json!(i.manufacturer));
    put("model_number", json!(i.model_number));
    put("serial_number", json!(i.serial_number));
    put("purchase_from", json!(i.purchase_from));
    put("purchase_price", money(i.purchase_price));
    put("purchase_time", json!(i.purchase_time));
    put("replacement_value", money(i.replacement_value));
    put("valued_at", json!(i.valued_at));
    put("insured", json!(i.insured));
    put("lifetime_warranty", json!(i.lifetime_warranty));
    put("warranty_expires", json!(i.warranty_expires));
    put("warranty_details", json!(i.warranty_details));
    put("sold_to", json!(i.sold_to));
    put("sold_time", json!(i.sold_time));
    put("sold_price", money(i.sold_price));
    put("sold_notes", json!(i.sold_notes));
    put("notes", json!(i.notes));
    put("kind_preset", json!(i.kind_preset));
    put("unit", json!(i.unit));
    put("par_level", json!(i.par_level));
    put("reorder_qty", json!(i.reorder_qty));
    put("preferred_vendor", json!(i.preferred_vendor));
    put("product_ref", json!(i.product_ref));
    put("parent_item_id", json!(i.parent_item_id));
    put("import_ref", json!(i.import_ref));
    put("fields", json!(lk.fields_of(&i.id)));
    put("sources", json!(lk.sources_of(&i.id).iter().map(Source::display).collect::<Vec<_>>()));
    put("created_at", json!(i.created_at));
    put("updated_at", json!(i.updated_at));
    r
}

pub fn lot_record(l: &Lot, unit: Option<&str>, tree: &Tree) -> Record {
    to_record(&json!({
        "id": l.id,
        "qty": l.qty,
        "unit": unit,
        "status": l.status,
        "acquired_at": l.acquired_at,
        "opened_at": l.opened_at,
        "expires_at": l.expires_at,
        "best_by": l.best_by,
        "frozen_at": l.frozen_at,
        "location": tree.opt_path(l.location_id),
        "unit_price": l.unit_price.map(crate::util::money_value),
        "source_ref": l.source_ref,
    }))
}

pub fn parse_ctx(ctx: &Ctx) -> Vec<String> {
    ctx.input_date_formats()
}

/// Apply `add`/`edit` field flags to an item.
pub fn apply_flags(item: &mut Item, f: &ItemFlags, p: &Parse, today: &str) -> Result<()> {
    let pairs: [(&str, &Option<String>); 24] = [
        ("quantity", &f.qty),
        ("purchase_price", &f.price),
        ("purchase_time", &f.bought),
        ("purchase_from", &f.from),
        ("serial_number", &f.serial),
        ("model_number", &f.model),
        ("manufacturer", &f.maker),
        ("owner_entity", &f.owner),
        ("description", &f.description),
        ("notes", &f.notes),
        ("insured", &f.insured),
        ("lifetime_warranty", &f.lifetime_warranty),
        ("warranty_expires", &f.warranty_expires),
        ("warranty_details", &f.warranty_details),
        ("replacement_value", &f.value),
        ("valued_at", &f.valued_at),
        ("status", &f.status),
        ("asset_id", &f.asset_id),
        ("kind_preset", &f.preset),
        ("unit", &f.unit),
        ("par_level", &f.par),
        ("reorder_qty", &f.reorder_qty),
        ("preferred_vendor", &f.vendor),
        ("product_ref", &f.product_ref),
    ];
    for (k, v) in pairs {
        if let Some(v) = v {
            fields::set(item, k, Some(v), p)?;
        }
    }
    if f.value.is_some() && f.valued_at.is_none() {
        item.valued_at = Some(today.to_string());
    }
    if let Some(r) = f.import_ref.as_deref().map(str::trim) {
        if r.is_empty() {
            return Err(AppError::invalid("--import-ref cannot be empty"));
        }
        item.import_ref = r.to_string();
    }
    Ok(())
}

/// `NAME=VALUE` custom field flags (an empty value removes the field).
pub fn parse_field_flags(specs: &[String]) -> Result<Vec<(String, Option<String>)>> {
    specs
        .iter()
        .map(|s| {
            let (n, v) =
                s.split_once('=').ok_or_else(|| AppError::usage(format!("bad --field '{s}' (expected NAME=VALUE)")))?;
            let n = n.trim();
            if n.is_empty() {
                return Err(AppError::usage(format!("bad --field '{s}' (empty name)")));
            }
            if crate::commands::homebox::is_reserved_field(n) {
                return Err(AppError::usage(format!(
                    "'{n}' is a built-in field; use its own flag (see `wherebox add --help`)"
                )));
            }
            Ok((n.to_string(), Some(v.trim().to_string()).filter(|v| !v.is_empty())))
        })
        .collect()
}

pub fn parse_sources(specs: &[String]) -> Result<Vec<Source>> {
    specs.iter().map(|s| Source::parse(s).ok_or_else(|| AppError::usage(format!("bad --source '{s}'")))).collect()
}

/// Append events describing what changed between `before` and `after`.
pub fn record_changes(db: &Db, tree: &Tree, before: &Item, after: &Item, via: &str) -> Result<()> {
    if before.location_id != after.location_id {
        store::add_event(
            db,
            &after.id,
            "moved",
            json!({"from": tree.opt_path(before.location_id), "to": tree.opt_path(after.location_id), "via": via}),
        )?;
    }
    if before.status != after.status {
        store::add_event(db, &after.id, "status", json!({"from": before.status, "to": after.status, "via": via}))?;
    }
    if before.replacement_value != after.replacement_value || before.valued_at != after.valued_at {
        store::add_event(
            db,
            &after.id,
            "valued",
            json!({"value": after.replacement_value.map(money_value), "previous": before.replacement_value.map(money_value), "valued_at": after.valued_at, "via": via}),
        )?;
    }
    let edited: Vec<&str> = fields::changed(before, after)
        .into_iter()
        .filter(|f| !matches!(*f, "location" | "status" | "replacement_value" | "valued_at"))
        .collect();
    if !edited.is_empty() {
        store::add_event(db, &after.id, "edited", json!({"fields": edited, "via": via}))?;
    }
    Ok(())
}

/// Resolve `--inside ITEM`, refusing an item inside itself (directly or not).
fn resolve_parent(db: &Db, item_id: &str, spec: &str) -> Result<String> {
    let parent = store::resolve_item(db, spec)?;
    let mut cur = Some(parent.clone());
    while let Some(p) = cur {
        if p.id == item_id {
            return Err(AppError::invalid(format!("'{}' cannot be inside itself", parent.name)));
        }
        cur = p.parent_item_id.as_deref().map(|id| store::get_item(db, id)).transpose()?.flatten();
    }
    Ok(parent.id)
}

fn today(ctx: &Ctx) -> String {
    ctx.tz.today().format("%Y-%m-%d").to_string()
}

pub fn add(ctx: &Ctx, a: AddArgs) -> Result<()> {
    let db = ctx.db()?;
    let fmts = parse_ctx(ctx);
    let p = Parse { tz: &ctx.tz, date_formats: &fmts };
    let today = today(ctx);
    let mut item = Item::new(&a.name)?;
    apply_flags(&mut item, &a.item, &p, &today)?;
    let mut custom = parse_field_flags(&a.item.fields)?;
    let mut sources = parse_sources(&a.item.sources)?;
    if sources.is_empty() {
        sources.push(Source { source: "manual".into(), source_ref: String::new() });
    }
    let catalog = crate::presets::Catalog::load(&ctx.resolved.config_path)?;
    let preset = item.kind_preset.as_deref().map(|n| catalog.require(n)).transpose()?;
    // A preset with a shelf life tracks the purchase as a lot to date it.
    let make_lot = a.lot || a.lot_qty.is_some() || a.dates.any() || preset.is_some_and(|p| p.shelf_life.date.is_some());
    let lot_qty =
        a.lot_qty.as_deref().or(a.item.qty.as_deref()).map(crate::util::parse_qty).transpose()?.unwrap_or(1.0);
    let mut lot =
        make_lot.then(|| crate::commands::lots::new_lot(&item.id, lot_qty, &a.dates, None, &p, &today)).transpose()?;
    let mut labels = a.labels.clone();
    let mut filled = None;
    let mut tree = Tree::load(&db)?;
    db.transaction(|db| {
        if let Some(at) = &a.at {
            let (loc, created) = tree.resolve_or_create(db, at)?;
            if created > 0 {
                ctx.info(&format!("created location {}", tree.path(loc)));
            }
            item.location_id = Some(loc);
        }
        if let Some(spec) = &a.item.inside {
            item.parent_item_id = Some(resolve_parent(db, &item.id, spec)?);
        }
        if item.asset_id.is_none() {
            item.asset_id = Some(store::next_asset_id(db)?);
        }
        if let Some(pr) = preset {
            let have: BTreeMap<String, String> =
                custom.iter().filter_map(|(n, v)| v.clone().map(|v| (n.clone(), v))).collect();
            let (fields, more_labels) = pr.apply_item(&mut item, &have, ctx.tz.today());
            custom.extend(fields.into_iter().map(|(n, v)| (n, Some(v))));
            labels = crate::util::merge_tags(&labels, &more_labels, &[]);
            let storage = tree.opt_path(item.location_id).and_then(|p| crate::presets::Storage::from_path(&p));
            filled = lot.as_mut().and_then(|l| pr.fill_lot(l, storage));
        }
        store::insert_item(db, &item)?;
        store::set_item_labels(db, &item.id, &labels)?;
        custom.iter().try_for_each(|(n, v)| store::set_item_field(db, &item.id, n, v.as_deref()))?;
        sources.iter().try_for_each(|s| store::add_source(db, &item.id, s))?;
        store::add_event(db, &item.id, "created", json!({"location": tree.opt_path(item.location_id), "via": "add"}))?;
        if let Some(l) = &lot {
            store::insert_lot(db, l)?;
            store::sync_lot_quantity(db, &item.id)?;
        }
        Ok(())
    })?;
    let item = store::get_item(&db, &item.id)?.ok_or_else(|| AppError::db("item vanished after insert"))?;
    let lk = Lookups::load(&db)?;
    ctx.info(&format!(
        "added {} {}{}",
        item.asset_id.as_deref().unwrap_or(&item.id),
        item.name,
        lk.tree.opt_path(item.location_id).map(|p| format!(" at {p}")).unwrap_or_default()
    ));
    if let (Some(f), Some(pr)) = (&filled, preset) {
        ctx.info(&format!(
            "{}: {} {} ({}; see `wherebox preset show {}`)",
            pr.name,
            fill_label(f.field),
            f.date,
            f.why,
            pr.name
        ));
    }
    ctx.emit_mutation(&Report::object("item", item_record(&item, &lk)))
}

/// How a filled lot date is called in messages.
pub fn fill_label(field: &str) -> &'static str {
    if field == "expires_at" {
        "use by"
    } else {
        "best by"
    }
}

pub fn edit(ctx: &Ctx, a: EditArgs) -> Result<()> {
    let db = ctx.db()?;
    let fmts = parse_ctx(ctx);
    let p = Parse { tz: &ctx.tz, date_formats: &fmts };
    let before = store::resolve_item(&db, &a.item)?;
    let mut item = before.clone();
    apply_flags(&mut item, &a.item_flags, &p, &today(ctx))?;
    if let Some(n) = &a.name {
        fields::set(&mut item, "name", Some(n), &p)?;
    }
    for c in &a.clear {
        let f = fields::by_key(c).ok_or_else(|| AppError::usage(format!("unknown field '{c}' for --clear")))?;
        if matches!(f.key, "name" | "quantity" | "status") {
            return Err(AppError::usage(format!("{} cannot be cleared", f.key)));
        }
        fields::set(&mut item, f.key, None, &p)?;
    }
    let custom = parse_field_flags(&a.item_flags.fields)?;
    let sources = parse_sources(&a.item_flags.sources)?;
    let mut tree = Tree::load(&db)?;
    db.transaction(|db| {
        if let Some(at) = &a.at {
            item.location_id = Some(tree.resolve_or_create(db, at)?.0);
        }
        if a.not_inside {
            item.parent_item_id = None;
        }
        if let Some(spec) = &a.item_flags.inside {
            item.parent_item_id = Some(resolve_parent(db, &item.id, spec)?);
        }
        let labels_before = store::item_labels(db, &item.id)?;
        let labels = crate::util::merge_tags(&labels_before, &a.labels, &[]);
        let labels: Vec<String> =
            labels.into_iter().filter(|l| !a.unlabels.iter().any(|u| u.trim().eq_ignore_ascii_case(l))).collect();
        let fields_before = store::item_fields(db, &item.id)?;
        let changed = !item.same_content(&before);
        if changed {
            item.updated_at = crate::util::now_iso();
            store::update_item(db, &item)?;
            record_changes(db, &tree, &before, &item, "edit")?;
        }
        if labels != labels_before {
            store::set_item_labels(db, &item.id, &labels)?;
            store::add_event(db, &item.id, "edited", json!({"labels": labels, "via": "edit"}))?;
        }
        custom.iter().try_for_each(|(n, v)| store::set_item_field(db, &item.id, n, v.as_deref()))?;
        if store::item_fields(db, &item.id)? != fields_before {
            store::add_event(
                db,
                &item.id,
                "edited",
                json!({"custom_fields": custom.iter().map(|c| &c.0).collect::<Vec<_>>(), "via": "edit"}),
            )?;
        }
        sources.iter().try_for_each(|s| store::add_source(db, &item.id, s))?;
        Ok(())
    })?;
    let item = store::get_item(&db, &item.id)?.ok_or_else(|| AppError::db("item vanished"))?;
    let lk = Lookups::load(&db)?;
    ctx.info(&format!("updated {} {}", item.asset_id.as_deref().unwrap_or(&item.id), item.name));
    ctx.emit_mutation(&Report::object("item", item_record(&item, &lk)))
}

pub fn mv(ctx: &Ctx, a: MvArgs) -> Result<()> {
    let db = ctx.db()?;
    let mut tree = Tree::load(&db)?;
    let items = a.items.iter().map(|s| store::resolve_item(&db, s)).collect::<Result<Vec<_>>>()?;
    let moved = db.transaction(|db| {
        let (to, created) = tree.resolve_or_create(db, &a.to)?;
        if created > 0 {
            ctx.info(&format!("created location {}", tree.path(to)));
        }
        items
            .iter()
            .filter(|i| i.location_id != Some(to))
            .map(|before| {
                let after = Item { location_id: Some(to), updated_at: crate::util::now_iso(), ..before.clone() };
                store::update_item(db, &after)?;
                record_changes(db, &tree, before, &after, "mv")?;
                Ok(after)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let lk = Lookups::load(&db)?;
    ctx.info(&format!("moved {} item(s) to {}", moved.len(), lk.tree.path(lk.tree.resolve(&a.to)?)));
    ctx.emit_mutation(
        &Report::list("moved", moved.iter().map(|i| item_record(i, &lk)).collect()).table_columns(ITEM_TABLE),
    )
}

pub fn rm(ctx: &Ctx, a: RmArgs) -> Result<()> {
    let db = ctx.db()?;
    let fmts = parse_ctx(ctx);
    let p = Parse { tz: &ctx.tz, date_formats: &fmts };
    let before = store::resolve_item(&db, &a.item)?;
    if a.purge {
        let atts = store::attachments(&db, Some(&before.id))?;
        store::delete_item(&db, &before.id)?;
        crate::commands::attach::remove_unreferenced(ctx, &db, &atts)?;
        ctx.info(&format!("purged {} {}", before.asset_id.as_deref().unwrap_or(&before.id), before.name));
        return ctx.emit_mutation(&Report::object("purged", to_record(&json!({"id": before.id, "name": before.name}))));
    }
    let status = store::check_choice("status", &a.status, GONE)?;
    let date = crate::util::parse_day(a.date.as_deref().unwrap_or("today"), &[], &ctx.tz)?;
    let mut item = before.clone();
    item.status = status.clone();
    if status == "sold" {
        item.sold_time = Some(date.clone());
        if let Some(to) = &a.to {
            item.sold_to = Some(to.clone());
        }
        if let Some(price) = &a.price {
            fields::set(&mut item, "sold_price", Some(price), &p)?;
        }
        if let Some(n) = &a.note {
            item.sold_notes = Some(n.clone());
        }
    } else if a.price.is_some() {
        return Err(AppError::usage("--price only applies to --as sold"));
    }
    item.updated_at = crate::util::now_iso();
    let tree = Tree::load(&db)?;
    db.transaction(|db| {
        store::update_item(db, &item)?;
        record_changes(db, &tree, &before, &item, "rm")?;
        store::add_event(db, &item.id, "gone", json!({"as": status, "on": date, "to": a.to, "note": a.note}))
    })?;
    let lk = Lookups::load(&db)?;
    ctx.info(&format!("{} {} ({status})", item.asset_id.as_deref().unwrap_or(&item.id), item.name));
    ctx.emit_mutation(&Report::object("item", item_record(&item, &lk)))
}

/// Text formats get one-line summaries of nested lists in `show`.
fn is_structured(f: Format) -> bool {
    f.is_envelope() || f == Format::Jsonl
}

pub fn show(ctx: &Ctx, spec: &str) -> Result<()> {
    let db = ctx.db()?;
    let item = store::resolve_item(&db, spec)?;
    let lk = Lookups::load(&db)?;
    let mut rec = item_record(&item, &lk);
    let lots = store::lots_for(&db, &item.id)?;
    let atts = store::attachments(&db, Some(&item.id))?;
    let events = store::events_for(&db, &item.id)?;
    let children: Vec<Item> =
        store::all_items(&db)?.into_iter().filter(|c| c.parent_item_id.as_deref() == Some(&item.id)).collect();
    let parent = item.parent_item_id.as_deref().map(|id| store::get_item(&db, id)).transpose()?.flatten();
    rec.insert(
        "inside".into(),
        json!(parent.as_ref().map(|p| format!("{} {}", p.asset_id.as_deref().unwrap_or(&p.id), p.name))),
    );
    if is_structured(ctx.out.format) {
        rec.insert(
            "contains".into(),
            json!(children
                .iter()
                .map(|c| json!({"id": c.id, "asset_id": c.asset_id, "name": c.name}))
                .collect::<Vec<_>>()),
        );
        rec.insert(
            "lots".into(),
            json!(lots.iter().map(|l| lot_record(l, item.unit.as_deref(), &lk.tree)).collect::<Vec<_>>()),
        );
        rec.insert("consumption".into(), json!(store::consumption_for(&db, &item.id)?));
        rec.insert("attachments".into(), json!(atts));
        rec.insert("history".into(), json!(events));
    } else {
        let active: Vec<&Lot> = lots.iter().filter(|l| l.is_active()).collect();
        let on_hand: f64 = active.iter().map(|l| l.qty).sum();
        let next_due = active.iter().filter_map(|l| l.due()).min();
        rec.insert("contains".into(), json!(children.iter().map(|c| c.name.clone()).collect::<Vec<_>>()));
        rec.insert(
            "lots".into(),
            json!((!lots.is_empty()).then(|| format!(
                "{} active ({} {}){}",
                active.len(),
                crate::util::fmt_qty(on_hand),
                item.unit.as_deref().unwrap_or("ea"),
                next_due.map(|d| format!(", next due {d}")).unwrap_or_default()
            ))),
        );
        rec.insert(
            "attachments".into(),
            json!(atts
                .iter()
                .map(|a| format!("#{} {}: {}", a.id, a.kind, a.filename.as_deref().unwrap_or(&a.sha256)))
                .collect::<Vec<_>>()),
        );
        rec.insert(
            "history".into(),
            json!(events.iter().map(|e| format!("{} {}", e.at.get(..10).unwrap_or(&e.at), e.kind)).collect::<Vec<_>>()),
        );
        rec.retain(|_, v| !v.is_null() && v != &json!([]) && v != &json!({}));
    }
    ctx.emit(&Report::object("item", rec))
}

/// Items matching the filter flags (gone items only with --all or --status).
pub fn filtered(ctx: &Ctx, db: &Db, lk: &Lookups, f: &ItemFilter) -> Result<Vec<Item>> {
    let statuses =
        f.status.iter().map(|s| store::check_choice("status", s, store::STATUSES)).collect::<Result<Vec<_>>>()?;
    let at = f.at.as_deref().map(|a| lk.tree.resolve(a)).transpose()?.map(|id| lk.tree.subtree(id));
    let before = f.warranty_before.as_deref().map(|d| parse_date(d, &ctx.tz)).transpose()?.map(|d| d.to_string());
    let inside = f.inside.as_deref().map(|s| store::resolve_item(db, s)).transpose()?.map(|i| i.id);
    let labels: Vec<String> = f.labels.iter().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()).collect();
    Ok(store::all_items(db)?
        .into_iter()
        .filter(|i| if statuses.is_empty() { f.all || !i.is_gone() } else { statuses.contains(&i.status) })
        .filter(|i| at.as_ref().is_none_or(|ids| i.location_id.is_some_and(|l| ids.contains(&l))))
        .filter(|i| {
            f.owner
                .as_deref()
                .is_none_or(|o| i.owner_entity.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(o.trim())))
        })
        .filter(|i| {
            before
                .as_deref()
                .is_none_or(|b| i.warranty_expires.as_deref().is_some_and(|w| w < b) && !i.lifetime_warranty)
        })
        .filter(|i| inside.as_deref().is_none_or(|p| i.parent_item_id.as_deref() == Some(p)))
        .filter(|i| {
            let have: Vec<String> = lk.labels_of(&i.id).iter().map(|l| l.to_lowercase()).collect();
            labels.iter().all(|l| have.contains(l))
        })
        .collect())
}

fn sort_items(items: &mut [Item], lk: &Lookups, key: ItemSort) {
    let path = |i: &Item| lk.tree.opt_path(i.location_id).unwrap_or_default().to_lowercase();
    items.sort_by(|a, b| {
        let by_name = a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id));
        match key {
            ItemSort::Location => path(a).cmp(&path(b)).then(by_name),
            ItemSort::Name => by_name,
            ItemSort::Value => b.value().cmp(&a.value()).then(by_name),
            ItemSort::Added => a.id.cmp(&b.id),
            ItemSort::Asset => a
                .asset_id
                .as_deref()
                .and_then(store::parse_asset_id)
                .cmp(&b.asset_id.as_deref().and_then(store::parse_asset_id))
                .then(by_name),
        }
    });
}

fn list_report(kind: &'static str, items: &[Item], lk: &Lookups) -> Report {
    let total: i64 = items.iter().filter_map(Item::value).sum();
    Report::list(kind, items.iter().map(|i| item_record(i, lk)).collect())
        .table_columns(ITEM_TABLE)
        .dates(&["purchase_time", "warranty_expires", "valued_at", "sold_time"])
        .meta("total_value", json!(money_value(total)))
}

pub fn ls(ctx: &Ctx, a: LsArgs) -> Result<()> {
    let db = ctx.db()?;
    let lk = Lookups::load(&db)?;
    let mut items = filtered(ctx, &db, &lk, &a.filter)?;
    sort_items(&mut items, &lk, a.sort);
    if a.reverse {
        items.reverse();
    }
    items.truncate(a.limit.unwrap_or(usize::MAX));
    ctx.emit(&list_report("items", &items, &lk))
}

pub fn find(ctx: &Ctx, a: FindArgs) -> Result<()> {
    let db = ctx.db()?;
    let lk = Lookups::load(&db)?;
    let needle = a.text.trim().to_lowercase();
    let mut items: Vec<Item> = filtered(ctx, &db, &lk, &a.filter)?
        .into_iter()
        .filter(|i| {
            [Some(&i.name), i.description.as_ref(), i.serial_number.as_ref(), i.model_number.as_ref(), i.notes.as_ref()]
                .into_iter()
                .flatten()
                .any(|t| t.to_lowercase().contains(&needle))
        })
        .collect();
    sort_items(&mut items, &lk, ItemSort::Location);
    if items.is_empty() {
        ctx.info(&format!("nothing matches '{}'", a.text));
    }
    ctx.emit(&list_report("items", &items, &lk).meta("query", json!(a.text)))
}

pub fn history(ctx: &Ctx, spec: &str) -> Result<()> {
    let db = ctx.db()?;
    let item = store::resolve_item(&db, spec)?;
    let rows = store::events_for(&db, &item.id)?
        .iter()
        .map(|e| to_record(&json!({"at": e.at, "kind": e.kind, "detail": e.detail})))
        .collect();
    ctx.emit(
        &Report::list("history", rows)
            .table_columns(&["at", "kind", "detail"])
            .meta("item", json!({"id": item.id, "asset_id": item.asset_id, "name": item.name})),
    )
}

pub fn value(ctx: &Ctx, a: ValueArgs) -> Result<()> {
    let db = ctx.db()?;
    let before = store::resolve_item(&db, &a.item)?;
    let mut item = before.clone();
    item.replacement_value = Some(crate::util::parse_money(&a.amount)?);
    item.valued_at = Some(crate::util::parse_day(a.on.as_deref().unwrap_or("today"), &[], &ctx.tz)?);
    item.updated_at = crate::util::now_iso();
    db.transaction(|db| {
        store::update_item(db, &item)?;
        if item.same_content(&before) {
            return Ok(());
        }
        store::add_event(
            db,
            &item.id,
            "valued",
            json!({"value": item.replacement_value.map(money_value), "previous": before.replacement_value.map(money_value), "valued_at": item.valued_at, "note": a.note, "via": "value"}),
        )
    })?;
    let lk = Lookups::load(&db)?;
    ctx.info(&format!(
        "{} {} valued at {} on {}",
        item.asset_id.as_deref().unwrap_or(&item.id),
        item.name,
        crate::util::fmt_money(item.replacement_value.unwrap_or_default()),
        item.valued_at.as_deref().unwrap_or_default()
    ));
    ctx.emit_mutation(&Report::object("item", item_record(&item, &lk)))
}
