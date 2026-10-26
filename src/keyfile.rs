//! Key files: the default home of a database's key-encryption key (KEK).
//!
//! Like an ssh private key, a key file is a small owner-only file holding a
//! random 256-bit KEK (hex, one line, `#` comments allowed). It lives under
//! the config directory (`<config dir>/wherebox-cli/keys/<db-id>.key`), not
//! next to the database, so a synced or backed-up data directory never
//! carries its own key. The `key_file` setting / `WHEREBOX_KEY_FILE` points
//! at another file instead.
//!
//! A key file other users can read is refused, the way ssh refuses a shared
//! private key. New key files are written as `<name>.next` and renamed into
//! place once the database sealed with them is on disk ([`commit`]), so a
//! crash in between never loses the key: [`load`] also tries the `.next` file.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::crypto::{hex, unhex, Key, ID_LEN, KEY_LEN};
use crate::error::Result;
use crate::keys::key_error;
use crate::perms::{self, Access};

thread_local! {
    static OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Use `path` as the key file for the databases this thread opens
/// (`key_file` setting); `None` restores the per-database default. Per
/// thread, so tests can run in parallel.
pub fn set_override(path: Option<PathBuf>) {
    OVERRIDE.with(|o| *o.borrow_mut() = path.filter(|p| !p.as_os_str().is_empty()));
}

/// Directory holding the per-database key files.
pub fn default_dir() -> PathBuf {
    crate::paths::default_config_path().parent().map_or_else(|| PathBuf::from("keys"), |d| d.join("keys"))
}

/// The key file for database `db_id`.
pub fn path_for(db_id: &[u8; ID_LEN]) -> PathBuf {
    let o = OVERRIDE.with(|o| o.borrow().clone());
    o.unwrap_or_else(|| default_dir().join(format!("{}.key", hex(db_id))))
}

fn staged(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".next");
    PathBuf::from(s)
}

/// Whether a key file (or a staged one) exists for `db_id`.
pub fn exists(db_id: &[u8; ID_LEN]) -> bool {
    let p = path_for(db_id);
    p.exists() || staged(&p).exists()
}

/// Refuse a key file other users can access.
pub fn check_private(path: &Path) -> Result<()> {
    match perms::inspect(path) {
        Some(Access::Shared(how)) => Err(key_error(format!(
            "key file {} is accessible by other users ({how}); make it owner-only{}",
            path.display(),
            if cfg!(unix) { format!(" (chmod 600 {})", path.display()) } else { String::new() }
        ))),
        _ => Ok(()),
    }
}

fn parse(text: &str, path: &Path) -> Result<Key> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("");
    let bytes = Zeroizing::new(unhex(line).unwrap_or_default());
    Key::from_bytes(&bytes)
        .ok_or_else(|| key_error(format!("key file {} must hold {} hex characters", path.display(), 2 * KEY_LEN)))
}

fn read(path: &Path) -> Result<Option<Key>> {
    if !path.exists() {
        return Ok(None);
    }
    check_private(path)?;
    let text = Zeroizing::new(
        std::fs::read_to_string(path).map_err(|e| key_error(format!("reading key file {}: {e}", path.display())))?,
    );
    parse(&text, path).map(Some)
}

/// The KEKs on disk for `db_id`: the key file, then a staged one left by an
/// interrupted write.
pub fn load(db_id: &[u8; ID_LEN]) -> Result<Vec<Key>> {
    let p = path_for(db_id);
    Ok([read(&p)?, read(&staged(&p))?].into_iter().flatten().collect())
}

fn ensure_dir(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() || dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| key_error(format!("creating {}: {e}", dir.display())))?;
    perms::restrict_dir(dir).map_err(|e| key_error(format!("restricting {}: {e}", dir.display())))
}

/// Write `kek` as the staged key file for `db_id`; [`commit`] makes it live.
pub fn stage(db_id: &[u8; ID_LEN], kek: &Key) -> Result<PathBuf> {
    let path = path_for(db_id);
    ensure_dir(path.parent().unwrap_or(Path::new("")))?;
    let next = staged(&path);
    let body = Zeroizing::new(format!(
        "# wherebox-cli key for database {}; keep it private and back it up separately from the database\n{}\n",
        hex(db_id),
        hex(kek.as_bytes())
    ));
    let write = || -> std::io::Result<()> {
        let mut f = crate::crypto::create_private(&next)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()
    };
    write().map_err(|e| key_error(format!("writing key file {}: {e}", next.display())))?;
    Ok(path)
}

/// Promote the staged key file for `db_id` (after the database sealed with it
/// is durably written).
pub fn commit(db_id: &[u8; ID_LEN]) -> Result<()> {
    let path = path_for(db_id);
    std::fs::rename(staged(&path), &path).map_err(|e| key_error(format!("installing key file {}: {e}", path.display())))
}

/// Delete the key files of `db_id` (after rekeying away from a key file).
pub fn forget(db_id: &[u8; ID_LEN]) {
    let p = path_for(db_id);
    let _ = std::fs::remove_file(staged(&p));
    let _ = std::fs::remove_file(p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comments_and_checks_length() {
        let k = "ab".repeat(KEY_LEN);
        let p = Path::new("k");
        assert!(parse(&format!("# c\n\n{k}\n"), p).is_ok());
        assert!(parse("# only a comment\n", p).is_err());
        assert!(parse("abcd\n", p).is_err());
    }

    #[test]
    fn stage_commit_load_forget() {
        let t = tempfile::TempDir::new().unwrap();
        let file = t.path().join("sub").join("db.key");
        set_override(Some(file.clone()));
        let id = [7u8; ID_LEN];
        let kek = Key::random().unwrap();
        stage(&id, &kek).unwrap();
        assert!(!file.exists() && exists(&id));
        assert_eq!(load(&id).unwrap()[0].as_bytes(), kek.as_bytes());
        commit(&id).unwrap();
        assert!(file.exists());
        if let Some(a) = perms::inspect(&file) {
            assert!(matches!(a, Access::Private(_)), "{a:?}");
        }
        forget(&id);
        assert!(!exists(&id));
        set_override(None);
    }
}
