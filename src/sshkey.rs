//! SSH keys as database keys: finding the user's key, reading OpenSSH
//! private keys (ed25519 and RSA, passphrase-protected or not) with age, and
//! SHA256 fingerprints the way `ssh-keygen -l` prints them.
//!
//! The key is the `ssh_key` setting / `WHEREBOX_SSH_KEY` when set, else
//! `~/.ssh/id_ed25519`, then `~/.ssh/id_rsa` (`%USERPROFILE%\.ssh` on
//! Windows, where OpenSSH keeps them too). A passphrase-protected key's
//! passphrase comes from `WHEREBOX_SSH_PASSPHRASE` or a prompt.

use std::cell::RefCell;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use age::ssh::{Identity, Recipient};
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::keys::key_error;
use crate::prompt::Prompter;

pub const ENV_PASSPHRASE: &str = "WHEREBOX_SSH_PASSPHRASE";

thread_local! {
    static OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Use `path` as the SSH private key (`ssh_key` setting); `None` restores the
/// default search. Per thread, so tests can run in parallel.
pub fn set_override(path: Option<PathBuf>) {
    OVERRIDE.with(|o| *o.borrow_mut() = path.filter(|p| !p.as_os_str().is_empty()));
}

pub fn override_path() -> Option<PathBuf> {
    OVERRIDE.with(|o| o.borrow().clone())
}

/// Private key files to try, in order.
pub fn candidates() -> Vec<PathBuf> {
    if let Some(p) = override_path() {
        return vec![p];
    }
    crate::paths::home_dir()
        .map(|h| h.join(".ssh"))
        .map(|d| vec![d.join("id_ed25519"), d.join("id_rsa")])
        .unwrap_or_default()
}

/// An SSH key usable as a database key.
pub struct SshKey {
    pub identity: PathBuf,
    pub recipient: Recipient,
    pub fingerprint: String,
    /// The private key is passphrase-protected.
    pub encrypted: bool,
}

/// `SHA256:<base64>` of an OpenSSH public key line (`type base64 [comment]`).
pub fn fingerprint(line: &str) -> Option<String> {
    let blob = base64::engine::general_purpose::STANDARD.decode(line.split_whitespace().nth(1)?).ok()?;
    Some(format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&blob))))
}

/// Parse an OpenSSH private key file; CRLF line endings (a key copied
/// through Windows tools or git) are accepted.
fn read_identity(path: &Path) -> Result<Identity> {
    let text =
        std::fs::read_to_string(path).map_err(|e| key_error(format!("reading SSH key {}: {e}", path.display())))?;
    let text = zeroize::Zeroizing::new(text.replace("\r\n", "\n"));
    Identity::from_buffer(BufReader::new(text.as_bytes()), Some(path.display().to_string()))
        .map_err(|e| key_error(format!("{} is not an OpenSSH private key: {e}", path.display())))
}

/// Load the SSH private key at `path` (its public half and fingerprint).
pub fn load(path: &Path) -> Result<SshKey> {
    let id = read_identity(path)?;
    let encrypted = matches!(id, Identity::Encrypted(_));
    if let Identity::Unsupported(k) = &id {
        return Err(key_error(format!("{}: unsupported SSH key ({k:?}); use ed25519 or RSA", path.display())));
    }
    let recipient = Recipient::try_from(id)
        .map_err(|e| key_error(format!("{}: unusable SSH key ({e:?}); use ed25519 or RSA", path.display())))?;
    let fingerprint = fingerprint(&recipient.to_string()).unwrap_or_default();
    Ok(SshKey { identity: path.to_path_buf(), recipient, fingerprint, encrypted })
}

/// The first usable SSH key among [`candidates`], and why others were skipped.
pub fn find() -> (Option<SshKey>, Vec<String>) {
    let mut skipped = Vec::new();
    for p in candidates().into_iter().filter(|p| p.exists()) {
        match load(&p) {
            Ok(k) => return (Some(k), skipped),
            Err(e) => skipped.push(e.message),
        }
    }
    (None, skipped)
}

/// A public key given as an OpenSSH line or a path to a `.pub` file, with
/// its fingerprint and the private key path next to it (if the input was a
/// `<key>.pub` path).
pub fn parse_public(arg: &str) -> Result<(Recipient, String, String)> {
    let path = Path::new(arg);
    let (line, identity) = if path.exists() {
        let text = std::fs::read_to_string(path).map_err(|e| key_error(format!("reading {arg}: {e}")))?;
        let identity = arg.strip_suffix(".pub").filter(|p| Path::new(p).exists()).unwrap_or("").to_string();
        (text.trim().to_string(), identity)
    } else {
        (arg.trim().to_string(), String::new())
    };
    let recipient: Recipient =
        line.parse().map_err(|e| key_error(format!("not an ssh-ed25519 or ssh-rsa public key ({e:?}): {arg}")))?;
    let fp = fingerprint(&recipient.to_string()).unwrap_or_default();
    Ok((recipient, fp, identity))
}

