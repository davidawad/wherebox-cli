//! Estimated on hand (docs/SPEC.md, "Estimated on hand").
//!
//! Every active lot (sealed or open, qty > 0) of a consumable is either
//! counted or presumed used up: a lot with an expiry (computed, or a printed
//! best-by) once it is past it; any other lot once it is older than the
//! item's reorder cadence, counted from the latest of its purchase, its last
//! logged use and its last `lot confirm`. Logged consumption always wins: it
//! changes the lots themselves.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use chrono::NaiveDate;

use crate::db::Db;
use crate::error::{AppError, Result};
use crate::presets::{Catalog, Preset};
use crate::shelf_life::{self, Class, Expiry, Storage, Types};
use crate::store::{self, Item, Lot, Tree};
use crate::util::{date_of, fmt_qty, parse_duration_days};

/// Default cadences: the `[cadence]` config table (by preset name, then
/// preset category), then the `default_cadence` setting.
#[derive(Debug, Clone)]
pub struct Cadences {
    pub default_days: f64,
    pub table: BTreeMap<String, f64>,
}

impl Default for Cadences {
    fn default() -> Self {
        Self { default_days: 90.0, table: BTreeMap::new() }
    }
}

impl Cadences {
    pub fn load(config_path: &Path, default: &str) -> Result<Self> {
        let default_days = parse_duration_days(default).map_err(|e| e.context("default_cadence"))?;
        let text = std::fs::read_to_string(config_path).unwrap_or_default();
        let (head, _) = crate::enc_config::split(&text);
        let table: toml::Table =
            head.parse().map_err(|e| AppError::config(format!("{}: {e}", config_path.display())))?;
        let mut out = BTreeMap::new();
        match table.get("cadence") {
            None => {}
            Some(toml::Value::Table(t)) => {
                for (k, v) in t {
                    let s = match v {
                        toml::Value::String(s) => s.clone(),
                        toml::Value::Integer(i) => format!("{i}d"),
                        _ => {
                            return Err(AppError::config(format!("[cadence] {k}: expected a duration such as \"90d\"")))
                        }
                    };
                    out.insert(
                        k.trim().to_string(),
                        parse_duration_days(&s).map_err(|e| e.context(format!("[cadence] {k}")))?,
                    );
                }
            }
            Some(_) => return Err(AppError::config("[cadence] must be a table (preset or category = \"90d\")")),
        }
        Ok(Self { default_days, table: out })
    }

    /// The default cadence of an item of this preset, and where it came from.
    pub fn fallback(&self, preset: Option<&Preset>) -> (f64, String) {
        if let Some(p) = preset {
            if let Some(d) = self.table.get(&p.name) {
                return (*d, format!("[cadence] {}", p.name));
            }
            if let Some(d) = self.table.get(&p.category) {
                return (*d, format!("[cadence] {}", p.category));
            }
        }
        (self.default_days, "default_cadence".into())
    }
}

/// The `Recurring` field as an interval in days: `Some(Some(d))` for a
/// cadence, `Some(None)` for "yes, learn it", `None` for no / absent.
pub fn recurring(fields: Option<&BTreeMap<String, String>>) -> Option<Option<f64>> {
    let v = fields?.iter().find(|(k, _)| k.eq_ignore_ascii_case("recurring"))?.1.trim().to_ascii_lowercase();
    let v = v.strip_prefix("every ").unwrap_or(&v).to_string();
    match v.as_str() {
        "no" | "false" | "0" | "n" | "" => None,
        "daily" | "day" => Some(Some(1.0)),
        "weekly" | "week" => Some(Some(7.0)),
        "biweekly" | "fortnightly" => Some(Some(14.0)),
        "monthly" | "month" => Some(Some(parse_duration_days("1m").unwrap_or(30.0))),
        "yearly" | "annually" | "year" => Some(Some(parse_duration_days("1y").unwrap_or(365.0))),
        other => Some(parse_duration_days(&other.replace(' ', "")).ok().filter(|d| *d > 0.0)),
    }
}

/// The `Avg Interval Days` field, when positive.
fn avg_interval(fields: Option<&BTreeMap<String, String>>) -> Option<f64> {
    crate::consumable::field(fields, "Avg Interval Days")?.trim().parse::<f64>().ok().filter(|d| *d > 0.0)
}

