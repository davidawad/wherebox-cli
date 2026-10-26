//! Shared harness for the end-to-end tests: an isolated environment per
//! test (temp HOME, XDG dirs, database, raw test key) driving the binary.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// Raw 256-bit test KEK (never use a fixed key outside tests).
pub const TEST_KEY: &str = "raw:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
/// 2026-01-01T00:00:00Z: pins `generated_at`, `created_at` and "today".
pub const EPOCH_2026: &str = "1767225600";

pub struct Env {
    pub dir: TempDir,
    /// `SOURCE_DATE_EPOCH` for every command (None = real time).
    pub epoch: Option<String>,
}

impl Env {
    pub fn new() -> Self {
        Self { dir: TempDir::new().unwrap(), epoch: None }
    }

    /// Time pinned to 2026-01-01 (deterministic output).
    pub fn pinned() -> Self {
        Self { epoch: Some(EPOCH_2026.into()), ..Self::new() }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn db(&self) -> PathBuf {
        self.path("test.db")
    }

    /// A command isolated from the user's environment and config.
    pub fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("wherebox").unwrap();
        c.env_clear()
            // Windows system DLLs expect SYSTEMROOT even in a cleared environment.
            .envs(std::env::var_os("SYSTEMROOT").map(|v| ("SYSTEMROOT", v)))
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("APPDATA", self.path("appdata"))
            .env("WHEREBOX_DB", self.db())
            .env("WHEREBOX_TZ", "UTC")
            .env("WHEREBOX_KEY", TEST_KEY)
            .env("WHEREBOX_KEY_SOURCE", "env")
            .env("NO_COLOR", "1");
        if let Some(e) = &self.epoch {
            c.env("SOURCE_DATE_EPOCH", e);
        }
        c
    }

    pub fn out(&self, args: &[&str]) -> std::process::Output {
        self.cmd().args(args).output().unwrap()
    }

    pub fn run(&self, args: &[&str]) -> String {
        let out = self.out(args);
        assert!(
            out.status.success(),
            "wherebox {args:?} failed ({:?}):\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Run expecting exit `code`; returns stderr.
    pub fn fail(&self, args: &[&str], code: i32) -> String {
        let out = self.out(args);
        assert_eq!(out.status.code(), Some(code), "wherebox {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// Run with `--json` and parse the `wherebox/v1` envelope.
    pub fn json(&self, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.push("--json");
        let v: Value = serde_json::from_str(&self.run(&a)).unwrap();
        assert_eq!(v["schema"], "wherebox/v1");
        v
    }

    /// Import the Homebox fixture (fixed import refs, so fixed ids).
    pub fn seed(&self) {
        self.run(&["import", fixture("homebox-import.csv").to_str().unwrap()]);
    }
}

pub fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

pub fn fixture(name: &str) -> PathBuf {
    repo_file("tests/fixtures").join(name)
}

/// Compare against (or, with WHEREBOX_BLESS set, rewrite) `tests/golden/<rel>`.
pub fn check_golden(rel: &str, actual: &str) {
    let path = repo_file("tests/golden").join(rel);
    if std::env::var_os("WHEREBOX_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(actual == expected, "{rel} changed:\n--- expected\n{expected}\n--- actual\n{actual}");
}

/// The `data` array of an envelope.
pub fn rows(v: &Value) -> &Vec<Value> {
    v["data"].as_array().unwrap()
}

/// The row of `rows(v)` whose `key` equals `value`.
pub fn row<'a>(v: &'a Value, key: &str, value: &str) -> &'a Value {
    rows(v).iter().find(|r| r[key] == value).unwrap_or_else(|| panic!("no row with {key} = {value} in {v}"))
}
