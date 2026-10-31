//! `engines-song` — headless song renderer (ADR 0029): runs a streaming [`Session`] with no audio
//! device and writes the played audio straight to a streaming, RF64-capable WAV (or a lossless
//! FLAC) as fast as the worker produces it. A loop doc with a `song` plays its blocks in order (the
//! controller and the Orrery reset at each block, seams glide) and ends after the tail; a song-less
//! doc runs for `--max-loops` / `--duration`. Same worker path as `engines-play`/the TUI, so a render is
//! exactly what live playback would have played (bit-reproducible for a fixed doc + seed + control).
//!
//! `--stems DIR` (ADR 0030) writes one loudness-normalized file per audio instrument + bus instead
//! of the stereo mix — see `stems.rs`. A song is written one file per block (ADR 0043), the mix and
//! every stem alike, cut sample-exactly so the files laid end to end are the whole song — see
//! `blocks.rs`; `--whole` writes it as one continuous file instead.

mod blocks;
mod checks;
mod stems;

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use engines_core::contract::SessionControl;
use engines_core::controller::DitherConfig;
#[cfg(feature = "flac")]
use engines_core::flac::FlacWriter;
use engines_core::loop_doc::LoopDoc;
use engines_core::loudness::LoudnessMeter;
use engines_core::piece::{Piece, Revision};
use engines_core::midi_log::MidiLogBuilder;
use engines_core::session::{Session, SessionConfig, SessionOptions};
use engines_core::telemetry::LoopRecord;
use engines_core::song::{self, fmt_hms, SongPlan};
use engines_core::wav::{SampleFormat, WavWriter};

use blocks::{Blocks, SplitOut, Written, BLOCKS_MANIFEST};

/// Render a song (or a long session) headlessly to a WAV or FLAC file — or to per-source stems.
#[derive(Parser, Debug, PartialEq)]
// `args_override_self`: a repeated single-value flag keeps its LAST value instead of erroring,
// so `make song -- --doc <doc>` overrides the Makefile's default. Repeatable
// `Vec` flags (--ceiling/--floor) still accumulate.
// `engines-song verify | seams | voyage` are the render checks; without one it renders.
#[command(name = "engines-song", version, args_override_self = true, args_conflicts_with_subcommands = true, subcommand_negates_reqs = true, after_help = "\
NOTES:
    A song doc (top-level `song`, ADR 0029) renders its whole sequence + tail and stops. For a
    song-less doc pass --max-loops or --duration. Files past 4 GiB (≈ 3.1 h of f32 stereo @48k)
    are written as RF64 (ffmpeg/sox/Reaper/Audacity/afinfo read it); `--format s24` cuts the size
    by a quarter, `s16` halves it. An `--out` ending in .flac (or `--container flac`) writes
    lossless FLAC instead (24-bit by default, or --format s16; FLAC has no float, so f32 is
    refused) — it decodes bit-for-bit to the same-format WAV. The mix is NOT limited or normalized
    (ADR 0021) — the summary reports peak + clip count.

    A song (ADR 0043) is written ONE FILE PER BLOCK, plus one for the tail: `--out` names a
    directory (an .flac/.wav extension on it picks the container and is dropped: --out
    renders/night.flac ⇒ renders/night/), holding <song>-mix-<NN>-<label>.flac — NN is the
    block's number in the --dry-run plan, <song> the piece slug or the doc's file name — and a
    blocks.json listing each file's start. The cuts are sample-exact (the s16 dither carries
    across): laid end to end on one DAW track the files are the whole song, bit for bit. --whole
    writes one continuous file instead.

    --stems DIR (ADR 0030) writes one file per audio instrument (its dry signal) and one per bus
    (its wet return, e.g. an instrument's reverb) instead of the mix, each brought to --stem-lufs
    (default -18 LUFS, BS.1770) with one static gain, peaks capped at -1 dBFS. DIR/stems.json lists
    every gain; set each DAW fader to its `mix_fader_db` to rebuild the engine's mix. The render
    first writes float temps (≈ 1.4 GB per stem-hour, deleted after) — check the --dry-run estimate.
    A song's stems are split per block too (<song>-<stem>-<NN>-<label>.flac): the gain is measured
    over the whole song, so every block file of a stem carries the same one, and a block where a
    stem is silent still gets its (silent) file — line a stem's files up in order and nothing moves.

CHECKS (each renders in memory and reports; --json for one object; exit 0 pass, 1 FAIL, 2 error):
    engines-song verify <doc> [--ab] [--block K]   level, bands, centroid, onsets, harmonicity + pulse,
                                                   route audibility, a song's block starts
    engines-song seams <song doc> [--full]         the song scaled down with drift + the Orrery:
                                                   peak, clips, block levels, seams, the tail
    engines-song voyage <doc> [--movement default] the Orrery headless: chords, tension, tamed/roaming

EXAMPLES:
    # See the block plan, total length and file size without rendering:
    cargo run --release --bin engines-song -- --doc lab/tests/fixtures/song_lydian_tide.json5 --out night.wav --dry-run

    # Render the whole song (drift + Orrery), 24-bit:
    cargo run --release --bin engines-song -- --doc lab/tests/fixtures/song_lydian_tide.json5 \\
        --out night.wav --format s24 --optimize drift --orrery

    # The same as a 24-bit FLAC:
    cargo run --release --bin engines-song -- --doc lab/tests/fixtures/song_lydian_tide.json5 \\
        --out night.flac --orrery

    # Loudness-normalized FLAC stems (one per instrument + bus) for a DAW:
    cargo run --release --bin engines-song -- --doc lab/tests/fixtures/song_glass_garden.json5 \\
        --stems renders/garden --orrery

    # Render a piece's song into pieces/<slug>/renders/<date>_r<rev>/ — a FLAC per block
    # (<slug>-mix-00-settle.flac, …) — + a <date>_r<rev>.json sidecar:
    cargo run --release --bin engines-song -- --piece <slug> --orrery

    # The same song as one continuous file:
    cargo run --release --bin engines-song -- --doc lab/tests/fixtures/song_lydian_tide.json5 --out night.wav --whole

    # A 2-hour render of an ordinary loop doc:
    cargo run --release --bin engines-song -- --doc lab/tests/fixtures/loop_drone_d_lydian.json5 \\
        --out drone.wav --duration 2h --orrery")]
struct Args {
    /// A check instead of a render
    #[command(subcommand)]
    check: Option<checks::Check>,
    /// Loop document JSON/JSON5 (schema v0.9), with or without a `song`
    #[arg(long, required_unless_present = "piece", default_value = "")]
    doc: String,
    /// Render a piece (ADR 0034): `pieces/<SLUG>/<SLUG>.json5`. With no --out/--stems the render
    /// lands in the piece's `renders/` as `<date>_r<rev>.flac` + a `.json` sidecar (a doc path
    /// inside `pieces/` does the same)
    #[arg(long, value_name = "SLUG", conflicts_with = "doc")]
    piece: Option<String>,
    /// Output file: .wav (RF64 past 4 GiB) or .flac (lossless; s24/s16 only). For a song, the
    /// directory its per-block files go into (an audio extension picks the container and is dropped)
    #[arg(long, conflicts_with = "stems")]
    out: Option<String>,
    /// Stem export (ADR 0030): write one loudness-normalized file per audio instrument + bus into
    /// DIR instead of the stereo mix (for a piece, a bare `--stems` picks `renders/<date>_r<rev>_stems/`)
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "")]
    stems: Option<String>,
    /// File type: flac | wav. Stems and a song's per-block files default to flac (wav in a build
    /// without FLAC); with --out it follows the extension
    #[arg(long)]
    container: Option<Container>,
    /// Sample encoding: f32 (verbatim) | s24 | s16 (TPDF-dithered) [default: f32 for wav, s24 for flac]
    #[arg(long)]
    format: Option<SampleFormat>,
    /// Stems: integrated loudness each stem is normalized to, in LUFS (BS.1770) [default: -18]
    #[arg(long, requires = "stems", allow_negative_numbers = true, value_parser = parse_lufs, value_name = "LUFS")]
    stem_lufs: Option<f64>,
    /// Stems: directory for the float temp files [default: DIR/.engines-stems-tmp]
    #[arg(long, requires = "stems", value_name = "DIR")]
    stem_temp_dir: Option<String>,
    /// Render sample rate in Hz
    #[arg(long, default_value_t = 48_000.0)]
    sample_rate: f64,
    /// Timbre axis: drift (default) | hold
    #[arg(long, default_value = "")]
    optimize: String,
    /// Enable generative Orrery harmony (movement axis; combines with any --optimize)
    #[arg(long)]
    orrery: bool,
    /// Objectives doc (JSON5) to steer with — loaded once, overrides the flag-built control
    #[arg(long)]
    objectives: Option<String>,
    /// Per-param upper bound in the param's unit as the doc writes it (Hz, s, dB for gains), repeatable:
    /// --ceiling instruments.pad.params.cutoff_hz=2000
    #[arg(long = "ceiling", value_name = "PATH=V")]
    ceilings: Vec<String>,
    /// Per-param lower bound in the param's unit (dB for gains, `-inf` for silence), repeatable:
    /// --floor instruments.pad.params.level_db=-24
    #[arg(long = "floor", value_name = "PATH=V")]
    floors: Vec<String>,
    /// Session seed (block 0 uses it as-is; later blocks hash it with their index)
    #[arg(long, default_value_t = 0x5EED)]
    seed: u64,
    /// Also write one JSON line per rendered loop (loop id, drift move, Orrery transfer, song
    /// block/seam, peak); `engines-song voyage` tabulates the Orrery's moves without a render
    #[arg(long, value_name = "FILE")]
    loop_log: Option<String>,
    /// Max per-loop change δ (normalized 0..1); omitted ⇒ the doc's, then the built-in default
    #[arg(long)]
    max_change: Option<f32>,
    /// Stop after N loops (cuts a song short; required-or---duration for a song-less doc)
    #[arg(long)]
    max_loops: Option<usize>,
    /// Song-less docs: render this long, e.g. 5h30m, 90m, 3600s, 2.5h
    #[arg(long, value_parser = parse_duration)]
    duration: Option<f64>,
    /// Print the plan (blocks, timing, size) and exit without rendering
    #[arg(long)]
    dry_run: bool,
    /// Write each block's materialized doc (song stripped) into this directory — render/verify a
    /// block on its own with engines-render or engines-play
    #[arg(long, value_name = "DIR")]
    emit_blocks: Option<String>,
    /// A song: write ONE continuous file (or one file per stem) instead of a file per block
    #[arg(long)]
    whole: bool,
}

