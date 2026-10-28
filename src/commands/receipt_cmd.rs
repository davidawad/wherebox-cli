//! `receipt import|apply|ls|show`: grocery receipts into lots (docs/SPEC.md,
//! "Receipts"; the parser is [`crate::receipt`], OCR [`crate::ocr`],
//! matching and applying [`crate::intake`]).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{ReceiptCmd, ReceiptImportArgs};
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::intake::{self, Applied, ApplyOpts, Head, Matcher, Review, StoreGuess};
use crate::ocr::{self, Ocr};
use crate::output::{to_record, Record, Report};
use crate::prompt::Prompter;
use crate::receipt;
use crate::shelf_life::Types;
use crate::sourcing;
use crate::store;
use crate::util::{fmt_qty, money_value, parse_day, parse_qty};

pub fn run(ctx: &Ctx, cmd: ReceiptCmd) -> Result<()> {
    match cmd {
        ReceiptCmd::Import(a) => import(ctx, &a, &ocr::System, &mut crate::prompt::Terminal),
        ReceiptCmd::Apply { file, dry_run, force, at } => apply_file(ctx, &file, dry_run, force, at.as_deref()),
        ReceiptCmd::Ls => ls(ctx),
        ReceiptCmd::Show { receipt, text, extract_image } => show(ctx, &receipt, text, extract_image.as_deref()),
    }
}

fn today(ctx: &Ctx) -> String {
    ctx.tz.today().format("%Y-%m-%d").to_string()
}

fn threshold(ctx: &Ctx) -> f64 {
    ctx.resolved.get("match_threshold").parse().unwrap_or(0.82)
}

const REVIEW_TABLE: &[&str] = &["n", "text", "qty", "unit", "total", "action", "item", "tier", "confidence"];

/// The rows of a review (with what was applied, if anything).
fn review_rows(file: &str, review: &Review, done: Option<&Applied>) -> Vec<Record> {
    review
        .lines
        .iter()
        .map(|l| {
            let applied = done.and_then(|d| d.lines.iter().find(|a| a.n == l.n));
            let action = if l.voided { "void" } else { l.action.as_str() };
            let item = applied.map(|a| a.item.clone()).or_else(|| match l.action.as_str() {
                "new" => l.new_name.clone(),
                "add" => l.item_name.clone().or_else(|| l.item.clone()),
                _ => None,
            });
            to_record(&json!({
                "file": file,
                "n": l.n,
                "text": l.text,
                "name": l.name,
                "code": l.code,
                "code_kind": l.code_kind,
                "qty": l.qty,
                "unit": l.unit,
                "size": l.size,
                "unit_price": l.unit_price.map(money_value),
                "total": money_value(l.total),
                "discount": money_value(l.discount),
                "voided": l.voided,
                "action": action,
                "item": item,
                "item_id": applied.map(|a| a.item_id.clone()).or_else(|| l.item.clone().filter(|_| l.action == "add")),
                "tier": l.tier,
                "confidence": l.confidence,
                "shelf_life": l.shelf_life,
                "lot": applied.map(|a| a.lot.clone()),
                "lot_qty": applied.map(|a| a.qty),
                "lot_unit": applied.map(|a| a.unit.clone()),
            }))
        })
        .collect()
}

fn head_json(
    file: &str,
    review: &Review,
    done: Option<&Applied>,
    review_file: Option<&Path>,
    below: &[usize],
) -> Value {
    let h = &review.receipt;
    json!({
        "file": file,
        "kind": h.kind,
        "receipt_id": done.map(|d| d.receipt_id.clone()),
        "fingerprint": h.fingerprint,
        "chain": h.chain,
        "store": h.store,
        "date": h.date,
        "time": h.time,
        "subtotal": h.subtotal.map(money_value),
        "tax": h.tax.map(money_value),
        "total": h.total.map(money_value),
        "review_file": review_file.map(|p| p.display().to_string()),
        "lots": done.map_or(0, |d| d.lines.len()),
        "items_created": done.map_or(0, |d| d.items_created),
        "products_created": done.map_or(0, |d| d.products_created),
        "products_updated": done.map_or(0, |d| d.products_updated),
        "below_threshold": below,
        "skipped": review.skipped,
        "warnings": h.warnings.iter().chain(done.map(|d| d.warnings.iter()).into_iter().flatten()).collect::<Vec<_>>(),
    })
}