#[derive(Clone)]
struct Passphrase(SecretString);

impl age::Callbacks for Passphrase {
    fn display_message(&self, _: &str) {}
    fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
        None
    }
    fn request_public_string(&self, _: &str) -> Option<String> {
        None
    }
    fn request_passphrase(&self, _: &str) -> Option<SecretString> {
        Some(self.0.clone())
    }
}

/// The SSH private key at `path` ready to decrypt with: an unencrypted key
/// as is; a protected one with its passphrase from `WHEREBOX_SSH_PASSPHRASE`
/// or `p` (only when `allow_prompt`). `None` when the key is missing or its
/// passphrase cannot be asked for.
pub fn identity(path: &Path, p: &mut dyn Prompter, allow_prompt: bool) -> Result<Option<Box<dyn age::Identity>>> {
    if !path.exists() {
        return Ok(None);
    }
    match read_identity(path)? {
        Identity::Unencrypted(k) => Ok(Some(Box::new(Identity::from(k)))),
        id @ Identity::Encrypted(_) => {
            let pass = match std::env::var(ENV_PASSPHRASE).ok().filter(|v| !v.is_empty()) {
                Some(v) => v,
                None if allow_prompt && p.interactive() => {
                    p.secret(&format!("Passphrase for SSH key {}: ", path.display()))?.to_string()
                }
                None => return Ok(None),
            };
            Ok(Some(Box::new(id.with_callbacks(Passphrase(SecretString::from(pass))))))
        }
        Identity::Unsupported(_) => Ok(None),
    }
}

#[cfg(test)]
pub(crate) mod testkeys {
    //! Throwaway OpenSSH keys generated in-process (no ssh-keygen needed).
    use std::path::{Path, PathBuf};

    use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};

    /// Write an ed25519 key (protected by `pass` if given) to `dir/name` and
    /// `dir/name.pub`; returns the private key path and the fingerprint.
    pub fn ed25519(dir: &Path, name: &str, pass: Option<&str>) -> (PathBuf, String) {
        let mut rng = rand_core::OsRng;
        let key = PrivateKey::random(&mut rng, Algorithm::Ed25519).unwrap();
        let fp = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let written = match pass {
            Some(p) => key.encrypt(&mut rng, p).unwrap(),
            None => key.clone(),
        };
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, written.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
        let mut pubp = path.clone().into_os_string();
        pubp.push(".pub");
        std::fs::write(PathBuf::from(pubp), key.public_key().to_openssh().unwrap() + "\n").unwrap();
        (path, fp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Scripted;

    #[test]
    fn loads_keys_and_matches_ssh_keygen_fingerprints() {
        let t = tempfile::TempDir::new().unwrap();
        let (plain, fp) = testkeys::ed25519(t.path(), "id_ed25519", None);
        let k = load(&plain).unwrap();
        assert_eq!((k.fingerprint.as_str(), k.encrypted), (fp.as_str(), false));
        let (locked, fp2) = testkeys::ed25519(t.path(), "locked", Some("hunter22"));
        let k = load(&locked).unwrap();
        assert_eq!((k.fingerprint.as_str(), k.encrypted), (fp2.as_str(), true));
        let pubfile = t.path().join("locked.pub");
        let (_, fp3, identity) = parse_public(pubfile.to_str().unwrap()).unwrap();
        assert_eq!((fp3, identity), (fp2, locked.display().to_string()));
    }

    #[test]
    fn crlf_key_files_load() {
        let t = tempfile::TempDir::new().unwrap();
        let (p, fp) = testkeys::ed25519(t.path(), "id_ed25519", None);
        let crlf = std::fs::read_to_string(&p).unwrap().replace('\n', "\r\n");
        std::fs::write(&p, crlf).unwrap();
        assert_eq!(load(&p).unwrap().fingerprint, fp);
    }

    #[test]
    fn search_order_and_override() {
        let t = tempfile::TempDir::new().unwrap();
        let (p, _) = testkeys::ed25519(t.path(), "mine", None);
        set_override(Some(p.clone()));
        assert_eq!(candidates(), vec![p.clone()]);
        assert_eq!(find().0.unwrap().identity, p);
        set_override(Some(t.path().join("absent")));
        assert!(find().0.is_none());
        set_override(None);
    }

    #[test]
    fn protected_key_needs_a_passphrase() {
        let t = tempfile::TempDir::new().unwrap();
        let (locked, _) = testkeys::ed25519(t.path(), "locked", Some("hunter22"));
        let mut quiet = Scripted { input: "".as_bytes(), output: Vec::new(), interactive: false };
        assert!(identity(&locked, &mut quiet, true).unwrap().is_none(), "no prompt without a terminal");
        let mut asked = Scripted { input: "hunter22\n".as_bytes(), output: Vec::new(), interactive: true };
        assert!(identity(&locked, &mut asked, true).unwrap().is_some());
        assert!(String::from_utf8(asked.output).unwrap().contains("Passphrase for SSH key"));
    }
}
