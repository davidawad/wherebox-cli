//! Lists and cohorts: hand-editable TOML files in the config directory, and
//! the matching ladder that checks them against a profile's inventory
//! (docs/SPEC.md, "Lists and matching").
//!
//! Files are read with serde and edited with `toml_edit`, so CLI edits keep
//! comments, key order and formatting.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item as TItem, Table};

use crate::db::Db;
use crate::error::{AppError, Result};
use crate::matching::{self, NameScore, Ranked, UrlKey};
use crate::onhand::{Estimates, Estimator};
use crate::store::{self, Item, Lot, Tree};

/// List types with their own behaviour; any other string is a custom type
/// checked like a recipe.
pub const TYPES: &[&str] = &["recipe", "kit", "packing", "wishlist"];

// ---------------------------------------------------------------------------
// File model
// ---------------------------------------------------------------------------

fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<V>::deserialize(d)? {
        None => Vec::new(),
        Some(V::One(s)) => vec![s],
        Some(V::Many(v)) => v,
    }
    .into_iter()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect())
}

fn number<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        I(i64),
        F(f64),
    }
    Ok(Option::<N>::deserialize(d)?.map(|n| match n {
        N::I(i) => i as f64,
        N::F(f) => f,
    }))
}

/// The matcher fields shared by list entries and cohorts.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Matcher {
    #[serde(default, deserialize_with = "one_or_many")]
    pub asin: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub sku: Vec<String>,
    #[serde(default, alias = "gtin", deserialize_with = "one_or_many")]
    pub upc: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub product_ref: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub url: Vec<String>,
    #[serde(default, alias = "any_names", deserialize_with = "one_or_many")]
    pub any: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub label: Vec<String>,
    #[serde(default, alias = "kind", deserialize_with = "one_or_many")]
    pub preset: Vec<String>,
    #[serde(default, alias = "location", deserialize_with = "one_or_many")]
    pub at: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub cohort: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub exclude: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub pins: Vec<String>,
    #[serde(default)]
    pub fuzzy: Option<bool>,
    #[serde(default, deserialize_with = "number")]
    pub threshold: Option<f64>,
}

impl Matcher {
    /// Selectors other than names and pins (which make the entry's own name
    /// a fuzzy matcher only on request).
    fn has_selectors(&self) -> bool {
        [&self.asin, &self.sku, &self.upc, &self.product_ref, &self.url, &self.label, &self.preset, &self.cohort]
            .iter()
            .any(|v| !v.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    #[serde(default, deserialize_with = "number")]
    pub qty: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(flatten)]
    pub m: Matcher,
}

impl Entry {
    pub fn want(&self) -> f64 {
        self.qty.unwrap_or(1.0)
    }
    pub fn unit(&self) -> String {
        self.unit.as_deref().map_or_else(|| "ea".to_string(), matching::canonical_unit)
    }
    /// Is the entry's own name a fuzzy matcher?
    pub fn fuzzy(&self) -> bool {
        self.m.fuzzy.unwrap_or(!self.m.has_selectors())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ListFile {
    pub name: String,
    #[serde(rename = "type", default = "default_type")]
    pub kind: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, deserialize_with = "number")]
    pub threshold: Option<f64>,
    #[serde(default, rename = "entry")]
    pub entries: Vec<Entry>,
}

fn default_type() -> String {
    "recipe".into()
}

impl ListFile {
    pub fn is_wishlist(&self) -> bool {
        self.kind.eq_ignore_ascii_case("wishlist")
    }
    /// Kits and packing lists need the thing here: lent items don't count.
    pub fn needs_present(&self) -> bool {
        matches!(self.kind.to_ascii_lowercase().as_str(), "kit" | "packing")
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CohortFile {
    pub name: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(flatten)]
    pub m: Matcher,
}

/// A parsed file and its editable document.
pub struct Doc<T> {
    pub path: PathBuf,
    pub slug: String,
    pub data: T,
    pub doc: DocumentMut,
}

impl<T: for<'de> Deserialize<'de>> Doc<T> {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).map_err(|e| AppError::io(format!("reading {}: {e}", path.display())))?;
        let bad = |e: String| AppError::invalid(format!("{}: {e}", path.display()));
        let doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| bad(e.to_string()))?;
        let data: T = toml::from_str(&text).map_err(|e| bad(e.to_string()))?;
        let slug = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self { path: path.to_path_buf(), slug, data, doc })
    }

    /// Write the document back (atomically) and re-parse it.
    pub fn save(&mut self) -> Result<()> {
        write_atomic(&self.path, &self.doc.to_string())?;
        let reloaded = Self::load(&self.path)?;
        self.data = reloaded.data;
        Ok(())
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| AppError::io(format!("creating {}: {e}", dir.display())))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| AppError::io(format!("writing {}: {e}", path.display())))
}

/// Which kind of file: lists or cohorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    List,
    Cohort,
}

impl Kind {
    fn dir_name(self) -> &'static str {
        match self {
            Self::List => "lists",
            Self::Cohort => "cohorts",
        }
    }
    fn noun(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Cohort => "cohort",
        }
    }
}

/// `<config dir>/lists` or `<config dir>/cohorts`.
pub fn dir(config_path: &Path, kind: Kind) -> PathBuf {
    config_path.parent().unwrap_or(Path::new(".")).join(kind.dir_name())
}

/// Every `*.toml` of a kind, by slug.
pub fn files(config_path: &Path, kind: Kind) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir(config_path, kind)) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    v.sort();
    v
}

/// The file of a list or cohort named by slug or name.
pub fn find(config_path: &Path, kind: Kind, spec: &str) -> Result<PathBuf> {
    let slug = crate::util::slugify(spec);
    let direct = dir(config_path, kind).join(format!("{slug}.toml"));
    if !slug.is_empty() && direct.is_file() {
        return Ok(direct);
    }
    #[derive(Deserialize)]
    struct Named {
        #[serde(default)]
        name: String,
    }
    for p in files(config_path, kind) {
        let named = std::fs::read_to_string(&p).ok().and_then(|t| toml::from_str::<Named>(&t).ok());
        if named.is_some_and(|n| n.name.trim().eq_ignore_ascii_case(spec.trim())) {
            return Ok(p);
        }
    }
    Err(AppError::not_found(format!("no {} '{spec}' (see `wherebox {} ls`)", kind.noun(), kind.noun())))
}

