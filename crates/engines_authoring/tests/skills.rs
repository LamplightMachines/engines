//! The portable skills: `skills/engines-new`, `engines-edit` and `engines-song` are one
//! copy for Claude Code (linked from `.claude/skills/`) and the VS Code extension, so they must run
//! anywhere the bundled `engines-*` commands do. This pins what that takes:
//! - they call only the bundled commands — no `make`, `cargo`, `uv`, Python or `engines-render` — and
//!   name no agent's question tool;
//! - each folder is named for its `name` field, its description within 1024 characters (VS Code's
//!   `chatSkills` requires both);
//! - each shared reference (`skills/shared/`) is in every skill unchanged (`make skills` copies it);
//! - `engines-new`'s skeleton carries `engines-defaults.json5`'s control settings and checks clean;
//! - `.claude/skills/` links to the same folders and has no `piece-*` skill left.

use std::path::{Path, PathBuf};

const SKILLS: [&str; 3] = ["engines-new", "engines-edit", "engines-song"];

/// The marked line that says where the tools are (K3 rewrites it in the shipped copy).
const BIN_OPEN: &str = "<!-- engines:bin -->";
const BIN_CLOSE: &str = "<!-- /engines:bin -->";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())).flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// `s` without its marked bin line (the only place a checkout's `target/release/` may appear).
fn without_bin_line(s: &str) -> String {
    match (s.find(BIN_OPEN), s.find(BIN_CLOSE)) {
        (Some(a), Some(z)) if a < z => format!("{}{}", &s[..a], &s[z + BIN_CLOSE.len()..]),
        _ => s.to_string(),
    }
}

/// The commands a markdown text shows: every inline code span and fenced code line, split at
/// `&&`, `;` and `|` into single commands.
fn commands(md: &str) -> Vec<String> {
    let mut snippets = Vec::new();
    let mut fenced = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            snippets.push(line.to_string());
            continue;
        }
        let mut parts = line.split('`');
        parts.next();
        while let (Some(code), Some(_)) = (parts.next(), parts.next()) {
            snippets.push(code.to_string());
        }
    }
    snippets
        .iter()
        .flat_map(|s| s.split(|c| c == ';' || c == '|').flat_map(|x| x.split("&&")).map(|x| x.trim().to_string()).collect::<Vec<_>>())
        .filter(|c| !c.is_empty())
        .collect()
}

/// A command the bundled tools don't cover, or a question tool named — what a portable skill can't
/// say.
fn forbidden(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in commands(text) {
        let first = c.split_whitespace().next().unwrap_or("");
        if ["make", "cargo", "uv", "python", "python3", "pip", "npm"].contains(&first) {
            out.push(format!("calls `{c}`"));
        }
    }
    let lower = text.to_lowercase();
    for word in ["engines-render", "target/release", "askuserquestion", "askquestions", "ask_user", "ask-questions", "uv run", ".py "] {
        if lower.contains(word) {
            out.push(format!("names `{word}`"));
        }
    }
    out
}

#[test]
fn the_skills_call_only_the_bundled_commands_and_name_no_question_tool() {
    let mut bad = Vec::new();
    for s in SKILLS {
        let dir = repo().join("skills").join(s);
        let skill = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert_eq!((skill.matches(BIN_OPEN).count(), skill.matches(BIN_CLOSE).count()), (1, 1), "{s}: one marked bin line");
        let mut all = Vec::new();
        files(&dir, &mut all);
        for f in all.iter().filter(|f| f.extension().is_some_and(|x| x == "md" || x == "json5")) {
            let text = without_bin_line(&std::fs::read_to_string(f).unwrap());
            bad.extend(forbidden(&text).into_iter().map(|b| format!("{}: {b}", f.strip_prefix(repo()).unwrap().display())));
        }
    }
    assert!(bad.is_empty(), "the portable skills may only call the bundled engines-* commands:\n{}", bad.join("\n"));
    // The rule itself: what it catches, and the English it leaves alone.
    assert_eq!(forbidden("Run `make check DOC=x` then `engines-check x`").len(), 1);
    assert_eq!(forbidden("```sh\ncargo build --release\n```").len(), 1);
    assert_eq!(forbidden("`./target/release/engines-render d.json5 o.wav 3`").len(), 2);
    assert_eq!(forbidden("Ask with AskUserQuestion.").len(), 1);
    assert!(forbidden("Make it darker: `engines-part add <slug> halo` && `engines-song verify d`").is_empty());
}