/// Pick one of several stores on a terminal.
fn pick_store(p: &mut dyn Prompter, g: &mut StoreGuess) -> Result<()> {
    p.notice(&format!("Which {} store is this receipt from?", g.chain.as_deref().unwrap_or("")));
    for (i, s) in g.candidates.iter().enumerate() {
        p.notice(&format!(
            "  {}. {} ({}{})",
            i + 1,
            s.id,
            s.name,
            s.address.as_deref().map(|a| format!(", {a}")).unwrap_or_default()
        ));
    }
    loop {
        let a = p.ask(&format!("[1-{}, empty: none]", g.candidates.len()))?;
        if a.is_empty() {
            return Ok(());
        }
        match a.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| g.candidates.get(i)) {
            Some(s) => {
                g.store = Some(s.clone());
                g.note = None;
                return Ok(());
            }
            None => p.notice("a number from the list"),
        }
    }
}

/// Confirm, edit or skip each line on a terminal.
pub fn decide(db: &Db, review: &mut Review, p: &mut dyn Prompter, types: &Types) -> Result<()> {
    let mut rest = false;
    for l in review.lines.iter_mut().filter(|l| !l.voided) {
        if rest {
            continue;
        }
        p.notice(&format!(
            "{:>3}  {}  {} {}  ${:.2}  -> {} {} ({}, {:.2})",
            l.n,
            l.text,
            fmt_qty(l.qty),
            l.unit,
            money_value(l.total),
            l.action,
            l.item_name.as_deref().unwrap_or("-"),
            l.tier.as_deref().unwrap_or("-"),
            l.confidence
        ));
        loop {
            let a = p.ask("[a]ccept, [e]dit, [n]ew item, [s]kip, [A]ccept the rest, [q]uit?")?;
            match a.as_str() {
                "" | "a" | "y" => break,
                "A" => {
                    rest = true;
                    break;
                }
                "s" => {
                    l.action = "skip".into();
                    break;
                }
                "n" => {
                    let def = l.new_name.clone().unwrap_or_else(|| l.name.clone());
                    let name = p.ask(&format!("new item name [{def}]:"))?;
                    let name = if name.is_empty() { def } else { name };
                    l.shelf_life = types.by_name(&name).map(|(t, _)| t.name.clone());
                    l.action = "new".into();
                    l.item = None;
                    l.item_name = Some(name.clone());
                    l.new_name = Some(name);
                    l.tier = Some("edited".into());
                    l.confidence = 1.0;
                    break;
                }
                "e" => {
                    let spec = p.ask("item (id, asset id or name; empty: back):")?;
                    if spec.is_empty() {
                        continue;
                    }
                    match store::resolve_item(db, &spec) {
                        Ok(i) => {
                            l.action = "add".into();
                            l.item = Some(i.id);
                            l.item_name = Some(i.name);
                            l.new_name = None;
                            l.tier = Some("edited".into());
                            l.confidence = 1.0;
                        }
                        Err(e) => {
                            p.notice(&e.message);
                            continue;
                        }
                    }
                    let q = p.ask(&format!("quantity [{}]:", fmt_qty(l.qty)))?;
                    if !q.is_empty() {
                        match parse_qty(&q) {
                            Ok(q) if q > 0.0 => l.qty = q,
                            _ => p.notice("kept the parsed quantity"),
                        }
                    }
                    break;
                }
                "q" => return Err(AppError::usage("stopped: nothing imported")),
                _ => p.notice("answer a, e, n, s, A or q"),
            }
        }
    }
    Ok(())
}

/// Apply in a transaction, rolled back on a dry run.
fn apply_tx(db: &Db, review: &Review, o: &ApplyOpts, dry_run: bool) -> Result<Applied> {
    db.begin()?;
    match intake::apply(db, review, o) {
        Ok(done) => {
            if dry_run {
                db.rollback()?;
            } else {
                db.commit()?;
            }
            Ok(done)
        }
        Err(e) => {
            let _ = db.rollback();
            Err(e)
        }
    }
}

