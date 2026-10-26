//! Encryption at rest: the sealed database container and the primitives it is
//! built from (XChaCha20-Poly1305, Argon2id, zeroized key material).
//!
//! fsqlite 0.4.7 ships a page cipher in `fsqlite-pager` but does not wire it
//! into `Connection` (`PRAGMA key` / `PRAGMA fsqlite.key` are silently
//! ignored; see docs/security.md and `tests/security.rs`). The database is
//! therefore kept as a *sealed container*: the whole SQLite image is decrypted
//! into an in-memory fsqlite connection on open and re-encrypted into a fresh
//! file (temp file, fsync, rename) after every committing statement. The
//! plaintext image never touches the filesystem.
//!
//! The container format (header with key slots + sealed image) is in
//! [`crate::container`]; its API is re-exported here.

use std::io::Write;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{KeyInit, Tag, XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{AppError, Result};

pub use crate::container::{is_sealed, open_image, read_header, replace_header, seal_image, Header};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
pub const ID_LEN: usize = 16;

/// Argon2id cost used for passphrase-derived KEKs (64 MiB, 3 passes).
pub const ARGON_M_COST: u32 = 64 * 1024;
pub const ARGON_T_COST: u32 = 3;
pub const ARGON_P_COST: u32 = 1;

/// A 256-bit secret key, wiped from memory on drop.
#[derive(Clone, Zeroize, zeroize::ZeroizeOnDrop)]
pub struct Key([u8; KEY_LEN]);

impl Key {
    pub fn random() -> Result<Self> {
        let mut k = [0u8; KEY_LEN];
        fill_random(&mut k)?;
        Ok(Self(k))
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        <[u8; KEY_LEN]>::try_from(b).ok().map(Self)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.0.into())
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

pub fn fill_random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|e| AppError::new(crate::error::ErrorKind::General, format!("OS RNG: {e}")))
}

pub fn random_array<const N: usize>() -> Result<[u8; N]> {
    let mut a = [0u8; N];
    fill_random(&mut a)?;
    Ok(a)
}

/// Encrypt `buf` in place and return the detached tag.
pub fn seal_in_place(key: &Key, nonce: &[u8; NONCE_LEN], aad: &[u8], buf: &mut [u8]) -> Result<[u8; TAG_LEN]> {
    let tag = key
        .cipher()
        .encrypt_inout_detached(&XNonce::from(*nonce), aad, buf.into())
        .map_err(|_| AppError::invalid("encryption failed (message too long)"))?;
    Ok(tag.into())
}

/// Decrypt `buf` in place, verifying `tag`. Returns false on authentication failure.
pub fn open_in_place(key: &Key, nonce: &[u8; NONCE_LEN], aad: &[u8], buf: &mut [u8], tag: &[u8; TAG_LEN]) -> bool {
    key.cipher().decrypt_inout_detached(&XNonce::from(*nonce), aad, buf.into(), &Tag::from(*tag)).is_ok()
}

/// Seal a small message as `nonce || ciphertext || tag`.
pub fn seal_message(key: &Key, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let nonce = random_array::<NONCE_LEN>()?;
    let mut out = Vec::with_capacity(NONCE_LEN + plaintext.len() + TAG_LEN);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(plaintext);
    let tag = seal_in_place(key, &nonce, aad, &mut out[NONCE_LEN..])?;
    out.extend_from_slice(&tag);
    Ok(out)
}

/// Open a message produced by [`seal_message`]; `None` if it fails to authenticate.
pub fn open_message(key: &Key, aad: &[u8], msg: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if msg.len() < NONCE_LEN + TAG_LEN {
        return None;
    }
    let nonce: [u8; NONCE_LEN] = msg[..NONCE_LEN].try_into().ok()?;
    let tag: [u8; TAG_LEN] = msg[msg.len() - TAG_LEN..].try_into().ok()?;
    let mut body = Zeroizing::new(msg[NONCE_LEN..msg.len() - TAG_LEN].to_vec());
    open_in_place(key, &nonce, aad, &mut body, &tag).then_some(body)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: [u8; 16],
}

impl KdfParams {
    pub fn fresh() -> Result<Self> {
        Ok(Self { m_cost: ARGON_M_COST, t_cost: ARGON_T_COST, p_cost: ARGON_P_COST, salt: random_array()? })
    }
}

/// Derive a KEK from a passphrase with Argon2id.
pub fn derive_kek(passphrase: &[u8], p: &KdfParams) -> Result<Key> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(p.m_cost, p.t_cost, p.p_cost, Some(KEY_LEN))
        .map_err(|e| AppError::invalid(format!("bad Argon2id parameters in header: {e}")))?;
    let mut out = [0u8; KEY_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase, &p.salt, &mut out)
        .map_err(|e| AppError::invalid(format!("Argon2id: {e}")))?;
    let k = Key(out);
    out.zeroize();
    Ok(k)
}

/// The data keys protected by the KEK.
#[derive(Clone)]
pub struct DataKeys {
    /// Encrypts the database image.
    pub dek: Key,
    /// Encrypts audit-log records (kept across DEK rotation so old records stay readable).
    pub audit: Key,
}

