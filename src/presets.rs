//! Kind presets: what a kind of item tracks, its defaults and alerts.
//!
//! A preset is a TOML file. The built-in set lives in `presets/kinds/*.toml`
//! (compiled in); a file of the same name in `<config dir>/presets/kinds/`
//! replaces a built-in preset, and new names add presets. See
//! docs/presets.md for the format.
//!
//! What a preset fills in:
//!
//! * a lot's use-by or best-by date (`[shelf_life]`), from the day it was
//!   bought, frozen or opened and where it is kept: a location named like a
//!   freezer, a fridge or a pantry (the lot's location, else the item's);
//! * an item's service dates (`[intervals]`) as custom fields (`Replace By`,
//!   `Next Inspection`, `Next Service`, `Reappraise By`) from its `Installed`
//!   field or purchase date, and `warranty_expires` from a warranty term;
//! * `[defaults]`: unit, labels and custom fields of new items.
//!
//! Ranges such as `3-5d` fill with the shorter, safer end. Every value in a
//! built-in preset is covered by a `[[sources]]` citation (checked by a
//! test); values without a source are left unset.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::store::{Item, Lot};
use crate::util::{date_after, parse_span, Span};

/// The built-in presets, `(name, TOML)`.
const BUILTIN: &[(&str, &str)] = &[
    ("alcohol", include_str!("../presets/kinds/alcohol.toml")),
    ("appliance", include_str!("../presets/kinds/appliance.toml")),
    ("art", include_str!("../presets/kinds/art.toml")),
    ("battery", include_str!("../presets/kinds/battery.toml")),
    ("beverage", include_str!("../presets/kinds/beverage.toml")),
    ("bread", include_str!("../presets/kinds/bread.toml")),
    ("co-detector", include_str!("../presets/kinds/co-detector.toml")),
    ("collectible", include_str!("../presets/kinds/collectible.toml")),
    ("condiment", include_str!("../presets/kinds/condiment.toml")),
    ("dairy", include_str!("../presets/kinds/dairy.toml")),
    ("document", include_str!("../presets/kinds/document.toml")),
    ("eggs", include_str!("../presets/kinds/eggs.toml")),
    ("electronics", include_str!("../presets/kinds/electronics.toml")),
    ("filter", include_str!("../presets/kinds/filter.toml")),
    ("fire-extinguisher", include_str!("../presets/kinds/fire-extinguisher.toml")),
    ("firearm", include_str!("../presets/kinds/firearm.toml")),
    ("first-aid", include_str!("../presets/kinds/first-aid.toml")),
    ("frozen", include_str!("../presets/kinds/frozen.toml")),
    ("furniture", include_str!("../presets/kinds/furniture.toml")),
    ("generator", include_str!("../presets/kinds/generator.toml")),
    ("household", include_str!("../presets/kinds/household.toml")),
    ("hvac", include_str!("../presets/kinds/hvac.toml")),
    ("jewelry", include_str!("../presets/kinds/jewelry.toml")),
    ("leftovers", include_str!("../presets/kinds/leftovers.toml")),
    ("meat.ground", include_str!("../presets/kinds/meat.ground.toml")),
    ("meat.raw", include_str!("../presets/kinds/meat.raw.toml")),
    ("medicine", include_str!("../presets/kinds/medicine.toml")),
    ("pantry", include_str!("../presets/kinds/pantry.toml")),
    ("pet-supply", include_str!("../presets/kinds/pet-supply.toml")),
    ("plant", include_str!("../presets/kinds/plant.toml")),
    ("poultry.raw", include_str!("../presets/kinds/poultry.raw.toml")),
    ("produce", include_str!("../presets/kinds/produce.toml")),
    ("seafood.raw", include_str!("../presets/kinds/seafood.raw.toml")),
    ("seasonal", include_str!("../presets/kinds/seasonal.toml")),
    ("smoke-detector", include_str!("../presets/kinds/smoke-detector.toml")),
    ("supplement", include_str!("../presets/kinds/supplement.toml")),
    ("toiletry", include_str!("../presets/kinds/toiletry.toml")),
    ("tool", include_str!("../presets/kinds/tool.toml")),
    ("vehicle", include_str!("../presets/kinds/vehicle.toml")),
    ("water-heater", include_str!("../presets/kinds/water-heater.toml")),
];

