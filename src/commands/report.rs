//! `report insurance` and `report warranties`.
//!
//! The insurance report lists every owned durable (not gone, not a
//! consumable: no lots, and not a consumable by `Kind` field, preset or
//! label, see [`crate::consumable`]) with its location path, quantity,
//! maker/model/serial, purchase date/vendor/price and replacement value. The
//! unit value is the replacement value, else the purchase price (the line is
//! then flagged); the line value is the unit value times the quantity.
//! `--min-value` keeps lines worth at least that much, `--bought-after`
//! items bought on or after a date. Photos and receipts are listed with
//! their paths. Lines are grouped by property and room with a subtotal per
//! room, a total per property and a grand total, followed by the gaps an
//! adjuster would ask about: no value, no photo, no receipt, no serial
//! number on serialized kinds (presets that track `serial`), and "verify
//! still owned" for items bought more than ten years ago and still marked
//! owned. A rented property's section says "contents only".
//!
//! Formats: table, csv/tsv (one row per line, `section` says which), json
//! (nested), markdown, html (a standalone document), xlsx and pdf.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::cli::{InsuranceArgs, ReportCmd, WarrantyArgs};
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::output::{html_escape, to_record, Format, Record, Report};
use crate::presets::Catalog;
use crate::profiles::Profile;
use crate::store::{self, Item, Tree};
use crate::util::{fmt_money, money_value, parse_duration_days};

pub fn run(ctx: &Ctx, cmd: ReportCmd) -> Result<()> {
    match cmd {
        ReportCmd::Insurance(a) => insurance(ctx, a),
        ReportCmd::Warranties(a) => warranties(ctx, a),
    }
}

/// One insured item.
#[derive(Debug, Clone)]
struct Entry {
    room: String,
    location: Option<String>,
    item: Item,
    /// One unit's value.
    value: Option<i64>,
    /// `value` times the quantity.
    line: Option<i64>,
    /// Bought more than ten years ago and still marked owned.
    old: bool,
    /// `replacement`, `purchase price` (flagged) or empty.
    basis: &'static str,
    photos: Vec<String>,
    receipts: Vec<String>,
    serialized: bool,
}

impl Entry {
    fn gaps(&self) -> Vec<&'static str> {
        let mut g = Vec::new();
        if self.value.is_none() {
            g.push("no value");
        }
        if self.photos.is_empty() {
            g.push("no photo");
        }
        if self.receipts.is_empty() {
            g.push("no receipt");
        }
        if self.serialized && self.item.serial_number.as_deref().is_none_or(|s| s.trim().is_empty()) {
            g.push("no serial");
        }
        if self.old {
            g.push(VERIFY_OWNED);
        }
        g
    }
}

#[derive(Debug, Clone)]
struct Property {
    name: String,
    profile: Option<String>,
    address: Option<String>,
    rented: bool,
    /// Room name -> entries, rooms in name order.
    rooms: BTreeMap<String, Vec<Entry>>,
}

const VERIFY_OWNED: &str = "verify still owned";

fn sum(entries: &[Entry]) -> i64 {
    entries.iter().filter_map(|e| e.line).sum()
}

/// Which items the report covers.
struct Filter<'a> {
    owner: Option<&'a str>,
    /// Least line value, in cents.
    min_value: Option<i64>,
    /// Earliest purchase date.
    bought_after: Option<String>,
}

impl Property {
    fn total(&self) -> i64 {
        self.rooms.values().map(|r| sum(r)).sum()
    }
    fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.rooms.values().flatten()
    }
    fn heading(&self) -> String {
        let mut h = self.name.clone();
        if let Some(a) = self.address.as_deref().filter(|a| *a != self.name) {
            h.push_str(&format!(", {a}"));
        }
        if self.rented {
            h.push_str(" (rented: contents only)");
        }
        h
    }
}

const NO_ROOM: &str = "(no room)";

