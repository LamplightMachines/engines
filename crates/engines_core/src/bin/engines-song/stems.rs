//! `engines-song --stems DIR` (ADR 0030): one loudness-normalized file per audio instrument + bus.
//!
//! One render, then a rescale: the session streams `2 + 2N`-wide frames (the exact master, then a
//! stereo pair per source — `SessionConfig::source_stems`); each source goes to a **float temp
//! WAV** (lossless, overs kept) while a BS.1770 meter measures it. After the render each stem's
//! static gain is decided (`loudness::normalization_gain`: the `--stem-lufs` target, peaks capped
//! at −1 dBFS) and a parallel post-pass rescales every temp into its final FLAC/WAV — through the
//! same writers/quantizer as a stereo render — then deletes it. `stems.json` records every gain;
//! `mix_fader_db` is the fader setting that rebuilds the engine's mix in a DAW.
//!
//! The normalization is an export-side gain on files — the scored master stays the unlimited,
//! unnormalized mix of ADR 0021.
//!
//! The shared half — names, temps, meters, gains, the rescale, temp-dir ownership — is
//! `engines_core::stems` (ADR 0040: the live player's stem previews use it too); this is the CLI's: the
//! plan and its estimates, the drain into the splitter, the FLAC/WAV writers, the manifest.
//!
//! A song's stems are split per block (ADR 0043): the post-pass writes each stem through
//! `blocks::SplitOut` into `<song>-<stem>-<NN>-<label>` files, with the one gain the whole-song
//! measurement decided — so a stem's block files, laid end to end, are its whole-song stem exactly.
//!
//! Nothing here deletes a file it can't prove is its own. The temps live in a fresh, run-owned
//! directory under the temp root (`engines-stems-<pid>`, created exclusively, with an owner marker), so
//! a `--stem-temp-dir` shared with other files is never touched; a killed run's directory is
//! reclaimed only once its pid is gone. A re-export into the same DIR removes only the stems the
//! previous `stems.json` recorded that this export didn't rewrite (now silent, removed from the doc,
//! or another container).

use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{fs, io};

use engines_core::engine::{source_layout, SourceKey};
use engines_core::song::fmt_hms;
use engines_core::stems::{
    assign_names, create_run_dir, db, decide, free_bytes, parallel_map, reclaim_stale_runs, remove_stale_stems, same_volume, Decision, StemSplitter, TempGuard, DEFAULT_LUFS, MANIFEST,
    OWNER_MARKER, PEAK_CEILING_DBFS, TEMP_SUFFIX,
};
use engines_core::wav::SampleFormat;

use super::blocks::{self, Blocks, Files, SplitOut, Written};
use super::{build_control, drain, human_bytes, tappable, open_loop_log, print_parts, report_tail, start_session, Args, Container, Fail, FrameSink, RenderPlan};
use engines_core::midi_log::{self, MidiLogBuilder};

/// One planned stem.
struct Stem {
    key: SourceKey,
    /// File stem (no extension): the instrument id, or `bus_<id>`.
    name: String,
    /// `subtractive`/`dual`/`modal` for instruments; the effect chain (e.g. `reverb`) for buses.
    what: String,
    /// An instrument authored `enabled: false` (an objectives override may still turn it on).
    authored_off: bool,
}

/// Everything decided before rendering: names, paths, the target, disk estimates.
pub(super) struct StemsPlan {
    stems: Vec<Stem>,
    pub(super) dir: PathBuf,
    temp_dir: PathBuf,
    container: Container,
    format: SampleFormat,
    pub(super) target_lufs: f64,
    temp_bytes: u64,
    final_bytes: u64,
    /// A song: every stem is written one file per block.
    blocks: Option<Blocks>,
    sr: f64,
}