pub const CATEGORIES: &[&str] = &["food", "consumable", "safety", "durable", "valuable", "document", "other"];

/// Dates and fields a preset can say it tracks.
pub const TRACKS: &[&str] = &[
    "use_by",
    "best_by",
    "opened_at",
    "frozen_at",
    "expiration",
    "installed_at",
    "replace",
    "inspection",
    "service",
    "warranty",
    "serial",
    "appraisal",
    "replacement_value",
    "registration",
    "shelf_life",
    "hazmat",
    "care",
    "par_level",
];

/// Values that need a citation when set, as named in `[[sources]] covers`.
const CITED: &[&str] =
    &["fridge", "freezer", "pantry", "opened", "replace", "inspect", "service", "appraise", "warranty", "alert_days"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShelfLife {
    /// Which lot date the shelf life fills: `use_by` (expires_at) or `best_by`.
    pub date: Option<String>,
    pub fridge: Option<String>,
    pub freezer: Option<String>,
    pub pantry: Option<String>,
    /// After opening (caps the date of an opened lot).
    pub opened: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intervals {
    pub replace: Option<String>,
    pub inspect: Option<String>,
    pub service: Option<String>,
    pub appraise: Option<String>,
    /// Warranty term from the purchase date.
    pub warranty: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub unit: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    /// Which values it backs, e.g. "fridge, freezer".
    pub covers: String,
    pub cite: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(default)]
    pub tracks: Vec<String>,
    /// Warn this many days before a tracked date.
    pub alert_days: Option<u32>,
    pub notes: Option<String>,
    #[serde(default)]
    pub shelf_life: ShelfLife,
    #[serde(default)]
    pub intervals: Intervals,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub sources: Vec<Citation>,
    /// `built-in` or the override file's path.
    #[serde(skip_deserializing)]
    pub origin: String,
}

/// Where a lot is kept, from its location's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Fridge,
    Freezer,
    Pantry,
}

impl Storage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fridge => "fridge",
            Self::Freezer => "freezer",
            Self::Pantry => "pantry",
        }
    }

    /// The storage named by the nearest segment of a location path
    /// (`Kitchen / Fridge / Freezer drawer` is a freezer).
    pub fn from_path(path: &str) -> Option<Self> {
        crate::store::split_path(path).iter().rev().find_map(|seg| {
            let s = seg.to_lowercase();
            if s.contains("freezer") || s.contains("deep freeze") {
                Some(Self::Freezer)
            } else if s.contains("fridge") || s.contains("refrigerator") {
                Some(Self::Fridge)
            } else if s.contains("pantry") || s.contains("cupboard") || s.contains("larder") {
                Some(Self::Pantry)
            } else {
                None
            }
        })
    }
}

/// A shelf life or interval: `3-5d`, `12m`, `10y`, `3-5w`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Life {
    pub min: u32,
    pub max: u32,
    pub unit: char,
}

impl Life {
    pub fn parse(s: &str) -> Result<Self> {
        let t = s.trim().to_ascii_lowercase();
        let bad = || AppError::config(format!("bad duration '{s}' (examples: 3-5d, 2w, 4-12m, 10y)"));
        let unit = t.chars().last().filter(|c| matches!(c, 'd' | 'w' | 'm' | 'y')).ok_or_else(bad)?;
        let nums = &t[..t.len() - 1];
        let (a, b) = nums.split_once('-').unwrap_or((nums, nums));
        let (min, max): (u32, u32) = (a.trim().parse().map_err(|_| bad())?, b.trim().parse().map_err(|_| bad())?);
        if min == 0 || max < min {
            return Err(bad());
        }
        Ok(Self { min, max, unit })
    }

    /// The shorter end as a calendar span.
    pub fn span(self) -> Span {
        parse_span(&format!("{}{}", self.min, self.unit)).unwrap_or(Span::Days(f64::from(self.min)))
    }

