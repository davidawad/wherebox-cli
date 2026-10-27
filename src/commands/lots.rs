//! Consumables: `lot add|ls|edit|rm|confirm`, `use`, `eat`, `toss` and
//! `expiring`.
//!
//! A lot is one purchase of a consumable with its own quantity, dates and
//! place (fridge, freezer, pantry shelf). Consumption is logged against the
//! oldest lot first (see [`Lot::fifo_key`]); an item that tracks lots keeps
//! `quantity` equal to what is logged on hand. Expiry is computed (see
//! [`crate::shelf_life`]) and on hand estimated (see [`crate::onhand`]).

use serde_json::json;

use crate::cli::{ConsumeArgs, ExpiringArgs, LotCmd, LotFlags, TossArgs};
use crate::commands::items::lot_record;
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::fields::Parse;
use crate::onhand::{Estimates, Estimator};
use crate::output::{to_record, Record, Report};
use crate::shelf_life::Storage;
use crate::store::{self, check_choice, Item, Lot, Tree, LOT_STATUSES};
use crate::util::{fmt_qty, parse_day, parse_duration_days, parse_qty};

/// Quantities are rounded to a millionth so 0.1 + 0.2 leaves no residue.
fn round(q: f64) -> f64 {
    (q * 1e6).round() / 1e6
}

fn today(ctx: &Ctx) -> String {
    ctx.tz.today().format("%Y-%m-%d").to_string()
}

/// A new lot from `--best-by`/`--opened`/... flags (acquired today by default).
pub fn new_lot(item_id: &str, qty: f64, d: &LotFlags, location_id: Option<i64>, p: &Parse, today: &str) -> Result<Lot> {
    let day = |v: &Option<String>| v.as_deref().map(|s| p_day(p, s)).transpose();
    let opened_at = day(&d.opened)?;
    Ok(Lot {
        id: crate::ulid::new()?,
        item_id: item_id.to_string(),
        qty: round(qty),
        acquired_at: Some(day(&d.acquired)?.unwrap_or_else(|| today.to_string())),
        status: if opened_at.is_some() { "open".into() } else { "sealed".into() },
        opened_at,
        best_by: day(&d.best_by)?.filter(|s| !s.is_empty()),
        frozen_at: day(&d.frozen)?,
        location_id,
        created_at: crate::util::now_iso(),
        unit_price: None,
        source_ref: None,
        confirmed_at: None,
        vendor: None,
    })
}

/// The shelf-life type of an item (`None`: non-perishable).
pub fn shelf_type_of(db: &Db, est: &Estimator, item: &Item, has_lots: bool) -> Result<Option<String>> {
    let fields = store::item_fields(db, &item.id)?;
    let labels = store::item_labels(db, &item.id)?;
    let facts = crate::shelf_life::ItemFacts { item, fields: &fields, labels: &labels, has_lots, food_place: false };
    Ok(est.types.classify(&facts, &est.catalog).map(|c| c.ty.name.clone()))
}

/// A printed best-by only goes on perishables: non-perishables never expire.
pub fn check_best_by(item: &Item, lot: &Lot, ty: Option<&str>) -> Result<()> {
    if lot.best_by.is_some() && ty.is_none() {
        return Err(AppError::invalid(format!(
            "{} is not perishable (no shelf-life type), so it has no expiry; \
             give it a type with `wherebox edit {:?} --shelf-life TYPE` (see `wherebox shelf-life ls`)",
            item.name, item.name
        )));
    }
    Ok(())
}

/// The storage a lot is kept in (its location, else its item's).
fn storage_of(tree: &Tree, lot: &Lot, item: &Item) -> Storage {
    Storage::of_location(tree, lot.location_id.or(item.location_id)).unwrap_or(Storage::Pantry)
}

/// Moving into a freezer starts the freezer clock (re-freezing re-bases it).
pub fn refreeze(lot: &mut Lot, before: Storage, after: Storage, today: &str) {
    if after == Storage::Freezer && before != Storage::Freezer {
        lot.frozen_at = Some(today.to_string());
    }
}

