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

/// Expiry is computed now (docs/SPEC.md, "Shelf life and on hand"): lots
/// remember when they were last confirmed on hand, and `expires_at` is
/// retired (see [`retire_stored_expiry`]).
const SCHEMA_V3: &str = r"
ALTER TABLE lots ADD COLUMN confirmed_at TEXT;
";

pub const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, name: "inventory schema", apply: |db| db.execute_batch(SCHEMA_V1) },
    Migration { version: 2, name: "lot unit price and source ref", apply: |db| db.execute_batch(SCHEMA_V2) },
    Migration {
        version: 3,
        name: "computed expiry: lot confirmed_at, stored expiries retired",
        apply: |db| {
            db.execute_batch(SCHEMA_V3)?;
            retire_stored_expiry(db)
        },
    },
];

/// A built-in preset's old lot fill: (field, fridge, freezer, pantry).
type LegacyFill = (&'static str, Option<&'static str>, Option<&'static str>, Option<&'static str>);

/// The lot date a built-in preset used to fill in before expiry was
/// computed: (field, fridge, freezer, pantry), the shorter end of each range.
fn legacy_fill(preset: &str) -> Option<LegacyFill> {
    Some(match preset {
        "meat.raw" => ("expires_at", Some("3d"), Some("4m"), None),
        "meat.ground" => ("expires_at", Some("1d"), Some("3m"), None),
        "poultry.raw" => ("expires_at", Some("1d"), Some("9m"), None),
        "seafood.raw" => ("expires_at", Some("1d"), Some("2m"), None),
        "leftovers" => ("expires_at", Some("3d"), Some("1m"), None),
        "eggs" => ("best_by", Some("3w"), None, None),
        "bread" => ("best_by", None, Some("3m"), Some("14d")),
        "condiment" => ("best_by", None, None, Some("3m")),
        "frozen" => ("best_by", None, Some("12m"), None),
        "pantry" => ("best_by", None, None, Some("12m")),
        _ => return None,
    })
}

/// What the old preset fill would have stored in `field` for this lot.
fn legacy_date(preset: Option<&str>, field: &str, lot: &crate::store::Lot, path: Option<&str>) -> Option<String> {
    use crate::presets::{Life, Storage};
    let (f, fridge, freezer, pantry) = legacy_fill(preset?)?;
    if f != field {
        return None;
    }
    let of = |s: Storage| match s {
        Storage::Fridge => fridge,
        Storage::Freezer => freezer,
        Storage::Pantry => pantry,
    };
    let storage = if lot.frozen_at.is_some() { Some(Storage::Freezer) } else { path.and_then(Storage::from_path) };
    let storage = storage
        .filter(|s| of(*s).is_some())
        .or_else(|| [Storage::Fridge, Storage::Pantry, Storage::Freezer].into_iter().find(|s| of(*s).is_some()))?;
    let life = Life::parse(of(storage)?).ok()?;
    let acquired = crate::util::date_of(lot.acquired_at.as_deref()?).ok()?;
    let base = match storage {
        Storage::Freezer => lot.frozen_at.as_deref().and_then(|d| crate::util::date_of(d).ok()).unwrap_or(acquired),
        _ => acquired,
    };
    Some(crate::util::date_after(base, life.span()).to_string())
}

