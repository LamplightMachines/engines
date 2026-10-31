//! Loudness-normalized stem export, the shared half (ADR 0030; moved out of `engines-song` by ADR 0040
//! so the live player's stem previews use the same code): the file names, the float temps and
//! their meters, the per-stem gain decision, the exact static-gain rescale, and the temp-dir
//! ownership rules. `engines-song --stems` drives it for a song; `engines-live`'s `preview {stems: true}`
//! for a few seconds of the playing doc.
//!
//! A render streams `2 + 2N`-wide frames (the exact master, then a stereo pair per source —
//! `SessionConfig::source_stems`); [`StemSplitter`] sends each source to a **float temp WAV**
//! (lossless, overs kept) while a BS.1770 meter measures it. Afterwards [`decide`] picks each stem's
//! static gain (`loudness::normalization_gain`: the target, peaks capped at −1 dBFS) and [`rescale`]
//! applies it, exactly, into the caller's writer.
//!
//! Nothing here deletes a file it can't prove is its own: temps live in a fresh, run-owned directory
//! ([`create_run_dir`]), a killed run's are reclaimed only once its pid is gone
//! ([`reclaim_stale_runs`]), and a re-export removes only the stems the previous manifest recorded
//! ([`remove_stale_stems`]).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::{fs, io};

use crate::engine::SourceKey;
use crate::loudness::{normalization_gain, LoudnessMeter};
use crate::wav::{SampleFormat, WavReader, WavWriter};

/// Sample-peak ceiling every normalized stem stays under (dBFS).
pub const PEAK_CEILING_DBFS: f64 = -1.0;
/// Default per-stem loudness target (LUFS).
pub const DEFAULT_LUFS: f64 = -18.0;
/// Temp files are `<run dir>/<name>.f32.wav`.
pub const TEMP_SUFFIX: &str = ".f32.wav";
/// A run's temp dir is `<temp root>/engines-stems-<pid>[.n]`, holding [`OWNER_MARKER`] (the pid).
pub const RUN_DIR_PREFIX: &str = "engines-stems-";
pub const OWNER_MARKER: &str = ".engines-stems-owner";
/// The manifest a stems directory carries.
pub const MANIFEST: &str = "stems.json";

/// A file-name-safe version of an id: ASCII letters, digits, `-` and `_`; anything else is `_`.
pub fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// A `.wav` / `.flac` file name (what a stems directory holds).
fn is_audio_file(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("flac"))
}

