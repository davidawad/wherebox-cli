//! `key status | add-ssh | add-passphrase | add-file | remove`: the key
//! slots that can open the database.

use serde_json::json;

use crate::cli::KeyCmd;
use crate::container::{Header, Holder, SlotKind};
use crate::context::Ctx;
use crate::error::{AppError, Result};
use crate::keys::{self, ENV_NEW_KEY};
use crate::keysetup::{self, Setup};
use crate::output::{to_record, Report};
use crate::perms::{self, Access};
use crate::prompt::Terminal;
use crate::vault::{self, Edit, State};
use crate::{keyfile, sshkey};

/// (status, detail) for one slot, checked against this machine without
/// unlocking anything.
fn slot_state(h: &Header, kind: &SlotKind) -> (&'static str, String) {
    match kind {
        SlotKind::Ssh { fingerprint, identity, .. } => {
            let path = sshkey::override_path().unwrap_or_else(|| identity.into());
            let here = if path.exists() { "present" } else { "not on this machine" };
            (if path.exists() { "ok" } else { "warn" }, format!("{fingerprint} {} ({here})", path.display()))
        }
        SlotKind::Raw(Holder::File) => key_file_state(h),
        SlotKind::Raw(Holder::Env) => {
            let set = std::env::var_os(keys::ENV_KEY).is_some_and(|v| !v.is_empty());
            ("ok", format!("{}=raw:<hex> ({})", keys::ENV_KEY, if set { "set" } else { "not set" }))
        }
        SlotKind::Raw(Holder::Legacy) => ("warn", format!("0.3-era key: {}", keys::LEGACY_HELP)),
        SlotKind::Passphrase(p) => ("ok", format!("Argon2id m={}KiB t={} p={}", p.m_cost, p.t_cost, p.p_cost)),
    }
}

fn key_file_state(h: &Header) -> (&'static str, String) {
    let path = keyfile::path_for(&h.db_id);
    if !keyfile::exists(&h.db_id) {
        return ("warn", format!("{} (not on this machine)", path.display()));
    }
    match (keyfile::check_private(&path), perms::inspect(&path)) {
        (Err(e), _) => ("fail", e.message),
        (Ok(()), Some(Access::Private(d))) => ("ok", format!("{} ({d})", path.display())),
        (Ok(()), _) => ("ok", path.display().to_string()),
    }
}

/// `(slot number, kind, status, detail)` for every slot.
pub fn slot_rows(h: &Header) -> Vec<(usize, &'static str, &'static str, String)> {
    h.slots
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (status, detail) = slot_state(h, &s.kind);
            (i + 1, s.kind.name(), status, detail)
        })
        .collect()
}

fn sealed_header(ctx: &Ctx) -> Result<Header> {
    match vault::state(&ctx.db_path)? {
        State::Sealed(h) => Ok(h),
        State::Missing => Err(AppError::not_found(format!("no database at {}", ctx.db_path.display()))),
        State::Plain => Err(AppError::invalid(format!("{} is not encrypted", ctx.db_path.display()))),
    }
}

fn status(ctx: &Ctx) -> Result<()> {
    let h = sealed_header(ctx)?;
    let db = ctx.db_path.display().to_string();
    let mut rows: Vec<_> = slot_rows(&h)
        .into_iter()
        .map(|(n, kind, status, detail)| {
            to_record(&json!({"slot": n, "kind": kind, "status": status, "detail": detail}))
        })
        .collect();
    rows.push(to_record(&json!({"slot": null, "kind": "recover", "status": "info",
        "detail": crate::enc_config::recovery(&db, &h)})));
    rows.push(to_record(&json!({"slot": null, "kind": "config", "status": "info",
        "detail": format!("recorded in {}", ctx.resolved.config_path.display())})));
    ctx.emit(&Report::list("key_status", rows).table_columns(&["slot", "kind", "status", "detail"]))
}

fn edit(
    ctx: &Ctx,
    what: &str,
    f: impl FnOnce(&Header, &crate::crypto::DataKeys, &mut dyn crate::prompt::Prompter) -> Result<Edit>,
) -> Result<()> {
    let (h, from) = vault::change_slots(&ctx.db_path, &ctx.open_opts()?, &mut Terminal, f)?;
    ctx.record_keys(&h);
    ctx.info(&format!("{what} ({} key slots; unlocked with {from})", h.slots.len()));
    ctx.emit_mutation(&Report::object(
        "key_change",
        to_record(&json!({"path": ctx.db_path, "change": what, "slots": keys::describe_slots(&h)})),
    ))
}

pub fn run(ctx: &Ctx, cmd: KeyCmd) -> Result<()> {
    let config = ctx.resolved.config_path.clone();
    let db = ctx.db_path.display().to_string();
    match cmd {
        KeyCmd::Status => status(ctx),
        KeyCmd::AddSsh { public_key } => edit(ctx, "added an SSH key", |h, k, _| {
            let mut slots = h.slots.clone();
            slots.push(keysetup::public_ssh_slot(&public_key, k)?);
            Ok(Edit { slots, staged: None })
        }),
        KeyCmd::AddPassphrase => edit(ctx, "added a passphrase", |h, k, p| {
            let mut s = Setup { what: &db, config: &config, prompter: p };
            let mut slots = h.slots.clone();
            slots.push(keysetup::new_passphrase_slot(ENV_NEW_KEY, h.db_id, k, &mut s)?);
            Ok(Edit { slots, staged: None })
        }),
        KeyCmd::AddFile => edit(ctx, "added a key file", |h, k, _| {
            if h.slots.iter().any(|s| s.kind == SlotKind::Raw(Holder::File)) {
                return Err(AppError::invalid("the database already has a key file"));
            }
            let ns = keysetup::file_slot(h.db_id, k)?;
            Ok(Edit { slots: [h.slots.clone(), ns.slots.clone()].concat(), staged: Some(ns) })
        }),
        KeyCmd::Remove { slot } => edit(ctx, &format!("removed key slot {slot}"), |h, _, _| {
            let i = slot.checked_sub(1).filter(|i| *i < h.slots.len());
            let i = i.ok_or_else(|| AppError::invalid(format!("no key slot {slot} (see `wherebox key status`)")))?;
            let mut slots = h.slots.clone();
            slots.remove(i);
            Ok(Edit { slots, staged: None })
        }),
    }
}
