//! Attachments: `attach`, `attachment ls|get|rm`.
//!
//! Files live in `<data dir>/attachments/<item id>/`, named by the SHA-256 of
//! their content. When the database is encrypted, each file is sealed
//! (XChaCha20-Poly1305, `<sha256>.enc`, the item id and hash as associated
//! data) with a key derived from the database's long-term data key (the one
//! that also seals the audit log and survives `db rekey --rotate-dek`), so
//! attachments open with exactly the keys that open the database. A
//! plaintext (`--insecure-plaintext`) database keeps plaintext attachments.

use std::path::Path;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::cli::{AttachArgs, AttachmentCmd};
use crate::context::Ctx;
use crate::crypto;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::output::{to_record, Report};
use crate::store::{self, check_choice, Attachment, ATTACHMENT_KINDS};

fn guess_kind(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("jpg" | "jpeg" | "png" | "heic" | "heif" | "webp" | "gif" | "tif" | "tiff") => "photo",
        _ => "other",
    }
}

/// The attachment key of an encrypted database.
fn key(db: &Db) -> Option<crypto::Key> {
    db.sealed().and_then(|s| {
        let mut h = Sha256::new();
        h.update(b"wherebox-attachments-v1\0");
        h.update(s.keys.audit.as_bytes());
        crypto::Key::from_bytes(&h.finalize())
    })
}

fn aad(item_id: &str, sha: &str) -> Vec<u8> {
    format!("wherebox-attachment:{item_id}:{sha}").into_bytes()
}

pub fn attach(ctx: &Ctx, a: AttachArgs) -> Result<()> {
    let db = ctx.db()?;
    let item = store::resolve_item(&db, &a.item)?;
    let kind = match &a.kind {
        Some(k) => check_choice("attachment kind", k, ATTACHMENT_KINDS)?,
        None => guess_kind(&a.file).to_string(),
    };
    let data = std::fs::read(&a.file).map_err(|e| AppError::io(format!("reading {}: {e}", a.file.display())))?;
    let sha = crypto::hex(&Sha256::digest(&data));
    let ext =
        a.file.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_default();
    let (rel, bytes, encrypted) = match key(&db) {
        Some(k) => (format!("{}/{sha}.enc", item.id), crypto::seal_message(&k, &aad(&item.id, &sha), &data)?, true),
        None => (format!("{}/{sha}{ext}", item.id), data.clone(), false),
    };
    let root = ctx.attachments_dir();
    let path = root.join(&rel);
    let att = Attachment {
        id: 0,
        item_id: item.id.clone(),
        kind,
        path: rel,
        filename: a.file.file_name().map(|n| n.to_string_lossy().into_owned()),
        sha256: sha,
        bytes: data.len() as i64,
        encrypted,
        primary: a.primary,
        created_at: crate::util::now_iso(),
    };
    let id = db.transaction(|db| {
        let id = store::insert_attachment(db, &att)?;
        for dir in [&root, &root.join(&item.id)] {
            if !dir.exists() {
                std::fs::create_dir_all(dir).map_err(|e| AppError::io(format!("creating {}: {e}", dir.display())))?;
                crate::perms::restrict_dir(dir)
                    .map_err(|e| AppError::io(format!("restricting {}: {e}", dir.display())))?;
            }
        }
        crypto::atomic_write(&path, &bytes)?;
        store::add_event(
            db,
            &item.id,
            "attached",
            json!({"attachment": id, "kind": att.kind, "filename": att.filename}),
        )?;
        Ok(id)
    })?;
    ctx.info(&format!(
        "attached {} to {} ({}{})",
        att.filename.as_deref().unwrap_or("file"),
        item.name,
        att.kind,
        if encrypted { ", encrypted" } else { "" }
    ));
    ctx.emit_mutation(&Report::object("attachment", to_record(&store::get_attachment(&db, id)?)))
}

