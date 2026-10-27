//! Layered configuration: built-in defaults < config file < `WHEREBOX_*`
//! environment variables < command-line flags. Every resolved value remembers
//! the layer it came from (`config show --effective`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{AppError, Result};
use crate::util::Tz;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Default,
    File,
    Env,
    Flag,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Env => "env",
            Self::Flag => "flag",
        }
    }
}

pub struct Setting {
    pub key: &'static str,
    pub env: &'static [&'static str],
    pub help: &'static str,
    pub choices: &'static [&'static str],
    pub kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Str,
    Bool,
    /// Decimal places (0..=12).
    Uint,
    Char,
}

pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "db_path",
        env: &["WHEREBOX_DB", "WHEREBOX_DB_PATH"],
        help: "database file",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "profile",
        env: &["WHEREBOX_PROFILE"],
        help: "property profile: selects <data dir>/wherebox-cli/profiles/<name>.db (see `wherebox profile ls`)",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "expiring_within",
        env: &["WHEREBOX_EXPIRING_WITHIN"],
        help: "default look-ahead for `wherebox expiring` (e.g. 3d, 2w)",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "default_cadence",
        env: &["WHEREBOX_DEFAULT_CADENCE"],
        help: "how long a non-perishable lot is presumed on hand without a learned reorder cadence or a [cadence] entry (e.g. 90d, 3m)",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "match_threshold",
        env: &["WHEREBOX_MATCH_THRESHOLD"],
        help: "fuzzy name-match threshold for lists, cohorts and `match` (0-1)",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "format",
        env: &["WHEREBOX_FORMAT"],
        help: "default output format",
        choices: crate::output::Format::NAMES,
        kind: Kind::Str,
    },
    Setting {
        key: "date_format",
        env: &["WHEREBOX_DATE_FORMAT"],
        help: "strftime format for dates in text output (table/csv/tsv/markdown/html/org) and extra import format",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "timezone",
        env: &["WHEREBOX_TIMEZONE", "WHEREBOX_TZ"],
        help: "local, UTC, IANA name or +HH:MM",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "color",
        env: &["WHEREBOX_COLOR"],
        help: "colour table output",
        choices: &["auto", "always", "never"],
        kind: Kind::Str,
    },
    Setting {
        key: "precision",
        env: &["WHEREBOX_PRECISION"],
        help: "decimal places in text output (table/csv/tsv/markdown/html/org)",
        choices: &[],
        kind: Kind::Uint,
    },
    Setting {
        key: "csv_delimiter",
        env: &["WHEREBOX_CSV_DELIMITER"],
        help: "CSV field delimiter (single character, or 'tab')",
        choices: &[],
        kind: Kind::Char,
    },
    Setting {
        key: "csv_quote",
        env: &["WHEREBOX_CSV_QUOTE"],
        help: "CSV quote character",
        choices: &[],
        kind: Kind::Char,
    },
    Setting {
        key: "csv_header",
        env: &["WHEREBOX_CSV_HEADER"],
        help: "write a header row in csv/tsv output",
        choices: &[],
        kind: Kind::Bool,
    },
    Setting {
        key: "null",
        env: &["WHEREBOX_NULL"],
        help: "text for missing values in text output (table/csv/tsv/markdown/html/org)",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "key_source",
        env: &["WHEREBOX_KEY_SOURCE"],
        help: "how databases are encrypted (auto = your SSH key, else a key file; WHEREBOX_KEY when set)",
        choices: &["auto", "ssh", "file", "env", "passphrase"],
        kind: Kind::Str,
    },
    Setting {
        key: "ssh_key",
        env: &["WHEREBOX_SSH_KEY"],
        help: "SSH private key for encryption (default: ~/.ssh/id_ed25519, then ~/.ssh/id_rsa)",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "key_file",
        env: &["WHEREBOX_KEY_FILE"],
        help: "key file for the database (default: <config dir>/keys/<database id>.key)",
        choices: &[],
        kind: Kind::Str,
    },
    Setting {
        key: "quiet",
        env: &["WHEREBOX_QUIET"],
        help: "suppress informational messages",
        choices: &[],
        kind: Kind::Bool,
    },
    Setting {
        key: "verbose",
        env: &["WHEREBOX_VERBOSE"],
        help: "print extra diagnostics to stderr",
        choices: &[],
        kind: Kind::Bool,
    },
];

