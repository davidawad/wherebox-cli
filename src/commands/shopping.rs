//! `par` and `shopping`: what to buy.
//!
//! The list has three parts, per property:
//!
//! 1. **below par**: the estimated on hand (lots not presumed used up, else
//!    the item quantity; see [`crate::onhand`]) is less than `par_level`;
//!    buy `reorder_qty`, else up to par.
//! 2. **expiring**: par items with perishable lots expiring (computed
//!    expiry) within the window (default the `expiring_within` setting) that
//!    would leave less than par on hand.
//! 3. **reorder**: learned cadence. Purchases are the item's lot acquisition
//!    dates (plus its purchase date). With three or more, the cadence is the
//!    median gap between them. With fewer, a `Recurring` custom field
//!    (`HB.field.Recurring`) gives it (`9d`, `2w`, `weekly`), or when just
//!    `yes`/`true` the `Avg Interval Days` field, else the average gap of two
//!    purchases. The item is due once the cadence has passed since the last
//!    purchase, unless it has an unused lot acquired within the last cadence.
//!    Items last bought before the `--since` cutoff (default 18 months) are
//!    dropped, not due.
//! 4. **list** (`--include-list`): what a list still needs (`list shop`).

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use serde_json::json;

use crate::cli::{ParArgs, ShoppingArgs};
use crate::context::Ctx;
use crate::db::Db;
use crate::error::Result;
use crate::onhand::{learned_cadence, purchases, ItemEst};
use crate::output::{to_record, Format, Record, Report};
use crate::profiles::Profile;
use crate::store::{self, Item, Lot, Tree};
use crate::util::{date_before, date_of, fmt_qty, parse_duration_days, parse_qty, parse_span, Span};

pub fn par(ctx: &Ctx, a: ParArgs) -> Result<()> {
    let db = ctx.db()?;
    let before = store::resolve_item(&db, &a.item)?;
    let mut item = before.clone();
    let clear = |s: &str| matches!(s.trim().to_ascii_lowercase().as_str(), "" | "none" | "off");
    let qty = |s: &str| -> Result<Option<f64>> {
        if clear(s) {
            Ok(None)
        } else {
            Ok(Some(parse_qty(s)?).filter(|q| *q > 0.0))
        }
    };
    item.par_level = qty(&a.level)?;
    if let Some(r) = &a.reorder {
        item.reorder_qty = qty(r)?;
    }
    let tree = Tree::load(&db)?;
    if !item.same_content(&before) {
        item.updated_at = crate::util::now_iso();
        db.transaction(|db| {
            store::update_item(db, &item)?;
            crate::commands::items::record_changes(db, &tree, &before, &item, "par")
        })?;
    }
    let unit = item.unit.as_deref().unwrap_or("ea");
    ctx.info(&match item.par_level {
        Some(p) => format!("par level of {}: {} {unit}", item.name, fmt_qty(p)),
        None => format!("{} has no par level", item.name),
    });
    let lk = crate::commands::items::Lookups::load(ctx, &db)?;
    ctx.emit_mutation(&Report::object("item", crate::commands::items::item_record(&item, &lk)))
}

/// Quantity on hand: the estimate, else active lots when the item tracks
/// lots, else its quantity; and its text (`~2` when estimated).
fn on_hand(item: &Item, lots: &[&Lot], est: Option<&ItemEst>) -> (f64, String) {
    match est {
        Some(e) => (e.on_hand, e.on_hand_text()),
        None if lots.is_empty() => (item.quantity as f64, item.quantity.to_string()),
        None => {
            let q = lots.iter().filter(|l| l.is_active()).map(|l| l.qty).sum();
            (q, fmt_qty(q))
        }
    }
}

/// When a lot expires: its estimate's computed expiry, else a printed best-by.
fn expires(l: &Lot, est: Option<&ItemEst>) -> Option<String> {
    match est {
        Some(e) => e.lot(&l.id)?.expiry.as_ref().map(|x| x.date.to_string()),
        None => l.best_by.clone(),
    }
}