/// Create a new file `slugify(name).toml` with `fields` (in order).
pub fn create(config_path: &Path, kind: Kind, name: &str, fields: &[(&str, TItem)]) -> Result<PathBuf> {
    let slug = crate::util::slugify(name);
    if slug.is_empty() {
        return Err(AppError::usage(format!("{} name '{name}' has no letters or digits", kind.noun())));
    }
    let path = dir(config_path, kind).join(format!("{slug}.toml"));
    if path.exists() {
        return Err(AppError::invalid(format!("{} '{slug}' already exists: {}", kind.noun(), path.display())));
    }
    let mut doc = DocumentMut::new();
    doc["name"] = value(name.trim());
    for (k, v) in fields {
        doc[*k] = v.clone();
    }
    write_atomic(&path, &doc.to_string())?;
    Ok(path)
}

/// Move a file into `.trash/` beside it (never deleted); returns the new path.
pub fn trash(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let bin = parent.join(".trash");
    std::fs::create_dir_all(&bin).map_err(|e| AppError::io(format!("creating {}: {e}", bin.display())))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut dest = bin.join(&name);
    if dest.exists() {
        let stamp = crate::util::now_iso().replace([':', '-'], "");
        dest = bin.join(format!("{}.{stamp}.toml", name.trim_end_matches(".toml")));
        let mut n = 1;
        while dest.exists() {
            n += 1;
            dest = bin.join(format!("{}.{stamp}-{n}.toml", name.trim_end_matches(".toml")));
        }
    }
    std::fs::rename(path, &dest).map_err(|e| AppError::io(format!("moving {} to the trash: {e}", path.display())))?;
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Editing (toml_edit)
// ---------------------------------------------------------------------------

/// Order of keys in a newly written entry or cohort.
pub const KEY_ORDER: &[&str] = &[
    "name",
    "qty",
    "unit",
    "note",
    "notes",
    "optional",
    "asin",
    "sku",
    "upc",
    "product_ref",
    "url",
    "any",
    "label",
    "preset",
    "at",
    "cohort",
    "exclude",
    "fuzzy",
    "threshold",
    "pins",
];

/// Fields of a matcher that hold strings (one or many).
pub const LIST_KEYS: &[&str] =
    &["asin", "sku", "upc", "product_ref", "url", "any", "label", "preset", "at", "cohort", "exclude", "pins"];

/// The canonical key of an alias as written in a file.
fn aliases(key: &str) -> &'static [&'static str] {
    match key {
        "upc" => &["gtin"],
        "any" => &["any_names"],
        "preset" => &["kind"],
        "at" => &["location"],
        _ => &[],
    }
}

/// One string or an array, as a TOML value (a single value stays a string).
pub fn strings(v: &[String]) -> TItem {
    match v {
        [one] => value(one.as_str()),
        many => value(many.iter().map(String::as_str).collect::<Array>()),
    }
}

/// Strings of a key in a table (string or array).
pub fn get_strings(t: &Table, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    for k in std::iter::once(key).chain(aliases(key).iter().copied()) {
        match t.get(k) {
            Some(TItem::Value(toml_edit::Value::String(s))) => out.push(s.value().clone()),
            Some(TItem::Value(toml_edit::Value::Array(a))) => {
                out.extend(a.iter().filter_map(|v| v.as_str().map(str::to_string)));
            }
            _ => {}
        }
    }
    out
}

/// Replace `key` (and its aliases) in a table, keeping its position when it
/// exists, else appending in [`KEY_ORDER`] position.
pub fn set_key(t: &mut Table, key: &str, item: Option<TItem>) {
    for a in aliases(key) {
        t.remove(a);
    }
    match item {
        None => {
            t.remove(key);
        }
        Some(item) if t.contains_key(key) => {
            t[key] = item;
        }
        Some(item) => {
            t.insert(key, item);
            // Position new keys by KEY_ORDER among the keys we know.
            let rank = |k: &str| KEY_ORDER.iter().position(|x| *x == k).unwrap_or(KEY_ORDER.len());
            let mine = rank(key);
            let pos = t
                .iter()
                .filter(|(k, _)| *k != key)
                .enumerate()
                .filter(|(_, (k, _))| rank(k) <= mine)
                .map(|(i, _)| i + 1)
                .max()
                .unwrap_or(0);
            let keys: Vec<String> = t.iter().map(|(k, _)| k.to_string()).collect();
            let mut order: Vec<String> = keys.iter().filter(|k| *k != key).cloned().collect();
            order.insert(pos.min(order.len()), key.to_string());
            t.sort_values_by(|a, _, b, _| {
                let ia = order.iter().position(|k| k == a.get()).unwrap_or(usize::MAX);
                let ib = order.iter().position(|k| k == b.get()).unwrap_or(usize::MAX);
                ia.cmp(&ib)
            });
        }
    }
}

/// The `[[entry]]` array of a list document.
pub fn entries_mut(doc: &mut DocumentMut) -> &mut ArrayOfTables {
    if !doc.contains_array_of_tables("entry") {
        doc.insert("entry", TItem::ArrayOfTables(ArrayOfTables::new()));
    }
    doc["entry"].as_array_of_tables_mut().expect("entry is an array of tables")
}

