//! Atomic replace, fsync and advisory locking behave the same on Unix and
//! Windows (where rename-over-existing and file locks differ the most).

use std::fs::TryLockError;
use std::io::Read;

use tempfile::TempDir;
use wherebox_cli::{crypto, db};

#[test]
fn atomic_write_replaces_an_existing_file_that_is_open() {
    let t = TempDir::new().unwrap();
    let p = t.path().join("wherebox.db");
    crypto::atomic_write(&p, b"first").unwrap();
    // A reader holding the old file open must not block the replace
    // (std opens with FILE_SHARE_DELETE on Windows).
    let mut reader = std::fs::File::open(&p).unwrap();
    crypto::atomic_write(&p, b"second, longer").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"second, longer");
    let mut old = Vec::new();
    reader.read_to_end(&mut old).unwrap();
    assert_eq!(old, b"first", "the open handle keeps seeing the old contents");
    let names: Vec<String> =
        std::fs::read_dir(t.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["wherebox.db"], "no temp files left behind");
}

#[test]
fn database_lock_is_exclusive() {
    let t = TempDir::new().unwrap();
    let p = t.path().join("sub").join("wherebox.db");
    let held = db::lock(&p).unwrap();
    let other = std::fs::OpenOptions::new().read(true).open(crypto::sidecar(&p, ".lock")).unwrap();
    assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)), "second lock must fail while held");
    drop(held);
    other.try_lock().expect("lock is free once the holder is gone");
}
