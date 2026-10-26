//! Per-property profiles: each property is its own wherebox, a separate
//! (separately encrypted) database with its own location tree.
//!
//! `--profile NAME` (env `WHEREBOX_PROFILE`, config `profile`) selects
//! `<data dir>/wherebox-cli/profiles/NAME.db`, or the `db_path` given in the
//! config file's `[profiles.NAME]` table:
//!
//! ```toml
//! [profiles.lake-house]
//! title = "Lake House"
//! address = "1 Shore Rd"
//! db_path = "~/vaults/lake.db"
//! rented = true   # a rental: insurance reports cover contents only
//! ```
//!
//! There are no built-in properties: profiles come from `[profiles.NAME]`
//! tables and from databases found in the profiles directory. Commands that
//! roll up every property (`shopping --all-profiles`, `report insurance
//! --all-profiles`) iterate [`list`] and open each existing database in turn
//! (`commands::profile::each_db`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// A `[profiles.NAME]` table in the config file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConf {
    pub title: Option<String>,
    pub address: Option<String>,
    pub db_path: Option<String>,
    pub rented: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub name: String,
    /// Display name, used for the property's top-level location.
    pub title: String,
    pub address: Option<String>,
    pub db_path: PathBuf,
    /// A rental (renter's insurance: contents only).
    pub rented: bool,
    /// `config` or `found` (a database in the profiles directory).
    pub source: &'static str,
}

/// Profile names are slugs: letters, digits, `-`, `_`.
pub fn validate_name(name: &str) -> Result<String> {
    let t = name.trim().to_lowercase();
    if !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        Ok(t)
    } else {
        Err(AppError::usage(format!(
            "invalid profile name '{name}' (letters, digits, '-', '_'; e.g. '{}')",
            crate::util::slugify(name)
        )))
    }
}

/// `<data dir>/wherebox-cli/profiles`.
pub fn profiles_dir() -> PathBuf {
    let db = crate::paths::default_db_path();
    db.parent().map_or_else(|| PathBuf::from("profiles"), |p| p.join("profiles"))
}

/// The `[profiles.*]` tables of the config file (missing file = none).
pub fn config_entries(config_path: &Path) -> Result<BTreeMap<String, ProfileConf>> {
    let Ok(text) = std::fs::read_to_string(config_path) else { return Ok(BTreeMap::new()) };
    let table: toml::Table = text.parse().map_err(|e| AppError::config(format!("{}: {e}", config_path.display())))?;
    match table.get("profiles") {
        None => Ok(BTreeMap::new()),
        Some(v) => v
            .clone()
            .try_into::<BTreeMap<String, ProfileConf>>()
            .map_err(|e| AppError::config(format!("{} [profiles]: {e}", config_path.display()))),
    }
}

/// Resolve one profile (it need not exist yet).
pub fn get(config_path: &Path, name: &str) -> Result<Profile> {
    get_in(config_path, name, &profiles_dir())
}

fn get_in(config_path: &Path, name: &str, dir: &Path) -> Result<Profile> {
    let name = validate_name(name)?;
    let conf = config_entries(config_path)?.remove(&name);
    let db_path = conf
        .as_ref()
        .and_then(|c| c.db_path.as_deref())
        .map(|p| PathBuf::from(crate::paths::expand_tilde(p)))
        .unwrap_or_else(|| dir.join(format!("{name}.db")));
    let source = if conf.is_some() { "config" } else { "found" };
    Ok(Profile {
        title: conf.as_ref().and_then(|c| c.title.clone()).unwrap_or_else(|| name.clone()),
        address: conf.as_ref().and_then(|c| c.address.clone()),
        rented: conf.as_ref().and_then(|c| c.rented).unwrap_or(false),
        name,
        db_path,
        source,
    })
}

/// Every known profile: configured and databases found in the
/// profiles directory, sorted by name.
pub fn list(config_path: &Path) -> Result<Vec<Profile>> {
    list_in(config_path, &profiles_dir())
}

fn list_in(config_path: &Path, dir: &Path) -> Result<Vec<Profile>> {
    let found = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    e.path()
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_suffix(".db"))
                        .map(str::to_string)
                })
                .filter(|n| validate_name(n).is_ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut names: Vec<String> = config_entries(config_path)?.into_keys().chain(found).collect();
    names.sort();
    names.dedup();
    names.iter().map(|n| get_in(config_path, n, dir)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_config_tables() {
        assert_eq!(validate_name("Lake-House").unwrap(), "lake-house");
        assert!(validate_name("lake house").is_err());
        let t = tempfile::TempDir::new().unwrap();
        let cfg = t.path().join("config.toml");
        std::fs::write(
            &cfg,
            "format = \"json\"\n[profiles.lake]\ntitle = \"Lake House\"\ndb_path = \"/x/lake.db\"\nrented = true\n",
        )
        .unwrap();
        let dir = t.path().join("profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cabin.db"), b"").unwrap();
        let p = get_in(&cfg, "lake", &dir).unwrap();
        assert_eq!((p.title.as_str(), p.source), ("Lake House", "config"));
        assert_eq!(p.db_path, PathBuf::from("/x/lake.db"));
        assert!(p.rented);
        assert_eq!(get_in(&cfg, "cabin", &dir).unwrap().source, "found");
        assert!(!get_in(&cfg, "cabin", &dir).unwrap().rented);
        let names: Vec<String> = list_in(&cfg, &dir).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["cabin", "lake"]);
    }
}