/// Index of an entry by name (case-insensitive) or 1-based position (`3`, `#3`).
pub fn entry_index(list: &ListFile, spec: &str) -> Result<usize> {
    let s = spec.trim();
    if let Some(i) = list.entries.iter().position(|e| e.name.trim().eq_ignore_ascii_case(s)) {
        return Ok(i);
    }
    if let Ok(n) = s.trim_start_matches('#').parse::<usize>() {
        if (1..=list.entries.len()).contains(&n) {
            return Ok(n - 1);
        }
    }
    let low = s.to_lowercase();
    let hits: Vec<usize> =
        (0..list.entries.len()).filter(|i| list.entries[*i].name.to_lowercase().contains(&low)).collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err(AppError::not_found(format!("no entry '{s}' in list '{}' (see `wherebox list show`)", list.name))),
        many => Err(AppError::invalid(format!(
            "'{s}' matches {} entries: {} (use the name or position)",
            many.len(),
            many.iter().map(|i| list.entries[*i].name.clone()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

// ---------------------------------------------------------------------------
// Cohort resolution
// ---------------------------------------------------------------------------

/// A matcher with its cohorts loaded, recursively.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub m: Matcher,
    /// Whether the matcher's own name is a fuzzy query, and the name.
    pub own_name: Option<String>,
    pub cohorts: Vec<(String, Resolved)>,
}

/// Load the cohorts a matcher includes, detecting cycles.
pub fn resolve(
    m: &Matcher,
    own_name: Option<String>,
    load: &mut dyn FnMut(&str) -> Result<Matcher>,
    stack: &mut Vec<String>,
) -> Result<Resolved> {
    let mut cohorts = Vec::new();
    for c in &m.cohort {
        let slug = crate::util::slugify(c);
        if let Some(at) = stack.iter().position(|s| *s == slug) {
            let mut path: Vec<String> = stack[at..].to_vec();
            path.push(slug);
            return Err(AppError::invalid(format!("cohort cycle: {}", path.join(" -> "))));
        }
        let inner = load(&slug)?;
        stack.push(slug.clone());
        let r = resolve(&inner, None, load, stack)?;
        stack.pop();
        cohorts.push((slug, r));
    }
    Ok(Resolved { m: m.clone(), own_name, cohorts })
}

/// Resolve against the cohort files of a config directory.
pub fn resolve_files(config_path: &Path, m: &Matcher, own_name: Option<String>, top: Option<&str>) -> Result<Resolved> {
    let mut cache: HashMap<String, Matcher> = HashMap::new();
    let mut load = |slug: &str| -> Result<Matcher> {
        if let Some(m) = cache.get(slug) {
            return Ok(m.clone());
        }
        let p = find(config_path, Kind::Cohort, slug)?;
        let c: Doc<CohortFile> = Doc::load(&p)?;
        cache.insert(slug.to_string(), c.data.m.clone());
        Ok(c.data.m)
    };
    let mut stack: Vec<String> = top.map(|t| vec![t.to_string()]).unwrap_or_default();
    resolve(m, own_name, &mut load, &mut stack)
}

// ---------------------------------------------------------------------------
// Inventory and the ladder
// ---------------------------------------------------------------------------

/// Custom fields holding identifiers.
pub const ID_FIELDS: &[&str] =
    &["ASIN", "SKU", "UPC", "GTIN", "EAN", "ISBN", "MPN", "Product Ref", "Item Number", "Part Number"];

/// One profile's items, prepared for matching.
pub struct Inventory {
    pub profile: Option<String>,
    pub today: NaiveDate,
    pub items: Vec<Item>,
    pub fields: Vec<BTreeMap<String, String>>,
    pub labels: Vec<Vec<String>>,
    pub lots: Vec<Vec<Lot>>,
    pub tree: Tree,
    /// Computed expiry and estimated on hand (see [`crate::onhand`]).
    pub est: Estimates,
    ids: Vec<BTreeSet<String>>,
    urls: Vec<Vec<UrlKey>>,
    tokens: Vec<Vec<String>>,
}

impl Inventory {
    pub fn load(db: &Db, profile: Option<String>, estimator: &Estimator) -> Result<Self> {
        let est = estimator.all(db)?;
        let today = estimator.today;
        let items: Vec<Item> = store::all_items(db)?.into_iter().filter(|i| !i.is_gone()).collect();
        let mut fields = store::all_item_fields(db)?;
        let mut labels = store::all_item_labels(db)?;
        let mut lots: HashMap<String, Vec<Lot>> = HashMap::new();
        for l in store::all_lots(db)? {
            lots.entry(l.item_id.clone()).or_default().push(l);
        }
        let tree = Tree::load(db)?;
        let fields: Vec<BTreeMap<String, String>> =
            items.iter().map(|i| fields.remove(&i.id).unwrap_or_default()).collect();
        let labels: Vec<Vec<String>> = items.iter().map(|i| labels.remove(&i.id).unwrap_or_default()).collect();
        let lots: Vec<Vec<Lot>> = items.iter().map(|i| lots.remove(&i.id).unwrap_or_default()).collect();
        Ok(Self::new(profile, today, items, fields, labels, lots, tree, est))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile: Option<String>,
        today: NaiveDate,
        items: Vec<Item>,
        fields: Vec<BTreeMap<String, String>>,
        labels: Vec<Vec<String>>,
        lots: Vec<Vec<Lot>>,
        tree: Tree,
        est: Estimates,
    ) -> Self {
        let ids = items
            .iter()
            .zip(&fields)
            .map(|(i, f)| {
                let mut vals: Vec<&str> = [&i.product_ref, &i.model_number, &i.serial_number]
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
                    .collect();
                for name in ID_FIELDS {
                    vals.extend(crate::consumable::field(Some(f), name));
                }
                vals.iter().flat_map(|v| matching::id_keys(v)).collect()
            })
            .collect();
        let urls = items
            .iter()
            .zip(&fields)
            .map(|(i, f)| {
                let text = [i.notes.as_deref(), i.description.as_deref()];
                f.values()
                    .map(String::as_str)
                    .chain(text.into_iter().flatten())
                    .flat_map(matching::urls_in)
                    .filter_map(matching::normalize_url)
                    .collect()
            })
            .collect();
        let tokens = items.iter().map(|i| matching::name_tokens(&i.name)).collect();
        Self { profile, today, items, fields, labels, lots, tree, est, ids, urls, tokens }
    }

    /// Lower-case location names of an item location, root first.
    fn path_names(&self, loc: Option<i64>) -> Vec<String> {
        loc.map(|id| self.tree.lineage(id).iter().map(|l| l.name.trim().to_lowercase()).collect()).unwrap_or_default()
    }

    /// Is a location at (or below) a place whose path contains `at`'s segments in order?
    pub fn loc_matches(&self, loc: Option<i64>, at: &[String]) -> bool {
        if at.is_empty() {
            return true;
        }
        let names = self.path_names(loc);
        at.iter().any(|spec| {
            let want: Vec<String> = store::split_path(spec).iter().map(|s| s.to_lowercase()).collect();
            !want.is_empty() && names.windows(want.len()).any(|w| w == want.as_slice())
        })
    }

    /// Does the item (or, for consumables, one of its active lots) satisfy `at`?
    fn item_at(&self, i: usize, at: &[String]) -> bool {
        self.loc_matches(self.items[i].location_id, at)
            || self.lots[i].iter().filter(|l| l.is_active()).any(|l| self.loc_matches(l.location_id, at))
    }

    pub fn item_path(&self, i: usize) -> Option<String> {
        self.tree.opt_path(self.items[i].location_id)
    }

    /// The identifier a pin stores for an item.
    pub fn pin_for(&self, i: usize) -> String {
        pin_of(&self.items[i], &self.fields[i], self.profile.as_deref())
    }

    fn pin_matches(&self, i: usize, pin: &str) -> bool {
        let (kind, rest) = pin.split_once(':').unwrap_or(("ref", pin));
        match kind.to_ascii_lowercase().as_str() {
            "item" => {
                let (p, id) = rest.rsplit_once(':').unwrap_or(("", rest));
                (p.is_empty() || Some(p) == self.profile.as_deref()) && self.items[i].id.eq_ignore_ascii_case(id)
            }
            _ => matching::id_keys(rest).iter().any(|k| self.ids[i].contains(k)),
        }
    }
}

/// The pin `list pin` stores for an item: its ASIN field, else its product
/// ref, else a UPC/GTIN/EAN field, else `item:<profile>:<id>`.
pub fn pin_of(item: &Item, fields: &BTreeMap<String, String>, profile: Option<&str>) -> String {
    let f = Some(fields);
    if let Some(a) = crate::consumable::field(f, "ASIN").filter(|v| !v.trim().is_empty()) {
        return format!("asin:{}", a.trim());
    }
    if let Some(r) = item.product_ref.as_deref().filter(|v| !v.trim().is_empty()) {
        return format!("ref:{}", r.trim());
    }
    for n in ["UPC", "GTIN", "EAN"] {
        if let Some(u) = crate::consumable::field(f, n).filter(|v| !v.trim().is_empty()) {
            return format!("upc:{}", u.trim());
        }
    }
    format!("item:{}:{}", profile.unwrap_or(""), item.id)
}

/// Rungs of the ladder, highest confidence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Identifier,
    Url,
    Pinned,
    Attribute,
    Name,
}

impl Tier {
    pub const ALL: [Tier; 5] = [Tier::Identifier, Tier::Url, Tier::Pinned, Tier::Attribute, Tier::Name];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identifier => "identifier",
            Self::Url => "url",
            Self::Pinned => "pinned",
            Self::Attribute => "attribute",
            Self::Name => "name",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub tier: Tier,
    pub score: f64,
    pub via: String,
    /// Fuzzy hits: the query and whether every query token was found.
    pub query: Option<String>,
    pub full: bool,
}

impl Hit {
    fn exact(tier: Tier, via: String) -> Self {
        Self { tier, score: 1.0, via, query: None, full: true }
    }
}

/// Everything the ladder found for one item.
#[derive(Debug, Clone, Default)]
pub struct Eval {
    pub hits: Vec<Hit>,
    /// The best fuzzy score of any name query, even below the threshold.
    pub best_name: Option<(String, NameScore)>,
    pub excluded: Option<String>,
    pub location_ok: bool,
}

impl Eval {
    /// The match, unless excluded or out of place: the highest tier, then score.
    pub fn best(&self) -> Option<&Hit> {
        if self.excluded.is_some() || !self.location_ok {
            return None;
        }
        self.hits.iter().min_by(|a, b| a.tier.cmp(&b.tier).then(b.score.total_cmp(&a.score)))
    }
}

fn add_name(e: &mut Eval, q: &str, s: NameScore) {
    if e.best_name.as_ref().is_none_or(|(_, b)| s.score > b.score) {
        e.best_name = Some((q.to_string(), s));
    }
}

/// Run the ladder of a resolved matcher over item `i`.
pub fn eval(inv: &Inventory, r: &Resolved, i: usize, threshold: f64) -> Eval {
    let m = &r.m;
    let threshold = m.threshold.unwrap_or(threshold);
    let item = &inv.items[i];
    let mut e = Eval { location_ok: inv.item_at(i, &m.at), ..Default::default() };
    if let Some(x) =
        m.exclude.iter().find(|x| matching::score_tokens(&matching::name_tokens(x), &inv.tokens[i]).score >= threshold)
    {
        e.excluded = Some(x.clone());
    }
    // a. identifiers
    for (field, vals) in [("asin", &m.asin), ("sku", &m.sku), ("upc", &m.upc), ("product_ref", &m.product_ref)] {
        for v in vals {
            if matching::id_keys(v).iter().any(|k| inv.ids[i].contains(k)) {
                e.hits.push(Hit::exact(Tier::Identifier, format!("{field} {v}")));
            }
        }
    }
    // b. URLs
    for u in &m.url {
        let hit = match matching::normalize_url(u) {
            Some(UrlKey::Asin(a)) => {
                matching::id_keys(&a).iter().any(|k| inv.ids[i].contains(k)) || inv.urls[i].contains(&UrlKey::Asin(a))
            }
            Some(k) => inv.urls[i].contains(&k),
            None => false,
        };
        if hit {
            e.hits.push(Hit::exact(Tier::Url, format!("url {u}")));
        }
    }
    // c. pins
    for p in &m.pins {
        if inv.pin_matches(i, p) {
            e.hits.push(Hit::exact(Tier::Pinned, format!("pin {p}")));
        }
    }
    // d. attributes
    for l in &m.label {
        if inv.labels[i].iter().any(|x| x.trim().eq_ignore_ascii_case(l.trim())) {
            e.hits.push(Hit::exact(Tier::Attribute, format!("label {l}")));
        }
    }
    for p in &m.preset {
        if item.kind_preset.as_deref().is_some_and(|k| k.trim().eq_ignore_ascii_case(p.trim())) {
            e.hits.push(Hit::exact(Tier::Attribute, format!("preset {p}")));
        }
    }
    // cohorts: whatever tier their own ladder reached
    for (slug, c) in &r.cohorts {
        let ce = eval(inv, c, i, threshold);
        if let Some(h) = ce.best() {
            e.hits.push(Hit { via: format!("cohort {slug}: {}", h.via), ..h.clone() });
        }
        if let Some((q, s)) = ce.best_name {
            add_name(&mut e, &q, s);
        }
    }
    // e. names
    let own = r.own_name.iter().map(|n| (n, "name"));
    for (q, what) in own.chain(m.any.iter().map(|n| (n, "any"))) {
        let s = matching::score_tokens(&matching::name_tokens(q), &inv.tokens[i]);
        add_name(&mut e, q, s);
        if s.score >= threshold {
            e.hits.push(Hit {
                tier: Tier::Name,
                score: s.score,
                via: format!("{what} \"{q}\""),
                query: Some(q.clone()),
                full: s.full,
            });
        }
    }
    e
}

/// A matched item with its best hit.
#[derive(Debug, Clone)]
pub struct Match {
    pub idx: usize,
    pub hit: Hit,
    /// Item details, filled in for output.
    pub item: Option<MatchedItem>,
}

#[derive(Debug, Clone, Default)]
pub struct MatchedItem {
    pub name: String,
    pub id: String,
    pub asset_id: Option<String>,
    pub location: Option<String>,
    pub status: String,
    pub vendor: Option<String>,
    pub product_ref: Option<String>,
}

/// Every item a matcher matches (best hit each, best first) and whether the
/// fuzzy part is ambiguous (only when nothing matched above the name tier).
pub fn run(inv: &Inventory, r: &Resolved, threshold: f64) -> (Vec<Match>, bool) {
    let mut out: Vec<Match> = (0..inv.items.len())
        .filter_map(|i| eval(inv, r, i, threshold).best().map(|h| Match { idx: i, hit: h.clone(), item: None }))
        .collect();
    out.sort_by(|a, b| {
        a.hit
            .tier
            .cmp(&b.hit.tier)
            .then(b.hit.score.total_cmp(&a.hit.score))
            .then_with(|| inv.items[a.idx].name.to_lowercase().cmp(&inv.items[b.idx].name.to_lowercase()))
    });
    let fuzzy_only = out.iter().all(|m| m.hit.tier == Tier::Name);
    let mut by_query: BTreeMap<String, Vec<Ranked<usize>>> = BTreeMap::new();
    for m in out.iter().filter(|m| m.hit.tier == Tier::Name) {
        by_query.entry(m.hit.query.clone().unwrap_or_default()).or_default().push(Ranked {
            key: m.idx,
            name: inv.items[m.idx].name.clone(),
            score: NameScore { score: m.hit.score, full: m.hit.full },
        });
    }
    let ambiguous = fuzzy_only && by_query.values().any(|r| matching::ambiguous(r));
    (out, ambiguous)
}

// ---------------------------------------------------------------------------
// Have-check
// ---------------------------------------------------------------------------

/// How much one matched item contributes, in the entry's unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    /// `None`: unit mismatch (unknown).
    pub qty: Option<f64>,
    /// Quantity on hand in the item's own unit.
    pub raw: f64,
    pub unit: String,
    /// Expired lots (confirmed on hand after their expiry), in the entry's
    /// unit when convertible (else the item's).
    pub expired: f64,
    /// Some lot was presumed used up: the quantity is an estimate.
    pub estimated: bool,
    /// Lots presumed used up, in the entry's unit when convertible.
    pub used_up: f64,
    /// Durables: the item counts as owned (wishlist "already own").
    pub owned: bool,
    /// Kits and packing lists: owned, but lent out (so not counted).
    pub lent: bool,
    pub basis: &'static str,
}

/// `qty` of the item's unit in `want`. Count units (`ea`, `can`, `jar`...)
/// compare one for one, except that an item counted in `ea`/`pack` whose name
/// states a pack count (`12-Pack`, `90 Count`) holds that many of an inner
/// unit (`can`, `capsule`). An item counted in `ea`/`pack` whose name states a
/// size (`16 oz`) holds that much of a mass or volume.
fn in_unit(qty: f64, unit: &str, name: &str, want: &str) -> (Option<f64>, &'static str) {
    let count = |u: &str| matches!(matching::unit_info(u), Some((matching::Dim::Count, _)));
    let outer = |u: &str| matches!(u, "ea" | "pack");
    let want_c = matching::canonical_unit(want);
    if outer(unit) && count(&want_c) && !outer(&want_c) {
        if let Some(n) = matching::pack_count_from_name(name) {
            return (Some(qty * n), "pack count from name");
        }
    }
    if let Some(q) = matching::convert(qty, unit, want) {
        return (Some(q), "unit");
    }
    if count(unit) {
        if let Some((size, su)) = matching::size_from_name(name) {
            if let Some(q) = matching::convert(qty * size, &su, want) {
                return (Some(q), "size from name");
            }
        }
    }
    (None, "unit mismatch")
}

pub fn contribution(inv: &Inventory, i: usize, at: &[String], want_unit: &str, present_only: bool) -> Contribution {
    let item = &inv.items[i];
    let unit = item.unit.as_deref().map_or_else(|| "ea".to_string(), matching::canonical_unit);
    let lots = &inv.lots[i];
    if lots.is_empty() {
        let ok = match item.status.as_str() {
            "owned" | "stored" => true,
            "lent" => !present_only,
            _ => false,
        };
        let raw = if ok { item.quantity.max(0) as f64 } else { 0.0 };
        let (qty, basis) = in_unit(raw, &unit, &item.name, want_unit);
        let lent = present_only && item.status == "lent";
        return Contribution { qty, raw, unit, expired: 0.0, estimated: false, used_up: 0.0, owned: ok, lent, basis };
    }
    let here = |l: &&Lot| at.is_empty() || inv.loc_matches(l.location_id.or(item.location_id), at);
    let active: Vec<&Lot> = lots.iter().filter(|l| l.is_active() && l.qty > 0.0).filter(here).collect();
    let state = |l: &Lot| inv.est.lot(l).map_or((false, false), |v| (v.used_up, v.expired));
    let estimated = active.iter().any(|l| state(l).0);
    // `+ 0.0`: an empty f64 sum is `-0.0`.
    let expired = active.iter().filter(|l| state(l) == (false, true)).map(|l| l.qty).sum::<f64>() + 0.0;
    let raw = active.iter().filter(|l| state(l) == (false, false)).map(|l| l.qty).sum::<f64>() + 0.0;
    let (qty, basis) = in_unit(raw, &unit, &item.name, want_unit);
    let expired = in_unit(expired, &unit, &item.name, want_unit).0.unwrap_or(expired);
    let used_up = active.iter().filter(|l| state(l).0).map(|l| l.qty).sum::<f64>() + 0.0;
    let used_up = in_unit(used_up, &unit, &item.name, want_unit).0.unwrap_or(used_up);
    Contribution { qty, raw, unit, expired, estimated, used_up, owned: raw > 0.0, lent: false, basis }
}

/// Status of one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Status {
    #[serde(rename = "have")]
    Have,
    #[serde(rename = "short")]
    Short,
    #[serde(rename = "missing")]
    Missing,
    #[serde(rename = "ambiguous")]
    Ambiguous,
    #[serde(rename = "unit-mismatch")]
    UnitMismatch,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "fulfilled")]
    Fulfilled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Have => "have",
            Self::Short => "short",
            Self::Missing => "missing",
            Self::Ambiguous => "ambiguous",
            Self::UnitMismatch => "unit-mismatch",
            Self::Open => "open",
            Self::Fulfilled => "fulfilled",
        }
    }
}