/// The properties (top-level locations) of one database with their insured items.
fn collect(ctx: &Ctx, profile: Option<&Profile>, db: &Db, catalog: &Catalog, filter: &Filter) -> Result<Vec<Property>> {
    let tree = Tree::load(db)?;
    let items = store::all_items(db)?;
    let with_lots: std::collections::HashSet<String> = store::all_lots(db)?.into_iter().map(|l| l.item_id).collect();
    let fields = store::all_item_fields(db)?;
    let labels = store::all_item_labels(db)?;
    let ten_years_ago = crate::util::date_before(ctx.tz.today(), crate::util::Span::Months(120)).to_string();
    let mut atts: BTreeMap<String, Vec<store::Attachment>> = BTreeMap::new();
    for a in store::attachments(db, None)? {
        atts.entry(a.item_id.clone()).or_default().push(a);
    }
    let dir = ctx.attachments_dir();
    let mut props: BTreeMap<String, Property> = BTreeMap::new();
    for item in items {
        if item.is_gone() || with_lots.contains(&item.id) {
            continue;
        }
        let preset = item.kind_preset.as_deref().and_then(|n| catalog.get(n));
        let kind = crate::consumable::field(fields.get(&item.id), crate::consumable::KIND_FIELD);
        let no_labels = Vec::new();
        let item_labels = labels.get(&item.id).unwrap_or(&no_labels);
        if crate::consumable::is_consumable(kind, item.kind_preset.as_deref(), item_labels, catalog) {
            continue;
        }
        if filter
            .owner
            .is_some_and(|o| !item.owner_entity.as_deref().is_some_and(|x| x.trim().eq_ignore_ascii_case(o.trim())))
        {
            continue;
        }
        if filter.bought_after.as_deref().is_some_and(|d| item.purchase_time.as_deref().is_none_or(|t| t < d)) {
            continue;
        }
        let lineage = item.location_id.map(|id| tree.lineage(id)).unwrap_or_default();
        let root = lineage.first().copied();
        let prop_name = root
            .map(|l| l.name.clone())
            .or_else(|| profile.map(|p| p.title.clone()))
            .unwrap_or_else(|| "(no location)".into());
        let room = lineage.get(1).map_or_else(|| NO_ROOM.to_string(), |l| l.name.clone());
        let paths = |kind: &str| -> Vec<String> {
            atts.get(&item.id)
                .map(|v| v.iter().filter(|a| a.kind == kind).map(|a| dir.join(&a.path).display().to_string()).collect())
                .unwrap_or_default()
        };
        let (value, basis) = match (item.replacement_value, item.purchase_price) {
            (Some(v), _) => (Some(v), "replacement"),
            (None, Some(p)) => (Some(p), "purchase price"),
            _ => (None, ""),
        };
        let line = value.map(|v| v * item.quantity);
        if filter.min_value.is_some_and(|m| line.is_none_or(|l| l < m)) {
            continue;
        }
        let entry = Entry {
            room: room.clone(),
            location: tree.opt_path(item.location_id),
            value,
            line,
            old: item.status == "owned" && item.purchase_time.as_deref().is_some_and(|t| t < ten_years_ago.as_str()),
            basis,
            photos: paths("photo"),
            receipts: paths("receipt"),
            serialized: preset.is_some_and(|p| p.is_serialized()),
            item,
        };
        let is_profile_root = profile.is_some_and(|p| p.title == prop_name);
        let prop = props.entry(prop_name.clone()).or_insert_with(|| Property {
            name: prop_name.clone(),
            profile: profile.map(|p| p.name.clone()),
            address: root.and_then(|r| r.address.clone()).or_else(|| profile.and_then(|p| p.address.clone())),
            rented: profile.is_some_and(|p| p.rented) && (is_profile_root || root.is_none() || profile.is_some()),
            rooms: BTreeMap::new(),
        });
        prop.rooms.entry(room).or_default().push(entry);
    }
    for p in props.values_mut() {
        for r in p.rooms.values_mut() {
            r.sort_by(|a, b| {
                a.location.cmp(&b.location).then_with(|| a.item.name.to_lowercase().cmp(&b.item.name.to_lowercase()))
            });
        }
    }
    Ok(props.into_values().collect())
}

fn yn(b: bool) -> &'static str {
    if b {
        "y"
    } else {
        "n"
    }
}

fn money(c: Option<i64>) -> Value {
    json!(c.map(money_value))
}

/// `$1,299.00`.
fn dollars(c: i64) -> String {
    let s = fmt_money(c.abs());
    let (whole, frac) = s.split_once('.').unwrap_or((&s, "00"));
    let mut out = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{}${out}.{frac}", if c < 0 { "-" } else { "" })
}