/// Median gap in days between consecutive dates (sorted, two or more).
pub fn median_gap(dates: &[NaiveDate]) -> f64 {
    let mut gaps: Vec<i64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days()).collect();
    gaps.sort_unstable();
    let n = gaps.len();
    if n % 2 == 1 {
        gaps[n / 2] as f64
    } else {
        (gaps[n / 2 - 1] + gaps[n / 2]) as f64 / 2.0
    }
}

/// Purchase dates of an item, oldest first, without duplicates.
pub fn purchases(item: &Item, lots: &[&Lot]) -> Vec<NaiveDate> {
    let mut d: Vec<NaiveDate> = lots
        .iter()
        .filter_map(|l| l.acquired_at.as_deref())
        .chain(item.purchase_time.as_deref())
        .filter_map(|s| date_of(s).ok())
        .collect();
    d.sort();
    d.dedup();
    d
}

/// The learned reorder cadence in whole days and where it came from: with
/// three or more purchase dates the median gap; else a `Recurring` field
/// (`9d`, `weekly`), or when just `yes` the `Avg Interval Days` field, else
/// the gap of two purchases.
pub fn learned_cadence(
    item: &Item,
    lots: &[&Lot],
    fields: Option<&BTreeMap<String, String>>,
) -> Option<(f64, &'static str)> {
    let dates = purchases(item, lots);
    let interval = match recurring(fields) {
        _ if dates.len() >= 3 => Some((median_gap(&dates), "median of purchases")),
        _ if dates.is_empty() => None,
        Some(Some(d)) => Some((d, "Recurring field")),
        Some(None) => match avg_interval(fields) {
            Some(d) => Some((d, "Avg Interval Days")),
            None if dates.len() >= 2 => Some((median_gap(&dates), "purchase history")),
            None => None,
        },
        None => None,
    };
    interval.filter(|(d, _)| *d >= 1.0).map(|(d, from)| (d.round(), from))
}

/// The estimate for one lot.
#[derive(Debug, Clone)]
pub struct LotEst {
    pub storage: Storage,
    pub expiry: Option<Expiry>,
    /// Past its expiry (perishables).
    pub expired: bool,
    /// Presumed used up: expired, or older than the reorder cadence.
    pub presumed_used_up: bool,
    /// Why it is presumed used up.
    pub why: Option<String>,
}

/// The estimate for one item.
#[derive(Debug, Clone, Default)]
pub struct ItemEst {
    pub class: Option<Class>,
    pub on_hand: f64,
    /// `logged` or `estimated`.
    pub basis: &'static str,
    /// Per lot id.
    pub lots: HashMap<String, LotEst>,
    /// The cadence used for non-perishable lots (days, source).
    pub cadence: Option<(f64, String)>,
}

impl ItemEst {
    pub fn lot(&self, id: &str) -> Option<&LotEst> {
        self.lots.get(id)
    }

    pub fn is_estimated(&self) -> bool {
        self.basis == "estimated"
    }

    /// `2`, or `~2` when estimated.
    pub fn on_hand_text(&self) -> String {
        format!("{}{}", if self.is_estimated() { "~" } else { "" }, fmt_qty(self.on_hand))
    }

    /// Does this lot count toward on hand?
    pub fn counts(&self, l: &Lot) -> bool {
        l.is_active() && l.qty > 0.0 && self.lot(&l.id).is_none_or(|e| !e.presumed_used_up)
    }
}

fn round(q: f64) -> f64 {
    (q * 1e6).round() / 1e6 + 0.0
}

/// Shelf-life types, presets and default cadences: what estimates need from
/// the configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub types: Types,
    pub catalog: Catalog,
    pub cadences: Cadences,
}

impl Config {
    pub fn load(ctx: &crate::context::Ctx) -> Result<Self> {
        let path = &ctx.resolved.config_path;
        Ok(Self {
            types: Types::load(path)?,
            catalog: Catalog::load(path)?,
            cadences: Cadences::load(path, ctx.resolved.get("default_cadence"))?,
        })
    }

    pub fn builtin() -> Self {
        Self { types: Types::builtin(), catalog: Catalog::builtin(), cadences: Cadences::default() }
    }
}

/// Everything needed to estimate the items of one database.
pub struct Estimator<'a> {
    pub cfg: &'a Config,
    pub tree: Tree,
    pub fields: HashMap<String, BTreeMap<String, String>>,
    pub labels: HashMap<String, Vec<String>>,
    pub last_use: HashMap<String, String>,
    pub today: NaiveDate,
}