/// Drop stored expiries: every date on lots of non-perishable items, and on
/// perishables every date the old preset fill would have computed; an
/// explicit date (one that differs) stays as the printed best-by.
fn retire_stored_expiry(db: &Db) -> Result<()> {
    let rows = db.query(
        "SELECT id, item_id, expires_at, best_by FROM lots WHERE expires_at IS NOT NULL OR best_by IS NOT NULL",
        &[],
    )?;
    if rows.is_empty() {
        return Ok(());
    }
    let catalog = crate::presets::Catalog::builtin();
    let types = crate::shelf_life::Types::builtin();
    let tree = crate::store::Tree::load(db)?;
    let fields = crate::store::all_item_fields(db)?;
    let labels = crate::store::all_item_labels(db)?;
    let none = std::collections::BTreeMap::new();
    for r in rows.iter() {
        let (Some(lot_id), Some(item_id)) = (r.s(0), r.s(1)) else { continue };
        let (expires, best_by) = (r.s(2), r.s(3));
        let Some(lot) = crate::store::get_lot(db, &lot_id)? else { continue };
        let Some(item) = crate::store::get_item(db, &item_id)? else { continue };
        let facts = crate::shelf_life::ItemFacts {
            item: &item,
            fields: fields.get(&item.id).unwrap_or(&none),
            labels: labels.get(&item.id).map_or(&[][..], Vec::as_slice),
            has_lots: true,
            food_place: false,
        };
        let keep = if types.classify(&facts, &catalog).is_some() {
            let path = tree.opt_path(lot.location_id.or(item.location_id));
            let explicit = |field: &str, v: Option<String>| {
                v.filter(|d| {
                    legacy_date(item.kind_preset.as_deref(), field, &lot, path.as_deref()).as_deref() != Some(d)
                })
            };
            explicit("best_by", best_by).or_else(|| explicit("expires_at", expires))
        } else {
            None
        };
        db.execute(
            "UPDATE lots SET expires_at = NULL, best_by = ?1 WHERE id = ?2",
            &[crate::db::opt_text(keep.as_ref()), text(&lot_id)],
        )?;
    }
    Ok(())
}

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
    use crate::store::{self, Item, Tree};

    /// A version-2 database with stored expiries: what migration 3 keeps.
    #[test]
    fn stored_expiries_are_retired() {
        let t = tempfile::TempDir::new().unwrap();
        let db = Db::open_plain(&t.path().join("v2.db")).unwrap();
        ensure_table(&db).unwrap();
        for m in &MIGRATIONS[..2] {
            (m.apply)(&db).unwrap();
            db.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                &[int(m.version), text(m.name), text("2026-01-01T00:00:00Z")],
            )
            .unwrap();
        }
        let mut tree = Tree::load(&db).unwrap();
        let fridge = tree.resolve_or_create(&db, "Home / Kitchen / Fridge").unwrap().0;
        let item = |name: &str, preset: Option<&str>, labels: &[&str]| {
            let mut i = Item::new(name).unwrap();
            i.kind_preset = preset.map(str::to_string);
            i.location_id = Some(fridge);
            store::insert_item(&db, &i).unwrap();
            store::set_item_labels(&db, &i.id, &labels.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
            i.id
        };
        let lot = |id: &str, item: &str, expires: Option<&str>, best_by: Option<&str>| {
            db.execute(
                "INSERT INTO lots (id, item_id, qty, acquired_at, expires_at, best_by, status, created_at) \
                 VALUES (?1, ?2, 1, '2026-01-01', ?3, ?4, 'sealed', '2026-01-01T00:00:00Z')",
                &[text(id), text(item), crate::db::opt_text(expires), crate::db::opt_text(best_by)],
            )
            .unwrap();
        };
        let towels = item("Paper Towels", Some("household"), &["consumable"]);
        let ribeye = item("Ribeye", Some("meat.raw"), &[]);
        let milk = item("Whole Milk", None, &["food"]);
        let rice = item("Basmati Rice", Some("pantry"), &[]);
        lot("T1", &towels, Some("2026-05-01"), Some("2026-06-01"));
        lot("R1", &ribeye, Some("2026-01-04"), None); // what meat.raw filled in (fridge, 3 days)
        lot("R2", &ribeye, Some("2026-01-10"), None); // typed in: kept as the printed date
        lot("M1", &milk, None, Some("2026-01-09"));
        lot("P1", &rice, None, Some("2027-01-01"));
        assert_eq!(migrate(&db).unwrap(), [3]);
        let got = |id: &str| {
            let r = db.query_opt("SELECT expires_at, best_by FROM lots WHERE id = ?1", &[text(id)]).unwrap().unwrap();
            (r.s(0), r.s(1))
        };
        assert_eq!(got("T1"), (None, None), "non-perishable: dropped");
        assert_eq!(got("R1"), (None, None), "computed by the old preset: dropped");
        assert_eq!(got("R2"), (None, Some("2026-01-10".into())), "explicit: the printed best-by");
        assert_eq!(got("M1"), (None, Some("2026-01-09".into())));
        assert_eq!(got("P1"), (None, None), "dry goods never expire");
        assert_eq!(store::get_lot(&db, "R2").unwrap().unwrap().confirmed_at, None);
    }
}
