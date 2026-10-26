//! Where database keys come from, and unlocking a container's key slots.
//!
//! Every database has random data keys (DEK + audit key) wrapped in one or
//! more *key slots* (see [`crate::container`]); any one slot opens it. The
//! `key_source` setting / `WHEREBOX_KEY_SOURCE` picks how new keys are made
//! (see [`crate::keysetup`]) and which slots are tried:
//!
//! * `auto` (default): new databases get an `ssh` slot for the user's SSH key
//!   (plus a `file` slot when that key has a passphrase), or a `file` slot
//!   when there is no SSH key; `WHEREBOX_KEY`, when set, makes an `env`
//!   slot instead. Unlocking tries the key file, then the SSH key (silently,
//!   or with its passphrase), then `WHEREBOX_KEY`, then a passphrase prompt.
//! * `ssh`, `file`, `env`, `passphrase`: only that kind.
//!
//! wherebox never uses the OS keychain. Databases from 0.3 and earlier whose
//! key was kept there must be moved to an SSH key first with a build that
//! still reads it (see [`LEGACY_HELP`]).

use zeroize::Zeroizing;

use crate::container::{Header, Holder, Slot, SlotKind};
use crate::crypto::{derive_kek, unhex, DataKeys, Key, KEY_LEN};
use crate::error::{AppError, ErrorKind, Result};
use crate::prompt::Prompter;
use crate::{keyfile, sshkey};

pub const ENV_KEY: &str = "WHEREBOX_KEY";
pub const ENV_NEW_KEY: &str = "WHEREBOX_NEW_KEY";

/// What to do with a database whose only key is a 0.3-era raw key.
pub const LEGACY_HELP: &str = "it was made by wherebox 0.3 or earlier, whose key lived in the OS keychain, which \
wherebox no longer uses: set WHEREBOX_KEY=raw:<its key>, or move it to your SSH key with wherebox 0.3 \
(`wherebox db rekey --to passphrase`) and then `wherebox key add-ssh`";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Auto,
    Ssh,
    File,
    Env,
    Passphrase,
}

const ALL: [Source; 5] = [Source::Auto, Source::Ssh, Source::File, Source::Env, Source::Passphrase];

impl Source {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "" => Ok(Self::Auto),
            s => ALL
                .into_iter()
                .find(|x| x.as_str() == s)
                .ok_or_else(|| AppError::config(format!("unknown key source '{s}'"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ssh => "ssh",
            Self::File => "file",
            Self::Env => "env",
            Self::Passphrase => "passphrase",
        }
    }

    /// Unlock steps, in order.
    fn unlock_order(self) -> &'static [Self] {
        match self {
            Self::Auto => &[Self::File, Self::Ssh, Self::Env, Self::Passphrase],
            Self::Ssh => &[Self::Ssh],
            Self::File => &[Self::File],
            Self::Env => &[Self::Env],
            Self::Passphrase => &[Self::Passphrase],
        }
    }
}

pub fn key_error(m: impl Into<String>) -> AppError {
    AppError::new(ErrorKind::Key, m)
}

/// A value of `WHEREBOX_KEY`-style variables.
pub enum EnvKey {
    Raw(Key),
    Passphrase(Zeroizing<String>),
}

pub fn parse_env_key(var: &str) -> Result<Option<EnvKey>> {
    let Some(v) = std::env::var_os(var) else { return Ok(None) };
    let v = Zeroizing::new(v.to_string_lossy().into_owned());
    if v.is_empty() {
        return Ok(None);
    }
    if let Some(h) = v.strip_prefix("raw:") {
        let bytes = Zeroizing::new(unhex(h).unwrap_or_default());
        return Key::from_bytes(&bytes)
            .map(|k| Some(EnvKey::Raw(k)))
            .ok_or_else(|| key_error(format!("{var}=raw:… must be {} hex characters", 2 * KEY_LEN)));
    }
    Ok(Some(EnvKey::Passphrase(v)))
}

/// Result of unlocking a container.
pub struct Unlocked {
    pub keys: DataKeys,
    /// What opened it: "file", "ssh", "env" or "passphrase".
    pub source: &'static str,
}

/// One unlock attempt over a header's slots.
struct Attempt<'a> {
    header: &'a Header,
    what: &'a str,
    allow_prompt: bool,
    prompter: &'a mut dyn Prompter,
    tried: Vec<&'static str>,
}

