//! Shelf-life types: how long a kind of perishable keeps in the pantry, the
//! fridge and the freezer, and once opened (docs/SPEC.md, "Shelf life").
//!
//! The built-in types live in `shelf-life/types.toml` (compiled in).
//! `[shelf_life.<type>]` tables in `config.toml` override any field of a
//! built-in type or add new types. Expiry is computed from a lot's dates and
//! where it is kept, never stored; only a printed best-by date on the lot
//! overrides it. Items without a type (durables, non-perishable consumables)
//! never expire.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::presets::{Catalog, Life};
use crate::store::{Item, Lot, Tree};
use crate::util::{date_after, date_of};

/// The built-in types.
pub const BUILTIN: &str = include_str!("../shelf-life/types.toml");

pub const GROUPS: &[&str] =
    &["fruit", "vegetable", "dairy", "eggs", "meat", "poultry", "seafood", "bakery", "prepared", "opened", "other"];

/// Where the figures of each built-in group come from.
const GROUP_SOURCES: &[(&str, &str)] = &[
    ("fruit", "USDA FSIS FoodKeeper (Produce: fresh fruits)"),
    ("vegetable", "USDA FSIS FoodKeeper (Produce: fresh vegetables)"),
    ("dairy", "USDA FSIS FoodKeeper (Dairy products); FDA / FoodSafety.gov Cold Food Storage Chart"),
    ("eggs", "FDA / FoodSafety.gov Cold Food Storage Chart (Eggs); USDA FSIS FoodKeeper"),
    ("meat", "FDA / FoodSafety.gov Cold Food Storage Chart (meat, ground meat, bacon, sausage, ham, hot dogs, luncheon meat); USDA FSIS FoodKeeper"),
    ("poultry", "FDA / FoodSafety.gov Cold Food Storage Chart (Fresh poultry); USDA FSIS FoodKeeper"),
    ("seafood", "FDA / FoodSafety.gov Cold Food Storage Chart (fish, shellfish, smoked fish); USDA FSIS FoodKeeper"),
    ("bakery", "USDA FSIS FoodKeeper (Baked goods)"),
    ("prepared", "FDA / FoodSafety.gov Cold Food Storage Chart (leftovers, soups, salads, pizza, frozen dinners); USDA FSIS FoodKeeper"),
    ("opened", "FDA / FoodSafety.gov Cold Food Storage Chart (condiments); USDA FSIS FoodKeeper (opened canned goods, broth, sauces)"),
];

pub const FOODKEEPER_URL: &str = "https://www.fsis.usda.gov/shared/data/EN/foodkeeper.json";
pub const COLD_CHART_URL: &str = "https://www.foodsafety.gov/food-safety-charts/cold-food-storage-charts";

/// Words in a name that say the product keeps on the shelf: such items are
/// not classified by name.
pub const SHELF_STABLE: &[&str] = &[
    "canned",
    "can",
    "dried",
    "dry",
    "powder",
    "chips",
    "jerky",
    "freeze dried",
    "shelf stable",
    "uht",
    "jar",
    "jarred",
    "pickled",
];

/// Minimum name score to classify an item by name.
pub const CLASSIFY_MIN: f64 = 0.9;

/// The `shelf_life` item value that says "never perishable".
pub const NONE: &str = "none";

/// Where a lot is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    Pantry,
    Fridge,
    Freezer,
}

impl Storage {
    pub const ALL: [Self; 3] = [Self::Pantry, Self::Fridge, Self::Freezer];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fridge => "fridge",
            Self::Freezer => "freezer",
            Self::Pantry => "pantry",
        }
    }

    /// A location kind that names a storage.
    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_lowercase().as_str() {
            "fridge" => Some(Self::Fridge),
            "freezer" => Some(Self::Freezer),
            "pantry" => Some(Self::Pantry),
            _ => None,
        }
    }

    /// The storage a location name says (`Chest Freezer`, `Refrigerator`, `Larder`).
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

    /// The storage of a location: the nearest location (itself, then up the
    /// tree) whose kind or name says fridge, freezer or pantry; else pantry.
    pub fn of_location(tree: &Tree, loc: Option<i64>) -> Self {
        loc.map(|id| tree.lineage(id))
            .unwrap_or_default()
            .iter()
            .rev()
            .find_map(|l| Self::from_kind(&l.kind).or_else(|| Self::from_name(&l.name)))
            .unwrap_or(Self::Pantry)
    }
}

