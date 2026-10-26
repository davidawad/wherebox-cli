//! The `[encryption]` section wherebox keeps at the end of its config file:
//! for every database it has opened, which keys can decrypt it and how to
//! get the data back on another machine, in plain comments and as data.
//! Settings above the section are the user's; the section itself is
//! rewritten whenever a database's keys change, and the setting loader
//! ignores it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::container::{Header, Holder, SlotKind};
use crate::crypto::hex;
use crate::error::{AppError, Result};
use crate::{keyfile, keys};

pub const MARKER: &str = "# ---- wherebox-cli encryption: maintained by wherebox, rewritten when keys change ----";

/// Split a config file into the user's part and the maintained section.
pub fn split(text: &str) -> (&str, &str) {
    match text.find(MARKER) {
        Some(i) => (&text[..i], &text[i..]),
        None => (text, ""),
    }
}

/// One database's entry: (database path, slot descriptions, recovery advice).
type Entry = (String, Vec<String>, String);

fn read_entries(block: &str) -> BTreeMap<String, Entry> {
    let Ok(table) = block.parse::<toml::Table>() else { return BTreeMap::new() };
    let Some(toml::Value::Table(enc)) = table.get("encryption") else { return BTreeMap::new() };
    enc.iter()
        .filter_map(|(id, v)| {
            let db = v.get("db")?.as_str()?.to_string();
            let slots = v.get("opens_with")?.as_array()?.iter().filter_map(|s| s.as_str().map(String::from)).collect();
            let recover = v.get("recover").and_then(|r| r.as_str()).unwrap_or("").to_string();
            Some((id.clone(), (db, slots, recover)))
        })
        .collect()
}

/// Plain-English recovery advice for a database's slots.
pub fn recovery(db: &str, h: &Header) -> String {
    let first = |f: fn(&SlotKind) -> bool| h.slots.iter().map(|s| &s.kind).find(|k| f(k));
    if let Some(SlotKind::Ssh { identity, .. }) = first(|k| matches!(k, SlotKind::Ssh { .. })) {
        return format!("on another machine: copy {identity} and {db} there, then run `wherebox doctor`");
    }
    if first(|k| matches!(k, SlotKind::Raw(Holder::File))).is_some() {
        let kf = keyfile::path_for(&h.db_id);
        return format!(
            "on another machine: copy the key file {} and {db} there (keep the key file out of the database's backups)",
            kf.display()
        );
    }
    if first(|k| matches!(k, SlotKind::Passphrase(_))).is_some() {
        return format!("copy {db} anywhere and enter its passphrase");
    }
    if first(|k| matches!(k, SlotKind::Raw(Holder::Env))).is_some() {
        return format!("set {} to the same raw key wherever {db} is opened", keys::ENV_KEY);
    }
    format!("this is a 0.3-era database: {}", keys::LEGACY_HELP)
}

fn quote(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn render(entries: &BTreeMap<String, Entry>) -> String {
    let mut out = vec![
        MARKER.to_string(),
        "# Your databases are encrypted. Each opens with ANY ONE of the keys listed for it;".into(),
        "# lose all of them and its data cannot be recovered.".into(),
    ];
    for (id, (db, slots, recover)) in entries {
        out.push("#".into());
        out.push(format!("# {db}"));
        out.extend(slots.iter().map(|s| format!("#   opens with: {s}")));
        out.push(format!("#   recover: {recover}"));
        out.push(format!("[encryption.{}]", quote(id)));
        out.push(format!("db = {}", quote(db)));
        let list = slots.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", ");
        out.push(format!("opens_with = [{list}]"));
        out.push(format!("recover = {}", quote(recover)));
    }
    out.join("\n") + "\n"
}

/// The config file text with `header`'s database recorded in the section.
pub fn updated(text: &str, db: &Path, header: &Header) -> String {
    let (head, block) = split(text);
    let mut entries = read_entries(block);
    let id = hex(&header.db_id);
    let db_s = db.display().to_string();
    let advice = recovery(&db_s, header);
    entries.insert(id, (db_s, keys::describe_slots(header), advice));
    let head = head.trim_end();
    let sep = if head.is_empty() { "" } else { "\n\n" };
    format!("{head}{sep}{}", render(&entries))
}

/// Record `header`'s database in the config file at `config` (written only
/// when something changed). Returns whether the file was written.
pub fn sync(config: &Path, db: &Path, header: &Header) -> Result<bool> {
    let text = std::fs::read_to_string(config).unwrap_or_default();
    let (_, old_block) = split(&text);
    let new = updated(&text, db, header);
    let (_, new_block) = split(&new);
    if same_entry(old_block, new_block, &hex(&header.db_id)) {
        return Ok(false);
    }
    crate::config::ensure_file(config)?;
    std::fs::write(config, new).map_err(|e| AppError::io(format!("writing {}: {e}", config.display())))?;
    Ok(true)
}

fn same_entry(old: &str, new: &str, id: &str) -> bool {
    !old.is_empty() && read_entries(old).get(id) == read_entries(new).get(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::Slot;
    use crate::crypto::{DataKeys, Key};

    fn header(id: u8) -> Header {
        let (keys, kek) = (DataKeys::random().unwrap(), Key::random().unwrap());
        let db_id = [id; 16];
        Header::new(db_id, vec![Slot::with_kek(SlotKind::Raw(Holder::Env), &db_id, &kek, &keys).unwrap()])
    }

    #[test]
    fn keeps_user_settings_and_one_entry_per_database() {
        let user = "format = \"csv\"\n[csv]\ndelimiter = \";\"\n";
        let a = updated(user, Path::new("/data/a.db"), &header(1));
        assert!(a.starts_with(user.trim_end()));
        assert!(a.contains("#   recover: set WHEREBOX_KEY"));
        let b = updated(&a, Path::new("/data/b.db"), &header(2));
        let again = updated(&b, Path::new("/data/b.db"), &header(2));
        assert_eq!(b, again, "stable when nothing changed");
        assert_eq!(b.matches(MARKER).count(), 1);
        assert_eq!(read_entries(split(&b).1).len(), 2);
        // still a valid config: the loader skips the section
        assert!(crate::config::parse_toml_layer(&b).unwrap().iter().all(|(k, _)| !k.starts_with("encryption")));
    }

    #[test]
    fn sync_writes_only_on_change() {
        let t = tempfile::TempDir::new().unwrap();
        let cfg = t.path().join("conf").join("config.toml");
        let h = header(3);
        assert!(sync(&cfg, Path::new("x.db"), &h).unwrap());
        assert!(!sync(&cfg, Path::new("x.db"), &h).unwrap());
    }
}
