//! Data access: items, the location tree, labels, custom fields, sources,
//! events, lots, consumption and attachments.
//!
//! A personal inventory is small (thousands of rows, not millions), so lists
//! are loaded whole and filtered in Rust; SQL stays simple and portable
//! across fsqlite releases.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::Value as Json;

use crate::db::{int, opt_int, opt_real, opt_text, real, text, Db, Row, RowExt, Value};
use crate::error::{AppError, Result};
use crate::util::now_iso;

/// Item lifecycle states. The first three are "on hand".
pub const STATUSES: &[&str] = &["owned", "lent", "stored", "disposed", "sold", "lost", "gifted"];
/// Statuses that mean the item is gone (hidden from `ls` by default).
pub const GONE: &[&str] = &["disposed", "sold", "lost", "gifted"];
pub const LOCATION_KINDS: &[&str] = &["property", "room", "container", "shelf"];
pub const ATTACHMENT_KINDS: &[&str] = &["photo", "receipt", "manual", "warranty", "other"];
pub const LOT_STATUSES: &[&str] = &["sealed", "open", "consumed", "discarded"];
pub const CONSUMPTION_REASONS: &[&str] = &["eaten", "used", "spoiled", "discarded"];
/// Separator of location path segments (`Lake House / Garage / Shelf 2`), as in Homebox.
pub const PATH_SEP: &str = " / ";

pub fn check_choice(what: &str, v: &str, choices: &[&str]) -> Result<String> {
    let l = v.trim().to_ascii_lowercase();
    if choices.contains(&l.as_str()) {
        Ok(l)
    } else {
        Err(AppError::invalid(format!("unknown {what} '{v}' (expected one of: {})", choices.join(", "))))
    }
}

fn unique_err(e: AppError, what: impl FnOnce() -> String) -> AppError {
    if e.message.contains("UNIQUE") {
        AppError::invalid(what())
    } else {
        e
    }
}

// ---------------------------------------------------------------------------
// Locations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Location {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    pub kind: String,
    pub address: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
}

/// Default kind for a new location at `depth` (0 = top level).
pub fn default_kind(depth: usize) -> &'static str {
    match depth {
        0 => "property",
        1 => "room",
        _ => "container",
    }
}

/// Split `A / B / C` (or `A/B/C`) into trimmed, non-empty segments.
pub fn split_path(s: &str) -> Vec<String> {
    s.split('/').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect()
}

/// The whole location tree, loaded once per command.
#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub locs: Vec<Location>,
}

impl Tree {
    pub fn load(db: &Db) -> Result<Self> {
        let locs = db
            .query("SELECT id, name, parent_id, kind, address, notes, created_at FROM locations ORDER BY id", &[])?
            .iter()
            .map(|r| Location {
                id: r.i(0).unwrap_or_default(),
                name: r.s(1).unwrap_or_default(),
                parent_id: r.i(2),
                kind: r.s(3).unwrap_or_else(|| "room".into()),
                address: r.s(4),
                notes: r.s(5),
                created_at: r.s(6).unwrap_or_default(),
            })
            .collect();
        Ok(Self { locs })
    }

    pub fn get(&self, id: i64) -> Option<&Location> {
        self.locs.iter().find(|l| l.id == id)
    }

    /// Children of `parent` (`None` = top level), sorted by name.
    pub fn children(&self, parent: Option<i64>) -> Vec<&Location> {
        let mut c: Vec<&Location> = self.locs.iter().filter(|l| l.parent_id == parent).collect();
        c.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        c
    }

    /// Ancestors from the root down to `id` (inclusive).
    pub fn lineage(&self, id: i64) -> Vec<&Location> {
        let mut out = Vec::new();
        let mut cur = self.get(id);
        while let Some(l) = cur {
            if out.iter().any(|o: &&Location| o.id == l.id) {
                break; // defensive: a cycle would otherwise loop forever
            }
            out.push(l);
            cur = l.parent_id.and_then(|p| self.get(p));
        }
        out.reverse();
        out
    }

    /// Full path `A / B / C` of a location.
    pub fn path(&self, id: i64) -> String {
        self.lineage(id).iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join(PATH_SEP)
    }

    pub fn opt_path(&self, id: Option<i64>) -> Option<String> {
        id.map(|i| self.path(i))
    }

    pub fn depth(&self, id: i64) -> usize {
        self.lineage(id).len().saturating_sub(1)
    }

    /// The top-level ancestor (the property) of a location.
    pub fn root_of(&self, id: i64) -> Option<&Location> {
        self.lineage(id).into_iter().next()
    }

