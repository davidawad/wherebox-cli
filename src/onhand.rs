//! Estimated on hand (docs/SPEC.md, "Shelf life and on hand").
//!
//! Every consumable lot used to count as on hand forever. Here an active
//! lot is presumed used up when (a) it has an expiry (a perishable) and that
//! date has passed, or (b) it has none and it is older than the item's
//! reorder cadence: the median gap between purchases, else the `Recurring`
//! field, else the `[reorder_cadence]` config table (by preset name, then
//! preset category, then `default`, built-in 90 days). Age counts from the
//! lot's last evidence: acquired, last logged consumption, or `lot
//! confirm`. Explicit logs always win: they set the logged quantity.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::db::Db;
use crate::error::{AppError, Result};
use crate::presets::Catalog;
use crate::shelf_life::{Basis, Expiry, ItemFacts, Storage, Types};
use crate::store::{self, Item, Lot, Tree};
use crate::util::{date_of, fmt_qty, parse_duration_days};

/// Fallback reorder cadence when nothing else gives one.
pub const DEFAULT_CADENCE_DAYS: f64 = 90.0;

// ---------------------------------------------------------------------------
// Reorder cadence (shared with `shopping`)
// ---------------------------------------------------------------------------

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
pub fn avg_interval(fields: Option<&BTreeMap<String, String>>) -> Option<f64> {
    crate::consumable::field(fields, "Avg Interval Days")?.trim().parse::<f64>().ok().filter(|d| *d > 0.0)
}

/// Median gap in days between consecutive dates (sorted, two or more).
pub fn median_gap(dates: &[NaiveDate]) -> f64 {
    let mut gaps: Vec<i64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days()).collect();
    gaps.sort_unstable();
    let n = gaps.len();
    if n == 0 {
        return 0.0;
    }
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

/// The `[reorder_cadence]` config table: fallback cadences by preset name
/// or category, and `default`.
#[derive(Debug, Clone)]
pub struct CadenceDefaults {
    pub default: f64,
    pub by_key: BTreeMap<String, f64>,
}

impl Default for CadenceDefaults {
    fn default() -> Self {
        Self { default: DEFAULT_CADENCE_DAYS, by_key: BTreeMap::new() }
    }
}

impl CadenceDefaults {
    /// From a config file's text.
    pub fn from_config(text: &str) -> Result<Self> {
        let (head, _) = crate::enc_config::split(text);
        let table: toml::Table = head.parse().map_err(|e| AppError::config(format!("TOML: {e}")))?;
        let mut out = Self::default();
        let Some(t) = table.get("reorder_cadence") else { return Ok(out) };
        let t = t.as_table().ok_or_else(|| AppError::config("[reorder_cadence] must be a table"))?;
        for (k, v) in t {
            let s =
                v.as_str().map(str::to_string).or_else(|| v.as_integer().map(|i| format!("{i}d"))).ok_or_else(
                    || AppError::config(format!("reorder_cadence.{k}: expected a duration such as \"90d\"")),
                )?;
            let days = parse_duration_days(&s).map_err(|e| e.context(format!("reorder_cadence.{k}")))?;
            if days < 1.0 {
                return Err(AppError::config(format!("reorder_cadence.{k}: at least one day")));
            }
            if k == "default" {
                out.default = days;
            } else {
                out.by_key.insert(k.trim().to_lowercase(), days);
            }
        }
        Ok(out)
    }

    /// The fallback for an item of this preset.
    pub fn for_preset(&self, catalog: &Catalog, preset: Option<&str>) -> (f64, String) {
        let p = preset.map(|p| p.trim().to_lowercase());
        if let Some(d) = p.as_ref().and_then(|p| self.by_key.get(p)) {
            return (*d, format!("reorder_cadence.{}", p.as_deref().unwrap_or_default()));
        }
        if let Some(cat) = p.as_deref().and_then(|p| catalog.get(p)).map(|p| p.category.clone()) {
            if let Some(d) = self.by_key.get(&cat) {
                return (*d, format!("reorder_cadence.{cat}"));
            }
        }
        (self.default, "default cadence".into())
    }
}

// ---------------------------------------------------------------------------
// Estimates
// ---------------------------------------------------------------------------

/// What an active lot's state is now.
#[derive(Debug, Clone, PartialEq)]
pub struct LotView {
    pub storage: Storage,
    pub expiry: Option<Expiry>,
    /// Presumed used up (doesn't count on hand).
    pub used_up: bool,
    /// Past its expiry but confirmed on hand since: counts on hand, but not
    /// for recipes.
    pub expired: bool,
    /// Why it is presumed used up.
    pub why: Option<String>,
}

