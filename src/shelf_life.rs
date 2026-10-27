//! Shelf-life types and computed expiry (docs/SPEC.md, "Shelf life and on
//! hand").
//!
//! A type (`banana`, `milk`, `raw-poultry`...) says how long food keeps in
//! the pantry, the fridge and the freezer, and after opening. The built-in
//! types are `data/shelf_life.toml` (compiled in); `[shelf_life.<type>]`
//! tables in config.toml override their values or add types, and a user
//! preset file with the old inline `[shelf_life]` table adds a type named
//! after the preset.
//!
//! Expiry is computed, never stored: from the lot's acquired (or frozen, or
//! opened) date plus the days for where the lot is kept now. A printed
//! best-by on the lot wins. Items get their type from their `Shelf Life`
//! custom field, else by name (head-noun matching, consumable food only),
//! else from their preset.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::matching::{self, Segment};
use crate::presets::{Catalog, Life};
use crate::store::{Item, Lot, Tree};
use crate::util::date_after;

/// The built-in types.
pub const DATA: &str = include_str!("../data/shelf_life.toml");
/// Custom field holding an item's type (or `none`).
pub const FIELD: &str = "Shelf Life";
/// Groups of types (display only).
pub const GROUPS: &[&str] =
    &["fruit", "vegetable", "herb", "dairy", "egg", "meat", "seafood", "bakery", "prepared", "condiment", "other"];
/// Name matches at or above this score classify an item.
pub const CLASSIFY_THRESHOLD: f64 = 0.85;

/// Where a lot is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    Pantry,
    Fridge,
    Freezer,
}

impl Storage {
    pub const ALL: [Storage; 3] = [Storage::Pantry, Storage::Fridge, Storage::Freezer];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fridge => "fridge",
            Self::Freezer => "freezer",
            Self::Pantry => "pantry",
        }
    }

    /// The storage a location kind or name says (`Chest Freezer`, `Fridge`,
    /// `Pantry`, or kind `freezer`).
    pub fn from_name(name: &str) -> Option<Self> {
        let s = name.to_lowercase();
        if s.contains("freezer") || s.contains("deep freeze") {
            Some(Self::Freezer)
        } else if s.contains("fridge") || s.contains("refrigerator") {
            Some(Self::Fridge)
        } else if s.contains("pantry") || s.contains("cupboard") || s.contains("larder") {
            Some(Self::Pantry)
        } else {
            None
        }
    }

    /// The storage named by the nearest segment of a location path
    /// (`Kitchen / Fridge / Freezer drawer` is a freezer).
    pub fn from_path(path: &str) -> Option<Self> {
        crate::store::split_path(path).iter().rev().find_map(|seg| Self::from_name(seg))
    }

    /// The storage of a location: the nearest location (itself, then its
    /// parents) whose kind or name says so.
    pub fn of_location(tree: &Tree, loc: Option<i64>) -> Option<Self> {
        let loc = loc?;
        tree.lineage(loc).iter().rev().find_map(|l| Self::from_name(&l.kind).or_else(|| Self::from_name(&l.name)))
    }
}

/// A type as written in the data file or a config table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeSpec {
    pub group: Option<String>,
    pub pantry: Option<String>,
    pub fridge: Option<String>,
    pub freezer: Option<String>,
    pub opened: Option<String>,
    #[serde(default)]
    pub synonyms: Vec<String>,
}

/// A loaded type.
#[derive(Debug, Clone)]
pub struct ShelfType {
    pub name: String,
    pub group: String,
    pub pantry: Option<Life>,
    pub fridge: Option<Life>,
    pub freezer: Option<Life>,
    pub opened: Option<Life>,
    pub synonyms: Vec<String>,
    /// `built-in`, `config`, `built-in, overridden in config`, or `preset <file>`.
    pub origin: String,
    phrases: Vec<Vec<String>>,
}

