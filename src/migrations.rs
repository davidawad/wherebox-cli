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

/// Expiry is computed, not stored (docs/SPEC.md, "Shelf life"): items get a
/// shelf-life type, lots a thaw date and a last confirmation.
const SCHEMA_V3: &str = r"
ALTER TABLE items ADD COLUMN shelf_life TEXT;
ALTER TABLE lots ADD COLUMN thawed_at TEXT;
ALTER TABLE lots ADD COLUMN confirmed_at TEXT;
";

/// (preset, fridge, freezer, pantry).
type LegacyLife = (&'static str, Option<&'static str>, Option<&'static str>, Option<&'static str>);

/// The lot dates built-in presets used to fill in before schema v3.
const LEGACY_PRESET_LIFE: &[LegacyLife] = &[
    ("meat.raw", Some("3d"), Some("4m"), None),
    ("meat.ground", Some("1d"), Some("3m"), None),
    ("poultry.raw", Some("1d"), Some("9m"), None),
    ("seafood.raw", Some("1d"), Some("2m"), None),
    ("leftovers", Some("3d"), Some("1m"), None),
    ("eggs", Some("3w"), None, None),
    ("bread", None, Some("3m"), Some("14d")),
    ("frozen", None, Some("12m"), None),
    ("pantry", None, None, Some("12m")),
    ("condiment", None, None, Some("3m")),
];

/// Dates a built-in preset would have filled into a lot.
fn legacy_fills(preset: Option<&str>, acquired: Option<&str>, frozen: Option<&str>) -> Vec<String> {
    let day = |d: Option<&str>| d.and_then(|d| crate::util::date_of(d).ok());
    let (Some((_, fridge, freezer, pantry)), Some(acq)) =
        (LEGACY_PRESET_LIFE.iter().find(|p| Some(p.0) == preset), day(acquired))
    else {
        return Vec::new();
    };
    let after = |base, l: &str| {
        crate::presets::Life::parse(l).ok().map(|l| crate::util::date_after(base, l.span()).to_string())
    };
    [
        fridge.and_then(|l| after(acq, l)),
        pantry.and_then(|l| after(acq, l)),
        freezer.and_then(|l| after(day(frozen).unwrap_or(acq), l)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Schema v3: non-perishables lose stored lot dates; perishables keep one
/// only as a printed best-by override, and only when it is not what the
/// item's preset filled in. `expires_at` is emptied everywhere.
/// Classification uses the built-in shelf-life types.
fn apply_v3(db: &Db) -> Result<()> {
    use crate::db::opt_text;
    db.execute_batch(SCHEMA_V3)?;
    let catalog = crate::presets::Catalog::builtin();
    let types = crate::shelf_life::Types::builtin();
    let items = crate::store::all_items(db)?;
    let fields = crate::store::all_item_fields(db)?;
    let labels = crate::store::all_item_labels(db)?;
    let rows = db.query(
        "SELECT id, item_id, acquired_at, frozen_at, expires_at, best_by FROM lots \
         WHERE expires_at IS NOT NULL OR best_by IS NOT NULL",
        &[],
    )?;
    for r in rows {
        let (Some(id), Some(item_id)) = (r.s(0), r.s(1)) else { continue };
        let Some(item) = items.iter().find(|i| i.id == item_id) else { continue };
        let none = Vec::new();
        let class = crate::shelf_life::classify(
            item,
            fields.get(&item.id),
            labels.get(&item.id).unwrap_or(&none),
            true,
            &catalog,
            &types,
        );
        let stored = r.s(5).or_else(|| r.s(4));
        let keep = class
            .and(stored)
            .filter(|d| !legacy_fills(item.kind_preset.as_deref(), r.s(2).as_deref(), r.s(3).as_deref()).contains(d));
        db.execute(
            "UPDATE lots SET expires_at = NULL, best_by = ?1 WHERE id = ?2",
            &[opt_text(keep.as_ref()), text(id)],
        )?;
    }
    Ok(())
}

pub const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, name: "inventory schema", apply: |db| db.execute_batch(SCHEMA_V1) },
    Migration { version: 2, name: "lot unit price and source ref", apply: |db| db.execute_batch(SCHEMA_V2) },
    Migration { version: 3, name: "computed shelf life", apply: apply_v3 },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_drops_stored_expiry_of_non_perishables_and_preset_fills() {
        let t = tempfile::TempDir::new().unwrap();
        let db = Db::open_plain(&t.path().join("v2.db")).unwrap();
        for m in &MIGRATIONS[..2] {
            (m.apply)(&db).unwrap();
        }
        let now = "2026-01-01T00:00:00Z";
        let item = |id: &str, name: &str, preset: Option<&str>| {
            db.execute(
                "INSERT INTO items (id, import_ref, name, kind_preset, created_at, updated_at) VALUES (?1, ?1, ?2, ?3, ?4, ?4)",
                &[text(id), text(name), crate::db::opt_text(preset), text(now)],
            )
            .unwrap();
        };
        item("T", "Canned Tuna", Some("pantry"));
        item("S", "Dish Soap", Some("household"));
        item("R", "Ribeye", Some("meat.raw"));
        item("M", "Whole Milk", None);
        item("B", "Sourdough", Some("bread"));
        let lot = |id: &str, item: &str, expires: Option<&str>, best_by: Option<&str>| {
            db.execute(
                "INSERT INTO lots (id, item_id, qty, acquired_at, expires_at, best_by, status, created_at) \
                 VALUES (?1, ?2, 1, '2026-01-01', ?3, ?4, 'sealed', ?5)",
                &[text(id), text(item), crate::db::opt_text(expires), crate::db::opt_text(best_by), text(now)],
            )
            .unwrap();
        };
        lot("T1", "T", Some("2027-01-01"), Some("2027-06-01")); // non-perishable: both dropped
        lot("S1", "S", None, Some("2028-01-01")); // non-perishable
        lot("R1", "R", Some("2026-01-04"), None); // what meat.raw filled in (fridge 3 days): dropped
        lot("R2", "R", Some("2026-01-10"), None); // typed by hand: kept as a printed date
        lot("M1", "M", None, Some("2026-01-09")); // printed: kept
        lot("B1", "B", None, Some("2026-01-15")); // bread's pantry fill (14 days): dropped
        apply_v3(&db).unwrap();
        let got: Vec<(String, Option<String>, Option<String>)> = db
            .query("SELECT id, expires_at, best_by FROM lots ORDER BY id", &[])
            .unwrap()
            .iter()
            .map(|r| (r.s(0).unwrap(), r.s(1), r.s(2)))
            .collect();
        let want = |id: &str, b: Option<&str>| (id.to_string(), None, b.map(str::to_string));
        assert_eq!(
            got,
            [
                want("B1", None),
                want("M1", Some("2026-01-09")),
                want("R1", None),
                want("R2", Some("2026-01-10")),
                want("S1", None),
                want("T1", None),
            ]
        );
    }
}