/// The check of one entry against one inventory.
#[derive(Debug, Clone)]
pub struct EntryCheck {
    pub want: f64,
    pub unit: String,
    pub have: f64,
    pub expired: f64,
    /// Some matched lot was presumed used up.
    pub estimated: bool,
    /// How much was presumed used up.
    pub used_up: f64,
    pub status: Status,
    pub ambiguous: bool,
    pub matches: Vec<(Match, Contribution)>,
}

pub fn check_entry(inv: &Inventory, list: &ListFile, entry: &Entry, r: &Resolved, threshold: f64) -> EntryCheck {
    let threshold = entry.m.threshold.or(list.threshold).unwrap_or(threshold);
    let (found, ambiguous) = run(inv, r, threshold);
    let unit = entry.unit();
    let want = entry.want();
    let matches: Vec<(Match, Contribution)> = found
        .into_iter()
        .map(|m| {
            let c = contribution(inv, m.idx, &entry.m.at, &unit, list.needs_present());
            (m, c)
        })
        .collect();
    let have = matches.iter().filter_map(|(_, c)| c.qty).sum::<f64>() + 0.0;
    let expired = matches.iter().map(|(_, c)| c.expired).sum::<f64>() + 0.0;
    let estimated = matches.iter().any(|(_, c)| c.estimated);
    let used_up = matches.iter().map(|(_, c)| c.used_up).sum::<f64>() + 0.0;
    let unknown = matches.iter().any(|(_, c)| c.qty.is_none() && c.raw > 0.0);
    let status = if list.is_wishlist() {
        if matches.iter().any(|(_, c)| c.owned) && !ambiguous {
            Status::Fulfilled
        } else {
            Status::Open
        }
    } else if matches.is_empty() {
        Status::Missing
    } else if ambiguous {
        Status::Ambiguous
    } else if have + 1e-9 >= want {
        Status::Have
    } else if unknown {
        Status::UnitMismatch
    } else if have > 0.0 {
        Status::Short
    } else {
        Status::Missing
    };
    EntryCheck { want, unit, have, expired, estimated, used_up, status, ambiguous, matches }
}