pub fn run(ctx: &Ctx, cmd: AttachmentCmd) -> Result<()> {
    let db = ctx.db()?;
    match cmd {
        AttachmentCmd::Ls { item } => {
            let item = item.as_deref().map(|i| store::resolve_item(&db, i)).transpose()?;
            let rows = store::attachments(&db, item.as_ref().map(|i| i.id.as_str()))?.iter().map(to_record).collect();
            ctx.emit(&Report::list("attachments", rows).table_columns(&[
                "id",
                "item_id",
                "kind",
                "filename",
                "bytes",
                "primary",
                "encrypted",
            ]))
        }
        AttachmentCmd::Get { id, dest } => {
            let a = store::get_attachment(&db, id)?;
            let data = read(ctx, &db, &a)?;
            if dest.exists() {
                return Err(AppError::invalid(format!("{} already exists", dest.display())));
            }
            crypto::create_private(&dest)
                .and_then(|mut f| std::io::Write::write_all(&mut f, &data))
                .map_err(|e| AppError::io(format!("writing {}: {e}", dest.display())))?;
            ctx.info(&format!("wrote {} ({} bytes)", dest.display(), data.len()));
            ctx.emit_mutation(&Report::object(
                "attachment_get",
                to_record(&json!({"id": id, "dest": dest, "bytes": data.len()})),
            ))
        }
        AttachmentCmd::Rm { id } => {
            let a = store::get_attachment(&db, id)?;
            store::delete_attachment(&db, id)?;
            store::add_event(&db, &a.item_id, "detached", json!({"attachment": id, "filename": a.filename}))?;
            remove_unreferenced(ctx, &db, std::slice::from_ref(&a))?;
            ctx.info(&format!("removed attachment {id}"));
            ctx.emit_mutation(&Report::object("removed", to_record(&json!({"attachment": id}))))
        }
    }
}

/// The plaintext content of an attachment, verified against its hash.
pub fn read(ctx: &Ctx, db: &Db, a: &Attachment) -> Result<Vec<u8>> {
    let path = ctx.attachments_dir().join(&a.path);
    let raw = std::fs::read(&path).map_err(|e| AppError::io(format!("reading {}: {e}", path.display())))?;
    let data = if a.encrypted {
        let k = key(db).ok_or_else(|| AppError::invalid("attachment is encrypted but the database is not"))?;
        crypto::open_message(&k, &aad(&a.item_id, &a.sha256), &raw)
            .map(|z| z.to_vec())
            .ok_or_else(|| AppError::invalid(format!("{} failed to decrypt (tampered or wrong key)", path.display())))?
    } else {
        raw
    };
    if crypto::hex(&Sha256::digest(&data)) != a.sha256 {
        return Err(AppError::invalid(format!("{} does not match its recorded SHA-256", path.display())));
    }
    Ok(data)
}

/// Delete the files of removed attachment rows that no other row uses.
pub fn remove_unreferenced(ctx: &Ctx, db: &Db, removed: &[Attachment]) -> Result<()> {
    let still: Vec<String> = store::attachments(db, None)?.into_iter().map(|a| a.path).collect();
    for a in removed.iter().filter(|a| !still.contains(&a.path)) {
        let p = ctx.attachments_dir().join(&a.path);
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| AppError::io(format!("removing {}: {e}", p.display())))?;
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::remove_dir(dir); // only succeeds when empty
        }
    }
    Ok(())
}

/// Seal attachments stored in plaintext (after `db encrypt`); returns how many.
pub fn seal_plaintext(ctx: &Ctx, db: &Db) -> Result<usize> {
    let Some(k) = key(db) else { return Ok(0) };
    let plain: Vec<Attachment> = store::attachments(db, None)?.into_iter().filter(|a| !a.encrypted).collect();
    for a in &plain {
        let data = read(ctx, db, a)?;
        let rel = format!("{}/{}.enc", a.item_id, a.sha256);
        crypto::atomic_write(
            &ctx.attachments_dir().join(&rel),
            &crypto::seal_message(&k, &aad(&a.item_id, &a.sha256), &data)?,
        )?;
        db.execute(
            "UPDATE attachments SET path = ?1, encrypted = 1 WHERE id = ?2",
            &[crate::db::text(&rel), crate::db::int(a.id)],
        )?;
        let old = ctx.attachments_dir().join(&a.path);
        if old.exists() {
            crypto::wipe_file(&old)?;
        }
    }
    Ok(plain.len())
}
