//! Making key slots: a new database's first key (the user's SSH key by
//! default, announced and confirmed), rekey targets, and `key add-*`.

use std::path::Path;

use crate::container::{Holder, Slot, SlotKind};
use crate::crypto::{derive_kek, DataKeys, KdfParams, Key, ID_LEN};
use crate::error::Result;
use crate::keyfile;
use crate::keys::{key_error, parse_env_key, EnvKey, Source};
use crate::prompt::Prompter;
use crate::sshkey::{self, SshKey};

/// Who is asking, for notices and prompts.
pub struct Setup<'a> {
    /// The database, as shown to the user.
    pub what: &'a str,
    /// The config file that records how the database opens.
    pub config: &'a Path,
    pub prompter: &'a mut dyn Prompter,
}

/// New slots for one database, plus what must be committed once the
/// container sealed with them is on disk.
pub struct NewSlots {
    pub slots: Vec<Slot>,
    db_id: [u8; ID_LEN],
    staged_file: bool,
}

impl NewSlots {
    fn one(slot: Slot, db_id: [u8; ID_LEN]) -> Self {
        Self { slots: vec![slot], db_id, staged_file: false }
    }

    /// Kinds of the new slots, e.g. "ssh+file".
    pub fn label(&self) -> String {
        self.slots.iter().map(|s| s.kind.name()).collect::<Vec<_>>().join("+")
    }

    /// Make a staged key file live. Call after the container
    /// sealed with these slots has been durably written.
    pub fn commit(&self) -> Result<()> {
        if self.staged_file {
            keyfile::commit(&self.db_id)?;
        }
        Ok(())
    }
}

/// A `file` slot with a new random key file (staged).
pub fn file_slot(db_id: [u8; ID_LEN], keys: &DataKeys) -> Result<NewSlots> {
    let kek = Key::random()?;
    let slot = Slot::with_kek(SlotKind::Raw(Holder::File), &db_id, &kek, keys)?;
    keyfile::stage(&db_id, &kek)?;
    Ok(NewSlots { staged_file: true, ..NewSlots::one(slot, db_id) })
}

/// An `ssh` slot for `key`, plus a `file` slot when the key is passphrase-protected.
fn ssh_slots(key: &SshKey, db_id: [u8; ID_LEN], keys: &DataKeys) -> Result<NewSlots> {
    let ssh = Slot::ssh(&key.recipient, key.fingerprint.clone(), key.identity.display().to_string(), keys)?;
    if !key.encrypted {
        return Ok(NewSlots::one(ssh, db_id));
    }
    let mut ns = file_slot(db_id, keys)?;
    ns.slots.insert(0, ssh);
    Ok(ns)
}

/// An `ssh` slot for a public key given on the command line (`key add-ssh`).
pub fn public_ssh_slot(arg: &str, keys: &DataKeys) -> Result<Slot> {
    let (recipient, fp, identity) = sshkey::parse_public(arg)?;
    Slot::ssh(&recipient, fp, identity, keys)
}

fn passphrase_slot(pass: &[u8], db_id: [u8; ID_LEN], keys: &DataKeys) -> Result<Slot> {
    let params = KdfParams::fresh()?;
    Slot::with_kek(SlotKind::Passphrase(params), &db_id, &derive_kek(pass, &params)?, keys)
}

/// A passphrase slot from `env_var`, else asked twice.
pub fn new_passphrase_slot(env_var: &str, db_id: [u8; ID_LEN], keys: &DataKeys, s: &mut Setup) -> Result<Slot> {
    if let Some(EnvKey::Passphrase(p)) = parse_env_key(env_var)? {
        return passphrase_slot(p.as_bytes(), db_id, keys);
    }
    if !s.prompter.interactive() {
        return Err(key_error(format!("a new passphrase is needed: set {env_var} or run in a terminal")));
    }
    let a = s.prompter.secret(&format!("New passphrase for {}: ", s.what))?;
    if a.chars().count() < 8 {
        return Err(key_error("passphrase must be at least 8 characters"));
    }
    if *s.prompter.secret("Repeat passphrase: ")? != *a {
        return Err(key_error("passphrases do not match"));
    }
    passphrase_slot(a.as_bytes(), db_id, keys)
}

fn env_slot(env_var: &str, db_id: [u8; ID_LEN], keys: &DataKeys) -> Result<NewSlots> {
    match parse_env_key(env_var)? {
        Some(EnvKey::Raw(kek)) => {
            Ok(NewSlots::one(Slot::with_kek(SlotKind::Raw(Holder::Env), &db_id, &kek, keys)?, db_id))
        }
        Some(EnvKey::Passphrase(p)) => Ok(NewSlots::one(passphrase_slot(p.as_bytes(), db_id, keys)?, db_id)),
        None => Err(key_error(format!("key_source is env but {env_var} is not set"))),
    }
}

fn recover_line(s: &Setup, how: &str) -> String {
    format!(
        "  How to open it again is written to {} (the [encryption] section). {how} Without that key the data cannot be recovered.",
        s.config.display()
    )
}

fn ssh_notice(key: &SshKey, db_id: &[u8; ID_LEN], s: &Setup) -> String {
    let mut lines = vec![format!(
        "wherebox: encrypting {} with your SSH key {} ({}).",
        s.what,
        key.identity.display(),
        key.fingerprint
    )];
    if key.encrypted {
        lines.push(format!(
            "  That key has a passphrase, so a key file ({}) opens the database day to day; the SSH key is the recovery key.",
            keyfile::path_for(db_id).display()
        ));
    }
    let how = format!(
        "On another machine: copy {} and the database there, then run `wherebox doctor`.",
        key.identity.display()
    );
    lines.push(recover_line(s, &how));
    lines.join("\n")
}

