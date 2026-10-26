//! Every command resolves the database through one resolver, and a database
//! at the 0.2-era location (`~/.local/share/wherebox-cli`, still the default
//! on Linux/macOS, legacy on Windows) is found and used, never shadowed by a
//! new empty one at the platform location. Runs on all platforms.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const RAW: &str = "raw:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// A clean environment with HOME = `home`, no XDG_DATA_HOME and no
/// WHEREBOX_DB, so the default data location is resolved.
fn cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("wherebox").unwrap();
    c.env_clear()
        .envs(std::env::var_os("SYSTEMROOT").map(|v| ("SYSTEMROOT", v)))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env("WHEREBOX_KEY", RAW)
        .env("NO_COLOR", "1");
    c
}

fn run(c: &mut Command) -> String {
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn legacy_db(home: &Path) -> PathBuf {
    home.join(".local").join("share").join("wherebox-cli").join("wherebox.db")
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flat_map(|e| {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p)
            } else {
                vec![p]
            }
        })
        .collect()
}

#[test]
fn a_legacy_database_is_used_not_shadowed() {
    let t = TempDir::new().unwrap();
    let home = t.path();
    let legacy = legacy_db(home);
    // An existing database at the 0.2 location.
    run(cmd(home).env("WHEREBOX_DB", &legacy).args(["add", "alex"]));
    assert!(legacy.exists());
    // Without WHEREBOX_DB every command resolves to it.
    let v: Value = serde_json::from_str(&run(cmd(home).args(["db", "path", "--format", "json"]))).unwrap();
    assert_eq!(Path::new(v["data"]["path"].as_str().unwrap()), legacy);
    assert!(run(cmd(home).args(["ls"])).contains("alex"));
    let before = std::fs::read(&legacy).unwrap();
    run(cmd(home).args(["key", "status"]));
    run(cmd(home).args(["doctor"]));
    assert_eq!(std::fs::read(&legacy).unwrap(), before, "read-only commands left it alone");
    // ... and no second database appeared anywhere under HOME.
    let dbs: Vec<PathBuf> = walk(home).into_iter().filter(|p| p.extension().is_some_and(|e| e == "db")).collect();
    assert_eq!(dbs, [legacy]);
}