impl StemsPlan {
    /// Plan the stems and print the table + disk estimates (also the `--dry-run` output).
    pub(super) fn new(args: &Args, plan: &RenderPlan, dir: &str, container: Container, format: SampleFormat) -> Result<Self, Fail> {
        let layout = source_layout(&plan.doc);
        if layout.is_empty() {
            return Err(Fail::Usage(format!("{} has no audio instruments or buses to export as stems", args.doc)));
        }
        let dir = PathBuf::from(dir);
        if dir.is_file() {
            return Err(Fail::Usage(format!("--stems {} is a file — pass a directory", dir.display())));
        }
        let temp_dir = args.stem_temp_dir.as_ref().map(PathBuf::from).unwrap_or_else(|| dir.join(".engines-stems-tmp"));
        let stems: Vec<Stem> = layout
            .iter()
            .zip(assign_names(&layout))
            .map(|(key, name)| {
                let (what, authored_off) = match key {
                    SourceKey::Inst(id) => {
                        let i = &plan.doc.instruments[id];
                        (i.synth_type().to_string(), !i.enabled)
                    }
                    SourceKey::Bus(id) => {
                        let fx: Vec<&str> = plan.doc.buses[id].effects.values().map(|e| e.kind.as_str()).collect();
                        (if fx.is_empty() { "dry return".into() } else { fx.join(" → ") }, false)
                    }
                };
                Stem { key: key.clone(), name, what, authored_off }
            })
            .collect();
        if let Some(b) = &plan.blocks {
            let names: Vec<&str> = stems.iter().map(|s| s.name.as_str()).collect();
            if let Some((file, a, b)) = blocks::clash(b, &names, container.ext()) {
                return Err(Fail::Usage(format!(
                    "stems {a} and {b} would both write {file} — rename one of those instruments/buses or block labels so the per-block names differ (ADR 0043)"
                )));
            }
        }
        let n = stems.len() as u64;
        let sp = StemsPlan {
            temp_bytes: n * plan.frames_planned * 2 * 4,
            final_bytes: n * plan.frames_planned * 2 * format.bytes() as u64,
            stems,
            dir,
            temp_dir,
            container,
            format,
            target_lufs: args.stem_lufs.unwrap_or(DEFAULT_LUFS),
            blocks: plan.blocks.clone(),
            sr: plan.sr,
        };
        sp.print();
        Ok(sp)
    }

    fn file_name(&self, s: &Stem) -> String {
        format!("{}.{}", s.name, self.container.ext())
    }

    /// The stem's files, each with its first frame: one, or a song's one per block.
    fn files(&self, s: &Stem) -> Files {
        match &self.blocks {
            Some(b) => b.files(&self.dir, &s.name, self.container.ext()),
            None => vec![(self.dir.join(self.file_name(s)), 0)],
        }
    }

    /// How the tables name a stem: its file, or for a song its name × the block files.
    fn shown(&self, s: &Stem) -> String {
        match &self.blocks {
            Some(b) => format!("{} ×{}", s.name, b.parts.len()),
            None => self.file_name(s),
        }
    }

    fn print(&self) {
        eprintln!(
            "stems: {} → {}/ ({} {:?}, each normalized to {} LUFS, peaks ≤ {PEAK_CEILING_DBFS} dBFS)",
            self.stems.len(),
            self.dir.display(),
            self.container.ext().to_uppercase(),
            self.format,
            self.target_lufs
        );
        for (i, s) in self.stems.iter().enumerate() {
            eprintln!(
                "  {:>2}  {:<20} {:<10} {}{}",
                i + 1,
                self.shown(s),
                s.key.kind(),
                s.what,
                if s.authored_off { "  (authored disabled — skipped if it never sounds)" } else { "" }
            );
        }
        if let Some(b) = &self.blocks {
            eprintln!("  each stem is written one file per block — laid end to end they are the whole stem, sample for sample:");
            print_parts(b, "<stem>", self.container.ext(), self.sr);
        }
        let free = free_bytes(&self.temp_dir).map_or(String::new(), |b| format!(" · {} free", human_bytes(b)));
        eprintln!(
            "temp: ≈ {} as float in {} (deleted after){free} · stems ≤ {} as PCM{}",
            human_bytes(self.temp_bytes),
            self.temp_dir.display(),
            human_bytes(self.final_bytes),
            if self.container == Container::Flac { ", smaller once compressed" } else { "" }
        );
    }

    /// Refuse a render whose temps can't fit; warn when temps + the finished stems might not.
    fn preflight(&self) -> Result<(), Fail> {
        let short = |need: u64, free: u64, at: &Path| {
            Fail::Runtime(format!(
                "not enough disk: the float temps need ≈ {} but {} has {} free — shorten the render (--max-loops / --duration), or put the temps on another volume with --stem-temp-dir",
                human_bytes(need),
                at.display(),
                human_bytes(free)
            ))
        };
        if let Some(free) = free_bytes(&self.temp_dir) {
            if self.temp_bytes > free {
                return Err(short(self.temp_bytes, free, &self.temp_dir));
            }
            if same_volume(&self.temp_dir, &self.dir) && self.temp_bytes + self.final_bytes > free {
                eprintln!(
                    "warning: temps (≈ {}) + finished stems (≤ {} as PCM) may exceed the {} free — each temp is deleted as its stem finishes",
                    human_bytes(self.temp_bytes),
                    human_bytes(self.final_bytes),
                    human_bytes(free)
                );
            }
        }
        if !same_volume(&self.temp_dir, &self.dir) {
            if let Some(free) = free_bytes(&self.dir) {
                if self.final_bytes > free {
                    eprintln!("warning: the finished stems (≤ {} as PCM) may exceed the {} free at {}", human_bytes(self.final_bytes), human_bytes(free), self.dir.display());
                }
            }
        }
        Ok(())
    }
}