/// `ready`, or `missing N of M` (non-optional entries not had).
pub fn verdict(list: &ListFile, checks: &[EntryCheck]) -> (bool, String) {
    if list.is_wishlist() {
        let done = checks.iter().filter(|c| c.status == Status::Fulfilled).count();
        return (true, format!("{} open, {done} fulfilled", checks.len() - done));
    }
    let required: Vec<&EntryCheck> =
        list.entries.iter().zip(checks).filter(|(e, _)| !e.optional).map(|(_, c)| c).collect();
    let missing = required.iter().filter(|c| c.status != Status::Have).count();
    if missing == 0 {
        (true, "ready".into())
    } else {
        (false, format!("missing {missing} of {}", required.len()))
    }
}

// ---------------------------------------------------------------------------
// Plain-text import
// ---------------------------------------------------------------------------

fn vulgar(c: char) -> Option<f64> {
    Some(match c {
        '½' => 0.5,
        '⅓' => 1.0 / 3.0,
        '⅔' => 2.0 / 3.0,
        '¼' => 0.25,
        '¾' => 0.75,
        '⅕' => 0.2,
        '⅛' => 0.125,
        '⅜' => 0.375,
        '⅝' => 0.625,
        '⅞' => 0.875,
        _ => return None,
    })
}

/// One quantity token: `2`, `0.5`, `1,5`, `1/2`, `½`, `1½`.
fn amount_token(t: &str) -> Option<f64> {
    if let Some((a, b)) = t.split_once('/') {
        let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
        return (b > 0.0).then_some(a / b);
    }
    let last = t.chars().last()?;
    if let Some(f) = vulgar(last) {
        let head = &t[..t.len() - last.len_utf8()];
        return if head.is_empty() { Some(f) } else { crate::util::parse_qty(head).ok().map(|w| w + f) };
    }
    crate::util::parse_qty(t).ok()
}

