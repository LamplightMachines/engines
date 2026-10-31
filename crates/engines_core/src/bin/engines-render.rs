//! `engines-render` — headless render of a loop document to a WAV file.
//!
//! Dev/verification tool (the live Swift shell does streaming, CE-R12). Renders N loops
//! continuously through one `Engine` (tails/phase cross the seam) and concatenates.
//!
//! Usage: `engines-render <loop_doc.json> <out.wav> [loops] [sample_rate]`
//!    or: `engines-render --emit-template <loop|objectives> [out.json5]`  (write a commented JSON5 template)

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use engines_core::engine::{Engine, PhaseSnapshot};
use engines_core::loop_doc::LoopDoc;
use engines_core::wav::write_wav_f32;

/// Headless render of a loop document to WAV: N loops through ONE engine (tails/phase
/// cross the loop seam), concatenated. Or emit a commented JSON5 template (ADR 0023).
#[derive(Parser, Debug, PartialEq)]
#[command(name = "engines-render", version)]
struct Args {
    /// Loop document JSON/JSON5 (schema v0.9)
    #[arg(required_unless_present = "emit_template")]
    doc: Option<String>,
    /// Output WAV path
    #[arg(required_unless_present = "emit_template")]
    out: Option<String>,
    /// Number of control loops to render
    #[arg(default_value_t = 4)]
    loops: usize,
    /// Render sample rate in Hz
    #[arg(default_value_t = 48_000.0)]
    sample_rate: f64,
    /// Instead: write a commented JSON5 starter template — KIND is `loop` or `objectives`,
    /// OUT is the output path (stdout when omitted)
    #[arg(long, num_args = 1..=2, value_names = ["KIND", "OUT"], conflicts_with_all = ["doc", "out"])]
    emit_template: Option<Vec<String>>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Some(t) = &args.emit_template {
        return emit_template(t.first().map(String::as_str), t.get(1).map(String::as_str));
    }
    let doc_path = args.doc.as_deref().expect("clap requires doc unless --emit-template");
    let out_path = args.out.as_deref().expect("clap requires out unless --emit-template");
    let (loops, sr) = (args.loops, args.sample_rate);

    let src = match std::fs::read_to_string(doc_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {doc_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut doc = match LoopDoc::from_json_str(&src) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: parse {doc_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if doc.take_song().is_some() {
        eprintln!("note: {doc_path} has a `song` — engines-render plays only the top-level matrix (render the whole song with `engines-song`, or one block via `engines-song --emit-blocks`)");
    }

    // The output's folder may not exist yet (a fresh piece's `scratch/`): make it before rendering.
    if let Some(dir) = Path::new(out_path).parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("error: create {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    }

    let mut eng = Engine::new(sr);
    let mut snap = PhaseSnapshot::empty();
    let mut audio: Vec<f32> = Vec::new();
    for n in 0..loops {
        let r = eng.render(&doc, &snap);
        audio.extend_from_slice(&r.audio);
        snap = r.next_snapshot;
        eprintln!("  loop {} → {} frames", n + 1, r.num_frames);
    }

    if let Err(e) = write_wav_f32(Path::new(out_path), &audio, 2, sr as u32) {
        eprintln!("error: write {out_path}: {e}");
        return ExitCode::FAILURE;
    }
    let secs = audio.len() as f64 / 2.0 / sr;
    println!(
        "wrote {out_path}: {loops} loops, {:.1}s stereo @ {} Hz (engine {})",
        secs,
        sr as u32,
        engines_core::build_id()
    );
    ExitCode::SUCCESS
}

/// Emit a commented JSON5 template (ADR 0023) to `out` (or stdout when omitted).
///   `loop`       → the annotated loop-doc template (`LOOP_TEMPLATE_JSON5`).
///   `objectives` → the objectives template rendered at the default control.
fn emit_template(kind: Option<&str>, out: Option<&str>) -> ExitCode {
    let body = match kind {
        Some("loop") => engines_core::contract::LOOP_TEMPLATE_JSON5.to_string(),
        Some("objectives") => {
            engines_core::contract::objectives_template_json5(&engines_core::contract::SessionControl::default())
        }
        other => {
            eprintln!("usage: --emit-template <loop|objectives> [out.json5]  (got {other:?})");
            return ExitCode::from(2);
        }
    };
    match out {
        Some(path) => match std::fs::write(path, &body) {
            Ok(()) => {
                println!("wrote {path}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: write {path}: {e}");
                ExitCode::FAILURE
            }
        },
        None => {
            print!("{body}");
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_is_wellformed_and_parses_the_frozen_contracts() {
        use clap::CommandFactory;
        Args::command().debug_assert();
        // lab/engines_lab/cli.py::render + the Makefile `render` target — frozen contract.
        let a = Args::parse_from(["engines-render", "doc.json", "out.wav", "4", "48000"]);
        assert_eq!(a.doc.as_deref(), Some("doc.json"));
        assert_eq!(a.out.as_deref(), Some("out.wav"));
        assert_eq!((a.loops, a.sample_rate), (4, 48_000.0));
        // Defaults when the optional positionals are omitted.
        let a = Args::parse_from(["engines-render", "doc.json", "out.wav"]);
        assert_eq!((a.loops, a.sample_rate), (4, 48_000.0));
        // The Makefile `templates` target — `--emit-template <kind> <out>`.
        let a = Args::parse_from(["engines-render", "--emit-template", "loop", "loop_doc.template.json5"]);
        assert_eq!(a.emit_template, Some(vec!["loop".into(), "loop_doc.template.json5".into()]));
        assert!(a.doc.is_none());
    }
}
