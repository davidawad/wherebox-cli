//! `preset ls|show`: kind presets (see [`crate::presets`]).

use serde_json::json;

use crate::cli::PresetCmd;
use crate::context::Ctx;
use crate::error::Result;
use crate::output::{to_record, Record, Report};
use crate::presets::{Catalog, Life, Preset};

pub fn run(ctx: &Ctx, cmd: PresetCmd) -> Result<()> {
    let catalog = Catalog::load(&ctx.resolved.config_path)?;
    match cmd {
        PresetCmd::Ls => {
            let rows = catalog.presets.values().map(summary).collect();
            ctx.emit(&Report::list("presets", rows).table_columns(&[
                "name",
                "category",
                "tracks",
                "shelf_life_type",
                "intervals",
                "alert_days",
            ]))
        }
        PresetCmd::Show { name } => {
            let p = catalog.require(&name)?;
            ctx.emit(&Report::object("preset", detail(p)).json_data(json!(p)))
        }
    }
}

fn shown(v: &Option<String>) -> Option<String> {
    v.as_deref().map(|s| Life::parse(s).map_or_else(|_| s.to_string(), Life::display))
}

fn interval_text(p: &Preset) -> Option<String> {
    let i = &p.intervals;
    let parts: Vec<String> = [
        ("replace", &i.replace),
        ("inspect", &i.inspect),
        ("service", &i.service),
        ("appraise", &i.appraise),
        ("warranty", &i.warranty),
    ]
    .iter()
    .filter_map(|(k, v)| shown(v).map(|t| format!("{k} every {t}")))
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn summary(p: &Preset) -> Record {
    to_record(&json!({
        "name": p.name,
        "category": p.category,
        "description": p.description,
        "tracks": p.tracks.join(", "),
        "shelf_life_type": p.shelf_life_type,
        "intervals": interval_text(p),
        "alert_days": p.alert_days,
        "origin": p.origin,
    }))
}

fn detail(p: &Preset) -> Record {
    let mut r = summary(p);
    if let Some(t) = &p.shelf_life_type {
        r.insert("shelf_life".into(), json!(format!("{t} (see `wherebox shelf-life show {t}`)")));
    }
    r.insert("notes".into(), json!(p.notes));
    if p.defaults.unit.is_some() || !p.defaults.labels.is_empty() || !p.defaults.fields.is_empty() {
        r.insert("defaults".into(), json!(p.defaults));
    }
    for (n, s) in p.sources.iter().enumerate() {
        let url = s.url.as_deref().map(|u| format!(" <{u}>")).unwrap_or_default();
        r.insert(format!("source_{}", n + 1), json!(format!("[{}] {}{url}", s.covers, s.cite)));
    }
    if p.sources.is_empty() {
        r.insert("sources".into(), json!("none (no cited defaults)"));
    }
    r
}
