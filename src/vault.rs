//! Opening, creating, migrating and rekeying encrypted databases, and
//! changing their key slots.

use std::path::{Path, PathBuf};

use crate::container::{Holder, Slot, SlotKind};
use crate::crypto::{self, random_array, DataKeys, Header};
use crate::db::{Db, RowExt, Sealed};
use crate::error::{AppError, Result};
use crate::keyfile;
use crate::keys::{self, key_error, Source};
use crate::keysetup::{self, NewSlots, Setup};
use crate::prompt::Prompter;

pub const INSECURE_WARNING: &str = "warning: --insecure-plaintext: inventory data is stored UNENCRYPTED on disk";

#[derive(Debug, Clone)]
pub struct OpenOpts {
    pub source: Source,
    /// Allow plaintext databases (`--insecure-plaintext`).
    pub insecure: bool,
    /// The config file that records how databases open (for notices).
    pub config: PathBuf,
}

/// What is at a database path.
pub enum State {
    Missing,
    Sealed(Header),
    Plain,
}

pub fn state(path: &Path) -> Result<State> {
    if !path.exists() {
        Ok(State::Missing)
    } else if crypto::is_sealed(path) {
        crypto::read_header(path).map(State::Sealed)
    } else {
        Ok(State::Plain)
    }
}

fn disp(path: &Path) -> String {
    path.display().to_string()
}

pub fn plaintext_error(path: &Path) -> AppError {
    key_error(format!(
        "{} is an unencrypted database; encrypt it in place with `wherebox db encrypt`, \
         or pass --insecure-plaintext to keep using it unencrypted",
        path.display()
    ))
}

/// Slots for a brand-new database at `path` (announced through `p`).
fn first_slots(
    path: &Path,
    o: &OpenOpts,
    id: [u8; crypto::ID_LEN],
    keys: &DataKeys,
    p: &mut dyn Prompter,
) -> Result<NewSlots> {
    let what = disp(path);
    let mut s = Setup { what: &what, config: &o.config, prompter: p };
    keysetup::new_slots(o.source, keys::ENV_KEY, id, keys, &mut s)
}

/// Open (creating if missing) the database at `path`. New databases are
/// encrypted unless `insecure`; they are migrated and sealed before return.
pub fn open(path: &Path, o: &OpenOpts, warn: &dyn Fn(&str), p: &mut dyn Prompter) -> Result<Db> {
    let lock = crate::db::lock(path)?;
    match state(path)? {
        State::Missing | State::Plain if o.insecure => {
            warn(INSECURE_WARNING);
            Ok(Db::open_plain(path)?.with_lock(lock))
        }
        State::Missing => {
            let id = random_array()?;
            let keys = DataKeys::random()?;
            let ns = first_slots(path, o, id, &keys, p)?;
            let header = Header::new(id, ns.slots.clone());
            let key_source = ns.slots[0].kind.name();
            let db = Db::create_sealed(path, Sealed { header, keys, key_source })?;
            crate::migrations::migrate(&db)?;
            db.persist()?;
            ns.commit()?;
            Ok(db.with_lock(lock))
        }
        State::Sealed(header) => {
            let u = keys::unlock(o.source, &header, &disp(path), true, p)?;
            Ok(Db::open_sealed(path, Sealed { header, keys: u.keys, key_source: u.source })?.with_lock(lock))
        }
        State::Plain => Err(plaintext_error(path)),
    }
}

/// Unlock an existing encrypted database's keys without loading it.
pub fn unlock(
    path: &Path,
    source: Source,
    allow_prompt: bool,
    p: &mut dyn Prompter,
) -> Result<(Header, keys::Unlocked)> {
    match state(path)? {
        State::Sealed(h) => {
            let u = keys::unlock(source, &h, &disp(path), allow_prompt, p)?;
            Ok((h, u))
        }
        State::Missing => Err(AppError::not_found(format!("no database at {}", path.display()))),
        State::Plain => Err(AppError::invalid(format!("{} is not encrypted", path.display()))),
    }
}

/// Row count per user table, for round-trip verification.
fn table_counts(db: &Db) -> Result<Vec<(String, i64)>> {
    let names: Vec<String> = db
        .query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name", &[])?
        .iter()
        .filter_map(|r| r.s(0))
        .collect();
    names
        .into_iter()
        .map(|n| {
            let c = db.query_scalar_i64(&format!("SELECT count(*) FROM \"{}\"", n.replace('"', "\"\"")), &[])?;
            Ok((n, c))
        })
        .collect()
}

pub struct EncryptReport {
    pub tables: Vec<(String, i64)>,
    pub bytes: u64,
    pub key_source: String,
    pub wiped: Vec<String>,
}

/// Reopen the freshly sealed `tmp` and compare it with the plaintext.
fn verify_sealed(tmp: &Path, keys: &DataKeys, tables: &[(String, i64)]) -> Result<()> {
    let h = crypto::read_header(tmp)?;
    let db = Db::open_sealed(tmp, Sealed { header: h, keys: keys.clone(), key_source: "verify" })?;
    let integrity: Vec<String> = db.query("PRAGMA integrity_check", &[])?.iter().filter_map(|r| r.s(0)).collect();
    if integrity != ["ok"] {
        return Err(AppError::db(format!("verification: integrity check failed: {integrity:?}")));
    }
    if table_counts(&db)? != tables {
        return Err(AppError::db("verification: row counts differ after encryption"));
    }
    Ok(())
}