fn p_day(p: &Parse, s: &str) -> Result<String> {
    let fmts: Vec<&str> = p.date_formats.iter().map(String::as_str).collect();
    parse_day(s, &fmts, p.tz)
}

pub fn run(ctx: &Ctx, cmd: LotCmd) -> Result<()> {
    match cmd {
        LotCmd::Add { item, qty, at, vendor, dates } => add(ctx, &item, &qty, at.as_deref(), vendor.as_deref(), &dates),
        LotCmd::Ls { item, all } => ls(ctx, item.as_deref(), all),
        LotCmd::Edit { lot, qty, at, status, vendor, dates } => {
            edit(ctx, &lot, qty.as_deref(), at.as_deref(), status.as_deref(), vendor.as_deref(), &dates)
        }
        LotCmd::Rm { lot } => rm(ctx, &lot),
        LotCmd::Confirm { item, qty, lot, on } => confirm(ctx, &item, qty.as_deref(), lot.as_deref(), on.as_deref()),
    }
}

fn record(l: &Lot, item: &Item, tree: &Tree, est: &Estimates) -> Record {
    let mut r = lot_record(l, item.unit.as_deref(), tree, est.lot(l));
    r.insert("item".into(), json!(item.name));
    r.insert("asset_id".into(), json!(item.asset_id));
    r.insert("item_id".into(), json!(item.id));
    if r.get("location").is_none_or(serde_json::Value::is_null) {
        r.insert("location".into(), json!(tree.opt_path(item.location_id)));
    }
    r
}

const LOT_TABLE: &[&str] =
    &["id", "item", "qty", "unit", "status", "acquired_at", "expires_at", "expiry_basis", "used_up", "location"];

fn add(ctx: &Ctx, item: &str, qty: &str, at: Option<&str>, vendor: Option<&str>, dates: &LotFlags) -> Result<()> {
    let db = ctx.db()?;
    let fmts = ctx.input_date_formats();
    let p = Parse { tz: &ctx.tz, date_formats: &fmts };
    let item = store::resolve_item(&db, item)?;
    let est = Estimator::for_ctx(ctx)?;
    let ty = shelf_type_of(&db, &est, &item, true)?;
    let mut tree = Tree::load(&db)?;
    let lot = db.transaction(|db| {
        let loc = at.map(|a| tree.resolve_or_create(db, a).map(|r| r.0)).transpose()?;
        let mut lot =
            new_lot(&item.id, parse_qty(qty)?, dates, loc.filter(|l| Some(*l) != item.location_id), &p, &today(ctx))?;
        lot.vendor = vendor.map(|v| crate::sourcing::vendor_ref(db, v)).transpose()?.flatten();
        check_best_by(&item, &lot, ty.as_deref())?;
        store::insert_lot(db, &lot)?;
        store::sync_lot_quantity(db, &item.id)?;
        store::add_event(db, &item.id, "lot_added", json!({"lot": lot.id, "qty": lot.qty, "best_by": lot.best_by}))?;
        Ok(lot)
    })?;
    let views = est.all(&db)?;
    let expiry = views.lot(&lot).and_then(|v| v.expiry.clone());
    ctx.info(&format!(
        "added a lot of {} ({} {}{})",
        item.name,
        fmt_qty(lot.qty),
        item.unit.as_deref().unwrap_or("ea"),
        expiry.as_ref().map(|e| format!(", expires {}", e.date)).unwrap_or_default()
    ));
    if let Some(e) = expiry.filter(|e| e.basis == "shelf_life") {
        ctx.info(&e.detail);
    }
    ctx.emit_mutation(&Report::object("lot", record(&lot, &item, &tree, &views)))
}

fn ls(ctx: &Ctx, item: Option<&str>, all: bool) -> Result<()> {
    let db = ctx.db()?;
    let tree = Tree::load(&db)?;
    let items = store::all_items(&db)?;
    let est = Estimator::for_ctx(ctx)?.all(&db)?;
    let only = item.map(|i| store::resolve_item(&db, i)).transpose()?;
    let rows = store::all_lots(&db)?
        .iter()
        .filter(|l| all || l.is_active())
        .filter(|l| only.as_ref().is_none_or(|i| i.id == l.item_id))
        .filter_map(|l| items.iter().find(|i| i.id == l.item_id).map(|i| record(l, i, &tree, &est)))
        .collect();
    ctx.emit(&Report::list("lots", rows).table_columns(LOT_TABLE))
}