impl<'a> Estimator<'a> {
    pub fn load(cfg: &'a Config, db: &Db, today: NaiveDate) -> Result<Self> {
        Ok(Self {
            cfg,
            tree: Tree::load(db)?,
            fields: store::all_item_fields(db)?,
            labels: store::all_item_labels(db)?,
            last_use: store::last_use_by_lot(db)?,
            today,
        })
    }

    /// An item's shelf-life type.
    pub fn class(&self, item: &Item, has_lots: bool) -> Option<Class> {
        let none = Vec::new();
        shelf_life::classify(
            item,
            self.fields.get(&item.id),
            self.labels.get(&item.id).unwrap_or(&none),
            has_lots,
            &self.cfg.catalog,
            &self.cfg.types,
        )
    }

    /// Where a lot (of `item`) is kept.
    pub fn storage(&self, item: &Item, lot: &Lot) -> Storage {
        Storage::of_location(&self.tree, lot.location_id.or(item.location_id))
    }

    /// Estimate an item from its lots (all of them, any status).
    pub fn item(&self, item: &Item, lots: &[Lot]) -> ItemEst {
        let class = self.class(item, !lots.is_empty());
        if lots.is_empty() {
            return ItemEst { class, on_hand: item.quantity.max(0) as f64, basis: "logged", ..Default::default() };
        }
        let ty = class.as_ref().and_then(|c| self.cfg.types.get(&c.name));
        let fields = self.fields.get(&item.id);
        let refs: Vec<&Lot> = lots.iter().collect();
        let preset = item.kind_preset.as_deref().and_then(|p| self.cfg.catalog.get(p));
        let mut cadence: Option<(f64, String)> = None;
        let mut out = ItemEst { class: class.clone(), basis: "logged", ..Default::default() };
        let day = |s: Option<&str>| s.and_then(|d| date_of(d).ok());
        for l in lots {
            let storage = self.storage(item, l);
            let expiry = shelf_life::expiry(l, ty, storage);
            let active = l.is_active() && l.qty > 0.0;
            let expired = active && expiry.as_ref().is_some_and(|e| e.date < self.today);
            let mut why =
                expired.then(|| format!("expired {}", expiry.as_ref().map(|e| e.date.to_string()).unwrap_or_default()));
            if active && expiry.is_none() {
                let (days, from) = cadence
                    .get_or_insert_with(|| {
                        learned_cadence(item, &refs, fields)
                            .map(|(d, f)| (d, f.to_string()))
                            .unwrap_or_else(|| self.cfg.cadences.fallback(preset))
                    })
                    .clone();
                let clock = [
                    day(l.acquired_at.as_deref()).or_else(|| day(Some(&l.created_at))),
                    day(self.last_use.get(&l.id).map(String::as_str)),
                    day(l.confirmed_at.as_deref()),
                ]
                .into_iter()
                .flatten()
                .max();
                if let Some(c) = clock {
                    let age = (self.today - c).num_days();
                    if age as f64 > days {
                        why = Some(format!("{age} days since {c}, reorder cadence {days} days ({from})"));
                    }
                }
            }
            let presumed = active && why.is_some();
            if active && !presumed {
                out.on_hand += l.qty;
            }
            if presumed {
                out.basis = "estimated";
            }
            out.lots.insert(l.id.clone(), LotEst { storage, expiry, expired, presumed_used_up: presumed, why });
        }
        out.on_hand = round(out.on_hand);
        out.cadence = cadence;
        out
    }

