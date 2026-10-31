//! `engines-clean` — delete old renders of a piece (ADR 0034). **Dry run by default.**
//!
//! ```text
//! engines-clean <slug> [--keep N] [--yes]
//! ```
//! Only renders `engines-song` made (with a `.json` sidecar) are candidates; the newest N (default 3),
//! any marked `"published": true` / `"keep": true` in its sidecar, and any with an Ableton `.asd`
//! sibling (or, for a stems dir, inside it) stay. Everything else in `renders/` is left alone.
//! `--yes` actually deletes.

use clap::Parser;
use engines_authoring::clean::{self, Verdict};
use engines_core::piece::Piece;
use std::path::Path;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "engines-clean", about = "Delete old renders of a piece (dry run unless --yes); keeps the newest N, published/keep-marked and Ableton-linked renders")]
struct Args {
    /// The piece: `pieces/<SLUG>/`.
    slug: String,
    /// How many of the newest renders to keep.
    #[arg(long, default_value_t = 3)]
    keep: usize,
    /// Really delete (otherwise only list what would go).
    #[arg(long)]
    yes: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let piece = match Piece::open(Path::new(""), &args.slug) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("engines-clean: {e}");
            return ExitCode::from(2);
        }
    };
    let (renders, unmanaged) = match clean::plan(&piece.renders_dir(), args.keep) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("engines-clean: {}: {e}", piece.renders_dir().display());
            return ExitCode::from(2);
        }
    };
    for r in &renders {
        let rev = r.rev.map(|n| format!("r{n}")).unwrap_or_default();
        println!("  {:<28} {:>5} {:>10}  {}", r.stem, rev, human(r.bytes), r.verdict.label());
    }
    for n in &unmanaged {
        println!("  {n:<28} {:>5} {:>10}  left alone (not made by engines-song)", "", "");
    }
    if renders.iter().any(|r| r.verdict == Verdict::KeepUnlisted) {
        println!("  (the piece has a Live set but no ableton/protected.json: run `engines-als build {}` once so its renders can be cleaned)", piece.slug);
    }
    let planned: Vec<_> = renders.iter().filter(|r| r.verdict == Verdict::Delete).collect();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    if !args.yes {
        let bytes = planned.iter().map(|r| r.bytes).sum();
        println!("{}: would delete {} render{} · {} (dry run — pass --yes to delete)", piece.slug, planned.len(), plural(planned.len()), human(bytes));
        return ExitCode::SUCCESS;
    }
    // Only what actually went counts: a failed delete frees nothing.
    let out = clean::delete_planned(&renders);
    for (stem, e) in &out.failed {
        eprintln!("engines-clean: {stem}: {e}");
    }
    let of = if out.failed.is_empty() { String::new() } else { format!(" of {}", planned.len()) };
    println!("{}: deleted {}{of} render{} · freed {}", piece.slug, out.deleted, plural(planned.len()), human(out.freed));
    if out.failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn human(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} kB", b / 1e3)
    }
}