struct Line<'a> {
    item: &'a Item,
    qty: f64,
    reason: &'static str,
    detail: String,
    /// Reorders: last purchase, cadence in days and days past it.
    last: Option<NaiveDate>,
    cadence: Option<f64>,
    overdue: Option<i64>,
}

impl<'a> Line<'a> {
    fn new(item: &'a Item, qty: f64, reason: &'static str, detail: String) -> Self {
        Self { item, qty, reason, detail, last: None, cadence: None, overdue: None }
    }
}

/// What to buy, and how many recurring items were dropped (last bought
/// before `cutoff`).
fn needs<'a>(
    items: &'a [Item],
    lots: &[Lot],
    fields: &BTreeMap<String, BTreeMap<String, String>>,
    est: &HashMap<String, ItemEst>,
    within: i64,
    today: NaiveDate,
    cutoff: NaiveDate,
) -> (Vec<Line<'a>>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0;
    for item in items.iter().filter(|i| !i.is_gone()) {
        let mine: Vec<&Lot> = lots.iter().filter(|l| l.item_id == item.id).collect();
        let e = est.get(&item.id);
        let counts = |l: &Lot| e.map_or(l.is_active() && l.qty > 0.0, |e| e.counts(l));
        let (have, have_text) = on_hand(item, &mine, e);
        let unit = item.unit.as_deref().unwrap_or("ea");
        if let Some(par) = item.par_level {
            if have < par {
                out.push(Line::new(
                    item,
                    item.reorder_qty.unwrap_or(par - have),
                    "below par",
                    format!("{have_text} of {} {unit} on hand", fmt_qty(par)),
                ));
                continue;
            }
            let due: Vec<(&Lot, String, i64)> = mine
                .iter()
                .filter(|l| counts(l))
                .filter_map(|l| {
                    let d = expires(l, e)?;
                    let left = crate::util::days_until(&d, today)?;
                    Some((*l, d, left))
                })
                .filter(|(_, _, d)| *d <= within)
                .collect();
            let going: f64 = due.iter().map(|(l, _, _)| l.qty).sum();
            if !due.is_empty() && have - going < par {
                let first = due.iter().map(|(_, d, _)| d.clone()).min().unwrap_or_default();
                out.push(Line::new(
                    item,
                    item.reorder_qty.unwrap_or(par - (have - going)),
                    "expiring",
                    format!(
                        "{} {unit} due by {first}; {} of {} left after",
                        fmt_qty(going),
                        fmt_qty(have - going),
                        fmt_qty(par)
                    ),
                ));
                continue;
            }
        }
        let f = fields.get(&item.id);
        // A durable bought more than once (an exchange, a replacement) is
        // not a reorder: learned cadence applies to consumables only.
        let durable = f.and_then(|f| f.get("Kind")).is_some_and(|k| k.trim().eq_ignore_ascii_case("durable"));
        if durable && mine.is_empty() {
            continue;
        }
        let dates = purchases(item, &mine);
        let Some((every, from)) = learned_cadence(item, &mine, f) else { continue };
        let last = *dates.last().expect("interval needs a purchase");
        if last < cutoff {
            dropped += 1;
            continue;
        }
        let since = (today - last).num_days();
        let window_start = today - chrono::Duration::days(every as i64);
        let stocked = mine.iter().any(|l| {
            let got = l.acquired_at.as_deref().or(Some(l.created_at.as_str())).and_then(|d| date_of(d).ok());
            counts(l) && got.is_some_and(|d| d > window_start)
        });
        if since as f64 >= every && !stocked {
            let last_qty = mine.iter().filter(|l| l.acquired_at.is_some()).max_by_key(|l| l.fifo_key()).map(|l| l.qty);
            let overdue = since - every as i64;
            out.push(Line {
                last: Some(last),
                cadence: Some(every),
                overdue: Some(overdue),
                ..Line::new(
                    item,
                    item.reorder_qty.or(last_qty).filter(|q| *q > 0.0).unwrap_or(1.0),
                    "reorder",
                    format!("bought every ~{every} days ({from}); last {last}, {overdue} days overdue"),
                )
            });
        }
    }
    (out, dropped)
}

