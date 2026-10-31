//! `engines-migrate` — schema v0.8 loop docs → v0.9 (ADR 0054 P9): time, frequency and gain in real units
//! with unit-suffixed keys, route depths in their dest's unit, comments kept.
//!
//! ```text
//! engines-migrate <doc | piece slug>... [--dry-run] [--json]   # a loop doc, a part, or an objectives file
//! engines-migrate --piece <slug>... [--dry-run] [--json]       # the doc, its .live/history and objectives
//! engines-migrate <file.objectives.json5> --doc <v0.8 doc>     # an objectives file, against its doc
//! ```
//! Each file is rewritten in one atomic save; a v0.9 file is left alone. `--piece` keeps every v0.8
//! original under `pieces/<slug>/.v0.8-backup/` and never touches `renders/` or `ableton/`.
//! `--dry-run` prints the line diff instead of writing. Exit status: 0 = migrated (or nothing to do),
//! 1 = a file refused (not v0.8, not JSON5), 2 = a file or piece couldn't be read or written.

use clap::Parser;
use engines_authoring::migrate::{self, FileResult};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "engines-migrate", about = "Migrate schema v0.8 loop docs to v0.9 (real units, comments kept)")]
struct Args {
    /// Loop docs, parts or objectives files (a path, or the slug of a piece in pieces/).
    #[arg(required = true)]
    docs: Vec<String>,
    /// Each argument is a piece: migrate its doc, its `.live/history` and its objectives files.
    #[arg(long)]
    piece: bool,
    /// The v0.8 loop doc an objectives file steers (what `decay`, `t60_scale` and depth bands mean).
    #[arg(long, conflicts_with = "piece")]
    doc: Option<PathBuf>,
    /// Print the diff; write nothing.
    #[arg(long)]
    dry_run: bool,
    /// Machine-readable result (one JSON object).
    #[arg(long)]
    json: bool,
    /// Write every converted number at full precision (the one-time null test of ADR 0054 P11;
    /// shipped docs use the per-unit decimals).
    #[arg(long, hide = true)]
    full_precision: bool,
}

fn main() -> ExitCode {
    let a = Args::parse();
    if a.full_precision {
        return migrate::full_precision(|| run(a));
    }
    run(a)
}

fn run(a: Args) -> ExitCode {
    let ctx = match a.doc.as_deref().map(std::fs::read_to_string).transpose() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("engines-migrate: --doc: {e}");
            return ExitCode::from(2);
        }
    };
    let mut results: Vec<FileResult> = Vec::new();
    let mut missing = false;
    for d in &a.docs {
        if a.piece {
            match engines_core::piece::Piece::open(Path::new(""), d) {
                Ok(p) => results.extend(migrate::migrate_piece(&p, a.dry_run)),
                Err(e) => {
                    eprintln!("engines-migrate: piece {d}: {e}");
                    missing = true;
                }
            }
        } else {
            results.push(migrate::migrate_file(&resolve(d), ctx.as_deref(), a.dry_run, None));
        }
    }
    let refused = results.iter().any(|r| r.refused);
    let io = missing || results.iter().any(|r| r.error.is_some() && !r.refused);
    if a.json {
        println!("{}", serde_json::json!({ "ok": !refused && !io, "dry_run": a.dry_run, "files": results }));
    } else {
        report(&results, a.dry_run);
    }
    if io {
        ExitCode::from(2)
    } else if refused {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn report(results: &[FileResult], dry_run: bool) {
    let (mut done, mut same) = (0, 0);
    for r in results {
        let path = r.path.display();
        let Some(m) = &r.migration else {
            eprintln!("engines-migrate: {path}: {}", r.error.as_deref().unwrap_or("not migrated"));
            continue;
        };
        if m.unchanged {
            same += 1;
            continue;
        }
        done += 1;
        let verb = if dry_run { "would migrate" } else { "migrated" };
        println!("{verb} {path}: {} change{}", m.changes.len(), if m.changes.len() == 1 { "" } else { "s" });
        for (k, n) in migrate::summary(m) {
            println!("  {k}{}", if n > 1 { format!(" ×{n}") } else { String::new() });
        }
        for n in &m.notes {
            println!("  note {n}");
        }
        if let Some(b) = &r.backup {
            println!("  v0.8 kept in {}", b.display());
        }
        if let Some(e) = &r.error {
            eprintln!("engines-migrate: {path}: {e}");
        }
        if dry_run {
            print!("{}", migrate::diff(&r.before, &m.text));
        }
    }
    if same > 0 {
        println!("{same} file{} already v0.9 (or nothing to migrate)", if same == 1 { "" } else { "s" });
    }
    if done == 0 && same == 0 && results.iter().all(|r| r.migration.is_none()) {
        eprintln!("engines-migrate: nothing migrated");
    }
}

/// A path as given, or a piece slug's doc (`pieces/<slug>/<slug>.json5`).
fn resolve(doc: &str) -> PathBuf {
    let p = Path::new(doc);
    if doc.ends_with(".json5") || doc.ends_with(".json") || doc.contains('/') {
        return p.to_path_buf();
    }
    engines_core::piece::Piece::open(Path::new(""), doc).map(|piece| piece.doc_path()).unwrap_or_else(|_| p.to_path_buf())
}