fn entry_record(p: &Property, e: &Entry) -> Record {
    let i = &e.item;
    to_record(&json!({
        "section": "item",
        "property": p.name,
        "coverage": if p.rented { "contents only (rented)" } else { "" },
        "room": e.room,
        "location": e.location,
        "name": i.name,
        "asset_id": i.asset_id,
        "quantity": i.quantity,
        "manufacturer": i.manufacturer,
        "model_number": i.model_number,
        "serial_number": i.serial_number,
        "purchase_time": i.purchase_time,
        "purchase_from": i.purchase_from,
        "purchase_price": money(i.purchase_price),
        "replacement_value": money(i.replacement_value),
        "unit_value": money(e.value),
        "line_value": money(e.line),
        "value_basis": e.basis,
        "flag": if e.basis == "purchase price" { "purchase price" } else { "" },
        "photo": yn(!e.photos.is_empty()),
        "photo_paths": e.photos.join("; "),
        "receipt": yn(!e.receipts.is_empty()),
        "receipt_paths": e.receipts.join("; "),
        "owner_entity": i.owner_entity,
        "gaps": e.gaps().join(", "),
        "profile": p.profile,
    }))
}

fn total_record(section: &str, property: &str, room: Option<&str>, label: String, cents: i64, items: usize) -> Record {
    to_record(&json!({
        "section": section,
        "property": property,
        "room": room,
        "name": label,
        "quantity": items,
        "line_value": money_value(cents),
    }))
}

/// Flat rows: items with room subtotals, property totals, the grand total, then gaps.
fn flat_rows(props: &[Property]) -> Vec<Record> {
    let mut rows = Vec::new();
    for p in props {
        for (room, entries) in &p.rooms {
            rows.extend(entries.iter().map(|e| entry_record(p, e)));
            rows.push(total_record(
                "room subtotal",
                &p.name,
                Some(room),
                format!("Subtotal: {room}"),
                sum(entries),
                entries.len(),
            ));
        }
        rows.push(total_record(
            "property total",
            &p.name,
            None,
            format!("Total: {}", p.heading()),
            p.total(),
            p.entries().count(),
        ));
    }
    let grand: i64 = props.iter().map(Property::total).sum();
    let n: usize = props.iter().map(|p| p.entries().count()).sum();
    rows.push(total_record("grand total", "", None, "Grand total".into(), grand, n));
    for p in props {
        for e in p.entries().filter(|e| !e.gaps().is_empty()) {
            let mut r = to_record(&json!({
                "section": "gap",
                "property": p.name,
                "room": e.room,
                "location": e.location,
                "name": e.item.name,
                "asset_id": e.item.asset_id,
                "gaps": e.gaps().join(", "),
            }));
            r.insert("profile".into(), json!(p.profile));
            rows.push(r);
        }
    }
    rows
}

fn nested(props: &[Property], header: &[String]) -> Value {
    let grand: i64 = props.iter().map(Property::total).sum();
    json!({
        "header": header,
        "properties": props.iter().map(|p| json!({
            "name": p.name,
            "profile": p.profile,
            "address": p.address,
            "rented": p.rented,
            "coverage": if p.rented { "contents only" } else { "building and contents" },
            "rooms": p.rooms.iter().map(|(room, entries)| json!({
                "room": room,
                "items": entries.iter().map(|e| entry_record(p, e)).collect::<Vec<_>>(),
                "subtotal": money_value(sum(entries)),
            })).collect::<Vec<_>>(),
            "total": money_value(p.total()),
        })).collect::<Vec<_>>(),
        "grand_total": money_value(grand),
        "flagged": props.iter().flat_map(|p| p.entries()).filter(|e| e.basis == "purchase price").count(),
        "gap_counts": gap_counts(props),
        "gaps": props.iter().flat_map(|p| p.entries().filter(|e| !e.gaps().is_empty()).map(move |e| json!({
            "property": p.name, "location": e.location, "name": e.item.name, "asset_id": e.item.asset_id, "gaps": e.gaps(),
        }))).collect::<Vec<_>>(),
    })
}