fn is_raw(s: &Slot) -> bool {
    matches!(s.kind, SlotKind::Raw(_))
}

impl Attempt<'_> {
    /// Try `kek` on every slot accepted by `fits`.
    fn with_kek(&self, kek: Key, fits: fn(&Slot) -> bool, src: &'static str) -> Option<Unlocked> {
        let id = &self.header.db_id;
        let keys = self.header.slots.iter().filter(|s| fits(s)).find_map(|s| s.open_kek(id, &kek))?;
        Some(Unlocked { keys, source: src })
    }

    fn file(&mut self) -> Result<Option<Unlocked>> {
        if !self.header.slots.iter().any(is_raw) {
            return Ok(None);
        }
        let keks = keyfile::load(&self.header.db_id)?;
        if !keks.is_empty() {
            self.tried.push("key file");
        }
        Ok(keks.into_iter().find_map(|k| self.with_kek(k, is_raw, "file")))
    }

    fn ssh(&mut self) -> Result<Option<Unlocked>> {
        for slot in &self.header.slots {
            let SlotKind::Ssh { identity, .. } = &slot.kind else { continue };
            let path = sshkey::override_path().unwrap_or_else(|| identity.into());
            let Some(id) = sshkey::identity(&path, self.prompter, self.allow_prompt)? else { continue };
            self.tried.push("ssh key");
            if let Some(keys) = slot.open_identity(id.as_ref()) {
                return Ok(Some(Unlocked { keys, source: "ssh" }));
            }
        }
        Ok(None)
    }

    fn env(&mut self) -> Result<Option<Unlocked>> {
        let Some(k) = parse_env_key(ENV_KEY)? else { return Ok(None) };
        self.tried.push(ENV_KEY);
        Ok(match k {
            EnvKey::Raw(kek) => self.with_kek(kek, is_raw, "env"),
            EnvKey::Passphrase(p) => self.passphrase_slots(p.as_bytes(), "env")?,
        })
    }

    fn passphrase_slots(&self, pass: &[u8], src: &'static str) -> Result<Option<Unlocked>> {
        for slot in &self.header.slots {
            if let SlotKind::Passphrase(params) = &slot.kind {
                let kek = derive_kek(pass, params)?;
                if let Some(keys) = slot.open_kek(&self.header.db_id, &kek) {
                    return Ok(Some(Unlocked { keys, source: src }));
                }
            }
        }
        Ok(None)
    }

    fn prompt(&mut self) -> Result<Option<Unlocked>> {
        let has = self.header.slots.iter().any(|s| matches!(s.kind, SlotKind::Passphrase(_)));
        if !(has && self.allow_prompt && self.prompter.interactive()) {
            return Ok(None);
        }
        let p = self.prompter.secret(&format!("Passphrase for {}: ", self.what))?;
        match self.passphrase_slots(p.as_bytes(), "passphrase")? {
            Some(u) => Ok(Some(u)),
            None => Err(key_error(format!("wrong passphrase for {}", self.what))),
        }
    }

    fn step(&mut self, s: Source) -> Result<Option<Unlocked>> {
        match s {
            Source::File => self.file(),
            Source::Ssh => self.ssh(),
            Source::Env => self.env(),
            Source::Passphrase => self.prompt(),
            Source::Auto => Ok(None),
        }
    }

    fn failure(&self) -> AppError {
        let legacy_only = self.header.slots.iter().all(|s| s.kind == SlotKind::Raw(Holder::Legacy));
        if legacy_only && self.tried.is_empty() {
            return key_error(format!("cannot open {}: {LEGACY_HELP}", self.what));
        }
        if !self.tried.is_empty() {
            return key_error(format!("wrong key for {} (tried: {})", self.what, self.tried.join(", ")));
        }
        key_error(format!(
            "{} is encrypted and none of its keys is available here ({}); `wherebox key status` lists them",
            self.what,
            describe_slots(self.header).join("; ")
        ))
    }
}

/// One line per slot, for errors and the config file.
pub fn describe_slots(h: &Header) -> Vec<String> {
    h.slots
        .iter()
        .map(|s| match &s.kind {
            SlotKind::Ssh { fingerprint, identity, .. } => format!("SSH key {identity} ({fingerprint})"),
            SlotKind::Raw(Holder::File) => format!("key file {}", keyfile::path_for(&h.db_id).display()),
            SlotKind::Raw(Holder::Env) => format!("{ENV_KEY}=raw:<hex>"),
            SlotKind::Raw(Holder::Legacy) => format!("{ENV_KEY}=raw:<hex> (a 0.3-era key)"),
            SlotKind::Passphrase(_) => "a passphrase".into(),
        })
        .collect()
}

