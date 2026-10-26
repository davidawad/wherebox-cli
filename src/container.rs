//! The sealed database container: a header holding one or more *key slots*,
//! each wrapping the same data keys, followed by the encrypted SQLite image.
//!
//! Version 2 (`BMSEAL02`, written since 0.4; all integers little-endian):
//!
//! ```text
//! off  len  field
//!   0    8  magic "BMSEAL02"
//!   8   16  database id (random, stable across rekey)
//!  24    4  slot table length L
//!  28    L  slot table (JSON array, see [`Slot`])
//! 28+L  24  body nonce
//!        …  body: SQLite image encrypted with the DEK + tag; AAD = "BMSEAL01" || db id
//! ```
//!
//! A slot wraps `DEK || audit key` (64 bytes): `ssh` slots as an age message
//! to an SSH public key; `file` and `env` slots under a raw
//! 256-bit KEK; `passphrase` slots under an Argon2id-derived KEK. Raw and
//! passphrase wraps are XChaCha20-Poly1305 with AAD = magic || db id || slot
//! kind (|| Argon2id parameters), so a slot cannot be relabelled.
//!
//! Version 1 (`BMSEAL01`, 0.2 and 0.3) has exactly one slot in a fixed
//! 160-byte header; it stays readable (as a `legacy` or `passphrase` slot)
//! and is rewritten as version 2 the first time its slots change. The body
//! AAD is the same in both versions, so changing slots never re-encrypts the
//! body.

use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::crypto::{
    hex, open_in_place, open_message, random_array, seal_in_place, seal_message, unhex, DataKeys, KdfParams, Key,
    ID_LEN, KEY_LEN, NONCE_LEN, TAG_LEN,
};
use crate::error::{AppError, Result};

pub const MAGIC_V1: &[u8; 8] = b"BMSEAL01";
pub const MAGIC_V2: &[u8; 8] = b"BMSEAL02";
const V1_LEN: usize = 160;
const V1_AAD_END: usize = 56;
const V2_FIXED: usize = 28;
const KEYS_LEN: usize = 2 * KEY_LEN;

/// Where a raw 256-bit KEK is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    /// An owner-only key file ([`crate::keyfile`]).
    File,
    /// `WHEREBOX_KEY=raw:<hex>`.
    Env,
    /// A raw key from wherebox 0.3 or earlier, kept in the OS keychain (no
    /// longer used) or in `WHEREBOX_KEY=raw:<hex>`; only the latter opens it.
    Legacy,
}

impl Holder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Env => "env",
            Self::Legacy => "legacy",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            // pre-release keychain slots read as legacy: unopenable here, with a clear error
            "keychain" => Some(Self::Legacy),
            s => [Self::File, Self::Env, Self::Legacy].into_iter().find(|h| h.as_str() == s),
        }
    }
}

/// What can open a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotKind {
    Raw(Holder),
    Passphrase(KdfParams),
    /// `recipient` is the OpenSSH public key line, `identity` the private key
    /// path it was created from (a hint; `ssh_key` / `WHEREBOX_SSH_KEY` win).
    Ssh {
        recipient: String,
        fingerprint: String,
        identity: String,
    },
}

impl SlotKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Raw(h) => h.as_str(),
            Self::Passphrase(_) => "passphrase",
            Self::Ssh { .. } => "ssh",
        }
    }
}

/// One key slot.
#[derive(Debug, Clone)]
pub struct Slot {
    pub kind: SlotKind,
    sealed: Vec<u8>,
    /// A version-1 wrap (fixed nonce/ciphertext layout and AAD).
    v1: bool,
}

fn keys_bytes(keys: &DataKeys) -> Zeroizing<[u8; KEYS_LEN]> {
    let mut b = Zeroizing::new([0u8; KEYS_LEN]);
    b[..KEY_LEN].copy_from_slice(keys.dek.as_bytes());
    b[KEY_LEN..].copy_from_slice(keys.audit.as_bytes());
    b
}

fn keys_from(b: &[u8]) -> Option<DataKeys> {
    (b.len() == KEYS_LEN)
        .then(|| Some(DataKeys { dek: Key::from_bytes(&b[..KEY_LEN])?, audit: Key::from_bytes(&b[KEY_LEN..])? }))?
}