/// A failed run: a usage error (exit 2) or a runtime failure (exit 1). The message is printed
/// after `error: `.
#[derive(Debug, PartialEq)]
enum Fail {
    Usage(String),
    Runtime(String),
}

/// `5h30m` / `90m` / `45s` / `2.5h` / `3600` (bare = seconds) → seconds.
fn parse_duration(s: &str) -> Result<f64, String> {
    let s = s.trim();
    if let Ok(secs) = s.parse::<f64>() {
        return if secs > 0.0 { Ok(secs) } else { Err("duration must be > 0".into()) };
    }
    let (mut total, mut num) = (0.0f64, String::new());
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let unit = match c {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => return Err(format!("bad duration {s:?} (use e.g. 5h30m, 90m, 3600s)")),
        };
        let v: f64 = num.parse().map_err(|_| format!("bad duration {s:?}"))?;
        total += v * unit;
        num.clear();
    }
    if !num.is_empty() || total <= 0.0 {
        return Err(format!("bad duration {s:?} (use e.g. 5h30m, 90m, 3600s)"));
    }
    Ok(total)
}

/// A stem loudness target: finite, above the −70 LUFS absolute gate, below full scale.
fn parse_lufs(s: &str) -> Result<f64, String> {
    let v: f64 = s.trim().parse().map_err(|_| format!("bad LUFS value {s:?} (e.g. -18, -14, -23.5)"))?;
    if v.is_finite() && v > -70.0 && v < 0.0 {
        Ok(v)
    } else {
        Err(format!("LUFS target {v} out of range (must be between -70 and 0, e.g. -18)"))
    }
}

/// Append `records` to the loop log as JSONL (when one is open), counting them — through the doc
/// writer, so a value that is `-Infinity` reads back as one. Serialization and write errors
/// propagate — a full disk must fail the render, not leave a silently truncated log.
fn write_records<W: Write>(log: Option<&mut W>, records: Vec<LoopRecord>, n: &mut usize) -> io::Result<()> {
    let Some(w) = log else {
        *n += records.len();
        return Ok(());
    };
    for r in records {
        let line = engines_core::doc_value::to_string(&r).map_err(io::Error::other)?;
        w.write_all(line.as_bytes())?; // keeps an I/O error's kind
        w.write_all(b"\n")?;
        *n += 1;
    }
    Ok(())
}

/// Output file type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Container {
    Wav,
    Flac,
}

impl Container {
    fn ext(self) -> &'static str {
        match self {
            Container::Wav => "wav",
            Container::Flac => "flac",
        }
    }
}

/// The container a path's extension names, if it names one.
fn container_of_ext(path: &str) -> Option<Container> {
    let ext = Path::new(path).extension()?;
    if ext.eq_ignore_ascii_case("flac") {
        Some(Container::Flac)
    } else if ext.eq_ignore_ascii_case("wav") {
        Some(Container::Wav)
    } else {
        None
    }
}

/// The container of stems / a song's per-block files when `--container` doesn't name one: FLAC, or
/// WAV in a build without it.
const STEMS_DEFAULT: Container = if cfg!(feature = "flac") { Container::Flac } else { Container::Wav };

/// Container + sample encoding from `--out`/`--stems` + `--container` + `--format`. `--out`'s
/// extension decides unless it names none (then `--container`, else WAV); an explicit
/// `--container` that contradicts the extension is refused rather than silently renaming. Output
/// into a directory (`dir_output`: stems, or a song's per-block files) defaults to FLAC. FLAC is
/// integer-only: it defaults to s24 and refuses an explicit f32 rather than silently clamping the
/// overs f32 would have kept. A build without the `flac` feature refuses any FLAC request (and its
/// directory output defaults to WAV).
fn resolve_output(out: Option<&str>, dir_output: bool, container: Option<Container>, format: Option<SampleFormat>) -> Result<(Container, SampleFormat), String> {
    let container = match (out.and_then(container_of_ext), container) {
        (Some(by_ext), Some(c)) if by_ext != c => {
            return Err(format!("--container {} contradicts --out {} — drop one", c.ext(), out.unwrap_or_default()));
        }
        (Some(by_ext), _) => by_ext,
        (None, Some(c)) => c,
        (None, None) if dir_output => STEMS_DEFAULT,
        (None, None) => Container::Wav,
    };
    if container == Container::Flac && !cfg!(feature = "flac") {
        return Err("this engines-song was built without FLAC support — write wav, or rebuild with `--features flac` (on by default)".into());
    }
    match (container, format) {
        (Container::Flac, Some(SampleFormat::F32)) => Err("FLAC is integer PCM — use --format s24 (the flac default) or s16, or write wav for f32".into()),
        (Container::Flac, f) => Ok((Container::Flac, f.unwrap_or(SampleFormat::S24))),
        (Container::Wav, f) => Ok((Container::Wav, f.unwrap_or(SampleFormat::F32))),
    }
}