/// Unlock a container header. Prompts (SSH key passphrase, database
/// passphrase) only if `allow_prompt` and `p` is interactive.
pub fn unlock(
    source: Source,
    header: &Header,
    what: &str,
    allow_prompt: bool,
    p: &mut dyn Prompter,
) -> Result<Unlocked> {
    let mut a = Attempt { header, what, allow_prompt, prompter: p, tried: Vec::new() };
    for s in source.unlock_order() {
        if let Some(u) = a.step(*s)? {
            return Ok(u);
        }
    }
    Err(a.failure())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KdfParams;
    use crate::prompt::Scripted;
    use crate::sshkey::testkeys;

    fn quiet() -> Scripted<&'static [u8], Vec<u8>> {
        Scripted { input: b"", output: Vec::new(), interactive: false }
    }

    #[test]
    fn source_names_round_trip() {
        for s in ALL {
            assert_eq!(Source::parse(s.as_str()).unwrap(), s);
        }
        assert!(Source::parse("nope").is_err());
    }

    #[test]
    fn key_file_wins_before_a_protected_ssh_key_is_asked_about() {
        let t = tempfile::TempDir::new().unwrap();
        let (sshp, fp) = testkeys::ed25519(&t.path().join(".ssh"), "id_ed25519", Some("pw-for-ssh"));
        keyfile::set_override(Some(t.path().join("db.key")));
        sshkey::set_override(None);
        let (id, keys) = ([1u8; 16], DataKeys::random().unwrap());
        let k = sshkey::load(&sshp).unwrap();
        let file_kek = Key::random().unwrap();
        keyfile::stage(&id, &file_kek).unwrap();
        keyfile::commit(&id).unwrap();
        let h = Header::new(
            id,
            vec![
                Slot::ssh(&k.recipient, fp, sshp.display().to_string(), &keys).unwrap(),
                Slot::with_kek(SlotKind::Raw(Holder::File), &id, &file_kek, &keys).unwrap(),
            ],
        );
        // interactive, but the key file opens it before any prompt
        let mut p = Scripted { input: b"" as &[u8], output: Vec::new(), interactive: true };
        assert_eq!(unlock(Source::Auto, &h, "db", true, &mut p).unwrap().source, "file");
        assert!(p.output.is_empty(), "no prompt");
        // key file gone: the SSH key's passphrase is asked for
        keyfile::forget(&id);
        let mut p = Scripted { input: b"pw-for-ssh\n" as &[u8], output: Vec::new(), interactive: true };
        assert_eq!(unlock(Source::Auto, &h, "db", true, &mut p).unwrap().source, "ssh");
        // no terminal and no key file: a clear error, nothing touched
        let e = unlock(Source::Auto, &h, "db", true, &mut quiet()).err().unwrap();
        assert!(e.message.contains("SSH key"), "{}", e.message);
        keyfile::set_override(None);
    }

    #[test]
    fn a_0_3_keychain_database_says_what_to_do() {
        let (id, keys, kek) = ([3u8; 16], DataKeys::random().unwrap(), Key::random().unwrap());
        let h = Header::legacy_v1(id, None, &kek, &keys).unwrap();
        let e = unlock(Source::Auto, &h, "old.db", true, &mut quiet()).err().unwrap();
        assert!(e.message.contains("0.3 or earlier") && e.message.contains("no longer uses"), "{}", e.message);
    }

    #[test]
    fn explicit_sources_only_try_their_slot() {
        let (id, keys) = ([2u8; 16], DataKeys::random().unwrap());
        let params = KdfParams { m_cost: 8, t_cost: 1, p_cost: 1, salt: [4; 16] };
        let kek = derive_kek(b"correct horse", &params).unwrap();
        let h = Header::new(id, vec![Slot::with_kek(SlotKind::Passphrase(params), &id, &kek, &keys).unwrap()]);
        let mut p = Scripted { input: b"correct horse\n" as &[u8], output: Vec::new(), interactive: true };
        assert_eq!(unlock(Source::Passphrase, &h, "db", true, &mut p).unwrap().source, "passphrase");
        assert!(unlock(Source::File, &h, "db", true, &mut quiet()).is_err());
    }
}