fn rows_for(
    ctx: &Ctx,
    profile: Option<&Profile>,
    db: &Db,
    within: i64,
    since: Span,
    dropped: &mut usize,
) -> Result<Vec<Record>> {
    let items = store::all_items(db)?;
    let lots = store::all_lots(db)?;
    let fields: BTreeMap<String, BTreeMap<String, String>> = store::all_item_fields(db)?.into_iter().collect();
    let tree = Tree::load(db)?;
    let today = ctx.tz.today();
    let cfg = crate::onhand::Config::load(ctx)?;
    let est = crate::onhand::Estimator::load(&cfg, db, today)?.all(&items, &lots);
    let (mut lines, gone) = needs(&items, &lots, &fields, &est, within, today, date_before(today, since));
    *dropped += gone;
    let order = |r: &str| ["below par", "expiring", "reorder"].iter().position(|x| *x == r).unwrap_or(9);
    lines.sort_by(|a, b| {
        order(a.reason).cmp(&order(b.reason)).then_with(|| a.item.name.to_lowercase().cmp(&b.item.name.to_lowercase()))
    });
    Ok(lines
        .iter()
        .map(|l| {
            to_record(&json!({
                "item": l.item.name,
                "qty": l.qty,
                "unit": l.item.unit.as_deref().unwrap_or("ea"),
                "vendor": l.item.preferred_vendor.as_ref().or(l.item.purchase_from.as_ref()),
                "product_ref": l.item.product_ref,
                "reason": l.reason,
                "detail": l.detail,
                "last_bought": l.last.map(|d| d.to_string()),
                "cadence_days": l.cadence.map(|c| c as i64),
                "overdue_days": l.overdue,
                "location": tree.opt_path(l.item.location_id),
                "profile": profile.map(|p| p.name.clone()),
                "asset_id": l.item.asset_id,
            }))
        })
        .collect())
}

fn checklist(rows: &[Record], titles: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut current: Option<String> = None;
    let multi = titles.len() > 1;
    for r in rows {
        let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let p = s("profile");
        if multi && current.as_deref() != Some(p.as_str()) {
            if current.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("## {}\n\n", titles.get(&p).cloned().unwrap_or_else(|| p.clone())));
            current = Some(p);
        }
        let qty = r.get("qty").and_then(serde_json::Value::as_f64).unwrap_or(1.0);
        let vendor = Some(s("vendor")).filter(|v| !v.is_empty()).map(|v| format!(" @ {v}")).unwrap_or_default();
        out.push_str(&format!(
            "- [ ] {}: {} {}{vendor} ({}: {})\n",
            s("item"),
            fmt_qty(qty),
            s("unit"),
            s("reason"),
            s("detail")
        ));
    }
    out
}