/// A type as written in the data file or a config table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    group: Option<String>,
    description: Option<String>,
    pantry: Option<String>,
    fridge: Option<String>,
    freezer: Option<String>,
    opened: Option<String>,
    synonyms: Option<Vec<String>>,
    not: Option<Vec<String>>,
    source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShelfType {
    pub name: String,
    pub group: String,
    pub description: String,
    pub pantry: Option<String>,
    pub fridge: Option<String>,
    pub freezer: Option<String>,
    pub opened: Option<String>,
    pub synonyms: Vec<String>,
    pub not: Vec<String>,
    pub source: String,
    /// `built-in`, `config` or `built-in+config`.
    pub origin: String,
}

fn life(v: &Option<String>) -> Option<Life> {
    v.as_deref().and_then(|s| Life::parse(s).ok())
}

impl ShelfType {
    /// How long it keeps stored there, if storing it there is recommended.
    pub fn figure(&self, s: Storage) -> Option<Life> {
        life(match s {
            Storage::Pantry => &self.pantry,
            Storage::Fridge => &self.fridge,
            Storage::Freezer => &self.freezer,
        })
    }

    pub fn opened_life(&self) -> Option<Life> {
        life(&self.opened)
    }

    /// The figure that applies in `storage`: its own, else the shorter of
    /// pantry and fridge (conservative), else the freezer's.
    pub fn figure_for(&self, storage: Storage) -> Option<(Life, Storage)> {
        if let Some(l) = self.figure(storage) {
            return Some((l, storage));
        }
        let days = |l: Life| crate::util::parse_duration_days(&format!("{}{}", l.min, l.unit)).unwrap_or(f64::MAX);
        [Storage::Pantry, Storage::Fridge]
            .into_iter()
            .filter_map(|s| self.figure(s).map(|l| (l, s)))
            .min_by(|a, b| days(a.0).total_cmp(&days(b.0)))
            .or_else(|| self.figure(Storage::Freezer).map(|l| (l, Storage::Freezer)))
    }

    /// "pantry 3 weeks, fridge 4 weeks, freezer 8 months".
    pub fn figures_text(&self) -> String {
        let mut parts: Vec<String> = Storage::ALL
            .iter()
            .filter_map(|s| self.figure(*s).map(|l| format!("{} {}", s.as_str(), l.display())))
            .collect();
        if let Some(o) = self.opened_life() {
            parts.push(format!("opened {}", o.display()));
        }
        parts.join(", ")
    }

    /// Names to classify by: the type name (dashes as spaces) and synonyms.
    fn queries(&self) -> Vec<String> {
        std::iter::once(self.name.replace('-', " ")).chain(self.synonyms.iter().cloned()).collect()
    }

    fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(AppError::config(format!("shelf-life type '{}': {m}", self.name)));
        check_name(&self.name)?;
        if !GROUPS.contains(&self.group.as_str()) {
            return bad(format!("group '{}' is not one of {}", self.group, GROUPS.join(", ")));
        }
        for (k, v) in
            [("pantry", &self.pantry), ("fridge", &self.fridge), ("freezer", &self.freezer), ("opened", &self.opened)]
        {
            if let Some(v) = v {
                Life::parse(v).map_err(|e| e.context(format!("shelf-life type '{}' {k}", self.name)))?;
            }
        }
        if [&self.pantry, &self.fridge, &self.freezer, &self.opened].iter().all(|v| v.is_none()) {
            return bad("needs at least one figure (pantry, fridge, freezer or opened)".into());
        }
        Ok(())
    }
}

