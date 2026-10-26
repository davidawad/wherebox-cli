//! `label add|ls|rm`.

use serde_json::json;

use crate::cli::LabelCmd;
use crate::context::Ctx;
use crate::error::{AppError, Result};
use crate::output::{to_record, Report};
use crate::store;

pub fn run(ctx: &Ctx, cmd: LabelCmd) -> Result<()> {
    let db = ctx.db()?;
    match cmd {
        LabelCmd::Add { name, description } => {
            if store::find_label(&db, &name)?.is_some() {
                return Err(AppError::invalid(format!("label '{}' already exists", name.trim())));
            }
            let (id, _) = store::ensure_label(&db, &name, description.as_deref())?;
            ctx.info(&format!("added label {}", name.trim()));
            ctx.emit_mutation(&Report::object(
                "label",
                to_record(&json!({"id": id, "name": name.trim(), "description": description})),
            ))
        }
        LabelCmd::Ls => {
            let counts = store::all_item_labels(&db)?;
            let rows = store::list_labels(&db)?
                .iter()
                .map(|l| {
                    let n = counts.values().filter(|v| v.contains(&l.name)).count();
                    to_record(&json!({"name": l.name, "items": n, "description": l.description}))
                })
                .collect();
            ctx.emit(&Report::list("labels", rows).table_columns(&["name", "items", "description"]))
        }
        LabelCmd::Rm { name } => {
            let l = store::find_label(&db, &name)?.ok_or_else(|| AppError::not_found(format!("no label '{name}'")))?;
            store::delete_label(&db, l.id)?;
            ctx.info(&format!("removed label {}", l.name));
            ctx.emit_mutation(&Report::object("removed", to_record(&json!({"label": l.name}))))
        }
    }
}