fn v1_aad(kind: &SlotKind, db_id: &[u8; ID_LEN]) -> [u8; V1_AAD_END] {
    let mut b = [0u8; V1_AAD_END];
    b[..8].copy_from_slice(MAGIC_V1);
    match kind {
        SlotKind::Passphrase(p) => {
            b[8] = 2;
            b[12..16].copy_from_slice(&p.m_cost.to_le_bytes());
            b[16..20].copy_from_slice(&p.t_cost.to_le_bytes());
            b[20..24].copy_from_slice(&p.p_cost.to_le_bytes());
            b[24..40].copy_from_slice(&p.salt);
        }
        _ => b[8] = 1,
    }
    b[40..56].copy_from_slice(db_id);
    b
}

fn v2_aad(kind: &SlotKind, db_id: &[u8; ID_LEN]) -> Vec<u8> {
    let mut a = [MAGIC_V2.as_slice(), db_id, kind.name().as_bytes()].concat();
    if let SlotKind::Passphrase(p) = kind {
        for n in [p.m_cost, p.t_cost, p.p_cost] {
            a.extend_from_slice(&n.to_le_bytes());
        }
        a.extend_from_slice(&p.salt);
    }
    a
}

impl Slot {
    /// Wrap `keys` under a raw or passphrase KEK.
    pub fn with_kek(kind: SlotKind, db_id: &[u8; ID_LEN], kek: &Key, keys: &DataKeys) -> Result<Self> {
        let sealed = seal_message(kek, &v2_aad(&kind, db_id), &keys_bytes(keys)[..])?;
        Ok(Self { kind, sealed, v1: false })
    }

    /// Wrap `keys` to an SSH public key with age.
    pub fn ssh(
        recipient: &age::ssh::Recipient,
        fingerprint: String,
        identity: String,
        keys: &DataKeys,
    ) -> Result<Self> {
        let err = |e: &dyn std::fmt::Display| AppError::invalid(format!("age: {e}"));
        let enc =
            age::Encryptor::with_recipients(std::iter::once(recipient as &dyn age::Recipient)).map_err(|e| err(&e))?;
        let mut sealed = Vec::new();
        let mut w = enc.wrap_output(&mut sealed).map_err(|e| err(&e))?;
        std::io::Write::write_all(&mut w, &keys_bytes(keys)[..]).map_err(|e| err(&e))?;
        w.finish().map_err(|e| err(&e))?;
        Ok(Self { kind: SlotKind::Ssh { recipient: recipient.to_string(), fingerprint, identity }, sealed, v1: false })
    }

    /// Open a raw or passphrase slot with `kek`.
    pub fn open_kek(&self, db_id: &[u8; ID_LEN], kek: &Key) -> Option<DataKeys> {
        if matches!(self.kind, SlotKind::Ssh { .. }) {
            return None;
        }
        if self.v1 {
            let mut buf = Zeroizing::new([0u8; KEYS_LEN]);
            let nonce: [u8; NONCE_LEN] = self.sealed.get(..NONCE_LEN)?.try_into().ok()?;
            buf.copy_from_slice(self.sealed.get(NONCE_LEN..NONCE_LEN + KEYS_LEN)?);
            let tag: [u8; TAG_LEN] = self.sealed.get(NONCE_LEN + KEYS_LEN..)?.try_into().ok()?;
            return open_in_place(kek, &nonce, &v1_aad(&self.kind, db_id), &mut buf[..], &tag)
                .then(|| keys_from(&buf[..]))?;
        }
        keys_from(&open_message(kek, &v2_aad(&self.kind, db_id), &self.sealed)?)
    }

    /// Open an `ssh` slot with an (already decrypted) SSH identity.
    pub fn open_identity(&self, identity: &dyn age::Identity) -> Option<DataKeys> {
        let dec = age::Decryptor::new(&self.sealed[..]).ok()?;
        let mut r = dec.decrypt(std::iter::once(identity)).ok()?;
        let mut out = Zeroizing::new(Vec::new());
        r.read_to_end(&mut out).ok()?;
        keys_from(&out)
    }

