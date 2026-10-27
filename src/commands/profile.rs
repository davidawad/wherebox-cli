//! `profile ls|init|show`: one database per property.

use serde_json::json;

use crate::cli::ProfileCmd;
use crate::context::Ctx;
use crate::error::{AppError, Result};
use crate::output::{to_record, Report};
use crate::profiles::{self, Profile};
use crate::store::{self, Tree};
use crate::util::money_value;

pub fn run(ctx: &Ctx, cmd: ProfileCmd) -> Result<()> {
    match cmd {
        ProfileCmd::Ls => ls(ctx),
        ProfileCmd::Init { name, title, address } => init(ctx, name.as_deref(), title.as_deref(), address.as_deref()),
        ProfileCmd::Show { name } => show(ctx, name.as_deref()),
    }
}

/// The named profile, else the one in use.
fn pick(ctx: &Ctx, name: Option<&str>) -> Result<Profile> {
    match (name, &ctx.profile) {
        (Some(n), _) => profiles::get(&ctx.resolved.config_path, n),
        (None, Some(p)) => Ok(p.clone()),
        (None, None) => Err(AppError::usage("no profile given: pass a NAME or --profile (see `wherebox profile ls`)")),
    }
}

fn ls(ctx: &Ctx) -> Result<()> {
    let active = ctx.profile.as_ref().map(|p| p.name.clone());
    let rows = profiles::list(&ctx.resolved.config_path)?
        .into_iter()
        .map(|p| {
            to_record(&json!({
                "name": p.name,
                "title": p.title,
                "address": p.address,
                "rented": p.rented,
                "active": active.as_deref() == Some(p.name.as_str()),
                "exists": p.db_path.exists(),
                "db_path": p.db_path,
                "source": p.source,
            }))
        })
        .collect();
    ctx.emit(&Report::list("profiles", rows).table_columns(&["name", "title", "active", "exists", "db_path"]))
}

/// Run `f` on the database in use, or with `all` on every profile that has a
/// database (in name order). `f` gets the context and the profile (`None`
/// for a database chosen without a profile, e.g. `--db`).
pub fn each_db<T>(
    ctx: &Ctx,
    all: bool,
    command: &str,
    mut f: impl FnMut(&Ctx, Option<&Profile>, &crate::db::Db) -> Result<T>,
) -> Result<Vec<T>> {
    if !all {
        let db = ctx.db()?;
        return Ok(vec![f(ctx, ctx.profile.as_ref(), &db)?]);
    }
    if ctx.resolved.source("profile") == crate::config::Source::Flag {
        return Err(AppError::usage("give either --profile or --all-profiles, not both"));
    }
    let mut out = Vec::new();
    for p in profiles::list(&ctx.resolved.config_path)?.into_iter().filter(|p| p.db_path.exists()) {
        let pc = if ctx.db_path == p.db_path { None } else { Some(with_profile(ctx, &p)?) };
        let c = pc.as_ref().unwrap_or(ctx);
        let db = c.db()?;
        let r = f(c, Some(&p), &db);
        // The audit append takes the database lock: release it first.
        drop(db);
        if let Some(c) = &pc {
            c.finish_audit(command, i32::from(r.is_err()));
        }
        ctx.mark_touched();
        out.push(r?);
    }
    if out.is_empty() {
        ctx.info("no property has a database yet (see `wherebox profile init`)");
    }
    Ok(out)
}

/// Run `f` on profile `p`'s database (through a context of its own unless
/// it is the database in use).
pub fn on_profile<T>(
    ctx: &Ctx,
    p: &Profile,
    command: &str,
    f: impl FnOnce(&Ctx, Option<&Profile>, &crate::db::Db) -> Result<T>,
) -> Result<T> {
    let pc = if ctx.db_path == p.db_path { None } else { Some(with_profile(ctx, p)?) };
    let c = pc.as_ref().unwrap_or(ctx);
    let db = c.db()?;
    let r = f(c, Some(p), &db);
    // The audit append takes the database lock: release it first.
    drop(db);
    if let Some(c) = &pc {
        c.finish_audit(command, i32::from(r.is_err()));
    }
    ctx.mark_touched();
    r
}

/// A context whose database is `p`'s.
fn with_profile(ctx: &Ctx, p: &Profile) -> Result<Ctx> {
    let mut c = Ctx::new(&crate::cli::GlobalOpts {
        // The outer context created the default config on first run; a
        // missing file here must not count as an explicit, missing --config.
        config: Some(ctx.resolved.config_path.clone()).filter(|p| p.exists()),
        db: Some(p.db_path.clone()),
        insecure_plaintext: ctx.insecure,
        ..Default::default()
    })?;
    c.out = ctx.out.clone();
    c.profile = Some(p.clone());
    Ok(c)
}

fn init(ctx: &Ctx, name: Option<&str>, title: Option<&str>, address: Option<&str>) -> Result<()> {
    let mut p = pick(ctx, name)?;
    if let Some(t) = title {
        p.title = t.trim().to_string();
    }
    if let Some(a) = address {
        p.address = Some(a.trim().to_string()).filter(|a| !a.is_empty());
    }
    let pc = if ctx.db_path == p.db_path { None } else { Some(with_profile(ctx, &p)?) };
    let c = pc.as_ref().unwrap_or(ctx);
    let existed = p.db_path.exists();
    let db = c.db()?;
    let mut tree = Tree::load(&db)?;
    let root = match tree.find_path(&p.title) {
        Some(id) => id,
        None => {
            let (id, _) = tree.ensure_path(&db, &p.title, Some("property"))?;
            if let Some(a) = &p.address {
                let mut l = tree.get(id).cloned().expect("just created");
                l.address = Some(a.clone());
                store::update_location(&db, &l)?;
            }
            id
        }
    };
    ctx.info(&format!(
        "{} profile {} at {} (top-level location: {})",
        if existed { "ready:" } else { "created" },
        p.name,
        p.db_path.display(),
        tree.path(root)
    ));
    let encrypted = db.sealed().is_some();
    // The audit append takes the database lock: release it first.
    drop(db);
    if let Some(c) = &pc {
        c.finish_audit("profile init", 0);
    }
    ctx.emit_mutation(&Report::object(
        "profile_init",
        to_record(&json!({"name": p.name, "title": p.title, "db_path": p.db_path, "created": !existed, "encrypted": encrypted})),
    ))
}

fn show(ctx: &Ctx, name: Option<&str>) -> Result<()> {
    let p = pick(ctx, name)?;
    let mut rec = to_record(&json!({
        "name": p.name,
        "title": p.title,
        "address": p.address,
        "rented": p.rented,
        "stores": p.stores.join(", "),
        "db_path": p.db_path,
        "source": p.source,
        "exists": p.db_path.exists(),
    }));
    if p.db_path.exists() {
        let pc = if ctx.db_path == p.db_path { None } else { Some(with_profile(ctx, &p)?) };
        let c = pc.as_ref().unwrap_or(ctx);
        let db = c.db()?;
        let items: Vec<_> = store::all_items(&db)?.into_iter().filter(|i| !i.is_gone()).collect();
        rec.insert("items".into(), json!(items.len()));
        rec.insert("locations".into(), json!(Tree::load(&db)?.locs.len()));
        rec.insert("value".into(), json!(money_value(items.iter().filter_map(|i| i.value()).sum())));
        drop(db);
        if let Some(c) = &pc {
            c.finish_audit("profile show", 0);
        }
    }
    ctx.emit(&Report::object("profile", rec))
}