/// Type names: lower-case letters, digits and dashes.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == NONE
        || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AppError::invalid(format!(
            "bad shelf-life type name '{name}' (lower-case letters, digits and '-'; not 'none')"
        )));
    }
    Ok(())
}

/// A duration flag or config value: `5d`, `2w`, `6m`, `1y` (`none` clears).
pub fn normalize_figure(v: &str) -> Result<Option<String>> {
    let t = v.trim().to_ascii_lowercase();
    if t == NONE || t.is_empty() {
        return Ok(None);
    }
    Life::parse(&t)?;
    Ok(Some(t))
}

/// Every shelf-life type: built-in, with config overrides and additions.
#[derive(Debug, Clone, Default)]
pub struct Types {
    pub types: BTreeMap<String, ShelfType>,
    /// (type name, tokens of one of its names), for classification.
    queries: Vec<(String, Vec<String>)>,
}

fn group_source(group: &str) -> String {
    GROUP_SOURCES.iter().find(|(g, _)| *g == group).map_or_else(|| "config".to_string(), |(_, s)| (*s).to_string())
}

impl Types {
    pub fn builtin() -> Self {
        Self::parse_builtin().unwrap_or_else(|e| panic!("built-in shelf-life types: {e}"))
    }

    fn parse_builtin() -> Result<Self> {
        let raw: BTreeMap<String, Raw> =
            toml::from_str(BUILTIN).map_err(|e| AppError::config(format!("shelf-life/types.toml: {e}")))?;
        let mut types = BTreeMap::new();
        for (name, r) in raw {
            let group = r.group.clone().unwrap_or_default();
            let t = ShelfType {
                description: r.description.unwrap_or_default(),
                pantry: r.pantry,
                fridge: r.fridge,
                freezer: r.freezer,
                opened: r.opened,
                synonyms: r.synonyms.unwrap_or_default(),
                not: r.not.unwrap_or_default(),
                source: r.source.unwrap_or_else(|| group_source(&group)),
                group,
                origin: "built-in".into(),
                name: name.clone(),
            };
            t.validate()?;
            types.insert(name, t);
        }
        Ok(Self::index(types))
    }

    fn index(types: BTreeMap<String, ShelfType>) -> Self {
        let queries = types
            .values()
            .flat_map(|t| t.queries().into_iter().map(|q| (t.name.clone(), crate::matching::name_tokens(&q))))
            .filter(|(_, q)| !q.is_empty())
            .collect();
        Self { types, queries }
    }

    /// Built-in types plus the `[shelf_life]` tables of the config file.
    pub fn load(config_path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(config_path).unwrap_or_default();
        let (head, _) = crate::enc_config::split(&text);
        let table: toml::Table =
            head.parse().map_err(|e| AppError::config(format!("{}: {e}", config_path.display())))?;
        match table.get("shelf_life") {
            None => Ok(Self::builtin()),
            Some(toml::Value::Table(t)) => {
                Self::builtin().with_overrides(t).map_err(|e| e.context(config_path.display()))
            }
            Some(_) => Err(AppError::config("[shelf_life] must be a table of [shelf_life.<type>] tables")),
        }
    }