fn summary(ctx: &Ctx, file: &str, done: &Applied, dry_run: bool) {
    ctx.info(&format!(
        "{}{file}: {} receipt {}: {} lot(s), {} new item(s), {} store product(s) new, {} updated",
        if dry_run { "dry run, nothing written: " } else { "" },
        if dry_run { "would import" } else { "imported" },
        if dry_run { "-" } else { done.receipt_id.as_str() },
        done.lines.len(),
        done.items_created,
        done.products_created,
        done.products_updated
    ));
}

pub fn import(ctx: &Ctx, a: &ReceiptImportArgs, ocr: &dyn Ocr, p: &mut dyn Prompter) -> Result<()> {
    if a.review.is_some() && a.files.len() > 1 {
        return Err(AppError::usage("--review FILE takes one receipt; without it each gets <name>.review.json"));
    }
    if !(0.0..=1.0).contains(&a.min_confidence) {
        return Err(AppError::usage("--min-confidence is between 0 and 1"));
    }
    let db = ctx.db()?;
    let est = crate::onhand::Estimator::for_ctx(ctx)?;
    let fmts = ctx.input_date_formats();
    let fmts: Vec<&str> = fmts.iter().map(String::as_str).collect();
    let date_flag = a.date.as_deref().map(|d| parse_day(d, &fmts, &ctx.tz)).transpose()?;
    let forced = a.store.as_deref().map(|s| sourcing::resolve_store(&db, s)).transpose()?;
    let profile_stores = ctx.profile.as_ref().map(|p| p.stores.clone()).unwrap_or_default();
    let interactive = p.interactive() && !a.yes && a.review.is_none() && !a.dry_run;
    let (mut rows, mut heads, mut failed) = (Vec::new(), Vec::new(), None);
    for file in &a.files {
        let name = file.display().to_string();
        let (text, kind, bytes) = ocr::read(file, ocr)?;
        let r = receipt::parse(&text, None);
        let mut g = match &forced {
            Some(s) => StoreGuess { store: Some(s.clone()), chain: Some(s.chain.clone()), ..Default::default() },
            None => intake::guess_store(&db, &r, &profile_stores)?,
        };
        if g.store.is_none() && !g.candidates.is_empty() && interactive {
            pick_store(p, &mut g)?;
        }
        if let Some(n) = g.note.as_deref().filter(|_| g.store.is_none()) {
            ctx.warn(&format!("{name}: {n}"));
        }
        let date = date_flag.clone().or_else(|| r.date.clone());
        let store_key = g.store.as_ref().map(|s| s.id.clone()).or_else(|| g.chain.clone());
        let head = Head {
            fingerprint: receipt::fingerprint(store_key.as_deref(), &r, date.as_deref()),
            file: (!a.no_path).then(|| name.clone()),
            kind: Some(kind.as_str().into()),
            chain: g.chain.clone(),
            store: g.store.as_ref().map(|s| s.id.clone()),
            date,
            time: r.time.clone(),
            subtotal: r.subtotal,
            tax: r.tax,
            total: r.total,
            keep_image: a.keep_image,
            warnings: r.warnings.clone(),
            ocr_text: text,
        };
        for w in &r.warnings {
            ctx.warn(&format!("{name}: {w}"));
        }
        let m = Matcher::load(&db, &est.types, g.store.as_ref(), g.chain.as_deref(), threshold(ctx))?;
        let mut review = Review::new(head, &r, &m);
        if a.review.is_some() || (!a.yes && !interactive && !a.dry_run) {
            let stem = file.file_stem().map_or_else(|| "receipt".into(), |s| s.to_string_lossy().into_owned());
            let path = a.review.clone().unwrap_or_else(|| PathBuf::from(format!("{stem}.review.json")));
            std::fs::write(&path, review.to_json()).map_err(|e| AppError::io(format!("{}: {e}", path.display())))?;
            ctx.info(&format!(
                "{name}: nothing imported; wrote the review to {} (edit `action`, `item`, `new_name`, `qty`, then `wherebox receipt apply {}`; or use --yes)",
                path.display(),
                path.display()
            ));
            rows.extend(review_rows(&name, &review, None));
            heads.push(head_json(&name, &review, None, Some(&path), &[]));
            continue;
        }
        let below = if interactive {
            decide(&db, &mut review, p, &est.types).map(|()| Vec::new())?
        } else {
            review.accept_above(a.min_confidence)
        };
        if !below.is_empty() {
            ctx.info(&format!(
                "{name}: skipped line(s) {} below confidence {} (use --review FILE to decide them)",
                below.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                a.min_confidence
            ));
        }
        let image = (a.keep_image && kind != ocr::Kind::Text)
            .then(|| (file.file_name().map_or_else(|| name.clone(), |f| f.to_string_lossy().into_owned()), bytes));
        let o = ApplyOpts { force: a.force, at: a.at.as_deref(), today: today(ctx), image, keep_path: !a.no_path };
        match apply_tx(&db, &review, &o, a.dry_run) {
            Ok(done) => {
                done.warnings.iter().for_each(|w| ctx.warn(&format!("{name}: {w}")));
                summary(ctx, &name, &done, a.dry_run);
                rows.extend(review_rows(&name, &review, Some(&done)));
                heads.push(head_json(&name, &review, Some(&done), None, &below));
            }
            Err(e) if a.files.len() > 1 => {
                ctx.warn(&format!("{name}: {}", e.message));
                failed.get_or_insert(e);
            }
            Err(e) => return Err(e),
        }
    }
    let report = Report::list("receipt_import", rows)
        .table_columns(REVIEW_TABLE)
        .meta("receipts", json!(heads))
        .meta("dry_run", json!(a.dry_run));
    ctx.emit(&report)?;
    failed.map_or(Ok(()), Err)
}

