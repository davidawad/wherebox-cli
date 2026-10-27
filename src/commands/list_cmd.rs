//! `list`, `cohort` and `match`: lists checked against a property with the
//! matching ladder (docs/SPEC.md, "Lists and matching"; the engine is in
//! [`crate::lists`]).

use serde_json::{json, Value};
use toml_edit::{value, Table};

use crate::cli::{CohortCmd, EntryFlags, ListCmd, MatchArgs, MatcherFlags};
use crate::commands::Status as ExitStatus;
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::lists::{self, CohortFile, Doc, Entry, EntryCheck, Inventory, Kind, ListFile, Matcher, Resolved, Status};
use crate::output::{to_record, Format, Record, Report};
use crate::profiles::Profile;
use crate::util::fmt_qty;

/// Exit status of `list check` when the list is not ready.
pub const NOT_READY: ExitStatus = 3;

pub fn run(ctx: &Ctx, cmd: ListCmd) -> Result<ExitStatus> {
    match cmd {
        ListCmd::Check { list, scope, threshold } => {
            return check(ctx, &list, scope.all_profiles, threshold.as_deref())
        }
        ListCmd::New { name, kind, notes } => new(ctx, &name, &kind, notes.as_deref()),
        ListCmd::Ls => ls(ctx),
        ListCmd::Show { list } => show(ctx, &list),
        ListCmd::Rm { list } => rm(ctx, Kind::List, &list),
        ListCmd::Add { list, entry, want, m } => add(ctx, &list, &entry, &want, &m),
        ListCmd::Edit { list, entry, name, required, clear, want, m } => {
            edit(ctx, &list, &entry, name.as_deref(), required, &clear, &want, &m)
        }
        ListCmd::Drop { list, entry } => drop_entry(ctx, &list, &entry),
        ListCmd::Shop { list, scope } => shop(ctx, &list, scope.all_profiles),
        ListCmd::Pin { list, entry, item } => pin(ctx, &list, &entry, &item),
        ListCmd::Unpin { list, entry, pin, all } => unpin(ctx, &list, &entry, pin.as_deref(), all),
        ListCmd::Explain { list, entry, threshold } => explain(ctx, &list, &entry, threshold.as_deref()),
        ListCmd::Import { list, file, kind } => import(ctx, &list, &file, &kind),
    }
    .map(|()| 0)
}

fn config(ctx: &Ctx) -> &std::path::Path {
    &ctx.resolved.config_path
}

fn load_list(ctx: &Ctx, spec: &str) -> Result<Doc<ListFile>> {
    Doc::load(&lists::find(config(ctx), Kind::List, spec)?)
}

fn load_cohort(ctx: &Ctx, spec: &str) -> Result<Doc<CohortFile>> {
    Doc::load(&lists::find(config(ctx), Kind::Cohort, spec)?)
}

/// The fuzzy threshold: `--threshold`, else the `match_threshold` setting.
fn threshold(ctx: &Ctx, flag: Option<&str>) -> Result<f64> {
    let raw = flag.unwrap_or_else(|| ctx.resolved.get("match_threshold"));
    match raw.trim().parse::<f64>() {
        Ok(t) if (0.0..=1.0).contains(&t) => Ok(t),
        _ if raw.trim().is_empty() => Ok(crate::matching::DEFAULT_THRESHOLD),
        _ => Err(AppError::usage(format!("threshold '{raw}' is not a number from 0 to 1"))),
    }
}

fn parse_bool_flag(s: &str) -> Result<bool> {
    crate::util::parse_bool(s)?.ok_or_else(|| AppError::usage(format!("expected true or false, not '{s}'")))
}

/// Quantities such as `2`, `0.5`, `1/2`, `1 1/2`, `½`.
pub fn parse_amount(s: &str) -> Result<f64> {
    lists::parse_amount(s).ok_or_else(|| AppError::invalid(format!("cannot parse quantity '{s}'")))
}

/// Run `f` on the database the list is checked against: every profile with
/// `all`, else `-P`/`--db`, else the list's `profile`, else the one in use.
fn on_list_db<T>(
    ctx: &Ctx,
    list: &ListFile,
    all: bool,
    command: &str,
    mut f: impl FnMut(&Ctx, Option<&Profile>, &Db) -> Result<T>,
) -> Result<Vec<T>> {
    use crate::config::Source;
    let explicit = ctx.resolved.source("profile") == Source::Flag || ctx.resolved.source("db_path") == Source::Flag;
    match (&list.profile, all || explicit) {
        (Some(p), false) => {
            let p = crate::profiles::get(config(ctx), p)?;
            Ok(vec![crate::commands::profile::on_profile(ctx, &p, command, f)?])
        }
        _ => crate::commands::profile::each_db(ctx, all, command, |c, p, db| f(c, p, db)),
    }
}

/// A profile's inventory with estimated on hand (see [`crate::onhand`]).
fn inventory(ctx: &Ctx, p: Option<&Profile>, db: &Db) -> Result<Inventory> {
    let mut inv = Inventory::load(db, p.map(|p| p.name.clone()), ctx.tz.today())?;
    let cfg = crate::onhand::Config::load(ctx)?;
    let e = crate::onhand::Estimator::load(&cfg, db, inv.today)?;
    inv.est = inv.items.iter().zip(&inv.lots).map(|(i, l)| e.item(i, l)).collect();
    Ok(inv)
}

/// `2`, or `~2` when it rests on an estimate.
fn have_text(q: f64, estimated: bool) -> String {
    format!("{}{}", if estimated { "~" } else { "" }, fmt_qty(round(q)))
}