pub fn setting(key: &str) -> Option<&'static Setting> {
    let k = key.replace('-', "_");
    SETTINGS.iter().find(|s| s.key == k)
}

pub use crate::paths::{default_config_path, default_db_path};

fn default_value(key: &str) -> String {
    match key {
        "db_path" => default_db_path().to_string_lossy().into_owned(),
        "format" => "table".into(),
        "date_format" => "%Y-%m-%d".into(),
        "timezone" => "local".into(),
        "color" => "auto".into(),
        "precision" => "2".into(),
        "csv_delimiter" => ",".into(),
        "csv_quote" => "\"".into(),
        "csv_header" => "true".into(),
        "expiring_within" => "7d".into(),
        "default_cadence" => "90d".into(),
        "match_threshold" => "0.82".into(),
        "key_source" => "auto".into(),
        "quiet" | "verbose" => "false".into(),
        _ => String::new(),
    }
}

/// Validate and normalise a raw value for `key`.
pub fn normalize(key: &str, raw: &str) -> Result<String> {
    let s = setting(key).ok_or_else(|| AppError::config(format!("unknown config key '{key}'")))?;
    let v = raw.trim();
    let bad = |why: &str| AppError::config(format!("invalid value '{raw}' for {}: {why}", s.key));
    match s.kind {
        Kind::Bool => match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok("true".into()),
            "0" | "false" | "no" | "off" | "" => Ok("false".into()),
            _ => Err(bad("expected true/false")),
        },
        Kind::Uint => {
            v.parse::<u8>().map(|n| n.min(12).to_string()).map_err(|_| bad("expected a small non-negative integer"))
        }
        Kind::Char => match raw {
            "tab" | "\\t" | "\t" => Ok("\t".into()),
            c if c.chars().count() == 1 && c.is_ascii() => Ok(c.into()),
            _ => Err(bad("expected a single ASCII character or 'tab'")),
        },
        Kind::Str if !s.choices.is_empty() => {
            let l = v.to_ascii_lowercase();
            if s.choices.contains(&l.as_str()) {
                Ok(l)
            } else {
                Err(bad(&format!("expected one of {}", s.choices.join(", "))))
            }
        }
        Kind::Str if s.key == "timezone" => Tz::parse(v).map(|_| v.to_string()),
        Kind::Str if s.key == "profile" && !v.is_empty() => {
            crate::profiles::validate_name(v).map_err(|e| bad(&e.message))
        }
        Kind::Str if s.key == "match_threshold" => match v.trim().parse::<f64>() {
            Ok(t) if (0.0..=1.0).contains(&t) => Ok(v.trim().to_string()),
            _ => Err(bad("expected a number from 0 to 1")),
        },
        Kind::Str if matches!(s.key, "expiring_within" | "default_cadence") => {
            crate::util::parse_duration_days(v).map(|_| v.to_string()).map_err(|e| bad(&e.message))
        }
        Kind::Str => Ok(v.to_string()),
    }
}

/// One layer of raw key/value pairs.
pub type Layer = Vec<(String, String)>;

/// Resolved settings with provenance, in `SETTINGS` order.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub values: BTreeMap<&'static str, (String, Source)>,
    pub config_path: PathBuf,
    pub config_path_source: Source,
    pub config_file_exists: bool,
}

impl Resolved {
    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map_or("", |(v, _)| v.as_str())
    }
    pub fn source(&self, key: &str) -> Source {
        self.values.get(key).map_or(Source::Default, |(_, s)| *s)
    }
    pub fn flag(&self, key: &str) -> bool {
        self.get(key) == "true"
    }
    /// Settings in canonical display order.
    pub fn ordered(&self) -> Vec<(&'static str, &str, Source)> {
        SETTINGS.iter().filter_map(|s| self.values.get(s.key).map(|(v, src)| (s.key, v.as_str(), *src))).collect()
    }
}

