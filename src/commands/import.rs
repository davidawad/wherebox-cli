//! `wherebox import`: Homebox CSV, wherebox's own CSV / JSON / JSONL, generic
//! CSV/TSV with a column mapping, and spreadsheets (xlsx, xls, ods).
//!
//! Rows are first turned into [`Incoming`] items (format specific), then one
//! shared upsert applies them: an item is matched by id, then by import ref
//! (`HB.import_ref`), and updated in place; otherwise it is created. The
//! whole import runs in one transaction; `--dry-run` rolls it back, so the
//! reported plan is exactly what a real run would do.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};

use crate::cli::{ImportArgs, InputFormat};
use crate::commands::homebox;
use crate::commands::items::record_changes;
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::fields::{self, Parse};
use crate::output::{to_record, Format, Record, Report};
use crate::presets::Catalog;
use crate::sheet;
use crate::store::{self, Item, Lot, Source, Tree};
use crate::util::{new_batch_id, parse_qty, parse_tags};

/// One input row as ordered (column, text) pairs.
pub type RawRow = Vec<(String, String)>;

/// A lot read from the input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IncomingLot {
    pub id: Option<String>,
    pub qty: f64,
    pub acquired_at: Option<String>,
    pub opened_at: Option<String>,
    pub expires_at: Option<String>,
    pub best_by: Option<String>,
    pub frozen_at: Option<String>,
    pub thawed_at: Option<String>,
    pub status: Option<String>,
    /// Location path.
    pub location: Option<String>,
    /// Price of one unit (as written in the input).
    pub unit_price: Option<String>,
    /// The purchase row this lot came from; re-imports match on it.
    pub source_ref: Option<String>,
}

/// An item as read from one input row. `Some(None)` means "the column is
/// present but empty" (clear the value); `None` means "not in the input"
/// (keep what the database has).
#[derive(Debug, Clone, Default)]
pub struct Incoming {
    pub line: usize,
    /// Native item id (wherebox exports).
    pub id: Option<String>,
    /// Import ref (`HB.import_ref`, without any `#lot:` suffix).
    pub key: Option<String>,
    /// Scalar fields by registry key ([`crate::fields`]).
    pub values: Vec<(&'static str, Option<String>)>,
    pub location: Option<Option<String>>,
    pub labels: Option<Vec<String>>,
    /// Custom fields to set (`None` value removes).
    pub custom: Vec<(String, Option<String>)>,
    /// The complete set of custom fields (replaces all existing ones).
    pub all_custom: Option<BTreeMap<String, String>>,
    pub sources: Option<Vec<Source>>,
    /// Parent item (id or import ref); `Some(None)` = not inside anything.
    pub parent: Option<Option<String>>,
    pub lots: Vec<IncomingLot>,
    /// Columns that were not understood.
    pub ignored: Vec<String>,
    /// Problems worth telling the user about that did not stop the row.
    pub warnings: Vec<String>,
    /// Set on the item built from consumable purchase rows: the property
    /// (top-level location) and product key they share.
    pub purchase: Option<(Option<String>, String)>,
}

impl Incoming {
    pub fn value(&self, key: &str) -> Option<&str> {
        self.values.iter().rev().find(|(k, _)| *k == key).and_then(|(_, v)| v.as_deref())
    }

    fn name(&self) -> Option<&str> {
        self.value("name")
    }
}

// ---------------------------------------------------------------------------
// Mapping (generic CSV, spreadsheets, wherebox CSV/JSON)
// ---------------------------------------------------------------------------

/// Non-scalar fields a column can map to.
const SPECIAL: &[&str] = &["id", "import_ref", "location", "labels", "fields", "sources", "parent_item_id", "lots"];

/// Header synonyms for generic files. An exact field-name column always wins
/// over a synonym.
fn synonyms(field: &str) -> &'static [&'static str] {
    match field {
        "name" => &["item", "item_name", "title", "product"],
        "quantity" => &["qty", "count"],
        "location" => &["where", "room", "place"],
        "labels" => &["label", "tags", "tag", "category"],
        "purchase_price" => &["price", "cost", "paid"],
        "purchase_time" => &["purchase_date", "bought", "date_purchased", "purchased"],
        "purchase_from" => &["store", "seller", "retailer", "bought_from"],
        "serial_number" => &["serial", "sn", "serial_no"],
        "model_number" => &["model", "model_no"],
        "manufacturer" => &["maker", "brand", "make"],
        "description" => &["desc"],
        "notes" => &["note", "comments", "comment"],
        "owner_entity" => &["owner"],
        "replacement_value" => &["value", "replacement"],
        "asset_id" => &["asset", "asset_tag"],
        "warranty_expires" => &["warranty", "warranty_until", "warranty_end"],
        _ => &[],
    }
}

fn norm(h: &str) -> String {
    h.trim().to_lowercase().replace([' ', '-', '.'], "_")
}

/// Parsed mapping file.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingFile {
    /// Field -> column header. Fields: item field names, `location`,
    /// `labels`, `import_ref`, `id`, `sources`, or `field.<Name>` for a
    /// custom field.
    #[serde(default)]
    pub columns: BTreeMap<String, String>,
    /// Field -> value used when the row has none.
    #[serde(default)]
    pub defaults: BTreeMap<String, toml::Value>,
    pub date_format: Option<String>,
    pub delimiter: Option<String>,
    pub sheet: Option<String>,
    pub header_row: Option<usize>,
}

#[derive(Debug, Default, Clone)]
pub struct Mapping {
    pub columns: BTreeMap<String, String>,
    pub defaults: BTreeMap<String, String>,
    pub date_format: Option<String>,
    pub delimiter: Option<String>,
    pub sheet: Option<String>,
    pub header_row: Option<usize>,
}

fn valid_target(f: &str) -> bool {
    fields::by_key(f).is_some()
        || SPECIAL.contains(&f)
        || f.strip_prefix("field.").is_some_and(|n| !n.trim().is_empty())
}