fn resolve_entry(ctx: &Ctx, e: &Entry) -> Result<Resolved> {
    lists::resolve_files(config(ctx), &e.m, e.fuzzy().then(|| e.name.clone()), None)
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// One-line summary of a matcher: `asin B0..; cohort nuts; any pecans|almonds`.
pub fn matcher_summary(m: &Matcher, own_name: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(n) = own_name {
        parts.push(format!("name \"{n}\""));
    }
    for (k, v) in [
        ("asin", &m.asin),
        ("sku", &m.sku),
        ("upc", &m.upc),
        ("product_ref", &m.product_ref),
        ("url", &m.url),
        ("any", &m.any),
        ("label", &m.label),
        ("preset", &m.preset),
        ("at", &m.at),
        ("cohort", &m.cohort),
        ("exclude", &m.exclude),
    ] {
        if !v.is_empty() {
            parts.push(format!("{k} {}", v.join("|")));
        }
    }
    if !m.pins.is_empty() {
        parts.push(format!("{} pin(s)", m.pins.len()));
    }
    if let Some(t) = m.threshold {
        parts.push(format!("threshold {t}"));
    }
    parts.join("; ")
}

fn matcher_json(m: &Matcher) -> Value {
    json!({
        "asin": m.asin, "sku": m.sku, "upc": m.upc, "product_ref": m.product_ref, "url": m.url,
        "any": m.any, "label": m.label, "preset": m.preset, "at": m.at, "cohort": m.cohort,
        "exclude": m.exclude, "pins": m.pins, "fuzzy": m.fuzzy, "threshold": m.threshold,
    })
}

fn entry_record(i: usize, e: &Entry) -> Record {
    let mut r = to_record(&json!({
        "n": i + 1,
        "name": e.name,
        "qty": e.want(),
        "unit": e.unit(),
        "optional": e.optional,
        "note": e.note,
        "matcher": matcher_summary(&e.m, e.fuzzy().then_some(e.name.as_str())),
    }));
    if let Value::Object(m) = matcher_json(&e.m) {
        r.extend(m);
    }
    r
}

fn list_header(d: &Doc<ListFile>) -> Value {
    json!({
        "slug": d.slug,
        "name": d.data.name,
        "type": d.data.kind,
        "profile": d.data.profile,
        "notes": d.data.notes,
        "threshold": d.data.threshold,
        "entries": d.data.entries.len(),
        "path": d.path,
    })
}

// ---------------------------------------------------------------------------
// Files: new, ls, show, rm
// ---------------------------------------------------------------------------

fn new(ctx: &Ctx, name: &str, kind: &str, notes: Option<&str>) -> Result<()> {
    let kind = kind.trim().to_lowercase();
    if kind.is_empty() {
        return Err(AppError::usage("--type must not be empty"));
    }
    let mut fields = vec![("type", value(kind.as_str()))];
    let profile =
        (ctx.resolved.source("profile") == crate::config::Source::Flag).then(|| ctx.profile.clone()).flatten();
    if let Some(p) = &profile {
        fields.push(("profile", value(p.name.as_str())));
    }
    if let Some(n) = notes.filter(|n| !n.trim().is_empty()) {
        fields.push(("notes", value(n.trim())));
    }
    let path = lists::create(config(ctx), Kind::List, name, &fields)?;
    let d: Doc<ListFile> = Doc::load(&path)?;
    let custom = if lists::TYPES.contains(&kind.as_str()) { "" } else { ", a custom type: checked like a recipe" };
    ctx.info(&format!("created list {} ({kind}{custom}) at {}", d.slug, path.display()));
    ctx.emit_mutation(&Report::object("list", to_record(&list_header(&d))))
}

fn ls(ctx: &Ctx) -> Result<()> {
    let mut rows = Vec::new();
    for p in lists::files(config(ctx), Kind::List) {
        let d: Doc<ListFile> = Doc::load(&p)?;
        let mut r = to_record(&list_header(&d));
        let (mut open, mut done) = (Value::Null, Value::Null);
        if d.data.is_wishlist() {
            match wishlist_counts(ctx, &d.data) {
                Ok((o, f)) => (open, done) = (json!(o), json!(f)),
                Err(e) => ctx.verbose(&format!("{}: open/fulfilled unknown: {}", d.slug, e.message)),
            }
        }
        r.insert("open".into(), open);
        r.insert("fulfilled".into(), done);
        rows.push(r);
    }
    if rows.is_empty() {
        ctx.info("no lists yet (see `wherebox list new`)");
    }
    ctx.emit(&Report::list("lists", rows).table_columns(&[
        "slug",
        "name",
        "type",
        "profile",
        "entries",
        "open",
        "fulfilled",
    ]))
}

fn wishlist_counts(ctx: &Ctx, list: &ListFile) -> Result<(usize, usize)> {
    let t = threshold(ctx, None)?;
    let checks = on_list_db(ctx, list, false, "list ls", |c, p, db| check_all(c, p, db, list, t))?;
    let checks = checks.into_iter().next().unwrap_or_default();
    let done = checks.iter().filter(|c| c.status == Status::Fulfilled).count();
    Ok((checks.len() - done, done))
}

fn show(ctx: &Ctx, spec: &str) -> Result<()> {
    let d = load_list(ctx, spec)?;
    let rows = d.data.entries.iter().enumerate().map(|(i, e)| entry_record(i, e)).collect();
    let header = list_header(&d);
    ctx.info(&format!(
        "{} ({}){}",
        d.data.name,
        d.data.kind,
        d.data.profile.as_ref().map(|p| format!(", profile {p}")).unwrap_or_default()
    ));
    ctx.emit(
        &Report::list("list", rows)
            .table_columns(&["n", "name", "qty", "unit", "optional", "matcher", "note"])
            .meta("list", header),
    )
}

fn rm(ctx: &Ctx, kind: Kind, spec: &str) -> Result<()> {
    let path = lists::find(config(ctx), kind, spec)?;
    let dest = lists::trash(&path)?;
    ctx.info(&format!("moved {} to {}", path.display(), dest.display()));
    ctx.emit_mutation(&Report::object("removed", to_record(&json!({"path": path, "trashed_to": dest}))))
}

// ---------------------------------------------------------------------------
// Entries: add, edit, drop
// ---------------------------------------------------------------------------

/// Matcher flags as (key, values) pairs, in file order.
fn flag_values(m: &MatcherFlags) -> Vec<(&'static str, &Vec<String>)> {
    vec![
        ("asin", &m.asin),
        ("sku", &m.sku),
        ("upc", &m.upc),
        ("product_ref", &m.product_ref),
        ("url", &m.url),
        ("any", &m.any),
        ("label", &m.label),
        ("preset", &m.preset),
        ("at", &m.at),
        ("cohort", &m.cohort),
        ("exclude", &m.exclude),
    ]
}

fn clean(v: &[String]) -> Vec<String> {
    v.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Write matcher flags into a table: list fields replaced (`append`: added
/// to what is there), `fuzzy` set (`threshold`: see [`apply_matcher_ctx`]).
fn apply_matcher(t: &mut Table, m: &MatcherFlags, append: bool) -> Result<()> {
    for (k, v) in flag_values(m) {
        let v = clean(v);
        if v.is_empty() {
            continue;
        }
        if k == "url" {
            if let Some(bad) = v.iter().find(|u| crate::matching::normalize_url(u).is_none()) {
                return Err(AppError::invalid(format!("'{bad}' is not an http(s) URL")));
            }
        }
        let mut all = if append { lists::get_strings(t, k) } else { Vec::new() };
        for x in v {
            if !all.iter().any(|a| a.eq_ignore_ascii_case(&x)) {
                all.push(x);
            }
        }
        lists::set_key(t, k, Some(lists::strings(&all)));
    }
    if let Some(f) = &m.fuzzy {
        lists::set_key(t, "fuzzy", Some(value(parse_bool_flag(f)?)));
    }
    Ok(())
}

fn apply_want(t: &mut Table, w: &EntryFlags) -> Result<()> {
    if let Some(q) = &w.qty {
        let q = parse_amount(q)?;
        lists::set_key(t, "qty", Some(qty_value(q)));
    }
    if let Some(u) = w.unit.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        lists::set_key(t, "unit", Some(value(crate::matching::canonical_unit(u))));
    }
    if let Some(n) = &w.note {
        lists::set_key(t, "note", Some(n.trim()).filter(|n| !n.is_empty()).map(value));
    }
    if w.optional {
        lists::set_key(t, "optional", Some(value(true)));
    }
    Ok(())
}

/// Whole quantities as TOML integers (`qty = 200`), others as floats.
fn qty_value(q: f64) -> toml_edit::Item {
    if q.fract() == 0.0 && q.abs() < 1e15 {
        value(q as i64)
    } else {
        value(q)
    }
}

/// Validate a list document after an edit (cohorts exist, no cycles) and save it.
fn save_list(ctx: &Ctx, d: &mut Doc<ListFile>) -> Result<()> {
    let text = d.doc.to_string();
    let data: ListFile = toml::from_str(&text).map_err(|e| AppError::invalid(format!("{}: {e}", d.path.display())))?;
    for e in &data.entries {
        resolve_entry(ctx, e)?;
    }
    d.save()
}

fn emit_entry(ctx: &Ctx, d: &Doc<ListFile>, i: usize) -> Result<()> {
    let mut r = entry_record(i, &d.data.entries[i]);
    r.insert("list".into(), json!(d.slug));
    ctx.emit_mutation(&Report::object("list_entry", r))
}

fn add(ctx: &Ctx, spec: &str, name: &str, want: &EntryFlags, m: &MatcherFlags) -> Result<()> {
    let mut d = load_list(ctx, spec)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::usage("entry name must not be empty"));
    }
    if d.data.entries.iter().any(|e| e.name.trim().eq_ignore_ascii_case(name)) {
        return Err(AppError::invalid(format!(
            "list '{}' already has an entry '{name}' (see `wherebox list edit`)",
            d.slug
        )));
    }
    let mut t = Table::new();
    t.insert("name", value(name));
    apply_want(&mut t, want)?;
    apply_matcher_ctx(ctx, &mut t, m, false)?;
    lists::entries_mut(&mut d.doc).push(t);
    save_list(ctx, &mut d)?;
    let i = d.data.entries.len() - 1;
    ctx.info(&format!("added {} to {}", d.data.entries[i].name, d.slug));
    emit_entry(ctx, &d, i)
}

