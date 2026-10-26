//! SSH keys and key files as database keys, on every platform: first-run
//! setup from ~/.ssh, a key file for passphrase-protected SSH keys, the
//! config file record, `key` slot management and moving a 0.3 database to
//! an SSH key. Keys are generated in-process; the real ~/.ssh and the OS
//! keychain are never touched (HOME is a temp dir; wherebox has no keychain code).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};
use tempfile::TempDir;
use wherebox_cli::perms::{self, Access};

const RAW: &str = "raw:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

struct Env {
    dir: TempDir,
}

/// Write an ed25519 key pair at `path` (+ `.pub`), protected by `pass` if
/// given; returns its SHA256 fingerprint.
fn ssh_keypair(path: &Path, pass: Option<&str>) -> String {
    let mut rng = rand_core::OsRng;
    let key = PrivateKey::random(&mut rng, Algorithm::Ed25519).unwrap();
    let written = pass.map_or_else(|| key.clone(), |p| key.encrypt(&mut rng, p).unwrap());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, written.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    std::fs::write(pub_path(path), key.public_key().to_openssh().unwrap() + "\n").unwrap();
    key.public_key().fingerprint(HashAlg::Sha256).to_string()
}

fn pub_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".pub");
    PathBuf::from(s)
}

impl Env {
    fn new() -> Self {
        Self { dir: TempDir::new().unwrap() }
    }
    fn home(&self) -> &Path {
        self.dir.path()
    }
    fn ssh(&self, name: &str) -> PathBuf {
        self.home().join(".ssh").join(name)
    }
    fn config(&self) -> PathBuf {
        self.home().join("config").join("wherebox-cli").join("config.toml")
    }
    fn keys_dir(&self) -> PathBuf {
        self.home().join("config").join("wherebox-cli").join("keys")
    }
    fn db(&self) -> PathBuf {
        self.home().join("data").join("wherebox-cli").join("wherebox.db")
    }
    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("wherebox").unwrap();
        c.env_clear()
            .envs(std::env::var_os("SYSTEMROOT").map(|v| ("SYSTEMROOT", v)))
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home().join("config"))
            .env("XDG_DATA_HOME", self.home().join("data"))
            .env("NO_COLOR", "1");
        c
    }
    fn ok(&self, c: &mut Command) -> (String, String) {
        let out = c.output().unwrap();
        let (o, e) =
            (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned());
        assert!(out.status.success(), "stdout: {o}\nstderr: {e}");
        (o, e)
    }
    fn run(&self, args: &[&str]) -> (String, String) {
        self.ok(self.cmd().args(args))
    }
    fn fail(&self, c: &mut Command, code: i32) -> String {
        let out = c.output().unwrap();
        assert_eq!(out.status.code(), Some(code), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
    fn slots(&self, c: &mut Command) -> Vec<Value> {
        let (o, _) = self.ok(c.args(["key", "status", "--format", "json"]));
        let v: Value = serde_json::from_str(&o).unwrap();
        v["data"].as_array().unwrap().iter().filter(|r| !r["slot"].is_null()).cloned().collect()
    }
}

#[test]
fn first_run_encrypts_with_the_ssh_key_and_records_how() {
    let e = Env::new();
    let fp = ssh_keypair(&e.ssh("id_ed25519"), None);
    let (_, notice) = e.run(&["add", "alex"]);
    assert!(notice.contains(&fp) && notice.contains("config.toml") && notice.contains("wherebox doctor"), "{notice}");
    // Daily use: silent, no key file, nothing new on stderr.
    let (out, err) = e.run(&["ls"]);
    assert!(out.contains("alex") && err.is_empty(), "{err}");
    assert!(!e.keys_dir().exists());
    let slots = e.slots(&mut e.cmd());
    assert_eq!(slots.len(), 1);
    assert_eq!((slots[0]["kind"].as_str(), slots[0]["status"].as_str()), (Some("ssh"), Some("ok")));
    // The config file says which key opens the database and how to recover it.
    let cfg = std::fs::read_to_string(e.config()).unwrap();
    assert!(
        cfg.contains("[encryption.") && cfg.contains(&fp) && cfg.contains("#   recover: on another machine"),
        "{cfg}"
    );
    // ... and stays a valid config file.
    e.run(&["config", "set", "format", "json"]);
    let cfg = std::fs::read_to_string(e.config()).unwrap();
    assert!(cfg.starts_with("format = \"json\"") && cfg.contains(&fp), "{cfg}");
    // Without the SSH key the database stays shut.
    std::fs::rename(e.ssh("id_ed25519"), e.ssh("moved")).unwrap();
    let err = e.fail(e.cmd().args(["ls"]), 8);
    assert!(err.contains("SSH key") && err.contains(&fp), "{err}");
    let mut c = e.cmd();
    c.env("WHEREBOX_SSH_KEY", e.ssh("moved"));
    e.ok(c.args(["ls"]));
}

#[test]
fn protected_ssh_key_gets_a_private_daily_key_file() {
    let e = Env::new();
    let fp = ssh_keypair(&e.ssh("id_ed25519"), Some("ssh key pass"));
    let (_, notice) = e.run(&["add", "alex"]);
    assert!(notice.contains(&fp) && notice.contains("recovery key"), "{notice}");
    let kinds: Vec<String> = e.slots(&mut e.cmd()).iter().map(|s| s["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["ssh", "file"]);
    let files: Vec<PathBuf> = std::fs::read_dir(e.keys_dir()).unwrap().map(|d| d.unwrap().path()).collect();
    assert_eq!(files.len(), 1);
    if let Some(a) = perms::inspect(&files[0]) {
        assert!(matches!(a, Access::Private(_)), "{a:?}");
    }
    if let Some(a) = perms::inspect(&e.keys_dir()) {
        assert!(matches!(a, Access::Private(_)), "{a:?}");
    }
    e.run(&["ls"]); // no passphrase needed day to day
                    // Key file gone: the SSH key is the way back in.
    std::fs::remove_file(&files[0]).unwrap();
    let err = e.fail(e.cmd().args(["ls"]), 8);
    assert!(err.contains("none of its keys"), "{err}");
    let mut c = e.cmd();
    c.env("WHEREBOX_SSH_PASSPHRASE", "wrong one");
    e.fail(c.args(["ls"]), 8);
    let mut c = e.cmd();
    c.env("WHEREBOX_SSH_PASSPHRASE", "ssh key pass");
    e.ok(c.args(["ls"]));
}

/// Give other local users read access to `p`.
fn share(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    #[cfg(windows)]
    {
        let st = std::process::Command::new("icacls").arg(p).args(["/grant", "*S-1-5-32-545:R"]).status().unwrap();
        assert!(st.success(), "icacls");
    }
}

#[test]
fn without_an_ssh_key_a_key_file_is_made_and_must_stay_private() {
    let e = Env::new();
    let (_, notice) = e.run(&["add", "alex"]);
    assert!(notice.contains("no SSH key found") && notice.contains("Back up"), "{notice}");
    let file = std::fs::read_dir(e.keys_dir()).unwrap().next().unwrap().unwrap().path();
    e.run(&["ls"]);
    share(&file);
    assert!(matches!(perms::inspect(&file), Some(Access::Shared(_)) | None));
    if matches!(perms::inspect(&file), Some(Access::Shared(_))) {
        let err = e.fail(e.cmd().args(["ls"]), 8);
        assert!(err.contains("accessible by other users"), "{err}");
    }
}

#[test]
fn key_slots_can_be_added_and_removed() {
    let e = Env::new();
    ssh_keypair(&e.ssh("id_ed25519"), None);
    let other_fp = ssh_keypair(&e.ssh("laptop"), None);
    e.run(&["add", "alex"]);
    let mut c = e.cmd();
    e.ok(c.args(["key", "add-ssh", pub_path(&e.ssh("laptop")).to_str().unwrap()]));
    let mut c = e.cmd();
    e.ok(c.env("WHEREBOX_NEW_KEY", "a long passphrase").args(["key", "add-passphrase"]));
    let kinds: Vec<String> = e.slots(&mut e.cmd()).iter().map(|s| s["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["ssh", "ssh", "passphrase"]);
    assert!(std::fs::read_to_string(e.config()).unwrap().contains(&other_fp));
    // Drop the first SSH key: the second one (and the passphrase) still open it.
    e.run(&["key", "remove", "1"]);
    std::fs::remove_file(e.ssh("id_ed25519")).unwrap();
    let mut c = e.cmd();
    e.ok(c.env("WHEREBOX_SSH_KEY", e.ssh("laptop")).args(["ls"]));
    let mut c = e.cmd();
    e.ok(c.env("WHEREBOX_KEY", "a long passphrase").args(["key", "remove", "1"]));
    let mut c = e.cmd();
    let err = e.fail(c.env("WHEREBOX_KEY", "a long passphrase").args(["key", "remove", "1"]), 4);
    assert!(err.contains("last key"), "{err}");
    let cfg = std::fs::read_to_string(e.config()).unwrap();
    assert!(!cfg.contains(&other_fp) && cfg.contains("a passphrase"), "{cfg}");
}

#[test]
fn a_0_3_database_moves_to_the_ssh_key() {
    use wherebox_cli::container::{replace_header, Header};
    use wherebox_cli::crypto::Key;
    let e = Env::new();
    let fp = ssh_keypair(&e.ssh("id_ed25519"), None);
    // Make a 0.3-style database: one raw key in a version-1 header.
    let mut c = e.cmd();
    e.ok(c.env("WHEREBOX_KEY", RAW).args(["add", "alex"]));
    let bytes = std::fs::read(e.db()).unwrap();
    let (h, _) = Header::decode(&bytes).unwrap();
    let kek = Key::from_bytes(&wherebox_cli::crypto::unhex(&RAW[4..]).unwrap()).unwrap();
    let keys = h.slots[0].open_kek(&h.db_id, &kek).unwrap();
    let v1 = Header::legacy_v1(h.db_id, None, &kek, &keys).unwrap();
    std::fs::write(e.db(), replace_header(&bytes, &v1).unwrap()).unwrap();
    assert_eq!(&std::fs::read(e.db()).unwrap()[..8], b"BMSEAL01");
    let mut c = e.cmd();
    let legacy = e.slots(c.env("WHEREBOX_KEY", RAW));
    assert_eq!(legacy[0]["kind"], "legacy");
    // Rekey to the SSH key; afterwards no WHEREBOX_KEY is needed.
    let mut c = e.cmd();
    e.ok(c.env("WHEREBOX_KEY", RAW).args(["db", "rekey", "--to", "ssh"]));
    assert_eq!(&std::fs::read(e.db()).unwrap()[..8], b"BMSEAL02");
    let (out, _) = e.run(&["ls"]);
    assert!(out.contains("alex"));
    let mut c = e.cmd();
    let err = e.fail(c.env("WHEREBOX_KEY", RAW).env("WHEREBOX_KEY_SOURCE", "env").args(["ls"]), 8);
    assert!(err.contains("wrong key"), "the old key no longer opens it: {err}");
    assert!(std::fs::read_to_string(e.config()).unwrap().contains(&fp));
}
