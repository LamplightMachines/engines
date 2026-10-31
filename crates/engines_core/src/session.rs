//! Streaming session (CE-R3.3, CE-R11): the live, pull-based render.
//!
//! A background **render worker** runs the loop indefinitely (or for a bounded number of loops, or
//! a whole song): controller pick → render → push the audio into a lock-free ring → re-base. The
//! **caller's audio thread** (the Swift `AVAudioSourceNode` render block over the FFI, a cpal
//! callback, or a Rust test) drains the ring via [`Session::pull_audio`] — RT-safe, pops only.
//! Per-loop [`LoopRecord`]s, telemetry, viz frames and MIDI are drained off the audio path. The
//! audio is a pure function of (doc, seed, control timeline) — CE-R7.3, pinned by `tests/golden.rs`.

use crate::contract::{LiveToggle, SessionControl};
use crate::controller::{doc_swap_is_structural, Controller, DitherConfig};
use crate::engine::{source_layout, Engine, PhaseSnapshot, RenderResult, SourceKey, SourceStems};
use crate::loop_doc::LoopDoc;
use crate::midi::{flush_tap, flush_notes, MidiMsg, TimedMidi, TimedTap};
use crate::song::SongDriver;
use crate::telemetry::{Applied, BlockTelemetry, ConstraintStatus, LoopRecord, OrreryTelemetry, SongStamp, SongTelemetry, Telemetry, TrackStatus, AUDIBLE_EPS};
use rtrb::{Consumer, Producer, RingBuffer};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long [`Session::join`]/`Drop` waits for the worker to exit before detaching it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

mod live;
mod worker;
use worker::{apply_doc_swap, flush_midi_on_exit, render_next, song_loop_top, take_pending_doc, FrameLayout, Routed, WorkerIo};

/// Where each audio source plays on a multichannel device (ADR 0044): stereo pair `k` (device
/// channels `2k`, `2k + 1`, 0-based) carries `slots[k]`'s dry/wet, and pair `residual` additionally
/// carries whatever no slot does — master − Σ slotted pairs (a source a live edit added, a source
/// past the device width, a retiring bus tail) — so the pairs always sum to the master. The
/// residual pair is normally an empty "overflow" slot; it may share a slotted pair when the device
/// is full. Live-only: offline renders never route.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceRouting {
    /// Device frame width (channels); even.
    pub width: usize,
    /// One entry per stereo pair (`width / 2` of them): the source it carries, or `None`.
    pub slots: Vec<Option<SourceKey>>,
    /// The pair that carries the residual.
    pub residual: usize,
}

impl SourceRouting {
    /// `keys` on consecutive pairs from 1/2, the residual on the first pair left empty — or, when
    /// `keys` fill (or overflow) the device, sharing the last pair. Sources past the last pair ride
    /// the residual.
    pub fn sequential(keys: &[SourceKey], width: usize) -> Self {
        let pairs = (width / 2).max(1);
        let slots: Vec<Option<SourceKey>> = (0..pairs).map(|i| keys.get(i).cloned()).collect();
        let residual = if keys.len() < pairs { keys.len() } else { pairs - 1 };
        Self { width: pairs * 2, slots, residual }
    }

    /// Reject a routing that can't describe a device frame: an odd or zero width, a slot count that
    /// isn't `width / 2`, a residual past the last pair, or a source on two pairs.
    pub fn validate(&self) -> Result<(), String> {
        if self.width < 2 || !self.width.is_multiple_of(2) {
            return Err(format!("routing width {} must be even and at least 2", self.width));
        }
        if self.slots.len() != self.width / 2 {
            return Err(format!("routing has {} slots for {} channels (want {})", self.slots.len(), self.width, self.width / 2));
        }
        if self.residual >= self.slots.len() {
            return Err(format!("residual pair {} is past the last pair {}", self.residual, self.slots.len() - 1));
        }
        let mut seen = std::collections::BTreeSet::new();
        for key in self.slots.iter().flatten() {
            if !seen.insert(key) {
                return Err(format!("{key} is routed to two pairs"));
            }
        }
        Ok(())
    }
}