pub fn load_mapping(a: &ImportArgs) -> Result<Mapping> {
    let file = a
        .mapping
        .as_deref()
        .map(|p| {
            std::fs::read_to_string(p)
                .map_err(|e| AppError::io(format!("reading mapping {}: {e}", p.display())))
                .and_then(|t| {
                    toml::from_str::<MappingFile>(&t)
                        .map_err(|e| AppError::invalid(format!("mapping {}: {e}", p.display())))
                })
        })
        .transpose()?
        .unwrap_or_default();
    let canon = |k: &str| {
        let k = k.trim();
        match k.strip_prefix("field.") {
            Some(n) => format!("field.{}", n.trim()),
            None => fields::by_key(k).map_or_else(|| norm(k), |f| f.key.to_string()),
        }
    };
    let flag_maps = a
        .maps
        .iter()
        .map(|m| {
            m.split_once('=')
                .map(|(f, c)| (canon(f), c.trim().to_string()))
                .ok_or_else(|| AppError::usage(format!("bad --map '{m}' (expected FIELD=COLUMN)")))
        })
        .collect::<Result<Vec<_>>>()?;
    let columns: BTreeMap<String, String> =
        file.columns.iter().map(|(k, v)| (canon(k), v.clone())).chain(flag_maps).collect();
    let defaults: BTreeMap<String, String> = file
        .defaults
        .iter()
        .map(|(k, v)| {
            (
                canon(k),
                match v {
                    toml::Value::String(s) => s.clone(),
                    other => other.to_string(),
                },
            )
        })
        .collect();
    if let Some(bad) = columns.keys().chain(defaults.keys()).find(|k| !valid_target(k)) {
        return Err(AppError::usage(format!(
            "unknown import field '{bad}' (item fields such as name, quantity, purchase_price; or {}; or field.<Name>)",
            SPECIAL.join(", ")
        )));
    }
    Ok(Mapping {
        columns,
        defaults,
        date_format: a.input_date_format.clone().or(file.date_format),
        delimiter: file.delimiter,
        sheet: a.sheet.clone().or(file.sheet),
        header_row: a.header_row.or(file.header_row),
    })
}

/// Column -> target field for a header row.
fn resolve_columns(headers: &[String], m: &Mapping) -> Result<Vec<(String, String)>> {
    if let Some((f, c)) =
        m.columns.iter().find(|(_, c)| !headers.iter().any(|h| h.trim().eq_ignore_ascii_case(c.trim())))
    {
        return Err(AppError::usage(format!(
            "mapped column '{c}' for '{f}' not found (columns: {})",
            headers.join(", ")
        )));
    }
    let mut out: Vec<(String, String)> = Vec::new();
    let mut taken: Vec<String> = m.columns.keys().cloned().collect();
    for (f, c) in &m.columns {
        if let Some(h) = headers.iter().find(|h| h.trim().eq_ignore_ascii_case(c.trim())) {
            out.push((h.clone(), f.clone()));
        }
    }
    let unmapped = |h: &String, out: &[(String, String)]| !out.iter().any(|(c, _)| c == h);
    // Exact field names first ...
    for h in headers {
        let n = norm(h);
        let target =
            fields::by_key(&n).map(|f| f.key.to_string()).or_else(|| SPECIAL.contains(&n.as_str()).then(|| n.clone()));
        if let Some(t) = target.filter(|t| !taken.contains(t)) {
            if unmapped(h, &out) {
                taken.push(t.clone());
                out.push((h.clone(), t));
            }
        }
    }
    // ... then synonyms for fields still unmapped.
    for h in headers {
        if !unmapped(h, &out) {
            continue;
        }
        let n = norm(h);
        let target = fields::FIELDS
            .iter()
            .map(|f| f.key)
            .chain(["location", "labels"])
            .find(|f| !taken.iter().any(|t| t == f) && synonyms(f).contains(&n.as_str()));
        if let Some(t) = target {
            taken.push(t.to_string());
            out.push((h.clone(), t.to_string()));
        }
    }
    Ok(out)
}

/// A generic / wherebox-native row into an [`Incoming`].
fn parse_native(line: usize, row: &RawRow, cols: &[(String, String)], m: &Mapping) -> Result<Incoming> {
    let mut inc = Incoming { line, ..Default::default() };
    let target = |h: &str| cols.iter().find(|(c, _)| c == h).map(|(_, t)| t.as_str());
    let mut seen: Vec<String> = Vec::new();
    let apply = |inc: &mut Incoming, t: &str, v: &str| -> Result<()> {
        let present = Some(v.trim().to_string()).filter(|v| !v.is_empty());
        match t {
            "id" => inc.id = present.map(|s| s.to_ascii_uppercase()),
            "import_ref" => inc.key = present,
            "location" => inc.location = Some(present),
            "labels" => inc.labels = Some(parse_tags(v)),
            "sources" => {
                let list: Vec<String> = serde_json::from_str(v.trim())
                    .unwrap_or_else(|_| v.split([';', ',']).map(str::to_string).collect());
                inc.sources = Some(list.iter().filter_map(|s| Source::parse(s)).collect());
            }
            "parent_item_id" => inc.parent = Some(present),
            "fields" => inc.all_custom = Some(parse_custom(v)?),
            "lots" => inc.lots = parse_lots(v)?,
            t if t.starts_with("field.") => inc.custom.push((t["field.".len()..].to_string(), present)),
            t => {
                let f = fields::by_key(t).ok_or_else(|| AppError::usage(format!("unknown field '{t}'")))?;
                inc.values.push((f.key, present));
            }
        }
        Ok(())
    };
    for (h, v) in row {
        match target(h) {
            Some(t) => {
                apply(&mut inc, t, v)?;
                seen.push(t.to_string());
            }
            None => inc.ignored.push(h.clone()),
        }
    }
    for (f, d) in &m.defaults {
        let missing = match f.as_str() {
            "location" => inc.location.as_ref().is_none_or(Option::is_none),
            "labels" => inc.labels.as_ref().is_none_or(Vec::is_empty),
            other => !seen.iter().any(|s| s == other) || inc.value(other).is_none(),
        };
        if missing {
            apply(&mut inc, f, d)?;
        }
    }
    if inc.name().is_none() && inc.key.is_none() && inc.id.is_none() {
        return Err(AppError::invalid("row has no name"));
    }
    Ok(inc)
}

