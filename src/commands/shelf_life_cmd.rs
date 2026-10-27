//! `shelf-life ls|show|set|reset`: shelf-life types (see [`crate::shelf_life`]).

use serde_json::json;
use toml_edit::{value, Array, DocumentMut, Item as TItem, Table};

use crate::cli::ShelfLifeCmd;
use crate::context::Ctx;
use crate::error::{AppError, Result};
use crate::output::{to_record, Record, Report};
use crate::shelf_life::{check_name, normalize_figure, ShelfType, Storage, Types, GROUPS};

pub fn run(ctx: &Ctx, cmd: ShelfLifeCmd) -> Result<()> {
    let path = ctx.resolved.config_path.clone();
    match cmd {
        ShelfLifeCmd::Ls => {
            let types = Types::load(&path)?;
            let rows = types.types.values().map(summary).collect();
            ctx.emit(
                &Report::list("shelf_life", rows)
                    .table_columns(&["name", "group", "pantry", "fridge", "freezer", "opened", "origin"]),
            )
        }
        ShelfLifeCmd::Show { name } => {
            let types = Types::load(&path)?;
            let t = types.require(&name)?;
            ctx.emit(&Report::object("shelf_life", detail(t)).json_data(json!(t)))
        }
        ShelfLifeCmd::Set { name, pantry, fridge, freezer, opened, synonyms, description, group } => {
            check_name(&name)?;
            if let Some(g) = group.as_deref().filter(|g| !GROUPS.contains(g)) {
                return Err(AppError::invalid(format!("group '{g}' is not one of {}", GROUPS.join(", "))));
            }
            let figures = [("pantry", pantry), ("fridge", fridge), ("freezer", freezer), ("opened", opened)];
            if figures.iter().all(|(_, v)| v.is_none())
                && synonyms.is_empty()
                && description.is_none()
                && group.is_none()
            {
                return Err(AppError::usage("nothing to set (give --pantry, --fridge, --freezer, --opened, --synonym, --description or --group)"));
            }
            edit_config(&path, |doc| {
                let t = type_table(doc, &name);
                for (k, v) in &figures {
                    if let Some(v) = v {
                        t[k] = value(normalize_figure(v)?.unwrap_or_else(|| "none".into()));
                    }
                }
                if !synonyms.is_empty() {
                    let mut a: Array = t.get("synonyms").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    for s in &synonyms {
                        if !a.iter().any(|x| x.as_str() == Some(s.as_str())) {
                            a.push(s.as_str());
                        }
                    }
                    t["synonyms"] = value(a);
                }
                if let Some(d) = &description {
                    t["description"] = value(d.as_str());
                }
                if let Some(g) = &group {
                    t["group"] = value(g.as_str());
                }
                Ok(())
            })?;
            let types = Types::load(&path)?;
            let t = types.require(&name)?;
            ctx.info(&format!("shelf-life {}: {} (config: {})", t.name, t.figures_text(), path.display()));
            ctx.emit_mutation(&Report::object("shelf_life", detail(t)).json_data(json!(t)))
        }
        ShelfLifeCmd::Reset { name } => {
            let mut removed = false;
            edit_config(&path, |doc| {
                if let Some(t) = doc.get_mut("shelf_life").and_then(TItem::as_table_mut) {
                    removed = t.remove(&name).is_some();
                    if t.is_empty() {
                        doc.remove("shelf_life");
                    }
                }
                Ok(())
            })?;
            if !removed {
                return Err(AppError::not_found(format!("no [shelf_life.{name}] table in {}", path.display())));
            }
            let types = Types::load(&path)?;
            match types.get(&name) {
                Some(t) => {
                    ctx.info(&format!("shelf-life {}: back to built-in ({})", t.name, t.figures_text()));
                    ctx.emit_mutation(&Report::object("shelf_life", detail(t)).json_data(json!(t)))
                }
                None => {
                    ctx.info(&format!("removed shelf-life type {name}"));
                    ctx.emit_mutation(&Report::object("removed", to_record(&json!({"shelf_life": name}))))
                }
            }
        }
    }
}

/// `[shelf_life.<name>]`, created when missing.
fn type_table<'a>(doc: &'a mut DocumentMut, name: &str) -> &'a mut Table {
    let root = doc.entry("shelf_life").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        TItem::Table(t)
    });
    root.as_table_mut()
        .expect("shelf_life is a table")
        .entry(name)
        .or_insert_with(|| TItem::Table(Table::new()))
        .as_table_mut()
        .expect("a type is a table")
}

/// Edit the config file's settings part with `toml_edit` (comments and the
/// rest kept), checking the shelf-life tables before writing.
fn edit_config(path: &std::path::Path, f: impl FnOnce(&mut DocumentMut) -> Result<()>) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let (head, block) = crate::enc_config::split(&text);
    let mut doc: DocumentMut = head.parse().map_err(|e| AppError::config(format!("{}: {e}", path.display())))?;
    if doc.get("shelf_life").is_some_and(|v| !v.is_table()) {
        return Err(AppError::config(format!("{}: shelf_life must be a table", path.display())));
    }
    f(&mut doc)?;
    let head = doc.to_string();
    let table: toml::Table = head.parse().map_err(|e| AppError::config(format!("{e}")))?;
    if let Some(toml::Value::Table(t)) = table.get("shelf_life") {
        Types::builtin().with_overrides(t)?;
    }
    crate::config::ensure_file(path)?;
    let mut out = head;
    if !block.is_empty() {
        out = format!("{}\n\n{block}", out.trim_end());
    }
    std::fs::write(path, out).map_err(|e| AppError::io(format!("writing {}: {e}", path.display())))
}

fn fig(t: &ShelfType, s: Storage) -> Option<String> {
    t.figure(s).map(crate::presets::Life::display)
}

fn summary(t: &ShelfType) -> Record {
    to_record(&json!({
        "name": t.name,
        "group": t.group,
        "description": t.description,
        "pantry": fig(t, Storage::Pantry),
        "fridge": fig(t, Storage::Fridge),
        "freezer": fig(t, Storage::Freezer),
        "opened": t.opened_life().map(crate::presets::Life::display),
        "origin": t.origin,
    }))
}

fn detail(t: &ShelfType) -> Record {
    let mut r = summary(t);
    r.insert("synonyms".into(), json!(t.synonyms.join(", ")));
    if !t.not.is_empty() {
        r.insert("not".into(), json!(t.not.join(", ")));
    }
    r.insert("source".into(), json!(t.source));
    r
}
