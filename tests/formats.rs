//! Every `--format` against golden files, plus round-trips proving the
//! structured formats (YAML, TOON) carry exactly the JSON envelope.
//!
//! Regenerate the goldens with `WHEREBOX_BLESS=1 cargo test --test formats`.

mod support;

use serde_json::Value;
use support::{check_golden, Env};

/// Canonical name and golden-file extension of every output format.
const FORMATS: &[(&str, &str)] = &[
    ("table", "txt"),
    ("json", "json"),
    ("jsonl", "jsonl"),
    ("csv", "csv"),
    ("tsv", "tsv"),
    ("yaml", "yaml"),
    ("toon", "toon"),
    ("markdown", "md"),
    ("html", "html"),
    ("org", "org"),
];

/// The Homebox fixture (fixed ids) with time pinned to 2026-01-01.
fn seeded() -> Env {
    let e = Env::pinned();
    e.seed();
    e
}

const LS: &[&str] = &["ls", "--at", "Garage"];

#[test]
fn every_format_matches_its_golden() {
    let e = seeded();
    for (format, ext) in FORMATS {
        check_golden(&format!("formats/ls.{ext}"), &e.run(&[LS, &["-f", format]].concat()));
    }
    // An object body (key/value pairs) in the text-table formats.
    for (format, ext) in [("table", "txt"), ("markdown", "md"), ("html", "html"), ("org", "org")] {
        check_golden(&format!("formats/show.{ext}"), &e.run(&["show", "000-001", "-f", format]));
    }
    check_golden("formats/loc-ls.txt", &e.run(&["loc", "ls"]));
}

#[test]
fn columns_and_no_header_are_honoured() {
    let e = seeded();
    let sel = [LS, &["--columns", "name,value", "--no-header"]].concat();
    let org = e.run(&[&sel[..], &["-f", "org"]].concat());
    assert_eq!(org, "| Red Toolbox | 89.00 |\n| Socket Set |  |\n| Cordless Drill | 149.99 |\n");
    let html = e.run(&[&sel[..], &["-f", "html"]].concat());
    assert!(!html.contains("<thead>") && html.contains("<td>Red Toolbox</td><td>89.00</td>"), "{html}");
    // Markdown tables cannot exist without a header row, so it stays.
    let md = e.run(&[&sel[..], &["-f", "md"]].concat());
    assert!(md.starts_with("| name | value |\n|---|---|\n"), "{md}");
}

/// Numbers as f64, so `112.0` (JSON) equals TOON's canonical `112`.
fn numbers_as_f64(v: Value) -> Value {
    match v {
        Value::Number(n) => serde_json::json!(n.as_f64()),
        Value::Array(a) => Value::Array(a.into_iter().map(numbers_as_f64).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, numbers_as_f64(v))).collect()),
        other => other,
    }
}

#[test]
fn yaml_and_toon_carry_the_json_envelope() {
    let e = seeded();
    let json: Value = serde_json::from_str(&e.run(&[LS, &["-f", "json"]].concat())).unwrap();
    let yaml: Value = yaml_serde::from_str(&e.run(&[LS, &["-f", "yaml"]].concat())).unwrap();
    assert_eq!(yaml, json);
    let toon: Value = toon_format::decode_no_coerce(&e.run(&[LS, &["-f", "toon"]].concat())).unwrap();
    assert_eq!(numbers_as_f64(toon), numbers_as_f64(json));
}

#[test]
fn aliases_and_config_default() {
    let e = seeded();
    let md = e.run(&[LS, &["-f", "markdown"]].concat());
    for alias in ["md", "MD"] {
        assert_eq!(e.run(&[LS, &["-f", alias]].concat()), md);
    }
    assert_eq!(e.run(&[LS, &["-f", "yml"]].concat()), e.run(&[LS, &["-f", "yaml"]].concat()));
    assert_eq!(e.run(&[LS, &["-f", "ndjson"]].concat()), e.run(&[LS, &["-f", "jsonl"]].concat()));
    // The default format comes from config/env like every other setting.
    let out = e.cmd().args(LS).env("WHEREBOX_FORMAT", "toon").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), e.run(&[LS, &["-f", "toon"]].concat()));
    e.run(&["config", "set", "format", "org"]);
    assert!(e.run(LS).starts_with("| asset_id | name |"));
}

#[test]
fn structured_exports_nest_lots() {
    let e = seeded();
    for format in ["json", "yaml", "toon"] {
        let out = e.run(&["export", "-f", format]);
        assert!(out.contains("01J9W0000000000000000ABC01"), "{format} export lost the lots:\n{out}");
    }
}
