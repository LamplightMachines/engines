//! A song written one file per block (ADR 0043): the session's one continuous stream — the mix, or
//! a stem in the post-pass — cut sample-exactly at every block start (and the tail's) into
//! consecutive files `<song>-<part>-<NN>-<label>.<ext>`. Laid end to end, one after another on a
//! DAW track, they are the whole render bit for bit: no fade, gap or overlap at a join, the s16
//! dither carried across ([`AudioOut::continuing_at`]), and a stem's one static gain on every one
//! of its files. Every file of a stream is written — a block where a stem is digitally silent too —
//! so sequential placement never shifts a later block.
//!
//! [`SplitOut`] with one file starting at frame 0 is a plain single-file render (song-less docs,
//! `--whole`), so both shapes share one writer.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use engines_core::song::SongPlan;
use engines_core::stems::sanitize;
use engines_core::wav::SampleFormat;

use super::{AudioOut, Container};

/// The manifest a block-split mix directory carries (a stems directory has `stems.json`).
pub(super) const BLOCKS_MANIFEST: &str = "blocks.json";

/// A stream's files in order, each with the stream frame it starts at (the first at 0).
pub(super) type Files = Vec<(PathBuf, u64)>;

/// Interleaved channels of every stream written here (stereo: the mix, or one stem's pair).
const CH: u64 = 2;

/// One file's stretch of the song.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Part {
    /// The block index as the plan numbers it (the tail is numbered after the last block).
    pub idx: usize,
    pub label: String,
    pub tail: bool,
    pub start_frame: u64,
    /// Planned length (shorter for the last part of a render cut short by `--max-loops`).
    pub frames: u64,
}

/// A song's cut points and file names.
#[derive(Clone, Debug)]
pub(super) struct Blocks {
    song: String,
    pub parts: Vec<Part>,
    /// Zero-padded width of the block number (2, more for a 100-block song).
    digits: usize,
}

impl Blocks {
    /// `plan`'s segments in frames (`frames_per_loop` a loop) — the parts a render of `loops` loops
    /// reaches; the first part always exists. `song` is the file-name prefix (sanitized).
    pub fn new(song: &str, plan: &SongPlan, frames_per_loop: u64, loops: usize) -> Self {
        let segments = plan.segments();
        let digits = segments.last().map_or(1, |s| s.idx.to_string().len()).max(2);
        let parts = segments
            .into_iter()
            .filter(|s| s.start_loop < loops || s.start_loop == 0)
            .map(|s| Part {
                idx: s.idx,
                label: s.label,
                tail: s.tail,
                start_frame: s.start_loop as u64 * frames_per_loop,
                frames: s.loops.min(loops.saturating_sub(s.start_loop)) as u64 * frames_per_loop,
            })
            .collect();
        Blocks { song: sanitize(song), parts, digits }
    }

    /// `<song>-<part>-<NN>-<label>.<ext>` for part `i` of stream `part` (`mix`, or a stem's name).
    pub fn file_name(&self, i: usize, part: &str, ext: &str) -> String {
        let p = &self.parts[i];
        format!("{}-{part}-{:0w$}-{}.{ext}", self.song, p.idx, sanitize(&p.label), w = self.digits)
    }

    /// Every file of stream `part` in `dir`, each with its first frame — what [`SplitOut`] writes.
    pub fn files(&self, dir: &Path, part: &str, ext: &str) -> Files {
        (0..self.parts.len()).map(|i| (dir.join(self.file_name(i, part, ext)), self.parts[i].start_frame)).collect()
    }
}

/// The first file two streams would both write, if any: `(file, first, second)`, each stream given
/// as `<part> (block NN)`. Names compare case-insensitively (APFS: `Pad…` *is* `pad…`). Ids and
/// labels may hold `-` and digits, so different (part, block) pairs can spell the same name:
/// `pad` + block 0 `01-deep` and `pad-00` + block 1 `deep` are both `<song>-pad-00-01-deep` — and
/// the parallel post-pass would overwrite one with the other.
pub(super) fn clash(b: &Blocks, parts: &[&str], ext: &str) -> Option<(String, String, String)> {
    let mut seen = std::collections::HashMap::new();
    for part in parts {
        for (i, p) in b.parts.iter().enumerate() {
            let file = b.file_name(i, part, ext);
            let who = format!("{part} (block {:0w$})", p.idx, w = b.digits);
            if let Some(first) = seen.insert(file.to_lowercase(), who.clone()) {
                return Some((file, first, who));
            }
        }
    }
    None
}

/// What one finished file holds.
#[derive(Clone, Debug)]
pub(super) struct Written {
    pub path: PathBuf,
    pub start_frame: u64,
    pub frames: u64,
    pub peak: f32,
    pub clipped: u64,
    pub rf64: bool,
    pub bytes: u64,
}