fn apply_file(ctx: &Ctx, file: &Path, dry_run: bool, force: bool, at: Option<&str>) -> Result<()> {
    let text = std::fs::read_to_string(file).map_err(|e| AppError::io(format!("{}: {e}", file.display())))?;
    let review = Review::parse(&text)?;
    let db = ctx.db()?;
    let h = &review.receipt;
    let image = if h.keep_image && h.kind.as_deref() != Some("text") {
        match h.file.as_deref().map(|f| (f, std::fs::read(f))) {
            Some((f, Ok(b))) => {
                Some((Path::new(f).file_name().map_or_else(|| f.to_string(), |n| n.to_string_lossy().into_owned()), b))
            }
            _ => {
                ctx.warn("keep_image: the original file is not readable; the receipt keeps its text only");
                None
            }
        }
    } else {
        None
    };
    let o = ApplyOpts { force, at, today: today(ctx), image, keep_path: h.file.is_some() };
    let done = apply_tx(&db, &review, &o, dry_run)?;
    let name = h.file.clone().unwrap_or_else(|| file.display().to_string());
    done.warnings.iter().for_each(|w| ctx.warn(&format!("{name}: {w}")));
    summary(ctx, &name, &done, dry_run);
    let report = Report::list("receipt_import", review_rows(&name, &review, Some(&done)))
        .table_columns(REVIEW_TABLE)
        .meta("receipts", json!([head_json(&name, &review, Some(&done), None, &[])]))
        .meta("dry_run", json!(dry_run));
    ctx.emit(&report)
}

fn receipt_record(r: &intake::StoredReceipt, lots: usize) -> Record {
    to_record(&json!({
        "id": r.id,
        "date": r.purchased_on,
        "time": r.purchased_time,
        "store": r.store_id,
        "chain": r.chain,
        "subtotal": r.subtotal.map(money_value),
        "tax": r.tax.map(money_value),
        "total": r.total.map(money_value),
        "lines": r.lines,
        "lots": lots,
        "file": r.source_path,
        "image": r.image_name,
        "fingerprint": r.fingerprint,
        "created_at": r.created_at,
    }))
}

fn ls(ctx: &Ctx) -> Result<()> {
    let db = ctx.db()?;
    let lots = store::all_lots(&db)?;
    let rows = intake::all_receipts(&db)?
        .iter()
        .map(|r| {
            let prefix = intake::lot_prefix(&r.id);
            let n = lots.iter().filter(|l| l.source_ref.as_deref().is_some_and(|s| s.starts_with(&prefix))).count();
            receipt_record(r, n)
        })
        .collect();
    ctx.emit(&Report::list("receipts", rows).table_columns(&["id", "date", "time", "store", "total", "lots", "file"]))
}