/// `{"Color": "red"}` or `Color=red; Size=L`.
fn parse_custom(v: &str) -> Result<BTreeMap<String, String>> {
    let t = v.trim();
    if t.is_empty() {
        return Ok(BTreeMap::new());
    }
    if let Ok(m) = serde_json::from_str::<BTreeMap<String, Value>>(t) {
        return Ok(m.into_iter().map(|(k, v)| (k, json_text(&v))).filter(|(_, v)| !v.is_empty()).collect());
    }
    t.split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .ok_or_else(|| AppError::invalid(format!("bad custom field '{p}' (expected NAME=VALUE)")))
        })
        .collect()
}

fn parse_lots(v: &str) -> Result<Vec<IncomingLot>> {
    let t = v.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    let arr: Vec<Value> = serde_json::from_str(t).map_err(|e| AppError::invalid(format!("lots: {e}")))?;
    arr.iter()
        .map(|l| {
            let s = |k: &str| l.get(k).map(json_text).filter(|v| !v.is_empty());
            Ok(IncomingLot {
                id: s("id"),
                qty: s("qty").map(|q| parse_qty(&q)).transpose()?.unwrap_or(1.0),
                acquired_at: s("acquired_at"),
                opened_at: s("opened_at"),
                expires_at: s("expires_at"),
                best_by: s("best_by"),
                frozen_at: s("frozen_at"),
                thawed_at: s("thawed_at"),
                status: s("status"),
                location: s("location"),
                unit_price: s("unit_price"),
                source_ref: s("source_ref"),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

pub fn detect_format(path: &Path, explicit: Option<InputFormat>, text: &str) -> InputFormat {
    explicit.unwrap_or_else(|| {
        let first_line = text.trim_start_matches('\u{feff}').lines().next().unwrap_or("");
        match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("json") => InputFormat::Json,
            Some("jsonl" | "ndjson") => InputFormat::Jsonl,
            Some("tsv" | "tab") if first_line.contains("HB.") => InputFormat::Homebox,
            Some("tsv" | "tab") => InputFormat::Tsv,
            Some(e) if sheet::EXTENSIONS.contains(&e) => InputFormat::Spreadsheet,
            _ => match text.trim_start().chars().next() {
                Some('[') => InputFormat::Json,
                Some('{') if text.trim_start().starts_with("{\"schema\"") || !text.trim().contains('\n') => {
                    InputFormat::Json
                }
                Some('{') => InputFormat::Jsonl,
                _ if first_line.contains("HB.") => InputFormat::Homebox,
                _ => InputFormat::Csv,
            },
        }
    })
}

fn json_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.iter().all(Value::is_string) => a.iter().map(json_text).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

fn object_row(v: &Value) -> Result<RawRow> {
    v.as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), json_text(v))).collect())
        .ok_or_else(|| AppError::invalid("expected a JSON object per record"))
}

pub fn parse_rows(text: &str, fmt: InputFormat, delimiter: u8) -> Result<Vec<(usize, RawRow)>> {
    match fmt {
        InputFormat::Csv | InputFormat::Tsv | InputFormat::Homebox => {
            let body = text.trim_start_matches('\u{feff}');
            let first = body.lines().next().unwrap_or("");
            // Homebox accepts TSV as well as CSV.
            let delim = match fmt {
                InputFormat::Tsv => b'\t',
                InputFormat::Homebox if first.contains('\t') && !first.contains(',') => b'\t',
                _ => delimiter,
            };
            let mut rdr = csv::ReaderBuilder::new().delimiter(delim).flexible(true).from_reader(body.as_bytes());
            let headers: Vec<String> = rdr.headers()?.iter().map(|h| h.trim().to_string()).collect();
            rdr.records()
                .map(|r| {
                    let r = r?;
                    let line = r.position().map_or(0, |p| p.line() as usize);
                    Ok((line, headers.iter().cloned().zip(r.iter().map(str::to_string)).collect()))
                })
                .filter(|r: &Result<(usize, RawRow)>| {
                    r.as_ref().map_or(true, |(_, row)| row.iter().any(|(_, v)| !v.trim().is_empty()))
                })
                .collect()
        }
        InputFormat::Json => {
            let v: Value = serde_json::from_str(text)?;
            let items = match &v {
                Value::Array(a) => a.clone(),
                Value::Object(o) => match o.get("data") {
                    Some(Value::Array(a)) => a.clone(),
                    _ => vec![v.clone()],
                },
                _ => {
                    return Err(AppError::invalid("JSON input must be an array, an object, or a wherebox/v1 envelope"))
                }
            };
            items.iter().enumerate().map(|(i, v)| object_row(v).map(|r| (i + 1, r))).collect()
        }
        InputFormat::Jsonl => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                serde_json::from_str::<Value>(l)
                    .map_err(|e| AppError::invalid(format!("line {}: {e}", i + 1)))
                    .and_then(|v| object_row(&v))
                    .map(|r| (i + 1, r))
            })
            .collect(),
        InputFormat::Spreadsheet => Err(AppError::invalid("spreadsheets are read with the sheet reader, not as text")),
    }
}