impl Written {
    pub fn file_name(&self) -> String {
        self.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// The whole stream's totals over its files.
pub(super) struct Totals {
    pub frames: u64,
    pub peak: f32,
    pub clipped: u64,
    pub rf64: bool,
    pub bytes: u64,
}

pub(super) fn totals(w: &[Written]) -> Totals {
    Totals {
        frames: w.iter().map(|f| f.frames).sum(),
        peak: w.iter().fold(0.0, |p, f| p.max(f.peak)),
        clipped: w.iter().map(|f| f.clipped).sum(),
        rf64: w.iter().any(|f| f.rf64),
        bytes: w.iter().map(|f| f.bytes).sum(),
    }
}

/// A stereo stream written as consecutive files: each holds the frames from its start up to the
/// next file's start (the last, the rest). The first file opens at once — a bad path fails before
/// the render, not an hour in — the others as the stream reaches them.
pub(super) struct SplitOut {
    files: Files,
    container: Container,
    sr: u32,
    format: SampleFormat,
    cur: Option<AudioOut>,
    /// Index of the open file.
    at: usize,
    /// Interleaved samples received.
    samples: u64,
    done: Vec<Written>,
}

impl SplitOut {
    pub fn create(files: Files, container: Container, sr: u32, format: SampleFormat) -> io::Result<Self> {
        let first = files.first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no output file"))?;
        debug_assert!(first.1 == 0 && files.windows(2).all(|w| w[0].1 < w[1].1), "files start at 0 and ascend: {files:?}");
        let cur = AudioOut::create(&first.0, container, sr, format)?;
        Ok(SplitOut { files, container, sr, format, cur: Some(cur), at: 0, samples: 0, done: Vec::new() })
    }

    /// Append interleaved stereo samples, crossing into the next file exactly at its first frame.
    pub fn write_interleaved(&mut self, mut s: &[f32]) -> io::Result<()> {
        while !s.is_empty() {
            let room = match self.files.get(self.at + 1) {
                Some(&(_, start)) => start * CH - self.samples,
                None => s.len() as u64,
            };
            if room == 0 {
                self.next_file()?;
                continue;
            }
            let n = room.min(s.len() as u64) as usize;
            self.cur.as_mut().expect("a file is open").write_interleaved(&s[..n])?;
            self.samples += n as u64;
            s = &s[n..];
        }
        Ok(())
    }

    fn next_file(&mut self) -> io::Result<()> {
        let done = self.cur.take().expect("a file is open");
        self.done.push(finish(&self.files[self.at], done)?);
        self.at += 1;
        let (path, start) = &self.files[self.at];
        self.cur = Some(AudioOut::create(path, self.container, self.sr, self.format)?.continuing_at(start * CH));
        Ok(())
    }

    /// Durability point for the open file (the finished ones are already final).
    pub fn checkpoint(&mut self) -> io::Result<()> {
        self.cur.as_mut().map_or(Ok(()), AudioOut::checkpoint)
    }

    /// Finalize the open file: every file written, in order. Files the stream never reached (a
    /// render that ended early) were never created.
    pub fn finalize(mut self) -> io::Result<Vec<Written>> {
        if let Some(cur) = self.cur.take() {
            self.done.push(finish(&self.files[self.at], cur)?);
        }
        Ok(self.done)
    }
}

fn finish((path, start): &(PathBuf, u64), out: AudioOut) -> io::Result<Written> {
    let st = out.finalize()?;
    Ok(Written { path: path.clone(), start_frame: *start, frames: st.frames, peak: st.peak, clipped: st.clipped, rf64: st.rf64, bytes: fs::metadata(path)?.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engines_core::loop_doc::LoopDoc;
    use engines_core::wav::WavReader;

    fn song_plan(tail: u32) -> SongPlan {
        let doc = LoopDoc::from_json_str(&format!(
            r#"{{
  schema_version: "0.9",
  transport: {{ bpm: 120, master_steps: 4 }},
  harmony: {{ root: "D", mode: "dorian" }},
  instruments: {{ pad: {{ params: {{ wave: "sine", level_db: -7.96, sustain: 1.0 }} }} }},
  modulators: {{ seq: {{ type: "step_seq", output: "note", length: 4, steps: [ {{ step: 0, voicing: {{ v: {{ offset: 0 }} }} }} ] }} }},
  matrix: {{ m: {{ source: "seq", dest: "instruments.pad.notes", kind: "note" }} }},
  song: {{ matrices: {{ hush: {{ m: null }} }},
          sequence: [ {{ loops: 2, label: "first light" }}, {{ matrix: "hush", loops: 1 }}, {{ loops: 3, label: "deep" }} ],
          transition_s: 1, tail_loops: {tail} }},
}}"#
        ))
        .unwrap();
        engines_core::song::plan(&doc, doc.song.as_ref().unwrap(), 0).unwrap()
    }

    #[test]
    fn parts_and_names_follow_the_plan() {
        let b = Blocks::new("Night Song", &song_plan(2), 100, 8);
        let rows: Vec<(usize, &str, u64, u64)> = b.parts.iter().map(|p| (p.idx, p.label.as_str(), p.start_frame, p.frames)).collect();
        assert_eq!(rows, [(0, "first light", 0, 200), (1, "hush", 200, 100), (2, "deep", 300, 300), (3, "tail", 600, 200)]);
        assert_eq!(b.file_name(0, "mix", "flac"), "Night_Song-mix-00-first_light.flac");
        assert_eq!(b.file_name(3, "bus_rv", "wav"), "Night_Song-bus_rv-03-tail.wav");
        // `--max-loops` cuts the song: unreached parts go, the last reached one is short.
        let b = Blocks::new("s", &song_plan(2), 100, 4);
        let rows: Vec<(usize, u64, u64)> = b.parts.iter().map(|p| (p.idx, p.start_frame, p.frames)).collect();
        assert_eq!(rows, [(0, 0, 200), (1, 200, 100), (2, 300, 100)]);
        assert_eq!(b.file_name(2, "mix", "flac"), "s-mix-02-deep.flac", "the number stays the block's");
        assert_eq!(Blocks::new("s", &song_plan(0), 100, 6).parts.len(), 3, "no tail loops ⇒ no tail file");
    }

    #[test]
    fn a_name_two_stems_would_share_is_found() {
        // `pad` in block 0 (`01-deep`) and `pad-00` in block 1 (`deep`) spell the same file.
        let doc = |labels: [&str; 2]| {
            let src = format!(
                r#"{{ schema_version: "0.9", transport: {{ bpm: 120, master_steps: 4 }}, harmony: {{ root: "D", mode: "dorian" }},
  instruments: {{ pad: {{ params: {{ wave: "sine" }} }} }}, modulators: {{}}, matrix: {{}},
  song: {{ sequence: [ {{ loops: 1, label: "{}" }}, {{ loops: 1, label: "{}" }} ], transition_s: 1, tail_loops: 0 }} }}"#,
                labels[0], labels[1]
            );
            let doc = LoopDoc::from_json_str(&src).unwrap();
            Blocks::new("night", &engines_core::song::plan(&doc, doc.song.as_ref().unwrap(), 0).unwrap(), 100, 2)
        };
        let b = doc(["01-deep", "deep"]);
        assert_eq!(clash(&b, &["pad", "pad-00"], "wav"), Some(("night-pad-00-01-deep.wav".into(), "pad (block 00)".into(), "pad-00 (block 01)".into())));
        assert_eq!(clash(&b, &["pad", "PAD-00"], "wav").map(|c| c.0), Some("night-PAD-00-01-deep.wav".into()), "case-insensitively");
        assert_eq!(clash(&b, &["pad", "pad-x"], "wav"), None);
        assert_eq!(clash(&doc(["a", "b"]), &["pad", "pad-00"], "wav"), None);
    }

