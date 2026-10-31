//! Render housekeeping for a piece (ADR 0034): which renders `engines-clean` may delete.
//!
//! Disk is tight, renders are big. The rules are conservative:
//! - only **managed** renders are candidates — ones `engines-song` made, i.e. with a `.json` sidecar
//!   (`<stem>.json` beside `<stem>.flac|.wav` or a `<stem>/` stems dir). Anything else in
//!   `renders/` (older hand-made renders) is never touched;
//! - the newest `keep` managed renders stay;
//! - a sidecar with `"published": true` or `"keep": true` stays;
//! - a render with an Ableton `.asd` sibling stays — a set links it (CLAUDE.md: ask first). A stems
//!   dir counts as linked when Ableton wrote its `.asd` files *inside* it, next to the stems;
//! - a render the piece's own Live set (`ableton/*.als`, or one of its backups) plays stays, even
//!   before Live has written any `.asd` (ADR 0044: `engines-als` render mode lays stems in). This tool
//!   reads no Live set: `engines-als` lists what its sets play in `ableton/protected.json` ([`Protected`]),
//!   rewritten on every build. A piece with a Live set but no list yet keeps every render.

use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

/// One managed render: its audio (file or stems dir) + sidecar.
#[derive(Debug, Clone, PartialEq)]
pub struct Render {
    pub stem: String,
    /// The audio file or stems directory.
    pub audio: PathBuf,
    pub sidecar: PathBuf,
    pub rev: Option<u64>,
    pub rendered_at: u64,
    pub bytes: u64,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Delete,
    KeepRecent,
    KeepMarked,
    KeepAbleton,
    /// The piece's Live set (or a backup of it) plays it.
    KeepSet,
    /// The piece has a Live set but no `protected.json` yet: what it plays is unknown, so every
    /// render stays until `engines-als build` writes the list.
    KeepUnlisted,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Delete => "delete",
            Verdict::KeepRecent => "keep (recent)",
            Verdict::KeepMarked => "keep (published/keep)",
            Verdict::KeepAbleton => "keep (Ableton .asd)",
            Verdict::KeepSet => "keep (the Live set plays it)",
            Verdict::KeepUnlisted => "keep (a Live set, no protected.json yet)",
        }
    }
}

/// Plan a clean of `renders_dir`, keeping the newest `keep` managed renders. Returns the managed
/// renders newest first, each with its verdict, plus the names of unmanaged entries (left alone).
pub fn plan(renders_dir: &Path, keep: usize) -> io::Result<(Vec<Render>, Vec<String>)> {
    let mut names: Vec<String> = match std::fs::read_dir(renders_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(e),
    };
    names.sort();
    let mut renders = Vec::new();
    let mut claimed: Vec<String> = Vec::new();
    // A list that can't be read can't be checked: refuse rather than risk deleting what a set plays.
    let piece_dir = renders_dir.parent().unwrap_or(Path::new("."));
    let in_sets = match Protected::read(piece_dir).map_err(|e| io::Error::other(format!("{e} — not cleaning (a Live set might play a render; fix or remove it first)")))? {
        Some(list) => Some(list.renders.iter().map(|r| abs(&normalize(&piece_dir.join(r)))).collect::<Vec<_>>()),
        None if has_live_set(piece_dir)? => None,
        None => Some(Vec::new()),
    };
    for name in names.iter().filter(|n| n.ends_with(".json")) {
        let stem = name.trim_end_matches(".json").to_string();
        let Some(audio_name) = ["flac", "wav"].iter().map(|e| format!("{stem}.{e}")).chain([stem.clone()]).find(|n| names.contains(n)) else {
            continue; // a sidecar whose audio is gone — not ours to judge
        };
        let sidecar = renders_dir.join(name);
        // JSON5: a sidecar's control may hold `-Infinity` (the doc writer's, ADR 0054 P3).
        let side: Value = std::fs::read_to_string(&sidecar).ok().and_then(|s| json5::from_str(&s).ok()).unwrap_or(Value::Null);
        if side.get("piece").is_none() {
            continue; // not an engines-song sidecar
        }
        let audio = renders_dir.join(&audio_name);
        let played = in_sets.as_ref().is_some_and(|ps| ps.iter().any(|p| p.starts_with(abs(&audio))));
        let ableton = names.iter().any(|n| n.starts_with(&audio_name) && n.ends_with(".asd")) || asd_inside(&audio);
        let marked = side.get("published").and_then(Value::as_bool) == Some(true) || side.get("keep").and_then(Value::as_bool) == Some(true);
        claimed.push(name.clone());
        claimed.extend(names.iter().filter(|n| n.starts_with(&audio_name) && n.ends_with(".asd")).cloned());
        claimed.push(audio_name);
        renders.push(Render {
            stem,
            bytes: size_of(&audio),
            audio,
            sidecar,
            rev: side.get("rev").and_then(Value::as_u64),
            rendered_at: side.get("rendered_at").and_then(Value::as_u64).unwrap_or(0),
            verdict: if in_sets.is_none() {
                Verdict::KeepUnlisted
            } else if played {
                Verdict::KeepSet
            } else if ableton {
                Verdict::KeepAbleton
            } else if marked {
                Verdict::KeepMarked
            } else {
                Verdict::Delete
            },
        });
    }
    renders.sort_by(|a, b| b.rendered_at.cmp(&a.rendered_at).then_with(|| b.stem.cmp(&a.stem)));
    for r in renders.iter_mut().filter(|r| r.verdict == Verdict::Delete).take(keep) {
        r.verdict = Verdict::KeepRecent;
    }
    let unmanaged = names.into_iter().filter(|n| !claimed.contains(n)).collect();
    Ok((renders, unmanaged))
}