/// A quantity such as `2`, `0.5`, `1/2`, `1 1/2`, `½`, `1½`.
pub fn parse_amount(s: &str) -> Option<f64> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    match toks.as_slice() {
        [one] => amount_token(one),
        [whole, frac] if frac.contains('/') || frac.chars().all(|c| vulgar(c).is_some()) => {
            Some(amount_token(whole)? + amount_token(frac)?)
        }
        _ => None,
    }
}

/// Unit words of recipes and shopping lists beyond [`matching::unit_info`].
const COUNT_WORDS: &[&str] = &[
    "pack", "can", "clove", "pinch", "bunch", "jar", "bottle", "bag", "box", "stick", "slice", "sprig", "dash",
    "handful", "head", "packet", "tin", "carton", "dozen", "loaf", "ea", "each",
];

/// The unit a word names (canonical spelling), if it is one.
fn unit_word(w: &str) -> Option<String> {
    let w = w.trim_end_matches('.').to_lowercase();
    if w.is_empty() {
        return None;
    }
    if matching::unit_info(&w).is_some() {
        return Some(matching::canonical_unit(&w));
    }
    let one = w.strip_suffix("es").filter(|s| COUNT_WORDS.contains(s)).or_else(|| w.strip_suffix('s'));
    COUNT_WORDS.iter().find(|c| **c == w || Some(**c) == one).map(|c| {
        if *c == "each" {
            "ea".to_string()
        } else {
            c.to_string()
        }
    })
}