    /// Apply `[shelf_life.<type>]` tables: fields of a built-in type are
    /// replaced (`none` removes a figure; synonyms and `not` words are
    /// added), unknown names are new types.
    pub fn with_overrides(self, table: &toml::Table) -> Result<Self> {
        let mut types = self.types;
        for (name, v) in table {
            check_name(name).map_err(|e| e.context("[shelf_life]"))?;
            let r: Raw = v.clone().try_into().map_err(|e| AppError::config(format!("[shelf_life.{name}]: {e}")))?;
            let fig = |v: &Option<String>| v.as_deref().map(normalize_figure).transpose();
            let (pantry, fridge, freezer, opened) =
                (fig(&r.pantry)?, fig(&r.fridge)?, fig(&r.freezer)?, fig(&r.opened)?);
            let t = match types.remove(name) {
                Some(mut t) => {
                    let set = |slot: &mut Option<String>, v: Option<Option<String>>| {
                        if let Some(v) = v {
                            *slot = v;
                        }
                    };
                    set(&mut t.pantry, pantry);
                    set(&mut t.fridge, fridge);
                    set(&mut t.freezer, freezer);
                    set(&mut t.opened, opened);
                    if let Some(d) = r.description {
                        t.description = d;
                    }
                    if let Some(g) = r.group {
                        t.group = g;
                    }
                    t.synonyms.extend(r.synonyms.unwrap_or_default());
                    t.not.extend(r.not.unwrap_or_default());
                    if let Some(s) = r.source {
                        t.source = s;
                    }
                    t.origin = "built-in+config".into();
                    t
                }
                None => ShelfType {
                    name: name.clone(),
                    group: r.group.unwrap_or_else(|| "other".into()),
                    description: r.description.unwrap_or_default(),
                    pantry: pantry.flatten(),
                    fridge: fridge.flatten(),
                    freezer: freezer.flatten(),
                    opened: opened.flatten(),
                    synonyms: r.synonyms.unwrap_or_default(),
                    not: r.not.unwrap_or_default(),
                    source: r.source.unwrap_or_else(|| "config".into()),
                    origin: "config".into(),
                },
            };
            t.validate()?;
            types.insert(name.clone(), t);
        }
        Ok(Self::index(types))
    }

    pub fn get(&self, name: &str) -> Option<&ShelfType> {
        self.types.get(name.trim())
    }

    pub fn require(&self, name: &str) -> Result<&ShelfType> {
        self.get(name)
            .ok_or_else(|| AppError::not_found(format!("no shelf-life type '{name}' (see `wherebox shelf-life ls`)")))
    }

    /// The type a product name is, by the matching ladder's name tier: the
    /// best type name or synonym that every word of is in the name, scoring
    /// at least [`CLASSIFY_MIN`]. Names with a shelf-stable marker, or a
    /// word a type rules out, are not classified (as that type).
    pub fn classify_name(&self, name: &str) -> Option<(&ShelfType, f64)> {
        let words = words(name);
        let has = |phrase: &str| {
            let p = words_of(phrase);
            !p.is_empty() && p.iter().all(|w| words.iter().any(|x| same_word(x, w)))
        };
        if SHELF_STABLE.iter().any(|m| has(m)) {
            return None;
        }
        let tokens = crate::matching::name_tokens(name);
        self.queries
            .iter()
            .filter_map(|(t, q)| {
                let s = crate::matching::score_tokens(q, &tokens);
                (s.full && s.score >= CLASSIFY_MIN).then_some((t, q.len(), s.score))
            })
            .filter_map(|(t, n, s)| self.types.get(t).map(|t| (t, n, s)))
            .filter(|(t, _, _)| !t.not.iter().any(|w| has(w)))
            .max_by(|a, b| a.2.total_cmp(&b.2).then(a.1.cmp(&b.1)).then(b.0.name.cmp(&a.0.name)))
            .map(|(t, _, s)| (t, s))
    }
}

/// Lower-case alphanumeric words.
fn words_of(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn words(s: &str) -> Vec<String> {
    words_of(s)
}

/// `cans` is `can`, `jars` is `jar`.
fn same_word(a: &str, b: &str) -> bool {
    a == b || a.strip_suffix('s') == Some(b) || b.strip_suffix('s') == Some(a)
}

/// An item's shelf-life type and how it was found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Class {
    pub name: String,
    /// `explicit`, `name` or `preset`.
    pub basis: &'static str,
}

/// Is the item a durable (`Kind = durable`) or of a non-food preset?
fn never_perishable(fields: Option<&BTreeMap<String, String>>, item: &Item, catalog: &Catalog) -> bool {
    let durable = crate::consumable::field(fields, crate::consumable::KIND_FIELD)
        .is_some_and(|k| k.trim().eq_ignore_ascii_case("durable"));
    let non_food = item.kind_preset.as_deref().and_then(|p| catalog.get(p)).is_some_and(|p| p.category != "food");
    durable || non_food
}