    fn to_json(&self) -> Value {
        let mut v = match &self.kind {
            SlotKind::Raw(h) => json!({"kind": h.as_str()}),
            SlotKind::Passphrase(p) => json!({"kind": "passphrase", "m_cost": p.m_cost, "t_cost": p.t_cost,
                "p_cost": p.p_cost, "salt": hex(&p.salt)}),
            SlotKind::Ssh { recipient, fingerprint, identity } => json!({"kind": "ssh", "recipient": recipient,
                "fingerprint": fingerprint, "identity": identity}),
        };
        v["sealed"] = json!(hex(&self.sealed));
        if self.v1 {
            v["v1"] = json!(true);
        }
        v
    }

    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        let n = |k: &str| v.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
        let kind = match s("kind")? {
            "passphrase" => SlotKind::Passphrase(KdfParams {
                m_cost: n("m_cost")?,
                t_cost: n("t_cost")?,
                p_cost: n("p_cost")?,
                salt: unhex(s("salt")?)?.try_into().ok()?,
            }),
            "ssh" => SlotKind::Ssh {
                recipient: s("recipient")?.into(),
                fingerprint: s("fingerprint")?.into(),
                identity: s("identity").unwrap_or("").into(),
            },
            other => SlotKind::Raw(Holder::parse(other)?),
        };
        Some(Self { kind, sealed: unhex(s("sealed")?)?, v1: v.get("v1").and_then(Value::as_bool).unwrap_or(false) })
    }
}

/// Parsed container header.
#[derive(Debug, Clone)]
pub struct Header {
    pub db_id: [u8; ID_LEN],
    pub slots: Vec<Slot>,
    /// Encode as a version-1 header (an untouched 0.2/0.3 database).
    v1: bool,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

fn bad(m: &str) -> AppError {
    AppError::db(format!("not a valid wherebox encrypted database: {m}"))
}

impl Header {
    /// A version-2 header with `slots`.
    pub fn new(db_id: [u8; ID_LEN], slots: Vec<Slot>) -> Self {
        Self { db_id, slots, v1: false }
    }

    /// A version-1 header as 0.2/0.3 wrote it (`passphrase` is the Argon2id
    /// parameters, `None` for a raw key). Kept for compatibility tests.
    pub fn legacy_v1(db_id: [u8; ID_LEN], passphrase: Option<KdfParams>, kek: &Key, keys: &DataKeys) -> Result<Self> {
        let kind = passphrase.map_or(SlotKind::Raw(Holder::Legacy), SlotKind::Passphrase);
        let nonce = random_array::<NONCE_LEN>()?;
        let mut buf = keys_bytes(keys);
        let tag = seal_in_place(kek, &nonce, &v1_aad(&kind, &db_id), &mut buf[..])?;
        let sealed = [&nonce[..], &buf[..], &tag[..]].concat();
        Ok(Self { db_id, slots: vec![Slot { kind, sealed, v1: true }], v1: true })
    }

    /// Format version this header is written as.
    pub fn version(&self) -> u8 {
        if self.v1 {
            1
        } else {
            2
        }
    }

    /// Replace the slots (switches to version 2).
    pub fn with_slots(&self, slots: Vec<Slot>) -> Self {
        Self { db_id: self.db_id, slots, v1: false }
    }

    pub fn encode(&self) -> Vec<u8> {
        if self.v1 {
            let slot = &self.slots[0];
            let mut b = v1_aad(&slot.kind, &self.db_id).to_vec();
            b.extend_from_slice(&slot.sealed);
            return b;
        }
        let table = serde_json::to_vec(&Value::Array(self.slots.iter().map(Slot::to_json).collect()))
            .expect("JSON of plain values");
        let len = u32::try_from(table.len()).expect("slot table under 4 GiB");
        [MAGIC_V2.as_slice(), &self.db_id, &len.to_le_bytes(), &table].concat()
    }

