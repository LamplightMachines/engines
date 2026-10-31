//! The house-style reference pieces aren't published: no test or fixture reads the skills'
//! `reference/house/` or the old "Idle" fixtures. The tests use the neutral docs
//! (`lab/tests/fixtures/loop_drone_d_lydian.json5`, `song_lydian_tide.json5`) instead.

use std::path::{Path, PathBuf};

/// What a test may not name.
const PRIVATE: [&str; 5] = ["reference/house", "loop_engine_drone_a_mixolydian", "song_idle_night", "idle-night-v2", "house/idle"];

/// The open crates' sources and tests, the lab's tests and the extension's tests.
const OPEN: [&str; 9] = [
    "crates/engines_core",
    "crates/engines_live",
    "crates/engines_authoring",
    "crates/engines_tui",
    "lab/tests",
    "lab/engines_lab",
    "editors/vscode/src",
    "editors/vscode/test",
    "editors/vscode/test-e2e",
];

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() && !matches!(name.as_str(), "target" | "node_modules" | "out" | "__pycache__") {
            files(&p, out);
        } else if p.is_file() && ["rs", "py", "ts", "js", "json", "json5"].iter().any(|x| name.ends_with(&format!(".{x}"))) {
            out.push(p);
        }
    }
}

#[test]
fn no_open_test_reads_the_house_pieces() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let me = Path::new(file!()).file_name().unwrap().to_owned();
    let mut all = Vec::new();
    for d in OPEN {
        files(&repo.join(d), &mut all);
    }
    assert!(all.len() > 50, "found the open trees: {}", all.len());
    let hits: Vec<String> = all
        .iter()
        .filter(|p| p.file_name().unwrap() != me.as_os_str())
        .flat_map(|p| {
            let text = std::fs::read_to_string(p).unwrap_or_default();
            PRIVATE.iter().filter(|w| text.contains(*w)).map(|w| format!("{}: {w}", p.display())).collect::<Vec<_>>()
        })
        .collect();
    assert!(hits.is_empty(), "open tests name private fixtures:\n{}", hits.join("\n"));
}