/// Encrypt a plaintext database in place: seal into a temp file, verify the
/// round trip from disk (integrity check + identical row counts), swap it in,
/// then overwrite and delete the plaintext file and its sidecars.
pub fn encrypt_in_place(path: &Path, o: &OpenOpts, p: &mut dyn Prompter) -> Result<EncryptReport> {
    let lock = crate::db::lock(path)?;
    match state(path)? {
        State::Missing => return Err(AppError::not_found(format!("no database at {}", path.display()))),
        State::Sealed(_) => return Err(AppError::invalid(format!("{} is already encrypted", path.display()))),
        State::Plain => {}
    }
    let plain = Db::open_plain(path)?;
    crate::migrations::migrate(&plain)?;
    let tables = table_counts(&plain)?;
    let image = plain.export_image()?;
    drop(plain);

    let id = random_array()?;
    let keys = DataKeys::random()?;
    let ns = first_slots(path, o, id, &keys, p)?;
    let header = Header::new(id, ns.slots.clone());
    let sealed = crypto::seal_image(&header, &keys.dek, image)?;
    let tmp = crypto::sidecar(path, ".encrypting");
    crypto::atomic_write(&tmp, &sealed)?;
    if let Err(e) = verify_sealed(&tmp, &keys, &tables) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context("db encrypt aborted; the plaintext database is unchanged"));
    }

    let aside = crypto::sidecar(path, ".plaintext-wipe");
    std::fs::rename(path, &aside).map_err(|e| AppError::io(format!("renaming {}: {e}", path.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| AppError::io(format!("renaming {}: {e}", tmp.display())))?;
    ns.commit()?;
    let mut wiped = Vec::new();
    for f in [aside, crypto::sidecar(path, "-wal"), crypto::sidecar(path, "-shm"), crypto::sidecar(path, "-journal")] {
        if f.exists() {
            crypto::wipe_file(&f)?;
            wiped.push(f.display().to_string());
        }
    }
    drop(lock);
    Ok(EncryptReport { tables, bytes: sealed.len() as u64, key_source: ns.label(), wiped })
}

/// Delete the key file when no file slot is left.
fn forget_dropped(old: &Header, new: &Header) {
    let file = |h: &Header| h.slots.iter().any(|s| s.kind == SlotKind::Raw(Holder::File));
    if file(old) && !file(new) {
        keyfile::forget(&old.db_id);
    }
}

/// Write `path` with `header`, keeping the body (same DEK) after checking it
/// authenticates.
fn swap_header(path: &Path, old: &Header, keys: &DataKeys, header: &Header) -> Result<()> {
    let bytes = std::fs::read(path).map_err(|e| AppError::io(format!("reading {}: {e}", path.display())))?;
    drop(crypto::open_image(old, &keys.dek, bytes.clone())?);
    crypto::atomic_write(path, &crypto::replace_header(&bytes, header)?)
}

pub struct RekeyReport {
    pub from: &'static str,
    pub to: String,
    pub rotated_dek: bool,
    pub db_id: [u8; crypto::ID_LEN],
    pub audit_key: crypto::Key,
    pub header: Header,
}

/// Replace every key slot with new ones from `to` (and with `rotate_dek`
/// re-encrypt the image under a fresh DEK). For the env source the new key
/// is read from `WHEREBOX_NEW_KEY`.
pub fn rekey(path: &Path, o: &OpenOpts, to: Source, rotate_dek: bool, p: &mut dyn Prompter) -> Result<RekeyReport> {
    let _lock = crate::db::lock(path)?;
    let (header, u) = unlock(path, o.source, true, p)?;
    let new_keys =
        if rotate_dek { DataKeys { dek: crypto::Key::random()?, audit: u.keys.audit.clone() } } else { u.keys.clone() };
    let what = disp(path);
    let mut s = Setup { what: &what, config: &o.config, prompter: p };
    let ns = keysetup::new_slots(to, keys::ENV_NEW_KEY, header.db_id, &new_keys, &mut s)?;
    let new_header = header.with_slots(ns.slots.clone());
    if rotate_dek {
        let bytes = std::fs::read(path).map_err(|e| AppError::io(format!("reading {}: {e}", path.display())))?;
        let mut image = crypto::open_image(&header, &u.keys.dek, bytes)?;
        crypto::atomic_write(path, &crypto::seal_image(&new_header, &new_keys.dek, std::mem::take(&mut *image))?)?;
    } else {
        swap_header(path, &header, &u.keys, &new_header)?;
    }
    ns.commit()?;
    forget_dropped(&header, &new_header);
    Ok(RekeyReport {
        from: u.source,
        to: ns.label(),
        rotated_dek: rotate_dek,
        db_id: header.db_id,
        audit_key: new_keys.audit,
        header: new_header,
    })
}

/// New slots for a database plus anything staged that must be committed
/// once they are written.
pub struct Edit {
    pub slots: Vec<Slot>,
    pub staged: Option<NewSlots>,
}

/// Replace the database's slots with what `edit` returns (same data keys,
/// body untouched). `edit` gets the current header, the unlocked keys and
/// the prompter. Returns the new header and what unlocked the database.
pub fn change_slots(
    path: &Path,
    o: &OpenOpts,
    p: &mut dyn Prompter,
    edit: impl FnOnce(&Header, &DataKeys, &mut dyn Prompter) -> Result<Edit>,
) -> Result<(Header, &'static str)> {
    let _lock = crate::db::lock(path)?;
    let (header, u) = unlock(path, o.source, true, p)?;
    let e = edit(&header, &u.keys, p)?;
    if e.slots.is_empty() {
        return Err(AppError::invalid("refusing to remove the last key: the database could never be opened again"));
    }
    let new_header = header.with_slots(e.slots);
    swap_header(path, &header, &u.keys, &new_header)?;
    if let Some(ns) = e.staged {
        ns.commit()?;
    }
    forget_dropped(&header, &new_header);
    Ok((new_header, u.source))
}