/// A spreadsheet's rows below its header row.
fn sheet_rows(path: &Path, m: &Mapping) -> Result<Vec<(usize, RawRow)>> {
    let grid = sheet::read(path, m.sheet.as_deref())?;
    let header_idx = match m.header_row {
        Some(n) if n >= 1 => n - 1,
        Some(_) => return Err(AppError::usage("--header-row is 1-based")),
        None => grid
            .rows
            .iter()
            .position(|r| r.iter().any(|c| !c.is_empty()))
            .ok_or_else(|| AppError::invalid(format!("sheet '{}' is empty", grid.name)))?,
    };
    let width = grid.width();
    let headers: Vec<String> = (0..width).map(|c| grid.cell(header_idx, c).text()).collect();
    Ok((header_idx + 1..grid.rows.len())
        .filter(|r| grid.rows[*r].iter().any(|c| !c.is_empty()))
        .map(|r| {
            let row = headers
                .iter()
                .enumerate()
                .filter(|(_, h)| !h.is_empty())
                .map(|(c, h)| (h.clone(), grid.cell(r, c).text()))
                .collect();
            (r + 1, row)
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Totals {
    created: usize,
    updated: usize,
    unchanged: usize,
    locations: usize,
    labels: usize,
    lots: usize,
}

struct Applier<'a> {
    db: &'a Db,
    catalog: &'a Catalog,
    types: &'a crate::shelf_life::Types,
    /// Printed dates dropped from lots of non-perishables.
    warnings: Vec<String>,
    tree: Tree,
    p: Parse<'a>,
    batch: String,
    default_at: Option<String>,
    extra_labels: Vec<String>,
    totals: Totals,
    /// (item id, parent spec) resolved after every row is in.
    parents: Vec<(String, Option<String>, usize)>,
}

/// Everything about an item that an import can change, for "unchanged" detection.
type Snapshot = (Item, Vec<String>, BTreeMap<String, String>, Vec<Source>, Vec<Lot>);

impl Applier<'_> {
    fn snapshot(&self, item: &Item) -> Result<Snapshot> {
        let strip = |mut l: Lot| {
            l.created_at.clear();
            l
        };
        Ok((
            Item { created_at: String::new(), updated_at: String::new(), ..item.clone() },
            store::item_labels(self.db, &item.id)?,
            store::item_fields(self.db, &item.id)?,
            store::item_sources(self.db, &item.id)?,
            store::lots_for(self.db, &item.id)?.into_iter().map(strip).collect(),
        ))
    }

    fn location(&mut self, path: &str) -> Result<i64> {
        let (id, created) = self.tree.ensure_path(self.db, path, None)?;
        self.totals.locations += created;
        Ok(id)
    }

    fn find_existing(&self, group: &[Incoming]) -> Result<Option<Item>> {
        let first = &group[0];
        if let Some(i) = first.id.as_deref().map(|id| store::get_item(self.db, id)).transpose()?.flatten() {
            return Ok(Some(i));
        }
        if let Some((property, product)) = &first.purchase {
            return self.find_consumable(first, property.as_deref(), product);
        }
        first.key.as_deref().map(|k| store::item_by_ref(self.db, k)).transpose().map(Option::flatten)
    }

    /// The item purchases of `product` at `property` belong to: the item
    /// holding a lot from one of these rows, else the item one of them
    /// created, else a consumable of that product at that property.
    fn find_consumable(&self, inc: &Incoming, property: Option<&str>, product: &str) -> Result<Option<Item>> {
        for r in inc.lots.iter().filter_map(|l| l.source_ref.as_deref()) {
            if let Some(l) = store::lot_by_source(self.db, r)? {
                return store::get_item(self.db, &l.item_id);
            }
        }
        for r in inc.lots.iter().filter_map(|l| l.source_ref.as_deref()).chain(inc.key.as_deref()) {
            if let Some(i) = store::item_by_ref(self.db, r)? {
                return Ok(Some(i));
            }
        }
        let ids: Vec<String> = self
            .db
            .query(
                "SELECT f.item_id FROM item_fields f JOIN items i ON i.id = f.item_id \
                 WHERE upper(trim(f.value)) = ?1 AND lower(f.name) = lower(?2) \
                 UNION SELECT id FROM items WHERE upper(trim(product_ref)) = ?1 ORDER BY 1",
                &[crate::db::text(product), crate::db::text(crate::consumable::ASIN_FIELD)],
            )?
            .iter()
            .filter_map(|r| crate::db::RowExt::s(&r[..], 0))
            .collect();
        for id in ids {
            let Some(i) = store::get_item(self.db, &id)? else { continue };
            let fields = store::item_fields(self.db, &i.id)?;
            let key = crate::consumable::product_key(
                crate::consumable::field(Some(&fields), crate::consumable::ASIN_FIELD),
                i.product_ref.as_deref(),
            );
            let root = i.location_id.and_then(|l| self.tree.root_of(l)).map(|l| l.name.to_lowercase());
            if i.is_gone()
                || key.as_deref() != Some(product)
                || root.as_deref().unwrap_or("") != property.unwrap_or("").to_lowercase()
            {
                continue;
            }
            let labels = store::item_labels(self.db, &i.id)?;
            let kind = crate::consumable::field(Some(&fields), crate::consumable::KIND_FIELD);
            if crate::consumable::is_consumable(kind, i.kind_preset.as_deref(), &labels, self.catalog) {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// Upsert one item (all rows sharing its import ref). Returns the plan row.
    fn apply(&mut self, group: &[Incoming]) -> Result<Record> {
        let first = &group[0];
        let existing = self.find_existing(group)?;
        let before = existing.as_ref().map(|e| self.snapshot(e)).transpose()?;
        let mut item = match &existing {
            Some(e) => e.clone(),
            None => {
                let name = first.name().ok_or_else(|| AppError::invalid("new item without a name"))?;
                let mut i = Item::new(name)?;
                let wanted = first
                    .id
                    .clone()
                    .or_else(|| first.key.clone().filter(|k| crate::ulid::is_ulid(k)).map(|k| k.to_ascii_uppercase()));
                if let Some(id) = wanted.filter(|id| crate::ulid::is_ulid(id)) {
                    if store::get_item(self.db, &id)?.is_none() {
                        i.id = id.clone();
                        i.import_ref = id;
                    }
                }
                if let Some(k) = &first.key {
                    i.import_ref = k.clone();
                }
                i
            }
        };
        // Purchases older than the item's latest only add lots: the item's
        // fields stay those of its latest purchase.
        let stale = match (&first.purchase, &existing, first.value("purchase_time")) {
            (Some(_), Some(e), Some(t)) => {
                let fmts: Vec<&str> = self.p.date_formats.iter().map(String::as_str).collect();
                let t = crate::util::parse_day(t, &fmts, self.p.tz)?;
                e.purchase_time.as_deref().is_some_and(|have| t.as_str() < have)
            }
            _ => false,
        };
        let merge = first.purchase.is_some() && existing.is_some();
        for (k, v) in first.values.iter().filter(|_| !stale) {
            fields::set(&mut item, k, v.as_deref(), &self.p)?;
        }
        // A consumable keeps the import ref it was created with; its purchases
        // are matched through their lots.
        if let (Some(k), Some(_), None) = (&first.key, &existing, &first.purchase) {
            item.import_ref = k.clone();
        }
        // Rows without a location get --at; a present-but-empty cell clears it.
        let loc: Option<Option<i64>> = match (&first.location, &self.default_at) {
            _ if stale => None,
            (Some(Some(p)), _) => Some(Some(self.location(p)?)),
            (l, Some(at)) if existing.is_none() || l.is_some() => {
                let (id, created) = self.tree.resolve_or_create(self.db, &at.clone())?;
                self.totals.locations += created;
                Some(Some(id))
            }
            (l, _) => l.as_ref().map(|_| None),
        };
        if let Some(l) = loc {
            item.location_id = l;
        }
        if item.asset_id.is_none() {
            item.asset_id = Some(store::next_asset_id(self.db)?);
        }
        if let Some(p) = &first.parent {
            self.parents.push((item.id.clone(), p.clone(), first.line));
        }
        let created = existing.is_none();
        if created {
            store::insert_item(self.db, &item)?;
        } else if !item.same_content(existing.as_ref().expect("existing")) {
            item.updated_at = crate::util::now_iso();
            store::update_item(self.db, &item)?;
        }
        if let Some(mut labels) = first.labels.clone() {
            if merge {
                labels = crate::util::merge_tags(&store::item_labels(self.db, &item.id)?, &labels, &[]);
            }
            labels.extend(self.extra_labels.iter().cloned());
            self.totals.labels +=
                store::set_item_labels(self.db, &item.id, &crate::util::merge_tags(&[], &labels, &[]))?;
        } else if !self.extra_labels.is_empty() {
            let have = store::item_labels(self.db, &item.id)?;
            self.totals.labels +=
                store::set_item_labels(self.db, &item.id, &crate::util::merge_tags(&have, &self.extra_labels, &[]))?;
        }
        if let Some(all) = first.all_custom.as_ref().filter(|_| !stale) {
            for name in store::item_fields(self.db, &item.id)?.keys().filter(|n| !all.contains_key(*n)) {
                store::set_item_field(self.db, &item.id, name, None)?;
            }
            for (n, v) in all {
                store::set_item_field(self.db, &item.id, n, Some(v))?;
            }
        }
        for (n, v) in first.custom.iter().filter(|_| !stale) {
            store::set_item_field(self.db, &item.id, n, v.as_deref())?;
        }
        if let Some(s) = &first.sources {
            let mut s = s.clone();
            if merge {
                let have = store::item_sources(self.db, &item.id)?;
                s = have.iter().chain(s.iter().filter(|x| !have.contains(x))).cloned().collect();
            }
            store::set_sources(self.db, &item.id, &s)?;
        }
        let lots: Vec<&IncomingLot> = group.iter().flat_map(|g| g.lots.iter()).collect();
        for l in &lots {
            self.apply_lot(&item, l)?;
        }
        store::sync_lot_quantity(self.db, &item.id)?;
        let item = store::get_item(self.db, &item.id)?.ok_or_else(|| AppError::db("item vanished during import"))?;
        let action = match &before {
            None => {
                store::add_event(
                    self.db,
                    &item.id,
                    "created",
                    json!({"location": self.tree.opt_path(item.location_id), "via": "import", "batch": self.batch}),
                )?;
                self.totals.created += 1;
                "created"
            }
            Some(b) if *b == self.snapshot(&item)? => {
                self.totals.unchanged += 1;
                "unchanged"
            }
            Some(b) => {
                record_changes(self.db, &self.tree, &b.0, &item, "import")?;
                if b.1 != store::item_labels(self.db, &item.id)? || b.2 != store::item_fields(self.db, &item.id)? {
                    store::add_event(self.db, &item.id, "edited", json!({"labels_or_fields": true, "via": "import"}))?;
                }
                self.totals.updated += 1;
                "updated"
            }
        };
        self.totals.lots += lots.len();
        Ok(to_record(&json!({
            "line": first.line,
            "action": action,
            "id": item.id,
            "asset_id": item.asset_id,
            "name": item.name,
            "location": self.tree.opt_path(item.location_id),
            "lots": lots.len(),
            "error": null,
        })))
    }

    fn apply_lot(&mut self, item: &Item, l: &IncomingLot) -> Result<()> {
        let day = |v: &Option<String>| -> Result<Option<String>> {
            let fmts: Vec<&str> = self.p.date_formats.iter().map(String::as_str).collect();
            v.as_deref().map(|d| crate::util::parse_day(d, &fmts, self.p.tz)).transpose()
        };
        let existing = match l.id.as_deref() {
            Some(id) => store::get_lot(self.db, &id.to_ascii_uppercase())?,
            None => l.source_ref.as_deref().map(|r| store::lot_by_source(self.db, r)).transpose()?.flatten(),
        };
        if let Some(e) = &existing {
            if e.item_id != item.id {
                return Err(AppError::invalid(format!("lot {} belongs to another item", e.id)));
            }
        }
        let location_id = match &l.location {
            Some(p) => Some(self.location(p)?).filter(|x| Some(*x) != item.location_id),
            None => None,
        };
        let opened_at = day(&l.opened_at)?;
        let status = match &l.status {
            Some(s) => store::check_choice("lot status", s, store::LOT_STATUSES)?,
            None if opened_at.is_some() => "open".into(),
            None => "sealed".into(),
        };
        let mut lot = Lot {
            id: match &existing {
                Some(e) => e.id.clone(),
                None => {
                    l.id.as_deref()
                        .map(str::to_ascii_uppercase)
                        .filter(|i| crate::ulid::is_ulid(i))
                        .map_or_else(crate::ulid::new, Ok)?
                }
            },
            item_id: item.id.clone(),
            qty: l.qty,
            acquired_at: day(&l.acquired_at)?,
            opened_at,
            best_by: self.printed_date(item, day(&l.best_by)?.or(day(&l.expires_at)?))?,
            frozen_at: day(&l.frozen_at)?,
            thawed_at: day(&l.thawed_at)?,
            confirmed_at: None,
            location_id,
            status,
            created_at: existing.as_ref().map_or_else(crate::util::now_iso, |e| e.created_at.clone()),
            unit_price: l.unit_price.as_deref().map(crate::util::parse_money).transpose()?,
            source_ref: l.source_ref.clone(),
        };
        // A purchase row only says what was bought when: what has been used
        // since (quantity, status, dates) is the database's.
        if let (Some(e), None, Some(_)) = (&existing, &l.id, &l.source_ref) {
            lot = Lot { acquired_at: lot.acquired_at, unit_price: lot.unit_price, ..e.clone() };
        }
        match existing {
            Some(_) => store::update_lot(self.db, &lot),
            None => store::insert_lot(self.db, &lot),
        }
    }

    /// A printed best-by (or legacy expiry) date is kept for perishables
    /// and dropped, with a warning, for anything else.
    fn printed_date(&mut self, item: &Item, date: Option<String>) -> Result<Option<String>> {
        let Some(d) = date else { return Ok(None) };
        let fields = store::item_fields(self.db, &item.id)?;
        let labels = store::item_labels(self.db, &item.id)?;
        if crate::shelf_life::classify(item, Some(&fields), &labels, true, self.catalog, self.types).is_some() {
            return Ok(Some(d));
        }
        self.warnings
            .push(format!("{}: best-by/expiry date {d} dropped: not perishable (no shelf-life type)", item.name));
        Ok(None)
    }

    fn link_parents(&mut self) -> Result<Vec<String>> {
        let mut warnings = Vec::new();
        for (id, parent, line) in std::mem::take(&mut self.parents) {
            let Some(mut item) = store::get_item(self.db, &id)? else { continue };
            let pid = match parent.as_deref() {
                None => None,
                Some(spec) => {
                    match store::get_item(self.db, &spec.to_ascii_uppercase())?.or(store::item_by_ref(self.db, spec)?) {
                        Some(p) if p.id != item.id => Some(p.id),
                        _ => {
                            warnings.push(format!("line {line}: parent item '{spec}' not found; left unset"));
                            None
                        }
                    }
                }
            };
            if item.parent_item_id != pid {
                item.parent_item_id = pid;
                store::update_item(self.db, &item)?;
            }
        }
        Ok(warnings)
    }
}

/// The group key of a consumable purchase row: one without an item id or
/// lots of its own, for a consumable (see [`crate::consumable`]) with an
/// ASIN or product ref.
fn purchase_group(inc: &Incoming, extra_labels: &[String], at: Option<&str>, catalog: &Catalog) -> Option<String> {
    if inc.id.is_some() || !inc.lots.is_empty() {
        return None;
    }
    let custom = |name: &str| {
        inc.custom
            .iter()
            .rev()
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .and_then(|(_, v)| v.as_deref())
            .or_else(|| crate::consumable::field(inc.all_custom.as_ref(), name))
    };
    let product = crate::consumable::product_key(custom(crate::consumable::ASIN_FIELD), inc.value("product_ref"))?;
    let labels: Vec<String> = inc.labels.iter().flatten().chain(extra_labels).cloned().collect();
    if !crate::consumable::is_consumable(
        custom(crate::consumable::KIND_FIELD),
        inc.value("kind_preset"),
        &labels,
        catalog,
    ) {
        return None;
    }
    Some(crate::consumable::group_key(property_of(inc, at).as_deref(), &product))
}

fn property_of(inc: &Incoming, at: Option<&str>) -> Option<String> {
    match &inc.location {
        Some(Some(path)) => store::split_path(path).into_iter().next(),
        _ => at.map(str::to_string),
    }
}

/// One item from the purchase rows of a consumable: the latest row's fields,
/// the earliest row's import ref, every row's labels and sources, and a lot
/// per row (quantity, purchase date, unit price, the row's import ref).
fn purchases_item(mut rows: Vec<Incoming>, p: &Parse, at: Option<&str>) -> Incoming {
    let fmts: Vec<&str> = p.date_formats.iter().map(String::as_str).collect();
    let day =
        |i: &Incoming| i.value("purchase_time").map(|d| crate::util::parse_day(d, &fmts, p.tz).unwrap_or_default());
    rows.sort_by_cached_key(|i| (day(i).is_none(), day(i), i.line));
    let mut lots: Vec<IncomingLot> = Vec::new();
    for r in &rows {
        lots.retain(|l| l.source_ref.is_none() || l.source_ref != r.key);
        lots.push(IncomingLot {
            qty: r.value("quantity").and_then(|q| parse_qty(q).ok()).unwrap_or(1.0),
            acquired_at: r.value("purchase_time").map(str::to_string),
            unit_price: r.value("purchase_price").map(str::to_string),
            source_ref: r.key.clone(),
            location: r.location.clone().flatten(),
            ..Default::default()
        });
    }
    let first = &rows[0];
    let last = rows.last().expect("a purchase group has rows");
    let mut labels: Option<Vec<String>> = None;
    let mut sources: Option<Vec<Source>> = None;
    for r in &rows {
        if let Some(l) = &r.labels {
            let have = labels.get_or_insert_with(Vec::new);
            *have = crate::util::merge_tags(have, l, &[]);
        }
        if let Some(s) = &r.sources {
            let have = sources.get_or_insert_with(Vec::new);
            have.extend(s.iter().filter(|x| !have.contains(x)).cloned().collect::<Vec<_>>());
        }
    }
    let property = property_of(last, at);
    let product = crate::consumable::product_key(
        last.custom
            .iter()
            .rev()
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(crate::consumable::ASIN_FIELD))
            .and_then(|(_, v)| v.as_deref()),
        last.value("product_ref"),
    )
    .unwrap_or_default();
    Incoming {
        line: first.line,
        id: None,
        key: first.key.clone(),
        values: last.values.iter().filter(|(k, _)| *k != "quantity").cloned().collect(),
        location: last.location.clone(),
        labels,
        custom: rows.iter().flat_map(|r| r.custom.iter().cloned()).collect(),
        all_custom: last.all_custom.clone(),
        sources,
        parent: last.parent.clone(),
        lots,
        ignored: Vec::new(),
        warnings: Vec::new(),
        purchase: Some((property, product)),
    }
}

fn read_input(path: &Path) -> Result<String> {
    if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).map_err(|e| AppError::io(format!("reading {}: {e}", path.display())))
    }
}

/// `--format X` doubles as the input format when it names one.
fn format_from_flag(f: Option<Format>) -> Option<InputFormat> {
    match f? {
        Format::Homebox => Some(InputFormat::Homebox),
        Format::Csv => Some(InputFormat::Csv),
        Format::Tsv => Some(InputFormat::Tsv),
        Format::Json => Some(InputFormat::Json),
        Format::Jsonl => Some(InputFormat::Jsonl),
        _ => None,
    }
}

fn format_name(f: InputFormat) -> &'static str {
    match f {
        InputFormat::Homebox => "homebox",
        InputFormat::Csv => "csv",
        InputFormat::Tsv => "tsv",
        InputFormat::Json => "json",
        InputFormat::Jsonl => "jsonl",
        InputFormat::Spreadsheet => "spreadsheet",
    }
}

pub fn run(ctx: &Ctx, a: ImportArgs) -> Result<()> {
    let m = load_mapping(&a)?;
    let flag_format = format_from_flag(ctx.format_flag);
    let explicit = a.input_format.or(flag_format);
    let is_sheet = explicit == Some(InputFormat::Spreadsheet) || (explicit.is_none() && sheet::is_spreadsheet(&a.file));
    let (fmt, rows) = if is_sheet {
        (InputFormat::Spreadsheet, sheet_rows(&a.file, &m)?)
    } else {
        let text = read_input(&a.file)?;
        let fmt = detect_format(&a.file, explicit, &text);
        let delim = a
            .input_delimiter
            .as_deref()
            .or(m.delimiter.as_deref())
            .map(|d| crate::config::normalize("csv_delimiter", d))
            .transpose()?;
        let delim = delim.and_then(|d| d.bytes().next()).unwrap_or(ctx.out.delimiter);
        (fmt, parse_rows(&text, fmt, delim)?)
    };
    let headers: Vec<String> =
        rows.iter().flat_map(|(_, r)| r.iter().map(|(h, _)| h.clone())).fold(Vec::new(), |mut acc, h| {
            if !acc.contains(&h) {
                acc.push(h);
            }
            acc
        });
    let homebox =
        fmt == InputFormat::Homebox || (fmt == InputFormat::Spreadsheet && homebox::is_homebox_header(&headers));
    let cols = if homebox { Vec::new() } else { resolve_columns(&headers, &m)? };
    ctx.verbose(&format!("format: {}, columns: {cols:?}", format_name(fmt)));

    let mut plan: Vec<Record> = Vec::new();
    let mut parsed: Vec<Incoming> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    let mut row_warnings: Vec<String> = Vec::new();
    let fail = |line: usize, msg: String, plan: &mut Vec<Record>| -> Result<()> {
        if a.skip_invalid {
            plan.push(to_record(&json!({"line": line, "action": "failed", "id": null, "asset_id": null, "name": null, "location": null, "lots": 0, "error": msg})));
            Ok(())
        } else {
            Err(AppError::invalid(format!("line {line}: {msg} (use --skip-invalid to skip bad rows)")))
        }
    };
    for (line, row) in &rows {
        let r = if homebox { homebox::parse_row(*line, row) } else { parse_native(*line, row, &cols, &m) };
        match r {
            Ok(inc) => {
                row_warnings.extend(inc.warnings.iter().cloned());
                for c in &inc.ignored {
                    if !ignored.contains(c) {
                        ignored.push(c.clone());
                    }
                }
                parsed.push(inc);
            }
            Err(e) => fail(*line, e.message, &mut plan)?,
        }
    }
    let catalog = Catalog::load(&ctx.resolved.config_path)?;
    let types = crate::shelf_life::Types::load(&ctx.resolved.config_path)?;
    let fmts: Vec<String> = m.date_format.iter().cloned().chain(ctx.input_date_formats()).collect();
    let p = Parse { tz: &ctx.tz, date_formats: &fmts };
    let default_property = a.at.as_deref().and_then(|at| store::split_path(at).into_iter().next());
    // Rows sharing an import ref (or id) are one item: the first row carries
    // the item, every row may add a lot. Purchases of one consumable product
    // at one property are one item too, each row a lot.
    let mut groups: Vec<Vec<Incoming>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut purchase_groups: Vec<usize> = Vec::new();
    for inc in parsed {
        if let Some(k) = purchase_group(&inc, &a.labels, default_property.as_deref(), &catalog) {
            let k = format!("purchase\u{1f}{k}");
            match index.get(&k) {
                Some(&g) => groups[g].push(inc),
                None => {
                    index.insert(k, groups.len());
                    purchase_groups.push(groups.len());
                    groups.push(vec![inc]);
                }
            }
            continue;
        }
        let k = inc.id.clone().or_else(|| inc.key.clone());
        match k.as_ref().and_then(|k| index.get(k)) {
            Some(&g) => groups[g].push(inc),
            None => {
                if let Some(k) = k {
                    index.insert(k, groups.len());
                }
                groups.push(vec![inc]);
            }
        }
    }

    let purchases: usize = purchase_groups.iter().map(|g| groups[*g].len()).sum();
    for g in &purchase_groups {
        let rows = std::mem::take(&mut groups[*g]);
        groups[*g] = vec![purchases_item(rows, &p, default_property.as_deref())];
    }
    if purchases > 0 {
        ctx.info(&format!("{purchases} consumable purchase row(s) are lots on {} item(s)", purchase_groups.len()));
    }

    let db = ctx.db()?;
    let batch = new_batch_id();
    let mut ap = Applier {
        db: &db,
        catalog: &catalog,
        types: &types,
        warnings: Vec::new(),
        tree: Tree::load(&db)?,
        p,
        batch: batch.clone(),
        default_at: a.at.clone(),
        extra_labels: a.labels.clone(),
        totals: Totals::default(),
        parents: Vec::new(),
    };
    db.begin()?;
    let result = (|| -> Result<Vec<String>> {
        for g in &groups {
            match ap.apply(g) {
                Ok(r) => plan.push(r),
                Err(e) => fail(g[0].line, e.message, &mut plan)?,
            }
        }
        let mut warnings = std::mem::take(&mut ap.warnings);
        warnings.extend(ap.link_parents()?);
        if !a.dry_run {
            store::record_batch(&db, &batch, &a.file.to_string_lossy(), format_name(fmt), rows.len())?;
        }
        Ok(warnings)
    })();
    let warnings = match result {
        Ok(w) if !a.dry_run => {
            db.commit()?;
            row_warnings.into_iter().chain(w).collect::<Vec<_>>()
        }
        Ok(w) => {
            db.rollback()?;
            row_warnings.into_iter().chain(w).collect()
        }
        Err(e) => {
            let _ = db.rollback();
            return Err(e);
        }
    };
    warnings.iter().for_each(|w| ctx.warn(&format!("warning: {w}")));
    let t = &ap.totals;
    let failed = plan.iter().filter(|r| r.get("action") == Some(&json!("failed"))).count();
    ctx.info(&format!(
        "{}{} row(s): {} created, {} updated, {} unchanged, {} failed; {} location(s) and {} label(s) new, {} lot(s)",
        if a.dry_run { "dry run, nothing written: " } else { "" },
        rows.len(),
        t.created,
        t.updated,
        t.unchanged,
        failed,
        t.locations,
        t.labels,
        t.lots
    ));
    if !ignored.is_empty() {
        ctx.info(&format!("ignored column(s): {}", ignored.join(", ")));
    }
    let mut out = ctx.out.clone();
    if ctx.json_flag {
        out.format = Format::Json;
    } else if flag_format.is_some() {
        out.format = Format::Table;
    }
    let report = Report::list("import", plan)
        .table_columns(&["line", "action", "asset_id", "name", "location", "lots", "error"])
        .meta("format", json!(format_name(fmt)))
        .meta("dry_run", json!(a.dry_run))
        .meta("batch", json!((!a.dry_run).then_some(batch)))
        .meta(
            "summary",
            json!({"rows": rows.len(), "created": t.created, "updated": t.updated, "unchanged": t.unchanged, "failed": failed,
                   "locations_created": t.locations, "labels_created": t.labels, "lots": t.lots}),
        )
        .meta("ignored_columns", json!(ignored))
        .meta("warnings", json!(warnings));
    if out.format == Format::Table && !a.dry_run && !ctx.resolved.flag("verbose") {
        return Ok(());
    }
    ctx.emit_with(&report, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_prefer_exact_names_over_synonyms() {
        let h = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let cols = resolve_columns(
            &h(&["Item", "Qty", "value", "replacement_value", "Where", "Brand", "Color"]),
            &Mapping::default(),
        )
        .unwrap();
        let get = |c: &str| cols.iter().find(|(h, _)| h == c).map(|(_, t)| t.as_str());
        assert_eq!(get("Item"), Some("name"));
        assert_eq!(get("Qty"), Some("quantity"));
        assert_eq!(get("replacement_value"), Some("replacement_value"));
        assert_eq!(get("value"), None, "a synonym never shadows the exact column");
        assert_eq!(get("Where"), Some("location"));
        assert_eq!(get("Brand"), Some("manufacturer"));
        assert_eq!(get("Color"), None);
    }

    #[test]
    fn detects_homebox_by_header() {
        let p = Path::new("x.csv");
        assert_eq!(detect_format(p, None, "HB.name,HB.location\na,b\n"), InputFormat::Homebox);
        assert_eq!(detect_format(p, None, "name,location\na,b\n"), InputFormat::Csv);
        assert_eq!(detect_format(Path::new("x.json"), None, "[]"), InputFormat::Json);
    }

    #[test]
    fn custom_fields_parse_both_ways() {
        assert_eq!(parse_custom(r#"{"Color":"red","Size":3}"#).unwrap().get("Size").map(String::as_str), Some("3"));
        assert_eq!(parse_custom("Color=red; Size=L").unwrap().len(), 2);
        assert!(parse_custom("nope").is_err());
    }
}