    pub fn display(self) -> String {
        let unit = match self.unit {
            'd' => "day",
            'w' => "week",
            'm' => "month",
            _ => "year",
        };
        let n = if self.min == self.max { self.min.to_string() } else { format!("{}-{}", self.min, self.max) };
        format!("{n} {unit}{}", if self.max == 1 { "" } else { "s" })
    }
}

fn life(v: &Option<String>) -> Option<Life> {
    v.as_deref().and_then(|s| Life::parse(s).ok())
}

/// What a preset filled into a lot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LotFill {
    /// `expires_at` or `best_by`.
    pub field: &'static str,
    pub date: String,
    pub storage: &'static str,
    /// e.g. "3-5 days in the fridge from 2026-01-01".
    pub why: String,
}

impl Preset {
    fn parse(text: &str, origin: &str) -> Result<Self> {
        let mut p: Self = toml::from_str(text).map_err(|e| AppError::config(format!("preset {origin}: {e}")))?;
        p.origin = origin.to_string();
        p.validate().map_err(|e| e.context(format!("preset {origin}")))?;
        Ok(p)
    }

    fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(AppError::config(m));
        if self.name.is_empty()
            || !self.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        {
            return bad(format!("bad name '{}' (lowercase letters, digits, '.', '-', '_')", self.name));
        }
        if !CATEGORIES.contains(&self.category.as_str()) {
            return bad(format!("category '{}' is not one of {}", self.category, CATEGORIES.join(", ")));
        }
        if let Some(t) = self.tracks.iter().find(|t| !TRACKS.contains(&t.as_str())) {
            return bad(format!("unknown track '{t}' (known: {})", TRACKS.join(", ")));
        }
        if let Some(d) = self.shelf_life.date.as_deref().filter(|d| !matches!(*d, "use_by" | "best_by")) {
            return bad(format!("shelf_life.date '{d}' must be use_by or best_by"));
        }
        for (k, v) in self.values() {
            Life::parse(v).map_err(|e| e.context(k))?;
        }
        let has_life =
            [&self.shelf_life.fridge, &self.shelf_life.freezer, &self.shelf_life.pantry].iter().any(|v| v.is_some());
        if has_life && self.shelf_life.date.is_none() {
            return bad("shelf_life needs date = \"use_by\" or \"best_by\"".into());
        }
        Ok(())
    }

    /// Every duration value that is set, by its key.
    fn values(&self) -> Vec<(&'static str, &str)> {
        let (s, i) = (&self.shelf_life, &self.intervals);
        [
            ("fridge", &s.fridge),
            ("freezer", &s.freezer),
            ("pantry", &s.pantry),
            ("opened", &s.opened),
            ("replace", &i.replace),
            ("inspect", &i.inspect),
            ("service", &i.service),
            ("appraise", &i.appraise),
            ("warranty", &i.warranty),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_deref().map(|v| (k, v)))
        .collect()
    }

    /// Values that are set but named by no `[[sources]] covers`.
    pub fn uncited(&self) -> Vec<&'static str> {
        let covered = |k: &str| self.sources.iter().any(|s| s.covers.split([',', ' ', '(']).any(|c| c.trim() == k));
        self.values()
            .into_iter()
            .map(|(k, _)| k)
            .chain(self.alert_days.map(|_| "alert_days"))
            .filter(|k| CITED.contains(k) && !covered(k))
            .collect()
    }

    /// Items of this kind are consumed rather than kept (food and supplies).
    pub fn is_consumable(&self) -> bool {
        matches!(self.category.as_str(), "food" | "consumable")
    }

    /// Insurers want a serial number for items of this kind.
    pub fn is_serialized(&self) -> bool {
        self.tracks.iter().any(|t| t == "serial")
    }

    /// Fill a lot's use-by / best-by date from the shelf life. `storage` is
    /// where the lot is kept (from its location); a frozen lot counts as in
    /// the freezer. Dates already given are kept. Returns what was filled.
    pub fn fill_lot(&self, lot: &mut Lot, storage: Option<Storage>) -> Option<LotFill> {
        let s = &self.shelf_life;
        let field = match s.date.as_deref()? {
            "use_by" if lot.expires_at.is_none() => "expires_at",
            "best_by" if lot.best_by.is_none() => "best_by",
            _ => return None,
        };
        let of = |st: Storage| match st {
            Storage::Fridge => life(&s.fridge),
            Storage::Freezer => life(&s.freezer),
            Storage::Pantry => life(&s.pantry),
        };
        let storage = if lot.frozen_at.is_some() { Some(Storage::Freezer) } else { storage };
        let storage = storage.filter(|st| of(*st).is_some()).or_else(|| {
            [Storage::Fridge, Storage::Pantry, Storage::Freezer].into_iter().find(|st| of(*st).is_some())
        })?;
        let l = of(storage)?;
        let acquired = lot.acquired_at.as_deref().and_then(|d| crate::util::date_of(d).ok())?;
        let (base, from) = match storage {
            Storage::Freezer => {
                let f = lot.frozen_at.as_deref().and_then(|d| crate::util::date_of(d).ok()).unwrap_or(acquired);
                lot.frozen_at.get_or_insert_with(|| f.to_string());
                (f, "freezing")
            }
            _ => (acquired, "purchase"),
        };
        let mut due = date_after(base, l.span());
        let mut why = format!("{} in the {} from {from} on {base}", l.display(), storage.as_str());
        if let (Some(o), Some(opened)) =
            (life(&s.opened), lot.opened_at.as_deref().and_then(|d| crate::util::date_of(d).ok()))
        {
            let after = date_after(opened, o.span());
            if after < due {
                due = after;
                why = format!("{} after opening on {opened}", o.display());
            }
        }
        let date = due.to_string();
        match field {
            "expires_at" => lot.expires_at = Some(date.clone()),
            _ => lot.best_by = Some(date.clone()),
        }
        Some(LotFill { field, date, storage: storage.as_str(), why })
    }

    /// Defaults and service dates for a new item: sets the unit and the
    /// warranty end (when unset) and returns custom fields and labels to add.
    /// `fields` are the item's custom fields so far (`Installed` is the base
    /// date of intervals, else the purchase date). Recurring dates
    /// (inspection, service, appraisal) are the first one on or after
    /// `today`; `Replace By` is not rolled forward, so an overdue
    /// replacement shows as a past date.
    pub fn apply_item(
        &self,
        item: &mut Item,
        fields: &BTreeMap<String, String>,
        today: NaiveDate,
    ) -> (Vec<(String, String)>, Vec<String>) {
        if item.unit.is_none() {
            item.unit.clone_from(&self.defaults.unit);
        }
        let get = |k: &str| fields.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());
        let base: Option<NaiveDate> =
            get("Installed").or(item.purchase_time.as_deref()).and_then(|d| crate::util::date_of(d).ok());
        let mut out: Vec<(String, String)> = Vec::new();
        if let Some(b) = base {
            let i = &self.intervals;
            for (name, v, recurring) in [
                ("Replace By", &i.replace, false),
                ("Next Inspection", &i.inspect, true),
                ("Next Service", &i.service, true),
                ("Reappraise By", &i.appraise, true),
            ] {
                if let (Some(l), None) = (life(v), get(name)) {
                    let mut d = date_after(b, l.span());
                    while recurring && d < today {
                        d = date_after(d, l.span());
                    }
                    out.push((name.to_string(), d.to_string()));
                }
            }
            if let (Some(l), None, false) = (life(&i.warranty), &item.warranty_expires, item.lifetime_warranty) {
                item.warranty_expires = Some(date_after(b, l.span()).to_string());
            }
        }
        for (k, v) in &self.defaults.fields {
            if get(k).is_none() && !out.iter().any(|(n, _)| n == k) {
                out.push((k.clone(), v.clone()));
            }
        }
        (out, self.defaults.labels.clone())
    }
}

