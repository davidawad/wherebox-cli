//! `shelf-life ls|show|set|unset`: shelf-life types (see [`crate::shelf_life`]).

use serde_json::json;

use crate::cli::ShelfLifeCmd;
use crate::context::Ctx;
use crate::error::{AppError, Result};
use crate::output::{to_record, Record, Report};
use crate::presets::{Catalog, Life};
use crate::shelf_life::{ShelfType, Storage, Types, GROUPS};

pub fn run(ctx: &Ctx, cmd: ShelfLifeCmd) -> Result<()> {
    let catalog = Catalog::load(&ctx.resolved.config_path)?;
    let types = Types::load(&ctx.resolved.config_path, &catalog)?;
    match cmd {
        ShelfLifeCmd::Ls { group } => {
            if let Some(g) = group.as_deref().filter(|g| !GROUPS.contains(g)) {
                return Err(AppError::usage(format!("no group '{g}' (groups: {})", GROUPS.join(", "))));
            }
            let rows =
                types.types.values().filter(|t| group.as_deref().is_none_or(|g| t.group == g)).map(record).collect();
            ctx.emit(
                &Report::list("shelf_life", rows)
                    .table_columns(&["name", "group", "pantry", "fridge", "freezer", "opened", "origin"]),
            )
        }
        ShelfLifeCmd::Show { name } => {
            let t = types.require(&name)?;
            let mut r = record(t);
            r.insert("synonyms".into(), json!(t.synonyms));
            r.insert("summary".into(), json!(t.summary()));
            r.insert("items".into(), json!(classified_items(ctx, &t.name)?));
            ctx.emit(&Report::object("shelf_life", r))
        }
        ShelfLifeCmd::Set { name, pantry, fridge, freezer, opened, group, synonyms } => {
            let name = name.trim().to_ascii_lowercase();
            let mut set: Vec<(&str, Option<toml_edit::Item>)> = Vec::new();
            for (k, v) in [("pantry", &pantry), ("fridge", &fridge), ("freezer", &freezer), ("opened", &opened)] {
                if let Some(v) = v {
                    let v = v.trim();
                    if v.eq_ignore_ascii_case("none") || v.is_empty() {
                        // On a built-in type `none` must be written to remove its value.
                        let builtin = Types::builtin().get(&name).is_some_and(|t| life_of(t, k).is_some());
                        set.push((k, builtin.then(|| toml_edit::value("none"))));
                    } else {
                        Life::parse(v).map_err(|e| e.context(format!("--{k}")))?;
                        set.push((k, Some(toml_edit::value(v))));
                    }
                }
            }
            if let Some(g) = &group {
                set.push(("group", Some(toml_edit::value(g.trim()))));
            }
            if !synonyms.is_empty() {
                let mut all: Vec<String> = current_synonyms(&ctx.resolved.config_path, &name);
                for s in &synonyms {
                    if !all.iter().any(|x| x.eq_ignore_ascii_case(s.trim())) {
                        all.push(s.trim().to_string());
                    }
                }
                let arr: toml_edit::Array = all.iter().map(String::as_str).collect();
                set.push(("synonyms", Some(toml_edit::value(arr))));
            }
            if set.is_empty() {
                return Err(AppError::usage(
                    "nothing to set (give --pantry, --fridge, --freezer, --opened, --group or --synonym)",
                ));
            }
            crate::shelf_life::write_config(&ctx.resolved.config_path, &name, &set, false)?;
            let types = Types::load(&ctx.resolved.config_path, &catalog)?;
            let t = types.require(&name)?;
            ctx.info(&format!("{}: {} (in {})", t.name, t.summary(), ctx.resolved.config_path.display()));
            ctx.emit_mutation(&Report::object("shelf_life", record(t)))
        }
        ShelfLifeCmd::Unset { name } => {
            let name = name.trim().to_ascii_lowercase();
            crate::shelf_life::write_config(&ctx.resolved.config_path, &name, &[], true)?;
            let types = Types::load(&ctx.resolved.config_path, &catalog)?;
            match types.get(&name) {
                Some(t) => {
                    ctx.info(&format!("{}: back to {}", t.name, t.summary()));
                    ctx.emit_mutation(&Report::object("shelf_life", record(t)))
                }
                None => {
                    ctx.info(&format!("removed shelf-life type {name}"));
                    ctx.emit_mutation(&Report::object("removed", to_record(&json!({"shelf_life": name}))))
                }
            }
        }
    }
}

fn life_of(t: &ShelfType, key: &str) -> Option<Life> {
    match key {
        "pantry" => t.pantry,
        "fridge" => t.fridge,
        "freezer" => t.freezer,
        _ => t.opened,
    }
}

fn record(t: &ShelfType) -> Record {
    let show = |l: Option<Life>| json!(l.map(Life::display));
    let mut r = to_record(&json!({
        "name": t.name,
        "group": t.group,
        "pantry": show(t.pantry),
        "fridge": show(t.fridge),
        "freezer": show(t.freezer),
        "opened": show(t.opened),
        "origin": t.origin,
    }));
    let days = |s: Storage| t.life(s).map(|l| l.span_days());
    r.insert(
        "days".into(),
        json!({
            "pantry": days(Storage::Pantry),
            "fridge": days(Storage::Fridge),
            "freezer": days(Storage::Freezer),
            "opened": t.opened.map(Life::span_days),
        }),
    );
    r
}

/// Synonyms already in the config table of a type.
fn current_synonyms(config_path: &std::path::Path, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(config_path).unwrap_or_default();
    let (head, _) = crate::enc_config::split(&text);
    let Ok(table) = head.parse::<toml::Table>() else { return Vec::new() };
    table
        .get("shelf_life")
        .and_then(|t| t.get(name))
        .and_then(|t| t.get("synonyms"))
        .and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Names of the items of the current profile classified as `name` (none
/// when there is no database yet).
fn classified_items(ctx: &Ctx, name: &str) -> Result<Vec<String>> {
    if !ctx.db_path.exists() {
        return Ok(Vec::new());
    }
    let db = ctx.db()?;
    let est = crate::onhand::Estimator::for_ctx(ctx)?.all(&db)?;
    let mut out: Vec<String> = crate::store::all_items(&db)?
        .into_iter()
        .filter(|i| !i.is_gone())
        .filter(|i| est.get(&i.id).and_then(|v| v.shelf_life.as_ref()).is_some_and(|s| s.0 == name))
        .map(|i| i.name)
        .collect();
    out.sort();
    Ok(out)
}
