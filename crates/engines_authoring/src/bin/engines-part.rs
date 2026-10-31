//! `engines-part` — the parts library (ADR 0039): multi-section fragments merged into a loop doc.
//!
//! ```text
//! engines-part list                                     # every part, with what it adds
//! engines-part show <part>                              # its source (placeholders and all)
//! engines-part add <doc | piece slug> <part> [--id ID] [--bus BUS] [--target INSTRUMENT] [--dry-run] [--json]
//! engines-part add <doc | piece slug> --file <part.json5> [same options]
//! ```
//! `add` merges every section at once — comments kept, ids made unique, the doc's layout followed —
//! checks the result like the engine loads it, and writes the doc in one atomic save (so a playing
//! `engines-live` hears it as one revision). A part that would leave errors isn't added, and neither is
//! one into a doc that already has errors (fix those first: the file must stay playable). `--dry-run`
//! prints the lines it would add instead. `--file` adds a part written in the library's format (a
//! `part` header, then sections — Claude's patches, ADR 0050), named for the file's stem. Exit status:
//! 0 = added (or listed), 1 = refused (a part file that isn't a part included), 2 = a file or part
//! couldn't be found.

use clap::{Parser, Subcommand};
use engines_authoring::parts::{self, Options};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "engines-part", about = "Add a part (instrument + its modulators, routes, sends) to a loop doc in one step")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the parts.
    List,
    /// Print a part's source.
    Show { part: String },
    /// Merge a part into a doc (a path, or the slug of a piece in pieces/).
    Add {
        doc: String,
        /// A part of the library (`engines-part list`).
        #[arg(required_unless_present = "file")]
        part: Option<String>,
        /// A part file in the library's format instead (its stem is the default id).
        #[arg(long, conflicts_with = "part")]
        file: Option<PathBuf>,
        /// The part's id (default: its name, `_2`… when taken).
        #[arg(long)]
        id: Option<String>,
        /// The bus its sends go to (default: the doc's first that isn't an external_return).
        #[arg(long)]
        bus: Option<String>,
        /// The instrument a part like `breath` or `shimmer` acts on (default: the first that fits).
        #[arg(long)]
        target: Option<String>,
        /// Show what would be added; don't write.
        #[arg(long)]
        dry_run: bool,
        /// Machine-readable result (one JSON object).
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    match Args::parse().cmd {
        Cmd::List => {
            for p in parts::library() {
                let target = p.target.as_deref().map(|t| if t == "any" { "  [acts on an instrument]".to_string() } else { format!("  [acts on an instrument's {t}]") }).unwrap_or_default();
                println!("{:<9} {}{target}", p.name, p.summary);
            }
            ExitCode::SUCCESS
        }
        Cmd::Show { part } => match parts::get(&part) {
            Some(p) => {
                print!("{}", p.source);
                ExitCode::SUCCESS
            }
            None => unknown(&part),
        },
        Cmd::Add { doc, part, file, id, bus, target, dry_run, json } => {
            let p = match (&part, &file) {
                (Some(name), _) => match parts::get(name) {
                    Some(p) => p,
                    None => return unknown(name),
                },
                (None, Some(f)) => match parts::from_file(f) {
                    Ok(p) => p,
                    Err(e @ parts::FileError::Read(_)) => {
                        eprintln!("engines-part: {}: {e}", f.display());
                        return ExitCode::from(2);
                    }
                    Err(e) => {
                        if json {
                            println!("{}", serde_json::json!({ "ok": false, "error": format!("{}: {e}", f.display()) }));
                        } else {
                            eprintln!("engines-part: {}: {e}", f.display());
                        }
                        return ExitCode::from(1);
                    }
                },
                (None, None) => unreachable!("clap requires a part or --file"),
            };
            // How messages name it: the library name, or the file.
            let part = file.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| p.name.to_string());
            let path = resolve(&doc);
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("engines-part: {}: {e}", path.display());
                    return ExitCode::from(2);
                }
            };
            let added = match parts::add(&src, &p, &Options { id, bus, target }) {
                Ok(a) => a,
                Err(e) => {
                    if json {
                        println!("{}", serde_json::json!({ "ok": false, "error": e }));
                    } else {
                        eprintln!("engines-part: can't add `{part}` to {}: {e}", path.display());
                    }
                    return ExitCode::from(1);
                }
            };
            // `engines-part` writes the file: it never leaves one the player refuses, so a doc that
            // already has errors is fixed first (the editor's source actions are lenient — a
            // buffer isn't the file).
            if !added.existing_errors.is_empty() {
                let errors: Vec<String> = added.existing_errors.iter().map(|d| d.to_string()).collect();
                let msg = format!(
                    "{} already has {} error{} (the player refuses it) — fix {} first (`engines-check {}`)",
                    path.display(),
                    errors.len(),
                    if errors.len() == 1 { "" } else { "s" },
                    if errors.len() == 1 { "it" } else { "them" },
                    path.display()
                );
                if json {
                    println!("{}", serde_json::json!({ "ok": false, "error": msg, "existing_errors": errors }));
                } else {
                    eprintln!("engines-part: can't add `{part}`: {msg}:");
                    for e in &errors {
                        eprintln!("  {e}");
                    }
                }
                return ExitCode::from(1);
            }
            if !dry_run {
                if let Err(e) = engines_core::piece::write_atomic(&path, added.text.as_bytes()) {
                    eprintln!("engines-part: {}: {e}", path.display());
                    return ExitCode::from(2);
                }
            }
            if json {
                let warnings: Vec<String> = added.warnings.iter().map(|w| w.to_string()).collect();
                println!(
                    "{}",
                    serde_json::json!({ "ok": true, "written": !dry_run, "doc": path, "part": p.name, "file": file, "id": added.id, "bus": added.bus, "target": added.target, "added": added.added, "warnings": warnings })
                );
                return ExitCode::SUCCESS;
            }
            let verb = if dry_run { "would add" } else { "added" };
            let to = added.target.as_ref().map(|t| format!(" on `{t}`")).unwrap_or_default();
            let send = added.bus.as_ref().map(|b| format!(", sending to `{b}`")).unwrap_or_default();
            println!("{verb} `{part}` as `{}`{to}{send} — {}:", added.id, path.display());
            for a in &added.added {
                println!("  + {a}");
            }
            for w in &added.warnings {
                println!("  warning {w}");
            }
            if dry_run {
                println!();
                for e in parts::diff_edits(&src, &added.text) {
                    for l in e.text.lines() {
                        println!("+ {l}");
                    }
                }
            }
            ExitCode::SUCCESS
        }
    }
}

fn unknown(part: &str) -> ExitCode {
    let names: Vec<String> = parts::library().into_iter().map(|p| p.name.into_owned()).collect();
    eprintln!("engines-part: no part `{part}` (parts: {})", names.join(", "));
    ExitCode::from(2)
}

/// A doc path as given, or a piece's doc (`pieces/<slug>/<slug>.json5`) for a bare slug.
fn resolve(doc: &str) -> PathBuf {
    let p = Path::new(doc);
    if doc.ends_with(".json5") || doc.ends_with(".json") || doc.contains('/') {
        return p.to_path_buf();
    }
    engines_core::piece::Piece::open(Path::new(""), doc).map(|piece| piece.doc_path()).unwrap_or_else(|_| p.to_path_buf())
}