    fn read_all(path: &Path) -> Vec<f32> {
        let mut rd = WavReader::open(path).unwrap();
        let mut out = vec![0.0f32; rd.frames() as usize * 2];
        let n = rd.read_interleaved(&mut out).unwrap();
        out.truncate(n);
        out
    }

    #[test]
    fn split_files_laid_end_to_end_are_the_whole_stream() {
        let d = std::env::temp_dir().join(format!("engines_song_split_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        // 1000 frames of a sine past full scale in places, cut at 300 and 301 frames and 700.
        let sig: Vec<f32> = (0..2000).map(|i| ((i as f32) * 0.013).sin() * 1.05).collect();
        for fmt in [SampleFormat::F32, SampleFormat::S16] {
            let whole_path = d.join("whole.wav");
            let mut w = SplitOut::create(vec![(whole_path.clone(), 0)], Container::Wav, 48_000, fmt).unwrap();
            w.write_interleaved(&sig).unwrap();
            let whole = w.finalize().unwrap();
            assert_eq!((whole.len(), whole[0].frames), (1, 1000));

            let files: Vec<(PathBuf, u64)> = [0u64, 300, 301, 700].iter().map(|&s| (d.join(format!("p{s}.wav")), s)).collect();
            let mut w = SplitOut::create(files, Container::Wav, 48_000, fmt).unwrap();
            for chunk in sig.chunks(2 * 97) {
                w.write_interleaved(chunk).unwrap(); // writes straddle every cut
            }
            let parts = w.finalize().unwrap();
            assert_eq!(parts.iter().map(|p| (p.start_frame, p.frames)).collect::<Vec<_>>(), [(0, 300), (300, 1), (301, 399), (700, 300)]);
            let t = totals(&parts);
            assert_eq!((t.frames, t.peak, t.clipped), (1000, whole[0].peak, whole[0].clipped), "{fmt:?}");
            let joined: Vec<f32> = parts.iter().flat_map(|p| read_all(&p.path)).collect();
            assert_eq!(joined, read_all(&whole_path), "{fmt:?}: the joined files are the whole stream, sample for sample");
        }
        // A stream that ends early never creates the files it didn't reach.
        let files: Vec<(PathBuf, u64)> = [0u64, 10, 20].iter().map(|&s| (d.join(format!("early{s}.wav")), s)).collect();
        let mut w = SplitOut::create(files, Container::Wav, 48_000, SampleFormat::F32).unwrap();
        w.write_interleaved(&sig[..2 * 15]).unwrap();
        assert_eq!(w.finalize().unwrap().len(), 2);
        assert!(!d.join("early20.wav").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