#[test]
fn each_folder_is_named_for_its_skill() {
    for s in SKILLS {
        let skill = std::fs::read_to_string(repo().join("skills").join(s).join("SKILL.md")).unwrap();
        let front = skill.strip_prefix("---\n").and_then(|r| r.split("\n---\n").next()).unwrap_or_else(|| panic!("{s}: no frontmatter"));
        let name = front.lines().find_map(|l| l.strip_prefix("name:")).map(str::trim);
        assert_eq!(name, Some(s), "skills/{s}/SKILL.md's `name` must be its folder's name");
        // VS Code loads a skill only with a description of at most 1024 characters.
        let description: Vec<&str> = front.lines().skip_while(|l| !l.starts_with("description:")).skip(1).take_while(|l| l.starts_with("  ")).map(str::trim).collect();
        let len = description.join(" ").chars().count();
        assert!((1..=1024).contains(&len), "{s}: a folded `description: >-` of 1–1024 characters (VS Code's limit), not {len}");
    }
}

#[test]
fn every_skill_carries_the_shared_references_unchanged() {
    let shared = repo().join("skills/shared");
    let mut refs = Vec::new();
    files(&shared, &mut refs);
    assert!(refs.len() >= 2, "engine-facts.md and music-theory.md");
    for r in &refs {
        let name = r.file_name().unwrap();
        let canonical = std::fs::read(r).unwrap();
        for s in SKILLS {
            let copy = repo().join("skills").join(s).join("reference").join(name);
            let bytes = std::fs::read(&copy).unwrap_or_else(|e| panic!("{}: {e} — run `make skills`", copy.display()));
            assert!(bytes == canonical, "{} differs from skills/shared/{} — edit the shared copy, then run `make skills`", copy.display(), name.to_string_lossy());
        }
    }
    // ambient-loop-doc reads the shared copy rather than keeping its own.
    for name in ["engine-facts.md", "music-theory.md"] {
        assert!(!repo().join(".claude/skills/ambient-loop-doc/reference").join(name).exists(), "ambient-loop-doc keeps no copy of {name}");
    }
}

#[test]
fn the_skeleton_carries_the_default_control_and_checks_clean() {
    let skeleton_src = std::fs::read_to_string(repo().join("skills/engines-new/reference/skeleton.json5")).unwrap();
    let skeleton: serde_json::Value = json5::from_str(&skeleton_src).unwrap();
    let defaults: serde_json::Value = json5::from_str(&std::fs::read_to_string(repo().join("engines-defaults.json5")).unwrap()).unwrap();
    for (key, value) in defaults.as_object().unwrap().iter().filter(|(k, _)| !k.starts_with("x_")) {
        assert_eq!(skeleton.get(key), Some(value), "the skeleton's `{key}` must be engines-defaults.json5's");
    }
    let report = engines_authoring::check("skeleton.json5", &skeleton_src);
    assert!(report.ok && report.errors == 0, "{}", engines_authoring::render_human(&report, false));
}

#[test]
fn claude_code_reads_the_same_folders() {
    let claude = repo().join(".claude/skills");
    for s in SKILLS {
        let link = claude.join(s);
        let meta = std::fs::symlink_metadata(&link).unwrap_or_else(|e| panic!("{}: {e}", link.display()));
        assert!(meta.file_type().is_symlink(), ".claude/skills/{s} links to skills/{s}");
        let target = std::fs::read_link(&link).unwrap();
        assert!(target.is_relative(), ".claude/skills/{s} is a relative link (it works in any checkout): {}", target.display());
        assert_eq!(link.canonicalize().unwrap(), repo().join("skills").join(s), ".claude/skills/{s}");
    }
    let old: Vec<String> = std::fs::read_dir(&claude).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("piece-")).collect();
    assert!(old.is_empty(), "the piece-* skills are replaced by the engines-* ones: {old:?}");
}