/// `<config dir>/presets/kinds`: user presets and overrides.
pub fn user_dir(config_path: &Path) -> PathBuf {
    config_path.parent().unwrap_or(Path::new(".")).join("presets").join("kinds")
}

/// Every preset: built-in, then the user's (which win by name).
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub presets: BTreeMap<String, Preset>,
}

impl Catalog {
    pub fn builtin() -> Self {
        let presets = BUILTIN
            .iter()
            .map(|(n, t)| {
                let p = Preset::parse(t, "built-in").unwrap_or_else(|e| panic!("built-in preset {n}: {e}"));
                (p.name.clone(), p)
            })
            .collect();
        Self { presets }
    }

    /// Built-in presets plus `*.toml` in [`user_dir`].
    pub fn load(config_path: &Path) -> Result<Self> {
        let mut c = Self::builtin();
        let dir = user_dir(config_path);
        let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(c) };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        for f in files {
            let text =
                std::fs::read_to_string(&f).map_err(|e| AppError::io(format!("reading {}: {e}", f.display())))?;
            let p = Preset::parse(&text, &f.display().to_string())?;
            c.presets.insert(p.name.clone(), p);
        }
        Ok(c)
    }

    pub fn get(&self, name: &str) -> Option<&Preset> {
        self.presets.get(name.trim())
    }

    pub fn require(&self, name: &str) -> Result<&Preset> {
        self.get(name).ok_or_else(|| AppError::not_found(format!("no preset '{name}' (see `wherebox preset ls`)")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_file_is_compiled_in_and_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("presets").join("kinds");
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().trim_end_matches(".toml").to_string())
            .collect();
        files.sort();
        let names: Vec<&str> = BUILTIN.iter().map(|(n, _)| *n).collect();
        assert_eq!(files, names);
        let c = Catalog::builtin();
        for (n, p) in &c.presets {
            assert_eq!(n, &p.name);
            assert!(p.uncited().is_empty(), "{n}: uncited {:?}", p.uncited());
        }
        for spec in [
            "meat.raw",
            "poultry.raw",
            "seafood.raw",
            "dairy",
            "eggs",
            "produce",
            "bread",
            "leftovers",
            "frozen",
            "pantry",
            "condiment",
            "beverage",
            "alcohol",
            "medicine",
            "supplement",
            "first-aid",
            "household",
            "toiletry",
            "filter",
            "battery",
            "smoke-detector",
            "co-detector",
            "fire-extinguisher",
            "appliance",
            "electronics",
            "tool",
            "furniture",
            "vehicle",
            "generator",
            "hvac",
            "water-heater",
            "firearm",
            "jewelry",
            "art",
            "collectible",
            "document",
            "seasonal",
            "plant",
            "pet-supply",
        ] {
            assert!(c.get(spec).is_some(), "SPEC preset {spec} missing");
        }
    }

    #[test]
    fn food_sources_cite_foodkeeper_and_smoke_alarms_nfpa() {
        let c = Catalog::builtin();
        for p in c.presets.values().filter(|p| {
            p.shelf_life.fridge.is_some() || p.shelf_life.freezer.is_some() || p.shelf_life.pantry.is_some()
        }) {
            assert!(p.sources.iter().any(|s| s.cite.contains("FoodKeeper")), "{}", p.name);
        }
        let smoke = c.get("smoke-detector").unwrap();
        assert_eq!(smoke.intervals.replace.as_deref(), Some("10y"));
        assert_eq!(
            smoke.sources[0].url.as_deref(),
            Some("https://www.nfpa.org/education-and-research/home-fire-safety/smoke-alarms")
        );
    }

    #[test]
    fn durations_parse() {
        assert_eq!(Life::parse("3-5d").unwrap(), Life { min: 3, max: 5, unit: 'd' });
        assert_eq!(Life::parse("10y").unwrap().display(), "10 years");
        assert_eq!(Life::parse("4-12m").unwrap().display(), "4-12 months");
        assert!(Life::parse("5-3d").is_err() && Life::parse("3").is_err() && Life::parse("0d").is_err());
    }

    #[test]
    fn storage_from_location_names() {
        assert_eq!(Storage::from_path("Rental apartment / Kitchen / Fridge"), Some(Storage::Fridge));
        assert_eq!(Storage::from_path("Rental apartment / Kitchen / Fridge / Freezer drawer"), Some(Storage::Freezer));
        assert_eq!(Storage::from_path("Garage / Chest Freezer / Bin 2"), Some(Storage::Freezer));
        assert_eq!(Storage::from_path("Kitchen / Pantry / Shelf 1"), Some(Storage::Pantry));
        assert_eq!(Storage::from_path("Kitchen"), None);
    }

    fn lot(acquired: &str) -> Lot {
        Lot { acquired_at: Some(acquired.into()), status: "sealed".into(), ..Default::default() }
    }

    #[test]
    fn lots_get_the_shorter_end_of_the_range_by_storage() {
        let c = Catalog::builtin();
        let meat = c.get("meat.raw").unwrap();
        let mut l = lot("2026-01-01");
        let f = meat.fill_lot(&mut l, Some(Storage::Fridge)).unwrap();
        assert_eq!((f.field, l.expires_at.as_deref()), ("expires_at", Some("2026-01-04")));
        let mut l = lot("2026-01-01");
        meat.fill_lot(&mut l, Some(Storage::Freezer)).unwrap();
        assert_eq!((l.expires_at.as_deref(), l.frozen_at.as_deref()), (Some("2026-05-01"), Some("2026-01-01")));
        // unknown place: the fridge
        let mut l = lot("2026-01-01");
        meat.fill_lot(&mut l, None).unwrap();
        assert_eq!(l.expires_at.as_deref(), Some("2026-01-04"));
        // a given date wins
        let mut l = Lot { expires_at: Some("2026-02-01".into()), ..lot("2026-01-01") };
        assert!(meat.fill_lot(&mut l, None).is_none());
        // bread: pantry best-by; no fridge figure, so a fridge location falls back to the pantry
        let mut l = lot("2026-01-01");
        let f = c.get("bread").unwrap().fill_lot(&mut l, Some(Storage::Fridge)).unwrap();
        assert_eq!((f.field, f.storage, l.best_by.as_deref()), ("best_by", "pantry", Some("2026-01-15")));
        // no shelf life: nothing
        assert!(c.get("dairy").unwrap().fill_lot(&mut lot("2026-01-01"), None).is_none());
    }

    #[test]
    fn intervals_become_fields_from_install_or_purchase() {
        let c = Catalog::builtin();
        let smoke = c.get("smoke-detector").unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let mut item = Item { purchase_time: Some("2020-03-01".into()), ..Default::default() };
        let (fields, _) = smoke.apply_item(&mut item, &BTreeMap::new(), today);
        assert!(fields.contains(&("Replace By".into(), "2030-03-01".into())), "{fields:?}");
        assert!(fields.contains(&("Next Inspection".into(), "2026-02-01".into())), "rolled forward: {fields:?}");
        let installed = BTreeMap::from([("Installed".to_string(), "2021-06-15".to_string())]);
        let (fields, _) = smoke.apply_item(&mut item, &installed, today);
        assert!(fields.contains(&("Replace By".into(), "2031-06-15".into())));
        let (fields, _) = smoke.apply_item(&mut Item::default(), &BTreeMap::new(), today);
        assert!(fields.is_empty(), "no base date, no guess");
    }

    #[test]
    fn user_presets_override_builtins() {
        let t = tempfile::TempDir::new().unwrap();
        let cfg = t.path().join("config.toml");
        let dir = user_dir(&cfg);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("meat.raw.toml"),
            "name = \"meat.raw\"\ndescription = \"mine\"\ncategory = \"food\"\n[shelf_life]\ndate = \"use_by\"\nfridge = \"2d\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("kombucha.toml"), "name = \"kombucha\"\ndescription = \"k\"\ncategory = \"food\"\n")
            .unwrap();
        let c = Catalog::load(&cfg).unwrap();
        assert_eq!(c.get("meat.raw").unwrap().description, "mine");
        assert!(c.get("meat.raw").unwrap().origin.ends_with("meat.raw.toml"));
        assert!(c.get("kombucha").is_some() && c.get("eggs").is_some());
        std::fs::write(dir.join("bad.toml"), "name = \"bad\"\ndescription = \"b\"\ncategory = \"nope\"\n").unwrap();
        assert!(Catalog::load(&cfg).is_err());
    }
}
