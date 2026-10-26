//! `loc add|ls|mv|edit|rm`: the location tree.

use std::collections::HashMap;

use serde_json::json;

use crate::cli::LocCmd;
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::output::{to_record, Record, Report};
use crate::store::{self, check_choice, Location, Tree, LOCATION_KINDS};
use crate::util::money_value;

pub fn run(ctx: &Ctx, cmd: LocCmd) -> Result<()> {
    match cmd {
        LocCmd::Add { path, kind, address } => add(ctx, &path, kind.as_deref(), address.as_deref()),
        LocCmd::Ls { path, flat } => ls(ctx, path.as_deref(), flat),
        LocCmd::Mv { path, to, rename } => mv(ctx, &path, to.as_deref(), rename.as_deref()),
        LocCmd::Edit { path, kind, address, notes } => {
            edit(ctx, &path, kind.as_deref(), address.as_deref(), notes.as_deref())
        }
        LocCmd::Rm { path } => rm(ctx, &path),
    }
}

fn record(tree: &Tree, l: &Location) -> Record {
    to_record(&json!({
        "id": l.id,
        "path": tree.path(l.id),
        "name": l.name,
        "kind": l.kind,
        "address": l.address,
        "notes": l.notes,
    }))
}

fn add(ctx: &Ctx, path: &str, kind: Option<&str>, address: Option<&str>) -> Result<()> {
    let db = ctx.db()?;
    let kind = kind.map(|k| check_choice("location kind", k, LOCATION_KINDS)).transpose()?;
    let mut tree = Tree::load(&db)?;
    if let Some(id) = tree.find_path(path) {
        return Err(AppError::invalid(format!("location '{}' already exists", tree.path(id))));
    }
    let id = db.transaction(|db| {
        let (id, _) = tree.ensure_path(db, path, kind.as_deref())?;
        if let Some(a) = address {
            let mut l = tree.get(id).cloned().expect("just created");
            l.address = Some(a.to_string());
            store::update_location(db, &l)?;
        }
        Ok(id)
    })?;
    let tree = Tree::load(&db)?;
    let l = tree.get(id).ok_or_else(|| AppError::db("location vanished"))?;
    ctx.info(&format!("added location {} ({})", tree.path(id), l.kind));
    ctx.emit_mutation(&Report::object("location", record(&tree, l)))
}

/// (item count, value in cents) of the items directly at each location.
type Totals = HashMap<Option<i64>, (i64, i64)>;

/// Totals per location (own items only, on-hand statuses) and overall value.
fn own_totals(db: &Db) -> Result<(Totals, i64)> {
    let mut m = Totals::new();
    let mut total = 0;
    for i in store::all_items(db)?.into_iter().filter(|i| !i.is_gone()) {
        let e = m.entry(i.location_id).or_default();
        e.0 += 1;
        e.1 += i.value().unwrap_or(0);
        total += i.value().unwrap_or(0);
    }
    Ok((m, total))
}

fn ls(ctx: &Ctx, path: Option<&str>, flat: bool) -> Result<()> {
    let db = ctx.db()?;
    let tree = Tree::load(&db)?;
    let (own, total) = own_totals(&db)?;
    let roots: Vec<i64> = match path {
        Some(p) => vec![tree.resolve(p)?],
        None => tree.children(None).iter().map(|l| l.id).collect(),
    };
    let base_depth = roots.first().map_or(0, |r| tree.depth(*r));
    let mut rows = Vec::new();
    fn walk(tree: &Tree, id: i64, out: &mut Vec<i64>) {
        out.push(id);
        tree.children(Some(id)).iter().for_each(|c| walk(tree, c.id, out));
    }
    let mut order = Vec::new();
    roots.iter().for_each(|r| walk(&tree, *r, &mut order));
    for id in order {
        let l = tree.get(id).expect("in tree");
        let sub = tree.subtree(id);
        let (items, value) = sub.iter().fold((0, 0), |acc, s| {
            let (n, v) = own.get(&Some(*s)).copied().unwrap_or_default();
            (acc.0 + n, acc.1 + v)
        });
        let depth = tree.depth(id) - base_depth;
        let mut r = record(&tree, l);
        r.insert("tree".into(), json!(format!("{}{}", "  ".repeat(depth), l.name)));
        r.insert("depth".into(), json!(depth));
        r.insert("items_here".into(), json!(own.get(&Some(id)).map_or(0, |o| o.0)));
        r.insert("items".into(), json!(items));
        r.insert("value".into(), json!(money_value(value)));
        rows.push(r);
    }
    if path.is_none() {
        if let Some((n, v)) = own.get(&None).filter(|o| o.0 > 0) {
            rows.push(to_record(&json!({
                "id": null, "path": "(no location)", "name": "(no location)", "kind": null, "address": null, "notes": null,
                "tree": "(no location)", "depth": 0, "items_here": n, "items": n, "value": money_value(*v),
            })));
        }
    }
    let cols: &[&str] = if flat { &["path", "kind", "items", "value"] } else { &["tree", "kind", "items", "value"] };
    ctx.emit(&Report::list("locations", rows).table_columns(cols).meta("total_value", json!(money_value(total))))
}