/// An item's type, its lots' states and its estimated on hand.
#[derive(Debug, Clone, Default)]
pub struct ItemView {
    /// The shelf-life type and how it was found.
    pub shelf_life: Option<(String, Basis)>,
    /// Active lots by id.
    pub lots: HashMap<String, LotView>,
    /// Logged quantity on hand (active lots, else the item quantity).
    pub logged: f64,
    /// Estimated on hand: logged minus lots presumed used up.
    pub on_hand: f64,
    /// Some lot was presumed used up.
    pub estimated: bool,
    /// The earliest expiry of the counted lots.
    pub expiry: Option<Expiry>,
    /// The cadence used for non-perishable lots, and where it came from.
    pub cadence: Option<(f64, String)>,
}

impl ItemView {
    pub fn basis(&self) -> &'static str {
        if self.estimated {
            "estimated"
        } else {
            "logged"
        }
    }

    /// `~2` when estimated, else `2`.
    pub fn display(&self, unit: Option<&str>) -> String {
        let q = fmt_qty(round(self.on_hand));
        let tilde = if self.estimated { "~" } else { "" };
        match unit {
            Some(u) => format!("{tilde}{q} {u}"),
            None => format!("{tilde}{q}"),
        }
    }

    pub fn lot(&self, id: &str) -> Option<&LotView> {
        self.lots.get(id)
    }

    /// The JSON fields every item record carries.
    pub fn fields(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("shelf_life", json!(self.shelf_life.as_ref().map(|s| &s.0))),
            ("shelf_life_basis", json!(self.shelf_life.as_ref().map(|s| s.1.as_str()))),
            ("on_hand", qty_json(self.on_hand)),
            ("on_hand_basis", json!(self.basis())),
            ("expires_at", json!(self.expiry.as_ref().map(|e| e.date.to_string()))),
            ("expiry_basis", json!(self.expiry.as_ref().map(|e| e.basis))),
        ]
    }
}

/// A quantity as JSON: a whole number as an integer.
pub fn qty_json(q: f64) -> Value {
    let q = round(q);
    if q.fract() == 0.0 && q.abs() < 1e15 {
        json!(q as i64)
    } else {
        json!(q)
    }
}

/// Quantities to a millionth, never `-0`.
fn round(q: f64) -> f64 {
    (q * 1e6).round() / 1e6 + 0.0
}

/// Everything estimates need besides the database rows.
pub struct Estimator {
    pub types: Types,
    pub catalog: Catalog,
    pub cadence: CadenceDefaults,
    pub today: NaiveDate,
}

impl Estimator {
    /// Shelf-life types, presets and cadences from the config file.
    pub fn load(config_path: &Path, today: NaiveDate) -> Result<Self> {
        let catalog = Catalog::load(config_path)?;
        let types = Types::load(config_path, &catalog)?;
        let text = std::fs::read_to_string(config_path).unwrap_or_default();
        let cadence = CadenceDefaults::from_config(&text).map_err(|e| e.context(config_path.display()))?;
        Ok(Self { types, catalog, cadence, today })
    }

    pub fn for_ctx(ctx: &crate::context::Ctx) -> Result<Self> {
        Self::load(&ctx.resolved.config_path, ctx.tz.today())
    }

    /// The fallback-free cadence of an item: median gap of 3+ purchases,
    /// else the `Recurring` field (or `Avg Interval Days` with
    /// `Recurring = yes`), else the config default.
    pub fn cadence(&self, item: &Item, lots: &[&Lot], fields: Option<&BTreeMap<String, String>>) -> (f64, String) {
        let dates = purchases(item, lots);
        if dates.len() >= 3 {
            let g = median_gap(&dates);
            if g >= 1.0 {
                return (g.round(), "median of purchases".into());
            }
        }
        match recurring(fields) {
            Some(Some(d)) if d >= 1.0 => return (d.round(), "Recurring field".into()),
            Some(None) => {
                if let Some(d) = avg_interval(fields).filter(|d| *d >= 1.0) {
                    return (d.round(), "Avg Interval Days".into());
                }
            }
            _ => {}
        }
        self.cadence.for_preset(&self.catalog, item.kind_preset.as_deref())
    }