/// Delete one planned render (audio + sidecar).
pub fn delete(r: &Render) -> io::Result<()> {
    if r.audio.is_dir() {
        std::fs::remove_dir_all(&r.audio)?;
    } else {
        std::fs::remove_file(&r.audio)?;
    }
    std::fs::remove_file(&r.sidecar)
}

/// What a `--yes` run did: only the renders [`delete`] actually removed count as deleted and freed.
#[derive(Debug, Default)]
pub struct Outcome {
    pub deleted: usize,
    pub freed: u64,
    /// The renders that couldn't be deleted, with why.
    pub failed: Vec<(String, io::Error)>,
}

/// Delete every render planned for deletion, counting only what went.
pub fn delete_planned(renders: &[Render]) -> Outcome {
    let mut out = Outcome::default();
    for r in renders.iter().filter(|r| r.verdict == Verdict::Delete) {
        match delete(r) {
            Ok(()) => {
                out.deleted += 1;
                out.freed += r.bytes;
            }
            Err(e) => out.failed.push((r.stem.clone(), e)),
        }
    }
    out
}

fn abs(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `pieces/<slug>/ableton/protected.json`: the renders the piece's Live sets (and their backups)
/// play, relative to the piece's folder — written by `engines-als` on every build, read
/// here so render housekeeping needs no Live set code.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Protected {
    pub schema: String,
    /// Paths relative to the piece's folder (`renders/2026-10-06_r6_stems/a-00-intro.flac`): a file a
    /// set plays, inside a render's file or stems dir.
    pub renders: Vec<String>,
}

impl Protected {
    /// The list's format tag.
    pub const SCHEMA: &'static str = "engines-protected/1";

    /// Where a piece's list lives.
    pub fn path(piece_dir: &Path) -> PathBuf {
        piece_dir.join("ableton").join("protected.json")
    }

