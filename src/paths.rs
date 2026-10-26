//! Platform-conventional locations for the config file and the database.
//!
//! | platform | config file | database |
//! |----------|-------------|----------|
//! | Linux, BSD | `$XDG_CONFIG_HOME/wherebox-cli/config.toml` (`~/.config/…`) | `$XDG_DATA_HOME/wherebox-cli/wherebox.db` (`~/.local/share/…`) |
//! | macOS | same XDG-style paths as Linux (shipped that way since 0.1) | same |
//! | Windows | `%APPDATA%\wherebox-cli\config.toml` | `%LOCALAPPDATA%\wherebox-cli\wherebox.db` |
//!
//! `XDG_CONFIG_HOME` / `XDG_DATA_HOME` are honoured on every platform when
//! set to an absolute path, and `WHEREBOX_CONFIG` / `WHEREBOX_DB` override
//! both (see [`crate::config`]).
//!
//! These functions are the only place default locations are decided, and the
//! resolved `db_path` setting is what every command opens. Up to 0.2,
//! wherebox used `~/.config` and `~/.local/share` on every OS; on Windows
//! those are now legacy locations. When nothing exists at the platform
//! location but a file exists at a legacy one, the legacy file is used, so a
//! new empty database (or config) never shadows an existing one.

use std::path::PathBuf;

const APP: &str = "wherebox-cli";

/// The user's home directory: `$HOME` when set, else what the OS reports
/// (`%USERPROFILE%` on Windows, the passwd entry on Unix).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()))
}

fn home() -> PathBuf {
    home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `$var` if it is an absolute path, else `platform()`.
fn base(var: &str, platform: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(platform)
}

#[cfg(not(windows))]
fn platform_config() -> PathBuf {
    home().join(".config")
}
#[cfg(not(windows))]
fn platform_data() -> PathBuf {
    home().join(".local").join("share")
}

/// `%APPDATA%` (roaming): the config file is small and follows the user.
#[cfg(windows)]
fn platform_config() -> PathBuf {
    directories::BaseDirs::new().map_or_else(|| home().join("AppData").join("Roaming"), |b| b.config_dir().into())
}
/// `%LOCALAPPDATA%`: the database stays on this machine.
#[cfg(windows)]
fn platform_data() -> PathBuf {
    directories::BaseDirs::new().map_or_else(|| home().join("AppData").join("Local"), |b| b.data_local_dir().into())
}

/// `preferred` unless it is missing and one of `legacy` exists.
pub fn resolve(preferred: PathBuf, legacy: &[PathBuf]) -> PathBuf {
    if preferred.exists() {
        return preferred;
    }
    legacy.iter().find(|p| p.exists()).cloned().unwrap_or(preferred)
}

/// Where 0.2 and earlier kept `file` under `dir` (`.config` / `.local/share`).
fn legacy(var: &str, dir: &[&str], file: &str) -> Vec<PathBuf> {
    if std::env::var_os(var).is_some_and(|v| std::path::Path::new(&v).is_absolute()) {
        return Vec::new(); // XDG_* set: same location as before
    }
    let p = dir.iter().fold(home(), |p, d| p.join(d)).join(APP).join(file);
    vec![p]
}

pub fn default_config_path() -> PathBuf {
    let preferred = base("XDG_CONFIG_HOME", platform_config).join(APP).join("config.toml");
    resolve(preferred, &legacy("XDG_CONFIG_HOME", &[".config"], "config.toml"))
}

pub fn default_db_path() -> PathBuf {
    let preferred = base("XDG_DATA_HOME", platform_data).join(APP).join("wherebox.db");
    resolve(preferred, &legacy("XDG_DATA_HOME", &[".local", "share"], "wherebox.db"))
}

/// Expand a leading `~/` (also `~\` on Windows) to the home directory.
pub fn expand_tilde(p: &str) -> String {
    let rest = p.strip_prefix("~/").or_else(|| if cfg!(windows) { p.strip_prefix("~\\") } else { None });
    match (rest, home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_end_in_app_dir() {
        assert!(default_db_path().ends_with(format!("{APP}/wherebox.db")));
        assert!(default_config_path().ends_with(format!("{APP}/config.toml")));
    }

    #[test]
    fn an_existing_legacy_file_wins_over_a_missing_preferred_one() {
        let t = tempfile::TempDir::new().unwrap();
        let (new, old) = (t.path().join("new").join("x.db"), t.path().join("old").join("x.db"));
        assert_eq!(resolve(new.clone(), std::slice::from_ref(&old)), new, "nothing anywhere: preferred");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"db").unwrap();
        assert_eq!(resolve(new.clone(), std::slice::from_ref(&old)), old, "legacy exists: never shadow it");
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, b"db").unwrap();
        assert_eq!(resolve(new.clone(), &[old]), new, "both: preferred");
    }

    #[test]
    fn tilde_only_expands_a_prefix() {
        assert_eq!(expand_tilde("/abs/x.db"), "/abs/x.db");
        assert_eq!(expand_tilde("a~/x.db"), "a~/x.db");
        if let Some(home) = home_dir() {
            assert_eq!(PathBuf::from(expand_tilde("~/x.db")), home.join("x.db"));
        }
    }
}