/// How many items have each gap.
fn gap_counts(props: &[Property]) -> BTreeMap<&'static str, usize> {
    let mut c = BTreeMap::new();
    for g in props.iter().flat_map(Property::entries).flat_map(Entry::gaps) {
        *c.entry(g).or_insert(0) += 1;
    }
    c
}

fn header_lines(ctx: &Ctx, props: &[Property], filter: &Filter) -> Vec<String> {
    let owner = filter.owner;
    let mut h = vec![format!("Insurance inventory, {}", ctx.tz.today())];
    let names: Vec<String> = props.iter().map(Property::heading).collect();
    if !names.is_empty() {
        h.push(format!("Properties: {}", names.join("; ")));
    }
    if let Some(o) = owner {
        h.push(format!("Owner: {o}"));
    }
    if props.iter().any(|p| p.rented) {
        let rented: Vec<&str> = props.iter().filter(|p| p.rented).map(|p| p.name.as_str()).collect();
        h.push(format!("Renter's view: {} rented; this report covers contents only", rented.join(", ")));
    }
    if let Some(m) = filter.min_value {
        h.push(format!("Lines worth at least {}", dollars(m)));
    }
    if let Some(d) = &filter.bought_after {
        h.push(format!("Bought on or after {d}"));
    }
    h.push(
        "Unit value = replacement value; * = purchase price used (no replacement value recorded). \
         Line value = unit value x quantity."
            .into(),
    );
    let old = gap_counts(props).get(VERIFY_OWNED).copied().unwrap_or(0);
    if old > 0 {
        h.push(format!("Verify still owned: {old} item(s) bought more than 10 years ago"));
    }
    h
}

fn insurance(ctx: &Ctx, a: InsuranceArgs) -> Result<()> {
    let catalog = Catalog::load(&ctx.resolved.config_path)?;
    let fmts = ctx.input_date_formats();
    let fmts: Vec<&str> = fmts.iter().map(String::as_str).collect();
    let filter = Filter {
        owner: a.owner.as_deref(),
        min_value: a.min_value.as_deref().map(crate::util::parse_money).transpose()?,
        bought_after: a.bought_after.as_deref().map(|d| crate::util::parse_day(d, &fmts, &ctx.tz)).transpose()?,
    };
    let props: Vec<Property> =
        crate::commands::profile::each_db(ctx, a.scope.all_profiles, "report insurance", |c, p, db| {
            collect(c, p, db, &catalog, &filter)
        })?
        .into_iter()
        .flatten()
        .collect();
    let header = header_lines(ctx, &props, &filter);
    let format = if ctx.json_flag { Format::Json } else { ctx.out.format };
    match format {
        Format::Xlsx | Format::Pdf => {
            let to_terminal = ctx.out.output.as_ref().is_none_or(|p| p.as_os_str() == "-")
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            if to_terminal && !ctx.encrypt_output {
                return Err(AppError::usage("xlsx and pdf are binary: write them with --output FILE"));
            }
            let bytes = if format == Format::Xlsx { xlsx(&props, &header)? } else { pdf(&props, &header) };
            ctx.write_output(&bytes, &ctx.out)
        }
        Format::Html => ctx.write_output(html(&props, &header).as_bytes(), &ctx.out),
        Format::Table => {
            let rows = flat_rows(&props);
            let (gaps, lines): (Vec<Record>, Vec<Record>) =
                rows.into_iter().partition(|r| r.get("section") == Some(&json!("gap")));
            let main = Report::list("insurance", lines).table_columns(&[
                "property",
                "room",
                "name",
                "quantity",
                "manufacturer",
                "model_number",
                "serial_number",
                "purchase_time",
                "unit_value",
                "line_value",
                "flag",
                "photo",
                "receipt",
            ]);
            let gap_report =
                Report::list("insurance_gaps", gaps).table_columns(&["property", "location", "name", "gaps"]);
            let mut text = header.join("\n") + "\n\n";
            text.push_str(&crate::output::render(&main, &ctx.out)?);
            let counts = gap_counts(&props).iter().map(|(g, n)| format!("{n} {g}")).collect::<Vec<_>>().join(", ");
            text.push_str(&format!("\nGaps ({}): {counts}\n", gap_report.rows().len()));
            text.push_str(&crate::output::render(&gap_report, &ctx.out)?);
            ctx.write_output(text.as_bytes(), &ctx.out)
        }
        _ => {
            let r = Report::list("insurance", flat_rows(&props))
                .json_data(nested(&props, &header))
                .meta("header", json!(header));
            ctx.emit(&r)
        }
    }
}