/// What a session starts with beyond the `Copy` [`SessionConfig`]: a device routing (ADR 0044), the
/// instruments whose notes are mirrored as MIDI (ADR 0048, drained with
/// [`Session::poll_note_tap`]), and whether each [`LoopRecord`] carries its loop's note log.
#[derive(Clone, Debug, Default)]
pub struct SessionOptions {
    pub routing: Option<SourceRouting>,
    pub note_tap: BTreeSet<String>,
    pub note_log: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct SessionConfig {
    pub sample_rate: f64,
    /// `Some(n)` runs `n` loops then finishes; `None` runs until [`Session::stop`]. A doc with a
    /// `song` (ADR 0029) always finishes when the song does — this can only cut it shorter.
    pub max_loops: Option<usize>,
    /// Audio ring capacity in interleaved samples (frames × channels, stereo-denominated). The
    /// default buffers ~22 s of stereo @ 48 k — the render lookahead, and therefore how long a
    /// control or doc edit takes to reach the speakers.
    pub ring_capacity: usize,
    /// Device frame width the audio ring carries (ADR 0027). `2` (default) streams the master
    /// mix verbatim — the classic stereo path, byte-identical to before. `>2` streams composed
    /// multichannel frames: device pair 1/2 = rest-of-mix, each instrument `out_channel` = its
    /// stereo pair (folding into 1/2 when it doesn't fit). Set by the shell to the opened output
    /// device's channel count.
    pub out_channels: usize,
    /// Stem export (ADR 0030): stream `2 + 2N`-wide frames — the exact master on 1/2, then one
    /// stereo pair per audio instrument + internal bus, in [`Session::source_layout`] order (fixed
    /// at start). Overrides `out_channels`. Default `false`.
    pub source_stems: bool,
    /// The shell's revision number of the starting doc, stamped on every [`LoopRecord`] until a
    /// [`Session::apply_doc_rev`] swap replaces it (ADR 0034). Default 0 (untracked).
    pub doc_rev: u64,
    /// **Live mode** (ADR 0035): `Some(n)` renders each loop in `n`-frame blocks and applies a
    /// pending doc edit mid-loop (fork + retarget + crossfade), emitting [`Applied`] and
    /// [`BlockTelemetry`]; pair it with a small `ring_capacity` (≈ 8192 frames) for low latency.
    /// `None` (default) = the whole-loop worker — every offline render, bit-reproducible.
    pub live_block: Option<usize>,
    /// Song audition (ADR 0036): start a doc's `song` at this block (0 = the beginning).
    pub song_start_block: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { sample_rate: 48_000.0, max_loops: None, ring_capacity: 1 << 21, out_channels: 2, source_stems: false, doc_rev: 0, live_block: None, song_start_block: 0 }
    }
}

/// Bounded queue depth for the live-viz frame channel (ADR 0024). Large enough to hold a couple of
/// loops of 25 Hz hops so a momentarily-busy TUI doesn't drop frames; `try_send` drops on overflow so
/// the worker is never gated by the UI (the viz is explicitly best-effort).
const VIZ_QUEUE: usize = 2048;

/// Bounded queue depth for live-mode block telemetry (a few seconds of 1024-frame blocks); dropped on
/// overflow — meters are best-effort.
const BLOCK_QUEUE: usize = 512;

/// One 25 Hz (40 ms) live-viz frame for the instrument screen (ADR 0024): raw mono audio for the
/// master + selected-instrument-dry spectrograms, plus a normalized effective-param snapshot for the
/// oscilloscope. Produced by the render worker (off the audio / determinism path), drained by
/// [`Session::poll_viz`]. Each frame carries its absolute [`audio_pos`](VizFrame::audio_pos) so the
/// TUI can reveal it in sync with playback (the spectrogram follows the play cursor, not render).
#[derive(Clone, Debug)]
pub struct VizFrame {
    pub loop_id: u64,
    /// Index of this hop within the loop (0-based).
    pub hop_index: u32,
    /// Total audio frames in the loop this hop belongs to (loop length can change under live-edit).
    pub num_frames: usize,
    /// Absolute audio-frame offset of this hop's first sample, counted from session start (the same
    /// origin the cpal callback counts played frames from). The TUI reveals a hop only once playback
    /// reaches this position, so the spectrogram follows the **play** cursor, not the render cursor.
    pub audio_pos: u64,
    /// Raw mono master audio for this hop (≤ 1920 samples @48k; the final hop may be short).
    pub master: Vec<f32>,
    /// Raw mono dry audio of the selected instrument (empty if none selected).
    pub inst_dry: Vec<f32>,
    /// Normalized effective (post-mod) param values keyed by JSONPath (the modulated set).
    pub eff: BTreeMap<String, f32>,
}