    /// Bytes `encode` takes for a header starting with `prefix` (at least 28 bytes).
    fn encoded_len(prefix: &[u8]) -> Result<usize> {
        match prefix.get(..8) {
            Some(m) if m == MAGIC_V1 => Ok(V1_LEN),
            Some(m) if m == MAGIC_V2 && prefix.len() >= V2_FIXED => Ok(V2_FIXED + u32_at(prefix, 24) as usize),
            _ => Err(bad("bad magic")),
        }
    }

    fn decode_v1(b: &[u8]) -> Result<Self> {
        let kind = match b[8] {
            1 => SlotKind::Raw(Holder::Legacy),
            2 => SlotKind::Passphrase(KdfParams {
                m_cost: u32_at(b, 12),
                t_cost: u32_at(b, 16),
                p_cost: u32_at(b, 20),
                salt: b[24..40].try_into().expect("16 bytes"),
            }),
            k => return Err(AppError::db(format!("unsupported key kind {k} in encrypted database header"))),
        };
        let slot = Slot { kind, sealed: b[V1_AAD_END..V1_LEN].to_vec(), v1: true };
        Ok(Self { db_id: b[40..56].try_into().expect("16 bytes"), slots: vec![slot], v1: true })
    }

    /// Parse a header from the start of `b`; returns it and its length.
    pub fn decode(b: &[u8]) -> Result<(Self, usize)> {
        let len = Self::encoded_len(b)?;
        if b.len() < len + NONCE_LEN {
            return Err(bad("truncated header"));
        }
        if &b[..8] == MAGIC_V1 {
            return Ok((Self::decode_v1(b)?, len));
        }
        let table: Value = serde_json::from_slice(&b[V2_FIXED..len]).map_err(|e| bad(&format!("slot table: {e}")))?;
        let slots =
            table.as_array().ok_or_else(|| bad("slot table"))?.iter().map(Slot::from_json).collect::<Option<Vec<_>>>();
        let slots = slots.filter(|s| !s.is_empty()).ok_or_else(|| bad("unreadable key slot"))?;
        Ok((Self { db_id: b[8..24].try_into().expect("16 bytes"), slots, v1: false }, len))
    }

    fn body_aad(&self) -> [u8; 8 + ID_LEN] {
        let mut a = [0u8; 8 + ID_LEN];
        a[..8].copy_from_slice(MAGIC_V1);
        a[8..].copy_from_slice(&self.db_id);
        a
    }
}

/// True when the file at `path` starts with a container magic.
pub fn is_sealed(path: &Path) -> bool {
    let mut m = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut m)).is_ok() && (&m == MAGIC_V1 || &m == MAGIC_V2)
}

/// Read only the header of a sealed file.
pub fn read_header(path: &Path) -> Result<Header> {
    let io = |e: std::io::Error| AppError::io(format!("reading {}: {e}", path.display()));
    let mut f = std::fs::File::open(path).map_err(io)?;
    let mut b = vec![0u8; V2_FIXED];
    f.read_exact(&mut b).map_err(io)?;
    let len = Header::encoded_len(&b)?;
    b.resize(len + NONCE_LEN, 0);
    f.read_exact(&mut b[V2_FIXED..]).map_err(io)?;
    Header::decode(&b).map(|(h, _)| h)
}

/// Encrypt `image` (consumed; encrypted in place to avoid a plaintext copy)
/// into a complete container byte vector.
pub fn seal_image(header: &Header, dek: &Key, image: Vec<u8>) -> Result<Vec<u8>> {
    let mut image = Zeroizing::new(image);
    let nonce = random_array::<NONCE_LEN>()?;
    let tag = seal_in_place(dek, &nonce, &header.body_aad(), &mut image)?;
    let head = header.encode();
    let mut out = Vec::with_capacity(head.len() + NONCE_LEN + image.len() + TAG_LEN);
    out.extend_from_slice(&head);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&image);
    out.extend_from_slice(&tag);
    Ok(out)
}