pub fn run(ctx: &Ctx, a: ShoppingArgs) -> Result<()> {
    let within = a.within.as_deref().unwrap_or(ctx.expiring_within());
    let days = parse_duration_days(within)?.round() as i64;
    // A bare number is months.
    let since_text =
        if a.since.trim().parse::<f64>().is_ok() { format!("{}m", a.since.trim()) } else { a.since.clone() };
    let since = parse_span(&since_text)?;
    let mut dropped = 0;
    let mut titles = BTreeMap::new();
    let lists = crate::commands::list_cmd::load_lists(ctx, &a.include_lists)?;
    let rows: Vec<Record> = crate::commands::profile::each_db(ctx, a.scope.all_profiles, "shopping", |c, p, db| {
        if let Some(p) = p {
            titles.insert(p.name.clone(), p.title.clone());
        }
        let mut rows = rows_for(c, p, db, days, since, &mut dropped)?;
        for l in &lists {
            rows.extend(crate::commands::list_cmd::list_shopping_rows(c, p, db, l)?);
        }
        Ok(rows)
    })?
    .into_iter()
    .flatten()
    .collect();
    if rows.is_empty() {
        ctx.info("nothing to buy");
    }
    if dropped > 0 {
        ctx.info(&format!("{dropped} recurring item(s) last bought before the --since {} cutoff: dropped", a.since));
    }
    let envelope = ctx.json_flag || ctx.out.format.is_envelope() || ctx.out.format == Format::Jsonl;
    if a.checklist && !envelope {
        return ctx.write_output(checklist(&rows, &titles).as_bytes(), &ctx.out);
    }
    let mut cols =
        vec!["item", "qty", "unit", "reason", "last_bought", "cadence_days", "overdue_days", "detail", "vendor"];
    if a.scope.all_profiles {
        cols.insert(0, "profile");
    }
    ctx.emit(
        &Report::list("shopping", rows)
            .table_columns(&cols)
            .meta("within_days", json!(days))
            .meta("dropped", json!(dropped)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: NaiveDate = NaiveDate::MIN;

    fn none_est() -> HashMap<String, ItemEst> {
        HashMap::new()
    }

    fn item(id: &str) -> Item {
        Item { id: id.into(), name: id.into(), quantity: 0, status: "owned".into(), ..Default::default() }
    }

    fn lot(item: &str, acquired: &str, qty: f64, status: &str) -> Lot {
        Lot {
            id: format!("{item}{acquired}"),
            item_id: item.into(),
            qty,
            acquired_at: Some(acquired.into()),
            status: status.into(),
            ..Default::default()
        }
    }

    #[test]
    fn learns_reorders_from_history_or_the_recurring_field() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        let lots = vec![
            // coffee every ~9 days, last 2026-02-18: 11 days ago, due
            lot("coffee", "2026-02-01", 0.0, "consumed"),
            lot("coffee", "2026-02-09", 0.0, "consumed"),
            lot("coffee", "2026-02-18", 1.0, "open"),
            // salt bought twice: no cadence without a Recurring field
            lot("salt", "2025-01-01", 0.0, "consumed"),
            lot("salt", "2026-01-01", 1.0, "open"),
        ];
        let mut fields = BTreeMap::new();
        let mut filters = item("filters");
        filters.purchase_time = Some("2025-12-15".into());
        let items = [item("coffee"), filters, item("salt")];
        fields.insert("filters".to_string(), BTreeMap::from([("Recurring".to_string(), "3m".to_string())]));
        let out = needs(&items, &lots, &fields, &none_est(), 7, today, OLD).0;
        let names: Vec<&str> = out.iter().map(|l| l.item.name.as_str()).collect();
        assert_eq!(names, ["coffee"], "filters not yet due (3 months from 2025-12-15)");
        assert!(out[0].detail.contains("every ~9 days"), "{}", out[0].detail);
        let later = NaiveDate::from_ymd_opt(2026, 2, 2).unwrap();
        assert!(needs(&items, &lots[..2], &fields, &none_est(), 7, later, OLD).0.is_empty());
        let june = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let out = needs(&items, &[], &fields, &none_est(), 7, june, OLD).0;
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].item.name.as_str(), out[0].reason), ("filters", "reorder"));
        fields.insert("salt".to_string(), BTreeMap::from([("Recurring".to_string(), "yes".to_string())]));
        // two purchases a year apart: not due until a year after the last
        assert!(!needs(&items, &lots, &fields, &none_est(), 7, june, OLD).0.iter().any(|l| l.item.name == "salt"));
        let next_year = NaiveDate::from_ymd_opt(2027, 1, 2).unwrap();
        assert!(needs(&items, &lots, &fields, &none_est(), 7, next_year, OLD).0.iter().any(|l| l.item.name == "salt"));
    }

    #[test]
    fn median_cadence_overdue_stock_and_cutoff() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let today = d("2026-10-23");
        // paper towels: gaps 30, 40, 200 -> median 40; last 2026-08-28, 56 days ago
        let lots = vec![
            lot("towels", "2025-12-31", 0.0, "consumed"),
            lot("towels", "2026-01-30", 0.0, "consumed"),
            lot("towels", "2026-03-11", 0.0, "consumed"),
            lot("towels", "2026-08-28", 2.0, "sealed"),
        ];
        let items = [item("towels")];
        let none = BTreeMap::new();
        let (out, dropped) = needs(&items, &lots, &none, &none_est(), 7, today, OLD);
        assert_eq!(dropped, 0);
        assert_eq!((out[0].cadence, out[0].overdue, out[0].last), (Some(40.0), Some(16), Some(d("2026-08-28"))));
        assert!(out[0].detail.contains("every ~40 days (median of purchases)"), "{}", out[0].detail);
        assert_eq!(out[0].qty, 2.0, "the last purchase's quantity");
        // a fresh unused lot inside the last cadence window: stocked, not due
        let mut more = lots.clone();
        let mut fresh = lot("towels", "2026-08-28", 1.0, "sealed");
        fresh.id = "fresh".into();
        fresh.acquired_at = None;
        fresh.created_at = "2026-10-01T00:00:00Z".into();
        more.push(fresh);
        assert!(needs(&items, &more, &none, &none_est(), 7, today, OLD).0.is_empty());
        // last bought before the cutoff: dropped, not due
        let (out, dropped) = needs(&items, &lots, &none, &none_est(), 7, today, d("2026-09-01"));
        assert!(out.is_empty());
        assert_eq!(dropped, 1);
        // no lot history: Recurring=true with Avg Interval Days
        let mut soap = item("soap");
        soap.purchase_time = Some("2026-09-01".into());
        let f = BTreeMap::from([(
            "soap".to_string(),
            BTreeMap::from([
                ("Recurring".to_string(), "true".to_string()),
                ("Avg Interval Days".to_string(), "45.5".to_string()),
            ]),
        )]);
        let soap = [soap];
        assert!(needs(&soap, &[], &f, &none_est(), 7, d("2026-10-10"), OLD).0.is_empty(), "46 days not yet passed");
        let (out, _) = needs(&soap, &[], &f, &none_est(), 7, d("2026-10-20"), OLD);
        assert_eq!((out[0].cadence, out[0].overdue), (Some(46.0), Some(3)));
        assert!(out[0].detail.contains("(Avg Interval Days)"), "{}", out[0].detail);
        // the same, but Kind=durable (shoes bought three times): never a reorder
        let mut shoes = item("shoes");
        shoes.purchase_time = Some("2026-09-01".into());
        let f = BTreeMap::from([(
            "shoes".to_string(),
            BTreeMap::from([
                ("Kind".to_string(), "durable".to_string()),
                ("Recurring".to_string(), "true".to_string()),
                ("Avg Interval Days".to_string(), "5.5".to_string()),
            ]),
        )]);
        assert!(needs(&[shoes], &[], &f, &none_est(), 7, d("2026-10-20"), OLD).0.is_empty());
    }

    #[test]
    fn par_and_expiring() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        let mut milk = item("milk");
        milk.par_level = Some(2.0);
        let mut eggs = item("eggs");
        eggs.par_level = Some(1.0);
        eggs.reorder_qty = Some(12.0);
        let mut l = lot("eggs", "2026-02-20", 6.0, "sealed");
        l.best_by = Some("2026-03-03".into());
        let lots = vec![lot("milk", "2026-02-28", 1.0, "sealed"), l];
        let (items, none) = ([milk, eggs], BTreeMap::new());
        let out = needs(&items, &lots, &none, &none_est(), 7, today, OLD).0;
        assert_eq!((out[0].reason, out[0].qty), ("below par", 1.0));
        assert_eq!((out[1].reason, out[1].qty), ("expiring", 12.0));
        assert!(needs(&[], &lots, &none, &none_est(), 1, today, OLD).0.is_empty());
    }
}