/// A live streaming run. Drop or [`stop`](Session::stop) to end it; [`join`](Session::join) to
/// block until the worker has exited.
pub struct Session {
    /// `None` once [`take_consumer`](Session::take_consumer) has moved the audio consumer
    /// onto a foreign audio thread (e.g. a cpal callback); [`pull_audio`](Session::pull_audio)
    /// then no-ops.
    cons: Option<Consumer<f32>>,
    /// One [`LoopRecord`] per rendered loop, drained by [`poll_records`](Session::poll_records).
    records_rx: mpsc::Receiver<LoopRecord>,
    stop: Arc<AtomicBool>,
    /// Set by the render worker once it has stopped producing audio (a bounded run ended, `stop`,
    /// or a panic — see [`worker_failure`](Session::worker_failure)).
    render_done: Arc<AtomicBool>,
    /// The render worker's panic message, if it died mid-run.
    failure: Arc<Mutex<Option<String>>>,
    worker: Option<thread::JoinHandle<()>>,
    channels: u32,
    /// The per-source stem layout when `SessionConfig::source_stems` is on (ADR 0030).
    source_layout: Option<Arc<[SourceKey]>>,
    /// The device routing when started with [`Session::start_routed`] (ADR 0044).
    routing: Option<SourceRouting>,
    /// Runtime control edits (Phase 1): set by [`apply_control`](Session::apply_control), taken by
    /// the render worker at the next loop boundary (re-base, never a jump).
    pending_control: Arc<Mutex<Option<(SessionControl, LiveToggle)>>>,
    /// Pending wholesale loop-doc swap (LiveEdit author mode, ADR 0024): set by
    /// [`apply_doc`](Session::apply_doc), taken by the worker at the next loop boundary. Same-id
    /// edits swap in place (seamless); structural edits crossfade a standby engine.
    pending_doc: Arc<Mutex<Option<(LoopDoc, Option<u64>)>>>,
    /// Latest read-only telemetry snapshot, written by the worker each loop (Phase 1).
    telemetry: Arc<Mutex<Option<Telemetry>>>,
    /// Live-viz frame stream (ADR 0024), drained by [`poll_viz`](Session::poll_viz).
    viz_rx: mpsc::Receiver<VizFrame>,
    /// Toggles the engine's viz tap (instrument screen on/off); read by the worker each loop.
    viz_enabled: Arc<AtomicBool>,
    /// Which instrument's dry hops the viz tap captures; read by the worker each loop.
    viz_inst: Arc<Mutex<Option<String>>>,
    /// MIDI-out event stream (ADR 0026): the played loop's events re-based onto the absolute
    /// sample clock, drained by [`poll_midi`](Session::poll_midi). Empty unless the doc has
    /// `type:"midi"` instruments.
    midi_rx: mpsc::Receiver<TimedMidi>,
    /// The MIDI mirror's stream (ADR 0048), drained by [`poll_note_tap`](Session::poll_note_tap).
    tap_rx: mpsc::Receiver<TimedTap>,
    /// The doc's `external_return` bus config `(return amplitude, latency_ms)`, if authored — the shell
    /// monitor-mixes that DAW input (ADR 0026 phase D); no audio enters the core render.
    ext_return: Option<(f32, f32)>,
    /// Audio frames the worker has pushed into the ring so far (ADR 0029: lets a shell that took
    /// the consumer play out the buffered end of a song before it exits).
    produced: Arc<AtomicU64>,
    /// Live mode (ADR 0035): applied mid-loop edits and per-block telemetry.
    applied_rx: mpsc::Receiver<Applied>,
    blocks_rx: mpsc::Receiver<BlockTelemetry>,
    /// Live gains (mute/solo): taken per block in live mode, per loop otherwise.
    pending_gains: Arc<Mutex<Option<BTreeMap<String, f32>>>>,
}