/// [`apply_matcher`] with the threshold checked against this context.
fn apply_matcher_ctx(ctx: &Ctx, t: &mut Table, m: &MatcherFlags, append: bool) -> Result<()> {
    if let Some(th) = &m.threshold {
        threshold(ctx, Some(th))?;
    }
    apply_matcher(t, m, append)?;
    if let Some(th) = &m.threshold {
        lists::set_key(t, "threshold", Some(value(threshold(ctx, Some(th))?)));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn edit(
    ctx: &Ctx,
    spec: &str,
    entry: &str,
    rename: Option<&str>,
    required: bool,
    clear: &[String],
    want: &EntryFlags,
    m: &MatcherFlags,
) -> Result<()> {
    let mut d = load_list(ctx, spec)?;
    let i = lists::entry_index(&d.data, entry)?;
    if let Some(n) = rename.map(str::trim) {
        if n.is_empty() {
            return Err(AppError::usage("--name must not be empty"));
        }
        if d.data.entries.iter().enumerate().any(|(j, e)| j != i && e.name.trim().eq_ignore_ascii_case(n)) {
            return Err(AppError::invalid(format!("list '{}' already has an entry '{n}'", d.slug)));
        }
    }
    let t = lists::entries_mut(&mut d.doc)
        .get_mut(i)
        .ok_or_else(|| AppError::not_found(format!("no entry {} in {}", i + 1, d.slug)))?;
    for c in clear {
        let k = c.trim().to_ascii_lowercase().replace('-', "_");
        if k == "name" || !lists::KEY_ORDER.contains(&k.as_str()) {
            return Err(AppError::usage(format!("cannot clear '{c}' (fields: {})", lists::KEY_ORDER[1..].join(", "))));
        }
        lists::set_key(t, &k, None);
    }
    if let Some(n) = rename {
        lists::set_key(t, "name", Some(value(n.trim())));
    }
    if required {
        lists::set_key(t, "optional", None);
    }
    apply_want(t, want)?;
    apply_matcher_ctx(ctx, t, m, false)?;
    save_list(ctx, &mut d)?;
    ctx.info(&format!("updated {} in {}", d.data.entries[i].name, d.slug));
    emit_entry(ctx, &d, i)
}

fn drop_entry(ctx: &Ctx, spec: &str, entry: &str) -> Result<()> {
    let mut d = load_list(ctx, spec)?;
    let i = lists::entry_index(&d.data, entry)?;
    let gone = entry_record(i, &d.data.entries[i]);
    lists::entries_mut(&mut d.doc).remove(i);
    save_list(ctx, &mut d)?;
    ctx.info(&format!("dropped {} from {}", gone.get("name").and_then(Value::as_str).unwrap_or_default(), d.slug));
    ctx.emit_mutation(&Report::object("removed", gone))
}

// ---------------------------------------------------------------------------
// check, shop
// ---------------------------------------------------------------------------

fn check_all(ctx: &Ctx, p: Option<&Profile>, db: &Db, list: &ListFile, t: f64) -> Result<Vec<EntryCheck>> {
    let inv = inventory(ctx, p, db)?;
    list.entries
        .iter()
        .map(|e| Ok(lists::check_entry(&inv, list, e, &resolve_entry(ctx, e)?, t)))
        .collect::<Result<Vec<_>>>()
        .map(|v| {
            // Rows reference the inventory by index: resolve them now.
            v.into_iter().map(|c| with_names(c, &inv)).collect()
        })
}

/// Item details copied out of the inventory (kept alongside each match).
fn with_names(mut c: EntryCheck, inv: &Inventory) -> EntryCheck {
    for (m, _) in &mut c.matches {
        let i = m.idx;
        m.item = Some(lists::MatchedItem {
            name: inv.items[i].name.clone(),
            id: inv.items[i].id.clone(),
            asset_id: inv.items[i].asset_id.clone(),
            location: inv.item_path(i),
            status: inv.items[i].status.clone(),
            vendor: inv.items[i].preferred_vendor.clone().or_else(|| inv.items[i].purchase_from.clone()),
            product_ref: inv.items[i].product_ref.clone(),
        });
    }
    c
}

fn match_json(m: &lists::Match, c: &lists::Contribution) -> Value {
    let it = m.item.clone().unwrap_or_default();
    json!({
        "item": it.name,
        "item_id": it.id,
        "asset_id": it.asset_id,
        "location": it.location,
        "status": it.status,
        "tier": m.hit.tier.as_str(),
        "score": m.hit.score,
        "via": m.hit.via,
        "qty": c.qty,
        "on_hand": c.raw,
        "on_hand_basis": if c.estimated { "estimated" } else { "logged" },
        "item_unit": c.unit,
        "lent": c.lent,
        "expired": c.expired,
        "basis": c.basis,
    })
}

fn check_rows(list: &ListFile, p: Option<&Profile>, checks: &[EntryCheck]) -> Vec<Record> {
    list.entries
        .iter()
        .zip(checks)
        .enumerate()
        .map(|(i, (e, c))| {
            let matched = c
                .matches
                .iter()
                .map(|(m, _)| {
                    let name = m.item.as_ref().map(|x| x.name.clone()).unwrap_or_default();
                    match m.hit.tier {
                        lists::Tier::Name => format!("{name} (name {:.2})", m.hit.score),
                        t => format!("{name} ({})", t.as_str()),
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            let status = match (list.is_wishlist(), c.status) {
                (true, Status::Fulfilled) => "fulfilled (already own)".to_string(),
                (_, Status::Short) => format!("short (have {} of {})", have_text(c.have, c.estimated), fmt_qty(c.want)),
                (_, s) => s.as_str().to_string(),
            };
            let mut status = status;
            if c.expired > 0.0 && c.status != Status::Have {
                status.push_str(&format!(" ({} {} expired)", fmt_qty(round(c.expired)), c.unit));
            }
            if c.status != Status::Have && c.matches.iter().any(|(_, x)| x.lent) {
                status.push_str(" (lent out)");
            }
            to_record(&json!({
                "profile": p.map(|p| p.name.clone()),
                "n": i + 1,
                "entry": e.name,
                "want": c.want,
                "unit": c.unit,
                "have": round(c.have),
                "on_hand_basis": if c.estimated { "estimated" } else { "logged" },
                "expired": round(c.expired),
                "state": c.status.as_str(),
                "status": status,
                "optional": e.optional,
                "ambiguous": c.ambiguous,
                "matched": matched,
                "matches": c.matches.iter().map(|(m, x)| match_json(m, x)).collect::<Vec<_>>(),
                "note": e.note,
            }))
        })
        .collect()
}

/// Three decimals, and never `-0` (an empty f64 sum is `-0.0`).
fn round(q: f64) -> f64 {
    (q * 1000.0).round() / 1000.0 + 0.0
}

fn check(ctx: &Ctx, spec: &str, all: bool, th: Option<&str>) -> Result<ExitStatus> {
    let d = load_list(ctx, spec)?;
    let t = threshold(ctx, th)?;
    let list = &d.data;
    let per: Vec<(Option<Profile>, Vec<EntryCheck>)> =
        on_list_db(ctx, list, all, "list check", |c, p, db| Ok((p.cloned(), check_all(c, p, db, list, t)?)))?;
    let mut rows = Vec::new();
    let mut verdicts = serde_json::Map::new();
    let mut lines = Vec::new();
    let mut any_ready = false;
    for (p, checks) in &per {
        let (ready, v) = lists::verdict(list, checks);
        any_ready |= ready;
        let who = p.as_ref().map_or_else(|| "-".to_string(), |p| p.name.clone());
        verdicts.insert(who.clone(), json!({"ready": ready, "verdict": v}));
        lines.push(if per.len() > 1 { format!("{who}: {v}") } else { v });
        rows.extend(check_rows(list, p.as_ref(), checks));
    }
    let ready = any_ready || list.is_wishlist();
    let verdict = lines.join("\n");
    if !(ctx.out.format.is_envelope() || ctx.out.format == Format::Jsonl) {
        // Text output marks a have count that rests on an estimate: `~2`.
        for r in &mut rows {
            if r.get("on_hand_basis").and_then(Value::as_str) == Some("estimated") {
                let q = r.get("have").and_then(Value::as_f64).unwrap_or_default();
                r.insert("have".into(), json!(have_text(q, true)));
            }
        }
    }
    let mut cols = vec!["n", "entry", "want", "unit", "have", "status", "matched"];
    if all {
        cols.insert(0, "profile");
    }
    let report = Report::list("list_check", rows)
        .table_columns(&cols)
        .meta("list", list_header(&d))
        .meta("threshold", json!(t))
        .meta("ready", json!(ready))
        .meta("verdict", json!(verdict))
        .meta("verdicts", Value::Object(verdicts));
    if ctx.out.format == Format::Table {
        let mut text = crate::output::render(&report, &ctx.out)?;
        text.push_str(&format!("{verdict}\n"));
        ctx.write_output(text.as_bytes(), &ctx.out)?;
    } else {
        ctx.emit(&report)?;
        ctx.info(&verdict);
    }
    Ok(if ready { 0 } else { NOT_READY })
}

/// Shortfall rows of a list in the `shopping` shape.
fn shop_rows(list: &ListFile, p: Option<&Profile>, checks: &[EntryCheck]) -> (Vec<Record>, Vec<String>) {
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    for (e, c) in list.entries.iter().zip(checks) {
        let (qty, detail) = match c.status {
            Status::Open => (c.want, format!("{}: wishlist", list.name)),
            Status::Missing => (c.want, format!("{}: missing", list.name)),
            Status::Short => (
                round(c.want - c.have),
                format!("{}: have {} of {} {}", list.name, fmt_qty(round(c.have)), fmt_qty(c.want), c.unit),
            ),
            Status::Ambiguous | Status::UnitMismatch => {
                skipped.push(format!("{} ({})", e.name, c.status.as_str()));
                continue;
            }
            Status::Have | Status::Fulfilled => continue,
        };
        let first = c.matches.first().and_then(|(m, _)| m.item.clone());
        let id = [&e.m.asin, &e.m.upc, &e.m.sku, &e.m.product_ref].iter().find_map(|v| v.first().cloned());
        rows.push(to_record(&json!({
            "item": e.name,
            "qty": qty,
            "unit": c.unit,
            "vendor": first.as_ref().and_then(|f| f.vendor.clone()),
            "product_ref": id.or_else(|| first.as_ref().and_then(|f| f.product_ref.clone())),
            "reason": "list",
            "detail": if e.optional { format!("{detail} (optional)") } else { detail },
            "last_bought": Value::Null,
            "cadence_days": Value::Null,
            "overdue_days": Value::Null,
            "location": first.as_ref().and_then(|f| f.location.clone()),
            "profile": p.map(|p| p.name.clone()),
            "asset_id": first.as_ref().and_then(|f| f.asset_id.clone()),
        })));
    }
    (rows, skipped)
}

/// `list shop` rows of a list against one database (for `shopping --include-list`).
pub fn list_shopping_rows(ctx: &Ctx, p: Option<&Profile>, db: &Db, list: &ListFile) -> Result<Vec<Record>> {
    let checks = check_all(ctx, p, db, list, threshold(ctx, None)?)?;
    let (rows, skipped) = shop_rows(list, p, &checks);
    if !skipped.is_empty() {
        ctx.info(&format!("{}: left out (unknown): {}", list.name, skipped.join(", ")));
    }
    Ok(rows)
}

/// Load the lists `shopping --include-list` names.
pub fn load_lists(ctx: &Ctx, specs: &[String]) -> Result<Vec<ListFile>> {
    specs.iter().map(|s| load_list(ctx, s).map(|d| d.data)).collect()
}

fn shop(ctx: &Ctx, spec: &str, all: bool) -> Result<()> {
    let d = load_list(ctx, spec)?;
    let list = &d.data;
    let rows: Vec<Record> = on_list_db(ctx, list, all, "list shop", |c, p, db| list_shopping_rows(c, p, db, list))?
        .into_iter()
        .flatten()
        .collect();
    if rows.is_empty() {
        ctx.info("nothing to buy");
    }
    let mut cols = vec!["item", "qty", "unit", "reason", "detail", "vendor"];
    if all {
        cols.insert(0, "profile");
    }
    ctx.emit(&Report::list("shopping", rows).table_columns(&cols).meta("list", list_header(&d)))
}

// ---------------------------------------------------------------------------
// pin, unpin, explain
// ---------------------------------------------------------------------------

fn pin(ctx: &Ctx, spec: &str, entry: &str, item: &str) -> Result<()> {
    let mut d = load_list(ctx, spec)?;
    let i = lists::entry_index(&d.data, entry)?;
    let list = d.data.clone();
    let (pin, name) = on_list_db(ctx, &list, false, "list pin", |c, p, db| {
        let it = crate::store::resolve_item(db, item)?;
        let fields = crate::store::item_fields(db, &it.id)?;
        let _ = c;
        Ok((lists::pin_of(&it, &fields, p.map(|p| p.name.as_str())), it.name))
    })?
    .into_iter()
    .next()
    .ok_or_else(|| AppError::not_found("no database to pin from"))?;
    let t = lists::entries_mut(&mut d.doc).get_mut(i).expect("entry index checked");
    let mut pins = lists::get_strings(t, "pins");
    if pins.iter().any(|p| p.eq_ignore_ascii_case(&pin)) {
        ctx.info(&format!("{name} is already pinned to {} ({pin})", list.entries[i].name));
    } else {
        pins.push(pin.clone());
        lists::set_key(t, "pins", Some(lists::strings(&pins)));
        save_list(ctx, &mut d)?;
        ctx.info(&format!("pinned {name} to {} as {pin}", d.data.entries[i].name));
    }
    emit_entry(ctx, &d, i)
}

fn unpin(ctx: &Ctx, spec: &str, entry: &str, which: Option<&str>, all: bool) -> Result<()> {
    let mut d = load_list(ctx, spec)?;
    let i = lists::entry_index(&d.data, entry)?;
    let t = lists::entries_mut(&mut d.doc).get_mut(i).expect("entry index checked");
    let pins = lists::get_strings(t, "pins");
    let keep: Vec<String> = match (which, all) {
        (_, true) => Vec::new(),
        (Some(w), _) => {
            let w = w.trim().to_lowercase();
            let k: Vec<String> = pins.iter().filter(|p| !p.to_lowercase().contains(&w)).cloned().collect();
            if k.len() == pins.len() {
                return Err(AppError::not_found(format!("no pin matching '{w}' (pins: {})", pins.join(", "))));
            }
            k
        }
        (None, false) if pins.len() <= 1 => Vec::new(),
        (None, false) => {
            return Err(AppError::usage(format!(
                "entry has {} pins: name one or pass --all ({})",
                pins.len(),
                pins.join(", ")
            )))
        }
    };
    let removed = pins.len() - keep.len();
    lists::set_key(t, "pins", (!keep.is_empty()).then(|| lists::strings(&keep)));
    save_list(ctx, &mut d)?;
    ctx.info(&format!("removed {removed} pin(s) from {}", d.data.entries[i].name));
    emit_entry(ctx, &d, i)
}

fn verdict_cell(e: &lists::Eval, tier: lists::Tier) -> Value {
    e.hits.iter().find(|h| h.tier == tier).map_or(Value::Null, |h| match tier {
        lists::Tier::Name => json!(format!("{} {:.4}", h.via, h.score)),
        _ => json!(h.via),
    })
}

fn explain_rows(inv: &Inventory, r: &Resolved, t: f64) -> Vec<Record> {
    let mut rows: Vec<(f64, Record)> = Vec::new();
    for i in 0..inv.items.len() {
        let e = lists::eval(inv, r, i, t);
        let near = e.best_name.as_ref().is_some_and(|(_, s)| s.score >= 0.5);
        if e.hits.is_empty() && !near && e.excluded.is_none() {
            continue;
        }
        let best = e.best();
        let result = match (best, &e.excluded, e.location_ok) {
            (Some(h), _, _) => h.tier.as_str().to_string(),
            (None, Some(x), _) if !e.hits.is_empty() || near => format!("excluded ({x})"),
            (None, _, false) if !e.hits.is_empty() => "elsewhere (at)".into(),
            _ => "below threshold".into(),
        };
        let score = best.map_or_else(|| e.best_name.as_ref().map_or(0.0, |(_, s)| s.score), |h| h.score);
        let rank = best.map_or(9.0, |h| h.tier as u8 as f64) - score;
        rows.push((
            rank,
            to_record(&json!({
                "item": inv.items[i].name,
                "asset_id": inv.items[i].asset_id,
                "item_id": inv.items[i].id,
                "location": inv.item_path(i),
                "identifier": verdict_cell(&e, lists::Tier::Identifier),
                "url": verdict_cell(&e, lists::Tier::Url),
                "pinned": verdict_cell(&e, lists::Tier::Pinned),
                "attribute": verdict_cell(&e, lists::Tier::Attribute),
                "name": verdict_cell(&e, lists::Tier::Name)
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| e.best_name.as_ref().map(|(q, s)| format!("\"{q}\" {:.4} (< {t})", s.score))),
                "name_score": e.best_name.as_ref().map(|(_, s)| s.score),
                "excluded": e.excluded,
                "location_ok": e.location_ok,
                "tier": best.map(|h| h.tier.as_str()),
                "score": score,
                "via": best.map(|h| h.via.clone()),
                "result": result,
            })),
        ));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, r)| r).collect()
}

fn explain(ctx: &Ctx, spec: &str, entry: &str, th: Option<&str>) -> Result<()> {
    let d = load_list(ctx, spec)?;
    let i = lists::entry_index(&d.data, entry)?;
    let e = d.data.entries[i].clone();
    let t = match th {
        Some(_) => threshold(ctx, th)?,
        None => e.m.threshold.or(d.data.threshold).map_or_else(|| threshold(ctx, None), Ok)?,
    };
    let r = resolve_entry(ctx, &e)?;
    let list = d.data.clone();
    let (rows, ambiguous) = on_list_db(ctx, &list, false, "list explain", |c, p, db| {
        let inv = inventory(c, p, db)?;
        Ok((explain_rows(&inv, &r, t), lists::run(&inv, &r, t).1))
    })?
    .into_iter()
    .next()
    .unwrap_or_default();
    ctx.info(&format!(
        "{}: {} (threshold {t}{})",
        e.name,
        matcher_summary(&e.m, e.fuzzy().then_some(e.name.as_str())),
        if ambiguous { "; AMBIGUOUS: pin one with `wherebox list pin`" } else { "" }
    ));
    ctx.emit(
        &Report::list("explain", rows)
            .table_columns(&[
                "item",
                "asset_id",
                "identifier",
                "url",
                "pinned",
                "attribute",
                "name",
                "excluded",
                "location_ok",
                "result",
            ])
            .meta("entry", Value::Object(entry_record(i, &e)))
            .meta("threshold", json!(t))
            .meta("ambiguous", json!(ambiguous)),
    )
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

fn import(ctx: &Ctx, spec: &str, file: &std::path::Path, kind: &str) -> Result<()> {
    let text = if file.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin()).map_err(AppError::from)?
    } else {
        std::fs::read_to_string(file).map_err(|e| AppError::io(format!("reading {}: {e}", file.display())))?
    };
    let mut d = match load_list(ctx, spec) {
        Ok(d) => d,
        Err(e) if e.kind == crate::error::ErrorKind::NotFound => {
            new(ctx, spec, kind, None)?;
            load_list(ctx, spec)?
        }
        Err(e) => return Err(e),
    };
    let parsed = lists::parse_text(&text);
    let before = d.data.entries.len();
    let mut taken: Vec<String> = d.data.entries.iter().map(|e| e.name.to_lowercase()).collect();
    let mut skipped = 0;
    for e in parsed {
        if taken.contains(&e.name.to_lowercase()) {
            skipped += 1;
            continue;
        }
        taken.push(e.name.to_lowercase());
        let mut t = Table::new();
        t.insert("name", value(e.name.as_str()));
        if let Some(q) = e.qty {
            t.insert("qty", qty_value(q));
        }
        if let Some(u) = &e.unit {
            t.insert("unit", value(u.as_str()));
        }
        if let Some(n) = &e.note {
            t.insert("note", value(n.as_str()));
        }
        if !e.m.url.is_empty() {
            t.insert("url", lists::strings(&e.m.url));
        }
        lists::entries_mut(&mut d.doc).push(t);
    }
    save_list(ctx, &mut d)?;
    let added = d.data.entries.len() - before;
    ctx.info(&format!(
        "imported {added} entr{} into {}{}",
        if added == 1 { "y" } else { "ies" },
        d.slug,
        if skipped > 0 { format!(" ({skipped} already there, skipped)") } else { String::new() }
    ));
    let rows = d.data.entries.iter().enumerate().skip(before).map(|(i, e)| entry_record(i, e)).collect();
    ctx.emit_mutation(
        &Report::list("list", rows)
            .table_columns(&["n", "name", "qty", "unit", "matcher", "note"])
            .meta("list", list_header(&d))
            .meta("skipped", json!(skipped)),
    )
}

// ---------------------------------------------------------------------------
// cohort
// ---------------------------------------------------------------------------

fn cohort_record(ctx: &Ctx, d: &Doc<CohortFile>) -> Result<Record> {
    let r = lists::resolve_files(config(ctx), &d.data.m, None, Some(&d.slug))?;
    fn includes(r: &Resolved, out: &mut Vec<String>) {
        for (s, c) in &r.cohorts {
            if !out.contains(s) {
                out.push(s.clone());
            }
            includes(c, out);
        }
    }
    let mut inc = Vec::new();
    includes(&r, &mut inc);
    let mut rec = to_record(&json!({
        "slug": d.slug,
        "name": d.data.name,
        "notes": d.data.notes,
        "matcher": matcher_summary(&d.data.m, None),
        "includes": inc,
        "path": d.path,
    }));
    if let Value::Object(m) = matcher_json(&d.data.m) {
        rec.extend(m);
    }
    Ok(rec)
}

/// Validate a cohort document after an edit (included cohorts exist, no cycle) and save it.
fn save_cohort(ctx: &Ctx, d: &mut Doc<CohortFile>) -> Result<()> {
    let data: CohortFile =
        toml::from_str(&d.doc.to_string()).map_err(|e| AppError::invalid(format!("{}: {e}", d.path.display())))?;
    lists::resolve_files(config(ctx), &data.m, None, Some(&d.slug))?;
    d.save()
}

pub fn cohort(ctx: &Ctx, cmd: CohortCmd) -> Result<()> {
    match cmd {
        CohortCmd::New { name, notes, m } => {
            let mut fields = Vec::new();
            if let Some(n) = notes.filter(|n| !n.trim().is_empty()) {
                fields.push(("notes", value(n.trim())));
            }
            let path = lists::create(config(ctx), Kind::Cohort, &name, &fields)?;
            let mut d: Doc<CohortFile> = Doc::load(&path)?;
            apply_matcher_ctx(ctx, d.doc.as_table_mut(), &m, true)?;
            if let Err(e) = save_cohort(ctx, &mut d) {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
            ctx.info(&format!("created cohort {} at {}", d.slug, path.display()));
            ctx.emit_mutation(&Report::object("cohort", cohort_record(ctx, &d)?))
        }
        CohortCmd::Ls => {
            let mut rows = Vec::new();
            for p in lists::files(config(ctx), Kind::Cohort) {
                let d: Doc<CohortFile> = Doc::load(&p)?;
                rows.push(cohort_record(ctx, &d)?);
            }
            if rows.is_empty() {
                ctx.info("no cohorts yet (see `wherebox cohort new`)");
            }
            ctx.emit(&Report::list("cohorts", rows).table_columns(&["slug", "name", "matcher", "includes"]))
        }
        CohortCmd::Show { cohort } => {
            let d = load_cohort(ctx, &cohort)?;
            ctx.emit(&Report::object("cohort", cohort_record(ctx, &d)?))
        }
        CohortCmd::Add { name, m } => {
            let mut d = load_cohort(ctx, &name)?;
            apply_matcher_ctx(ctx, d.doc.as_table_mut(), &m, true)?;
            save_cohort(ctx, &mut d)?;
            ctx.info(&format!("updated cohort {}", d.slug));
            ctx.emit_mutation(&Report::object("cohort", cohort_record(ctx, &d)?))
        }
        CohortCmd::Rm { cohort } => rm(ctx, Kind::Cohort, &cohort),
    }
}

// ---------------------------------------------------------------------------
// match
// ---------------------------------------------------------------------------

fn flags_matcher(ctx: &Ctx, m: &MatcherFlags) -> Result<Matcher> {
    let mut t = Table::new();
    t.insert("name", value("match"));
    apply_matcher_ctx(ctx, &mut t, m, false)?;
    let mut doc = toml_edit::DocumentMut::new();
    for (k, v) in t.iter() {
        doc[k] = v.clone();
    }
    let e: Entry = toml::from_str(&doc.to_string()).map_err(|e| AppError::invalid(e.to_string()))?;
    Ok(e.m)
}

pub fn match_cmd(ctx: &Ctx, a: MatchArgs) -> Result<()> {
    let m = flags_matcher(ctx, &a.m)?;
    let query = a.query.as_deref().map(str::trim).filter(|q| !q.is_empty());
    if query.is_none() && matcher_summary(&m, None).is_empty() {
        return Err(AppError::usage("give a name to look for, or matcher flags (--asin, --url, --label, ...)"));
    }
    let t = threshold(ctx, a.m.threshold.as_deref())?;
    let r = lists::resolve_files(config(ctx), &m, query.map(str::to_string), None)?;
    let (rows, ambiguous) = crate::commands::profile::each_db(ctx, false, "match", |c, p, db| {
        let inv = inventory(c, p, db)?;
        let (found, amb) = lists::run(&inv, &r, t);
        let rows: Vec<Record> = found
            .iter()
            .map(|mt| {
                let i = mt.idx;
                let c = lists::contribution(&inv, i, &m.at, inv.items[i].unit.as_deref().unwrap_or("ea"), false);
                to_record(&json!({
                    "item": inv.items[i].name,
                    "asset_id": inv.items[i].asset_id,
                    "item_id": inv.items[i].id,
                    "tier": mt.hit.tier.as_str(),
                    "score": mt.hit.score,
                    "via": mt.hit.via,
                    "on_hand": c.raw,
                    "on_hand_basis": if c.estimated { "estimated" } else { "logged" },
                    "unit": c.unit,
                    "expired": c.expired,
                    "location": inv.item_path(i),
                    "status": inv.items[i].status,
                    "profile": p.map(|p| p.name.clone()),
                }))
            })
            .collect();
        Ok((rows, amb))
    })?
    .into_iter()
    .next()
    .unwrap_or_default();
    if rows.is_empty() {
        ctx.info("no match");
    } else if ambiguous {
        ctx.info("ambiguous: the top candidates are too close to call");
    }
    ctx.emit(
        &Report::list("match", rows)
            .table_columns(&["item", "asset_id", "tier", "score", "via", "on_hand", "unit", "location"])
            .meta("query", json!(query))
            .meta("threshold", json!(t))
            .meta("ambiguous", json!(ambiguous)),
    )
}