impl DataKeys {
    pub fn random() -> Result<Self> {
        Ok(Self { dek: Key::random()?, audit: Key::random()? })
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `<path><suffix>`, e.g. `wherebox.db.audit`.
pub fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    sibling(path, suffix)
}

/// Open `path` with `o`, creating it readable and writable only by the owner
/// (0600 on Unix, an owner-only ACL on Windows; see [`crate::perms`]).
fn open_private(path: &Path, o: &mut std::fs::OpenOptions) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let existed = path.exists();
    let f = o.open(path)?;
    if !existed {
        crate::perms::restrict_file(path)?;
    }
    Ok(f)
}

/// Create (or truncate) a file readable and writable only by the owner.
pub fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    open_private(path, std::fs::OpenOptions::new().write(true).create(true).truncate(true))
}

/// Open (creating it owner-only if needed) a file for appending.
pub fn open_private_append(path: &Path) -> std::io::Result<std::fs::File> {
    open_private(path, std::fs::OpenOptions::new().append(true).create(true).read(true))
}

/// Atomically replace `path` with `bytes`: write a private temp file in the
/// same directory, fsync it, rename it over `path`, fsync the directory.
/// `std::fs::rename` replaces an existing target on Windows too
/// (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`); directories cannot be
/// opened for fsync there, so that last step is skipped.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = sibling(path, &format!(".tmp-{}", std::process::id()));
    let res = (|| {
        let mut f = create_private(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            // Persist the rename; not supported on every platform, so best effort.
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok::<_, std::io::Error>(())
    })();
    res.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AppError::io(format!("writing {}: {e}", path.display()))
    })
}

/// Best-effort secure delete: overwrite the file with random bytes, fsync,
/// then unlink. Not a guarantee on copy-on-write or wear-levelled storage
/// (APFS, btrfs, SSDs); see docs/security.md.
pub fn wipe_file(path: &Path) -> Result<()> {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::io(format!("{}: {e}", path.display()))),
    };
    let res = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
        let mut chunk = vec![0u8; 64 * 1024];
        let mut left = len;
        while left > 0 {
            let n = usize::try_from(left.min(chunk.len() as u64)).unwrap_or(chunk.len());
            fill_random(&mut chunk[..n]).map_err(|e| std::io::Error::other(e.message))?;
            f.write_all(&chunk[..n])?;
            left -= n as u64;
        }
        f.sync_all()?;
        drop(f);
        std::fs::remove_file(path)
    })();
    res.map_err(|e| AppError::io(format!("wiping {}: {e}", path.display())))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_round_trip_and_tamper() {
        use crate::container::{Holder, Slot, SlotKind};
        let kek = Key::random().unwrap();
        let keys = DataKeys::random().unwrap();
        let id = random_array().unwrap();
        let h = Header::new(id, vec![Slot::with_kek(SlotKind::Raw(Holder::File), &id, &kek, &keys).unwrap()]);
        let sealed = seal_image(&h, &keys.dek, b"SQLite format 3\0 secret".to_vec()).unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        let (h2, _) = Header::decode(&sealed).unwrap();
        let k2 = h2.slots[0].open_kek(&h2.db_id, &kek).unwrap();
        assert_eq!(&open_image(&h2, &k2.dek, sealed.clone()).unwrap()[..], b"SQLite format 3\0 secret");
        assert!(h2.slots[0].open_kek(&h2.db_id, &Key::random().unwrap()).is_none());
        let mut bad = sealed.clone();
        let n = bad.len();
        bad[n - 20] ^= 1;
        assert!(open_image(&h2, &k2.dek, bad).is_err());
        let mut bad_hdr = sealed;
        bad_hdr[10] ^= 1; // db id is authenticated by the key wrap
        let (h3, _) = Header::decode(&bad_hdr).unwrap();
        assert!(h3.slots[0].open_kek(&h3.db_id, &kek).is_none());
    }

    #[test]
    fn passphrase_header_round_trip() {
        use crate::container::{Slot, SlotKind};
        let p = KdfParams { m_cost: 64, t_cost: 1, p_cost: 1, salt: [7; 16] };
        let kek = derive_kek(b"pw", &p).unwrap();
        let keys = DataKeys::random().unwrap();
        let h = Header::new([1; 16], vec![Slot::with_kek(SlotKind::Passphrase(p), &[1; 16], &kek, &keys).unwrap()]);
        let (d, _) = Header::decode(&[&h.encode()[..], &[0u8; 24]].concat()).unwrap();
        assert_eq!(d.slots[0].kind, SlotKind::Passphrase(p));
        assert!(d.slots[0].open_kek(&d.db_id, &derive_kek(b"pw", &p).unwrap()).is_some());
        assert!(d.slots[0].open_kek(&d.db_id, &derive_kek(b"px", &p).unwrap()).is_none());
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(unhex(&hex(&[0, 255, 16])).unwrap(), vec![0, 255, 16]);
        assert!(unhex("zz").is_none());
    }
}
