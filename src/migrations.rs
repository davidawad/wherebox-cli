//! Versioned schema migrations.
//!
//! Each migration runs once, inside a transaction, and is recorded in
//! `schema_migrations`. `PRAGMA user_version` mirrors the latest version so
//! external tools can inspect it cheaply.

use crate::db::{int, text, Db, RowExt};
use crate::error::Result;

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub apply: fn(&Db) -> Result<()>,
}

/// The inventory schema. Item and lot ids are ULIDs (text); everything else
/// uses integer keys. Money is whole USD cents, dates are `YYYY-MM-DD`.
const SCHEMA_V1: &str = r"
CREATE TABLE IF NOT EXISTS locations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    parent_id   INTEGER REFERENCES locations(id),
    kind        TEXT NOT NULL DEFAULT 'room',
    address     TEXT,
    notes       TEXT,
    created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS items (
    id                TEXT PRIMARY KEY,
    import_ref        TEXT NOT NULL UNIQUE,
    asset_id          TEXT UNIQUE,
    name              TEXT NOT NULL,
    description       TEXT,
    quantity          INTEGER NOT NULL DEFAULT 1,
    location_id       INTEGER REFERENCES locations(id),
    parent_item_id    TEXT REFERENCES items(id),
    manufacturer      TEXT,
    model_number      TEXT,
    serial_number     TEXT,
    purchase_from     TEXT,
    purchase_price    INTEGER,
    purchase_time     TEXT,
    insured           INTEGER NOT NULL DEFAULT 0,
    lifetime_warranty INTEGER NOT NULL DEFAULT 0,
    warranty_expires  TEXT,
    warranty_details  TEXT,
    sold_to           TEXT,
    sold_time         TEXT,
    sold_price        INTEGER,
    sold_notes        TEXT,
    notes             TEXT,
    owner_entity      TEXT,
    replacement_value INTEGER,
    valued_at         TEXT,
    status            TEXT NOT NULL DEFAULT 'owned',
    kind_preset       TEXT,
    unit              TEXT,
    par_level         REAL,
    reorder_qty       REAL,
    preferred_vendor  TEXT,
    product_ref       TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS labels (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS item_labels (
    item_id     TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    label_id    INTEGER NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, label_id)
);
CREATE TABLE IF NOT EXISTS item_fields (
    item_id     TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    value       TEXT NOT NULL,
    PRIMARY KEY (item_id, name)
);
CREATE TABLE IF NOT EXISTS attachments (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id     TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL DEFAULT 'other',
    path        TEXT NOT NULL,
    filename    TEXT,
    sha256      TEXT NOT NULL,
    bytes       INTEGER NOT NULL DEFAULT 0,
    encrypted   INTEGER NOT NULL DEFAULT 0,
    is_primary  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    UNIQUE (item_id, sha256)
);
CREATE TABLE IF NOT EXISTS sources (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id     TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    source      TEXT NOT NULL,
    source_ref  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    UNIQUE (item_id, source, source_ref)
);
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id     TEXT NOT NULL,
    at          TEXT NOT NULL,
    kind        TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS lots (
    id          TEXT PRIMARY KEY,
    item_id     TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    qty         REAL NOT NULL,
    acquired_at TEXT,
    opened_at   TEXT,
    expires_at  TEXT,
    best_by     TEXT,
    frozen_at   TEXT,
    location_id INTEGER REFERENCES locations(id),
    status      TEXT NOT NULL DEFAULT 'sealed',
    created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS consumption (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    lot_id      TEXT NOT NULL REFERENCES lots(id) ON DELETE CASCADE,
    qty         REAL NOT NULL,
    at          TEXT NOT NULL,
    reason      TEXT NOT NULL,
    note        TEXT
);
CREATE TABLE IF NOT EXISTS import_batches (
    id          TEXT PRIMARY KEY,
    source      TEXT,
    format      TEXT,
    created_at  TEXT NOT NULL,
    row_count   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_items_location ON items(location_id);
CREATE INDEX IF NOT EXISTS idx_items_status ON items(status);
CREATE INDEX IF NOT EXISTS idx_locations_parent ON locations(parent_id);
CREATE INDEX IF NOT EXISTS idx_events_item ON events(item_id);
CREATE INDEX IF NOT EXISTS idx_lots_item ON lots(item_id);
CREATE INDEX IF NOT EXISTS idx_consumption_lot ON consumption(lot_id);
";

/// Lots remember what one unit cost and the purchase row they came from
/// (an import ref such as `amazon:<order>:<ASIN>`), so a consumable bought
/// many times is one item with a lot per purchase and re-imports match.
const SCHEMA_V2: &str = r"
ALTER TABLE lots ADD COLUMN unit_price INTEGER;
ALTER TABLE lots ADD COLUMN source_ref TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_lots_source_ref ON lots(source_ref) WHERE source_ref IS NOT NULL;
";

pub const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, name: "inventory schema", apply: |db| db.execute_batch(SCHEMA_V1) },
    Migration { version: 2, name: "lot unit price and source ref", apply: |db| db.execute_batch(SCHEMA_V2) },
];

pub fn latest_version() -> i64 {
    MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

fn ensure_table(db: &Db) -> Result<()> {
    // Check first: a no-op CREATE would still count as a write and re-seal
    // an encrypted database on every read-only command.
    if db.query_opt("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'", &[])?.is_some() {
        return Ok(());
    }
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
        );",
    )
}

/// Applied migrations as (version, name, applied_at).
pub fn applied(db: &Db) -> Result<Vec<(i64, String, String)>> {
    ensure_table(db)?;
    Ok(db
        .query("SELECT version, name, applied_at FROM schema_migrations ORDER BY version", &[])?
        .iter()
        .map(|r| (r.i(0).unwrap_or_default(), r.s(1).unwrap_or_default(), r.s(2).unwrap_or_default()))
        .collect())
}

pub fn current_version(db: &Db) -> Result<i64> {
    Ok(applied(db)?.iter().map(|a| a.0).max().unwrap_or(0))
}

/// Apply all pending migrations; returns the versions that were applied.
pub fn migrate(db: &Db) -> Result<Vec<i64>> {
    let current = current_version(db)?;
    if current > latest_version() {
        return Err(crate::error::AppError::db(format!(
            "database schema version {current} is newer than this binary supports ({})",
            latest_version()
        )));
    }
    MIGRATIONS
        .iter()
        .filter(|m| m.version > current)
        .map(|m| {
            db.transaction(|db| {
                (m.apply)(db)?;
                db.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                    &[int(m.version), text(m.name), text(crate::util::now_iso())],
                )?;
                Ok(m.version)
            })
        })
        .collect::<Result<Vec<_>>>()
        .and_then(|v| {
            if !v.is_empty() {
                db.execute(&format!("PRAGMA user_version = {}", latest_version()), &[])?;
            }
            Ok(v)
        })
}
