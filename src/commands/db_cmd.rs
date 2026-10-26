use serde_json::json;

use crate::cli::DbCmd;
use crate::context::Ctx;
use crate::db::RowExt;
use crate::error::{AppError, Result};
use crate::keys::{self, Source};
use crate::migrations;
use crate::output::{to_record, Report};
use crate::prompt::Terminal;
use crate::store;
use crate::vault;

pub fn run(ctx: &Ctx, cmd: DbCmd) -> Result<()> {
    match cmd {
        DbCmd::Path => ctx.emit(&Report::object(
            "db_path",
            to_record(&json!({"path": ctx.db_path, "exists": ctx.db_path.exists(), "source": ctx.resolved.source("db_path").as_str()})),
        )),
        DbCmd::Init { encrypt } => {
            if encrypt && ctx.insecure {
                return Err(AppError::usage("--encrypt and --insecure-plaintext are mutually exclusive"));
            }
            let existed = ctx.db_path.exists();
            let db = ctx.db_raw()?;
            let applied = migrations::migrate(&db)?;
            ctx.info(&format!("{} {}", if existed { "database ready:" } else { "created database" }, ctx.db_path.display()));
            ctx.emit(&Report::object(
                "db_init",
                to_record(&json!({
                    "path": ctx.db_path,
                    "created": !existed,
                    "applied": applied,
                    "version": migrations::current_version(&db)?,
                    "encrypted": db.sealed().is_some(),
                    "key_source": db.sealed().map(|s| s.key_source),
                })),
            ))
        }
        DbCmd::Encrypt => encrypt(ctx),
        DbCmd::Rekey { to, rotate_dek } => rekey(ctx, to.as_deref(), rotate_dek),
        DbCmd::Migrate { status } => migrate(ctx, status),
        DbCmd::Backup { dest } => {
            if dest.exists() {
                return Err(AppError::invalid(format!("{} already exists", dest.display())));
            }
            let db = ctx.db()?;
            db.backup_to(&dest)?;
            let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            let encrypted = db.sealed().is_some();
            ctx.info(&format!("backed up to {}{}", dest.display(), if encrypted { " (encrypted, same key)" } else { "" }));
            ctx.emit(&Report::object(
                "db_backup",
                to_record(&json!({"source": ctx.db_path, "dest": dest, "bytes": size, "encrypted": encrypted})),
            ))
        }
        DbCmd::Vacuum => {
            let db = ctx.db()?;
            db.execute("VACUUM", &[])?;
            ctx.info("vacuum complete");
            ctx.emit(&Report::object("db_vacuum", to_record(&json!({"path": ctx.db_path, "ok": true}))))
        }
        DbCmd::Check => check(ctx),
        DbCmd::Info => info(ctx),
        DbCmd::RegroupConsumables { dry_run } => crate::commands::regroup::run(ctx, dry_run),
    }
}

fn migrate(ctx: &Ctx, status_only: bool) -> Result<()> {
    let db = ctx.db_raw()?;
    let applied_now = if status_only { vec![] } else { migrations::migrate(&db)? };
    let applied = migrations::applied(&db)?;
    let rows = migrations::MIGRATIONS
        .iter()
        .map(|m| {
            let a = applied.iter().find(|a| a.0 == m.version);
            to_record(&json!({
                "version": m.version,
                "name": m.name,
                "applied": a.is_some(),
                "applied_at": a.map(|a| a.2.clone()),
                "applied_now": applied_now.contains(&m.version),
            }))
        })
        .collect();
    ctx.emit(&Report::list("migrations", rows).meta("current_version", json!(migrations::current_version(&db)?)))
}