    /// `id` and every location below it.
    pub fn subtree(&self, id: i64) -> Vec<i64> {
        let mut out = vec![id];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i];
            out.extend(self.locs.iter().filter(|l| l.parent_id == Some(cur)).map(|l| l.id));
            i += 1;
        }
        out
    }

    fn child_named(&self, parent: Option<i64>, name: &str) -> Option<&Location> {
        self.locs.iter().find(|l| l.parent_id == parent && l.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Exact path lookup (case-insensitive per segment).
    pub fn find_path(&self, path: &str) -> Option<i64> {
        let segs = split_path(path);
        if segs.is_empty() {
            return None;
        }
        segs.iter().try_fold(None, |parent, seg| self.child_named(parent, seg).map(|l| Some(l.id))).flatten()
    }

    /// Resolve a location by full path, else by a unique name or path suffix
    /// anywhere in the tree (`Garage` or `House / Garage`).
    pub fn resolve(&self, spec: &str) -> Result<i64> {
        if let Some(id) = self.find_path(spec) {
            return Ok(id);
        }
        if let Some(id) = spec.trim().strip_prefix('#').and_then(|n| n.parse::<i64>().ok()) {
            return self.get(id).map(|l| l.id).ok_or_else(|| AppError::not_found(format!("no location #{id}")));
        }
        let segs = split_path(spec);
        let suffix_matches: Vec<i64> = self
            .locs
            .iter()
            .filter(|l| {
                let lin = self.lineage(l.id);
                lin.len() >= segs.len()
                    && lin[lin.len() - segs.len()..].iter().zip(&segs).all(|(a, b)| a.name.eq_ignore_ascii_case(b))
            })
            .map(|l| l.id)
            .collect();
        match suffix_matches.as_slice() {
            [id] => Ok(*id),
            [] => Err(AppError::not_found(format!("no location '{spec}' (see: wherebox loc ls)"))),
            many => Err(AppError::invalid(format!(
                "location '{spec}' is ambiguous: {}",
                many.iter().map(|i| self.path(*i)).collect::<Vec<_>>().join("; ")
            ))),
        }
    }

    /// For commands typed by a user: an existing location by path or unique
    /// suffix (`--at Fridge`), else `path` created, under the property when
    /// the database holds exactly one.
    pub fn resolve_or_create(&mut self, db: &Db, path: &str) -> Result<(i64, usize)> {
        match self.resolve(path) {
            Ok(id) => Ok((id, 0)),
            Err(e) if e.kind == crate::error::ErrorKind::NotFound => {
                // In a single-property database new places go under the property.
                let roots = self.children(None);
                let first = split_path(path).into_iter().next().unwrap_or_default();
                match roots.as_slice() {
                    [root] if root.kind == "property" && !root.name.eq_ignore_ascii_case(&first) => {
                        let full = format!("{}{PATH_SEP}{}", root.name, split_path(path).join(PATH_SEP));
                        self.ensure_path(db, &full, None)
                    }
                    _ => self.ensure_path(db, path, None),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Find or create every segment of `path`. The leaf gets `leaf_kind` when
    /// it is created; intermediate segments get the default kind for their
    /// depth. Returns the leaf id and how many locations were created.
    pub fn ensure_path(&mut self, db: &Db, path: &str, leaf_kind: Option<&str>) -> Result<(i64, usize)> {
        let segs = split_path(path);
        if segs.is_empty() {
            return Err(AppError::invalid("empty location path"));
        }
        let mut parent = None;
        let mut created = 0;
        for (depth, seg) in segs.iter().enumerate() {
            parent = Some(match self.child_named(parent, seg) {
                Some(l) => l.id,
                None => {
                    let kind = if depth + 1 == segs.len() {
                        leaf_kind.unwrap_or(default_kind(depth))
                    } else {
                        default_kind(depth)
                    };
                    let loc = insert_location(db, seg, parent, kind, None)?;
                    self.locs.push(loc.clone());
                    created += 1;
                    loc.id
                }
            });
        }
        Ok((parent.expect("non-empty path"), created))
    }
}

pub fn insert_location(
    db: &Db,
    name: &str,
    parent: Option<i64>,
    kind: &str,
    address: Option<&str>,
) -> Result<Location> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') {
        return Err(AppError::invalid(format!("invalid location name '{name}' (must be non-empty, without '/')")));
    }
    let created_at = now_iso();
    db.execute(
        "INSERT INTO locations (name, parent_id, kind, address, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        &[text(name), opt_int(parent), text(kind), opt_text(address), text(&created_at)],
    )?;
    Ok(Location {
        id: db.last_insert_rowid(),
        name: name.to_string(),
        parent_id: parent,
        kind: kind.to_string(),
        address: address.map(str::to_string),
        notes: None,
        created_at,
    })
}

pub fn update_location(db: &Db, l: &Location) -> Result<()> {
    db.execute(
        "UPDATE locations SET name = ?1, parent_id = ?2, kind = ?3, address = ?4, notes = ?5 WHERE id = ?6",
        &[
            text(&l.name),
            opt_int(l.parent_id),
            text(&l.kind),
            opt_text(l.address.as_ref()),
            opt_text(l.notes.as_ref()),
            int(l.id),
        ],
    )
    .map(|_| ())
}

pub fn delete_location(db: &Db, id: i64) -> Result<()> {
    db.execute("DELETE FROM locations WHERE id = ?1", &[int(id)]).map(|_| ())
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// One row of `items`. Money is in cents; dates are `YYYY-MM-DD`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub import_ref: String,
    pub asset_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub quantity: i64,
    pub location_id: Option<i64>,
    pub parent_item_id: Option<String>,
    pub manufacturer: Option<String>,
    pub model_number: Option<String>,
    pub serial_number: Option<String>,
    pub purchase_from: Option<String>,
    pub purchase_price: Option<i64>,
    pub purchase_time: Option<String>,
    pub insured: bool,
    pub lifetime_warranty: bool,
    pub warranty_expires: Option<String>,
    pub warranty_details: Option<String>,
    pub sold_to: Option<String>,
    pub sold_time: Option<String>,
    pub sold_price: Option<i64>,
    pub sold_notes: Option<String>,
    pub notes: Option<String>,
    pub owner_entity: Option<String>,
    pub replacement_value: Option<i64>,
    pub valued_at: Option<String>,
    pub status: String,
    pub kind_preset: Option<String>,
    pub unit: Option<String>,
    pub par_level: Option<f64>,
    pub reorder_qty: Option<f64>,
    pub preferred_vendor: Option<String>,
    pub product_ref: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Item {
    /// A new item with a fresh id (import_ref = id) and defaults.
    pub fn new(name: &str) -> Result<Self> {
        let id = crate::ulid::new()?;
        let now = now_iso();
        Ok(Self {
            import_ref: id.clone(),
            id,
            name: name.trim().to_string(),
            quantity: 1,
            status: "owned".into(),
            created_at: now.clone(),
            updated_at: now,
            ..Default::default()
        })
    }

    /// Insurance/display value: the replacement value, else the purchase price.
    pub fn value(&self) -> Option<i64> {
        self.replacement_value.or(self.purchase_price)
    }

    pub fn is_gone(&self) -> bool {
        GONE.contains(&self.status.as_str())
    }

    /// Equal apart from bookkeeping timestamps.
    pub fn same_content(&self, other: &Self) -> bool {
        let strip = |i: &Self| Self { created_at: String::new(), updated_at: String::new(), ..i.clone() };
        strip(self) == strip(other)
    }
}

const ITEM_COLS: &str = "id, import_ref, asset_id, name, description, quantity, location_id, parent_item_id, \
    manufacturer, model_number, serial_number, purchase_from, purchase_price, purchase_time, insured, \
    lifetime_warranty, warranty_expires, warranty_details, sold_to, sold_time, sold_price, sold_notes, notes, \
    owner_entity, replacement_value, valued_at, status, kind_preset, unit, par_level, reorder_qty, \
    preferred_vendor, product_ref, created_at, updated_at";
const ITEM_NCOLS: usize = 35;

fn item_from(r: &Row) -> Item {
    Item {
        id: r.s(0).unwrap_or_default(),
        import_ref: r.s(1).unwrap_or_default(),
        asset_id: r.s(2),
        name: r.s(3).unwrap_or_default(),
        description: r.s(4),
        quantity: r.i(5).unwrap_or(1),
        location_id: r.i(6),
        parent_item_id: r.s(7),
        manufacturer: r.s(8),
        model_number: r.s(9),
        serial_number: r.s(10),
        purchase_from: r.s(11),
        purchase_price: r.i(12),
        purchase_time: r.s(13),
        insured: r.b(14).unwrap_or(false),
        lifetime_warranty: r.b(15).unwrap_or(false),
        warranty_expires: r.s(16),
        warranty_details: r.s(17),
        sold_to: r.s(18),
        sold_time: r.s(19),
        sold_price: r.i(20),
        sold_notes: r.s(21),
        notes: r.s(22),
        owner_entity: r.s(23),
        replacement_value: r.i(24),
        valued_at: r.s(25),
        status: r.s(26).unwrap_or_else(|| "owned".into()),
        kind_preset: r.s(27),
        unit: r.s(28),
        par_level: r.f(29),
        reorder_qty: r.f(30),
        preferred_vendor: r.s(31),
        product_ref: r.s(32),
        created_at: r.s(33).unwrap_or_default(),
        updated_at: r.s(34).unwrap_or_default(),
    }
}

fn item_params(i: &Item) -> Vec<Value> {
    vec![
        text(&i.id),
        text(&i.import_ref),
        opt_text(i.asset_id.as_ref()),
        text(&i.name),
        opt_text(i.description.as_ref()),
        int(i.quantity),
        opt_int(i.location_id),
        opt_text(i.parent_item_id.as_ref()),
        opt_text(i.manufacturer.as_ref()),
        opt_text(i.model_number.as_ref()),
        opt_text(i.serial_number.as_ref()),
        opt_text(i.purchase_from.as_ref()),
        opt_int(i.purchase_price),
        opt_text(i.purchase_time.as_ref()),
        int(i64::from(i.insured)),
        int(i64::from(i.lifetime_warranty)),
        opt_text(i.warranty_expires.as_ref()),
        opt_text(i.warranty_details.as_ref()),
        opt_text(i.sold_to.as_ref()),
        opt_text(i.sold_time.as_ref()),
        opt_int(i.sold_price),
        opt_text(i.sold_notes.as_ref()),
        opt_text(i.notes.as_ref()),
        opt_text(i.owner_entity.as_ref()),
        opt_int(i.replacement_value),
        opt_text(i.valued_at.as_ref()),
        text(&i.status),
        opt_text(i.kind_preset.as_ref()),
        opt_text(i.unit.as_ref()),
        opt_real(i.par_level),
        opt_real(i.reorder_qty),
        opt_text(i.preferred_vendor.as_ref()),
        opt_text(i.product_ref.as_ref()),
        text(&i.created_at),
        text(&i.updated_at),
    ]
}

fn placeholders(n: usize) -> String {
    (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ")
}

fn validate_item(i: &Item) -> Result<()> {
    if i.name.trim().is_empty() {
        return Err(AppError::invalid("an item needs a name"));
    }
    if i.quantity < 0 {
        return Err(AppError::invalid("quantity cannot be negative"));
    }
    if i.import_ref.chars().count() > 100 {
        return Err(AppError::invalid(format!("import ref '{}' is longer than 100 characters", i.import_ref)));
    }
    check_choice("status", &i.status, STATUSES)?;
    for (what, v) in [("notes", &i.notes), ("sold notes", &i.sold_notes)] {
        if v.as_ref().is_some_and(|n| n.chars().count() > 1000) {
            return Err(AppError::invalid(format!("{what} of '{}' exceed 1000 characters", i.name)));
        }
    }
    Ok(())
}

pub fn insert_item(db: &Db, i: &Item) -> Result<()> {
    validate_item(i)?;
    db.execute(&format!("INSERT INTO items ({ITEM_COLS}) VALUES ({})", placeholders(ITEM_NCOLS)), &item_params(i))
        .map_err(|e| {
            unique_err(e, || {
                format!(
                    "an item with import ref '{}' or asset id '{}' already exists",
                    i.import_ref,
                    i.asset_id.as_deref().unwrap_or("")
                )
            })
        })
        .map(|_| ())
}

pub fn update_item(db: &Db, i: &Item) -> Result<()> {
    validate_item(i)?;
    let sets = ITEM_COLS.split(", ").enumerate().skip(1).map(|(n, c)| format!("{c} = ?{}", n + 1)).collect::<Vec<_>>();
    db.execute(&format!("UPDATE items SET {} WHERE id = ?1", sets.join(", ")), &item_params(i))
        .map_err(|e| {
            unique_err(e, || format!("import ref '{}' or asset id is already used by another item", i.import_ref))
        })
        .map(|_| ())
}

pub fn get_item(db: &Db, id: &str) -> Result<Option<Item>> {
    Ok(db.query_opt(&format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"), &[text(id)])?.map(|r| item_from(&r)))
}

pub fn item_by_ref(db: &Db, import_ref: &str) -> Result<Option<Item>> {
    Ok(db
        .query_opt(&format!("SELECT {ITEM_COLS} FROM items WHERE import_ref = ?1"), &[text(import_ref)])?
        .map(|r| item_from(&r)))
}

/// Every item, ordered by id (creation order).
pub fn all_items(db: &Db) -> Result<Vec<Item>> {
    Ok(db.query(&format!("SELECT {ITEM_COLS} FROM items ORDER BY id"), &[])?.iter().map(item_from).collect())
}

/// Resolve `spec` to one item: id (or a unique id prefix or suffix of 6+
/// characters),
/// asset id, import ref, a unique case-insensitive name, or a unique part of
/// the name of an item still on hand.
pub fn resolve_item(db: &Db, spec: &str) -> Result<Item> {
    let s = spec.trim();
    if let Some(i) = get_item(db, &s.to_ascii_uppercase())? {
        return Ok(i);
    }
    if let Some(i) = item_by_ref(db, s)? {
        return Ok(i);
    }
    let all = all_items(db)?;
    let pick = |hits: Vec<&Item>, how: &str| -> Option<Result<Item>> {
        match hits.as_slice() {
            [] => None,
            [one] => Some(Ok((*one).clone())),
            many => Some(Err(AppError::invalid(format!(
                "'{s}' matches {} items by {how}: {} (use the id or asset id)",
                many.len(),
                many.iter()
                    .map(|i| format!("{} {}", i.asset_id.as_deref().unwrap_or(&i.id), i.name))
                    .collect::<Vec<_>>()
                    .join("; ")
            )))),
        }
    };
    let up = s.to_ascii_uppercase();
    pick(all.iter().filter(|i| i.asset_id.as_deref() == Some(s)).collect(), "asset id")
        .or_else(|| {
            (s.len() >= 6)
                .then(|| {
                    pick(
                        all.iter().filter(|i| i.id.starts_with(&up) || i.id.ends_with(&up)).collect(),
                        "id prefix/suffix",
                    )
                })
                .flatten()
        })
        .or_else(|| pick(all.iter().filter(|i| i.name.eq_ignore_ascii_case(s)).collect(), "name"))
        .or_else(|| {
            let low = s.to_lowercase();
            pick(all.iter().filter(|i| !i.is_gone() && i.name.to_lowercase().contains(&low)).collect(), "name")
        })
        .unwrap_or_else(|| Err(AppError::not_found(format!("no item '{s}' (see: wherebox find {s})"))))
}

/// Next free asset id: one past the highest `NNN-NNN` in use (`000-001` first).
pub fn next_asset_id(db: &Db) -> Result<String> {
    let max = db
        .query("SELECT asset_id FROM items WHERE asset_id IS NOT NULL", &[])?
        .iter()
        .filter_map(|r| r.s(0))
        .filter_map(|a| parse_asset_id(&a))
        .max()
        .unwrap_or(0);
    Ok(format_asset_id(max + 1))
}

pub fn parse_asset_id(s: &str) -> Option<u64> {
    let t = s.trim();
    let digits: String = t.chars().filter(|c| *c != '-').collect();
    (!digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && t.chars().all(|c| c.is_ascii_digit() || c == '-'))
    .then(|| digits.parse().ok())
    .flatten()
}

pub fn format_asset_id(n: u64) -> String {
    let s = format!("{n:06}");
    let (a, b) = s.split_at(s.len() - 3);
    format!("{a}-{b}")
}

/// Hard-delete an item and everything attached to it (children are detached).
pub fn delete_item(db: &Db, id: &str) -> Result<()> {
    db.transaction(|db| {
        for lot in lots_for(db, id)? {
            db.execute("DELETE FROM consumption WHERE lot_id = ?1", &[text(&lot.id)])?;
        }
        for t in ["lots", "item_labels", "item_fields", "attachments", "sources", "events"] {
            db.execute(&format!("DELETE FROM {t} WHERE item_id = ?1"), &[text(id)])?;
        }
        db.execute("UPDATE items SET parent_item_id = NULL WHERE parent_item_id = ?1", &[text(id)])?;
        db.execute("DELETE FROM items WHERE id = ?1", &[text(id)]).map(|_| ())
    })
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Label {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
}

pub fn list_labels(db: &Db) -> Result<Vec<Label>> {
    Ok(db
        .query("SELECT id, name, description FROM labels ORDER BY name", &[])?
        .iter()
        .map(|r| Label { id: r.i(0).unwrap_or_default(), name: r.s(1).unwrap_or_default(), description: r.s(2) })
        .collect())
}

pub fn find_label(db: &Db, name: &str) -> Result<Option<Label>> {
    Ok(list_labels(db)?.into_iter().find(|l| l.name.eq_ignore_ascii_case(name.trim())))
}

/// Find or create a label; returns its id and whether it was created.
pub fn ensure_label(db: &Db, name: &str, description: Option<&str>) -> Result<(i64, bool)> {
    let n = name.trim();
    if n.is_empty() || n.contains(',') {
        return Err(AppError::invalid(format!("invalid label '{name}' (non-empty, no commas)")));
    }
    if let Some(l) = find_label(db, n)? {
        return Ok((l.id, false));
    }
    db.execute(
        "INSERT INTO labels (name, description, created_at) VALUES (?1, ?2, ?3)",
        &[text(n), opt_text(description), text(now_iso())],
    )?;
    Ok((db.last_insert_rowid(), true))
}

pub fn delete_label(db: &Db, id: i64) -> Result<()> {
    db.execute("DELETE FROM item_labels WHERE label_id = ?1", &[int(id)])?;
    db.execute("DELETE FROM labels WHERE id = ?1", &[int(id)]).map(|_| ())
}

/// Labels of every item, sorted by name: item id -> names.
pub fn all_item_labels(db: &Db) -> Result<HashMap<String, Vec<String>>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    db.query("SELECT il.item_id, l.name FROM item_labels il JOIN labels l ON l.id = il.label_id ORDER BY l.name", &[])?
        .iter()
        .for_each(|r| m.entry(r.s(0).unwrap_or_default()).or_default().push(r.s(1).unwrap_or_default()));
    Ok(m)
}

pub fn item_labels(db: &Db, id: &str) -> Result<Vec<String>> {
    Ok(db
        .query("SELECT l.name FROM item_labels il JOIN labels l ON l.id = il.label_id WHERE il.item_id = ?1 ORDER BY l.name", &[text(id)])?
        .iter()
        .filter_map(|r| r.s(0))
        .collect())
}

/// Replace an item's labels; returns how many labels were newly created.
pub fn set_item_labels(db: &Db, id: &str, names: &[String]) -> Result<usize> {
    db.execute("DELETE FROM item_labels WHERE item_id = ?1", &[text(id)])?;
    let mut created = 0;
    for n in names.iter().filter(|n| !n.trim().is_empty()) {
        let (lid, new) = ensure_label(db, n, None)?;
        created += usize::from(new);
        db.execute("INSERT OR IGNORE INTO item_labels (item_id, label_id) VALUES (?1, ?2)", &[text(id), int(lid)])?;
    }
    Ok(created)
}

// ---------------------------------------------------------------------------
// Custom fields
// ---------------------------------------------------------------------------

pub fn all_item_fields(db: &Db) -> Result<HashMap<String, BTreeMap<String, String>>> {
    let mut m: HashMap<String, BTreeMap<String, String>> = HashMap::new();
    db.query("SELECT item_id, name, value FROM item_fields", &[])?.iter().for_each(|r| {
        m.entry(r.s(0).unwrap_or_default()).or_default().insert(r.s(1).unwrap_or_default(), r.s(2).unwrap_or_default());
    });
    Ok(m)
}

pub fn item_fields(db: &Db, id: &str) -> Result<BTreeMap<String, String>> {
    Ok(db
        .query("SELECT name, value FROM item_fields WHERE item_id = ?1", &[text(id)])?
        .iter()
        .map(|r| (r.s(0).unwrap_or_default(), r.s(1).unwrap_or_default()))
        .collect())
}

/// Set (or with `None` / empty, remove) one custom field.
pub fn set_item_field(db: &Db, id: &str, name: &str, value: Option<&str>) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::invalid("custom field names cannot be empty"));
    }
    db.execute("DELETE FROM item_fields WHERE item_id = ?1 AND name = ?2", &[text(id), text(name)])?;
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => db
            .execute(
                "INSERT INTO item_fields (item_id, name, value) VALUES (?1, ?2, ?3)",
                &[text(id), text(name), text(v)],
            )
            .map(|_| ()),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Source {
    pub source: String,
    #[serde(rename = "ref")]
    pub source_ref: String,
}

impl Source {
    /// Parse `source` or `source:ref` (e.g. `amazon-email:113-55-01`).
    pub fn parse(s: &str) -> Option<Self> {
        let (src, r) = s.trim().split_once(':').unwrap_or((s.trim(), ""));
        (!src.trim().is_empty()).then(|| Self { source: src.trim().to_string(), source_ref: r.trim().to_string() })
    }

    pub fn display(&self) -> String {
        if self.source_ref.is_empty() {
            self.source.clone()
        } else {
            format!("{}:{}", self.source, self.source_ref)
        }
    }
}

pub fn all_item_sources(db: &Db) -> Result<HashMap<String, Vec<Source>>> {
    let mut m: HashMap<String, Vec<Source>> = HashMap::new();
    db.query("SELECT item_id, source, source_ref FROM sources ORDER BY id", &[])?.iter().for_each(|r| {
        m.entry(r.s(0).unwrap_or_default())
            .or_default()
            .push(Source { source: r.s(1).unwrap_or_default(), source_ref: r.s(2).unwrap_or_default() });
    });
    Ok(m)
}

pub fn item_sources(db: &Db, id: &str) -> Result<Vec<Source>> {
    Ok(all_item_sources(db)?.remove(id).unwrap_or_default())
}

pub fn add_source(db: &Db, id: &str, s: &Source) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO sources (item_id, source, source_ref, created_at) VALUES (?1, ?2, ?3, ?4)",
        &[text(id), text(&s.source), text(&s.source_ref), text(now_iso())],
    )
    .map(|_| ())
}

pub fn set_sources(db: &Db, id: &str, sources: &[Source]) -> Result<()> {
    db.execute("DELETE FROM sources WHERE item_id = ?1", &[text(id)])?;
    sources.iter().try_for_each(|s| add_source(db, id, s))
}

// ---------------------------------------------------------------------------
// Events (append-only history)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub id: i64,
    pub item_id: String,
    pub at: String,
    pub kind: String,
    pub detail: Json,
}

pub fn add_event(db: &Db, item_id: &str, kind: &str, detail: Json) -> Result<()> {
    db.execute(
        "INSERT INTO events (item_id, at, kind, detail) VALUES (?1, ?2, ?3, ?4)",
        &[text(item_id), text(now_iso()), text(kind), text(detail.to_string())],
    )
    .map(|_| ())
}

pub fn events_for(db: &Db, item_id: &str) -> Result<Vec<Event>> {
    Ok(db
        .query("SELECT id, item_id, at, kind, detail FROM events WHERE item_id = ?1 ORDER BY id", &[text(item_id)])?
        .iter()
        .map(|r| Event {
            id: r.i(0).unwrap_or_default(),
            item_id: r.s(1).unwrap_or_default(),
            at: r.s(2).unwrap_or_default(),
            kind: r.s(3).unwrap_or_default(),
            detail: r.s(4).and_then(|d| serde_json::from_str(&d).ok()).unwrap_or(Json::Null),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Lots and consumption
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Lot {
    pub id: String,
    pub item_id: String,
    pub qty: f64,
    pub acquired_at: Option<String>,
    pub opened_at: Option<String>,
    pub expires_at: Option<String>,
    pub best_by: Option<String>,
    pub frozen_at: Option<String>,
    pub location_id: Option<i64>,
    pub status: String,
    pub created_at: String,
    /// What one unit cost, in cents.
    pub unit_price: Option<i64>,
    /// The purchase this lot came from (an import ref), unique across lots.
    pub source_ref: Option<String>,
}

impl Lot {
    pub fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "sealed" | "open")
    }

    /// The date the lot should be used by: `expires_at`, else `best_by`.
    pub fn due(&self) -> Option<&str> {
        self.expires_at.as_deref().or(self.best_by.as_deref())
    }

    /// Consumption order: oldest first (acquired date, then the earliest
    /// due date, then creation order). Undated lots go last.
    pub fn fifo_key(&self) -> (String, String, String) {
        let last = || "9999-99-99".to_string();
        (self.acquired_at.clone().unwrap_or_else(last), self.due().map_or_else(last, str::to_string), self.id.clone())
    }
}

const LOT_COLS: &str =
    "id, item_id, qty, acquired_at, opened_at, expires_at, best_by, frozen_at, location_id, status, \
    created_at, unit_price, source_ref";

fn lot_from(r: &Row) -> Lot {
    Lot {
        id: r.s(0).unwrap_or_default(),
        item_id: r.s(1).unwrap_or_default(),
        qty: r.f(2).unwrap_or_default(),
        acquired_at: r.s(3),
        opened_at: r.s(4),
        expires_at: r.s(5),
        best_by: r.s(6),
        frozen_at: r.s(7),
        location_id: r.i(8),
        status: r.s(9).unwrap_or_else(|| "sealed".into()),
        created_at: r.s(10).unwrap_or_default(),
        unit_price: r.i(11),
        source_ref: r.s(12),
    }
}

fn lot_params(l: &Lot) -> Vec<Value> {
    vec![
        text(&l.id),
        text(&l.item_id),
        real(l.qty),
        opt_text(l.acquired_at.as_ref()),
        opt_text(l.opened_at.as_ref()),
        opt_text(l.expires_at.as_ref()),
        opt_text(l.best_by.as_ref()),
        opt_text(l.frozen_at.as_ref()),
        opt_int(l.location_id),
        text(&l.status),
        text(&l.created_at),
        opt_int(l.unit_price),
        opt_text(l.source_ref.as_ref()),
    ]
}

pub fn insert_lot(db: &Db, l: &Lot) -> Result<()> {
    check_choice("lot status", &l.status, LOT_STATUSES)?;
    db.execute(&format!("INSERT INTO lots ({LOT_COLS}) VALUES ({})", placeholders(13)), &lot_params(l))
        .map_err(|e| unique_err(e, || format!("lot '{}' or source ref already exists", l.id)))
        .map(|_| ())
}

pub fn update_lot(db: &Db, l: &Lot) -> Result<()> {
    check_choice("lot status", &l.status, LOT_STATUSES)?;
    let sets = LOT_COLS.split(", ").enumerate().skip(1).map(|(n, c)| format!("{c} = ?{}", n + 1)).collect::<Vec<_>>();
    db.execute(&format!("UPDATE lots SET {} WHERE id = ?1", sets.join(", ")), &lot_params(l)).map(|_| ())
}

pub fn get_lot(db: &Db, id: &str) -> Result<Option<Lot>> {
    Ok(db.query_opt(&format!("SELECT {LOT_COLS} FROM lots WHERE id = ?1"), &[text(id)])?.map(|r| lot_from(&r)))
}

pub fn lot_by_source(db: &Db, source_ref: &str) -> Result<Option<Lot>> {
    Ok(db
        .query_opt(&format!("SELECT {LOT_COLS} FROM lots WHERE source_ref = ?1"), &[text(source_ref)])?
        .map(|r| lot_from(&r)))
}

/// Resolve a lot by id or a unique id prefix or suffix (6+ characters).
pub fn resolve_lot(db: &Db, spec: &str) -> Result<Lot> {
    let up = spec.trim().to_ascii_uppercase();
    if let Some(l) = get_lot(db, &up)? {
        return Ok(l);
    }
    let hits: Vec<Lot> = if up.len() >= 6 {
        all_lots(db)?.into_iter().filter(|l| l.id.starts_with(&up) || l.id.ends_with(&up)).collect()
    } else {
        vec![]
    };
    match hits.len() {
        1 => Ok(hits.into_iter().next().expect("one")),
        0 => Err(AppError::not_found(format!("no lot '{spec}'"))),
        n => Err(AppError::invalid(format!("'{spec}' matches {n} lots; give more of the id"))),
    }
}

/// Every lot, in FIFO order per item.
pub fn all_lots(db: &Db) -> Result<Vec<Lot>> {
    let mut v: Vec<Lot> = db.query(&format!("SELECT {LOT_COLS} FROM lots"), &[])?.iter().map(lot_from).collect();
    v.sort_by(|a, b| a.item_id.cmp(&b.item_id).then_with(|| a.fifo_key().cmp(&b.fifo_key())));
    Ok(v)
}

/// An item's lots in FIFO order.
pub fn lots_for(db: &Db, item_id: &str) -> Result<Vec<Lot>> {
    let mut v: Vec<Lot> = db
        .query(&format!("SELECT {LOT_COLS} FROM lots WHERE item_id = ?1"), &[text(item_id)])?
        .iter()
        .map(lot_from)
        .collect();
    v.sort_by_key(Lot::fifo_key);
    Ok(v)
}

/// Keep `items.quantity` equal to the (rounded-up) quantity on hand in active
/// lots, for items that track lots. Returns the new quantity, if it changed.
pub fn sync_lot_quantity(db: &Db, item_id: &str) -> Result<Option<i64>> {
    let lots = lots_for(db, item_id)?;
    if lots.is_empty() {
        return Ok(None);
    }
    let on_hand: f64 = lots.iter().filter(|l| l.is_active()).map(|l| l.qty).sum();
    let q = on_hand.ceil() as i64;
    let cur = db.query_scalar_i64("SELECT quantity FROM items WHERE id = ?1", &[text(item_id)])?;
    if cur == q {
        return Ok(None);
    }
    db.execute(
        "UPDATE items SET quantity = ?1, updated_at = ?2 WHERE id = ?3",
        &[int(q), text(now_iso()), text(item_id)],
    )?;
    Ok(Some(q))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Consumption {
    pub id: i64,
    pub lot_id: String,
    pub qty: f64,
    pub at: String,
    pub reason: String,
    pub note: Option<String>,
}

pub fn insert_consumption(db: &Db, lot_id: &str, qty: f64, at: &str, reason: &str, note: Option<&str>) -> Result<()> {
    check_choice("reason", reason, CONSUMPTION_REASONS)?;
    db.execute(
        "INSERT INTO consumption (lot_id, qty, at, reason, note) VALUES (?1, ?2, ?3, ?4, ?5)",
        &[text(lot_id), real(qty), text(at), text(reason), opt_text(note)],
    )
    .map(|_| ())
}

pub fn consumption_for(db: &Db, item_id: &str) -> Result<Vec<Consumption>> {
    Ok(db
        .query(
            "SELECT c.id, c.lot_id, c.qty, c.at, c.reason, c.note FROM consumption c JOIN lots l ON l.id = c.lot_id \
             WHERE l.item_id = ?1 ORDER BY c.id",
            &[text(item_id)],
        )?
        .iter()
        .map(|r| Consumption {
            id: r.i(0).unwrap_or_default(),
            lot_id: r.s(1).unwrap_or_default(),
            qty: r.f(2).unwrap_or_default(),
            at: r.s(3).unwrap_or_default(),
            reason: r.s(4).unwrap_or_default(),
            note: r.s(5),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attachment {
    pub id: i64,
    pub item_id: String,
    pub kind: String,
    /// Relative to the attachments directory.
    pub path: String,
    pub filename: Option<String>,
    pub sha256: String,
    pub bytes: i64,
    pub encrypted: bool,
    pub primary: bool,
    pub created_at: String,
}

const ATT_COLS: &str = "id, item_id, kind, path, filename, sha256, bytes, encrypted, is_primary, created_at";

fn att_from(r: &Row) -> Attachment {
    Attachment {
        id: r.i(0).unwrap_or_default(),
        item_id: r.s(1).unwrap_or_default(),
        kind: r.s(2).unwrap_or_default(),
        path: r.s(3).unwrap_or_default(),
        filename: r.s(4),
        sha256: r.s(5).unwrap_or_default(),
        bytes: r.i(6).unwrap_or_default(),
        encrypted: r.b(7).unwrap_or(false),
        primary: r.b(8).unwrap_or(false),
        created_at: r.s(9).unwrap_or_default(),
    }
}

pub fn insert_attachment(db: &Db, a: &Attachment) -> Result<i64> {
    if a.primary {
        db.execute(
            "UPDATE attachments SET is_primary = 0 WHERE item_id = ?1 AND kind = ?2",
            &[text(&a.item_id), text(&a.kind)],
        )?;
    }
    db.execute(
        "INSERT INTO attachments (item_id, kind, path, filename, sha256, bytes, encrypted, is_primary, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        &[
            text(&a.item_id),
            text(&a.kind),
            text(&a.path),
            opt_text(a.filename.as_ref()),
            text(&a.sha256),
            int(a.bytes),
            int(i64::from(a.encrypted)),
            int(i64::from(a.primary)),
            text(&a.created_at),
        ],
    )
    .map_err(|e| unique_err(e, || "this file is already attached to the item".into()))?;
    Ok(db.last_insert_rowid())
}

pub fn attachments(db: &Db, item_id: Option<&str>) -> Result<Vec<Attachment>> {
    let rows = match item_id {
        Some(id) => {
            db.query(&format!("SELECT {ATT_COLS} FROM attachments WHERE item_id = ?1 ORDER BY id"), &[text(id)])?
        }
        None => db.query(&format!("SELECT {ATT_COLS} FROM attachments ORDER BY id"), &[])?,
    };
    Ok(rows.iter().map(att_from).collect())
}

pub fn get_attachment(db: &Db, id: i64) -> Result<Attachment> {
    db.query_opt(&format!("SELECT {ATT_COLS} FROM attachments WHERE id = ?1"), &[int(id)])?
        .map(|r| att_from(&r))
        .ok_or_else(|| AppError::not_found(format!("no attachment {id}")))
}

pub fn delete_attachment(db: &Db, id: i64) -> Result<()> {
    db.execute("DELETE FROM attachments WHERE id = ?1", &[int(id)]).map(|_| ())
}

// ---------------------------------------------------------------------------
// Import batches
// ---------------------------------------------------------------------------

pub fn record_batch(db: &Db, id: &str, source: &str, format: &str, rows: usize) -> Result<()> {
    db.execute(
        "INSERT INTO import_batches (id, source, format, created_at, row_count) VALUES (?1, ?2, ?3, ?4, ?5)",
        &[text(id), text(source), text(format), text(now_iso()), int(rows as i64)],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_ids() {
        assert_eq!(format_asset_id(1), "000-001");
        assert_eq!(format_asset_id(123_456), "123-456");
        assert_eq!(format_asset_id(1_234_567), "1234-567");
        assert_eq!(parse_asset_id("000-042"), Some(42));
        assert_eq!(parse_asset_id("abc"), None);
    }

    #[test]
    fn paths_split_and_sources_parse() {
        assert_eq!(split_path("Lake House / Garage /Shelf 2"), ["Lake House", "Garage", "Shelf 2"]);
        assert!(split_path(" / ").is_empty());
        let s = Source::parse("amazon-email:113-1:2").unwrap();
        assert_eq!((s.source.as_str(), s.source_ref.as_str()), ("amazon-email", "113-1:2"));
        assert_eq!(s.display(), "amazon-email:113-1:2");
        assert_eq!(Source::parse("manual").unwrap().display(), "manual");
    }

    #[test]
    fn fifo_order_is_oldest_first() {
        let lot = |id: &str, acq: Option<&str>, exp: Option<&str>| Lot {
            id: id.into(),
            acquired_at: acq.map(str::to_string),
            expires_at: exp.map(str::to_string),
            ..Default::default()
        };
        let mut v = [
            lot("C", None, Some("2024-01-01")),
            lot("B", Some("2024-02-01"), None),
            lot("A", Some("2024-02-01"), Some("2024-03-01")),
            lot("D", Some("2024-01-15"), None),
        ];
        v.sort_by_key(Lot::fifo_key);
        assert_eq!(v.iter().map(|l| l.id.as_str()).collect::<String>(), "DABC");
    }
}