const STYLE: &str = "body{font-family:system-ui,sans-serif;margin:2em;color:#222}table{border-collapse:collapse;width:100%;margin:.5em 0 1.5em}\
th,td{border:1px solid #ccc;padding:3px 6px;font-size:13px;text-align:left;vertical-align:top}th{background:#f2f2f2}\
td.n{text-align:right;white-space:nowrap}tr.sub td{font-weight:bold;background:#fafafa}.flag{color:#a60}.paths{font-size:11px;color:#555;word-break:break-all}\
h2{margin-top:1.5em}.rented{color:#a00}";

fn html(props: &[Property], header: &[String]) -> String {
    let e = html_escape;
    let mut s = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Insurance inventory</title>\n<style>{STYLE}</style>\n</head>\n<body>\n<h1>Insurance inventory</h1>\n"
    );
    for h in header.iter().skip(1) {
        s.push_str(&format!("<p>{}</p>\n", e(h)));
    }
    let cols = [
        "Location",
        "Item",
        "Qty",
        "Maker / model",
        "Serial",
        "Purchased",
        "Vendor",
        "Price",
        "Unit value",
        "Line value",
        "Photo",
        "Receipt",
    ];
    for p in props {
        s.push_str(&format!("<h2>{}</h2>\n", e(&p.name)));
        if let Some(a) = &p.address {
            s.push_str(&format!("<p>{}</p>\n", e(a)));
        }
        if p.rented {
            s.push_str("<p class=\"rented\"><strong>Rented property: contents only.</strong></p>\n");
        }
        for (room, entries) in &p.rooms {
            s.push_str(&format!("<h3>{}</h3>\n<table>\n<thead><tr>", e(room)));
            s.extend(cols.iter().map(|c| format!("<th>{c}</th>")));
            s.push_str("</tr></thead>\n<tbody>\n");
            for en in entries {
                let i = &en.item;
                let mm = [i.manufacturer.as_deref(), i.model_number.as_deref()]
                    .iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" / ");
                let flag = if en.basis == "purchase price" {
                    " <span class=\"flag\" title=\"purchase price, no replacement value\">*</span>"
                } else {
                    ""
                };
                let paths = |v: &[String]| {
                    if v.is_empty() {
                        "n".to_string()
                    } else {
                        format!(
                            "y<div class=\"paths\">{}</div>",
                            v.iter().map(|x| e(x)).collect::<Vec<_>>().join("<br>")
                        )
                    }
                };
                s.push_str(&format!(
                    "<tr><td>{}</td><td>{}{}</td><td class=\"n\">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}{flag}</td><td class=\"n\">{}</td><td>{}</td><td>{}</td></tr>\n",
                    e(en.location.as_deref().unwrap_or("")),
                    e(&i.name),
                    i.asset_id.as_deref().map(|a| format!(" <small>({})</small>", e(a))).unwrap_or_default(),
                    i.quantity,
                    e(&mm),
                    e(i.serial_number.as_deref().unwrap_or("")),
                    e(i.purchase_time.as_deref().unwrap_or("")),
                    e(i.purchase_from.as_deref().unwrap_or("")),
                    i.purchase_price.map(dollars).unwrap_or_default(),
                    en.value.map(dollars).unwrap_or_default(),
                    en.line.map(dollars).unwrap_or_default(),
                    paths(&en.photos),
                    paths(&en.receipts),
                ));
            }
            s.push_str(&format!(
                "<tr class=\"sub\"><td colspan=\"9\">Subtotal: {}</td><td class=\"n\">{}</td><td colspan=\"2\"></td></tr>\n</tbody>\n</table>\n",
                e(room),
                dollars(sum(entries))
            ));
        }
        s.push_str(&format!("<p><strong>Total, {}: {}</strong></p>\n", e(&p.heading()), dollars(p.total())));
    }
    let grand: i64 = props.iter().map(Property::total).sum();
    s.push_str(&format!("<h2>Grand total: {}</h2>\n<h2>Gaps</h2>\n", dollars(grand)));
    let gaps: Vec<(&Property, &Entry)> =
        props.iter().flat_map(|p| p.entries().filter(|e| !e.gaps().is_empty()).map(move |e| (p, e))).collect();
    if gaps.is_empty() {
        s.push_str("<p>None: every item has a value, a photo and a receipt.</p>\n");
    } else {
        s.push_str("<table>\n<thead><tr><th>Property</th><th>Location</th><th>Item</th><th>Missing</th></tr></thead>\n<tbody>\n");
        for (p, en) in gaps {
            s.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                e(&p.name),
                e(en.location.as_deref().unwrap_or("")),
                e(&en.item.name),
                e(&en.gaps().join(", "))
            ));
        }
        s.push_str("</tbody>\n</table>\n");
    }
    s.push_str("</body>\n</html>\n");
    s
}