/// An item's shelf-life type: its `shelf_life` field (`none` = never),
/// else nothing for durables and non-food presets, else (for consumables
/// and items that track lots) its name, else its preset's type.
pub fn classify(
    item: &Item,
    fields: Option<&BTreeMap<String, String>>,
    labels: &[String],
    has_lots: bool,
    catalog: &Catalog,
    types: &Types,
) -> Option<Class> {
    if let Some(t) = item.shelf_life.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        return (t != NONE && types.get(t).is_some()).then(|| Class { name: t.to_string(), basis: "explicit" });
    }
    if never_perishable(fields, item, catalog) {
        return None;
    }
    let kind = crate::consumable::field(fields, crate::consumable::KIND_FIELD);
    let consumable = has_lots || crate::consumable::is_consumable(kind, item.kind_preset.as_deref(), labels, catalog);
    if !consumable {
        return None;
    }
    if let Some((t, _)) = types.classify_name(&item.name) {
        return Some(Class { name: t.name.clone(), basis: "name" });
    }
    let preset = item.kind_preset.as_deref().and_then(|p| catalog.get(p))?;
    let t = preset.shelf_life_type.as_deref().filter(|t| types.get(t).is_some())?;
    Some(Class { name: t.to_string(), basis: "preset" })
}

/// A lot's expiry.
#[derive(Debug, Clone, PartialEq)]
pub struct Expiry {
    pub date: NaiveDate,
    /// `printed` (the lot's best-by date) or `computed`.
    pub basis: &'static str,
    /// e.g. "fridge 1 week from 2026-01-01 (acquired)".
    pub why: String,
}

fn day(v: Option<&str>) -> Option<NaiveDate> {
    v.and_then(|d| date_of(d).ok())
}

