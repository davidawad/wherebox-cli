//! The default location: where you are, as a profile and/or a street address.
//!
//! ```toml
//! [location]
//! cmd = "wherebox-location"   # prints {"profile": "maris", "address": "..."}
//! profile = "maris"           # used when cmd is unset or fails
//! address = "10520 Lakeline Mall Dr, Austin, TX"
//! ```
//!
//! `location_cmd` (env `WHEREBOX_LOCATION_CMD`) runs through `sh -c` and
//! prints one JSON object with optional `profile` and `address` strings.
//! When it is unset, fails, times out or prints nothing usable, the static
//! `location_profile` / `location_address` settings are used instead; when
//! those are unset too there is no location. Nothing here is ever an error.
//!
//! A location picks the profile only when nothing else chose a database
//! (`--profile`, `WHEREBOX_PROFILE`, `profile`, `--db`, `WHEREBOX_DB`), and
//! then fills in that profile's address when its config has none.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::Resolved;

/// How long `location_cmd` may run before it is killed and ignored.
pub const CMD_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    pub profile: Option<String>,
    pub address: Option<String>,
    /// `cmd` or `config`.
    pub source: &'static str,
}

/// The location from `location_cmd`, else from the static settings.
/// `note` receives a line for each step that was skipped (verbose output).
pub fn resolve(r: &Resolved, note: &dyn Fn(&str)) -> Option<Location> {
    let from_cmd =
        Some(r.get("location_cmd").trim()).filter(|c| !c.is_empty()).and_then(|c| match run(c, CMD_TIMEOUT) {
            Ok(out) => parse(&out, "cmd").or_else(|| {
                note("location_cmd printed no profile or address; ignored");
                None
            }),
            Err(e) => {
                note(&format!("location_cmd ignored: {e}"));
                None
            }
        });
    from_cmd.or_else(|| build(r.get("location_profile"), r.get("location_address"), "config"))
}

fn build(profile: &str, address: &str, source: &'static str) -> Option<Location> {
    let profile = Some(profile.trim()).filter(|p| !p.is_empty()).and_then(|p| crate::profiles::validate_name(p).ok());
    let address = Some(address.trim()).filter(|a| !a.is_empty()).map(str::to_string);
    (profile.is_some() || address.is_some()).then_some(Location { profile, address, source })
}

/// Parse the command's output: one JSON object; anything else is no location.
pub fn parse(out: &str, source: &'static str) -> Option<Location> {
    let v: serde_json::Value = serde_json::from_str(out.trim()).ok()?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    build(field("profile"), field("address"), source)
}

/// Run `cmd` through `sh -c`, returning stdout of a successful run.
fn run(cmd: &str, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run: {e}"))?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    let mut out = String::new();
    child.stdout.take().map(|mut s| s.read_to_string(&mut out)).transpose().map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{resolve_layers, Source};

    fn resolved(pairs: &[(&str, &str)]) -> Resolved {
        let layer = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Resolved {
            values: resolve_layers(&[(Source::File, layer)]).unwrap(),
            config_path: "/nonexistent/config.toml".into(),
            config_path_source: Source::Flag,
            config_file_exists: false,
        }
    }

    fn quiet(_: &str) {}

    #[test]
    fn parses_command_output() {
        let l = parse(r#"{"profile": "Maris", "address": " 1 Main St "}"#, "cmd").unwrap();
        assert_eq!(l.profile.as_deref(), Some("maris"));
        assert_eq!(l.address.as_deref(), Some("1 Main St"));
        assert_eq!(parse(r#"{"address": "Austin, TX"}"#, "cmd").unwrap().profile, None);
        assert_eq!(parse(r#"{"profile": "bad name!"}"#, "cmd"), None);
        assert_eq!(parse("", "cmd"), None);
        assert_eq!(parse("maris", "cmd"), None);
        assert_eq!(parse("{}", "cmd"), None);
    }

    #[test]
    fn command_wins_then_static_then_nothing() {
        let stat = [("location_profile", "lake"), ("location_address", "1 Shore Rd")];
        let cmd = r#"printf '{"profile":"maris","address":"10 A St"}'"#;
        let with_cmd = resolved(&[stat[0], stat[1], ("location_cmd", cmd)]);
        let l = resolve(&with_cmd, &quiet).unwrap();
        assert_eq!((l.profile.as_deref(), l.address.as_deref(), l.source), (Some("maris"), Some("10 A St"), "cmd"));
        for broken in ["exit 1", "true", "echo not json", "/no/such/command"] {
            let l = resolve(&resolved(&[stat[0], stat[1], ("location_cmd", broken)]), &quiet).unwrap();
            assert_eq!((l.profile.as_deref(), l.source), (Some("lake"), "config"), "{broken}");
        }
        assert_eq!(resolve(&resolved(&[("location_cmd", "exit 1")]), &quiet), None);
        assert_eq!(resolve(&resolved(&[]), &quiet), None);
    }

    #[test]
    fn slow_command_is_killed() {
        let start = Instant::now();
        assert!(run("sleep 5", Duration::from_millis(200)).unwrap_err().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