fn edit(
    ctx: &Ctx,
    lot: &str,
    qty: Option<&str>,
    at: Option<&str>,
    status: Option<&str>,
    vendor: Option<&str>,
    d: &LotFlags,
) -> Result<()> {
    let db = ctx.db()?;
    let fmts = ctx.input_date_formats();
    let p = Parse { tz: &ctx.tz, date_formats: &fmts };
    let mut l = store::resolve_lot(&db, lot)?;
    let item = store::get_item(&db, &l.item_id)?.ok_or_else(|| AppError::db("lot without item"))?;
    if let Some(q) = qty {
        l.qty = round(parse_qty(q)?);
    }
    if let Some(s) = status {
        l.status = check_choice("lot status", s, LOT_STATUSES)?;
    }
    if let Some(v) = vendor {
        l.vendor = crate::sourcing::vendor_ref(&db, v)?;
    }
    let set = |slot: &mut Option<String>, v: &Option<String>| -> Result<()> {
        if let Some(v) = v {
            *slot = if v.trim().is_empty() { None } else { Some(p_day(&p, v)?) };
        }
        Ok(())
    };
    set(&mut l.acquired_at, &d.acquired)?;
    set(&mut l.opened_at, &d.opened)?;
    set(&mut l.best_by, &d.best_by)?;
    set(&mut l.frozen_at, &d.frozen)?;
    if d.opened.is_some() && l.status == "sealed" {
        l.status = "open".into();
    }
    let est = Estimator::for_ctx(ctx)?;
    let ty = shelf_type_of(&db, &est, &item, true)?;
    check_best_by(&item, &l, ty.as_deref())?;
    let mut tree = Tree::load(&db)?;
    let today = today(ctx);
    db.transaction(|db| {
        if let Some(a) = at {
            let before = storage_of(&tree, &l, &item);
            l.location_id = Some(tree.resolve_or_create(db, a)?.0).filter(|x| Some(*x) != item.location_id);
            let after = storage_of(&tree, &l, &item);
            if d.frozen.is_none() {
                refreeze(&mut l, before, after, &today);
            }
        }
        store::update_lot(db, &l)?;
        store::sync_lot_quantity(db, &item.id).map(|_| ())
    })?;
    let views = est.all(&db)?;
    ctx.info(&format!(
        "updated lot {}{}",
        l.id,
        views.lot(&l).and_then(|v| v.expiry.as_ref()).map(|e| format!(" (expires {})", e.date)).unwrap_or_default()
    ));
    ctx.emit_mutation(&Report::object("lot", record(&l, &item, &tree, &views)))
}

fn rm(ctx: &Ctx, lot: &str) -> Result<()> {
    let db = ctx.db()?;
    let l = store::resolve_lot(&db, lot)?;
    db.transaction(|db| {
        db.execute("DELETE FROM consumption WHERE lot_id = ?1", &[crate::db::text(&l.id)])?;
        db.execute("DELETE FROM lots WHERE id = ?1", &[crate::db::text(&l.id)])?;
        store::sync_lot_quantity(db, &l.item_id)?;
        store::add_event(db, &l.item_id, "lot_removed", json!({"lot": l.id}))
    })?;
    ctx.info(&format!("removed lot {}", l.id));
    ctx.emit_mutation(&Report::object("removed", to_record(&json!({"lot": l.id}))))
}

/// One step of a consumption: how much came out of which lot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Taken {
    pub lot: String,
    pub taken: f64,
    pub left: f64,
    pub status: String,
}