impl Session {
    /// Spawn the render worker and begin streaming. `control: None` falls back to the doc's own
    /// control settings (`optimize` / `movement` / `constraints`, ADR 0033), else plain drift.
    pub fn start(doc: LoopDoc, cfg: SessionConfig, dither: DitherConfig, control: Option<SessionControl>) -> Self {
        Self::start_inner(doc, cfg, dither, control, SessionOptions::default())
    }

    /// [`Session::start`] with [`SessionOptions`]: a routing (as [`Session::start_routed`]), a MIDI
    /// mirror, a note log. Fails only on an invalid routing.
    pub fn start_with(doc: LoopDoc, cfg: SessionConfig, dither: DitherConfig, control: Option<SessionControl>, opts: SessionOptions) -> Result<Self, String> {
        if let Some(r) = &opts.routing {
            r.validate()?;
        }
        Ok(Self::start_inner(doc, cfg, dither, control, opts))
    }

    /// [`Session::start`] streaming **routed device frames** (ADR 0044): `routing.width`-wide
    /// frames, each source on its slot's pair and the residual on the overflow pair (see
    /// [`SourceRouting`]). Overrides `out_channels` and `source_stems`. The master math is
    /// untouched — the sources are the ADR 0030 observation tap. Live-shell only.
    pub fn start_routed(doc: LoopDoc, cfg: SessionConfig, dither: DitherConfig, control: Option<SessionControl>, routing: SourceRouting) -> Result<Self, String> {
        Self::start_with(doc, cfg, dither, control, SessionOptions { routing: Some(routing), ..Default::default() })
    }