/// A duration value: `none` (or empty) is absent.
fn life_of(key: &str, v: Option<&str>) -> Result<Option<Life>> {
    match v.map(str::trim) {
        None | Some("" | "none") => Ok(None),
        Some(s) => Life::parse(s).map(Some).map_err(|e| e.context(key)),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_'))
}

impl ShelfType {
    fn build(name: &str, spec: &TypeSpec, origin: &str) -> Result<Self> {
        let ctx = |e: AppError| e.context(format!("shelf-life type {name}"));
        if !valid_name(name) {
            return Err(AppError::config(format!(
                "bad shelf-life type name '{name}' (lowercase letters, digits, '-', '.', '_')"
            )));
        }
        let group = spec.group.clone().unwrap_or_else(|| "other".into());
        if !GROUPS.contains(&group.as_str()) {
            return Err(ctx(AppError::config(format!("group '{group}' is not one of {}", GROUPS.join(", ")))));
        }
        let mut t = Self {
            name: name.to_string(),
            group,
            pantry: life_of("pantry", spec.pantry.as_deref()).map_err(ctx)?,
            fridge: life_of("fridge", spec.fridge.as_deref()).map_err(ctx)?,
            freezer: life_of("freezer", spec.freezer.as_deref()).map_err(ctx)?,
            opened: life_of("opened", spec.opened.as_deref()).map_err(ctx)?,
            synonyms: spec.synonyms.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            origin: origin.to_string(),
            phrases: Vec::new(),
        };
        if [t.pantry, t.fridge, t.freezer, t.opened].iter().all(Option::is_none) {
            return Err(ctx(AppError::config("needs at least one of pantry, fridge, freezer, opened")));
        }
        t.index();
        Ok(t)
    }

    fn index(&mut self) {
        let own = self.name.replace(['-', '_', '.'], " ");
        self.phrases = std::iter::once(own.as_str())
            .chain(self.synonyms.iter().map(String::as_str))
            .map(matching::name_tokens)
            .filter(|p| !p.is_empty())
            .collect();
    }

    /// Apply a config table on top (given values replace, `none` removes,
    /// synonyms are added).
    fn overlay(&mut self, spec: &TypeSpec) -> Result<()> {
        let ctx = |e: AppError| e.context(format!("shelf-life type {}", self.name));
        for (key, slot, v) in [
            ("pantry", &mut self.pantry, &spec.pantry),
            ("fridge", &mut self.fridge, &spec.fridge),
            ("freezer", &mut self.freezer, &spec.freezer),
            ("opened", &mut self.opened, &spec.opened),
        ] {
            if v.is_some() {
                *slot = life_of(key, v.as_deref()).map_err(ctx)?;
            }
        }
        if let Some(g) = &spec.group {
            if !GROUPS.contains(&g.as_str()) {
                return Err(ctx(AppError::config(format!("group '{g}' is not one of {}", GROUPS.join(", ")))));
            }
            self.group.clone_from(g);
        }
        for s in &spec.synonyms {
            if !self.synonyms.iter().any(|x| x.eq_ignore_ascii_case(s.trim())) {
                self.synonyms.push(s.trim().to_string());
            }
        }
        if [self.pantry, self.fridge, self.freezer, self.opened].iter().all(Option::is_none) {
            return Err(ctx(AppError::config("needs at least one of pantry, fridge, freezer, opened")));
        }
        self.origin = "built-in, overridden in config".into();
        self.index();
        Ok(())
    }

    pub fn life(&self, s: Storage) -> Option<Life> {
        match s {
            Storage::Pantry => self.pantry,
            Storage::Fridge => self.fridge,
            Storage::Freezer => self.freezer,
        }
    }

    /// `fridge 7 days, freezer 3 months, opened 7 days`.
    pub fn summary(&self) -> String {
        Storage::ALL
            .iter()
            .filter_map(|s| self.life(*s).map(|l| format!("{} {}", s.as_str(), l.display())))
            .chain(self.opened.map(|l| format!("opened {}", l.display())))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The best head-noun score of the type's name and synonyms against a
    /// product name's segments.
    pub fn name_score(&self, segs: &[Segment]) -> f64 {
        self.phrases.iter().map(|p| matching::head_match(p, segs).score).fold(0.0, f64::max)
    }
}

/// When a lot expires and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Expiry {
    pub date: NaiveDate,
    /// `best_by` (the printed date) or `shelf_life` (computed).
    pub basis: &'static str,
    /// e.g. `raw-poultry: 1 day in the fridge from purchase on 2026-01-01`.
    pub detail: String,
}

fn day(s: Option<&str>) -> Option<NaiveDate> {
    s.and_then(|d| crate::util::date_of(d).ok())
}

/// The expiry of a lot of type `t` kept in `storage` (`None` for a
/// non-perishable, or a type with no value for the lot's state). A printed
/// best-by wins.
pub fn expiry(t: Option<&ShelfType>, lot: &Lot, storage: Storage) -> Option<Expiry> {
    let t = t?;
    if let Some(d) = day(lot.best_by.as_deref()) {
        return Some(Expiry { date: d, basis: "best_by", detail: format!("printed best-by {d}") });
    }
    let acquired = day(lot.acquired_at.as_deref()).or_else(|| day(Some(&lot.created_at)))?;
    // A storage without a value (not recommended there) falls back to the
    // shortest of pantry and fridge, else the freezer.
    let fallback = || {
        [Storage::Pantry, Storage::Fridge]
            .into_iter()
            .filter_map(|s| t.life(s).map(|l| (s, l)))
            .min_by_key(|(_, l)| date_after(acquired, l.span()))
            .or_else(|| t.freezer.map(|l| (Storage::Freezer, l)))
    };
    let kept = t.life(storage).map(|l| (storage, l, false)).or_else(|| fallback().map(|(s, l)| (s, l, true)));
    let mut out = kept.map(|(s, l, fell_back)| {
        let (base, from) = match s {
            Storage::Freezer => match day(lot.frozen_at.as_deref()) {
                Some(f) => (f, "freezing"),
                None => (acquired, "purchase"),
            },
            _ => (acquired, "purchase"),
        };
        let note = if fell_back {
            format!(" (not recommended in the {}; using the {s} figure)", storage.as_str(), s = s.as_str())
        } else {
            String::new()
        };
        Expiry {
            date: date_after(base, l.span()),
            basis: "shelf_life",
            detail: format!("{}: {} in the {} from {from} on {base}{note}", t.name, l.display(), s.as_str()),
        }
    });
    if let (Some(o), Some(opened)) = (t.opened, day(lot.opened_at.as_deref())) {
        let after = date_after(opened, o.span());
        if out.as_ref().is_none_or(|e| after < e.date) {
            out = Some(Expiry {
                date: after,
                basis: "shelf_life",
                detail: format!("{}: {} after opening on {opened}", t.name, o.display()),
            });
        }
    }
    out
}

/// How an item got its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Basis {
    /// Its `Shelf Life` field.
    Field,
    /// Its name.
    Name,
    /// Its preset's default type.
    Preset,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Name => "name",
            Self::Preset => "preset",
        }
    }
}