/// An open stereo output file — a streaming WAV or FLAC writer behind one interface.
enum AudioOut {
    Wav(WavWriter<File>),
    #[cfg(feature = "flac")]
    Flac(Box<FlacWriter<File>>), // boxed: the encoder state dwarfs the WAV writer
}

/// What the summary needs from either writer.
struct OutStats {
    frames: u64,
    peak: f32,
    clipped: u64,
    rf64: bool,
}

impl AudioOut {
    fn create(path: &Path, container: Container, sr: u32, fmt: SampleFormat) -> io::Result<Self> {
        Ok(match container {
            Container::Wav => AudioOut::Wav(WavWriter::create(path, 2, sr, fmt)?),
            #[cfg(feature = "flac")]
            Container::Flac => AudioOut::Flac(Box::new(FlacWriter::create(path, 2, sr, fmt)?)),
            // `resolve_output` already refused it; never reached.
            #[cfg(not(feature = "flac"))]
            Container::Flac => return Err(io::Error::new(io::ErrorKind::Unsupported, "built without FLAC support")),
        })
    }
    /// The file continues a stream at its interleaved sample `index` (the s16 dither carries on).
    fn continuing_at(self, index: u64) -> Self {
        match self {
            AudioOut::Wav(w) => AudioOut::Wav(w.continuing_at(index)),
            #[cfg(feature = "flac")]
            AudioOut::Flac(w) => AudioOut::Flac(Box::new(w.continuing_at(index))),
        }
    }
    fn write_interleaved(&mut self, s: &[f32]) -> io::Result<()> {
        match self {
            AudioOut::Wav(w) => w.write_interleaved(s),
            #[cfg(feature = "flac")]
            AudioOut::Flac(w) => w.write_interleaved(s),
        }
    }
    fn checkpoint(&mut self) -> io::Result<()> {
        match self {
            AudioOut::Wav(w) => w.checkpoint(),
            #[cfg(feature = "flac")]
            AudioOut::Flac(w) => w.checkpoint(),
        }
    }
    fn finalize(self) -> io::Result<OutStats> {
        Ok(match self {
            AudioOut::Wav(w) => {
                let (_, st) = w.finalize()?;
                OutStats { frames: st.frames, peak: st.peak, clipped: st.clipped, rf64: st.rf64 }
            }
            #[cfg(feature = "flac")]
            AudioOut::Flac(w) => {
                let (_, st) = w.finalize()?;
                OutStats { frames: st.frames, peak: st.peak, clipped: st.clipped, rf64: false }
            }
        })
    }
}

/// Where the drain loop delivers the session's frames: the stereo file, or the stem split.
trait FrameSink {
    /// Whole interleaved frames, `width` wide (channels 0/1 are always the master).
    fn write_frames(&mut self, frames: &[f32]) -> io::Result<()>;
    /// Periodic (~10 s) durability point — a killed render keeps a valid file.
    fn checkpoint(&mut self) -> io::Result<()>;
}

fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} kB", b / 1e3)
    }
}

fn print_plan(p: &SongPlan) {
    eprintln!(
        "song: {} blocks · {} loops (incl. {} tail) · {:.3} s/loop · {} · seam {} s",
        p.blocks.len(),
        p.total_loops,
        p.tail_loops,
        p.loop_s,
        fmt_hms(p.total_s),
        p.transition_s
    );
    eprintln!("  {:>2}  {:<14} {:<12} {:>6}  {:>8}  {:>8}  {:<18}  routes vs previous", "#", "label", "layer", "loops", "start", "length", "seed");
    for b in &p.blocks {
        let mut routes: Vec<String> = Vec::new();
        routes.extend(b.added.iter().map(|r| format!("+{r}")));
        routes.extend(b.removed.iter().map(|r| format!("−{r}")));
        routes.extend(b.changed.iter().map(|r| format!("~{r}")));
        eprintln!(
            "  {:>2}  {:<14} {:<12} {:>6}  {:>8}  {:>8}  {:<18}  {}",
            b.idx,
            b.label,
            b.matrix.as_deref().unwrap_or("—"),
            b.loops,
            fmt_hms(b.start_s),
            fmt_hms(b.dur_s),
            format!("{:#x}", b.seed),
            if routes.is_empty() { "·".into() } else { routes.join(" ") }
        );
        if let Some(c) = &b.control {
            eprintln!("  {:>2}  control: {}", "", c.summary());
        }
    }
    if p.tail_loops > 0 {
        let start = p.blocks.last().map(|b| b.start_s + b.dur_s).unwrap_or(0.0);
        eprintln!("  {:>2}  {:<14} {:<12} {:>6}  {:>8}  {:>8}", "", "tail", "(no notes)", p.tail_loops, fmt_hms(start), fmt_hms(p.tail_loops as f64 * p.loop_s));
    }
}

/// The render's shape, before anything starts.
struct RenderPlan {
    doc: LoopDoc,
    sr: f64,
    planned_loops: usize,
    song: Option<SongPlan>,
    /// A song written one file per block (ADR 0043): its cut points and names. `None` for a
    /// song-less doc or `--whole`.
    blocks: Option<Blocks>,
    /// Frames the run should produce (the summary warns when it falls short).
    frames_planned: u64,
}

fn load_doc(path: &str, text: &str) -> Result<LoopDoc, Fail> {
    let doc = LoopDoc::from_json_str(text).map_err(|e| Fail::Runtime(format!("{path}: {e}")))?;
    for w in doc.lint() {
        eprintln!("warning: {w}");
    }
    Ok(doc)
}

/// Song ⇒ its block table (and, unless `--whole`, its per-block files named after `song_name`);
/// song-less ⇒ `--max-loops` / `--duration`. Prints the plan.
fn plan_render(args: &Args, doc: LoopDoc, song_name: &str) -> Result<RenderPlan, Fail> {
    let sr = args.sample_rate;
    let loop_s = doc.total_frames(sr) as f64 / sr;
    let (planned_loops, song) = match &doc.song {
        Some(spec) => {
            let p = song::plan(&doc, spec, args.seed).map_err(|e| Fail::Runtime(format!("song: {e}")))?;
            (args.max_loops.map_or(p.total_loops, |m| m.min(p.total_loops)), Some(p))
        }
        None => match (args.max_loops, args.duration) {
            (Some(n), _) => (n, None),
            (None, Some(secs)) => ((secs / loop_s).ceil() as usize, None),
            (None, None) => {
                return Err(Fail::Usage(format!("{} has no `song` — pass --max-loops N or --duration 5h30m (or add a song, ADR 0029)", args.doc)));
            }
        },
    };
    if let Some(p) = &song {
        print_plan(p);
        if planned_loops < p.total_loops {
            eprintln!("note: --max-loops {planned_loops} cuts the song short");
        }
    } else {
        eprintln!("session: {planned_loops} loops · {loop_s:.3} s/loop · {}", fmt_hms(planned_loops as f64 * loop_s));
    }
    let frames_per_loop = doc.total_frames(sr) as u64;
    let frames_planned = planned_loops as u64 * frames_per_loop;
    let blocks = song.as_ref().filter(|_| !args.whole).map(|p| Blocks::new(song_name, p, frames_per_loop, planned_loops));
    Ok(RenderPlan { doc, sr, planned_loops, song, blocks, frames_planned })
}