    /// Estimates of many items, by item id.
    pub fn all(&self, items: &[Item], lots: &[Lot]) -> HashMap<String, ItemEst> {
        let mut by_item: HashMap<&str, Vec<Lot>> = HashMap::new();
        for l in lots {
            by_item.entry(l.item_id.as_str()).or_default().push(l.clone());
        }
        items
            .iter()
            .map(|i| (i.id.clone(), self.item(i, by_item.get(i.id.as_str()).map_or(&[][..], Vec::as_slice))))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn item(name: &str) -> Item {
        Item { id: name.into(), name: name.into(), status: "owned".into(), ..Default::default() }
    }

    fn lot(id: &str, acquired: &str, qty: f64) -> Lot {
        Lot {
            id: id.into(),
            item_id: "x".into(),
            qty,
            acquired_at: Some(acquired.into()),
            status: "sealed".into(),
            ..Default::default()
        }
    }

    fn est<'a>(cfg: &'a Config, today: &str) -> Estimator<'a> {
        Estimator {
            cfg,
            tree: Tree::default(),
            fields: HashMap::new(),
            labels: HashMap::new(),
            last_use: HashMap::new(),
            today: d(today),
        }
    }

    #[test]
    fn non_perishables_are_presumed_used_up_after_the_cadence() {
        let cfg = Config::builtin();
        // 12 purchases of canned tuna over two years, ~66 days apart, never logged as used
        let start = d("2024-01-01");
        let lots: Vec<Lot> = (0..12)
            .map(|n| lot(&format!("L{n:02}"), &(start + chrono::Duration::days(66 * n)).to_string(), 12.0))
            .collect();
        let tuna = item("Canned Tuna");
        // the last purchase was 2025-12-28
        let e = est(&cfg, "2026-01-27").item(&tuna, &lots);
        assert!(e.class.is_none(), "canned: non-perishable");
        assert_eq!(e.cadence.as_ref().map(|c| c.0), Some(66.0));
        assert_eq!((e.on_hand, e.basis, e.on_hand_text().as_str()), (12.0, "estimated", "~12"), "only the latest lot");
        assert!(!e.lot("L11").unwrap().presumed_used_up && e.lot("L10").unwrap().presumed_used_up);
        // a logged use or a confirmation restarts a lot's clock
        let mut ev = est(&cfg, "2026-01-27");
        ev.last_use.insert("L10".into(), "2026-01-10".into());
        let mut lots2 = lots.clone();
        lots2[9].confirmed_at = Some("2026-01-20".into());
        let e = ev.item(&tuna, &lots2);
        assert_eq!(e.on_hand, 36.0);
        // one purchase, no history: the default cadence (90 days)
        let soap = item("Dish Soap");
        let one = [lot("S", "2026-01-01", 2.0)];
        assert_eq!(est(&cfg, "2026-03-01").item(&soap, &one).basis, "logged");
        let e = est(&cfg, "2026-05-01").item(&soap, &one);
        assert_eq!((e.on_hand, e.basis), (0.0, "estimated"));
        assert!(e.lot("S").unwrap().why.as_deref().unwrap().contains("default_cadence"));
        // no lots: the logged quantity
        let mut drill = item("Drill");
        drill.quantity = 2;
        let e = est(&cfg, "2026-05-01").item(&drill, &[]);
        assert_eq!((e.on_hand, e.basis), (2.0, "logged"));
    }

    #[test]
    fn perishables_are_presumed_used_up_past_expiry() {
        let cfg = Config::builtin();
        let milk = item("Whole Milk");
        let lots = [lot("A", "2026-01-01", 1.0), lot("B", "2026-01-06", 1.0)];
        let e = est(&cfg, "2026-01-10").item(&milk, &lots);
        assert_eq!(e.class.as_ref().map(|c| c.name.as_str()), Some("milk"));
        assert_eq!(e.lot("A").unwrap().expiry.as_ref().unwrap().date, d("2026-01-08"));
        assert!(e.lot("A").unwrap().expired && !e.lot("B").unwrap().expired);
        assert_eq!((e.on_hand, e.basis), (1.0, "estimated"));
        // a confirmation does not revive an expired perishable
        let mut confirmed = lots.clone();
        confirmed[0].confirmed_at = Some("2026-01-10".into());
        assert_eq!(est(&cfg, "2026-01-10").item(&milk, &confirmed).on_hand, 1.0);
    }

    #[test]
    fn cadence_defaults_by_preset_then_category() {
        let mut cfg = Config::builtin();
        cfg.cadences.table.insert("household".into(), 30.0);
        cfg.cadences.table.insert("consumable".into(), 60.0);
        let c = &cfg.cadences;
        assert_eq!(c.fallback(cfg.catalog.get("household")).0, 30.0);
        assert_eq!(c.fallback(cfg.catalog.get("toiletry")).0, 60.0);
        assert_eq!(c.fallback(None), (90.0, "default_cadence".to_string()));
    }

    #[test]
    fn recurring_values() {
        let f = |v: &str| recurring(Some(&BTreeMap::from([("recurring".to_string(), v.to_string())])));
        assert_eq!(f("9d"), Some(Some(9.0)));
        assert_eq!(f("every 2 w"), Some(Some(14.0)));
        assert_eq!(f("weekly"), Some(Some(7.0)));
        assert_eq!(f("yes"), Some(None));
        assert_eq!(f("no"), None);
        assert_eq!(recurring(None), None);
    }
}