fn file_notice(db_id: &[u8; ID_LEN], s: &Setup, why: &str) -> String {
    let path = keyfile::path_for(db_id);
    let how = format!("Back up {} separately from the database.", path.display());
    format!("wherebox: {why}; encrypting {} with a new key file {}.\n{}", s.what, path.display(), recover_line(s, &how))
}

/// `auto` for a new database: the user's SSH key, confirmed on a terminal;
/// otherwise (no usable key, or declined) a key file.
fn auto_slots(db_id: [u8; ID_LEN], keys: &DataKeys, s: &mut Setup) -> Result<NewSlots> {
    let (found, skipped) = sshkey::find();
    let why = match found {
        Some(key) => {
            s.prompter.notice(&ssh_notice(&key, &db_id, s));
            if !s.prompter.interactive() || s.prompter.confirm("Encrypt with this SSH key?")? {
                return ssh_slots(&key, db_id, keys);
            }
            "SSH key declined".to_string()
        }
        None if skipped.is_empty() => "no SSH key found (~/.ssh/id_ed25519, ~/.ssh/id_rsa)".to_string(),
        None => format!("no usable SSH key ({})", skipped.join("; ")),
    };
    s.prompter.notice(&file_notice(&db_id, s, &why));
    file_slot(db_id, keys)
}

/// Key slots for a new database (or a rekey target): `source` picks the
/// kind; `auto` uses `env_var` when set, else [`auto_slots`].
pub fn new_slots(
    source: Source,
    env_var: &str,
    db_id: [u8; ID_LEN],
    keys: &DataKeys,
    s: &mut Setup,
) -> Result<NewSlots> {
    let env_set = std::env::var_os(env_var).is_some_and(|v| !v.is_empty());
    match source {
        Source::Auto if env_set => env_slot(env_var, db_id, keys),
        Source::Auto => auto_slots(db_id, keys, s),
        Source::Env => env_slot(env_var, db_id, keys),
        Source::File => file_slot(db_id, keys),
        Source::Passphrase => Ok(NewSlots::one(new_passphrase_slot(env_var, db_id, keys, s)?, db_id)),
        Source::Ssh => match sshkey::find() {
            (Some(key), _) => {
                s.prompter.notice(&ssh_notice(&key, &db_id, s));
                ssh_slots(&key, db_id, keys)
            }
            (None, skipped) => Err(key_error(format!(
                "key_source is ssh but no usable SSH key was found ({}); set ssh_key / WHEREBOX_SSH_KEY",
                if skipped.is_empty() {
                    "tried ~/.ssh/id_ed25519, ~/.ssh/id_rsa".to_string()
                } else {
                    skipped.join("; ")
                }
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Scripted;
    use crate::sshkey::testkeys;

    struct Env {
        _t: tempfile::TempDir,
        dir: std::path::PathBuf,
    }

    fn env() -> Env {
        let t = tempfile::TempDir::new().unwrap();
        let dir = t.path().to_path_buf();
        keyfile::set_override(Some(dir.join("keys").join("db.key")));
        Env { _t: t, dir }
    }

    fn run(input: &'static str, interactive: bool) -> (NewSlots, String) {
        let mut p = Scripted { input: input.as_bytes(), output: Vec::new(), interactive };
        let cfg = Path::new("config.toml");
        let keys = DataKeys::random().unwrap();
        let mut s = Setup { what: "test.db", config: cfg, prompter: &mut p };
        let ns = new_slots(Source::Auto, "WHEREBOX_TEST_UNSET_KEY", [6; ID_LEN], &keys, &mut s).unwrap();
        (ns, String::from_utf8(p.output).unwrap())
    }

    #[test]
    fn auto_uses_the_ssh_key_and_says_so() {
        let e = env();
        let (key, fp) = testkeys::ed25519(&e.dir, "id_ed25519", None);
        sshkey::set_override(Some(key));
        let (ns, out) = run("", false);
        assert_eq!(ns.label(), "ssh");
        assert!(out.contains(&fp) && out.contains("config.toml") && out.contains("wherebox doctor"), "{out}");
        let (ns, out) = run("y\n", true);
        assert_eq!(ns.label(), "ssh");
        assert!(out.contains("Encrypt with this SSH key? [Y/n]"));
        let (ns, out) = run("n\n", true);
        assert_eq!(ns.label(), "file", "declined: key file instead");
        assert!(out.contains("SSH key declined"));
        sshkey::set_override(None);
        keyfile::set_override(None);
    }

    #[test]
    fn protected_ssh_key_adds_a_daily_key_file() {
        let e = env();
        let (key, _) = testkeys::ed25519(&e.dir, "id_ed25519", Some("ssh-pass"));
        sshkey::set_override(Some(key));
        let (ns, out) = run("", false);
        assert_eq!(ns.label(), "ssh+file");
        assert!(out.contains("recovery key"), "{out}");
        ns.commit().unwrap();
        assert!(keyfile::exists(&[6; ID_LEN]));
        sshkey::set_override(None);
        keyfile::set_override(None);
    }

    #[test]
    fn no_ssh_key_means_a_key_file() {
        let e = env();
        sshkey::set_override(Some(e.dir.join("missing_id")));
        let (ns, out) = run("", false);
        assert_eq!(ns.label(), "file");
        assert!(out.contains("no SSH key found") && out.contains("Back up"), "{out}");
        sshkey::set_override(None);
        keyfile::set_override(None);
    }
}