/// Take `qty` from `lots` (already in FIFO order) and log it. Lots that run
/// out become `consumed` (or `discarded` when tossed); a partly used sealed
/// lot becomes `open`.
pub fn take_fifo(db: &Db, lots: &[Lot], qty: f64, reason: &str, at: &str, note: Option<&str>) -> Result<Vec<Taken>> {
    let on_hand = round(lots.iter().map(|l| l.qty).sum());
    if qty <= 0.0 {
        return Err(AppError::invalid("quantity must be positive"));
    }
    if round(qty) > on_hand {
        return Err(AppError::invalid(format!("only {} on hand", fmt_qty(on_hand))));
    }
    let gone_status = if matches!(reason, "eaten" | "used") { "consumed" } else { "discarded" };
    let mut remaining = round(qty);
    let mut out = Vec::new();
    for lot in lots {
        if remaining <= 0.0 {
            break;
        }
        let take = remaining.min(lot.qty);
        if take <= 0.0 {
            continue;
        }
        remaining = round(remaining - take);
        let mut l = lot.clone();
        l.qty = round(l.qty - take);
        if l.qty <= 0.0 {
            l.qty = 0.0;
            l.status = gone_status.into();
        } else if l.status == "sealed" && gone_status == "consumed" {
            l.status = "open".into();
            l.opened_at = l.opened_at.or_else(|| Some(at.get(..10).unwrap_or(at).to_string()));
        }
        store::insert_consumption(db, &l.id, take, at, reason, note)?;
        store::update_lot(db, &l)?;
        out.push(Taken { lot: l.id.clone(), taken: take, left: l.qty, status: l.status.clone() });
    }
    Ok(out)
}

struct Request<'a> {
    item: &'a str,
    qty: Option<&'a str>,
    lot: Option<&'a str>,
    on: Option<&'a str>,
    note: Option<&'a str>,
    reason: &'a str,
    /// `toss` without a quantity takes the whole first lot.
    whole_lot: bool,
}

fn consume_inner(ctx: &Ctx, r: Request) -> Result<()> {
    let db = ctx.db()?;
    let item = store::resolve_item(&db, r.item)?;
    let at = crate::util::parse_when(r.on.unwrap_or("now"), &[], &ctx.tz)?;
    let all = store::lots_for(&db, &item.id)?;
    let mut lots: Vec<Lot> = all.iter().filter(|l| l.is_active() && l.qty > 0.0).cloned().collect();
    // Lots presumed used up go last: a use comes out of what is still there.
    let est = Estimator::for_ctx(ctx)?.all(&db)?;
    lots.sort_by_key(|l| est.lot(l).is_some_and(|v| v.used_up));
    if let Some(spec) = r.lot {
        let want = store::resolve_lot(&db, spec)?;
        if want.item_id != item.id {
            return Err(AppError::invalid(format!("lot {} belongs to another item", want.id)));
        }
        lots.retain(|l| l.id == want.id);
        if lots.is_empty() {
            return Err(AppError::invalid(format!("lot {} is used up", want.id)));
        }
    }
    let unit = item.unit.clone().unwrap_or_else(|| "ea".into());
    if all.is_empty() {
        // An item without lots: count down its quantity.
        let q = r.qty.map(parse_qty).transpose()?.unwrap_or(1.0);
        if q.fract() != 0.0 || q <= 0.0 {
            return Err(AppError::invalid("items without lots are used in whole numbers"));
        }
        if q as i64 > item.quantity {
            return Err(AppError::invalid(format!("only {} of {} on hand", item.quantity, item.name)));
        }
        let mut after = item.clone();
        after.quantity -= q as i64;
        after.updated_at = crate::util::now_iso();
        db.transaction(|db| {
            store::update_item(db, &after)?;
            store::add_event(
                db,
                &item.id,
                r.reason,
                json!({"qty": q, "left": after.quantity, "at": at, "note": r.note}),
            )
        })?;
        ctx.info(&format!("{} {} {} of {} ({} left)", r.reason, fmt_qty(q), unit, item.name, after.quantity));
        return ctx.emit_mutation(&Report::object(
            "consumption",
            to_record(&json!({"item": item.name, "item_id": item.id, "reason": r.reason, "qty": q, "on_hand": after.quantity, "lots": []})),
        ));
    }
    if lots.is_empty() {
        return Err(AppError::invalid(format!("no {} left (all lots used up)", item.name)));
    }
    let qty = match (r.qty, r.whole_lot) {
        (Some(q), _) => parse_qty(q)?,
        (None, true) => lots[0].qty,
        (None, false) => 1.0,
    };
    let taken = db.transaction(|db| {
        let t = take_fifo(db, &lots, qty, r.reason, &at, r.note)?;
        store::sync_lot_quantity(db, &item.id)?;
        store::add_event(
            db,
            &item.id,
            r.reason,
            json!({"qty": qty, "at": at, "lots": t.iter().map(|x| &x.lot).collect::<Vec<_>>(), "note": r.note}),
        )?;
        Ok(t)
    })?;
    let on_hand = round(store::lots_for(&db, &item.id)?.iter().filter(|l| l.is_active()).map(|l| l.qty).sum());
    ctx.info(&format!("{} {} {} of {} ({} {} left)", r.reason, fmt_qty(qty), unit, item.name, fmt_qty(on_hand), unit));
    ctx.emit_mutation(&Report::object(
        "consumption",
        to_record(&json!({"item": item.name, "item_id": item.id, "reason": r.reason, "qty": qty, "unit": unit, "on_hand": on_hand, "lots": taken})),
    ))
}