/// When a lot of a perishable expires (`None` for non-perishables, and for
/// a type with no figure for how the lot is kept, e.g. an unopened
/// shelf-stable-until-opened condiment):
///
/// * a printed best-by date wins;
/// * in the freezer (with a freezer figure): frozen_at (else acquired) + freezer;
/// * else opened (with an opened figure): opened_at + opened;
/// * else (thawed_at if later than acquired, else acquired) + the figure for
///   the storage (see [`ShelfType::figure_for`]).
pub fn expiry(lot: &Lot, ty: Option<&ShelfType>, storage: Storage) -> Option<Expiry> {
    let ty = ty?;
    if let Some(d) = day(lot.best_by.as_deref()) {
        return Some(Expiry { date: d, basis: "printed", why: "printed best-by date".into() });
    }
    let acquired = day(lot.acquired_at.as_deref()).or_else(|| day(Some(&lot.created_at)))?;
    let computed = |base: NaiveDate, l: Life, what: &str, from: &str| Expiry {
        date: date_after(base, l.span()),
        basis: "computed",
        why: if from.is_empty() {
            format!("{what} {} from {base}", l.display())
        } else {
            format!("{what} {} from {base} ({from})", l.display())
        },
    };
    if storage == Storage::Freezer {
        if let Some(l) = ty.figure(Storage::Freezer) {
            let (base, from) = match day(lot.frozen_at.as_deref()) {
                Some(f) => (f, "frozen"),
                None => (acquired, "acquired"),
            };
            return Some(computed(base, l, "freezer", from));
        }
    }
    if let (Some(o), Some(opened)) = (ty.opened_life(), day(lot.opened_at.as_deref())) {
        return Some(computed(opened, o, "opened", ""));
    }
    let (l, used) = ty.figure_for(storage)?;
    let (base, from) = match day(lot.thawed_at.as_deref()).filter(|t| *t > acquired) {
        Some(t) => (t, "thawed"),
        None => (acquired, "acquired"),
    };
    Some(computed(base, l, used.as_str(), from))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn lot(acquired: &str) -> Lot {
        Lot { acquired_at: Some(acquired.into()), status: "sealed".into(), ..Default::default() }
    }

    #[test]
    fn builtin_types_load_and_validate() {
        let t = Types::builtin();
        assert!(t.types.len() >= 60, "{} types", t.types.len());
        for (name, ty) in &t.types {
            assert_eq!(name, &ty.name);
            assert!(!ty.description.is_empty(), "{name}: description");
            assert!(ty.source.contains("FoodKeeper") || ty.source.contains("FDA"), "{name}: {}", ty.source);
            assert!(!ty.figures_text().is_empty(), "{name}");
            for q in ty.queries() {
                assert!(!crate::matching::name_tokens(&q).is_empty(), "{name}: '{q}' is all stopwords");
            }
        }
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
            "raw-chicken",
            "raw-beef",
            "ground-meat",
            "raw-fish",
            "cooked-leftovers",
            "bread",
            "tortillas",
            "deli-meat",
            "fresh-herbs",
            "opened-condiment",
            "opened-canned-food",
        ] {
            assert!(t.get(want).is_some(), "missing type {want}");
        }
        // shelf-stable until opened
        let c = t.get("opened-condiment").unwrap();
        assert!(Storage::ALL.iter().all(|s| c.figure(*s).is_none()) && c.opened_life().is_some());
    }

    #[test]
    fn config_overrides_and_new_types() {
        let table: toml::Table = toml::from_str(
            "[milk]\nfridge = \"5d\"\nfreezer = \"none\"\nsynonyms = [\"lactose free\"]\n\
             [kombucha]\nfridge = \"1m\"\ndescription = \"home brew\"\n",
        )
        .unwrap();
        let t = Types::builtin().with_overrides(&table).unwrap();
        let milk = t.get("milk").unwrap();
        assert_eq!((milk.fridge.as_deref(), milk.freezer.as_deref()), (Some("5d"), None));
        assert_eq!(milk.origin, "built-in+config");
        assert!(milk.synonyms.iter().any(|s| s == "whole milk") && milk.synonyms.iter().any(|s| s == "lactose free"));
        let k = t.get("kombucha").unwrap();
        assert_eq!((k.origin.as_str(), k.group.as_str(), k.fridge.as_deref()), ("config", "other", Some("1m")));
        assert_eq!(t.classify_name("Homemade Kombucha").unwrap().0.name, "kombucha");
        for bad in
            ["[x]\nfridge = \"soon\"\n", "[Bad_Name]\nfridge = \"1d\"\n", "[empty]\ndescription = \"no figures\"\n"]
        {
            let table: toml::Table = toml::from_str(bad).unwrap();
            assert!(Types::builtin().with_overrides(&table).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_classify_with_markers_and_exclusions() {
        let t = Types::builtin();
        let c = |n: &str| t.classify_name(n).map(|(t, _)| t.name.as_str());
        assert_eq!(c("Organic Bananas"), Some("banana"));
        assert_eq!(c("Horizon Organic Whole Milk, Half Gallon"), Some("milk"));
        assert_eq!(c("Boneless Skinless Chicken Thighs"), Some("raw-chicken"));
        assert_eq!(c("80/20 Ground Beef 1 lb"), Some("ground-meat"));
        assert_eq!(c("Sour Cream"), Some("sour-cream"));
        assert_eq!(c("Ben & Jerry's Ice Cream"), Some("ice-cream"));
        assert_eq!(c("Strawberries 1 lb"), Some("berries"));
        assert_eq!(c("Kerrygold Butter"), Some("butter"));
        assert_eq!(c("Atlantic Salmon Fillet"), Some("raw-fish"));
        // shelf-stable markers and exclusions
        assert_eq!(c("StarKist Chunk Light Tuna, 5 oz Can (Pack of 12)"), None);
        assert_eq!(c("Canned Chicken Breast"), None);
        assert_eq!(c("Banana Chips"), None);
        assert_eq!(c("Powdered Milk"), None);
        assert_eq!(c("Peanut Butter"), None);
        assert_eq!(c("Chicken Broth"), Some("opened-broth"));
        assert_eq!(c("Paper Towels"), None);
        assert_eq!(c("Dish Soap"), None);
    }

    #[test]
    fn storage_from_location_kind_or_name() {
        assert_eq!(Storage::from_path("Rental apartment / Kitchen / Fridge"), Some(Storage::Fridge));
        assert_eq!(Storage::from_path("Kitchen / Fridge / Freezer drawer"), Some(Storage::Freezer));
        assert_eq!(Storage::from_path("Garage / Chest Freezer / Bin 2"), Some(Storage::Freezer));
        assert_eq!(Storage::from_path("Kitchen / Pantry / Shelf 1"), Some(Storage::Pantry));
        assert_eq!(Storage::from_path("Kitchen"), None);
        let loc = |id: i64, name: &str, parent: Option<i64>, kind: &str| crate::store::Location {
            id,
            name: name.into(),
            parent_id: parent,
            kind: kind.into(),
            address: None,
            notes: None,
            created_at: String::new(),
        };
        let tree = Tree {
            locs: vec![
                loc(1, "Home", None, "property"),
                loc(2, "Garage", Some(1), "room"),
                loc(3, "Big white box", Some(2), "freezer"),
                loc(4, "Bin 2", Some(3), "container"),
                loc(5, "Refrigerator", Some(1), "container"),
            ],
        };
        assert_eq!(Storage::of_location(&tree, Some(4)), Storage::Freezer, "kind, up the tree");
        assert_eq!(Storage::of_location(&tree, Some(5)), Storage::Fridge, "name");
        assert_eq!(Storage::of_location(&tree, Some(2)), Storage::Pantry, "default");
        assert_eq!(Storage::of_location(&tree, None), Storage::Pantry);
    }

    #[test]
    fn expiry_by_storage_opening_freezing_and_thawing() {
        let t = Types::builtin();
        let beef = t.get("raw-beef"); // fridge 3d, freezer 4m
        let l = lot("2026-01-01");
        assert_eq!(expiry(&l, beef, Storage::Fridge).unwrap().date, d("2026-01-04"));
        // no pantry figure: the shortest of pantry/fridge applies
        assert_eq!(expiry(&l, beef, Storage::Pantry).unwrap().date, d("2026-01-04"));
        assert_eq!(expiry(&l, beef, Storage::Freezer).unwrap().date, d("2026-05-01"));
        // frozen later: the clock re-bases on freezing
        let frozen = Lot { frozen_at: Some("2026-01-03".into()), ..l.clone() };
        assert_eq!(expiry(&frozen, beef, Storage::Freezer).unwrap().date, d("2026-05-03"));
        // thawed into the fridge: fridge figure from the thaw
        let thawed = Lot { thawed_at: Some("2026-03-01".into()), ..frozen.clone() };
        assert_eq!(expiry(&thawed, beef, Storage::Fridge).unwrap().date, d("2026-03-04"));
        // opened: the opened figure from opening
        let ham = t.get("deli-meat"); // fridge 2w, opened 3d
        let opened = Lot { opened_at: Some("2026-01-05".into()), ..l.clone() };
        assert_eq!(expiry(&l, ham, Storage::Fridge).unwrap().date, d("2026-01-15"));
        assert_eq!(expiry(&opened, ham, Storage::Fridge).unwrap().date, d("2026-01-08"));
        // shelf-stable until opened
        let mayo = t.get("opened-condiment");
        assert!(expiry(&l, mayo, Storage::Pantry).is_none());
        assert_eq!(expiry(&opened, mayo, Storage::Fridge).unwrap().date, d("2026-03-05"));
        // a printed best-by wins
        let printed = Lot { best_by: Some("2026-02-01".into()), ..l.clone() };
        let e = expiry(&printed, beef, Storage::Fridge).unwrap();
        assert_eq!((e.date, e.basis), (d("2026-02-01"), "printed"));
        // non-perishables never expire
        assert!(expiry(&printed, None, Storage::Pantry).is_none());
        // ice cream outside a freezer: the freezer figure is all there is
        let ice = t.get("ice-cream");
        assert_eq!(expiry(&l, ice, Storage::Freezer).unwrap().date, d("2026-03-01"));
    }
}