fn xlsx(props: &[Property], header: &[String]) -> Result<Vec<u8>> {
    use rust_xlsxwriter::{Format as XFormat, Workbook};
    let err = |e: rust_xlsxwriter::XlsxError| AppError::io(format!("xlsx: {e}"));
    let mut wb = Workbook::new();
    let bold = XFormat::new().set_bold();
    let money_fmt = XFormat::new().set_num_format("$#,##0.00");
    let money_bold = XFormat::new().set_num_format("$#,##0.00").set_bold();
    let ws = wb.add_worksheet().set_name("Inventory").map_err(err)?;
    let mut row: u32 = 0;
    for h in header {
        ws.write_string_with_format(row, 0, h, &bold).map_err(err)?;
        row += 1;
    }
    row += 1;
    let cols = [
        "Property",
        "Room",
        "Location",
        "Item",
        "Asset ID",
        "Qty",
        "Manufacturer",
        "Model",
        "Serial",
        "Purchased",
        "Vendor",
        "Price",
        "Replacement value",
        "Unit value",
        "Line value",
        "Value basis",
        "Photo",
        "Photo paths",
        "Receipt",
        "Receipt paths",
        "Owner",
        "Gaps",
    ];
    for (c, name) in cols.iter().enumerate() {
        ws.write_string_with_format(row, c as u16, *name, &bold).map_err(err)?;
    }
    row += 1;
    for p in props {
        let pname = p.heading();
        for (room, entries) in &p.rooms {
            for en in entries {
                let i = &en.item;
                let texts: [(u16, Option<&str>); 15] = [
                    (0, Some(&pname)),
                    (1, Some(room)),
                    (2, en.location.as_deref()),
                    (3, Some(&i.name)),
                    (4, i.asset_id.as_deref()),
                    (6, i.manufacturer.as_deref()),
                    (7, i.model_number.as_deref()),
                    (8, i.serial_number.as_deref()),
                    (9, i.purchase_time.as_deref()),
                    (10, i.purchase_from.as_deref()),
                    (15, Some(if en.basis == "purchase price" { "purchase price (flagged)" } else { en.basis })),
                    (16, Some(yn(!en.photos.is_empty()))),
                    (18, Some(yn(!en.receipts.is_empty()))),
                    (20, i.owner_entity.as_deref()),
                    (21, None),
                ];
                for (c, v) in texts {
                    if let Some(v) = v {
                        ws.write_string(row, c, v).map_err(err)?;
                    }
                }
                ws.write_number(row, 5, i.quantity as f64).map_err(err)?;
                for (c, v) in [(11, i.purchase_price), (12, i.replacement_value), (13, en.value), (14, en.line)] {
                    if let Some(v) = v {
                        ws.write_number_with_format(row, c, money_value(v), &money_fmt).map_err(err)?;
                    }
                }
                ws.write_string(row, 17, en.photos.join("; ")).map_err(err)?;
                ws.write_string(row, 19, en.receipts.join("; ")).map_err(err)?;
                ws.write_string(row, 21, en.gaps().join(", ")).map_err(err)?;
                row += 1;
            }
            ws.write_string_with_format(row, 1, format!("Subtotal: {room}"), &bold).map_err(err)?;
            ws.write_number_with_format(row, 14, money_value(sum(entries)), &money_bold).map_err(err)?;
            row += 1;
        }
        ws.write_string_with_format(row, 0, format!("Total: {pname}"), &bold).map_err(err)?;
        ws.write_number_with_format(row, 14, money_value(p.total()), &money_bold).map_err(err)?;
        row += 2;
    }
    ws.write_string_with_format(row, 0, "Grand total", &bold).map_err(err)?;
    ws.write_number_with_format(row, 14, money_value(props.iter().map(Property::total).sum()), &money_bold)
        .map_err(err)?;
    ws.set_freeze_panes(header.len() as u32 + 2, 0).map_err(err)?;
    let gs = wb.add_worksheet().set_name("Gaps").map_err(err)?;
    for (c, name) in ["Property", "Location", "Item", "Asset ID", "Missing"].iter().enumerate() {
        gs.write_string_with_format(0, c as u16, *name, &bold).map_err(err)?;
    }
    let mut r = 1;
    for p in props {
        for en in p.entries().filter(|e| !e.gaps().is_empty()) {
            gs.write_string(r, 0, &p.name).map_err(err)?;
            gs.write_string(r, 1, en.location.as_deref().unwrap_or("")).map_err(err)?;
            gs.write_string(r, 2, &en.item.name).map_err(err)?;
            gs.write_string(r, 3, en.item.asset_id.as_deref().unwrap_or("")).map_err(err)?;
            gs.write_string(r, 4, en.gaps().join(", ")).map_err(err)?;
            r += 1;
        }
    }
    wb.save_to_buffer().map_err(err)
}