pub fn consume(ctx: &Ctx, a: ConsumeArgs, reason: &str) -> Result<()> {
    consume_inner(
        ctx,
        Request {
            item: &a.item,
            qty: a.qty.as_deref(),
            lot: a.lot.as_deref(),
            on: a.on.as_deref(),
            note: a.note.as_deref(),
            reason,
            whole_lot: false,
        },
    )
}

pub fn toss(ctx: &Ctx, a: TossArgs) -> Result<()> {
    consume_inner(
        ctx,
        Request {
            item: &a.item,
            qty: a.qty.as_deref(),
            lot: a.lot.as_deref(),
            on: a.on.as_deref(),
            note: a.note.as_deref(),
            reason: if a.spoiled { "spoiled" } else { "discarded" },
            whole_lot: true,
        },
    )
}

/// `lot confirm`: the item's active lots (or `--lot`) are on hand today;
/// with `--qty`, only that much (the newest lots first; the rest is logged
/// as used).
fn confirm(ctx: &Ctx, item: &str, qty: Option<&str>, lot: Option<&str>, on: Option<&str>) -> Result<()> {
    let db = ctx.db()?;
    let item = store::resolve_item(&db, item)?;
    let day = crate::util::parse_day(on.unwrap_or("today"), &[], &ctx.tz)?;
    let mut lots: Vec<Lot> =
        store::lots_for(&db, &item.id)?.into_iter().filter(|l| l.is_active() && l.qty > 0.0).collect();
    if let Some(spec) = lot {
        let want = store::resolve_lot(&db, spec)?;
        if want.item_id != item.id {
            return Err(AppError::invalid(format!("lot {} belongs to another item", want.id)));
        }
        lots.retain(|l| l.id == want.id);
    }
    if lots.is_empty() {
        return Err(AppError::invalid(format!(
            "{} has no lots on hand to confirm (add one with `wherebox lot add`)",
            item.name
        )));
    }
    let logged = round(lots.iter().map(|l| l.qty).sum());
    let keep = match qty.map(parse_qty).transpose()? {
        Some(q) if q < 0.0 => return Err(AppError::invalid("quantity cannot be negative")),
        Some(q) if round(q) > logged => {
            return Err(AppError::invalid(format!(
                "only {} logged on hand; add the rest with `wherebox lot add {:?} {}`",
                fmt_qty(logged),
                item.name,
                fmt_qty(round(q - logged))
            )))
        }
        Some(q) => round(q),
        None => logged,
    };
    // Newest first: what is left of a purchase is most likely the latest one.
    lots.reverse();
    let at = format!("{day}T00:00:00Z");
    db.transaction(|db| {
        let mut left = keep;
        for l in &lots {
            let mut l = l.clone();
            let stays = left.min(l.qty);
            left = round(left - stays);
            let gone = round(l.qty - stays);
            if gone > 0.0 {
                store::insert_consumption(db, &l.id, gone, &at, "used", Some("not found at lot confirm"))?;
                l.qty = stays;
                if l.qty <= 0.0 {
                    l.qty = 0.0;
                    l.status = "consumed".into();
                }
            }
            if l.qty > 0.0 {
                l.confirmed_at = Some(day.clone());
            }
            store::update_lot(db, &l)?;
        }
        store::sync_lot_quantity(db, &item.id)?;
        store::add_event(db, &item.id, "confirmed", json!({"on": day, "qty": keep, "logged": logged}))
    })?;
    let est = Estimator::for_ctx(ctx)?.all(&db)?;
    let view = est.get(&item.id).cloned().unwrap_or_default();
    let unit = item.unit.as_deref().unwrap_or("ea");
    ctx.info(&format!("{}: {} {unit} on hand on {day}", item.name, fmt_qty(keep)));
    let mut r =
        to_record(&json!({"item": item.name, "item_id": item.id, "confirmed_on": day, "qty": keep, "unit": unit}));
    for (k, v) in view.fields() {
        r.insert(k.into(), v);
    }
    ctx.emit_mutation(&Report::object("confirmed", r))
}