    fn start_inner(doc: LoopDoc, cfg: SessionConfig, dither: DitherConfig, control: Option<SessionControl>, opts: SessionOptions) -> Self {
        let SessionOptions { routing, note_tap, note_log } = opts;
        let control = control.or_else(|| SessionControl::from_doc(&doc));
        // Song form (ADR 0029): the driver owns the block sequence; the controller starts on block 0
        // with block 0's seed (= the session seed unless the block names one), and the run ends when
        // the song does. A song that fails to materialize (only possible for docs built in code —
        // `from_json_str` validates it) degrades to the base doc with a warning.
        let mut doc = doc;
        let song = doc.take_song().and_then(|spec| {
            SongDriver::new(&doc, &spec, dither.seed, cfg.sample_rate)
                .map(|d| d.start_at(cfg.song_start_block))
                .map_err(|e| eprintln!("engines_core::session: song disabled: {e}"))
                .ok()
        });
        let (doc, dither) = match &song {
            Some(s) => (s.block_doc(s.first_block()).clone(), DitherConfig { seed: s.block_seed(s.first_block()), ..dither }),
            None => (doc, dither),
        };
        let max_loops = match &song {
            Some(s) => Some(cfg.max_loops.map_or(s.total_loops(), |m| m.min(s.total_loops()))),
            None => cfg.max_loops,
        };
        let produced = Arc::new(AtomicU64::new(0));
        // What a ring frame carries: device frames (ADR 0027), the master + per-source stems
        // (ADR 0030; a song's blocks share block 0's instruments/buses, so its layout holds), or
        // routed device frames (ADR 0044).
        let layout = if let Some(r) = routing.clone() {
            if cfg.source_stems {
                eprintln!("engines_core::session: a routing overrides source_stems");
            }
            FrameLayout::Routed(Arc::new(Routed::new(r)))
        } else if cfg.source_stems {
            if cfg.out_channels > 2 {
                eprintln!("engines_core::session: source_stems overrides out_channels={}", cfg.out_channels);
            }
            FrameLayout::Sources(source_layout(&doc).into())
        } else {
            FrameLayout::Device(cfg.out_channels)
        };
        let source_keys = match &layout {
            FrameLayout::Sources(keys) => Some(keys.clone()),
            FrameLayout::Device(_) | FrameLayout::Routed(_) => None,
        };
        // Ring sized in FRAMES from the stereo-denominated `ring_capacity`, then scaled to the
        // frame width so the playback lead time is width-independent.
        let width = layout.width();
        let (prod, cons) = RingBuffer::<f32>::new((cfg.ring_capacity.max(4096) / 2) * width);
        // Unbounded like the MIDI stream: the worker never blocks on it, and a shell that doesn't
        // poll just accumulates a small record per loop.
        let (records_tx, records_rx) = mpsc::channel::<LoopRecord>();
        let stop = Arc::new(AtomicBool::new(false));
        let render_done = Arc::new(AtomicBool::new(false));
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let pending_control: Arc<Mutex<Option<(SessionControl, LiveToggle)>>> = Arc::new(Mutex::new(None));
        let pending_doc: Arc<Mutex<Option<(LoopDoc, Option<u64>)>>> = Arc::new(Mutex::new(None));
        let telemetry: Arc<Mutex<Option<Telemetry>>> = Arc::new(Mutex::new(None));
        let (viz_tx, viz_rx) = mpsc::sync_channel::<VizFrame>(VIZ_QUEUE);
        let viz_enabled = Arc::new(AtomicBool::new(false));
        let viz_inst: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let (midi_tx, midi_rx) = mpsc::channel::<TimedMidi>();
        let (tap_tx, tap_rx) = mpsc::channel::<TimedTap>();
        let (applied_tx, applied_rx) = mpsc::channel::<Applied>();
        let (blocks_tx, blocks_rx) = mpsc::sync_channel::<BlockTelemetry>(BLOCK_QUEUE);
        let pending_gains: Arc<Mutex<Option<BTreeMap<String, f32>>>> = Arc::new(Mutex::new(None));
        let ext_return = doc.buses.values().find(|b| b.is_external_return()).map(|b| (b.return_amp(), b.latency_ms));
        // A song started at a later block numbers its loops as the full song does there.
        let base_loop_id = doc.loop_id + song.as_ref().map_or(0, |s| s.offset() as u64);
        let sr = cfg.sample_rate;

        let mut io = WorkerIo {
            prod,
            layout,
            stop: stop.clone(),
            telemetry: telemetry.clone(),
            viz_tx,
            midi_tx,
            tap_tx,
            note_tap,
            note_log,
            records_tx,
            frames_produced: 0,
            doc_rev: cfg.doc_rev,
            applied_tx,
            blocks_tx,
        };
        let live_block = cfg.live_block;
        let pending_gains_w = pending_gains.clone();
        let render_done_w = render_done.clone();
        let failure_w = failure.clone();
        let pending_w = pending_control.clone();
        let pending_doc_w = pending_doc.clone();
        let viz_enabled_w = viz_enabled.clone();
        let viz_inst_w = viz_inst.clone();
        let produced_w = produced.clone();
        let mut song = song;
        // The body runs under `catch_unwind` so a panic still sets `render_done` (and records why):
        // every host waits on that flag, and an unwound worker would otherwise leave them hanging.
        let worker = thread::spawn(move || {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                #[cfg(test)]
                if base_loop_id == tests::PANIC_LOOP_ID {
                    panic!("test-only render worker panic");
                }
                if let Some(block) = live_block {
                    let args = live::LiveArgs {
                        block: block.max(1),
                        max_loops,
                        base_loop_id,
                        dither,
                        sr,
                        pending_control: pending_w,
                        pending_doc: pending_doc_w,
                        pending_gains: pending_gains_w,
                        viz_enabled: viz_enabled_w,
                        viz_inst: viz_inst_w,
                        produced: produced_w.clone(),
                    };
                    let snap = live::run(&mut io, doc, control, song, args);
                    flush_midi_on_exit(&snap, &io);
                    produced_w.store(io.frames_produced, Ordering::Release);
                    return;
                }
                // The doc as last authored (with its own control settings) — a live edit that changes
                // its movement/constraints updates the session control (ADR 0033).
                let mut authored = doc.clone();
                let mut ctl = Controller::new(doc, dither);
                ctl.set_control(control);
                let mut eng = Engine::new(sr);
                io.arm(&mut eng);
                let mut snap = PhaseSnapshot::empty();
                let mut n = 0usize;
                let mut warned_doc_swap = false;
                while !io.stop.load(Ordering::Acquire) {
                    if let Some(max) = max_loops {
                        if n >= max {
                            break;
                        }
                    }
                    let loop_id = base_loop_id + n as u64;
                    if let Ok(mut g) = pending_w.lock() {
                        if let Some((c, t)) = g.take() {
                            ctl.set_live_control(Some(c), t);
                        }
                    }
                    // Song form (ADR 0029): a block start resets the controller + arms the seam glide;
                    // after a control edit so an edit landing on a boundary folds into the new block.
                    produced_w.store(io.frames_produced, Ordering::Release);
                    let song_t = song_loop_top(song.as_mut(), n, &mut ctl, &mut snap);
                    // Before a doc swap, so a structural swap's standby engine inherits them.
                    if let Some(g) = pending_gains_w.lock().ok().and_then(|mut g| g.take()) {
                        eng.set_live_gains(g);
                    }
                    // LiveEdit: a hand-edited loop doc swaps in at the boundary (ADR 0024). Same-id
                    // edits swap in place (seamless); structural edits crossfade a standby engine,
                    // which renders + pushes the transition loop itself, so we skip render_next.
                    if let Some((new_doc, rev)) = take_pending_doc(&pending_doc_w, song.is_some(), &mut warned_doc_swap) {
                        if let Some(r) = rev {
                            io.doc_rev = r;
                        }
                        if let Some(c) = SessionControl::with_doc_settings(ctl.control_ref(), &authored, &new_doc) {
                            ctl.set_control(Some(c));
                        }
                        authored = new_doc.clone();
                        match apply_doc_swap(&mut eng, &mut ctl, new_doc, sr, &mut snap, &mut io, loop_id) {
                            Ok(Some((new_eng, new_ctl))) => {
                                eng = new_eng;
                                ctl = new_ctl;
                                n += 1;
                                continue;
                            }
                            Ok(None) => {}
                            Err(()) => break,
                        }
                    }
                    eng.set_viz(viz_enabled_w.load(Ordering::Relaxed));
                    eng.set_viz_instrument(viz_inst_w.lock().ok().and_then(|g| g.clone()));
                    if render_next(&mut ctl, &mut eng, &mut snap, &mut io, loop_id, song_t).is_err() {
                        break;
                    }
                    n += 1;
                }
                flush_midi_on_exit(&snap, &io);
                produced_w.store(io.frames_produced, Ordering::Release);
            }));
            if let Err(payload) = run {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|m| m.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                if let Ok(mut f) = failure_w.lock() {
                    *f = Some(msg);
                }
            }
            render_done_w.store(true, Ordering::Release);
        });

        Self {
            cons: Some(cons),
            records_rx,
            stop,
            render_done,
            failure,
            worker: Some(worker),
            channels: width as u32,
            source_layout: source_keys,
            routing,
            pending_control,
            pending_doc,
            telemetry,
            viz_rx,
            viz_enabled,
            viz_inst,
            midi_rx,
            tap_rx,
            ext_return,
            produced,
            applied_rx,
            blocks_rx,
            pending_gains,
        }
    }

    /// Interleaved frame width of the streamed audio: `2` (the master mix) unless the shell
    /// asked for composed multichannel frames via `SessionConfig::out_channels` (ADR 0027), or for
    /// `2 + 2N` master + per-source frames via `SessionConfig::source_stems` (ADR 0030).
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// With `SessionConfig::source_stems` (ADR 0030): the source on each stereo pair after the
    /// master — pair `i` (channels `2 + 2i`, `3 + 2i`, 0-based) carries `source_layout()[i]`.
    /// `None` otherwise.
    pub fn source_layout(&self) -> Option<&[SourceKey]> {
        self.source_layout.as_deref()
    }

    /// The device routing this session streams (ADR 0044), when started with
    /// [`Session::start_routed`].
    pub fn routing(&self) -> Option<&SourceRouting> {
        self.routing.as_ref()
    }

    /// Fill `out` with up to `out.len()` interleaved samples; returns the count written
    /// (0 means the ring is momentarily empty, or the consumer was taken). RT-safe: pops
    /// only, never blocks/allocs.
    pub fn pull_audio(&mut self, out: &mut [f32]) -> usize {
        let Some(cons) = self.cons.as_mut() else { return 0 };
        let mut i = 0;
        while i < out.len() {
            match cons.pop() {
                Ok(s) => {
                    out[i] = s;
                    i += 1;
                }
                Err(_) => break,
            }
        }
        i
    }

    /// Move the audio consumer out so it can live on a foreign audio thread (e.g. a cpal
    /// output callback). Afterward [`pull_audio`](Session::pull_audio) no-ops and the ring is
    /// no longer observable to [`finished`](Session::finished); use
    /// [`worker_finished`](Session::worker_finished) + your own drain for shutdown.
    pub fn take_consumer(&mut self) -> Option<Consumer<f32>> {
        self.cons.take()
    }

    /// Audio frames (device frames, any width) the worker has pushed into the ring so far —
    /// published at each loop boundary and on exit. A shell that took the consumer compares its own
    /// frames-played counter against this to play out the buffered end of a bounded run / song
    /// (ADR 0029) instead of cutting it off.
    pub fn frames_produced(&self) -> u64 {
        self.produced.load(Ordering::Acquire)
    }

    /// True once the worker has stopped producing audio (a bounded run ended, or `stop`) — the
    /// signal for "let the device play out the tail."
    pub fn worker_finished(&self) -> bool {
        self.render_done.load(Ordering::Acquire)
    }

    /// Why the render worker died, if it panicked (it then reports [`worker_finished`] /
    /// [`finished`] like a run that ended, so a host must check this to tell a failure from an end).
    ///
    /// [`worker_finished`]: Session::worker_finished
    /// [`finished`]: Session::finished
    pub fn worker_failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|f| f.clone())
    }

    /// Drain the [`LoopRecord`]s emitted since the last poll — one per rendered loop, in order.
    /// Not called on the audio thread.
    pub fn poll_records(&mut self) -> Vec<LoopRecord> {
        self.records_rx.try_iter().collect()
    }

    /// Drain the MIDI-out events emitted since the last poll (ADR 0026), each stamped with the
    /// absolute sample position its loop's audio occupies. The shell holds each message until
    /// its frames-played counter reaches `abs_frame`, then sends it to the OS MIDI port — never
    /// called on the audio thread. A session-end flush appends Note-Offs + CC 123 per used channel
    /// so external synths are never left hanging.
    pub fn poll_midi(&mut self) -> Vec<TimedMidi> {
        self.midi_rx.try_iter().collect()
    }

    /// Drain the MIDI mirror's notes (ADR 0048) emitted since the last poll, on the same absolute
    /// sample clock as [`poll_midi`](Session::poll_midi), tagged with their instrument (the shell
    /// picks the port and channel). A session-end flush appends a Note-Off for every note held.
    pub fn poll_note_tap(&mut self) -> Vec<TimedTap> {
        self.tap_rx.try_iter().collect()
    }

    /// The doc's `external_return` bus config `(return amplitude, latency_ms)` if authored — what
    /// the shell needs to monitor-mix the loopback input and decide to open it at all.
    pub fn external_return(&self) -> Option<(f32, f32)> {
        self.ext_return
    }

    /// Replace the live session control (CE-R8.2), applied atomically at the **next loop boundary**
    /// with re-base (Phase 1); the bumped `revision` is recorded in telemetry and each
    /// [`LoopRecord`].
    pub fn apply_control(&self, control: SessionControl) {
        self.apply_live_control(control, LiveToggle::default());
    }

    /// [`apply_control`](Self::apply_control) from a live panel: `toggle` names the switches the
    /// user just flipped (hold ⇄ drift, the Orrery), which beat a song block's own control until the
    /// next block starts (ADR 0055 D3). Toggles queued before the worker takes them add up.
    pub fn apply_live_control(&self, control: SessionControl, toggle: LiveToggle) {
        if let Ok(mut g) = self.pending_control.lock() {
            let toggle = g.take().map_or(toggle, |(_, earlier)| earlier.merged(toggle));
            *g = Some((control, toggle));
        }
    }

    /// Queue a wholesale loop-doc swap (LiveEdit author mode, ADR 0024), applied by the worker at the
    /// next loop boundary. Same-id edits swap in place seamlessly (DSP tails carry); structural edits
    /// crossfade a fresh standby engine. Only meaningful in `OptimizeMode::LiveEdit` (the controller
    /// is otherwise driving the operating point itself); a no-op effect if the worker isn't watching.
    pub fn apply_doc(&self, doc: LoopDoc) {
        if let Ok(mut g) = self.pending_doc.lock() {
            *g = Some((doc, None));
        }
    }

    /// [`Session::apply_doc`] with the shell's revision number for the doc: every [`LoopRecord`]
    /// from the swap on carries `doc_rev = rev`, so the shell knows when the edit is playing.
    pub fn apply_doc_rev(&self, doc: LoopDoc, rev: u64) {
        if let Ok(mut g) = self.pending_doc.lock() {
            *g = Some((doc, Some(rev)));
        }
    }

    /// Live gains (mute/solo, ADR 0035): instrument id → gain (0 = muted), ramped over ~10 ms.
    /// Instruments not named play at 1. Applied at the next block in live mode, else the next loop.
    pub fn set_live_gains(&self, gains: BTreeMap<String, f32>) {
        if let Ok(mut g) = self.pending_gains.lock() {
            *g = Some(gains);
        }
    }

    /// Drain the mid-loop edits applied since the last call (live mode).
    pub fn poll_applied(&mut self) -> Vec<Applied> {
        self.applied_rx.try_iter().collect()
    }

    /// Drain the per-block telemetry since the last call (live mode; best-effort, oldest first).
    pub fn poll_blocks(&mut self) -> Vec<BlockTelemetry> {
        self.blocks_rx.try_iter().collect()
    }

    /// Enable/disable the live-viz tap (ADR 0024) — the worker applies it to the engine at the next
    /// loop boundary. Off (default) ⇒ no viz frames produced and renders stay byte-identical.
    pub fn set_viz(&self, on: bool) {
        self.viz_enabled.store(on, Ordering::Relaxed);
    }

    /// Select which instrument's raw dry hops the viz tap captures (bandwidth control); `None` ⇒
    /// master + effective only. Applied by the worker at the next loop boundary.
    pub fn set_viz_instrument(&self, id: Option<String>) {
        if let Ok(mut g) = self.viz_inst.lock() {
            *g = id;
        }
    }

    /// Drain the live-viz frame stream (ADR 0024) — the instrument screen calls this each tick and
    /// advances its spectrograms/oscilloscope by the frames returned (oldest first).
    pub fn poll_viz(&mut self) -> Vec<VizFrame> {
        self.viz_rx.try_iter().collect()
    }

    /// Latest read-only telemetry snapshot (operating point, constraint status, Orrery state, track
    /// audibility, song position). `None` until the first loop has rendered.
    pub fn poll_telemetry(&self) -> Option<Telemetry> {
        self.telemetry.lock().ok().and_then(|g| g.clone())
    }

    /// True once the worker has finished **and** the ring is drained. If the consumer was taken
    /// (cpal path), the ring is no longer observable, so this reports on the worker only.
    pub fn finished(&self) -> bool {
        self.render_done.load(Ordering::Acquire) && self.cons.as_ref().is_none_or(|c| c.is_empty())
    }

    /// Signal the worker to stop at the next loop boundary / push point.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Stop and join the worker (bounded — see [`SHUTDOWN_GRACE`]).
    pub fn join(mut self) {
        self.shutdown();
    }

    /// Signal stop and join the worker, bounded by [`SHUTDOWN_GRACE`]; a worker that doesn't exit
    /// in time is detached with a logged warning, and one that panicked is surfaced — neither hangs
    /// shutdown.
    fn shutdown(&mut self) {
        self.stop();
        if let Some(w) = self.worker.take() {
            join_bounded(w, &self.render_done, "render worker");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Join `handle`, waiting at most [`SHUTDOWN_GRACE`] for it to signal `exited`; if it doesn't,
/// detach it with a warning so shutdown can't block forever. Surfaces a panic in the joined thread
/// instead of swallowing it.
fn join_bounded(handle: thread::JoinHandle<()>, exited: &AtomicBool, label: &str) {
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    while !exited.load(Ordering::Acquire) && !handle.is_finished() {
        if Instant::now() >= deadline {
            eprintln!("engines_core::session: {label} did not exit within {SHUTDOWN_GRACE:?}; detaching");
            return; // detach: drop the handle without joining
        }
        thread::sleep(Duration::from_millis(5));
    }
    if handle.join().is_err() {
        eprintln!("engines_core::session: {label} panicked during the run");
    }
}

#[cfg(test)]
mod tests;