/// An item's type, how it was found and (by name) how well it matched.
#[derive(Debug, Clone, Copy)]
pub struct Classified<'a> {
    pub ty: &'a ShelfType,
    pub basis: Basis,
    pub score: f64,
}

/// What classification looks at.
pub struct ItemFacts<'a> {
    pub item: &'a Item,
    pub fields: &'a BTreeMap<String, String>,
    pub labels: &'a [String],
    pub has_lots: bool,
    /// Kept in a fridge, freezer or pantry: food.
    pub food_place: bool,
}

/// Every type: built-in, overridden and added in config, and preset inline types.
#[derive(Debug, Clone, Default)]
pub struct Types {
    pub types: BTreeMap<String, ShelfType>,
}

impl Types {
    pub fn builtin() -> Self {
        let specs: BTreeMap<String, TypeSpec> =
            toml::from_str(DATA).unwrap_or_else(|e| panic!("data/shelf_life.toml: {e}"));
        let types = specs
            .iter()
            .map(|(n, s)| {
                let t = ShelfType::build(n, s, "built-in").unwrap_or_else(|e| panic!("data/shelf_life.toml: {e}"));
                (n.clone(), t)
            })
            .collect();
        Self { types }
    }

    /// Built-in types with the config file's `[shelf_life.*]` tables applied
    /// and the inline types of `catalog`'s presets added.
    pub fn load(config_path: &Path, catalog: &Catalog) -> Result<Self> {
        let text = match std::fs::read_to_string(config_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(AppError::config(format!("reading config {}: {e}", config_path.display()))),
        };
        let mut t = Self::from_config(&text).map_err(|e| e.context(config_path.display()))?;
        for p in catalog.presets.values() {
            if let Some(spec) = p.inline_shelf_life() {
                if !t.types.contains_key(&p.name) {
                    t.types.insert(p.name.clone(), ShelfType::build(&p.name, &spec, &format!("preset {}", p.origin))?);
                }
            }
        }
        Ok(t)
    }

