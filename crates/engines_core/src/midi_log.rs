//! The MIDI log a stems render writes beside its stems (ADR 0048): every mirrored instrument's notes
//! on the render's absolute frame clock, plus what the Live set builder needs to cut them into one
//! clip per phrase pass — per loop, the sources feeding each instrument and the Orrery's chord, and
//! per source, where each of its passes started.
//!
//! Pure data: [`MidiLogBuilder`] folds a session's mirror stream ([`crate::session::Session::poll_note_tap`])
//! and its [`LoopRecord`]s (with their note logs) into a [`MidiLog`]; the shell writes it as JSON.

use crate::loop_doc::LoopDoc;
use crate::midi::{TapMsg, TimedTap};
use crate::telemetry::LoopRecord;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The log's schema id.
pub const SCHEMA: &str = "engines-midi-log/1";
/// Its file name inside a stems render's directory.
pub const FILE: &str = "midi.json";

/// A render's MIDI log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiLog {
    pub schema: String,
    pub engine_version: String,
    /// The engine build that rendered it ([`crate::build_id`], ADR 0054 P10); empty in a log written
    /// before the field existed.
    #[serde(default)]
    pub engine_build: String,
    pub sample_rate: u32,
    pub bpm: f64,
    /// Frames rendered.
    pub frames: u64,
    /// The doc's key (`harmony.root` / `mode`) — note names, chord labels.
    pub root: String,
    pub mode: String,
    /// A song's blocks (and its tail) in order; empty for a song-less render.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<LogBlock>,
    /// Every loop rendered, in order.
    pub loops: Vec<LogLoop>,
    /// Pool-entry names, indexed by [`LogTile::entry`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<String>,
    /// Note source (modulator) id → its kind and pass starts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, LogSource>,
    /// Mirrored instrument → its notes.
    pub instruments: BTreeMap<String, LogInstrument>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogBlock {
    pub block: u32,
    pub label: String,
    pub start_frame: u64,
    pub frames: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tail: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogLoop {
    pub start_frame: u64,
    pub frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tail: bool,
    /// The Orrery's chord after this loop's pick (`None` without the Orrery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord: Option<String>,
    /// Instrument → the note sources routed into it this loop.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub feeds: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogSource {
    /// `phrase` | `steps` | `generator`.
    pub kind: String,
    pub tiles: Vec<LogTile>,
}

/// One pass of a source: where it started (absolute frame), its count, the entry it played.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogTile {
    pub start: u64,
    pub ordinal: i64,
    /// Index into [`MidiLog::entries`] (phrase arms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<u32>,
    /// The pass length as started; the next pass's start is the authority.
    pub len: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub margin: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub realized: bool,
}