    /// The view of one item. `last_used` maps lot ids to their last logged
    /// consumption.
    pub fn view(
        &self,
        item: &Item,
        fields: &BTreeMap<String, String>,
        labels: &[String],
        lots: &[&Lot],
        tree: &Tree,
        last_used: &HashMap<String, String>,
    ) -> ItemView {
        let food_place = Storage::of_location(tree, item.location_id).is_some();
        let facts = ItemFacts { item, fields, labels, has_lots: !lots.is_empty(), food_place };
        let class = self.types.classify(&facts, &self.catalog);
        let mut v = ItemView { shelf_life: class.map(|c| (c.ty.name.clone(), c.basis)), ..Default::default() };
        if lots.is_empty() {
            v.logged = item.quantity as f64;
            v.on_hand = v.logged;
            return v;
        }
        let ty = class.map(|c| c.ty);
        let today = self.today;
        let mut cadence: Option<(f64, String)> = None;
        for l in lots.iter().filter(|l| l.is_active() && l.qty > 0.0) {
            v.logged += l.qty;
            let storage = Storage::of_location(tree, l.location_id.or(item.location_id)).unwrap_or(Storage::Pantry);
            let expiry = crate::shelf_life::expiry(ty, l, storage);
            let confirmed = l.confirmed_at.as_deref().and_then(|d| date_of(d).ok());
            let mut lv = LotView { storage, expiry: expiry.clone(), used_up: false, expired: false, why: None };
            match &expiry {
                Some(e) if e.date < today => {
                    if confirmed.is_some_and(|c| c > e.date) {
                        lv.expired = true;
                    } else {
                        lv.used_up = true;
                        lv.why = Some(format!("past its expiry {} ({})", e.date, e.detail));
                    }
                }
                Some(_) => {}
                None => {
                    let (every, from) = cadence.get_or_insert_with(|| self.cadence(item, lots, Some(fields))).clone();
                    let last = [l.acquired_day(), last_used.get(&l.id).and_then(|d| date_of(d).ok()), confirmed]
                        .into_iter()
                        .flatten()
                        .max();
                    if let Some(last) = last {
                        let age = (today - last).num_days();
                        if age as f64 > every {
                            lv.used_up = true;
                            lv.why = Some(format!(
                                "{age} days since {last}, longer than its ~{every} day reorder cadence ({from})"
                            ));
                        }
                    }
                }
            }
            if lv.used_up {
                v.estimated = true;
            } else {
                v.on_hand += l.qty;
                if let Some(e) = &lv.expiry {
                    if v.expiry.as_ref().is_none_or(|x| e.date < x.date) {
                        v.expiry = Some(e.clone());
                    }
                }
            }
            v.lots.insert(l.id.clone(), lv);
        }
        v.cadence = cadence;
        v.logged = round(v.logged);
        v.on_hand = round(v.on_hand);
        v
    }

    /// Views of every item of a database, by item id.
    pub fn all(&self, db: &Db) -> Result<Estimates> {
        let items = store::all_items(db)?;
        let lots = store::all_lots(db)?;
        let fields = store::all_item_fields(db)?;
        let labels = store::all_item_labels(db)?;
        let tree = Tree::load(db)?;
        let last_used = store::last_consumption(db)?;
        Ok(self.of(&items, &lots, &fields, &labels, &tree, &last_used))
    }

    /// Views of these items.
    pub fn of(
        &self,
        items: &[Item],
        lots: &[Lot],
        fields: &HashMap<String, BTreeMap<String, String>>,
        labels: &HashMap<String, Vec<String>>,
        tree: &Tree,
        last_used: &HashMap<String, String>,
    ) -> Estimates {
        let mut by_item: HashMap<&str, Vec<&Lot>> = HashMap::new();
        for l in lots {
            by_item.entry(l.item_id.as_str()).or_default().push(l);
        }
        let none = BTreeMap::new();
        let views = items
            .iter()
            .map(|i| {
                let mine = by_item.get(i.id.as_str()).map_or(&[][..], Vec::as_slice);
                let f = fields.get(&i.id).unwrap_or(&none);
                let l = labels.get(&i.id).map_or(&[][..], Vec::as_slice);
                (i.id.clone(), self.view(i, f, l, mine, tree, last_used))
            })
            .collect();
        Estimates { views }
    }

    /// The built-in types and presets with default cadences (tests).
    pub fn builtin(today: NaiveDate) -> Self {
        Self { types: Types::builtin(), catalog: Catalog::builtin(), cadence: CadenceDefaults::default(), today }
    }
}

/// Item views by item id.
#[derive(Debug, Clone, Default)]
pub struct Estimates {
    pub views: HashMap<String, ItemView>,
}

impl Estimates {
    pub fn get(&self, item_id: &str) -> Option<&ItemView> {
        self.views.get(item_id)
    }

    pub fn lot(&self, l: &Lot) -> Option<&LotView> {
        self.get(&l.item_id)?.lot(&l.id)
    }
}