/// The render-side sink: the shared splitter, behind the drain loop's trait.
struct StemsSink(StemSplitter);

impl FrameSink for StemsSink {
    fn write_frames(&mut self, frames: &[f32]) -> io::Result<()> {
        self.0.write_frames(frames)
    }
    fn checkpoint(&mut self) -> io::Result<()> {
        Ok(()) // temps are scratch: a killed run's are cleaned by the next one
    }
}

/// What the rescale pass measured on a finished stem.
struct Rescaled {
    out_lufs: Option<f64>,
    peak: f32,
    clipped: u64,
    frames: u64,
    bytes: u64,
    /// The files written (one, or a song's one per block).
    files: Vec<Written>,
}

/// Rescale a float temp into its final FLAC/WAV file(s) (the shared exact gain, one for the whole
/// stem), then delete the temp.
fn rescale(temp: &Path, files: &[(PathBuf, u64)], gain_db: f64, container: Container, format: SampleFormat) -> io::Result<Rescaled> {
    let (w, out_lufs) = engines_core::stems::rescale(temp, gain_db, |sr| SplitOut::create(files.to_vec(), container, sr, format), |w, s| w.write_interleaved(s))?;
    let files = w.finalize()?;
    fs::remove_file(temp)?;
    let t = blocks::totals(&files);
    Ok(Rescaled { out_lufs, peak: t.peak, clipped: t.clipped, frames: t.frames, bytes: t.bytes, files })
}

