//! Per-property profiles: one database per property, selected with
//! `--profile` / `WHEREBOX_PROFILE` / config `profile`.

mod support;

use serde_json::Value;
use support::{row, rows, Env};

/// Like `Env::cmd`, but without WHEREBOX_DB so profiles pick the database.
fn cmd(e: &Env) -> assert_cmd::Command {
    let mut c = e.cmd();
    c.env_remove("WHEREBOX_DB");
    c
}

fn run(e: &Env, args: &[&str]) -> Value {
    let out = cmd(e).args(args).arg("--json").output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn profile_db(e: &Env, name: &str) -> std::path::PathBuf {
    e.path("data").join("wherebox-cli").join("profiles").join(format!("{name}.db"))
}

#[test]
fn configured_properties_are_listed() {
    let e = Env::new();
    let dir = e.path("config").join("wherebox-cli");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[profiles.beach-house]\ntitle = \"Beach House\"\naddress = \"1 Shore Rd\"\n\
         [profiles.cabin]\ntitle = \"Cabin\"\n\
         [profiles.lake-house]\ntitle = \"Lake House\"\n\
         [profiles.rental-apt]\ntitle = \"Rental apartment\"\nrented = true\n",
    )
    .unwrap();
    let v = run(&e, &["profile", "ls"]);
    let names: Vec<&str> = rows(&v).iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["beach-house", "cabin", "lake-house", "rental-apt"]);
    assert_eq!(row(&v, "name", "lake-house")["title"], "Lake House");
    assert_eq!(row(&v, "name", "beach-house")["title"], "Beach House");
    assert_eq!(row(&v, "name", "rental-apt")["rented"], true);
    assert!(rows(&v).iter().all(|r| r["exists"] == false && r["active"] == false));
}

#[test]
fn no_properties_without_config() {
    let e = Env::new();
    assert!(rows(&run(&e, &["profile", "ls"])).is_empty());
}

#[test]
fn each_property_is_its_own_database() {
    let e = Env::new();
    let v = run(&e, &["--profile", "lake-house", "profile", "init", "--title", "Lake House", "--address", "1 Shore Rd"]);
    assert_eq!(v["data"]["created"], true);
    assert_eq!(v["data"]["encrypted"], true);
    assert!(profile_db(&e, "lake-house").exists());
    // The property is the top-level location, with its address.
    let locs = run(&e, &["--profile", "lake-house", "loc", "ls"]);
    assert_eq!(rows(&locs)[0]["path"], "Lake House");
    assert_eq!(rows(&locs)[0]["kind"], "property");
    assert_eq!(rows(&locs)[0]["address"], "1 Shore Rd");

    run(&e, &["-P", "lake-house", "add", "Mower", "--at", "Lake House / Garage", "--price", "400"]);
    run(&e, &["profile", "init", "rental-apt", "--title", "Rental Apt 4B"]);
    run(&e, &["--profile", "rental-apt", "add", "Kettle", "--at", "Kitchen"]);
    assert_eq!(run(&e, &["--profile", "rental-apt", "show", "Kettle"])["data"]["location"], "Rental Apt 4B / Kitchen");
    // Nothing leaks between properties.
    assert_eq!(run(&e, &["--profile", "lake-house", "ls"])["count"], 1);
    assert_eq!(run(&e, &["--profile", "rental-apt", "ls"])["count"], 1);
    let out = cmd(&e).args(["--profile", "rental-apt", "show", "Mower"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    // WHEREBOX_PROFILE works like --profile; ls marks the active one.
    let out = cmd(&e).env("WHEREBOX_PROFILE", "lake-house").args(["ls", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"][0]["name"], "Mower");
    let v = run(&e, &["--profile", "lake-house", "profile", "ls"]);
    assert_eq!(row(&v, "name", "lake-house")["active"], true);
    assert_eq!(row(&v, "name", "lake-house")["exists"], true);
    // profile show summarises another property without switching.
    let s = run(&e, &["--profile", "rental-apt", "profile", "show", "lake-house"]);
    assert_eq!(s["data"]["items"], 1);
    assert_eq!(s["data"]["value"], 400.0);
    assert_eq!(run(&e, &["profile", "show", "beach-house"])["data"]["exists"], false);
}

#[test]
fn precedence_and_config_tables() {
    let e = Env::new();
    // --profile beats WHEREBOX_DB (set by Env::cmd) ...
    let out = e.cmd().args(["--profile", "beach-house", "db", "path", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["path"], profile_db(&e, "beach-house").to_str().unwrap());
    // ... and an explicit --db beats --profile.
    let other = e.path("elsewhere.db");
    let out = e
        .cmd()
        .args(["--profile", "beach-house", "--db", other.to_str().unwrap(), "db", "path", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["path"], other.to_str().unwrap());
    // [profiles.NAME] in the config file adds properties with their own paths.
    let cfg = e.path("config").join("wherebox-cli");
    std::fs::create_dir_all(&cfg).unwrap();
    let cabin = e.path("vault/cabin.db");
    std::fs::write(
        cfg.join("config.toml"),
        format!(
            "[profiles.cabin]\ntitle = \"The Cabin\"\naddress = \"9 Pine Rd\"\ndb_path = {:?}\n",
            cabin.to_str().unwrap()
        ),
    )
    .unwrap();
    run(&e, &["profile", "init", "cabin"]);
    assert!(cabin.exists());
    let v = run(&e, &["profile", "ls"]);
    assert_eq!(row(&v, "name", "cabin")["source"], "config");
    assert_eq!(run(&e, &["-P", "cabin", "loc", "ls"])["data"][0]["address"], "9 Pine Rd");
    // A default profile in the config file.
    e.cmd().args(["config", "set", "profile", "cabin"]).assert().success();
    assert_eq!(run(&e, &["db", "path"])["data"]["path"], cabin.to_str().unwrap());
    // Bad names are rejected.
    cmd(&e).args(["--profile", "Lake House", "ls"]).assert().code(7);
    let empty = e.path("empty.toml");
    std::fs::write(&empty, "").unwrap();
    cmd(&e).args(["--config", empty.to_str().unwrap(), "profile", "init"]).assert().code(2);
}

/// wherebox-7d9: on a fresh machine (no config dir at all) `profile init`
/// used to fail with "reading config ... No such file or directory".
#[test]
fn init_on_a_fresh_machine_creates_a_private_config() {
    let e = Env::new();
    let cfg_dir = e.path("config").join("wherebox-cli");
    assert!(!e.path("config").exists(), "empty XDG_CONFIG_HOME");
    let v = run(&e, &["profile", "init", "rental-apt"]);
    assert_eq!(v["data"]["created"], true);
    assert!(profile_db(&e, "rental-apt").exists());
    let cfg = cfg_dir.join("config.toml");
    assert!(cfg.exists(), "config file created");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&cfg_dir), 0o700);
        assert_eq!(mode(&cfg), 0o600);
    }
    // First-run key setup with a key file (no WHEREBOX_KEY) works too.
    let e2 = Env::new();
    let out = cmd(&e2)
        .env_remove("WHEREBOX_KEY")
        .env("WHEREBOX_KEY_SOURCE", "file")
        .args(["profile", "init", "lake-house", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(e2.path("config").join("wherebox-cli").join("config.toml").exists());
}