/// `qty unit name` of one line (`200 g walnuts`, `1 1/2 cup milk`, `200g sugar`, `3 eggs`).
fn parse_item_line(line: &str) -> Entry {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let mut i = 0;
    let mut qty = None;
    let mut unit = None;
    // a quantity: one or two tokens (`1 1/2`), or glued to its unit (`200g`)
    if toks.len() >= 2 {
        if let Some(q) = parse_amount(&format!("{} {}", toks[0], toks[1])) {
            qty = Some(q);
            i = 2;
        }
    }
    if qty.is_none() && !toks.is_empty() {
        if let Some(q) = amount_token(toks[0].trim_end_matches(['x', 'X'])) {
            qty = Some(q);
            i = 1;
        } else {
            let t = toks[0];
            let split = t
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '/' || vulgar(c).is_some()))
                .unwrap_or(t.len());
            if split > 0 {
                if let (Some(q), Some(u)) = (amount_token(&t[..split]), unit_word(&t[split..])) {
                    qty = Some(q);
                    unit = Some(u);
                    i = 1;
                }
            }
        }
    }
    if qty.is_some() && unit.is_none() && i < toks.len() {
        let two = toks.get(i + 1).map(|n| format!("{} {n}", toks[i]));
        if let Some(u) = two.as_deref().filter(|t| t.eq_ignore_ascii_case("fl oz")).and_then(unit_word) {
            unit = Some(u);
            i += 2;
        } else if let Some(u) = unit_word(toks[i]).filter(|_| i + 1 < toks.len()) {
            unit = Some(u);
            i += 1;
        }
    }
    if qty.is_some() && toks.get(i).is_some_and(|t| t.eq_ignore_ascii_case("of")) && i + 1 < toks.len() {
        i += 1;
    }
    let mut rest = toks[i.min(toks.len())..].join(" ");
    if qty.is_some() && rest.is_empty() {
        // `3` alone is a name, not a quantity
        rest = line.trim().to_string();
        qty = None;
        unit = None;
    }
    let mut notes = Vec::new();
    while let (Some(a), Some(b)) = (rest.find('('), rest.find(')')) {
        if b < a {
            break;
        }
        notes.push(rest[a + 1..b].trim().to_string());
        rest = format!("{} {}", &rest[..a], &rest[b + 1..]);
    }
    if let Some((name, note)) = rest.split_once(',') {
        notes.insert(0, note.trim().to_string());
        rest = name.to_string();
    }
    let name = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    let note = notes.into_iter().filter(|n| !n.is_empty()).collect::<Vec<_>>().join("; ");
    Entry { name, qty, unit, note: Some(note).filter(|n| !n.is_empty()), ..Default::default() }
}

