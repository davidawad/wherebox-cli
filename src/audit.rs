//! Append-only encrypted audit trail of commands that touch the database.
//!
//! `<db>.audit` holds a 24-byte header (`BMAUDIT1` || database id) followed by
//! records `u32 length || nonce || ciphertext || tag`. Each record is sealed
//! with the database's audit key (wrapped next to the DEK, so it survives
//! rekeys) and its AAD is `database id || previous record's tag`. Records
//! therefore form a MAC chain: editing, reordering or deleting a record in the
//! middle breaks verification. Truncating the newest records is not
//! detectable from the file alone (see docs/security.md).
//!
//! Records carry no inventory data: timestamp, OS user, command path, outcome
//! and counts only.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::crypto::{self, Key, ID_LEN, TAG_LEN};
use crate::error::{AppError, Result};

const MAGIC: &[u8; 8] = b"BMAUDIT1";
const HEADER_LEN: u64 = 8 + ID_LEN as u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub ts: String,
    pub user: String,
    pub command: String,
    pub ok: bool,
    pub exit_code: i32,
    /// Rows inserted/updated/deleted.
    pub rows_changed: u64,
    /// Rows (or objects) emitted.
    pub rows_out: u64,
    /// Where output went: stdout, file, encrypted-file, encrypted-stdout.
    pub output: String,
}

/// Where and how to write audit records for one database.
#[derive(Clone)]
pub struct Sink {
    pub db_path: PathBuf,
    pub db_id: [u8; ID_LEN],
    pub key: Key,
}

pub fn path_for(db_path: &Path) -> PathBuf {
    crypto::sidecar(db_path, ".audit")
}

fn aad(db_id: &[u8], prev: &[u8; TAG_LEN]) -> Vec<u8> {
    [db_id, &prev[..]].concat()
}

fn io_err(p: &Path, e: std::io::Error) -> AppError {
    AppError::io(format!("audit log {}: {e}", p.display()))
}

impl Sink {
    pub fn path(&self) -> PathBuf {
        path_for(&self.db_path)
    }

    /// Append one record (takes the database lock to keep the chain linear).
    pub fn append(&self, rec: &Record) -> Result<()> {
        let _lock = crate::db::lock(&self.db_path)?;
        let p = self.path();
        let mut f = crypto::open_private_append(&p).map_err(|e| io_err(&p, e))?;
        let len = f.metadata().map_err(|e| io_err(&p, e))?.len();
        let prev = if len == 0 {
            f.write_all(&[&MAGIC[..], &self.db_id[..]].concat()).map_err(|e| io_err(&p, e))?;
            [0u8; TAG_LEN]
        } else {
            let mut hdr = [0u8; HEADER_LEN as usize];
            f.seek(SeekFrom::Start(0)).and_then(|_| f.read_exact(&mut hdr)).map_err(|e| io_err(&p, e))?;
            if &hdr[..8] != MAGIC || hdr[8..] != self.db_id {
                // The log belongs to another database (e.g. the file was replaced):
                // keep it aside and start a fresh chain.
                drop(f);
                let aside = crypto::sidecar(&p, &format!(".{}.orphaned", crypto::hex(&hdr[8..])));
                std::fs::rename(&p, &aside).map_err(|e| io_err(&p, e))?;
                eprintln!("wherebox: warning: audit log belonged to another database; moved to {}", aside.display());
                drop(_lock);
                return self.append(rec);
            }
            let mut tag = [0u8; TAG_LEN];
            if len > HEADER_LEN {
                f.seek(SeekFrom::End(-(TAG_LEN as i64)))
                    .and_then(|_| f.read_exact(&mut tag))
                    .map_err(|e| io_err(&p, e))?;
            }
            tag
        };
        let msg = crypto::seal_message(&self.key, &aad(&self.db_id, &prev), &serde_json::to_vec(rec)?)?;
        let n = u32::try_from(msg.len()).map_err(|_| AppError::invalid("audit record too large"))?;
        let mut buf = n.to_le_bytes().to_vec();
        buf.extend_from_slice(&msg);
        f.write_all(&buf).and_then(|()| f.sync_data()).map_err(|e| io_err(&p, e))
    }

    /// Read and verify every record. Fails on any authentication or chain error.
    pub fn read(&self) -> Result<Vec<Record>> {
        let p = self.path();
        let data = match std::fs::read(&p) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&p, e)),
        };
        let bad = |i: usize, why: &str| {
            AppError::db(format!(
                "audit log {} failed verification at record {}: {why} (tampered or corrupted)",
                p.display(),
                i + 1
            ))
        };
        if data.len() < HEADER_LEN as usize || &data[..8] != MAGIC {
            return Err(bad(0, "bad header"));
        }
        if data[8..HEADER_LEN as usize] != self.db_id {
            return Err(AppError::db(format!("audit log {} belongs to a different database", p.display())));
        }
        let mut pos = HEADER_LEN as usize;
        let mut prev = [0u8; TAG_LEN];
        let mut out = Vec::new();
        while pos < data.len() {
            let i = out.len();
            let n = data
                .get(pos..pos + 4)
                .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize)
                .ok_or_else(|| bad(i, "truncated length"))?;
            let msg = data.get(pos + 4..pos + 4 + n).ok_or_else(|| bad(i, "truncated record"))?;
            let plain = crypto::open_message(&self.key, &aad(&self.db_id, &prev), msg)
                .ok_or_else(|| bad(i, "authentication failed"))?;
            out.push(serde_json::from_slice(&plain).map_err(|_| bad(i, "malformed record"))?);
            prev.copy_from_slice(&msg[msg.len() - TAG_LEN..]);
            pos += 4 + n;
        }
        Ok(out)
    }
}

pub fn current_user() -> String {
    ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(cmd: &str) -> Record {
        Record {
            ts: "2026-01-01T00:00:00Z".into(),
            user: "u".into(),
            command: cmd.into(),
            ok: true,
            exit_code: 0,
            rows_changed: 1,
            rows_out: 2,
            output: "stdout".into(),
        }
    }

    #[test]
    fn chain_verifies_and_detects_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Sink { db_path: dir.path().join("x.db"), db_id: [3; ID_LEN], key: Key::random().unwrap() };
        for c in ["add", "query", "export"] {
            sink.append(&rec(c)).unwrap();
        }
        let got: Vec<String> = sink.read().unwrap().into_iter().map(|r| r.command).collect();
        assert_eq!(got, ["add", "query", "export"]);
        let raw = std::fs::read(sink.path()).unwrap();
        assert!(!raw.windows(5).any(|w| w == b"query"));
        let mut t = raw.clone();
        let n = t.len();
        t[n / 2] ^= 0x40;
        std::fs::write(sink.path(), &t).unwrap();
        assert!(sink.read().is_err());
        // Dropping a middle record breaks the chain.
        let first_len = 4 + u32::from_le_bytes(raw[24..28].try_into().unwrap()) as usize;
        let mut cut = raw[..24].to_vec();
        cut.extend_from_slice(&raw[24 + first_len..]);
        std::fs::write(sink.path(), &cut).unwrap();
        assert!(sink.read().is_err());
    }
}