/// `expiring`: perishable lots whose computed expiry falls within the window,
/// and lots that expired within the same window back (older ones are
/// presumed used up, unless confirmed after their expiry).
pub fn expiring(ctx: &Ctx, a: ExpiringArgs) -> Result<()> {
    let db = ctx.db()?;
    let within = a.within.as_deref().unwrap_or(ctx.expiring_within());
    let days = parse_duration_days(within)?.round() as i64;
    let tree = Tree::load(&db)?;
    let under = a.at.as_deref().map(|p| tree.resolve(p)).transpose()?.map(|id| tree.subtree(id));
    let items = store::all_items(&db)?;
    let estimator = Estimator::for_ctx(ctx)?;
    let est = estimator.all(&db)?;
    // A preset's alert lead time widens the window for its items, unless
    // --within was given explicitly.
    let lead = |i: &Item| {
        let alert = i.kind_preset.as_deref().and_then(|n| estimator.catalog.get(n)).and_then(|p| p.alert_days);
        match (a.within.is_some(), alert) {
            (false, Some(d)) => days.max(i64::from(d)),
            _ => days,
        }
    };
    let today = estimator.today;
    let mut rows: Vec<(String, Record)> = store::all_lots(&db)?
        .iter()
        .filter(|l| l.is_active() && l.qty > 0.0)
        .filter_map(|l| {
            let item = items.iter().find(|i| i.id == l.item_id && !i.is_gone())?;
            let view = est.get(&item.id)?;
            let lv = view.lot(&l.id)?;
            let e = lv.expiry.as_ref()?;
            let left = (e.date - today).num_days();
            let loc = l.location_id.or(item.location_id);
            let recent = left >= -days || lv.expired;
            if left > lead(item) || !recent || under.as_ref().is_some_and(|u| loc.is_none_or(|x| !u.contains(&x))) {
                return None;
            }
            let rec = to_record(&json!({
                "item": item.name,
                "asset_id": item.asset_id,
                "lot": l.id,
                "qty": l.qty,
                "unit": item.unit.as_deref().unwrap_or("ea"),
                "due": e.date.to_string(),
                "expires_at": e.date.to_string(),
                "expiry_basis": e.basis,
                "expiry_detail": e.detail,
                "shelf_life": view.shelf_life.as_ref().map(|s| &s.0),
                "days_left": left,
                "state": if left < 0 { "expired" } else { "soon" },
                "on_hand_basis": if lv.used_up { "estimated" } else { "logged" },
                "used_up": lv.used_up,
                "storage": lv.storage.as_str(),
                "frozen_at": l.frozen_at,
                "location": tree.opt_path(loc),
            }));
            Some((format!("{}{}", e.date, l.id), rec))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    if rows.is_empty() {
        ctx.info(&format!("nothing expires within {within}"));
    }
    ctx.emit(
        &Report::list("expiring", rows.into_iter().map(|r| r.1).collect())
            .table_columns(&["due", "days_left", "state", "item", "qty", "unit", "location", "expiry_basis"])
            .dates(&["due", "expires_at", "frozen_at"])
            .meta("within_days", json!(days)),
    )
}
