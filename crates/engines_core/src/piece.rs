//! A **piece** project on disk (ADR 0034): `pieces/<slug>/` — self-contained and gitignored.
//!
//! ```text
//! pieces/<slug>/
//!   <slug>.json5          the one loop doc (song added in place)
//!   brief.md              intent, voice sheet, song plan, changelog
//!   renders/              <date>_r<rev>.flac + .json sidecars, <date>_r<rev>_stems/
//!   .live/                written only by the tools: status.json, history/
//!     history/r00042-<hash>.json5 + index.jsonl   every revision the doc went through
//! ```
//!
//! Desktop file I/O, so it is feature-gated (`pieces`) like `wav`/`flac` — the portable core never
//! sees it. Revisions are content-addressed: snapshotting text identical to the latest snapshot
//! returns that revision instead of making a new one, so a render always names the exact doc it
//! came from and `engines-live` + `engines-song` agree on numbering.

use crate::hash::fnv1a_bytes;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The directory pieces live in, relative to the repo root (the tools' working directory).
pub const PIECES_DIR: &str = "pieces";

/// One piece project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub slug: String,
    pub root: PathBuf,
}

/// One entry of `.live/history/index.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub rev: u32,
    /// FNV-1a of the doc text, hex.
    pub hash: String,
    /// The snapshot's file name inside `history/`.
    pub file: String,
    /// Unix seconds when the snapshot was taken.
    pub at: u64,
    /// Which tool took it (`engines-live`, `engines-song`, …).
    pub source: String,
}

impl Piece {
    /// `pieces/<slug>` under `base` (the repo root) for a valid slug, whether or not the piece exists yet — for
    /// tools that write beside a doc still to be created (`engines-midi analyze`, ADR 0053).
    pub fn at(base: &Path, slug: &str) -> io::Result<Piece> {
        if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("bad piece slug {slug:?} (letters, digits, - and _)")));
        }
        Ok(Piece { slug: slug.to_string(), root: base.join(PIECES_DIR).join(slug) })
    }

    /// `pieces/<slug>` under `base` (the repo root); it must exist and hold `<slug>.json5`.
    pub fn open(base: &Path, slug: &str) -> io::Result<Piece> {
        let piece = Piece::at(base, slug)?;
        if !piece.doc_path().is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("no piece doc at {}", piece.doc_path().display())));
        }
        Ok(piece)
    }

    /// The piece a doc path belongs to: `…/pieces/<slug>/<slug>.json5`. `None` for any other doc.
    pub fn of_doc(doc: &Path) -> Option<Piece> {
        let dir = doc.parent()?;
        let slug = dir.file_name()?.to_str()?;
        let is_main_doc = doc.file_stem()?.to_str()? == slug && doc.extension().is_some_and(|e| e == "json5");
        let in_pieces = dir.parent()?.file_name()? == PIECES_DIR;
        (is_main_doc && in_pieces).then(|| Piece { slug: slug.to_string(), root: dir.to_path_buf() })
    }

    pub fn doc_path(&self) -> PathBuf {
        self.root.join(format!("{}.json5", self.slug))
    }
    pub fn renders_dir(&self) -> PathBuf {
        self.root.join("renders")
    }
    pub fn live_dir(&self) -> PathBuf {
        self.root.join(".live")
    }
    /// The piece's Ableton Live set and its routing map (ADR 0044).
    pub fn ableton_dir(&self) -> PathBuf {
        self.root.join("ableton")
    }
    pub fn history_dir(&self) -> PathBuf {
        self.live_dir().join("history")
    }

    /// Every recorded revision, oldest first (an absent index ⇒ none). Malformed lines are skipped.
    pub fn history(&self) -> io::Result<Vec<Revision>> {
        match std::fs::read_to_string(self.history_dir().join("index.jsonl")) {
            Ok(s) => Ok(s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The revision of `text`: the latest snapshot's if its content is identical, else a new
    /// snapshot (`r<NNNNN>-<hash8>.json5`) appended to the index. `at` is unix seconds.
    ///
    /// Several tools record revisions (`engines-live`, `engines-song`, a second player): reading the last
    /// number, writing the snapshot and appending it happen under an exclusive lock on
    /// `history/.lock`, so two of them can't both take the same number.
    pub fn snapshot(&self, text: &str, source: &str, at: u64) -> io::Result<Revision> {
        let hash = format!("{:016x}", fnv1a_bytes(text.as_bytes()));
        let dir = self.history_dir();
        std::fs::create_dir_all(&dir)?;
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".lock"))?;
        lock.lock()?; // released when `lock` drops (or the process ends)
        let history = self.history()?;
        if let Some(last) = history.last() {
            if last.hash == hash {
                return Ok(last.clone());
            }
        }
        let rev = history.last().map_or(1, |r| r.rev + 1);
        let file = format!("r{rev:05}-{}.json5", &hash[..8]);
        write_atomic(&dir.join(&file), text.as_bytes())?;
        let r = Revision { rev, hash, file, at, source: source.to_string() };
        let index = dir.join("index.jsonl");
        drop_torn_tail(&index)?;
        let line = format!("{}\n", serde_json::to_string(&r).map_err(io::Error::from)?);
        std::fs::OpenOptions::new().create(true).append(true).open(&index)?.write_all(line.as_bytes())?;
        Ok(r)
    }

    /// The revision [`Piece::snapshot`] would record for `text`, without writing anything (a dry
    /// run's prediction: the latest revision if identical, else the next number).
    pub fn peek_revision(&self, text: &str) -> io::Result<Revision> {
        let hash = format!("{:016x}", fnv1a_bytes(text.as_bytes()));
        let history = self.history()?;
        Ok(match history.last() {
            Some(last) if last.hash == hash => last.clone(),
            last => {
                let rev = last.map_or(1, |r| r.rev + 1);
                Revision { rev, file: format!("r{rev:05}-{}.json5", &hash[..8]), hash, at: 0, source: String::new() }
            }
        })
    }

    /// A fresh render path stem in `renders/`: `<date>_r<rev>` (then `_2`, `_3`, … if a file or
    /// dir with that stem + any of `exts` already exists). `exts` like `["flac", "json"]`, or
    /// `[""]` for a directory.
    pub fn render_stem(&self, date: &str, rev: u32, suffix: &str, exts: &[&str]) -> PathBuf {
        let dir = self.renders_dir();
        let base = format!("{date}_r{rev}{suffix}");
        let taken = |stem: &str| exts.iter().any(|e| dir.join(if e.is_empty() { stem.to_string() } else { format!("{stem}.{e}") }).exists());
        let mut stem = base.clone();
        let mut n = 2;
        while taken(&stem) {
            stem = format!("{base}_{n}");
            n += 1;
        }
        dir.join(stem)
    }
}