/// The name a song's per-block files start with: the piece's slug, else the doc's file name.
fn song_name(args: &Args, piece: Option<&Piece>) -> String {
    match piece {
        Some(p) => p.slug.clone(),
        None => Path::new(&args.doc).file_stem().map_or_else(|| "song".into(), |s| s.to_string_lossy().into_owned()),
    }
}

/// The directory a song's per-block mix goes into: `--out` itself, minus an audio extension
/// (`renders/night.flac` ⇒ `renders/night`).
fn split_dir(out: &str) -> PathBuf {
    let p = Path::new(out);
    if container_of_ext(out).is_some() {
        p.with_extension("")
    } else {
        p.to_path_buf()
    }
}

/// The per-block file table (`part` is `mix`, or `<stem>` for the stems' pattern).
fn print_parts(b: &Blocks, part: &str, ext: &str, sr: f64) {
    for (i, p) in b.parts.iter().enumerate() {
        eprintln!("  {:<44} {:>8}  {:>8}", b.file_name(i, part, ext), fmt_hms(p.start_frame as f64 / sr), fmt_hms(p.frames as f64 / sr));
    }
}

/// `--emit-blocks DIR`: each block's materialized doc (song stripped), for per-block auditioning.
fn emit_blocks(doc: &LoopDoc, dir: &str) -> Result<(), Fail> {
    let Some(spec) = doc.song.clone() else {
        return Err(Fail::Usage("--emit-blocks needs a song doc".into()));
    };
    std::fs::create_dir_all(dir).map_err(|e| Fail::Runtime(format!("{dir}: {e}")))?;
    for i in 0..spec.sequence.len() {
        let block = song::materialize(doc, &spec, i).map_err(|e| Fail::Runtime(e.to_string()))?;
        let label = engines_core::stems::sanitize(&spec.label(i));
        let path = PathBuf::from(dir).join(format!("block_{i:02}_{label}.json"));
        engines_core::doc_value::to_string_pretty(&block)
            .map_err(|e| e.to_string())
            .and_then(|j| std::fs::write(&path, j + "\n").map_err(|e| e.to_string()))
            .map_err(|e| Fail::Runtime(format!("{}: {e}", path.display())))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}


/// The effective max per-loop change and the session control: an `--objectives` doc wins, else the
/// doc's own settings with CLI flags layered on top.
fn build_control(args: &Args, doc: &LoopDoc) -> Result<(f32, Option<SessionControl>), Fail> {
    let max_change = args.max_change.or(doc.max_change).unwrap_or(engines_core::controller::DEFAULT_MAX_CHANGE);
    let control = match &args.objectives {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| SessionControl::from_json_str(&s))
            .map(Some)
            .map_err(|e| Fail::Usage(format!("objectives {path}: {e}")))?,
        // The doc's own control settings (ADR 0033), with any CLI flags layered on top.
        None => SessionControl::with_flags(SessionControl::from_doc(doc), &args.optimize, args.orrery, &args.ceilings, &args.floors, max_change)
            .map_err(|e| Fail::Usage(format!("control: {e}")))?,
    };
    Ok((max_change, control))
}

fn open_loop_log(args: &Args) -> Result<Option<BufWriter<File>>, Fail> {
    args.loop_log
        .as_deref()
        .map(|p| File::create(p).map(BufWriter::new).map_err(|e| Fail::Runtime(format!("{p}: {e}"))))
        .transpose()
}

/// Start the headless session (`source_stems` ⇒ ADR 0030 master + per-source frames, and the MIDI
/// mirror of every instrument that can have one, with each loop's note log — ADR 0048).
fn start_session(args: &Args, plan: &RenderPlan, max_change: f32, control: Option<SessionControl>, source_stems: bool) -> Result<Session, Fail> {
    if plan.doc.instruments.values().any(|i| i.is_midi()) {
        eprintln!("note: `type:\"midi\"` instruments produce no audio here — their MIDI is discarded (use engines-play --midi)");
    }
    let max_loops = if plan.doc.song.is_some() { args.max_loops } else { Some(plan.planned_loops) };
    let opts = if source_stems { SessionOptions { note_tap: tappable(&plan.doc), note_log: true, ..Default::default() } } else { SessionOptions::default() };
    Session::start_with(
        plan.doc.clone(),
        SessionConfig {
            sample_rate: plan.sr,
            max_loops,
            source_stems,
            ..Default::default()
        },
        DitherConfig { max_change_per_loop: max_change, seed: args.seed, ..Default::default() },
        control,
        opts,
    )
    .map_err(Fail::Runtime)
}

/// The instruments a stems render logs as MIDI (ADR 0048): every one that can be mirrored.
fn tappable(doc: &LoopDoc) -> std::collections::BTreeSet<String> {
    doc.instruments.iter().filter(|(_, i)| i.can_tap_notes()).map(|(id, _)| id.clone()).collect()
}

/// What the drain loop observed, for the summary.
struct Drained {
    /// RMS of the master's last 0.5 s (the "ending cut off" check).
    tail_rms: f64,
    n_loops: usize,
    t0: Instant,
}