fn show(ctx: &Ctx, spec: &str, text: bool, extract: Option<&Path>) -> Result<()> {
    let db = ctx.db()?;
    let r = intake::resolve_receipt(&db, spec)?;
    if text {
        let mut t = r.ocr_text.clone();
        if !t.ends_with('\n') {
            t.push('\n');
        }
        return crate::output::write_stdout(t.as_bytes());
    }
    if let Some(dest) = extract {
        use base64::Engine;
        let b64 = r.image.as_deref().ok_or_else(|| {
            AppError::not_found(format!("receipt {} kept no image (import it with --keep-image)", r.id))
        })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| AppError::invalid(format!("receipt {}: kept image: {e}", r.id)))?;
        std::fs::write(dest, &bytes).map_err(|e| AppError::io(format!("{}: {e}", dest.display())))?;
        ctx.info(&format!("wrote {} ({} bytes)", dest.display(), bytes.len()));
        return Ok(());
    }
    let lots = intake::receipt_lots(&db, &r.id)?;
    let prefix = intake::lot_prefix(&r.id);
    let lot_rows: Vec<Record> = lots
        .iter()
        .map(|l| {
            let item = store::get_item(&db, &l.item_id).ok().flatten();
            to_record(&json!({
                "line": l.source_ref.as_deref().and_then(|s| s.strip_prefix(&prefix)).and_then(|n| n.parse::<usize>().ok()),
                "lot": l.id,
                "item": item.as_ref().map(|i| i.name.clone()),
                "asset_id": item.as_ref().and_then(|i| i.asset_id.clone()),
                "item_id": l.item_id,
                "qty": l.qty,
                "unit": item.as_ref().and_then(|i| i.unit.clone()),
                "unit_price": l.unit_price.map(money_value),
                "status": l.status,
                "acquired_at": l.acquired_at,
                "vendor": l.vendor,
            }))
        })
        .collect();
    let rec = receipt_record(&r, lots.len());
    if ctx.out.format == crate::output::Format::Table {
        ctx.emit(&Report::object("receipt", rec))?;
        return ctx.emit(&Report::list("receipt_lots", lot_rows).table_columns(&[
            "line",
            "item",
            "qty",
            "unit",
            "unit_price",
            "status",
            "lot",
        ]));
    }
    ctx.emit(&Report::object("receipt", rec).meta("lots", json!(lot_rows)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Scripted;

    #[test]
    fn interactive_review_accepts_edits_and_skips() {
        let t = tempfile::TempDir::new().unwrap();
        let db = Db::open_plain(&t.path().join("r.db")).unwrap();
        crate::migrations::migrate(&db).unwrap();
        let milk = store::Item::new("Whole Milk").unwrap();
        store::insert_item(&db, &milk).unwrap();
        let types = Types::builtin();
        let r = receipt::parse(include_str!("../../tests/fixtures/receipts/aldi.txt"), None);
        let m = Matcher::load(&db, &types, None, r.chain.as_deref(), 0.82).unwrap();
        let mut review = Review::new(Head { fingerprint: "f".into(), ..Default::default() }, &r, &m);
        // milk: edit to the item with 2 of them; spinach: skip; eggs: new name; the rest: accept
        let answers = "e\nWhole Milk\n2\ns\nn\nBrown Eggs\nA\n";
        let mut p = Scripted { input: answers.as_bytes(), output: Vec::new(), interactive: true };
        decide(&db, &mut review, &mut p, &types).unwrap();
        let by = |n: usize| review.lines.iter().find(|l| l.n == n).unwrap();
        assert_eq!((by(6).action.as_str(), by(6).item.as_deref(), by(6).qty), ("add", Some(milk.id.as_str()), 2.0));
        assert_eq!(by(8).action, "skip");
        assert_eq!((by(9).action.as_str(), by(9).new_name.as_deref()), ("new", Some("Brown Eggs")));
        assert_eq!(by(9).shelf_life.as_deref(), Some("eggs"));
        assert_eq!(by(14).action, "new", "accepted as proposed");
        let mut q = Scripted { input: "q\n".as_bytes(), output: Vec::new(), interactive: true };
        assert!(decide(&db, &mut review, &mut q, &types).is_err(), "quit writes nothing");
    }
}