fn check(ctx: &Ctx) -> Result<()> {
    let db = ctx.db()?;
    let integrity: Vec<String> = db.query("PRAGMA integrity_check", &[])?.iter().filter_map(|r| r.s(0)).collect();
    let tree = store::Tree::load(&db)?;
    let items = store::all_items(&db)?;
    let lots = store::all_lots(&db)?;
    let has_item = |id: &str| items.iter().any(|i| i.id == id);
    let has_loc = |id: Option<i64>| id.is_none_or(|l| tree.get(l).is_some());
    let orphan_items: Vec<&str> = items
        .iter()
        .filter(|i| !has_loc(i.location_id) || i.parent_item_id.as_deref().is_some_and(|p| !has_item(p)))
        .map(|i| i.id.as_str())
        .collect();
    let orphan_lots: Vec<&str> =
        lots.iter().filter(|l| !has_item(&l.item_id) || !has_loc(l.location_id)).map(|l| l.id.as_str()).collect();
    let bad_locations: Vec<i64> = tree
        .locs
        .iter()
        .filter(|l| {
            l.parent_id.is_some_and(|p| {
                tree.get(p).is_none() || tree.lineage(l.id).first().is_some_and(|r| r.parent_id.is_some())
            })
        })
        .map(|l| l.id)
        .collect();
    let version = migrations::current_version(&db)?;
    let ok = integrity == ["ok"]
        && orphan_items.is_empty()
        && orphan_lots.is_empty()
        && bad_locations.is_empty()
        && version == migrations::latest_version();
    let report = Report::object(
        "db_check",
        to_record(&json!({
            "ok": ok,
            "integrity": integrity,
            "orphan_items": orphan_items,
            "orphan_lots": orphan_lots,
            "broken_locations": bad_locations,
            "schema_version": version,
            "expected_version": migrations::latest_version(),
        })),
    );
    ctx.emit(&report)?;
    if ok {
        Ok(())
    } else {
        Err(AppError::db("database check failed"))
    }
}

fn info(ctx: &Ctx) -> Result<()> {
    let db = ctx.db()?;
    let count = |t: &str| db.query_scalar_i64(&format!("SELECT count(*) FROM {t}"), &[]);
    let size = std::fs::metadata(&ctx.db_path).map(|m| m.len()).unwrap_or(0);
    ctx.emit(&Report::object(
        "db_info",
        to_record(&json!({
            "path": ctx.db_path,
            "profile": ctx.profile.as_ref().map(|p| &p.name),
            "bytes": size,
            "encrypted": db.sealed().is_some(),
            "key_source": db.sealed().map(|s| s.key_source),
            "schema_version": migrations::current_version(&db)?,
            "items": count("items")?,
            "locations": count("locations")?,
            "labels": count("labels")?,
            "lots": count("lots")?,
            "attachments": count("attachments")?,
            "events": count("events")?,
            "import_batches": count("import_batches")?,
        })),
    ))
}

fn encrypt(ctx: &Ctx) -> Result<()> {
    let r = vault::encrypt_in_place(&ctx.db_path, &ctx.open_opts()?, &mut Terminal)?;
    ctx.info(&format!(
        "encrypted {} (key from {}); plaintext overwritten and removed",
        ctx.db_path.display(),
        r.key_source
    ));
    if let Ok((h, u)) = vault::unlock(&ctx.db_path, ctx.key_source()?, false, &mut Terminal) {
        ctx.record_keys(&h);
        ctx.set_audit(h.db_id, u.keys.audit);
    }
    let sealed_attachments = crate::commands::attach::seal_plaintext(ctx, &ctx.db()?)?;
    let tables: serde_json::Map<String, serde_json::Value> =
        r.tables.iter().map(|(t, n)| (t.clone(), json!(n))).collect();
    ctx.emit_mutation(&Report::object(
        "db_encrypt",
        to_record(&json!({
            "path": ctx.db_path,
            "bytes": r.bytes,
            "key_source": r.key_source,
            "verified_rows": tables,
            "wiped": r.wiped,
            "attachments_sealed": sealed_attachments,
        })),
    ))
}

fn rekey(ctx: &Ctx, to: Option<&str>, rotate_dek: bool) -> Result<()> {
    let current = ctx.key_source()?;
    let to = to.map_or(Ok(current), Source::parse)?;
    let r = vault::rekey(&ctx.db_path, &ctx.open_opts()?, to, rotate_dek, &mut Terminal)?;
    ctx.set_audit(r.db_id, r.audit_key.clone());
    ctx.record_keys(&r.header);
    ctx.info(&format!("rekeyed {} ({} -> {})", ctx.db_path.display(), r.from, r.to));
    if r.to == "env" {
        ctx.info(&format!("set {} to the value of {} from now on", keys::ENV_KEY, keys::ENV_NEW_KEY));
    }
    ctx.emit_mutation(&Report::object(
        "db_rekey",
        to_record(&json!({"path": ctx.db_path, "from": r.from, "to": r.to, "rotated_dek": r.rotated_dek})),
    ))
}
