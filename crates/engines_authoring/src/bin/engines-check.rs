//! `engines-check` — strictly check loop docs (ADR 0033) the way the engine will load them.
//!
//! ```text
//! engines-check <doc.json5>... [--json] [--parts]
//! engines-check --catalog [--json]
//! ```
//! Human output: a verdict per doc, one line per diagnostic (located by JSONPath, with a
//! did-you-mean where one fits), and the polymeter summary ("realigns every N loops").
//! `--json`: one JSON object per doc per line (`{path, ok, errors, warnings, diagnostics,
//! structure}`) for tools — the PostToolUse hook, the editor. Exit status: 0 = every doc loads
//! (warnings allowed), 1 = some doc has errors, 2 = a file couldn't be read.
//!
//! `--catalog` prints the engine's capability catalog instead (ADR 0050): synth types, params, unit
//! maps, harmony, modulators — `--json` for the catalog itself (`contracts/capability_catalog_v2.schema.json`).

use clap::Parser;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "engines-check", about = "Strictly check loop docs: syntax, unknown keys/params, enums, ranges, dangling routes, silent voices; plus the polymeter")]
struct Args {
    /// Loop docs to check (JSON5).
    #[arg(required_unless_present = "catalog")]
    docs: Vec<String>,
    /// Machine-readable output: one JSON object per doc per line (with --catalog: the catalog).
    #[arg(long)]
    json: bool,
    /// List every modulator's cycle under the structure summary.
    #[arg(long)]
    parts: bool,
    /// Print the engine's capability catalog (what each synth type reads, ranges, defaults, unit maps).
    #[arg(long, conflicts_with_all = ["docs", "parts"])]
    catalog: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.catalog {
        let c = engines_core::catalog::catalog();
        if args.json {
            println!("{}", serde_json::to_string_pretty(&c).expect("the catalog serializes"));
        } else {
            print!("{}", engines_authoring::render_catalog(&c));
        }
        return ExitCode::SUCCESS;
    }
    let mut status = ExitCode::SUCCESS;
    let mut refused = false;
    for path in &args.docs {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("engines-check: {path}: {e}");
                status = ExitCode::from(2);
                continue;
            }
        };
        let report = engines_authoring::check(path, &src);
        refused |= !report.ok;
        if args.json {
            println!("{}", serde_json::to_string(&report).expect("a report serializes"));
        } else {
            print!("{}", engines_authoring::render_human(&report, args.parts));
        }
    }
    if refused && status == ExitCode::SUCCESS {
        status = ExitCode::from(1);
    }
    status
}
