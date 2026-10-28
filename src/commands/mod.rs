//! Command implementations.

pub mod attach;
pub mod config_cmd;
pub mod db_cmd;
pub mod export;
pub mod homebox;
pub mod import;
pub mod items;
pub mod key_cmd;
pub mod label;
pub mod list_cmd;
pub mod loc;
pub mod lots;
pub mod misc;
pub mod preset;
pub mod profile;
pub mod receipt_cmd;
pub mod regroup;
pub mod report;
pub mod security;
pub mod shelf_life_cmd;
pub mod shopping;
pub mod store_cmd;

use crate::cli::Command;
use crate::context::Ctx;
use crate::error::Result;

/// Process exit status requested by a successful command.
pub type Status = i32;

pub fn dispatch(ctx: &Ctx, cmd: Command) -> Result<Status> {
    match cmd {
        Command::List(c) => return list_cmd::run(ctx, c),
        Command::Cohort(c) => list_cmd::cohort(ctx, c),
        Command::Match(a) => list_cmd::match_cmd(ctx, a),
        Command::Store(c) => store_cmd::store(ctx, c),
        Command::Source(c) => store_cmd::source(ctx, c),
        Command::Receipt(c) => receipt_cmd::run(ctx, c),
        Command::Add(a) => items::add(ctx, a),
        Command::Edit(a) => items::edit(ctx, a),
        Command::Mv(a) => items::mv(ctx, a),
        Command::Rm(a) => items::rm(ctx, a),
        Command::Show { item } => items::show(ctx, &item),
        Command::Ls(a) => items::ls(ctx, a),
        Command::Find(a) => items::find(ctx, a),
        Command::History { item } => items::history(ctx, &item),
        Command::Value(a) => items::value(ctx, a),
        Command::Loc(c) => loc::run(ctx, c),
        Command::Label(c) => label::run(ctx, c),
        Command::Attach(a) => attach::attach(ctx, a),
        Command::Attachment(c) => attach::run(ctx, c),
        Command::Lot(c) => lots::run(ctx, c),
        Command::Use(a) => lots::consume(ctx, a, "used"),
        Command::Eat(a) => lots::consume(ctx, a, "eaten"),
        Command::Toss(a) => lots::toss(ctx, a),
        Command::Expiring(a) => lots::expiring(ctx, a),
        Command::Preset(c) => preset::run(ctx, c),
        Command::ShelfLife(c) => shelf_life_cmd::run(ctx, c),
        Command::Par(a) => shopping::par(ctx, a),
        Command::Shopping(a) => shopping::run(ctx, a),
        Command::Report(c) => report::run(ctx, c),
        Command::Import(a) => import::run(ctx, a),
        Command::Export(a) => export::run(ctx, a),
        Command::Profile(c) => profile::run(ctx, c),
        Command::Db(c) => db_cmd::run(ctx, c),
        Command::Config(c) => config_cmd::run(ctx, c),
        Command::Audit(c) => security::audit(ctx, c),
        Command::Key(c) => key_cmd::run(ctx, c),
        Command::Doctor => security::doctor(ctx),
        Command::Completions(a) => misc::completions(a),
        Command::Man(a) => misc::man(a),
    }
    .map(|()| 0)
}