    /// The piece's list, `None` when there is none yet. An unreadable one is an error.
    pub fn read(piece_dir: &Path) -> Result<Option<Protected>, String> {
        let path = Protected::path(piece_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let p: Protected = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                if p.schema != Protected::SCHEMA {
                    return Err(format!("{}: unknown schema {:?} (want {})", path.display(), p.schema, Protected::SCHEMA));
                }
                Ok(Some(p))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Write the list for `renders` (sorted, without repeats) when it says something new. Returns
    /// whether it wrote.
    pub fn write(piece_dir: &Path, renders: impl IntoIterator<Item = String>) -> io::Result<bool> {
        let mut renders: Vec<String> = renders.into_iter().collect();
        renders.sort();
        renders.dedup();
        let list = Protected { schema: Protected::SCHEMA.into(), renders };
        if Protected::read(piece_dir).ok().flatten().as_ref() == Some(&list) {
            return Ok(false);
        }
        let path = Protected::path(piece_dir);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = serde_json::to_string_pretty(&list).map_err(io::Error::other)?;
        text.push('\n');
        engines_core::piece::write_atomic(&path, text.as_bytes())?;
        Ok(true)
    }
}

/// Whether the piece has a Live set (`ableton/*.als`, or one in its `Backup/`).
fn has_live_set(piece_dir: &Path) -> io::Result<bool> {
    let ableton = piece_dir.join("ableton");
    for dir in [ableton.clone(), ableton.join("Backup")] {
        match std::fs::read_dir(&dir) {
            Ok(rd) => {
                for e in rd {
                    if e?.path().extension().is_some_and(|x| x == "als") {
                        return Ok(true);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io::Error::other(format!("{}: {e}", dir.display()))),
        }
    }
    Ok(false)
}

/// `a/b/../c` ⇒ `a/c` (no filesystem access: the path may not exist).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// A stems dir that went into a DAW: Ableton writes `<stem>.flac.asd` beside each stem inside it.
/// One level is enough — `engines-song --stems` writes a flat dir. (A plain file reads as no `.asd`.)
fn asd_inside(audio: &Path) -> bool {
    std::fs::read_dir(audio)
        .map(|rd| rd.filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().ends_with(".asd")))
        .unwrap_or(false)
}

fn size_of(p: &Path) -> u64 {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).map(|rd| rd.filter_map(|e| e.ok()).map(|e| size_of(&e.path())).sum()).unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per call: the tests run in parallel in one process, so a shared one would be
    /// cleared under another test's feet.
    fn dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("engines-clean-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn render(d: &Path, stem: &str, at: u64, extra: &str) {
        std::fs::write(d.join(format!("{stem}.flac")), vec![0u8; 100]).unwrap();
        std::fs::write(d.join(format!("{stem}.json")), format!(r#"{{ "piece": "p", "rev": 1, "rendered_at": {at} {extra} }}"#)).unwrap();
    }

    #[test]
    fn keeps_recent_marked_and_ableton_linked_renders_and_ignores_unmanaged_files() {
        let d = dir();
        render(&d, "2026-10-01_r1", 1, "");
        render(&d, "2026-10-02_r2", 2, r#", "published": true"#);
        render(&d, "2026-10-03_r3", 3, "");
        std::fs::write(d.join("2026-10-03_r3.flac.asd"), b"x").unwrap();
        render(&d, "2026-10-04_r4", 4, "");
        render(&d, "2026-10-05_r5", 5, "");
        std::fs::create_dir_all(d.join("2026-10-06_r6_stems")).unwrap();
        std::fs::write(d.join("2026-10-06_r6_stems/pad.flac"), vec![0u8; 50]).unwrap();
        std::fs::write(d.join("2026-10-06_r6_stems.json"), r#"{ "piece": "p", "rev": 6, "rendered_at": 6 }"#).unwrap();
        // An older stems dir that went into Ableton: its `.asd` files sit *inside*, beside the stems.
        std::fs::create_dir_all(d.join("2026-10-00_r0_stems")).unwrap();
        std::fs::write(d.join("2026-10-00_r0_stems/pad.flac"), vec![0u8; 50]).unwrap();
        std::fs::write(d.join("2026-10-00_r0_stems/pad.flac.asd"), b"x").unwrap();
        std::fs::write(d.join("2026-10-00_r0_stems.json"), r#"{ "piece": "p", "rev": 0, "rendered_at": 0 }"#).unwrap();
        std::fs::write(d.join("old_hand_render.wav"), b"x").unwrap();

        let (renders, unmanaged) = plan(&d, 2).unwrap();
        let v = |stem: &str| renders.iter().find(|r| r.stem == stem).unwrap().verdict;
        assert_eq!(v("2026-10-06_r6_stems"), Verdict::KeepRecent);
        assert_eq!(v("2026-10-05_r5"), Verdict::KeepRecent);
        assert_eq!(v("2026-10-04_r4"), Verdict::Delete);
        assert_eq!(v("2026-10-03_r3"), Verdict::KeepAbleton);
        assert_eq!(v("2026-10-02_r2"), Verdict::KeepMarked);
        assert_eq!(v("2026-10-01_r1"), Verdict::Delete);
        assert_eq!(v("2026-10-00_r0_stems"), Verdict::KeepAbleton, "an .asd inside a stems dir links it");
        assert_eq!(unmanaged, vec!["old_hand_render.wav".to_string()]);
        assert_eq!(renders.iter().find(|r| r.stem == "2026-10-06_r6_stems").unwrap().bytes, 50);

        for r in renders.iter().filter(|r| r.verdict == Verdict::Delete) {
            delete(r).unwrap();
        }
        assert!(!d.join("2026-10-04_r4.flac").exists() && !d.join("2026-10-04_r4.json").exists());
        assert!(d.join("2026-10-03_r3.flac").exists() && d.join("old_hand_render.wav").exists());
        assert!(d.join("2026-10-00_r0_stems/pad.flac.asd").exists() && d.join("2026-10-00_r0_stems.json").exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_render_from_before_the_rename_is_still_managed() {
        let d = dir().join("legacy");
        std::fs::create_dir_all(&d).unwrap();
        render(&d, "2026-10-01_r1", 1, r#", "tool": "sme-song (ADR 0043: a song, one file per block)""#);
        render(&d, "2026-10-02_r2", 2, r#", "tool": "sme-song --stems (ADR 0030)""#);
        let (renders, unmanaged) = plan(&d, 1).unwrap();
        let v: Vec<(&str, Verdict)> = renders.iter().map(|r| (r.stem.as_str(), r.verdict)).collect();
        assert_eq!(v, [("2026-10-02_r2", Verdict::KeepRecent), ("2026-10-01_r1", Verdict::Delete)], "an sme-song sidecar is a render like any other");
        assert!(unmanaged.is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_render_the_live_set_plays_is_kept_even_without_asd_files() {
        let base = std::env::temp_dir().join(format!("engines-clean-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let d = base.join("renders");
        std::fs::create_dir_all(d.join("2026-10-06_r6_stems")).unwrap();
        std::fs::write(d.join("2026-10-06_r6_stems/a-00-intro.flac"), vec![0u8; 50]).unwrap();
        std::fs::write(d.join("2026-10-06_r6_stems.json"), r#"{ "piece": "p", "rev": 6, "rendered_at": 6 }"#).unwrap();
        render(&d, "2026-10-07_r7", 7, "");
        render(&d, "2026-10-08_r8", 8, "");
        // A piece with a Live set but no list yet (built before lists existed): what it plays is unknown, so
        // everything stays.
        std::fs::create_dir_all(base.join("ableton")).unwrap();
        std::fs::write(base.join("ableton/p.als"), b"a set").unwrap();
        let (renders, _) = plan(&d, 0).unwrap();
        assert!(renders.iter().all(|r| r.verdict == Verdict::KeepUnlisted), "{renders:?}");
        // engines-als lists what the set and a backup play: r6's stems and r7.
        assert!(Protected::write(&base, ["renders/2026-10-07_r7.flac".to_string(), "renders/2026-10-06_r6_stems/a-00-intro.flac".to_string()]).unwrap());
        assert!(!Protected::write(&base, ["renders/2026-10-06_r6_stems/a-00-intro.flac".to_string(), "renders/2026-10-07_r7.flac".to_string()]).unwrap(), "unchanged: not rewritten");
        let (renders, _) = plan(&d, 0).unwrap();
        let v = |stem: &str| renders.iter().find(|r| r.stem == stem).unwrap().verdict;
        assert_eq!(v("2026-10-06_r6_stems"), Verdict::KeepSet);
        assert_eq!(v("2026-10-07_r7"), Verdict::KeepSet);
        assert_eq!(v("2026-10-08_r8"), Verdict::Delete);

        // A list that can't be read: nothing is planned at all.
        std::fs::write(Protected::path(&base), b"not json").unwrap();
        let err = plan(&d, 0).unwrap_err();
        assert!(err.to_string().contains("not cleaning"), "{err}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_delete_that_fails_isnt_counted_as_freed() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("engines-clean-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        render(&d, "2026-10-01_r1", 1, "");
        render(&d, "2026-10-02_r2", 2, "");
        let (renders, _) = plan(&d, 0).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap(); // nothing can go
        if std::fs::write(d.join("probe"), "").is_ok() {
            // Root ignores the mode (a container running the tests as root): nothing can fail here.
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::fs::remove_dir_all(&d);
            eprintln!("skipped: permissions aren't enforced for this user");
            return;
        }
        let out = delete_planned(&renders);
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!((out.deleted, out.freed, out.failed.len()), (0, 0, 2), "nothing deleted, nothing freed");
        let out = delete_planned(&renders);
        assert_eq!((out.deleted, out.freed), (2, 200), "both go once they can (2 × 100 bytes of audio)");
        let _ = std::fs::remove_dir_all(d);
    }
}
