//! Thin synchronous facade over the async fsqlite (FrankenSQLite) connection.
//!
//! fsqlite exposes an async API driven by the `asupersync` runtime. The CLI is
//! a short-lived single-threaded process, so every call is simply
//! `block_on`-ed on a current-thread runtime owned by [`Db`].
//!
//! An encrypted database ([`Db::open_sealed`] / [`Db::create_sealed`]) lives in
//! an in-memory fsqlite connection; every committing statement re-seals the
//! whole image into the container file (see [`crate::crypto`]). Only
//! `--insecure-plaintext` databases ([`Db::open_plain`]) are file-backed.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use asupersync::runtime::{Runtime, RuntimeBuilder};
use fsqlite::{Connection, SqliteValue};

use crate::crypto::{self, DataKeys, Header};
use crate::error::{AppError, Result};

pub type Value = SqliteValue;
pub type Row = Vec<Value>;

/// Key material of an open encrypted database.
pub struct Sealed {
    pub header: Header,
    pub keys: DataKeys,
    /// What opened it ("ssh", "file", "env", ...), for diagnostics.
    pub key_source: &'static str,
}

pub struct Db {
    rt: Runtime,
    conn: Option<Connection>,
    path: PathBuf,
    /// True while an explicit transaction is open; nested `transaction` calls join it.
    in_tx: Cell<bool>,
    /// `Some` for an encrypted (in-memory, sealed-on-commit) database.
    sealed: Option<Sealed>,
    /// Uncommitted-to-disk changes exist in the in-memory image.
    dirty: Cell<bool>,
    dirty_before_tx: Cell<bool>,
    /// Running total of rows changed (for the audit log).
    changes: Rc<Cell<u64>>,
    /// Held for the lifetime of the connection: serialises CLI processes so
    /// whole-image re-sealing cannot lose a concurrent update.
    _lock: Option<std::fs::File>,
}

fn runtime() -> Result<Runtime> {
    RuntimeBuilder::current_thread().build().map_err(|e| AppError::db(format!("starting fsqlite runtime: {e}")))
}

/// Create the database's directory when missing, owner-only (0700 on Unix,
/// an owner-only ACL on Windows) so the sealed files are not even listable by
/// other local users. An existing directory the user chose is left as it is.
fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty() && !p.exists()) {
        std::fs::create_dir_all(parent).map_err(|e| AppError::io(format!("creating {}: {e}", parent.display())))?;
        crate::perms::restrict_dir(parent)
            .map_err(|e| AppError::io(format!("restricting {}: {e}", parent.display())))?;
    }
    Ok(())
}

/// Take the exclusive advisory lock `<db>.lock` (an empty file).
pub fn lock(path: &Path) -> Result<std::fs::File> {
    ensure_parent(path)?;
    let lp = crypto::sidecar(path, ".lock");
    let f = crypto::open_private_append(&lp).map_err(|e| AppError::io(format!("{}: {e}", lp.display())))?;
    f.lock().map_err(|e| AppError::io(format!("locking {}: {e}", lp.display())))?;
    Ok(f)
}

impl Db {
    fn from_conn(rt: Runtime, conn: Connection, path: &Path, sealed: Option<Sealed>) -> Result<Self> {
        let db = Self {
            rt,
            conn: Some(conn),
            path: path.to_path_buf(),
            in_tx: Cell::new(false),
            sealed,
            dirty: Cell::new(false),
            dirty_before_tx: Cell::new(false),
            changes: Rc::new(Cell::new(0)),
            _lock: None,
        };
        db.rt.block_on(db.conn().execute("PRAGMA foreign_keys = ON"))?;
        Ok(db)
    }

    /// Open (creating if needed) a plaintext, file-backed database without
    /// running migrations. Only for `--insecure-plaintext` and migration.
    pub fn open_plain(path: &Path) -> Result<Self> {
        ensure_parent(path)?;
        let rt = runtime()?;
        let p = path.to_string_lossy().into_owned();
        let conn =
            rt.block_on(Connection::open(p)).map_err(|e| AppError::db(format!("opening {}: {e}", path.display())))?;
        Self::from_conn(rt, conn, path, None)
    }

