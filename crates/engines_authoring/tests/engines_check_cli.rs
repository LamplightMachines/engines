//! `engines-check` over the process boundary: exit codes, JSON lines, the tracked docs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn engines_check(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_engines-check")).args(args).output().unwrap();
    (out.status.code().unwrap(), String::from_utf8(out.stdout).unwrap())
}

fn scratch(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("engines-check-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn the_template_and_the_house_style_fixtures_pass() {
    let docs = [
        repo().join("doc/loop_contract_v0.9.json5"),
        repo().join("lab/tests/fixtures/loop_drone_d_lydian.json5"),
        repo().join("lab/tests/fixtures/song_lydian_tide.json5"),
    ];
    let args: Vec<&str> = docs.iter().map(|p| p.to_str().unwrap()).collect();
    let (code, out) = engines_check(&args);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.matches(": ok — 0 errors").count(), 3, "{out}");
    assert!(out.contains("structure:"), "{out}");
}

#[test]
fn a_typo_exits_1_with_a_located_json_diagnostic() {
    let src = std::fs::read_to_string(repo().join("doc/loop_contract_v0.9.json5")).unwrap();
    assert!(src.contains("\"cutoff_hz\":"), "the template authors a cutoff");
    let p = scratch("typo.json5", &src.replacen("\"cutoff_hz\":", "\"cutof_hz\":", 1));
    let (code, out) = engines_check(&["--json", p.to_str().unwrap()]);
    assert_eq!(code, 1, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "one JSON object per doc per line");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["ok"], false);
    let d = v["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "unknown-key").expect("the typo");
    assert!(d["path"].as_str().unwrap().ends_with(".cutof_hz"), "{d}");
    assert!(d["message"].as_str().unwrap().contains("did you mean `cutoff_hz`"), "{d}");
}

#[test]
fn an_unreadable_file_exits_2() {
    let (code, _) = engines_check(&["/definitely/not/here.json5"]);
    assert_eq!(code, 2);
}

#[test]
fn the_catalog_prints_for_tools_and_people() {
    let (code, out) = engines_check(&["--catalog", "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("one JSON object");
    assert_eq!(v["catalog_version"], 2);
    assert_eq!(v["params"]["cutoff_hz"]["unit_maps"][0], "cutoff_hz");
    assert_eq!(v["unit_maps"]["cutoff_hz"]["samples"][5], serde_json::json!([0.5, 800.0]));
    let (code, out) = engines_check(&["--catalog"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("capability catalog v2 — loop schema 0.9") && out.contains("subtractive (default)") && out.contains("cutoff_hz"), "{out}");
    // --catalog takes no docs; without it, docs are required.
    assert_eq!(engines_check(&["--catalog", "doc.json5"]).0, 2);
    assert_eq!(engines_check(&[]).0, 2);
}