/// File names for a layout: instruments by id, buses as `bus_<id>`, sanitized; a name that
/// collides case-insensitively (APFS is case-insensitive by default) gets `_2`, `_3`, ….
pub fn assign_names(layout: &[SourceKey]) -> Vec<String> {
    let mut taken = std::collections::HashSet::new();
    layout
        .iter()
        .map(|k| {
            let base = match k {
                SourceKey::Inst(id) => sanitize(id),
                SourceKey::Bus(id) => format!("bus_{}", sanitize(id)),
            };
            let mut name = base.clone();
            let mut n = 2;
            while !taken.insert(name.to_lowercase()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}

/// Free bytes on the filesystem holding `path` (or its nearest existing ancestor).
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let p = existing_ancestor(path)?;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` only writes the struct we hand it; `c` is a valid NUL-terminated path.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}
#[cfg(not(unix))]
pub fn free_bytes(_: &Path) -> Option<u64> {
    None
}

/// Do two paths (or their nearest existing ancestors) live on the same filesystem?
#[cfg(unix)]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| existing_ancestor(p).and_then(|p| fs::metadata(p).ok()).map(|m| m.dev());
    matches!((dev(a), dev(b)), (Some(x), Some(y)) if x == y)
}
#[cfg(not(unix))]
pub fn same_volume(_: &Path, _: &Path) -> bool {
    true
}

fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut p = path.to_path_buf();
    loop {
        if p.as_os_str().is_empty() {
            return Some(PathBuf::from("."));
        }
        if p.exists() {
            return Some(p);
        }
        p = p.parent()?.to_path_buf();
    }
}

/// Removes our temp files, then our dirs (innermost first, each only once empty) however the run
/// ends short of a kill — a killed run's are reclaimed by the next ([`reclaim_stale_runs`]).
pub struct TempGuard {
    pub files: Vec<PathBuf>,
    pub dirs: Vec<PathBuf>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        for f in &self.files {
            let _ = fs::remove_file(f);
        }
        for d in &self.dirs {
            let _ = fs::remove_dir(d); // only succeeds when empty — never touches foreign files
        }
    }
}

/// A fresh temp dir under `root` that only this run uses: created exclusively (an existing dir is
/// never adopted), with an owner marker holding our pid.
pub fn create_run_dir(root: &Path) -> io::Result<PathBuf> {
    let pid = std::process::id();
    for n in 0..1000 {
        let dir = root.join(if n == 0 { format!("{RUN_DIR_PREFIX}{pid}") } else { format!("{RUN_DIR_PREFIX}{pid}.{n}") });
        match fs::create_dir(&dir) {
            Ok(()) => {
                if let Err(e) = fs::write(dir.join(OWNER_MARKER), format!("{pid}\n")) {
                    let _ = fs::remove_dir(&dir);
                    return Err(e);
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("no free {RUN_DIR_PREFIX}{pid}.* dir in {}", root.display())))
}

/// Is `pid` a running process? Unknown ⇒ yes, so a doubtful run dir is left alone.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return true };
    // SAFETY: signal 0 sends nothing — it only asks whether `pid` exists.
    let probed = unsafe { libc::kill(pid, 0) };
    probed == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
#[cfg(not(unix))]
fn process_alive(_: u32) -> bool {
    true
}

/// Reclaim the float temps of earlier runs killed before their [`TempGuard`] ran (Ctrl-C, a
/// crash): only `engines-stems-*` dirs under `root` whose owner marker names a pid that is no longer
/// running, and only our files in them (`*.f32.wav` + the marker); each dir is removed only if that
/// empties it. Returns the bytes freed.
pub fn reclaim_stale_runs(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root) else { return 0 };
    let mut freed = 0;
    for e in entries.flatten() {
        let dir = e.path();
        if !e.file_name().to_string_lossy().starts_with(RUN_DIR_PREFIX) || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let owner = fs::read_to_string(dir.join(OWNER_MARKER)).ok().and_then(|s| s.trim().parse::<u32>().ok());
        if owner.is_none_or(process_alive) {
            continue;
        }
        for f in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if f.file_name().to_string_lossy().ends_with(TEMP_SUFFIX) && f.file_type().is_ok_and(|t| t.is_file()) {
                let len = f.metadata().map_or(0, |m| m.len());
                if fs::remove_file(f.path()).is_ok() {
                    freed += len;
                }
            }
        }
        let _ = fs::remove_file(dir.join(OWNER_MARKER));
        let _ = fs::remove_dir(&dir);
    }
    freed
}

/// Before a re-export into `dir` writes its manifest: delete the stems the previous `stems.json`
/// recorded that this export doesn't rewrite (`fresh` file names) — a source that is now silent or
/// gone, or the other container — so no stale audio sits beside the new manifest. Only bare file
/// names with a `.wav`/`.flac` extension are honoured (a hand-edited manifest can't aim this outside
/// `dir`), and names compare case-insensitively (APFS: `PAD.flac` *is* a fresh `pad.flac`).
/// Returns the names removed.
pub fn remove_stale_stems(dir: &Path, fresh: &[String]) -> Vec<String> {
    remove_stale_files(dir, MANIFEST, fresh)
}

/// [`remove_stale_stems`] for any manifest `engines-song` writes: the audio files its previous
/// `dir/<manifest>` recorded — a stem's `file`, a song's per-block `files[].file` (ADR 0043), a
/// block-split mix's `blocks[].file` — that `fresh` doesn't rewrite.
pub fn remove_stale_files(dir: &Path, manifest: &str, fresh: &[String]) -> Vec<String> {
    let Ok(text) = fs::read_to_string(dir.join(manifest)) else { return vec![] };
    let Ok(prior) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    let fresh: HashSet<String> = fresh.iter().map(|f| f.to_lowercase()).collect();
    let entries = |key: &str| prior[key].as_array().cloned().unwrap_or_default();
    let stems = entries("stems");
    let recorded: Vec<&serde_json::Value> = stems
        .iter()
        .flat_map(|s| std::iter::once(&s["file"]).chain(s["files"].as_array().into_iter().flatten().map(|f| &f["file"])))
        .collect();
    let blocks = entries("blocks");
    recorded
        .into_iter()
        .chain(blocks.iter().map(|b| &b["file"]))
        .filter_map(|f| f.as_str())
        .filter(|f| Path::new(f).file_name() == Some(OsStr::new(f)) && is_audio_file(f) && !fresh.contains(&f.to_lowercase()))
        .filter(|f| fs::remove_file(dir.join(f)).is_ok())
        .map(String::from)
        .collect()
}

/// One stem's float temp + meters, filled during the render. The temp file is created at the
/// stem's first non-zero sample (the leading silence is written then), so a stem that never
/// sounds costs no disk.
pub struct TempStem {
    pub path: PathBuf,
    pub w: Option<WavWriter<File>>,
    /// Silent frames seen before the file was opened.
    pub zeros: u64,
    pub meter: LoudnessMeter,
    pub peak: f32,
}

impl TempStem {
    /// A stem whose temp will be `path` (created at its first non-zero sample).
    pub fn new(path: PathBuf, sr: f64) -> Self {
        TempStem { path, w: None, zeros: 0, meter: LoudnessMeter::new(sr, 2), peak: 0.0 }
    }

    pub fn push(&mut self, s: &[f32], sr: u32) -> io::Result<()> {
        self.meter.push_interleaved(s);
        self.peak = s.iter().fold(self.peak, |p, x| p.max(x.abs()));
        if self.w.is_none() {
            if s.iter().all(|&x| x == 0.0) {
                self.zeros += (s.len() / 2) as u64;
                return Ok(());
            }
            let mut w = WavWriter::create(&self.path, 2, sr, SampleFormat::F32)?;
            let silence = vec![0.0f32; 1 << 16];
            let mut left = self.zeros * 2;
            while left > 0 {
                let n = left.min(silence.len() as u64) as usize;
                w.write_interleaved(&silence[..n])?;
                left -= n as u64;
            }
            self.w = Some(w);
        }
        self.w.as_mut().unwrap().write_interleaved(s)
    }
}

/// The render side: deinterleaves `2 + 2N`-wide frames into the temps, and checks the master — its
/// peak, overs, loudness, and that the sources sum back to it.
pub struct StemSplitter {
    pub stems: Vec<TempStem>,
    pub width: usize,
    pub sr: u32,
    scratch: Vec<f32>,
    pub frames: u64,
    pub master_peak: f32,
    pub master_clipped: u64,
    /// BS.1770 on the mix.
    pub master_meter: LoudnessMeter,
    /// Largest |master − Σ source pairs| seen — the split's live self-check (float association only).
    pub max_err: f32,
}

impl StemSplitter {
    /// A splitter writing one temp per path in `temps` (the session's source order), frames `width` wide.
    pub fn new(temps: &[PathBuf], width: usize, sr: f64) -> Self {
        StemSplitter {
            stems: temps.iter().map(|p| TempStem::new(p.clone(), sr)).collect(),
            width,
            sr: sr as u32,
            scratch: Vec::new(),
            frames: 0,
            master_peak: 0.0,
            master_clipped: 0,
            master_meter: LoudnessMeter::new(sr, 2),
            max_err: 0.0,
        }
    }

    pub fn write_frames(&mut self, frames: &[f32]) -> io::Result<()> {
        let n = self.stems.len();
        for f in frames.chunks_exact(self.width) {
            for ch in 0..2 {
                let m = f[ch];
                self.master_peak = self.master_peak.max(m.abs());
                self.master_clipped += (m.abs() > 1.0) as u64;
                let sum: f32 = (0..n).map(|i| f[2 + 2 * i + ch]).sum();
                self.max_err = self.max_err.max((sum - m).abs());
            }
        }
        self.scratch.clear();
        self.scratch.extend(frames.chunks_exact(self.width).flat_map(|f| [f[0], f[1]]));
        self.master_meter.push_interleaved(&self.scratch);
        for (i, st) in self.stems.iter_mut().enumerate() {
            self.scratch.clear();
            self.scratch.extend(frames.chunks_exact(self.width).flat_map(|f| [f[2 + 2 * i], f[3 + 2 * i]]));
            st.push(&self.scratch, self.sr)?;
        }
        self.frames += (frames.len() / self.width) as u64;
        Ok(())
    }

    /// Close the temps: each stem's measurements, and its temp (`None` ⇒ it never sounded).
    pub fn finish(self) -> io::Result<Vec<Measured>> {
        self.stems
            .into_iter()
            .map(|st| {
                let temp = st.w.is_some().then(|| st.path.clone());
                if let Some(w) = st.w {
                    w.finalize()?;
                }
                Ok(Measured { temp, meter: st.meter, peak: st.peak })
            })
            .collect()
    }
}

/// A stem after the render: its temp (None ⇒ digital silence) and measurements.
pub struct Measured {
    pub temp: Option<PathBuf>,
    pub meter: LoudnessMeter,
    pub peak: f32,
}

/// The gain each written stem gets.
pub struct Decision {
    pub gain_db: f64,
    pub normalized: bool,
    pub limited_by: Option<&'static str>,
    pub raw_lufs: Option<f64>,
}

pub fn decide(m: &Measured, target: f64) -> Decision {
    let raw_lufs = m.meter.integrated();
    match normalization_gain(&m.meter, m.peak, target, PEAK_CEILING_DBFS) {
        Some(g) => Decision { gain_db: g.gain_db, normalized: true, limited_by: Some(g.limited_by.as_str()), raw_lufs },
        // Audible but entirely under the −70 LUFS gate: kept at unity so the stems still sum to the mix.
        None => Decision { gain_db: 0.0, normalized: false, limited_by: None, raw_lufs },
    }
}

/// Read a float temp, apply the static gain (`x as f64 × g`, back to f32 — exactly), meter the
/// result and hand each chunk to `write` on the writer `open` makes at the temp's sample rate.
/// Returns that writer (the caller finalizes it, then deletes the temp) and the output's integrated
/// loudness.
pub fn rescale<W>(temp: &Path, gain_db: f64, open: impl FnOnce(u32) -> io::Result<W>, mut write: impl FnMut(&mut W, &[f32]) -> io::Result<()>) -> io::Result<(W, Option<f64>)> {
    let mut rd = WavReader::open(temp)?;
    let sr = rd.sample_rate();
    let g = 10f64.powf(gain_db / 20.0);
    let mut w = open(sr)?;
    let mut meter = LoudnessMeter::new(sr as f64, 2);
    let mut buf = vec![0.0f32; 1 << 16];
    loop {
        let n = rd.read_interleaved(&mut buf)?;
        if n == 0 {
            break;
        }
        for x in &mut buf[..n] {
            *x = (*x as f64 * g) as f32;
        }
        meter.push_interleaved(&buf[..n]);
        write(&mut w, &buf[..n])?;
    }
    Ok((w, meter.integrated()))
}

/// [`rescale`] into a 24-bit / float WAV at `out` (finalized), the temp deleted: the output's
/// loudness and peak.
pub fn rescale_to_wav(temp: &Path, out: &Path, gain_db: f64, format: SampleFormat) -> io::Result<(Option<f64>, f32)> {
    let (w, lufs) = rescale(temp, gain_db, |sr| WavWriter::create(out, 2, sr, format), |w, s| w.write_interleaved(s))?;
    let (_, st) = w.finalize()?;
    fs::remove_file(temp)?;
    Ok((lufs, st.peak))
}

/// Run `f` over `jobs` on up to `available_parallelism` scoped threads, results in job order.
pub fn parallel_map<J: Sync, R: Send>(jobs: &[J], f: impl Fn(&J) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len()).max(1);
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<R>>> = jobs.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(i) else { break };
                *results[i].lock().unwrap() = Some(f(job));
            });
        }
    });
    results.into_iter().map(|m| m.into_inner().unwrap().expect("every job ran")).collect()
}

/// A linear peak in dBFS.
pub fn db(x: f32) -> f64 {
    20.0 * (x as f64).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("engines_stems_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_are_sanitized_prefixed_and_unique_ignoring_case() {
        let layout = [
            SourceKey::Inst("Pad".into()),
            SourceKey::Inst("pad".into()),
            SourceKey::Inst("bell/hi".into()),
            SourceKey::Inst("bus_rv".into()),
            SourceKey::Bus("rv".into()),
            SourceKey::Bus("rv".into()),
        ];
        assert_eq!(assign_names(&layout), ["Pad", "pad_2", "bell_hi", "bus_rv", "bus_rv_2", "bus_rv_3"]);
    }

    #[test]
    fn temp_stems_skip_silence_and_keep_alignment() {
        let d = scratch_dir("temp");
        let mut st = TempStem::new(d.join("x.f32.wav"), 8_000.0);
        st.push(&[0.0; 6], 8_000).unwrap();
        assert!(!st.path.exists(), "silence alone writes nothing");
        st.push(&[0.0, 0.0, 0.5, -0.25], 8_000).unwrap();
        st.w.take().unwrap().finalize().unwrap();
        let mut rd = WavReader::open(&st.path).unwrap();
        let mut buf = [1.0f32; 16];
        let n = rd.read_interleaved(&mut buf).unwrap();
        assert_eq!(&buf[..n], &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, -0.25], "leading silence is written once it sounds");
        assert_eq!(st.peak, 0.5);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn below_gate_audio_stays_at_unity() {
        let mut meter = LoudnessMeter::new(48_000.0, 2);
        meter.push_interleaved(&vec![1e-5; 48_000 * 2]);
        let d = decide(&Measured { temp: None, meter, peak: 1e-5 }, -18.0);
        assert_eq!((d.gain_db, d.normalized, d.limited_by), (0.0, false, None));
    }

    #[test]
    fn temp_guard_removes_only_its_own_files() {
        let d = scratch_dir("guard");
        let ours = d.join("a.f32.wav");
        let foreign = d.join("keep.txt");
        fs::write(&ours, b"x").unwrap();
        fs::write(&foreign, b"y").unwrap();
        drop(TempGuard { files: vec![ours.clone()], dirs: vec![d.clone()] });
        assert!(!ours.exists() && foreign.exists() && d.exists(), "a dir with foreign files is left alone");
        fs::remove_file(&foreign).unwrap();
        drop(TempGuard { files: vec![], dirs: vec![d.clone()] });
        assert!(!d.exists(), "an empty temp dir is removed");
    }

    /// The Copilot case: `--stem-temp-dir` is a shared dir that already holds a file named like one
    /// of our temps. The run works in its own fresh subdir, so that file survives the whole run.
    #[test]
    fn run_dirs_are_fresh_and_never_touch_a_shared_temp_root() {
        let root = scratch_dir("runroot");
        let pid = std::process::id();
        let users = root.join(format!("pad{TEMP_SUFFIX}"));
        fs::write(&users, b"user data").unwrap();
        // Someone else's dir that happens to carry our name is never adopted.
        let squatter = root.join(format!("{RUN_DIR_PREFIX}{pid}"));
        fs::create_dir(&squatter).unwrap();
        fs::write(squatter.join("theirs.txt"), b"y").unwrap();
        let run = create_run_dir(&root).unwrap();
        assert_eq!(run, root.join(format!("{RUN_DIR_PREFIX}{pid}.1")));
        assert_eq!(fs::read_to_string(run.join(OWNER_MARKER)).unwrap().trim(), pid.to_string());
        let ours = run.join(format!("pad{TEMP_SUFFIX}"));
        fs::write(&ours, b"temp").unwrap();
        drop(TempGuard { files: vec![ours, run.join(OWNER_MARKER)], dirs: vec![run.clone(), root.clone()] });
        assert!(!run.exists(), "the run dir is gone");
        assert_eq!(fs::read(&users).unwrap(), b"user data", "the user's same-named file is untouched");
        assert!(squatter.join("theirs.txt").exists() && root.exists(), "a non-empty root stays");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn reclaim_takes_only_dead_runs_and_only_our_files() {
        let root = scratch_dir("reclaim");
        // A pid that has certainly exited: a child we've already reaped.
        let dead = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        let mut dead = dead;
        dead.wait().unwrap();
        let run = |name: &str, owner: Option<u32>, files: &[(&str, &[u8])]| {
            let d = root.join(name);
            fs::create_dir(&d).unwrap();
            if let Some(pid) = owner {
                fs::write(d.join(OWNER_MARKER), format!("{pid}\n")).unwrap();
            }
            for (f, b) in files {
                fs::write(d.join(f), b).unwrap();
            }
            d
        };
        let killed = run("engines-stems-1", Some(dead_pid), &[("pad.f32.wav", b"12345"), ("bus_rv.f32.wav", b"678")]);
        let mixed = run("engines-stems-2", Some(dead_pid), &[("pad.f32.wav", b"12"), ("notes.txt", b"keep")]);
        let live = run("engines-stems-3", Some(std::process::id()), &[("pad.f32.wav", b"running")]);
        let unmarked = run("engines-stems-4", None, &[("pad.f32.wav", b"no marker")]);
        let other = run("renders", Some(dead_pid), &[("pad.f32.wav", b"wrong prefix")]);
        assert_eq!(reclaim_stale_runs(&root), 5 + 3 + 2, "bytes of the dead runs' temps");
        assert!(!killed.exists(), "a dead run's dir is removed whole");
        assert!(mixed.join("notes.txt").exists() && !mixed.join("pad.f32.wav").exists(), "foreign files survive, so does their dir");
        assert!(live.join("pad.f32.wav").exists(), "a running render is never touched");
        assert!(unmarked.join("pad.f32.wav").exists() && other.join("pad.f32.wav").exists(), "no marker / wrong name ⇒ not ours");
        assert_eq!(reclaim_stale_runs(&root.join("missing")), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reexport_removes_only_stale_stems_the_previous_manifest_recorded() {
        let root = scratch_dir("stale");
        let dir = root.join("stems");
        fs::create_dir(&dir).unwrap();
        let outside = root.join("outside.wav");
        for f in ["pad.flac", "bells.flac", "bus_rv.flac", "bells.wav", "mine.flac", "notes.txt"] {
            fs::write(dir.join(f), b"x").unwrap();
        }
        fs::write(&outside, b"x").unwrap();
        // The previous export: pad + bells + the reverb bus as FLAC, plus a silent `harp` (no file).
        // Hand-edited entries pointing outside `dir` or at a non-audio file must be ignored.
        let prior = serde_json::json!({
            "stems": [
                { "file": "PAD.flac" }, { "file": "bells.flac" }, { "file": "bus_rv.flac" },
                { "file": "../outside.wav" }, { "file": "notes.txt" }, { "file": "gone.flac" }, {},
            ],
            "skipped": [{ "kind": "instrument", "id": "harp", "reason": "digital silence" }],
        });
        fs::write(dir.join(MANIFEST), prior.to_string()).unwrap();
        // This export: pad again (case differs — the same file on APFS), bells now as WAV, the bus silent.
        let mut removed = remove_stale_stems(&dir, &["pad.flac".into(), "bells.wav".into()]);
        removed.sort();
        assert_eq!(removed, ["bells.flac", "bus_rv.flac"]);
        for keep in ["pad.flac", "bells.wav", "mine.flac", "notes.txt"] {
            assert!(dir.join(keep).exists(), "{keep} kept");
        }
        assert!(outside.exists(), "never outside the stems dir");
        assert!(remove_stale_stems(&root.join("nowhere"), &[]).is_empty(), "no prior manifest ⇒ nothing removed");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reexport_removes_stale_block_files_too() {
        // A song's stems (ADR 0043) record a `files` list per stem; a block-split mix, `blocks`.
        let dir = scratch_dir("stale_blocks");
        for f in ["s-pad-00-a.flac", "s-pad-01-b.flac", "pad.flac", "s-mix-00-a.flac", "s-mix-01-b.flac"] {
            fs::write(dir.join(f), b"x").unwrap();
        }
        let stems = serde_json::json!({ "stems": [
            { "files": [{ "file": "s-pad-00-a.flac" }, { "file": "s-pad-01-b.flac" }, { "file": "../x.flac" }] },
            { "file": "pad.flac" },
        ] });
        fs::write(dir.join(MANIFEST), stems.to_string()).unwrap();
        let mut removed = remove_stale_stems(&dir, &["s-pad-00-a.flac".into()]);
        removed.sort();
        assert_eq!(removed, ["pad.flac", "s-pad-01-b.flac"]);
        fs::write(dir.join("blocks.json"), serde_json::json!({ "blocks": [{ "file": "s-mix-00-a.flac" }, { "file": "s-mix-01-b.flac" }] }).to_string()).unwrap();
        assert_eq!(remove_stale_files(&dir, "blocks.json", &["s-mix-00-a.flac".into()]), ["s-mix-01-b.flac"]);
        assert!(dir.join("s-pad-00-a.flac").exists() && dir.join("s-mix-00-a.flac").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_map_keeps_job_order() {
        let jobs: Vec<u32> = (0..37).collect();
        assert_eq!(parallel_map(&jobs, |&j| j * 2), jobs.iter().map(|j| j * 2).collect::<Vec<_>>());
    }
}