fn mv(ctx: &Ctx, path: &str, to: Option<&str>, rename: Option<&str>) -> Result<()> {
    if to.is_none() && rename.is_none() {
        return Err(AppError::usage("give --to PARENT (or / for the top level) and/or --rename NAME"));
    }
    let db = ctx.db()?;
    let tree = Tree::load(&db)?;
    let id = tree.resolve(path)?;
    let mut l = tree.get(id).cloned().expect("resolved");
    if let Some(t) = to {
        l.parent_id = if store::split_path(t).is_empty() { None } else { Some(tree.resolve(t)?) };
        if l.parent_id.is_some_and(|p| tree.subtree(id).contains(&p)) {
            return Err(AppError::invalid(format!("cannot move {} inside itself", tree.path(id))));
        }
    }
    if let Some(n) = rename {
        if n.trim().is_empty() || n.contains('/') {
            return Err(AppError::invalid(format!("invalid location name '{n}'")));
        }
        l.name = n.trim().to_string();
    }
    if tree.locs.iter().any(|o| o.id != id && o.parent_id == l.parent_id && o.name.eq_ignore_ascii_case(&l.name)) {
        return Err(AppError::invalid(format!("a location named '{}' already exists there", l.name)));
    }
    let from = tree.path(id);
    store::update_location(&db, &l)?;
    let tree = Tree::load(&db)?;
    ctx.info(&format!("moved {from} -> {}", tree.path(id)));
    ctx.emit_mutation(&Report::object("location", record(&tree, &l)))
}

fn edit(ctx: &Ctx, path: &str, kind: Option<&str>, address: Option<&str>, notes: Option<&str>) -> Result<()> {
    let db = ctx.db()?;
    let tree = Tree::load(&db)?;
    let id = tree.resolve(path)?;
    let mut l = tree.get(id).cloned().expect("resolved");
    if let Some(k) = kind {
        l.kind = check_choice("location kind", k, LOCATION_KINDS)?;
    }
    let opt = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    if let Some(a) = address {
        l.address = opt(a);
    }
    if let Some(n) = notes {
        l.notes = opt(n);
    }
    store::update_location(&db, &l)?;
    ctx.info(&format!("updated location {}", tree.path(id)));
    ctx.emit_mutation(&Report::object("location", record(&tree, &l)))
}

fn rm(ctx: &Ctx, path: &str) -> Result<()> {
    let db = ctx.db()?;
    let tree = Tree::load(&db)?;
    let id = tree.resolve(path)?;
    let p = tree.path(id);
    if !tree.children(Some(id)).is_empty() {
        return Err(AppError::invalid(format!("{p} has sub-locations; move or remove them first")));
    }
    let items = store::all_items(&db)?.iter().filter(|i| i.location_id == Some(id)).count();
    let lots = store::all_lots(&db)?.iter().filter(|l| l.location_id == Some(id)).count();
    if items + lots > 0 {
        return Err(AppError::invalid(format!(
            "{p} still holds {items} item(s) and {lots} lot(s); move them first (wherebox mv ... --to ...)"
        )));
    }
    store::delete_location(&db, id)?;
    ctx.info(&format!("removed location {p}"));
    ctx.emit_mutation(&Report::object("removed", to_record(&json!({"location": p}))))
}