/// JSON fields of a lot's state.
pub fn lot_fields(v: Option<&LotView>) -> Vec<(&'static str, Value)> {
    let e = v.and_then(|v| v.expiry.as_ref());
    vec![
        ("expires_at", json!(e.map(|e| e.date.to_string()))),
        ("expiry_basis", json!(e.map(|e| e.basis))),
        ("expiry_detail", json!(e.map(|e| e.detail.clone()))),
        ("storage", json!(v.map(|v| v.storage.as_str()))),
        ("used_up", json!(v.is_some_and(|v| v.used_up))),
        ("used_up_why", json!(v.and_then(|v| v.why.clone()))),
        ("on_hand_basis", json!(if v.is_some_and(|v| v.used_up) { "estimated" } else { "logged" })),
    ]
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

    fn est(today: &str) -> Estimator {
        Estimator {
            types: Types::builtin(),
            catalog: Catalog::builtin(),
            cadence: CadenceDefaults::default(),
            today: d(today),
        }
    }

    fn view(e: &Estimator, i: &Item, lots: &[Lot], used: &HashMap<String, String>) -> ItemView {
        let refs: Vec<&Lot> = lots.iter().collect();
        let labels = vec!["food".to_string()];
        e.view(i, &BTreeMap::new(), &labels, &refs, &Tree::default(), used)
    }

    #[test]
    fn canned_tuna_bought_twelve_times_over_two_years() {
        // every ~2 months for two years; only the last purchase is still on hand
        let dates = [
            "2024-01-10",
            "2024-03-12",
            "2024-05-10",
            "2024-07-11",
            "2024-09-09",
            "2024-11-12",
            "2025-01-10",
            "2025-03-11",
            "2025-05-12",
            "2025-07-10",
            "2025-09-10",
            "2025-11-12",
        ];
        let lots: Vec<Lot> = dates.iter().enumerate().map(|(n, a)| lot(&format!("L{n}"), a, 6.0)).collect();
        let tuna = item("Albacore Tuna in Water, 5 oz Can");
        let e = est("2026-01-01");
        let v = view(&e, &tuna, &lots, &HashMap::new());
        assert_eq!(v.shelf_life, None, "canned tuna is not perishable");
        assert_eq!(v.logged, 72.0);
        assert_eq!((v.on_hand, v.estimated), (6.0, true));
        assert_eq!(v.cadence.as_ref().map(|c| c.0), Some(62.0));
        assert!(v.lot("L10").unwrap().used_up && !v.lot("L11").unwrap().used_up);
        assert!(v.lot("L10").unwrap().why.as_deref().unwrap().contains("reorder cadence"));
        assert_eq!(v.display(Some("ea")), "~6 ea");
    }

    #[test]
    fn logs_and_confirmations_reset_the_presumption() {
        let e = est("2026-01-01");
        let towels = item("Paper Towels");
        // one purchase: the 90-day default cadence
        let old = lot("A", "2025-08-01", 2.0);
        assert!(view(&e, &towels, std::slice::from_ref(&old), &HashMap::new()).lot("A").unwrap().used_up);
        // used from it in December: still there
        let used = HashMap::from([("A".to_string(), "2025-12-20T10:00:00Z".to_string())]);
        let v = view(&e, &towels, std::slice::from_ref(&old), &used);
        assert_eq!((v.on_hand, v.estimated), (2.0, false));
        // confirmed on hand
        let confirmed = Lot { confirmed_at: Some("2025-12-30".into()), ..old.clone() };
        assert!(!view(&e, &towels, &[confirmed], &HashMap::new()).estimated);
        // a configured default per preset category
        let mut e2 = est("2026-01-01");
        e2.cadence =
            CadenceDefaults::from_config("[reorder_cadence]\ndefault = \"30d\"\nhousehold = \"200d\"\n").unwrap();
        let mut soap = item("Dish Soap");
        soap.kind_preset = Some("household".into());
        assert!(!view(&e2, &soap, std::slice::from_ref(&old), &HashMap::new()).estimated, "200 days");
        assert!(view(&e2, &towels, &[lot("B", "2025-11-15", 1.0)], &HashMap::new()).estimated, "30 days");
    }

    #[test]
    fn perishables_are_used_up_after_their_expiry() {
        let e = est("2026-01-10");
        let milk = item("Whole Milk, 1 gal");
        let v = view(&e, &milk, &[lot("old", "2026-01-01", 1.0), lot("new", "2026-01-08", 1.0)], &HashMap::new());
        assert_eq!(v.shelf_life.as_ref().map(|s| s.0.as_str()), Some("milk"));
        // pantry (no location): milk has no pantry figure, so the fridge's 7 days
        assert!(v.lot("old").unwrap().used_up);
        assert_eq!((v.on_hand, v.basis()), (1.0, "estimated"));
        assert_eq!(v.expiry.as_ref().unwrap().date, d("2026-01-15"));
        // confirmed after its expiry: on hand, but expired
        let kept = Lot { confirmed_at: Some("2026-01-09".into()), ..lot("old", "2026-01-01", 1.0) };
        let v = view(&e, &milk, &[kept], &HashMap::new());
        let lv = v.lot("old").unwrap();
        assert!(lv.expired && !lv.used_up);
        assert_eq!(v.on_hand, 1.0);
    }
}