    /// Decrypt the container at `path` into memory.
    pub fn open_sealed(path: &Path, sealed: Sealed) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| AppError::io(format!("reading {}: {e}", path.display())))?;
        let image = crypto::open_image(&sealed.header, &sealed.keys.dek, bytes)?;
        let rt = runtime()?;
        let conn = rt
            .block_on(Connection::import_bytes(&image))
            .map_err(|e| AppError::db(format!("loading decrypted {}: {e}", path.display())))?;
        drop(image);
        Self::from_conn(rt, conn, path, Some(sealed))
    }

    /// A new, empty encrypted database; the container is written on the first commit.
    pub fn create_sealed(path: &Path, sealed: Sealed) -> Result<Self> {
        ensure_parent(path)?;
        let rt = runtime()?;
        let conn = rt.block_on(Connection::open(":memory:")).map_err(|e| AppError::db(format!("in-memory db: {e}")))?;
        let db = Self::from_conn(rt, conn, path, Some(sealed))?;
        db.dirty.set(true);
        Ok(db)
    }

    pub fn with_lock(mut self, lock: std::fs::File) -> Self {
        self._lock = Some(lock);
        self
    }

    pub fn with_change_counter(mut self, c: Rc<Cell<u64>>) -> Self {
        self.changes = c;
        self
    }

    fn conn(&self) -> &Connection {
        self.conn.as_ref().expect("connection is open until drop")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sealed(&self) -> Option<&Sealed> {
        self.sealed.as_ref()
    }

    /// The decrypted SQLite image (encrypted databases only ever hold it in memory).
    pub fn export_image(&self) -> Result<Vec<u8>> {
        self.rt.block_on(self.conn().export_bytes()).map_err(|e| AppError::db(format!("exporting image: {e}")))
    }

    /// Re-seal the in-memory image into the container file if it changed.
    pub fn persist(&self) -> Result<()> {
        let Some(s) = &self.sealed else { return Ok(()) };
        if !self.dirty.get() || self.in_tx.get() {
            return Ok(());
        }
        let sealed = crypto::seal_image(&s.header, &s.keys.dek, self.export_image()?)?;
        crypto::atomic_write(&self.path, &sealed)?;
        self.dirty.set(false);
        Ok(())
    }

    /// Statements through `execute`/`execute_batch` are writes unless they
    /// are plain SELECTs.
    fn note_write(&self, sql: &str, changed: usize) -> Result<()> {
        self.changes.set(self.changes.get() + changed as u64);
        let t = sql.trim_start();
        if t.get(..6).is_some_and(|w| w.eq_ignore_ascii_case("select")) {
            return Ok(());
        }
        self.dirty.set(true);
        self.persist()
    }

    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        let conn = self.conn();
        let r = if params.is_empty() {
            self.rt.block_on(conn.execute(sql))
        } else {
            self.rt.block_on(conn.execute_with_params(sql, params))
        };
        let n = r.map_err(AppError::from)?;
        self.note_write(sql, n)?;
        Ok(n)
    }

    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.rt.block_on(self.conn().execute_batch(sql)).map_err(AppError::from)?;
        self.note_write(sql, 0)
    }

    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let conn = self.conn();
        let rows = if params.is_empty() {
            self.rt.block_on(conn.query(sql))
        } else {
            self.rt.block_on(conn.query_with_params(sql, params))
        }?;
        Ok(rows.into_iter().map(|r| r.values().to_vec()).collect())
    }

    pub fn query_opt(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    pub fn query_scalar_i64(&self, sql: &str, params: &[Value]) -> Result<i64> {
        self.query_opt(sql, params)?
            .and_then(|r| r.first().and_then(as_i64))
            .ok_or_else(|| AppError::db(format!("expected integer result from: {sql}")))
    }

    pub fn last_insert_rowid(&self) -> i64 {
        self.conn().last_insert_rowid()
    }

    pub fn begin(&self) -> Result<()> {
        self.rt.block_on(self.conn().begin_transaction())?;
        self.in_tx.set(true);
        self.dirty_before_tx.set(self.dirty.get());
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.in_tx.set(false);
        self.rt.block_on(self.conn().commit_transaction()).map_err(AppError::from)?;
        self.persist()
    }

    pub fn rollback(&self) -> Result<()> {
        self.in_tx.set(false);
        self.dirty.set(self.dirty_before_tx.get());
        self.rt.block_on(self.conn().rollback_transaction()).map_err(AppError::from)
    }

    /// Run `f` inside a transaction, committing on success and rolling back on
    /// error. When a transaction is already open, `f` simply joins it and the
    /// outer owner decides whether to commit.
    pub fn transaction<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        if self.in_tx.get() {
            return f(self);
        }
        self.begin()?;
        match f(self) {
            Ok(v) => self.commit().map(|()| v),
            Err(e) => {
                let _ = self.rollback();
                Err(e)
            }
        }
    }

    /// Copy the database to `target`: the same encrypted container (same key)
    /// for an encrypted database, an exact page-level copy otherwise.
    pub fn backup_to(&self, target: &Path) -> Result<()> {
        if let Some(s) = &self.sealed {
            let sealed = crypto::seal_image(&s.header, &s.keys.dek, self.export_image()?)?;
            return crypto::atomic_write(target, &sealed);
        }
        self.rt
            .block_on(self.conn().backup_exact_to(target))
            .map(|_| ())
            .map_err(|e| AppError::db(format!("backup to {}: {e}", target.display())))
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = self.rt.block_on(conn.close());
        }
    }
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

pub fn text(s: impl AsRef<str>) -> Value {
    Value::from(s.as_ref())
}

pub fn opt_text<S: AsRef<str>>(s: Option<S>) -> Value {
    s.map_or(Value::Null, text)
}

pub fn real(f: f64) -> Value {
    Value::Float(f)
}

pub fn opt_real(f: Option<f64>) -> Value {
    f.map_or(Value::Null, Value::Float)
}

pub fn int(i: i64) -> Value {
    Value::Integer(i)
}

pub fn opt_int(i: Option<i64>) -> Value {
    i.map_or(Value::Null, Value::Integer)
}

pub fn opt_bool(b: Option<bool>) -> Value {
    b.map_or(Value::Null, |b| Value::Integer(i64::from(b)))
}

pub fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Float(f) => Some(*f as i64),
        Value::Text(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Text(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Text(s) => Some(s.to_string()),
        other => Some(other.to_text()),
    }
}

/// Column accessors for a row.
pub trait RowExt {
    fn s(&self, i: usize) -> Option<String>;
    fn f(&self, i: usize) -> Option<f64>;
    fn i(&self, i: usize) -> Option<i64>;
    fn b(&self, i: usize) -> Option<bool> {
        self.i(i).map(|v| v != 0)
    }
}

impl RowExt for [Value] {
    fn s(&self, i: usize) -> Option<String> {
        self.get(i).and_then(as_string)
    }
    fn f(&self, i: usize) -> Option<f64> {
        self.get(i).and_then(as_f64)
    }
    fn i(&self, i: usize) -> Option<i64> {
        self.get(i).and_then(as_i64)
    }
}