fn is_zero(x: &u32) -> bool {
    *x == 0
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogInstrument {
    /// `[start, end, key, velocity]`, sorted by (start, key); frames are absolute.
    pub notes: Vec<(u64, u64, u8, u8)>,
}

/// Folds a session's mirror stream and loop records into a [`MidiLog`].
#[derive(Clone, Debug)]
pub struct MidiLogBuilder {
    log: MidiLog,
    labels: Vec<String>,
    kinds: BTreeMap<String, String>,
    /// (instrument, key) → (start, velocity) of the note sounding.
    open: BTreeMap<(String, u8), (u64, u8)>,
}

impl MidiLogBuilder {
    /// A builder for `doc` rendered at `sample_rate`, logging `instruments` (each gets an entry, even
    /// if it plays nothing). A song's block labels come from the doc's `song`.
    pub fn new(doc: &LoopDoc, sample_rate: u32, instruments: impl IntoIterator<Item = String>) -> Self {
        let labels = doc.song.as_ref().map(|s| (0..s.sequence.len()).map(|i| s.label(i)).collect()).unwrap_or_default();
        let kinds = doc
            .modulators
            .values()
            .filter(|m| m.output == "note")
            .map(|m| {
                let kind = if m.phrase.is_some() { "phrase" } else if m.generator.is_some() { "generator" } else { "steps" };
                (m.id.clone(), kind.to_string())
            })
            .collect();
        let log = MidiLog {
            schema: SCHEMA.into(),
            engine_version: crate::version().into(),
            engine_build: crate::build_id().into(),
            sample_rate,
            bpm: doc.transport.bpm as f64,
            frames: 0,
            root: doc.harmony.root.clone(),
            mode: doc.harmony.mode.clone(),
            blocks: Vec::new(),
            loops: Vec::new(),
            entries: Vec::new(),
            sources: BTreeMap::new(),
            instruments: instruments.into_iter().map(|i| (i, LogInstrument::default())).collect(),
        };
        Self { log, labels, kinds, open: BTreeMap::new() }
    }

    /// The mirror's events, in order.
    pub fn on_taps(&mut self, events: &[TimedTap]) {
        for e in events {
            match e.msg {
                TapMsg::NoteOn { note, vel } => {
                    // A second On without an Off can't come from the mirror's diff; close the first.
                    self.close(&e.inst, note, e.abs_frame);
                    self.open.insert((e.inst.clone(), note), (e.abs_frame, vel));
                }
                TapMsg::NoteOff { note } => self.close(&e.inst, note, e.abs_frame),
            }
        }
    }

    fn close(&mut self, inst: &str, key: u8, at: u64) {
        if let Some((start, vel)) = self.open.remove(&(inst.to_string(), key)) {
            self.log.instruments.entry(inst.to_string()).or_default().notes.push((start, at, key, vel));
        }
    }

    /// Loop records, in order (each with its note log, when the session logged them).
    pub fn on_records(&mut self, records: &[LoopRecord]) {
        for r in records {
            let block = r.song.as_ref().map(|s| s.block);
            let tail = r.song.as_ref().is_some_and(|s| s.tail);
            self.log.loops.push(LogLoop {
                start_frame: r.start_frame,
                frames: r.num_frames as u64,
                block,
                tail,
                chord: r.orrery.as_ref().map(|o| o.chord.clone()),
                feeds: r.note_log.as_ref().map(|n| n.feeds.clone()).unwrap_or_default(),
            });
            if let Some(s) = &r.song {
                self.on_block(s.block, s.tail, r.start_frame, r.num_frames as u64);
            }
            let Some(note_log) = &r.note_log else { continue };
            for (src, marks) in &note_log.tiles {
                for m in marks {
                    let start = (r.start_frame as i64 + m.frame).max(0) as u64;
                    let entry = m.entry.as_ref().map(|e| self.entry_index(e));
                    let kind = self.kinds.get(src).cloned().unwrap_or_else(|| "steps".into());
                    let tiles = &mut self.log.sources.entry(src.clone()).or_insert_with(|| LogSource { kind, tiles: Vec::new() }).tiles;
                    let tile = LogTile { start, ordinal: m.ordinal, entry, len: m.len_frames, margin: m.margin, realized: m.realized };
                    // A free-run tile straddling the seam is reported again by the next loop (same
                    // start): the later report wins — an Orrery refit may have changed what it plays.
                    match tiles.last_mut() {
                        Some(last) if last.start == start => *last = tile,
                        _ => tiles.push(tile),
                    }
                }
            }
        }
    }

    fn on_block(&mut self, block: u32, tail: bool, start: u64, frames: u64) {
        match self.log.blocks.last_mut() {
            Some(b) if b.block == block && b.tail == tail => b.frames += frames,
            _ => {
                let label = if tail { "tail".to_string() } else { self.labels.get(block as usize).cloned().unwrap_or_else(|| format!("block {}", block + 1)) };
                self.log.blocks.push(LogBlock { block, label, start_frame: start, frames, tail });
            }
        }
    }

    fn entry_index(&mut self, name: &str) -> u32 {
        match self.log.entries.iter().position(|e| e == name) {
            Some(i) => i as u32,
            None => {
                self.log.entries.push(name.to_string());
                (self.log.entries.len() - 1) as u32
            }
        }
    }

    /// The finished log for a render of `frames` frames: notes still open end there (the session's
    /// exit flush normally ends them first), and every list is sorted.
    pub fn finish(mut self, frames: u64) -> MidiLog {
        for ((inst, key), (start, vel)) in std::mem::take(&mut self.open) {
            self.log.instruments.entry(inst).or_default().notes.push((start, frames.max(start), key, vel));
        }
        for i in self.log.instruments.values_mut() {
            i.notes.sort();
        }
        self.log.frames = frames;
        self.log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LoopNoteLog;
    use crate::modulator::TileMark;
    use crate::telemetry::SongStamp;

    fn doc() -> LoopDoc {
        LoopDoc::from_json_str(
            r#"{ "schema_version": "0.9", "transport": { "bpm": 72.5, "master_steps": 4 },
                 "harmony": { "root": "F", "mode": "ionian" },
                 "instruments": { "pad": { "params": {} } },
                 "modulators": { "ph": { "type": "step_seq", "output": "note", "sync": "free", "length": 4, "step_duration": "1/4",
                     "phrase": { "pool": ["a"], "select": "cycle" } },
                   "st": { "type": "step_seq", "output": "note", "sync": "loop", "length": 4,
                     "steps": [{ "step": 0, "voicing": { "v": { "offset": 0 } } }] } },
                 "sequence_pools": { "a": { "fit": "literal", "length": 3, "notes": [{ "t": 0, "offset": 0, "dur": 1 }] } },
                 "matrix": { "m": { "source": "ph", "dest": "instruments.pad.notes", "kind": "note", "voices": 2 } } }"#,
        )
        .unwrap()
    }

    fn on(abs: u64, note: u8) -> TimedTap {
        TimedTap { abs_frame: abs, inst: "pad".into(), msg: TapMsg::NoteOn { note, vel: 100 } }
    }
    fn off(abs: u64, note: u8) -> TimedTap {
        TimedTap { abs_frame: abs, inst: "pad".into(), msg: TapMsg::NoteOff { note } }
    }

    fn record(start: u64, marks: Vec<TileMark>, song: Option<SongStamp>) -> LoopRecord {
        LoopRecord {
            loop_id: 1,
            revision: 0,
            optimize: "hold".into(),
            start_frame: start,
            num_frames: 1000,
            path: String::new(),
            delta: 0.0,
            harmonic_move: None,
            categorical_move: None,
            song,
            peak: 0.0,
            doc_rev: 0,
            orrery: None,
            note_log: Some(LoopNoteLog { tiles: [("ph".to_string(), marks)].into_iter().collect(), feeds: [("pad".to_string(), vec!["ph".to_string()])].into_iter().collect() }),
        }
    }

    fn mark(frame: i64, ordinal: i64, entry: &str) -> TileMark {
        TileMark { frame, ordinal, entry: Some(entry.into()), len_frames: 750, margin: 0, realized: false }
    }

    #[test]
    fn notes_pair_up_and_a_re_strike_closes_the_note_it_restarts() {
        let mut b = MidiLogBuilder::new(&doc(), 48_000, ["pad".to_string()]);
        b.on_taps(&[on(0, 65), on(10, 69), off(500, 65), off(600, 69), on(600, 69), off(900, 69)]);
        let log = b.finish(2000);
        assert_eq!(log.instruments["pad"].notes, vec![(0, 500, 65, 100), (10, 600, 69, 100), (600, 900, 69, 100)]);
        assert_eq!((log.bpm, log.frames, log.root.as_str(), log.schema.as_str()), (72.5, 2000, "F", SCHEMA));
    }

    #[test]
    fn an_open_note_ends_at_the_render_end() {
        let mut b = MidiLogBuilder::new(&doc(), 48_000, ["pad".to_string()]);
        b.on_taps(&[on(100, 65)]);
        assert_eq!(b.finish(2000).instruments["pad"].notes, vec![(100, 2000, 65, 100)]);
    }

    #[test]
    fn tiles_go_absolute_and_a_straddling_tile_is_kept_once_with_its_latest_entry() {
        let mut b = MidiLogBuilder::new(&doc(), 48_000, ["pad".to_string()]);
        b.on_records(&[record(0, vec![mark(0, 0, "a"), mark(750, 1, "a")], None)]);
        // Loop 2 reports tile 1 again at its negative start, now refit (entry b).
        b.on_records(&[record(1000, vec![mark(-250, 1, "b"), mark(500, 2, "a")], None)]);
        let log = b.finish(2000);
        let tiles: Vec<(u64, i64, Option<u32>)> = log.sources["ph"].tiles.iter().map(|t| (t.start, t.ordinal, t.entry)).collect();
        assert_eq!(tiles, vec![(0, 0, Some(0)), (750, 1, Some(1)), (1500, 2, Some(0))]);
        assert_eq!(log.entries, vec!["a", "b"]);
        assert_eq!(log.sources["ph"].kind, "phrase");
        assert_eq!(log.loops.len(), 2);
        assert_eq!(log.loops[0].feeds["pad"], vec!["ph"]);
    }

    #[test]
    fn song_blocks_and_the_tail_are_gathered_from_the_loops() {
        let stamp = |block, tail| Some(SongStamp { block, seam: false, loop_in_block: 0, block_loops: 1, tail });
        let mut d = doc();
        d.song = Some(serde_json::from_value(serde_json::json!({ "sequence": [ { "loops": 1, "label": "dawn" }, { "loops": 2 } ], "tail_loops": 1 })).unwrap());
        let mut b = MidiLogBuilder::new(&d, 48_000, ["pad".to_string()]);
        b.on_records(&[record(0, vec![], stamp(0, false)), record(1000, vec![], stamp(1, false)), record(2000, vec![], stamp(1, false)), record(3000, vec![], stamp(1, true))]);
        let blocks: Vec<(u32, String, u64, u64, bool)> = b.finish(4000).blocks.into_iter().map(|b| (b.block, b.label, b.start_frame, b.frames, b.tail)).collect();
        assert_eq!(blocks, vec![(0, "dawn".into(), 0, 1000, false), (1, "base".into(), 1000, 2000, false), (1, "tail".into(), 3000, 1000, true)]);
    }

    #[test]
    fn the_log_round_trips_as_json() {
        let mut b = MidiLogBuilder::new(&doc(), 48_000, ["pad".to_string()]);
        b.on_taps(&[on(0, 65), off(500, 65)]);
        b.on_records(&[record(0, vec![mark(0, 0, "a")], None)]);
        let log = b.finish(1000);
        let j = serde_json::to_string(&log).unwrap();
        assert!(j.contains("\"notes\":[[0,500,65,100]]"), "notes are compact arrays: {j}");
        assert_eq!(serde_json::from_str::<MidiLog>(&j).unwrap(), log);
    }
}