/// Cut a torn last record (an append that failed part-way, e.g. on a full disk) off the history
/// index, so the next record starts on its own line instead of being glued to the fragment.
fn drop_torn_tail(index: &Path) -> io::Result<()> {
    let bytes = match std::fs::read(index) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if bytes.last().is_none_or(|&b| b == b'\n') {
        return Ok(());
    }
    let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    std::fs::OpenOptions::new().write(true).open(index)?.set_len(keep as u64)
}

/// Write via a sibling temp file + rename, so a reader never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Unix seconds now.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `YYYY-MM-DD` of unix time `t` shifted by `utc_offset_s` (the local date when given the local
/// offset — see [`local_utc_offset`]).
pub fn date_string(t: u64, utc_offset_s: i64) -> String {
    let days = (t as i64 + utc_offset_s).div_euclid(86_400);
    // Civil-from-days (Howard Hinnant), proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The local UTC offset in seconds at unix time `t` (0 where it can't be determined).
pub fn local_utc_offset(t: u64) -> i64 {
    #[cfg(unix)]
    {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `localtime_r` writes only into `tm`; both pointers are valid for the call.
        if unsafe { !libc::localtime_r(&tt, &mut tm).is_null() } {
            return tm.tm_gmtoff as i64;
        }
    }
    let _ = t;
    0
}

/// Today's local date, `YYYY-MM-DD`.
pub fn local_date_today() -> String {
    let t = unix_now();
    date_string(t, local_utc_offset(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_piece(slug: &str) -> (PathBuf, Piece) {
        let base = std::env::temp_dir().join(format!("engines-piece-test-{}-{slug}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(PIECES_DIR).join(slug)).unwrap();
        std::fs::write(base.join(PIECES_DIR).join(slug).join(format!("{slug}.json5")), "{}").unwrap();
        let p = Piece::open(&base, slug).unwrap();
        (base, p)
    }

    #[test]
    fn at_validates_the_slug_without_needing_the_doc() {
        let p = Piece::at(Path::new("/x"), "new-piece_2").unwrap();
        assert_eq!(p.root, Path::new("/x/pieces/new-piece_2"));
        assert!(Piece::at(Path::new("/x"), "../evil").is_err());
        assert!(Piece::at(Path::new("/x"), "").is_err());
        assert!(Piece::open(Path::new("/nonexistent"), "new-piece").is_err());
    }

    #[test]
    fn a_piece_is_found_by_slug_or_by_its_doc_path() {
        let (base, p) = temp_piece("night");
        assert_eq!(Piece::of_doc(&p.doc_path()), Some(p.clone()));
        assert_eq!(Piece::of_doc(Path::new("lab/tests/fixtures/x.json5")), None);
        assert_eq!(Piece::of_doc(Path::new("pieces/night/other.json5")), None, "only the piece's own doc");
        assert_eq!(Piece::of_doc(Path::new("pieces/night/night.json")), None, "pieces are .json5 (ADR 0034 D1)");
        assert!(Piece::open(&base, "missing").is_err());
        assert!(Piece::open(&base, "../etc").is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn snapshots_are_content_addressed_and_numbered() {
        let (base, p) = temp_piece("rev");
        assert!(p.history().unwrap().is_empty());
        let a = p.snapshot("{ a: 1 }", "engines-live", 100).unwrap();
        assert_eq!(a.rev, 1);
        assert_eq!(p.peek_revision("{ a: 1 }").unwrap(), a);
        assert_eq!(p.peek_revision("{ a: 9 }").unwrap().rev, 2, "a dry run predicts the next revision");
        assert_eq!(p.history().unwrap().len(), 1, "…without writing it");
        assert_eq!(p.snapshot("{ a: 1 }", "engines-song", 200).unwrap(), a, "same text ⇒ same revision");
        let b = p.snapshot("{ a: 2 }", "engines-live", 300).unwrap();
        assert_eq!(b.rev, 2);
        assert_eq!(std::fs::read_to_string(p.history_dir().join(&b.file)).unwrap(), "{ a: 2 }");
        // Going back to earlier text is a new revision (history is a timeline, not a set).
        assert_eq!(p.snapshot("{ a: 1 }", "engines-live", 400).unwrap().rev, 3);
        assert_eq!(p.history().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_history_from_before_the_rename_still_reads_and_grows() {
        let (base, p) = temp_piece("legacy");
        std::fs::create_dir_all(p.history_dir()).unwrap();
        std::fs::write(p.history_dir().join("r00001-aaaaaaaa.json5"), "{ a: 1 }").unwrap();
        // As `sme-live` and `sme-song` recorded them before the rename to Engines.
        let index = "{\"rev\":1,\"hash\":\"aaaaaaaa\",\"file\":\"r00001-aaaaaaaa.json5\",\"at\":100,\"source\":\"sme-live\"}\n{\"rev\":2,\"hash\":\"bbbbbbbb\",\"file\":\"r00002-bbbbbbbb.json5\",\"at\":200,\"source\":\"sme-song\"}\n";
        std::fs::write(p.history_dir().join("index.jsonl"), index).unwrap();
        let sources: Vec<(u32, String)> = p.history().unwrap().into_iter().map(|r| (r.rev, r.source)).collect();
        assert_eq!(sources, [(1, "sme-live".to_string()), (2, "sme-song".to_string())]);
        assert_eq!(p.snapshot("{ a: 3 }", "engines-live", 300).unwrap().rev, 3, "the timeline carries on");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_torn_index_record_is_cut_before_the_next_one() {
        let (base, p) = temp_piece("torn");
        p.snapshot("{ a: 1 }", "engines-live", 1).unwrap();
        // An append that died part-way (a full disk): no newline, half a record.
        let index = p.history_dir().join("index.jsonl");
        let mut f = std::fs::OpenOptions::new().append(true).open(&index).unwrap();
        f.write_all(br#"{"rev":2,"hash":"0123"#).unwrap();
        drop(f);
        let b = p.snapshot("{ a: 2 }", "engines-live", 2).unwrap();
        assert_eq!(b.rev, 2, "the torn record never counted");
        let revs: Vec<u32> = p.history().unwrap().iter().map(|r| r.rev).collect();
        assert_eq!(revs, vec![1, 2], "the new record parses: it isn't glued to the fragment");
        let text = std::fs::read_to_string(&index).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn concurrent_snapshots_never_share_a_revision() {
        // Two tools (threads here — each opens its own lock) recording at once.
        let (base, p) = temp_piece("race");
        std::thread::scope(|s| {
            for t in 0..6 {
                let p = &p;
                s.spawn(move || {
                    for i in 0..15 {
                        p.snapshot(&format!("{{ t: {t}, i: {i} }}"), "test", 0).unwrap();
                    }
                });
            }
        });
        let revs: Vec<u32> = p.history().unwrap().iter().map(|r| r.rev).collect();
        assert_eq!(revs, (1..=90).collect::<Vec<u32>>(), "one number per revision, in order");
        for r in p.history().unwrap() {
            assert!(p.history_dir().join(&r.file).is_file(), "{} was written", r.file);
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn render_stems_never_collide() {
        let (base, p) = temp_piece("ren");
        std::fs::create_dir_all(p.renders_dir()).unwrap();
        let first = p.render_stem("2026-10-09", 4, "", &["flac", "json"]);
        assert_eq!(first, p.renders_dir().join("2026-10-09_r4"));
        std::fs::write(first.with_extension("flac"), b"x").unwrap();
        assert_eq!(p.render_stem("2026-10-09", 4, "", &["flac", "json"]), p.renders_dir().join("2026-10-09_r4_2"));
        std::fs::create_dir_all(p.renders_dir().join("2026-10-09_r4_stems")).unwrap();
        assert_eq!(p.render_stem("2026-10-09", 4, "_stems", &[""]), p.renders_dir().join("2026-10-09_r4_stems_2"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(date_string(0, 0), "1970-01-01");
        assert_eq!(date_string(951_782_400, 0), "2000-02-29");
        assert_eq!(date_string(1_791_590_400, 0), "2026-10-10");
        assert_eq!(date_string(1_791_590_400, -7 * 3600), "2026-10-09", "a US evening is still the 9th");
    }
}