fn pdf(props: &[Property], header: &[String]) -> Vec<u8> {
    use crate::pdf::{cell, Doc, Style};
    const SIZE: f32 = 7.0;
    let mut d = Doc::new("Insurance inventory");
    d.text("Insurance inventory", 14.0, Style::Bold);
    for h in header.iter().skip(1) {
        d.text(h, 8.0, Style::Normal);
    }
    // Column widths in characters (Courier).
    let w = [28usize, 26, 4, 22, 14, 10, 14, 12, 13, 13, 3, 3];
    let right = [false, false, true, false, false, false, false, true, true, true, false, false];
    let row = |cells: [&str; 12]| -> String {
        cells.iter().enumerate().map(|(i, c)| cell(c, w[i], right[i])).collect::<Vec<_>>().join(" ")
    };
    let head = row([
        "Location",
        "Item",
        "Qty",
        "Maker / model",
        "Serial",
        "Purchased",
        "Vendor",
        "Price",
        "Unit value",
        "Line value",
        "Ph",
        "Rc",
    ]);
    for p in props {
        d.gap();
        d.text(&p.heading(), 11.0, Style::Bold);
        for (room, entries) in &p.rooms {
            d.text(room, 9.0, Style::Bold);
            d.text(&head, SIZE, Style::Bold);
            d.rule();
            for en in entries {
                let i = &en.item;
                let mm = [i.manufacturer.as_deref(), i.model_number.as_deref()]
                    .iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" / ");
                let value = en
                    .value
                    .map(|v| format!("{}{}", dollars(v), if en.basis == "purchase price" { "*" } else { "" }))
                    .unwrap_or_default();
                let line = en.line.map(dollars).unwrap_or_default();
                let price = i.purchase_price.map(dollars).unwrap_or_default();
                let qty = i.quantity.to_string();
                let loc = en.location.as_deref().unwrap_or("");
                d.text(
                    &row([
                        loc,
                        &i.name,
                        &qty,
                        &mm,
                        i.serial_number.as_deref().unwrap_or(""),
                        i.purchase_time.as_deref().unwrap_or(""),
                        i.purchase_from.as_deref().unwrap_or(""),
                        &price,
                        &value,
                        &line,
                        yn(!en.photos.is_empty()),
                        yn(!en.receipts.is_empty()),
                    ]),
                    SIZE,
                    Style::Normal,
                );
                for (what, v) in [("photo", &en.photos), ("receipt", &en.receipts)] {
                    for path in v.iter() {
                        d.text(&format!("    {what}: {path}"), 6.0, Style::Normal);
                    }
                }
            }
            d.rule();
            let sub = format!("Subtotal: {room}");
            let label_w: usize = w[..9].iter().sum::<usize>() + 8;
            d.text(
                &format!("{} {}", cell(&sub, label_w, false), cell(&dollars(sum(entries)), w[9], true)),
                SIZE,
                Style::Bold,
            );
        }
        d.gap();
        d.text(&format!("Total, {}: {}", p.heading(), dollars(p.total())), 9.0, Style::Bold);
    }
    d.gap();
    d.rule();
    d.text(&format!("Grand total: {}", dollars(props.iter().map(Property::total).sum())), 11.0, Style::Bold);
    d.gap();
    d.text("Gaps", 11.0, Style::Bold);
    let gaps: Vec<(&Property, &Entry)> =
        props.iter().flat_map(|p| p.entries().filter(|e| !e.gaps().is_empty()).map(move |e| (p, e))).collect();
    if gaps.is_empty() {
        d.text("None: every item has a value, a photo and a receipt.", 8.0, Style::Normal);
    }
    for (p, en) in gaps {
        d.text(
            &format!(
                "{} {} {}",
                cell(&p.name, 24, false),
                cell(&format!("{} ({})", en.item.name, en.location.as_deref().unwrap_or("-")), 70, false),
                en.gaps().join(", ")
            ),
            SIZE,
            Style::Normal,
        );
    }
    d.render()
}