/// Fold layers (lowest precedence first) over the defaults.
pub fn resolve_layers(layers: &[(Source, Layer)]) -> Result<BTreeMap<&'static str, (String, Source)>> {
    let defaults =
        SETTINGS.iter().map(|s| (s.key, (default_value(s.key), Source::Default))).collect::<BTreeMap<_, _>>();
    layers.iter().try_fold(defaults, |mut acc, (src, layer)| {
        layer.iter().try_for_each(|(k, v)| {
            let s =
                setting(k).ok_or_else(|| AppError::config(format!("unknown config key '{k}' ({})", src.as_str())))?;
            let v = normalize(s.key, v).map_err(|e| e.context(src.as_str()))?;
            acc.insert(s.key, (v, *src));
            Ok::<_, AppError>(())
        })?;
        Ok(acc)
    })
}

/// Locate the config file: `--config` > `WHEREBOX_CONFIG` > platform default
/// (see [`crate::paths`]).
pub fn locate(flag: Option<&Path>) -> (PathBuf, Source) {
    flag.map(|p| (p.to_path_buf(), Source::Flag))
        .or_else(|| {
            std::env::var_os("WHEREBOX_CONFIG").filter(|v| !v.is_empty()).map(|v| (PathBuf::from(v), Source::Env))
        })
        .unwrap_or_else(|| (default_config_path(), Source::Default))
}

/// Read the config file as a flat layer (missing default file = empty layer).
pub fn file_layer(path: &Path, explicit: bool) -> Result<Layer> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_toml_layer(&text).map_err(|e| e.context(path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(Vec::new()),
        Err(e) => Err(AppError::config(format!("reading config {}: {e}", path.display()))),
    }
}

fn toml_scalar(v: &toml::Value) -> Option<String> {
    match v {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Accepts flat keys and one level of tables (`[csv] delimiter = ";"` ==
/// `csv_delimiter = ";"`).
/// The `[encryption]` section is wherebox's record of database keys
/// ([`crate::enc_config`]); `[profiles.NAME]` tables describe property
/// profiles ([`crate::profiles`]); `[shelf_life.<type>]` tables are
/// shelf-life types ([`crate::shelf_life`]) and `[cadence]` default reorder
/// cadences ([`crate::onhand`]). None of them holds settings.
pub fn parse_toml_layer(text: &str) -> Result<Layer> {
    let table: toml::Table = text.parse().map_err(|e| AppError::config(format!("TOML: {e}")))?;
    table
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "encryption" | "profiles" | "shelf_life" | "cadence"))
        .flat_map(|(k, v)| match v {
            toml::Value::Table(t) => t.iter().map(|(k2, v2)| (format!("{k}_{k2}"), v2.clone())).collect::<Vec<_>>(),
            other => vec![(k.clone(), other.clone())],
        })
        .map(|(k, v)| {
            toml_scalar(&v)
                .map(|s| (k.clone(), s))
                .ok_or_else(|| AppError::config(format!("config key '{k}' must be a string, number or boolean")))
        })
        .collect()
}

/// `WHEREBOX_*` environment layer.
pub fn env_layer() -> Layer {
    SETTINGS
        .iter()
        .filter_map(|s| {
            s.env.iter().find_map(|e| std::env::var(e).ok().filter(|v| !v.is_empty())).map(|v| (s.key.to_string(), v))
        })
        .chain(
            std::env::var_os("NO_COLOR")
                .filter(|v| !v.is_empty())
                .filter(|_| std::env::var_os("WHEREBOX_COLOR").is_none())
                .map(|_| ("color".to_string(), "never".to_string())),
        )
        .collect()
}

pub fn resolve(config_flag: Option<&Path>, flags: Layer) -> Result<Resolved> {
    let (config_path, config_path_source) = locate(config_flag);
    if config_path_source == Source::Default {
        // First run: create the default config (best effort, e.g. a read-only
        // home just leaves it missing) so later readers and writers find it.
        let _ = ensure_file(&config_path);
    }
    let file = file_layer(&config_path, config_path_source != Source::Default)?;
    let values = resolve_layers(&[(Source::File, file), (Source::Env, env_layer()), (Source::Flag, flags)])?;
    Ok(Resolved { values, config_file_exists: config_path.exists(), config_path, config_path_source })
}

/// Create `path` (empty, owner-only: 0600) and any missing parent
/// directories (owner-only: 0700) unless it already exists. Returns whether
/// the file was created.
pub fn ensure_file(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let io_err = |p: &Path, e: std::io::Error| AppError::io(format!("creating {}: {e}", p.display()));
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let missing: Vec<&Path> = dir.ancestors().take_while(|d| !d.as_os_str().is_empty() && !d.exists()).collect();
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        for d in missing.iter().rev() {
            crate::perms::restrict_dir(d).map_err(|e| io_err(d, e))?;
        }
    }
    crate::crypto::open_private_append(path).map_err(|e| io_err(path, e))?;
    Ok(true)
}