    /// Built-in types with the `[shelf_life.*]` tables of a config file's text.
    pub fn from_config(text: &str) -> Result<Self> {
        let mut t = Self::builtin();
        let (head, _) = crate::enc_config::split(text);
        let table: toml::Table = head.parse().map_err(|e| AppError::config(format!("TOML: {e}")))?;
        let Some(sl) = table.get("shelf_life") else { return Ok(t) };
        let specs: BTreeMap<String, TypeSpec> =
            sl.clone().try_into().map_err(|e| AppError::config(format!("[shelf_life]: {e}")))?;
        for (name, spec) in specs {
            match t.types.get_mut(&name) {
                Some(existing) => existing.overlay(&spec)?,
                None => {
                    t.types.insert(name.clone(), ShelfType::build(&name, &spec, "config")?);
                }
            }
        }
        Ok(t)
    }

    pub fn get(&self, name: &str) -> Option<&ShelfType> {
        self.types.get(name.trim())
    }

    pub fn require(&self, name: &str) -> Result<&ShelfType> {
        self.get(name)
            .ok_or_else(|| AppError::not_found(format!("no shelf-life type '{name}' (see `wherebox shelf-life ls`)")))
    }

    /// The best type for a product name (score at or above the threshold),
    /// ties to the more specific phrase, then by name. Non-food names never
    /// classify.
    pub fn by_name(&self, name: &str) -> Option<(&ShelfType, f64)> {
        if crate::food::non_food_name(name) {
            return None;
        }
        let segs = matching::segments(name);
        self.types
            .values()
            .map(|t| (t, t.name_score(&segs)))
            .filter(|(_, s)| *s >= CLASSIFY_THRESHOLD)
            .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.name.cmp(&a.0.name)))
    }

    /// An item's type: its `Shelf Life` field (`none`: never perishable),
    /// else for consumable food (or anything kept in a fridge, freezer or
    /// pantry) its name, else its preset's default type.
    pub fn classify<'a>(&'a self, f: &ItemFacts, catalog: &Catalog) -> Option<Classified<'a>> {
        if let Some(v) = crate::consumable::field(Some(f.fields), FIELD).map(str::trim).filter(|v| !v.is_empty()) {
            if v.eq_ignore_ascii_case("none") {
                return None;
            }
            return self.get(&v.to_ascii_lowercase()).map(|ty| Classified { ty, basis: Basis::Field, score: 1.0 });
        }
        let kind = crate::consumable::field(Some(f.fields), crate::consumable::KIND_FIELD);
        if kind.is_some_and(|k| k.trim().eq_ignore_ascii_case("durable")) {
            return None;
        }
        let preset = f.item.kind_preset.as_deref().and_then(|p| catalog.get(p));
        let consumable = f.has_lots
            || f.food_place
            || crate::consumable::is_consumable(kind, f.item.kind_preset.as_deref(), f.labels, catalog);
        if !consumable || preset.is_some_and(|p| p.category != "food") {
            return None;
        }
        if !crate::food::non_food_label(f.labels) {
            if let Some((ty, score)) = self.by_name(&f.item.name) {
                return Some(Classified { ty, basis: Basis::Name, score });
            }
        }
        preset.and_then(|p| p.default_shelf_life()).and_then(|n| self.get(&n)).map(|ty| Classified {
            ty,
            basis: Basis::Preset,
            score: 1.0,
        })
    }
}