/// Decrypt a container's body (the header must already be parsed and the keys unwrapped).
pub fn open_image(header: &Header, dek: &Key, mut sealed: Vec<u8>) -> Result<Zeroizing<Vec<u8>>> {
    let start = Header::encoded_len(&sealed)?;
    if sealed.len() < start + NONCE_LEN + TAG_LEN {
        return Err(AppError::db("encrypted database is truncated"));
    }
    let nonce: [u8; NONCE_LEN] = sealed[start..start + NONCE_LEN].try_into().expect("24 bytes");
    let tag: [u8; TAG_LEN] = sealed[sealed.len() - TAG_LEN..].try_into().expect("16 bytes");
    sealed.truncate(sealed.len() - TAG_LEN);
    sealed.drain(..start + NONCE_LEN);
    let mut body = Zeroizing::new(sealed);
    if !open_in_place(dek, &nonce, &header.body_aad(), &mut body, &tag) {
        return Err(AppError::db(
            "encrypted database failed authentication: the file is corrupted or has been tampered with",
        ));
    }
    Ok(body)
}

/// A container with its header replaced and the body kept as is (slot
/// changes and rekeys that keep the DEK).
pub fn replace_header(sealed: &[u8], header: &Header) -> Result<Vec<u8>> {
    let start = Header::encoded_len(sealed)?;
    Ok([header.encode().as_slice(), &sealed[start..]].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> DataKeys {
        DataKeys::random().unwrap()
    }

    #[test]
    fn v1_header_round_trips_and_opens() {
        let (k, kek, id) = (keys(), Key::random().unwrap(), [3u8; ID_LEN]);
        let h = Header::legacy_v1(id, None, &kek, &k).unwrap();
        let sealed = seal_image(&h, &k.dek, b"image".to_vec()).unwrap();
        assert_eq!(&sealed[..8], MAGIC_V1);
        let (back, len) = Header::decode(&sealed).unwrap();
        assert_eq!((len, back.version(), back.slots[0].kind.name()), (V1_LEN, 1, "legacy"));
        let got = back.slots[0].open_kek(&id, &kek).unwrap();
        assert_eq!(&open_image(&back, &got.dek, sealed).unwrap()[..], b"image");
    }

    #[test]
    fn v2_header_holds_several_slots_and_swaps_without_reencrypting() {
        let (k, id) = (keys(), [5u8; ID_LEN]);
        let (a, b) = (Key::random().unwrap(), Key::random().unwrap());
        let params = KdfParams::fresh().unwrap();
        let v1 = Header::legacy_v1(id, None, &a, &k).unwrap();
        let sealed = seal_image(&v1, &k.dek, b"rows".to_vec()).unwrap();
        let mut slots = v1.slots.clone();
        slots.push(Slot::with_kek(SlotKind::Passphrase(params), &id, &b, &k).unwrap());
        let v2 = v1.with_slots(slots);
        let swapped = replace_header(&sealed, &v2).unwrap();
        let (back, _) = Header::decode(&swapped).unwrap();
        assert_eq!(back.version(), 2);
        assert!(back.slots[0].open_kek(&id, &a).is_some(), "legacy slot survives the move to v2");
        assert!(back.slots[1].open_kek(&id, &b).is_some());
        assert!(back.slots[1].open_kek(&id, &a).is_none());
        assert_eq!(&open_image(&back, &k.dek, swapped).unwrap()[..], b"rows");
    }

    #[test]
    fn slot_kind_is_bound_into_the_wrap() {
        let (k, id, kek) = (keys(), [9u8; ID_LEN], Key::random().unwrap());
        let mut s = Slot::with_kek(SlotKind::Raw(Holder::File), &id, &kek, &k).unwrap();
        s.kind = SlotKind::Raw(Holder::Env);
        assert!(s.open_kek(&id, &kek).is_none());
        assert!(s.open_kek(&[0u8; ID_LEN], &kek).is_none());
    }

    #[test]
    fn bad_headers_are_rejected() {
        assert!(Header::decode(b"NOTSEALED").is_err());
        let mut b = [MAGIC_V2.as_slice(), &[0u8; ID_LEN], &4u32.to_le_bytes(), b"[]  "].concat();
        b.extend_from_slice(&[0u8; NONCE_LEN]);
        assert!(Header::decode(&b).is_err(), "empty slot table");
    }
}
