//! `wherebox export`: Homebox CSV (the default), or wherebox's own records
//! as csv/tsv (flat) and json/jsonl/yaml/toon (nested), all re-importable.

use serde_json::json;

use crate::cli::ExportArgs;
use crate::commands::homebox;
use crate::commands::items::{filtered, item_record, lot_record, Lookups};
use crate::context::Ctx;
use crate::error::Result;
use crate::output::{Format, Record, Report};
use crate::store::{self, Item};

/// Export order: location path, then name, then id (stable across round trips).
fn sort(items: &mut [Item], lk: &Lookups) {
    let key = |i: &Item| {
        (lk.tree.opt_path(i.location_id).unwrap_or_default().to_lowercase(), i.name.to_lowercase(), i.id.clone())
    };
    items.sort_by_key(key);
}

pub fn run(ctx: &Ctx, a: ExportArgs) -> Result<()> {
    let db = ctx.db()?;
    let lk = Lookups::for_ctx(ctx, &db)?;
    let mut filter = a.filter.clone();
    // An export is a backup: every status unless asked otherwise.
    filter.all = filter.all || filter.status.is_empty();
    let mut items = filtered(ctx, &db, &lk, &filter)?;
    sort(&mut items, &lk);
    let lots = store::all_lots(&db)?;
    let mut out = ctx.out.clone();
    // Missing values are always empty cells so exports re-import cleanly.
    out.null = String::new();
    if out.format == Format::Table {
        out.format = Format::Homebox;
    }
    let report = if out.format == Format::Homebox {
        let all = store::all_items(&db)?;
        let (header, rows) = homebox::export_rows(&items, &lk, &lots, &all);
        let records: Vec<Record> = rows
            .into_iter()
            .map(|cells| header.iter().cloned().zip(cells.into_iter().map(serde_json::Value::String)).collect())
            .collect();
        if out.columns.is_none() {
            out.columns = Some(header);
        }
        Report::list("export", records).exact()
    } else {
        let flat = !out.format.is_envelope() && out.format != Format::Jsonl;
        let records = items
            .iter()
            .map(|i| {
                let mut r = item_record(i, &lk);
                let mine: Vec<Record> = lots
                    .iter()
                    .filter(|l| l.item_id == i.id)
                    .map(|l| lot_record(l, None, &lk.tree, lk.est.lot(l)))
                    .map(|mut l| {
                        l.remove("unit");
                        l
                    })
                    .collect();
                if flat {
                    let fields = r.get("fields").cloned().unwrap_or_default();
                    r.insert(
                        "fields".into(),
                        json!(if fields == json!({}) { String::new() } else { fields.to_string() }),
                    );
                    r.insert(
                        "lots".into(),
                        json!(if mine.is_empty() { String::new() } else { json!(mine).to_string() }),
                    );
                } else {
                    r.insert("lots".into(), json!(mine));
                }
                r
            })
            .collect();
        Report::list("export", records).exact().meta("profile", json!(ctx.profile.as_ref().map(|p| &p.name)))
    };
    if !ctx.quiet() && out.output.is_some() {
        eprintln!("exported {} item(s)", items.len());
    }
    ctx.emit_with(&report, &out)
}