/// `--stems`: render once to float temps, decide the gains, rescale in parallel, write the manifest.
pub(super) fn render(args: &Args, plan: &RenderPlan, sp: StemsPlan) -> Result<MixStats, Fail> {
    // An interrupted run's temps would otherwise count against this one's disk preflight.
    let reclaimed = reclaim_stale_runs(&sp.temp_dir);
    if reclaimed > 0 {
        eprintln!("reclaimed {} of float temps left in {} by an interrupted run", human_bytes(reclaimed), sp.temp_dir.display());
    }
    sp.preflight()?;
    let (max_change, control) = build_control(args, &plan.doc)?;
    let io_fail = |p: &Path, e: io::Error| Fail::Runtime(format!("{}: {e}", p.display()));
    fs::create_dir_all(&sp.dir).map_err(|e| io_fail(&sp.dir, e))?;
    // The root is ours to remove (once empty) when this run created it, or it's the default
    // `DIR/.engines-stems-tmp` — never a pre-existing `--stem-temp-dir`.
    let made_root = !sp.temp_dir.exists() || args.stem_temp_dir.is_none();
    fs::create_dir_all(&sp.temp_dir).map_err(|e| io_fail(&sp.temp_dir, e))?;
    let run_dir = match create_run_dir(&sp.temp_dir) {
        Ok(d) => d,
        Err(e) => {
            if made_root {
                let _ = fs::remove_dir(&sp.temp_dir);
            }
            return Err(io_fail(&sp.temp_dir, e));
        }
    };
    let temps: Vec<PathBuf> = sp.stems.iter().map(|s| run_dir.join(format!("{}{TEMP_SUFFIX}", s.name))).collect();
    let _guard = TempGuard {
        files: temps.iter().cloned().chain([run_dir.join(OWNER_MARKER)]).collect(),
        dirs: std::iter::once(run_dir.clone()).chain(made_root.then(|| sp.temp_dir.clone())).collect(),
    };
    let mut loop_log = open_loop_log(args)?;

    // ── the render: one session, master + a pair per source ──────────────────────────────
    let session = start_session(args, plan, max_change, control, true)?;
    let keys: Vec<SourceKey> = sp.stems.iter().map(|s| s.key.clone()).collect();
    if session.source_layout() != Some(keys.as_slice()) {
        return Err(Fail::Runtime(format!("stem layout mismatch: planned {keys:?}, session {:?}", session.source_layout())));
    }
    let sr = plan.sr as u32;
    let mut sink = StemsSink(StemSplitter::new(&temps, session.channels() as usize, plan.sr));
    // The MIDI log (ADR 0048): every instrument that can be mirrored, whatever the Live set's switches
    // say — turning one on later needs no re-render.
    let mirrored = tappable(&plan.doc);
    let mut midi = (!mirrored.is_empty()).then(|| MidiLogBuilder::new(&plan.doc, sr, mirrored));
    let drained = drain(session, &mut sink, &mut loop_log, args.loop_log.as_deref(), &format!("stem temps in {}", run_dir.display()), plan.sr, plan.frames_planned, midi.as_mut())?;
    let render_s = drained.t0.elapsed().as_secs_f64();
    let sink = sink.0;
    let (frames, master_peak, master_clipped, max_err) = (sink.frames, sink.master_peak, sink.master_clipped, sink.max_err);
    let master_lufs = sink.master_meter.integrated();
    let measured = sink.finish().map_err(|e| Fail::Runtime(format!("finalizing stem temps: {e}")))?;

    // ── gains + the parallel rescale ─────────────────────────────────────────────────────
    let t_post = Instant::now();
    let jobs: Vec<(usize, PathBuf, Files, Decision)> = measured
        .iter()
        .enumerate()
        .filter_map(|(i, m)| m.temp.clone().map(|t| (i, t, sp.files(&sp.stems[i]), decide(m, sp.target_lufs))))
        .collect();
    let results = parallel_map(&jobs, |(_, temp, files, d)| rescale(temp, files, d.gain_db, sp.container, sp.format));
    if let Some((j, Err(e))) = jobs.iter().zip(&results).find(|(_, r)| r.is_err()) {
        for (_, _, files, _) in &jobs {
            for (out, _) in files {
                let _ = fs::remove_file(out);
            }
        }
        return Err(Fail::Runtime(format!("writing {}: {e}", sp.shown(&sp.stems[j.0]))));
    }
    let post_s = t_post.elapsed().as_secs_f64();

    // ── manifest + summary ───────────────────────────────────────────────────────────────
    let r = |x: f64| (x * 100.0).round() / 100.0;
    let opt = |x: Option<f64>| x.map(r);
    let mut written = Vec::new();
    eprintln!("stems: {} written to {}/ · post-pass {}", jobs.len(), sp.dir.display(), fmt_hms(post_s));
    for ((i, _, _, d), res) in jobs.iter().zip(&results) {
        let res = res.as_ref().unwrap();
        let (s, m) = (&sp.stems[*i], &measured[*i]);
        debug_assert_eq!(res.frames, frames, "every stem spans the whole render");
        eprintln!(
            "  {:<20} {:>6} LUFS {:>6.1} dBFS → {:>+6.1} dB → {:>6} LUFS {:>6.1} dBFS{}",
            sp.shown(s),
            d.raw_lufs.map_or("—".into(), |l| format!("{l:.1}")),
            db(m.peak),
            d.gain_db,
            res.out_lufs.map_or("—".into(), |l| format!("{l:.1}")),
            db(res.peak),
            match (d.normalized, d.limited_by) {
                (false, _) => "  (under the −70 LUFS gate — kept at unity)",
                (true, Some("peak")) => "  (peak-limited)",
                _ => "",
            }
        );
        let mut entry = serde_json::json!({
            "kind": s.key.kind(),
            "id": s.key.id(),
            "type": s.what,
            "normalized": d.normalized,
            "limited_by": d.limited_by,
            "raw_lufs": opt(d.raw_lufs),
            "raw_peak_dbfs": r(db(m.peak)),
            // Unrounded: a null test against the mix needs the exact gain (0.01 dB of rounding
            // leaves a ≈ −75 dBFS residual — inaudible, but not a proof).
            "gain_db": d.gain_db,
            "gain_linear": 10f64.powf(d.gain_db / 20.0),
            "out_lufs": opt(res.out_lufs),
            "out_peak_dbfs": r(db(res.peak)),
            "clipped": res.clipped,
            "mix_fader_db": -d.gain_db,
            "bytes": res.bytes,
        });
        match &sp.blocks {
            // A song: the stem's files in order, each where it starts (ADR 0043).
            Some(b) => {
                entry["files"] = res
                    .files
                    .iter()
                    .zip(&b.parts)
                    .map(|(w, part)| {
                        serde_json::json!({
                            "file": w.file_name(),
                            "block": part.idx,
                            "label": part.label,
                            "start_frame": w.start_frame,
                            "frames": w.frames,
                            "out_peak_dbfs": r(db(w.peak)),
                            "clipped": w.clipped,
                            "bytes": w.bytes,
                        })
                    })
                    .collect();
            }
            None => entry["file"] = res.files.first().map(Written::file_name).into(),
        }
        written.push(entry);
    }
    let skipped: Vec<_> = measured
        .iter()
        .zip(&sp.stems)
        .filter(|(m, _)| m.temp.is_none())
        .map(|(_, s)| {
            eprintln!("  {:<20} skipped — silent for the whole render", sp.shown(s));
            serde_json::json!({ "kind": s.key.kind(), "id": s.key.id(), "reason": "digital silence" })
        })
        .collect();
    let manifest = serde_json::json!({
        "tool": "engines-song --stems (ADR 0030)",
        "engine_version": engines_core::version(),
        "engine_build": engines_core::build_id(),
        "doc": args.doc,
        "seed": args.seed,
        "control": { "optimize": args.optimize, "orrery": args.orrery, "objectives": args.objectives },
        "sample_rate": sr,
        "frames": frames,
        "duration_s": frames as f64 / plan.sr,
        "container": sp.container.ext(),
        "format": format!("{:?}", sp.format).to_lowercase(),
        "normalization": {
            "method": "ITU-R BS.1770-4 integrated loudness; one static gain per stem",
            "target_lufs": sp.target_lufs,
            "peak_ceiling_dbfs": PEAK_CEILING_DBFS,
            "note": "set each DAW fader to the stem's mix_fader_db to rebuild the engine's mix",
        },
        "master": { "peak_dbfs": r(db(master_peak)), "clipped": master_clipped, "integrated_lufs": opt(master_lufs), "stem_sum_max_err": max_err },
        "stems": written,
        "skipped": skipped,
    });
    let mut manifest = manifest;
    if let Some(host) = engines_core::host_build() {
        manifest["host_build"] = host.into();
    }
    // The MIDI log beside the stems (ADR 0048), written before the manifest that names it.
    let had_midi = fs::read_to_string(sp.dir.join(MANIFEST)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).is_some_and(|m| m["midi"].is_string());
    let mpath_midi = sp.dir.join(midi_log::FILE);
    match midi.map(|b| b.finish(frames)) {
        Some(log) => {
            let tmp = sp.dir.join(format!("{}.tmp", midi_log::FILE));
            fs::write(&tmp, serde_json::to_string(&log).unwrap_or_default() + "\n")
                .and_then(|()| fs::rename(&tmp, &mpath_midi))
                .map_err(|e| io_fail(&mpath_midi, e))?;
            let notes: usize = log.instruments.values().map(|i| i.notes.len()).sum();
            eprintln!("  MIDI log: {} notes from {} instrument(s) → {}", notes, log.instruments.len(), mpath_midi.display());
            manifest["midi"] = midi_log::FILE.into();
        }
        // No instrument to mirror any more: the previous export's log would describe another doc.
        None if had_midi => {
            let _ = fs::remove_file(&mpath_midi);
        }
        None => {}
    }
    if let Some(b) = &sp.blocks {
        manifest["blocks"] = b
            .parts
            .iter()
            .map(|p| serde_json::json!({ "block": p.idx, "label": p.label, "tail": p.tail, "start_frame": p.start_frame, "start_s": p.start_frame as f64 / plan.sr, "frames": p.frames }))
            .collect();
        manifest["note"] = "a song's stems, one file per block: a stem's files laid end to end in order are its whole stem, bit for bit".into();
    }
    // Removed while the previous manifest still records them, so a crash here leaves them tracked.
    let fresh: Vec<String> = results.iter().flat_map(|r| r.as_ref().unwrap().files.iter().map(Written::file_name)).collect();
    let stale = remove_stale_stems(&sp.dir, &fresh);
    if !stale.is_empty() {
        eprintln!("  removed {} stale stem(s) of the previous export: {}", stale.len(), stale.join(", "));
    }
    let mpath = sp.dir.join(MANIFEST);
    fs::write(&mpath, serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n").map_err(|e| io_fail(&mpath, e))?;
    eprintln!(
        "done: {} stems + {} · {} · render {} (×{:.1} realtime) + post-pass {} · mix peak {:.1} dBFS · {} · {} clipped",
        jobs.len(),
        mpath.display(),
        fmt_hms(frames as f64 / plan.sr),
        fmt_hms(render_s),
        frames as f64 / plan.sr / render_s.max(1e-9),
        fmt_hms(post_s),
        db(master_peak),
        master_lufs.map_or("— LUFS".to_string(), |l| format!("{l:.1} LUFS")),
        master_clipped
    );
    if max_err > 1e-4 {
        eprintln!("warning: the stems differ from the mix by up to {max_err:.2e} — the split should be exact to float rounding");
    }
    report_tail(args, plan, &drained, frames);
    Ok(MixStats { frames, peak: master_peak, clipped: master_clipped, lufs: master_lufs })
}

/// The mix's measurements from a stems render — what a piece's sidecar records for any render
/// (ADR 0034 D3), the same fields as a mix render's.
pub(super) struct MixStats {
    pub frames: u64,
    pub peak: f32,
    pub clipped: u64,
    pub lufs: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use engines_core::loudness::LoudnessMeter;
    use engines_core::stems::{Measured, TempStem};
    use engines_core::wav::WavReader;

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("engines_stems_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rescale_hits_the_target_and_removes_the_temp() {
        let d = scratch_dir("rescale");
        let sr = 48_000u32;
        // A −30 dBFS 997 Hz stereo tone (−30 LUFS) and a quiet bed with one loud click.
        let tone: Vec<f32> = (0..sr as usize * 3).flat_map(|i| {
            let v = (10f64.powf(-30.0 / 20.0) * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / sr as f64).sin()) as f32;
            [v, v]
        })
        .collect();
        let mut clicky: Vec<f32> = tone.iter().map(|x| x * 0.1).collect();
        clicky[50_000] = 0.9;
        // The tone goes through the FLAC writer when it's built in (s24 WAV otherwise).
        let tone_container = if cfg!(feature = "flac") { Container::Flac } else { Container::Wav };
        for (name, sig, container, format) in [("tone", &tone, tone_container, SampleFormat::S24), ("clicky", &clicky, Container::Wav, SampleFormat::F32)] {
            let temp = d.join(format!("{name}{TEMP_SUFFIX}"));
            let mut st = TempStem::new(temp.clone(), sr as f64);
            st.push(sig, sr).unwrap();
            let m = Measured { temp: Some(temp.clone()), meter: st.meter.clone(), peak: st.peak };
            st.w.take().unwrap().finalize().unwrap();
            let dec = decide(&m, -18.0);
            let out = d.join(format!("{name}.{}", container.ext()));
            let res = rescale(&temp, &[(out.clone(), 0)], dec.gain_db, container, format).unwrap();
            assert!(!temp.exists(), "{name}: temp deleted");
            assert_eq!(res.frames, (sig.len() / 2) as u64);
            match name {
                "tone" => {
                    assert_eq!(dec.limited_by, Some("loudness"));
                    assert!((res.out_lufs.unwrap() + 18.0).abs() < 0.05, "{name}: {:?}", res.out_lufs);
                    // Decode the file (a FLAC independently, with claxon) and confirm the gain landed.
                    let peak = if container == Container::Flac {
                        let mut rd = claxon::FlacReader::open(&out).unwrap();
                        rd.samples().map(|s| s.unwrap().unsigned_abs()).max().unwrap() as f64 / 8_388_607.0
                    } else {
                        let mut b = vec![0.0f32; sig.len()];
                        let n = WavReader::open(&out).unwrap().read_interleaved(&mut b).unwrap();
                        b[..n].iter().fold(0.0f32, |p, x| p.max(x.abs())) as f64
                    };
                    assert!((20.0 * peak.log10() - (-30.0 + dec.gain_db)).abs() < 0.05, "peak {peak}");
                }
                _ => {
                    assert_eq!(dec.limited_by, Some("peak"));
                    assert!((db(res.peak) - PEAK_CEILING_DBFS).abs() < 0.01, "{name}: peak {}", db(res.peak));
                    assert!(res.out_lufs.unwrap() < -18.0);
                    let back = {
                        let mut rd = WavReader::open(&out).unwrap();
                        let mut b = vec![0.0f32; sig.len()];
                        let n = rd.read_interleaved(&mut b).unwrap();
                        b.truncate(n);
                        b
                    };
                    let g = 10f64.powf(dec.gain_db / 20.0);
                    assert!(back.iter().zip(sig.iter()).all(|(&y, &x)| y == (x as f64 * g) as f32), "f32 stem = temp × gain, exactly");
                }
            }
        }
        let _ = fs::remove_dir_all(&d);
    }
}