/// Write (or with `None`, remove) `[shelf_life.<name>]` keys in the config
/// file, keeping the rest of it as it is.
pub fn write_config(path: &Path, name: &str, set: &[(&str, Option<toml_edit::Item>)], drop: bool) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let (head, block) = crate::enc_config::split(&text);
    let mut doc: toml_edit::DocumentMut =
        head.parse().map_err(|e| AppError::config(format!("{}: {e}", path.display())))?;
    if drop {
        if let Some(t) = doc.get_mut("shelf_life").and_then(toml_edit::Item::as_table_like_mut) {
            t.remove(name);
        }
    } else {
        let root = doc.entry("shelf_life").or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        });
        let root = root.as_table_mut().ok_or_else(|| AppError::config("config: shelf_life is not a table"))?;
        let t = root.entry(name).or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let t =
            t.as_table_mut().ok_or_else(|| AppError::config(format!("config: shelf_life.{name} is not a table")))?;
        for (k, v) in set {
            match v {
                Some(v) => {
                    t.insert(k, v.clone());
                }
                None => {
                    t.remove(k);
                }
            }
        }
    }
    let mut out = doc.to_string();
    if !block.is_empty() {
        out = format!("{}\n\n{block}", out.trim_end());
    }
    // Validate before writing: a bad value never reaches the file.
    Types::from_config(&out)?;
    crate::config::ensure_file(path)?;
    std::fs::write(path, out).map_err(|e| AppError::io(format!("writing {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lot(acquired: &str) -> Lot {
        Lot { acquired_at: Some(acquired.into()), status: "sealed".into(), ..Default::default() }
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn builtin_data_loads_and_validates() {
        let t = Types::builtin();
        assert!(t.types.len() >= 60, "{} types", t.types.len());
        let mut phrases: BTreeMap<String, String> = BTreeMap::new();
        let mut dups = Vec::new();
        for (n, ty) in &t.types {
            assert_eq!(n, &ty.name);
            assert!(ty.summary().contains(' '), "{n}: no storage");
            for p in &ty.phrases {
                if let Some(other) = phrases.insert(p.join(" "), n.clone()) {
                    dups.push(format!("'{}' ({other}, {n})", p.join(" ")));
                }
            }
        }
        assert!(dups.is_empty(), "names and synonyms must be unique: {}", dups.join(", "));
        for want in [
            "banana",
            "apple",
            "berries",
            "leafy-greens",
            "lettuce",
            "spinach",
            "tomato",
            "onion",
            "potato",
            "avocado",
            "citrus",
            "milk",
            "yogurt",
            "cheese-hard",
            "cheese-soft",
            "eggs",
            "butter",
            "raw-poultry",
            "raw-meat",
            "ground-meat",
            "raw-fish",
            "cooked-leftovers",
            "bread",
            "tortillas",
            "deli-meat",
            "fresh-herbs",
            "condiment-opened",
            "canned-food-opened",
        ] {
            assert!(t.get(want).is_some(), "missing {want}");
        }
        // every group has its sources cited in the data file
        assert!(DATA.matches("[FK]").count() + DATA.matches("[CSC]").count() >= 10);
    }

    #[test]
    fn config_overrides_and_adds_types() {
        let t = Types::from_config(
            "format = \"json\"\n[shelf_life.milk]\nfridge = \"5d\"\nsynonyms = [\"moo juice\"]\n\
             [shelf_life.kombucha]\nfridge = \"30d\"\ngroup = \"prepared\"\n[shelf_life.banana]\nfridge = \"none\"\n",
        )
        .unwrap();
        let milk = t.get("milk").unwrap();
        assert_eq!(milk.fridge.unwrap().display(), "5 days");
        assert_eq!(milk.freezer.unwrap().display(), "3 months", "untouched values stay");
        assert!(milk.origin.contains("overridden"));
        assert_eq!(t.by_name("Moo Juice, 1 gal").unwrap().0.name, "milk");
        assert_eq!(t.get("kombucha").unwrap().origin, "config");
        assert_eq!(t.by_name("GT's Synergy Kombucha, Gingerade").unwrap().0.name, "kombucha");
        assert!(t.get("banana").unwrap().fridge.is_none());
        assert!(Types::from_config("[shelf_life.empty]\ngroup = \"fruit\"\n").is_err(), "no storage");
        assert!(Types::from_config("[shelf_life.x]\nfridge = \"soon\"\n").is_err());
        assert!(Types::from_config("[shelf_life.x]\nfridge = \"1d\"\nnope = 1\n").is_err());
    }

    #[test]
    fn names_classify_by_head_noun() {
        let t = Types::builtin();
        let c = |n: &str| t.by_name(n).map(|(ty, _)| ty.name.clone());
        assert_eq!(c("Organic Bananas, 3 lb Bag").as_deref(), Some("banana"));
        assert_eq!(c("Frozen Organic Wild Blueberries, 3 lb").as_deref(), Some("berries"));
        assert_eq!(c("Ground Beef 80/20, 1 lb").as_deref(), Some("ground-meat"));
        assert_eq!(c("Boneless Skinless Chicken Breasts").as_deref(), Some("raw-poultry"));
        assert_eq!(c("Rotisserie Chicken").as_deref(), Some("cooked-leftovers"));
        assert_eq!(c("Cherry Tomatoes, 1 pint").as_deref(), Some("tomato"));
        assert_eq!(c("Fresh Garlic, 1 Head").as_deref(), Some("garlic"));
        assert_eq!(c("365 by Whole Foods Market, Organic Fuji Apples, 3 lb").as_deref(), Some("apple"));
        // compound products and non-food are not their modifier
        assert_eq!(c("Chili Garlic Sauce, 8 oz"), None);
        assert_eq!(c("Berry Trail Mix"), None);
        assert_eq!(c("Lost Cherry Eau de Parfum Spray"), None);
        assert_eq!(c("Ginger Root Capsules, 100 Count"), None);
        assert_eq!(c("Chicken Broth, 32 oz"), None);
        assert_eq!(c("Albacore Tuna in Water, 5 oz Can, 12-Pack"), None);
        assert_eq!(c("Paper Towels 6-pack"), None);
    }

    #[test]
    fn computed_expiry_by_storage() {
        let t = Types::builtin();
        let chicken = t.get("raw-poultry");
        let l = lot("2026-01-01");
        let e = expiry(chicken, &l, Storage::Fridge).unwrap();
        assert_eq!((e.date, e.basis), (d("2026-01-02"), "shelf_life"));
        assert!(e.detail.contains("1 day in the fridge from purchase on 2026-01-01"), "{}", e.detail);
        // freezer: from freezing, else purchase
        assert_eq!(expiry(chicken, &l, Storage::Freezer).unwrap().date, d("2026-10-01"));
        let frozen = Lot { frozen_at: Some("2026-03-01".into()), ..lot("2026-01-01") };
        assert_eq!(expiry(chicken, &frozen, Storage::Freezer).unwrap().date, d("2026-12-01"));
        // pantry: not recommended; the fridge figure
        let e = expiry(chicken, &l, Storage::Pantry).unwrap();
        assert_eq!(e.date, d("2026-01-02"));
        assert!(e.detail.contains("not recommended in the pantry"), "{}", e.detail);
        // pantry figure where there is one
        assert_eq!(expiry(t.get("banana"), &l, Storage::Pantry).unwrap().date, d("2026-01-06"));
        // opened: the earlier of opened + days and the storage date
        let milk = t.get("milk");
        let opened = Lot { opened_at: Some("2026-01-03".into()), ..lot("2026-01-01") };
        assert_eq!(expiry(milk, &opened, Storage::Fridge).unwrap().date, d("2026-01-08"));
        let late = Lot { opened_at: Some("2026-01-06".into()), ..lot("2026-01-01") };
        assert_eq!(expiry(milk, &late, Storage::Fridge).unwrap().date, d("2026-01-08"), "unopened date is earlier");
        // only an after-opening value: unopened lots never expire
        let canned = t.get("canned-food-opened");
        assert_eq!(expiry(canned, &l, Storage::Pantry), None);
        assert_eq!(expiry(canned, &opened, Storage::Pantry).unwrap().date, d("2026-01-06"));
        // the printed best-by wins; no type, no expiry
        let printed = Lot { best_by: Some("2026-02-01".into()), ..lot("2026-01-01") };
        assert_eq!(expiry(chicken, &printed, Storage::Fridge).unwrap().basis, "best_by");
        assert_eq!(expiry(None, &printed, Storage::Fridge), None);
    }

    #[test]
    fn storage_from_location_names() {
        assert_eq!(Storage::from_path("Rental apartment / Kitchen / Fridge"), Some(Storage::Fridge));
        assert_eq!(Storage::from_path("Rental apartment / Kitchen / Fridge / Freezer drawer"), Some(Storage::Freezer));
        assert_eq!(Storage::from_path("Garage / Chest Freezer / Bin 2"), Some(Storage::Freezer));
        assert_eq!(Storage::from_path("Kitchen / Pantry / Shelf 1"), Some(Storage::Pantry));
        assert_eq!(Storage::from_path("Kitchen"), None);
    }
}