// ---------------------------------------------------------------------------
// Warranties
// ---------------------------------------------------------------------------

fn warranties(ctx: &Ctx, a: WarrantyArgs) -> Result<()> {
    let days = parse_duration_days(&a.expiring)?.round() as i64;
    let today = ctx.tz.today();
    let mut rows: Vec<(String, Record)> =
        crate::commands::profile::each_db(ctx, a.scope.all_profiles, "report warranties", |_, p, db| {
            let tree = Tree::load(db)?;
            let receipts: std::collections::HashSet<String> = store::attachments(db, None)?
                .into_iter()
                .filter(|x| matches!(x.kind.as_str(), "receipt" | "warranty"))
                .map(|x| x.item_id)
                .collect();
            Ok(store::all_items(db)?
                .into_iter()
                .filter(|i| !i.is_gone() && !i.lifetime_warranty)
                .filter(|i| {
                    a.owner
                        .as_deref()
                        .is_none_or(|o| i.owner_entity.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(o)))
                })
                .filter_map(|i| {
                    let ends = i.warranty_expires.clone()?;
                    let left = crate::util::days_until(&ends, today)?;
                    if left > days || (left < 0 && !a.include_expired) {
                        return None;
                    }
                    let rec = to_record(&json!({
                        "warranty_expires": ends,
                        "days_left": left,
                        "state": if left < 0 { "expired" } else { "soon" },
                        "name": i.name,
                        "asset_id": i.asset_id,
                        "location": tree.opt_path(i.location_id),
                        "manufacturer": i.manufacturer,
                        "model_number": i.model_number,
                        "serial_number": i.serial_number,
                        "purchase_time": i.purchase_time,
                        "purchase_from": i.purchase_from,
                        "warranty_details": i.warranty_details,
                        "receipt": receipts.contains(&i.id),
                        "profile": p.map(|p| p.name.clone()),
                    }));
                    Some((format!("{ends}{}", i.id), rec))
                })
                .collect::<Vec<_>>())
        })?
        .into_iter()
        .flatten()
        .collect();
    rows.sort_by(|x, y| x.0.cmp(&y.0));
    if rows.is_empty() {
        ctx.info(&format!("no warranty ends within {}", a.expiring));
    }
    let mut cols =
        vec!["warranty_expires", "days_left", "state", "name", "location", "manufacturer", "model_number", "receipt"];
    if a.scope.all_profiles {
        cols.insert(3, "profile");
    }
    ctx.emit(
        &Report::list("warranties", rows.into_iter().map(|r| r.1).collect())
            .table_columns(&cols)
            .dates(&["warranty_expires", "purchase_time"])
            .meta("within_days", json!(days)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_group_thousands() {
        assert_eq!(dollars(0), "$0.00");
        assert_eq!(dollars(129_999), "$1,299.99");
        assert_eq!(dollars(123_456_789), "$1,234,567.89");
        assert_eq!(dollars(-5_000), "-$50.00");
    }
}