/// Pull the session's frames into `sink` until it finishes (whole frames only — `pull_audio`
/// pops samples, so a partial frame carries over), with the loop log, MIDI drain (into `midi`, the
/// stems' MIDI log, when given — ADR 0048), progress and checkpoints. Consumes the session (joins it
/// on return).
#[allow(clippy::too_many_arguments)]
fn drain(
    mut session: Session,
    sink: &mut dyn FrameSink,
    log: &mut Option<BufWriter<File>>,
    log_path: Option<&str>,
    out_label: &str,
    sr: f64,
    frames_planned: u64,
    mut midi: Option<&mut MidiLogBuilder>,
) -> Result<Drained, Fail> {
    let width = session.channels() as usize;
    let t0 = Instant::now();
    let mut buf = vec![0.0f32; (1 << 16) / width * width + width];
    let mut carry = 0usize;
    let tail_len = (sr * 0.5) as usize * 2;
    let mut tail: Vec<f32> = Vec::with_capacity(tail_len * 2);
    let mut frames_out: u64 = 0;
    let mut n_loops = 0usize;
    let (mut last_progress, mut last_checkpoint) = (Instant::now(), Instant::now());
    let log_err = |e: io::Error| Fail::Runtime(format!("writing loop log {}: {e}", log_path.unwrap_or("")));
    let mut io_err: Option<Fail> = None;
    loop {
        let n = session.pull_audio(&mut buf[carry..]);
        let total = carry + n;
        let whole = total / width * width;
        if whole > 0 {
            if let Err(e) = sink.write_frames(&buf[..whole]) {
                io_err = Some(Fail::Runtime(format!("writing {out_label}: {e}")));
                break;
            }
            frames_out += (whole / width) as u64;
            tail.extend(buf[..whole].chunks_exact(width).flat_map(|f| [f[0], f[1]]));
            if tail.len() > tail_len * 2 {
                tail.drain(..tail.len() - tail_len);
            }
        }
        buf.copy_within(whole..total, 0);
        carry = total - whole;
        let records = take_records(&mut session, midi.as_deref_mut());
        if let Err(e) = write_records(log.as_mut(), records, &mut n_loops) {
            io_err = Some(log_err(e));
            break;
        }
        let _ = session.poll_midi(); // unbounded channel — always drain it
        if last_progress.elapsed() >= Duration::from_secs(2) {
            last_progress = Instant::now();
            let audio_s = frames_out as f64 / sr;
            let rt = audio_s / t0.elapsed().as_secs_f64().max(1e-9);
            let pos = match session.poll_telemetry().and_then(|t| t.song) {
                Some(t) if t.tail => format!("tail {}/{}", t.loop_in_block + 1, t.block_loops),
                Some(t) => format!("block {}/{} {} · loop {}/{}{}", t.block + 1, t.blocks, t.label, t.loop_in_block + 1, t.block_loops, if t.seam { " ⇢seam" } else { "" }),
                None => String::new(),
            };
            eprintln!("  {} / {} · {pos} · ×{rt:.1} realtime", fmt_hms(audio_s), fmt_hms(frames_planned as f64 / sr));
        }
        if last_checkpoint.elapsed() >= Duration::from_secs(10) {
            last_checkpoint = Instant::now();
            if let Err(e) = sink.checkpoint() {
                io_err = Some(Fail::Runtime(format!("writing {out_label}: {e}")));
                break;
            }
            if let Some(Err(e)) = log.as_mut().map(|w| w.flush()) {
                io_err = Some(log_err(e));
                break;
            }
        }
        if n == 0 {
            if session.finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    if let Some(e) = io_err {
        session.stop();
        return Err(e);
    }
    if let Some(why) = session.worker_failure() {
        return Err(Fail::Runtime(format!("the render worker died: {why} — {out_label} is incomplete")));
    }
    let records = take_records(&mut session, midi.as_deref_mut());
    let tail_write = write_records(log.as_mut(), records, &mut n_loops);
    drop(session);
    tail_write.and_then(|()| log.take().map_or(Ok(()), |mut w| w.flush())).map_err(log_err)?;
    let tail_rms = (tail.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / tail.len().max(1) as f64).sqrt();
    Ok(Drained { tail_rms, n_loops, t0 })
}

/// The session's new loop records — folded into the MIDI log with the mirror's notes when there is
/// one, then stripped of their note logs (the `--loop-log` stays one slim line per loop).
fn take_records(session: &mut Session, midi: Option<&mut MidiLogBuilder>) -> Vec<LoopRecord> {
    let mut records = session.poll_records();
    let taps = session.poll_note_tap();
    if let Some(b) = midi {
        b.on_taps(&taps);
        b.on_records(&records);
    }
    for r in &mut records {
        r.note_log = None;
    }
    records
}

/// The end-of-run notes both modes share: the cut-off ending, the loop log, a short run.
fn report_tail(args: &Args, plan: &RenderPlan, d: &Drained, frames: u64) {
    // Only a song that ran to its end has a tail to blame (a --max-loops cut stops mid-sound by design).
    let ran_to_end = plan.song.as_ref().is_some_and(|p| plan.planned_loops >= p.total_loops);
    if ran_to_end && d.tail_rms > 1e-3 {
        eprintln!("warning: the last 0.5 s is still at {:.1} dBFS RMS — the ending is cut off; raise `song.tail_loops`", 20.0 * d.tail_rms.log10());
    }
    if let Some(p) = &args.loop_log {
        eprintln!("loop log: {} loops → {p}", d.n_loops);
    }
    if frames < plan.frames_planned {
        eprintln!("warning: rendered {} of {} planned frames", frames, plan.frames_planned);
    }
}

/// A render into a piece (ADR 0034): the doc's revision (snapshotted into `.live/history` once the
/// render is about to run) and where the sidecar goes.
struct PieceRender {
    piece: Piece,
    rev: Revision,
    /// `false` until [`record`](PieceRender::record) (and so always on a dry run): `rev` is then the
    /// revision the render *would* record.
    recorded: bool,
    /// The output path was derived from the revision (`renders/<date>_r<rev>…`), not given.
    auto_out: bool,
    auto_stems: bool,
    /// A song written one file per block: the derived mix output is a directory.
    split: bool,
}

/// `--piece SLUG` (or a doc inside `pieces/`) ⇒ the piece; fills in `--doc`.
fn piece_of(args: &mut Args) -> Result<Option<Piece>, Fail> {
    let piece = match &args.piece {
        Some(slug) => Some(Piece::open(Path::new(""), slug).map_err(|e| Fail::Usage(e.to_string()))?),
        None => Piece::of_doc(Path::new(&args.doc)),
    };
    match &piece {
        Some(p) => args.doc = p.doc_path().to_string_lossy().into_owned(),
        None if args.stems.as_deref() == Some("") => return Err(Fail::Usage("--stems needs a DIR (a bare --stems only works for a piece)".into())),
        None => {}
    }
    Ok(piece)
}

impl PieceRender {
    /// Predict the revision of `text` (nothing is written yet) and — with neither `--out` nor
    /// `--stems DIR` — derive a fresh `renders/<date>_r<rev>` path from it.
    fn plan(args: &mut Args, piece: Piece, text: &str, split: bool) -> Result<Self, Fail> {
        let rev = piece.peek_revision(text).map_err(|e| Fail::Runtime(format!("{}: {e}", piece.history_dir().display())))?;
        let auto_stems = args.stems.as_deref() == Some("");
        let auto_out = !auto_stems && args.out.is_none() && args.stems.is_none();
        let pr = PieceRender { piece, rev, recorded: false, auto_out, auto_stems, split };
        pr.fill_paths(args);
        Ok(pr)
    }

    fn fill_paths(&self, args: &mut Args) {
        let date = engines_core::piece::local_date_today();
        if self.auto_stems {
            args.stems = Some(self.piece.render_stem(&date, self.rev.rev, "_stems", &["", "json"]).to_string_lossy().into_owned());
        } else if self.auto_out && self.split {
            // A song's mix: a directory of per-block files (ADR 0043), never beside a same-named file.
            args.out = Some(self.piece.render_stem(&date, self.rev.rev, "", &["", "json", "flac", "wav"]).to_string_lossy().into_owned());
        } else if self.auto_out {
            let ext = args.container.unwrap_or(Container::Flac).ext();
            args.out = Some(self.piece.render_stem(&date, self.rev.rev, "", &[ext, "json"]).with_extension(ext).to_string_lossy().into_owned());
        }
    }

    /// Snapshot `text` — the exact text that was checked and is about to render — into the history
    /// (ADR 0034 D2). Called only once everything that can refuse the run has passed. If another
    /// tool recorded a revision since [`plan`](Self::plan), a derived output path follows the real
    /// number; returns whether it moved.
    fn record(&mut self, args: &mut Args, text: &str) -> Result<bool, Fail> {
        let history_err = |e: io::Error| Fail::Runtime(format!("{}: {e}", self.piece.history_dir().display()));
        let rev = self.piece.snapshot(text, "engines-song", engines_core::piece::unix_now()).map_err(history_err)?;
        let moved = rev.rev != self.rev.rev && (self.auto_out || self.auto_stems);
        self.rev = rev;
        self.recorded = true;
        if self.auto_out || self.auto_stems {
            std::fs::create_dir_all(self.piece.renders_dir()).map_err(|e| Fail::Runtime(format!("{}: {e}", self.piece.renders_dir().display())))?;
        }
        if moved {
            self.fill_paths(args);
        }
        Ok(moved)
    }

    fn announce(&self, args: &Args) {
        let to = args.out.as_ref().or(args.stems.as_ref()).map(|p| format!(" → {p}")).unwrap_or_default();
        eprintln!("piece: {} · r{}{}{to}", self.piece.slug, self.rev.rev, if self.recorded { "" } else { " (not recorded — dry run)" });
    }
}

/// The render's sidecar (`<out>.json` / `<stems dir>.json`): what the file is and exactly how to
/// re-create it (doc revision + snapshot, seed, control, flags).
#[allow(clippy::too_many_arguments)]
fn write_sidecar(pr: &PieceRender, args: &Args, kind: &str, output: &Path, control: Option<&SessionControl>, max_change: f32, sr: f64, stats: serde_json::Value) -> Result<(), Fail> {
    if !pr.recorded {
        return Ok(());
    }
    let sidecar = output.with_extension("json");
    let rel = |p: &Path| p.strip_prefix(&pr.piece.root).unwrap_or(p).to_string_lossy().into_owned();
    let mut v = serde_json::json!({
        "piece": pr.piece.slug,
        "rev": pr.rev.rev,
        "doc_hash": pr.rev.hash,
        "snapshot": rel(&pr.piece.history_dir().join(&pr.rev.file)),
        "kind": kind,
        "file": rel(output),
        "rendered_at": engines_core::piece::unix_now(),
        "sample_rate": sr,
        "seed": args.seed,
        "max_change": max_change,
        "argv": std::env::args().collect::<Vec<_>>(),
        "engine_version": engines_core::version(),
        "engine_build": engines_core::build_id(),
        "published": false,
    });
    if let Some(host) = engines_core::host_build() {
        v["host_build"] = host.into();
    }
    if let (Some(obj), serde_json::Value::Object(extra)) = (v.as_object_mut(), stats) {
        obj.extend(extra);
    }
    // The control holds doc values (a constraint floor may be `-Infinity`): through the doc writer.
    let mut v = engines_core::doc_value::DocValue::from(v);
    v["control"] = engines_core::doc_value::to_doc_value(&control).map_err(|e| Fail::Runtime(e.to_string()))?;
    let text = engines_core::doc_value::to_string_pretty(&v).map_err(|e| Fail::Runtime(e.to_string()))? + "\n";
    engines_core::piece::write_atomic(&sidecar, text.as_bytes()).map_err(|e| Fail::Runtime(format!("{}: {e}", sidecar.display())))?;
    eprintln!("sidecar: {}", sidecar.display());
    Ok(())
}

fn run(mut args: Args) -> Result<(), Fail> {
    let piece = piece_of(&mut args)?;
    // Read the doc once: this text is what gets checked, rendered and (for a piece) snapshotted.
    let text = std::fs::read_to_string(&args.doc).map_err(|e| Fail::Runtime(format!("{}: {e}", args.doc)))?;
    let doc = load_doc(&args.doc, &text)?;
    let split = doc.song.is_some() && !args.whole;
    let name = song_name(&args, piece.as_ref());
    let mut piece = piece.map(|p| PieceRender::plan(&mut args, p, &text, split)).transpose()?;
    let (container, format) = resolve_output(args.out.as_deref(), args.stems.is_some() || split, args.container, args.format).map_err(Fail::Usage)?;
    let plan = plan_render(&args, doc, &name)?;
    let (max_change, control) = build_control(&args, &plan.doc)?;
    if args.emit_blocks.is_some() && plan.doc.song.is_none() {
        return Err(Fail::Usage("--emit-blocks needs a song doc".into()));
    }
    if !args.dry_run && args.out.is_none() && args.stems.is_none() && args.emit_blocks.is_none() {
        return Err(Fail::Usage("--out <file.wav|file.flac> or --stems <dir> is required to render (or pass --dry-run)".into()));
    }
    let mut stems_plan = match &args.stems {
        Some(dir) => Some(stems::StemsPlan::new(&args, &plan, dir, container, format)?),
        None => None,
    };
    // Everything that can refuse the run has passed: only now does a piece record the revision.
    if let Some(pr) = piece.as_mut() {
        if !args.dry_run && pr.record(&mut args, &text)? {
            if let Some(dir) = &args.stems {
                stems_plan = Some(stems::StemsPlan::new(&args, &plan, dir, container, format)?);
            }
        }
        pr.announce(&args);
    }

    if stems_plan.is_none() {
        let est = plan.frames_planned * 2 * format.bytes() as u64;
        let split_to = match (&plan.blocks, &args.out) {
            (Some(b), Some(out)) => format!(" · a file per block → {}/ ({} files)", split_dir(out).display(), b.parts.len()),
            (Some(b), None) => format!(" · a file per block ({} files)", b.parts.len()),
            (None, _) => String::new(),
        };
        match container {
            Container::Wav => eprintln!(
                "output: {format:?} stereo @ {} Hz ≈ {}{}{split_to}",
                plan.sr as u32,
                human_bytes(est),
                if est > u32::MAX as u64 && plan.blocks.is_none() { " (RF64)" } else { "" }
            ),
            Container::Flac => eprintln!("output: {format:?} FLAC stereo @ {} Hz · {} as PCM, smaller once compressed{split_to}", plan.sr as u32, human_bytes(est)),
        }
        if let Some(b) = &plan.blocks {
            print_parts(b, "mix", container.ext(), plan.sr);
        }
    }
    if let Some(dir) = &args.emit_blocks {
        emit_blocks(&plan.doc, dir)?;
    }
    if args.dry_run {
        return Ok(());
    }
    if let Some(sp) = stems_plan {
        let dir = sp.dir.clone();
        let target = sp.target_lufs;
        let mix = stems::render(&args, &plan, sp)?;
        if let Some(pr) = &piece {
            let peak_db = 20.0 * (mix.peak as f64).log10();
            let stats = serde_json::json!({
                "container": container.ext(),
                "format": format!("{format:?}"),
                "duration_s": mix.frames as f64 / plan.sr,
                "frames": mix.frames,
                "peak_dbfs": if peak_db.is_finite() { Some(peak_db) } else { None },
                "clipped": mix.clipped,
                "integrated_lufs": mix.lufs,
                "stem_lufs": target,
                "manifest": "stems.json",
            });
            write_sidecar(pr, &args, "stems", &dir, control.as_ref(), max_change, plan.sr, stats)?;
        }
        return Ok(());
    }
    let Some(out_arg) = args.out.clone() else {
        return Ok(()); // --emit-blocks alone (checked above)
    };
    // A song: a directory of per-block files; else the one file.
    let (files, out_path) = match &plan.blocks {
        Some(b) => {
            let dir = split_dir(&out_arg);
            if dir.is_file() {
                return Err(Fail::Usage(format!("{} is a file — a song is written one file per block into a directory (pass a directory, or --whole for one file)", dir.display())));
            }
            std::fs::create_dir_all(&dir).map_err(|e| Fail::Runtime(format!("{}: {e}", dir.display())))?;
            (b.files(&dir, "mix", container.ext()), dir)
        }
        None => (vec![(PathBuf::from(&out_arg), 0)], PathBuf::from(&out_arg)),
    };
    let out_label = out_path.display().to_string();

    // Create the output before starting (a bad path fails now, not after the first hour).
    let out = SplitOut::create(files, container, plan.sr as u32, format).map_err(|e| Fail::Runtime(format!("{out_label}: {e}")))?;
    let mut loop_log = open_loop_log(&args)?;
    let session = start_session(&args, &plan, max_change, control.clone(), false)?;
    let mut metered = Metered { out, meter: LoudnessMeter::new(plan.sr, 2) };
    let drained = drain(session, &mut metered, &mut loop_log, args.loop_log.as_deref(), &out_label, plan.sr, plan.frames_planned, None)?;
    let lufs = metered.meter.integrated();
    let written = metered.out.finalize().map_err(|e| Fail::Runtime(format!("finalize {out_label}: {e}")))?;
    let stats = blocks::totals(&written);

    // ── summary ──────────────────────────────────────────────────────────────────
    let wall = drained.t0.elapsed().as_secs_f64();
    let audio_s = stats.frames as f64 / plan.sr;
    let peak_db = if stats.peak > 0.0 { 20.0 * stats.peak.log10() } else { f32::NEG_INFINITY };
    let size = stats.bytes;
    let pcm = stats.frames * 2 * format.bytes() as u64;
    eprintln!(
        "done: {out_label}{} · {} · {} · {format:?}{} · peak {peak_db:.1} dBFS · {} · {} clipped · {} wall (×{:.1} realtime)",
        if plan.blocks.is_some() { format!("/ ({} files)", written.len()) } else { String::new() },
        fmt_hms(audio_s),
        human_bytes(size),
        match container {
            Container::Wav if stats.rf64 => " RF64".to_string(),
            Container::Wav => String::new(),
            Container::Flac => format!(" FLAC ({:.0}% of PCM)", 100.0 * size as f64 / pcm.max(1) as f64),
        },
        lufs.map_or("— LUFS".to_string(), |l| format!("{l:.1} LUFS")),
        stats.clipped,
        fmt_hms(wall),
        audio_s / wall.max(1e-9),
    );
    if stats.clipped > 0 {
        eprintln!("warning: {} samples exceeded full scale — lower master_gain_db / per-voice level_db (no limiter, ADR 0021){}", stats.clipped, if format == SampleFormat::F32 { "; f32 kept the overs" } else { "; they were clamped" });
    }
    if let Some(b) = &plan.blocks {
        write_blocks_manifest(&args, &plan, b, &out_path, &written, container, format, lufs)?;
    }
    report_tail(&args, &plan, &drained, stats.frames);
    if let Some(pr) = &piece {
        let mut v = serde_json::json!({
            "container": container.ext(),
            "format": format!("{format:?}"),
            "duration_s": audio_s,
            "frames": stats.frames,
            "peak_dbfs": if peak_db.is_finite() { Some(peak_db) } else { None },
            "clipped": stats.clipped,
            "integrated_lufs": lufs,
            "bytes": size,
        });
        if plan.blocks.is_some() {
            v["manifest"] = BLOCKS_MANIFEST.into();
            v["files"] = written.len().into();
        }
        write_sidecar(pr, &args, "mix", &out_path, control.as_ref(), max_change, plan.sr, v)?;
    }
    Ok(())
}

/// A per-block mix's `blocks.json` (ADR 0043): every file, where it starts and what it measured —
/// placing each at its `start_s` (or simply end to end, in order) rebuilds the whole song. A
/// previous render into the same directory loses the files this one didn't rewrite.
#[allow(clippy::too_many_arguments)]
fn write_blocks_manifest(args: &Args, plan: &RenderPlan, b: &Blocks, dir: &Path, written: &[Written], container: Container, format: SampleFormat, lufs: Option<f64>) -> Result<(), Fail> {
    let r = |x: f64| (x * 100.0).round() / 100.0;
    let sr = plan.sr;
    let mut rows = Vec::new();
    for (w, part) in written.iter().zip(&b.parts) {
        eprintln!(
            "  {:<44} {:>8}  {:>8}  peak {:>6.1} dBFS{}",
            w.file_name(),
            fmt_hms(w.start_frame as f64 / sr),
            fmt_hms(w.frames as f64 / sr),
            engines_core::stems::db(w.peak),
            if w.clipped > 0 { format!(" · {} clipped", w.clipped) } else { String::new() }
        );
        rows.push(serde_json::json!({
            "file": w.file_name(),
            "block": part.idx,
            "label": part.label,
            "tail": part.tail,
            "start_frame": w.start_frame,
            "start_s": w.start_frame as f64 / sr,
            "frames": w.frames,
            "duration_s": w.frames as f64 / sr,
            "peak_dbfs": r(engines_core::stems::db(w.peak)),
            "clipped": w.clipped,
            "bytes": w.bytes,
        }));
    }
    let t = blocks::totals(written);
    let mut manifest = serde_json::json!({
        "tool": "engines-song (ADR 0043: a song, one file per block)",
        "engine_version": engines_core::version(),
        "engine_build": engines_core::build_id(),
        "doc": args.doc,
        "seed": args.seed,
        "control": { "optimize": args.optimize, "orrery": args.orrery, "objectives": args.objectives },
        "sample_rate": sr as u32,
        "frames": t.frames,
        "duration_s": t.frames as f64 / sr,
        "container": container.ext(),
        "format": format!("{format:?}").to_lowercase(),
        "note": "consecutive, sample-exact files: laid end to end in order they are the whole song, bit for bit",
        "master": { "peak_dbfs": r(engines_core::stems::db(t.peak)), "clipped": t.clipped, "integrated_lufs": lufs.map(r) },
        "blocks": rows,
    });
    if let Some(host) = engines_core::host_build() {
        manifest["host_build"] = host.into();
    }
    let fresh: Vec<String> = written.iter().map(Written::file_name).collect();
    let stale = engines_core::stems::remove_stale_files(dir, BLOCKS_MANIFEST, &fresh);
    if !stale.is_empty() {
        eprintln!("  removed {} stale file(s) of the previous render: {}", stale.len(), stale.join(", "));
    }
    let mpath = dir.join(BLOCKS_MANIFEST);
    std::fs::write(&mpath, serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n").map_err(|e| Fail::Runtime(format!("{}: {e}", mpath.display())))?;
    eprintln!("blocks: {}", mpath.display());
    Ok(())
}

/// The stereo sink with a BS.1770 meter on the master (the summary's / sidecar's LUFS).
struct Metered {
    out: SplitOut,
    meter: LoudnessMeter,
}

impl FrameSink for Metered {
    fn write_frames(&mut self, frames: &[f32]) -> io::Result<()> {
        self.meter.push_interleaved(frames);
        self.out.write_interleaved(frames)
    }
    fn checkpoint(&mut self) -> io::Result<()> {
        self.out.checkpoint()
    }
}

fn main() -> ExitCode {
    let mut args = Args::parse();
    if let Some(check) = args.check.take() {
        return checks::run(check);
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage(m)) => {
            eprintln!("error: {m}");
            ExitCode::from(2)
        }
        Err(Fail::Runtime(m)) => {
            eprintln!("error: {m}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_and_has_sane_defaults() {
        Args::command().debug_assert();
        let a = Args::parse_from(["engines-song", "--doc", "d.json5", "--out", "o.wav"]);
        assert_eq!((a.format, a.sample_rate, a.seed), (None, 48_000.0, 0x5EED));
        assert!(!a.dry_run && a.duration.is_none() && a.max_loops.is_none() && a.stems.is_none() && a.stem_lufs.is_none());
        let a = Args::parse_from(["engines-song", "--doc", "d", "--dry-run", "--format", "s24", "--duration", "5h30m"]);
        assert_eq!((a.format, a.duration), (Some(SampleFormat::S24), Some(19_800.0)));
        assert!(Args::try_parse_from(["engines-song", "--doc", "d", "--format", "mp3"]).is_err());
    }

    #[test]
    fn piece_flags_parse() {
        let a = Args::parse_from(["engines-song", "--piece", "night", "--orrery"]);
        assert_eq!((a.piece.as_deref(), a.doc.as_str()), (Some("night"), ""));
        let a = Args::parse_from(["engines-song", "--piece", "night", "--stems"]);
        assert_eq!(a.stems.as_deref(), Some(""), "a bare --stems = the piece's auto dir");
        assert!(Args::try_parse_from(["engines-song", "--orrery"]).is_err(), "--doc or --piece is required");
        assert!(Args::try_parse_from(["engines-song", "--piece", "a", "--doc", "b"]).is_err());
    }

    #[test]
    fn stem_flags_parse() {
        let a = Args::parse_from(["engines-song", "--doc", "d", "--stems", "renders/g", "--stem-lufs", "-14", "--container", "wav"]);
        assert_eq!((a.stems.as_deref(), a.stem_lufs, a.container), (Some("renders/g"), Some(-14.0), Some(Container::Wav)));
        let a = Args::parse_from(["engines-song", "--doc", "d", "--stems", "g", "--stem-lufs=-23.5", "--stem-temp-dir", "/Volumes/x/tmp"]);
        assert_eq!((a.stem_lufs, a.stem_temp_dir.as_deref()), (Some(-23.5), Some("/Volumes/x/tmp")));
        for bad in [
            vec!["engines-song", "--doc", "d", "--stem-lufs", "-14"],                           // needs --stems
            vec!["engines-song", "--doc", "d", "--stems", "g", "--stem-lufs", "3"],             // positive
            vec!["engines-song", "--doc", "d", "--stems", "g", "--stem-lufs", "-80"],           // under the gate
            vec!["engines-song", "--doc", "d", "--stems", "g", "--out", "o.wav"],              // one or the other
            vec!["engines-song", "--doc", "d", "--stems", "g", "--container", "mp3"],
        ] {
            assert!(Args::try_parse_from(&bad).is_err(), "{bad:?}");
        }
    }

    /// `make song -- --doc X --out Y` expands to the Makefile's own `--doc $(SONG) --out
    /// $(SONG_OUT)` FOLLOWED by the forwarded flags — the last occurrence must win, not error.
    #[test]
    fn forwarded_flags_override_makefile_defaults() {
        let a = Args::try_parse_from([
            "engines-song", "--doc", "default.json5", "--out", "renders/song.wav",
            "--doc", "mine.json5", "--out", "mine.wav", "--dry-run",
        ])
        .expect("a repeated --doc/--out must override, not error");
        assert_eq!((a.doc.as_str(), a.out.as_deref()), ("mine.json5", Some("mine.wav")));
        // Repeatable flags still accumulate rather than override.
        let a = Args::parse_from(["engines-song", "--doc", "d", "--floor", "a=0", "--floor", "b=1"]);
        assert_eq!(a.floors, ["a=0", "b=1"]);
    }

    #[test]
    fn output_container_and_format_resolve() {
        use SampleFormat::*;
        let r = |out: Option<&str>, stems: bool, c: Option<Container>, f: Option<SampleFormat>| resolve_output(out, stems, c, f);
        assert_eq!(r(Some("o.wav"), false, None, None), Ok((Container::Wav, F32)));
        assert_eq!(r(None, false, None, None), Ok((Container::Wav, F32)), "--dry-run without --out");
        assert_eq!(r(Some("o.wav"), false, None, Some(S16)), Ok((Container::Wav, S16)));
        assert_eq!(r(Some("flac"), false, None, None), Ok((Container::Wav, F32)), "extension, not substring");
        assert!(r(Some("o.wav"), false, Some(Container::Flac), None).unwrap_err().contains("contradicts"));
        assert_eq!(r(None, true, Some(Container::Wav), None), Ok((Container::Wav, F32)));
        assert_eq!(r(None, true, Some(Container::Wav), Some(S24)), Ok((Container::Wav, S24)));
        #[cfg(feature = "flac")]
        {
            assert_eq!(r(Some("renders/night.flac"), false, None, None), Ok((Container::Flac, S24)), "FLAC defaults to 24-bit");
            assert_eq!(r(Some("NIGHT.FLAC"), false, None, Some(S16)), Ok((Container::Flac, S16)));
            assert!(r(Some("o.flac"), false, None, Some(F32)).unwrap_err().contains("integer"), "no float FLAC");
            // --container: agrees with, or fills in for, --out's extension.
            assert_eq!(r(Some("o.flac"), false, Some(Container::Flac), None), Ok((Container::Flac, S24)));
            assert_eq!(r(Some("o.out"), false, Some(Container::Flac), None), Ok((Container::Flac, S24)), "no recognised extension");
            // Stems: a directory has no extension — FLAC unless --container says otherwise.
            assert_eq!(r(None, true, None, None), Ok((Container::Flac, S24)));
        }
        // A `--features cli` build (no `flac`) still renders WAV: it refuses FLAC, stems fall back to WAV.
        #[cfg(not(feature = "flac"))]
        {
            for (out, stems, c) in [(Some("o.flac"), false, None), (None, true, Some(Container::Flac)), (Some("o.out"), false, Some(Container::Flac))] {
                assert!(r(out, stems, c, None).unwrap_err().contains("without FLAC"), "{out:?} {c:?}");
            }
            assert_eq!(r(None, true, None, None), Ok((Container::Wav, F32)));
        }
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("5h30m"), Ok(19_800.0));
        assert_eq!(parse_duration("90m"), Ok(5_400.0));
        assert_eq!(parse_duration("2.5h"), Ok(9_000.0));
        assert_eq!(parse_duration("1h0m30s"), Ok(3_630.0));
        assert_eq!(parse_duration("3600"), Ok(3_600.0));
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("5h3").is_err());
        assert!(parse_duration("0").is_err());
    }

    #[test]
    fn loop_log_write_errors_propagate() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let rec = LoopRecord {
            loop_id: 1,
            revision: 0,
            optimize: "drift".into(),
            start_frame: 0,
            num_frames: 100,
            path: String::new(),
            delta: 0.0,
            harmonic_move: None,
            categorical_move: None,
            song: None,
            peak: 0.5,
            doc_rev: 0,
            orrery: None,
            note_log: None,
        };
        let mut n = 0;
        let err = write_records(Some(&mut Full), vec![rec.clone()], &mut n).unwrap_err();
        assert_eq!((err.kind(), n), (io::ErrorKind::StorageFull, 0), "a failed write is neither swallowed nor counted");
        let mut buf: Vec<u8> = Vec::new();
        write_records(Some(&mut buf), vec![rec.clone(), rec], &mut n).unwrap();
        assert_eq!((n, buf.iter().filter(|&&b| b == b'\n').count()), (2, 2));
        write_records(None::<&mut Vec<u8>>, vec![], &mut n).unwrap();
    }

    use clap::CommandFactory;
}
