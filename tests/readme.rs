//! Golden check for the README sample session: `docs/readme-session.txt` (and
//! the copy embedded in README.md) must match what the binary prints now.
//! Regenerate both with `just readme` (scripts/readme-samples.sh).

use std::path::Path;
use std::process::Command;

fn begin(name: &str) -> String {
    format!("<!-- {name}:begin -->\n```console\n")
}

fn end(name: &str) -> String {
    format!("```\n<!-- {name}:end -->")
}

fn repo_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Replay `scripts/readme-session.sh SESSION` and compare with `docs/GOLDEN`.
fn check_session(session: &str, golden: &str) {
    let bin = Path::new(env!("CARGO_BIN_EXE_wherebox"));
    let path = format!("{}:{}", bin.parent().unwrap().display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new("sh")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/readme-session.sh"))
        .arg(session)
        .env("PATH", path)
        .env("WHEREBOX_COLOR", "never")
        .output()
        .unwrap();
    assert!(out.status.success(), "readme-session.sh {session} failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let actual = String::from_utf8(out.stdout).unwrap();
    let expected = repo_file(golden);
    assert!(
        actual == expected,
        "{golden} is stale; run `just readme`.\n--- expected\n{expected}\n--- actual\n{actual}"
    );
}

/// The README block between the `name` markers must equal `golden`.
fn check_embedded(name: &str, golden: &str) {
    let readme = repo_file("README.md");
    let (b, e) = (begin(name), end(name));
    let start = readme.find(&b).unwrap_or_else(|| panic!("README.md lacks the {name}:begin marker")) + b.len();
    let len = readme[start..].find(&e).unwrap_or_else(|| panic!("README.md lacks the {name}:end marker"));
    assert!(
        readme[start..start + len] == repo_file(golden),
        "README.md {name} block differs from {golden}; run `just readme`"
    );
}

#[test]
#[cfg_attr(not(unix), ignore = "replays scripts/readme-session.sh, which needs a POSIX sh (checked on Linux/macOS)")]
fn readme_session_matches_binary() {
    check_session("full", "docs/readme-session.txt");
}

#[test]
#[cfg_attr(not(unix), ignore = "replays scripts/readme-session.sh, which needs a POSIX sh (checked on Linux/macOS)")]
fn readme_embeds_session() {
    check_embedded("readme-session", "docs/readme-session.txt");
}