/// Entries of a plain-text list: one `[qty] [unit] name` per line; bullets,
/// numbering, blank and `#` lines skipped; a URL line is a `url` matcher, on
/// the entry above when indented, else its own entry.
pub fn parse_text(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for raw in text.lines() {
        let indented = raw.starts_with([' ', '\t']);
        let mut t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        for b in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "- ", "* ", "• ", "+ "] {
            if let Some(r) = t.strip_prefix(b) {
                t = r.trim();
                break;
            }
        }
        let digits = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
        if digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")) {
            t = t[digits + 1..].trim();
        }
        if t.is_empty() {
            continue;
        }
        if !t.contains(char::is_whitespace) && matching::normalize_url(t).is_some() {
            match out.last_mut() {
                Some(last) if indented => last.m.url.push(t.to_string()),
                _ => out.push(Entry {
                    name: matching::name_from_url(t),
                    m: Matcher { url: vec![t.to_string()], ..Default::default() },
                    ..Default::default()
                }),
            }
            continue;
        }
        let e = parse_item_line(t);
        if !e.name.is_empty() {
            out.push(e);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(cohorts: &[&str]) -> Matcher {
        Matcher { cohort: cohorts.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn cohort_cycles_are_errors_naming_the_path() {
        let files: HashMap<&str, Matcher> = HashMap::from([
            ("nuts", matcher(&["tree-nuts"])),
            ("tree-nuts", matcher(&["walnuts"])),
            ("walnuts", matcher(&["nuts"])),
            ("seeds", matcher(&[])),
            ("mix", matcher(&["seeds", "seeds"])),
        ]);
        let mut load = |s: &str| files.get(s).cloned().ok_or_else(|| AppError::not_found(s.to_string()));
        let err = resolve(&matcher(&["nuts"]), None, &mut load, &mut Vec::new()).unwrap_err();
        assert_eq!(err.message, "cohort cycle: nuts -> tree-nuts -> walnuts -> nuts");
        let err = resolve(&matcher(&["walnuts"]), None, &mut load, &mut vec!["tree-nuts".into()]).unwrap_err();
        assert_eq!(err.message, "cohort cycle: tree-nuts -> walnuts -> nuts -> tree-nuts");
        // The same cohort twice (a diamond) is not a cycle.
        let r = resolve(&matcher(&["mix"]), None, &mut load, &mut Vec::new()).unwrap();
        assert_eq!(r.cohorts[0].1.cohorts.len(), 2);
        assert!(resolve(&matcher(&["ghost"]), None, &mut load, &mut Vec::new()).is_err());
    }

    #[test]
    fn entries_parse_strings_or_arrays_and_integers() {
        let l: ListFile = toml::from_str(
            r#"
name = "x"
[[entry]]
name = "walnuts"
qty = 200
unit = "grams"
any = "pecans"
gtin = ["0049000028911"]
location = "Kitchen"
[[entry]]
name = "salt"
qty = 0.5
"#,
        )
        .unwrap();
        assert_eq!(l.kind, "recipe");
        let e = &l.entries[0];
        assert_eq!(
            (e.qty, e.unit(), e.m.any.clone(), e.m.upc.len(), e.m.at.clone()),
            (Some(200.0), "g".into(), vec!["pecans".to_string()], 1, vec!["Kitchen".to_string()])
        );
        assert!(!e.fuzzy(), "an identifier: the name is not a fuzzy matcher by default");
        assert!(l.entries[1].fuzzy());
        assert_eq!(l.entries[1].want(), 0.5);
    }

    #[test]
    fn set_key_keeps_order_and_comments() {
        let mut doc: DocumentMut =
            "name = \"x\" # the name\n\n[[entry]]\n# walnuts\nname = \"walnuts\"\nexclude = \"oil\"\n".parse().unwrap();
        let t = entries_mut(&mut doc).get_mut(0).unwrap();
        set_key(t, "qty", Some(value(200)));
        set_key(t, "cohort", Some(strings(&["nuts".into()])));
        set_key(t, "exclude", Some(strings(&["oil".into(), "butter".into()])));
        let s = doc.to_string();
        assert!(s.contains("# the name") && s.contains("# walnuts"), "{s}");
        let keys: Vec<&str> =
            doc["entry"].as_array_of_tables().unwrap().get(0).unwrap().iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["name", "qty", "cohort", "exclude"], "{s}");
        assert!(s.contains("exclude = [\"oil\", \"butter\"]"), "{s}");
    }

    #[test]
    fn amounts_with_fractions() {
        assert_eq!(parse_amount("2"), Some(2.0));
        assert_eq!(parse_amount("1/2"), Some(0.5));
        assert_eq!(parse_amount("1 1/2"), Some(1.5));
        assert_eq!(parse_amount("½"), Some(0.5));
        assert_eq!(parse_amount("1½"), Some(1.5));
        assert_eq!(parse_amount("0,5"), Some(0.5));
        assert_eq!(parse_amount("x"), None);
        assert_eq!(parse_amount("1/0"), None);
    }

    #[test]
    fn text_lines_become_entries() {
        let text = "# Nutty pudding\n\n- 200 g walnuts, chopped\n* 1 1/2 cups milk\n1. ½ tsp salt\n- [ ] 3 eggs (large)\n200g sugar\nvanilla extract\n  https://www.amazon.com/Vanilla/dp/B000VANILA/ref=x\nhttps://www.amazon.com/Diamond-Pecans/dp/B0PECANS01\n2 fl oz cream\n2 cans of coconut milk\n";
        let e = parse_text(text);
        type Row = (String, Option<f64>, Option<String>, Option<String>, Vec<String>);
        let got: Vec<Row> =
            e.iter().map(|e| (e.name.clone(), e.qty, e.unit.clone(), e.note.clone(), e.m.url.clone())).collect();
        let s = |x: &str| Some(x.to_string());
        assert_eq!(
            got,
            [
                ("walnuts".into(), Some(200.0), s("g"), s("chopped"), vec![]),
                ("milk".into(), Some(1.5), s("cup"), None, vec![]),
                ("salt".into(), Some(0.5), s("tsp"), None, vec![]),
                ("eggs".into(), Some(3.0), None, s("large"), vec![]),
                ("sugar".into(), Some(200.0), s("g"), None, vec![]),
                (
                    "vanilla extract".into(),
                    None,
                    None,
                    None,
                    vec!["https://www.amazon.com/Vanilla/dp/B000VANILA/ref=x".to_string()]
                ),
                (
                    "Diamond Pecans".into(),
                    None,
                    None,
                    None,
                    vec!["https://www.amazon.com/Diamond-Pecans/dp/B0PECANS01".to_string()]
                ),
                ("cream".into(), Some(2.0), s("fl oz"), None, vec![]),
                ("coconut milk".into(), Some(2.0), s("can"), None, vec![]),
            ]
        );
        assert_eq!(parse_text("7\n")[0].name, "7");
    }

    #[test]
    fn count_units_compare_as_counts() {
        // `1 can albacore tuna` against a single can counted in `ea`
        assert_eq!(in_unit(1.0, "ea", "Albacore Tuna in Water, 5 oz", "can"), (Some(1.0), "unit"));
        assert_eq!(in_unit(3.0, "jar", "Cocoa powder", "ea"), (Some(3.0), "unit"));
        assert_eq!(in_unit(1.0, "jar", "Cocoa powder", "tbsp"), (None, "unit mismatch"));
    }

    #[test]
    fn pack_counts_multiply_inner_units() {
        let tuna = "StarKist Solid White Albacore Tuna in Water, 5 oz Can, 12-Pack";
        assert_eq!(in_unit(1.0, "ea", tuna, "can"), (Some(12.0), "pack count from name"));
        assert_eq!(in_unit(2.0, "pack", tuna, "cans"), (Some(24.0), "pack count from name"));
        // the outer unit is not multiplied
        assert_eq!(in_unit(1.0, "ea", tuna, "ea"), (Some(1.0), "unit"));
        assert_eq!(in_unit(1.0, "ea", tuna, "pack"), (Some(1.0), "unit"));
        // nor is an item already counted in the inner unit
        assert_eq!(in_unit(12.0, "can", tuna, "can"), (Some(12.0), "unit"));
        // mass still goes by the size the name states
        assert_eq!(in_unit(1.0, "ea", tuna, "oz"), (Some(5.0), "size from name"));
        let fish_oil = "Nature Made Fish Oil 1200 mg Softgels, 90 Count";
        assert_eq!(in_unit(1.0, "ea", fish_oil, "capsule"), (Some(90.0), "pack count from name"));
        assert_eq!(in_unit(1.0, "ea", "Vitamin D3 capsules", "capsule"), (Some(1.0), "unit"));
    }
}