/// Set `key = value` in the TOML file at `path`, creating it if needed.
pub fn write_setting(path: &Path, key: &str, value: Option<&str>) -> Result<()> {
    let s = setting(key).ok_or_else(|| AppError::config(format!("unknown config key '{key}'")))?;
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let (head, block) = crate::enc_config::split(&text);
    let mut table: toml::Table = head.parse().map_err(|e| AppError::config(format!("{}: {e}", path.display())))?;
    match value {
        Some(v) => {
            let v = normalize(s.key, v)?;
            let tv = match s.kind {
                Kind::Bool => toml::Value::Boolean(v == "true"),
                Kind::Uint => toml::Value::Integer(v.parse().unwrap_or(2)),
                _ => toml::Value::String(v),
            };
            table.insert(s.key.to_string(), tv);
        }
        None => {
            table.remove(s.key);
        }
    }
    ensure_file(path)?;
    let mut out = toml::to_string_pretty(&table).map_err(|e| AppError::config(e.to_string()))?;
    if !block.is_empty() {
        out = format!("{}\n\n{block}", out.trim_end());
    }
    std::fs::write(path, out).map_err(|e| AppError::io(format!("writing {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(pairs: &[(&str, &str)]) -> Layer {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn layers_override_in_order() {
        let r = resolve_layers(&[
            (Source::File, layer(&[("format", "csv"), ("precision", "3")])),
            (Source::Env, layer(&[("format", "json")])),
            (Source::Flag, layer(&[("precision", "1")])),
        ])
        .unwrap();
        assert_eq!(r["format"], ("json".to_string(), Source::Env));
        assert_eq!(r["precision"], ("1".to_string(), Source::Flag));
        assert_eq!(r["color"], ("auto".to_string(), Source::Default));
    }

    #[test]
    fn validates_values() {
        assert!(resolve_layers(&[(Source::File, layer(&[("format", "xml")]))]).is_err());
        assert!(resolve_layers(&[(Source::File, layer(&[("nope", "1")]))]).is_err());
        assert_eq!(normalize("csv_delimiter", "tab").unwrap(), "\t");
        assert_eq!(normalize("quiet", "YES").unwrap(), "true");
        assert!(normalize("timezone", "Mars/Olympus").is_err());
        assert!(normalize("timezone", "Europe/Berlin").is_ok());
    }

    #[test]
    fn ensure_file_creates_private_dirs_and_file() {
        let t = tempfile::TempDir::new().unwrap();
        let cfg = t.path().join("a").join("wherebox-cli").join("config.toml");
        assert!(ensure_file(&cfg).unwrap());
        assert!(!ensure_file(&cfg).unwrap(), "existing file left alone");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&cfg), 0o600);
            assert_eq!(mode(cfg.parent().unwrap()), 0o700);
            assert_eq!(mode(&t.path().join("a")), 0o700);
        }
    }

    #[test]
    fn toml_tables_flatten() {
        let l =
            parse_toml_layer("format = \"json\"\nprecision = 3\n[csv]\ndelimiter = \";\"\nheader = false\n").unwrap();
        assert!(l.contains(&("csv_delimiter".into(), ";".into())));
        assert!(l.contains(&("csv_header".into(), "false".into())));
        assert!(l.contains(&("precision".into(), "3".into())));
    }
}
