//! The **live-mode** render worker (ADR 0035): the loop is rendered a block at a time into a small
//! ring, and a pending doc edit is applied **mid-loop** — the engine is forked, the running loop
//! [`retarget`](crate::engine::LoopRender::retarget)ed onto the new doc, and the old and new renders
//! crossfaded (equal-gain raised cosine: 30 ms for a value edit, 250 ms for a structural one; reverb
//! tails survive because the buses are cloned). Each edit emits an [`Applied`]; each block a
//! [`BlockTelemetry`]; MIDI goes out block by block (the ring is too short to hold a loop's worth).
//!
//! Live-only: offline renders use the whole-loop worker and stay bit-reproducible (the live path's
//! edit timing depends on wall-clock arrival, which is the point).

use super::*;
use crate::engine::{EditKind, LoopRender};
use crate::telemetry::{Applied, BlockTelemetry, SongStamp};
use worker::{compose_parts, emit_loop, take_pending_doc, LoopEmit, RoutedFrames};

/// Value-edit crossfade (s) — short enough to be immediate, long enough to hide a step.
const XFADE_VALUE_S: f64 = 0.030;
/// Structural-edit crossfade (s) — masks a voice appearing/vanishing or a bus swap.
const XFADE_STRUCTURAL_S: f64 = 0.250;

/// The old side of a crossfade in progress.
struct Fade {
    lr: LoopRender,
    eng: Engine,
    len: usize,
    done: usize,
}

/// Shared handles the live worker reads each block.
pub(super) struct LiveArgs {
    pub(super) block: usize,
    pub(super) max_loops: Option<usize>,
    pub(super) base_loop_id: u64,
    pub(super) dither: DitherConfig,
    pub(super) sr: f64,
    pub(super) pending_control: Arc<Mutex<Option<(SessionControl, LiveToggle)>>>,
    pub(super) pending_doc: Arc<Mutex<Option<(LoopDoc, Option<u64>)>>>,
    pub(super) pending_gains: Arc<Mutex<Option<BTreeMap<String, f32>>>>,
    pub(super) viz_enabled: Arc<AtomicBool>,
    pub(super) viz_inst: Arc<Mutex<Option<String>>>,
    pub(super) produced: Arc<AtomicU64>,
}

/// Composes a [`LoopRender`]'s device frames for one block; built after the block renders (a
/// routed layout looks its slot buffers up once here, not per frame).
enum Framer<'a> {
    Plain(&'a LoopRender, &'a FrameLayout),
    Routed(RoutedFrames<'a>),
}

impl<'a> Framer<'a> {
    fn new(lr: &'a LoopRender, layout: &'a FrameLayout) -> Self {
        match layout {
            FrameLayout::Routed(rt) => Framer::Routed(RoutedFrames::new(lr.audio(), lr.sources(), rt)),
            _ => Framer::Plain(lr, layout),
        }
    }

    /// One composed device frame at loop frame `f` (`layout`-wide).
    fn frame(&self, f: usize, out: &mut [f32]) {
        match self {
            Framer::Routed(rf) => rf.frame(f, out),
            Framer::Plain(lr, FrameLayout::Sources(keys)) => {
                out[0] = lr.audio()[2 * f];
                out[1] = lr.audio()[2 * f + 1];
                for (i, k) in keys.iter().enumerate() {
                    let (l, r) = lr.sources().and_then(|s| s.get(k)).map_or((0.0, 0.0), |b| (b[2 * f], b[2 * f + 1]));
                    out[2 + 2 * i] = l;
                    out[3 + 2 * i] = r;
                }
            }
            Framer::Plain(lr, _) => compose_parts(lr.audio(), lr.stems(), f, out),
        }
    }
}

/// Run the live worker until stop / `max_loops` / a song's end; returns the last snapshot (for the
/// exit MIDI flush).
pub(super) fn run(io: &mut WorkerIo, doc: LoopDoc, control: Option<SessionControl>, mut song: Option<SongDriver>, a: LiveArgs) -> PhaseSnapshot {
    let mut authored = doc.clone();
    let mut ctl = Controller::new(doc, a.dither);
    ctl.set_control(control);
    let mut eng = Engine::new(a.sr);
    io.arm(&mut eng);
    eng.set_metering(true);
    let mut snap = PhaseSnapshot::empty();
    let mut warned_doc_swap = false;
    let width = io.layout.width();
    // Fixed for the session (cheap: the source lists are `Arc`s); a copy so composing can borrow it
    // while frames are pushed through `io`.
    let layout = io.layout.clone();
    let (mut fa, mut fb) = (vec![0.0f32; width], vec![0.0f32; width]);
    let mut n = 0usize;
    'loops: while !io.stop.load(Ordering::Acquire) {
        if a.max_loops.is_some_and(|max| n >= max) {
            break;
        }
        let loop_id = a.base_loop_id + n as u64;
        if let Ok(mut g) = a.pending_control.lock() {
            if let Some((c, t)) = g.take() {
                ctl.set_live_control(Some(c), t);
            }
        }
        a.produced.store(io.frames_produced, Ordering::Release);
        let song_t = song_loop_top(song.as_mut(), n, &mut ctl, &mut snap);
        eng.set_viz(a.viz_enabled.load(Ordering::Relaxed));
        eng.set_viz_instrument(a.viz_inst.lock().ok().and_then(|g| g.clone()));

        let p = ctl.next(loop_id);
        let op_value = serde_json::to_value(ctl.operating_point()).unwrap_or_default();
        let mut lr = eng.begin_loop(&p.doc, &snap);
        let mut played_doc = p.doc.clone();
        let loop_start = io.frames_produced;
        // Absolute frame of `lr`'s frame 0 (moves when a retarget remaps the position).
        let mut origin = loop_start as i64;
        let mut fade: Option<Fade> = None;
        let mut loop_peak = 0.0f32;

        while !lr.is_done() {
            // An edit lands at a block boundary (one at a time — a crossfade finishes first).
            if fade.is_none() {
                if let Some((new_doc, rev)) = take_pending_doc(&a.pending_doc, song.is_some(), &mut warned_doc_swap) {
                    if let Some(r) = rev {
                        io.doc_rev = r;
                    }
                    if let Some(c) = SessionControl::with_doc_settings(ctl.control_ref(), &authored, &new_doc) {
                        ctl.set_control(Some(c));
                    }
                    authored = new_doc.clone();
                    if doc_swap_is_structural(ctl.operating_point(), &new_doc) {
                        ctl = ctl.rebuilt(new_doc); // the Orrery's voyage carries over
                    } else {
                        ctl.swap_doc(new_doc);
                    }
                    let target = ctl.operating_point().clone();
                    let mut fork = eng.clone();
                    let (new_lr, kind) = lr.retarget(&mut fork, &target);
                    let secs = if kind == EditKind::Value { XFADE_VALUE_S } else { XFADE_STRUCTURAL_S };
                    let len = ((secs * a.sr) as usize).min(new_lr.total() - new_lr.pos()).min(lr.total() - lr.pos());
                    let _ = io.applied_tx.send(Applied { doc_rev: io.doc_rev, loop_id, abs_frame: io.frames_produced, pos: new_lr.pos(), kind, xfade_frames: len });
                    origin = io.frames_produced as i64 - new_lr.pos() as i64;
                    let old_lr = std::mem::replace(&mut lr, new_lr);
                    let old_eng = std::mem::replace(&mut eng, fork);
                    if len > 0 {
                        fade = Some(Fade { lr: old_lr, eng: old_eng, len, done: 0 });
                    }
                    played_doc = target;
                    if lr.is_done() {
                        break;
                    }
                }
            }
            if let Some(g) = a.pending_gains.lock().ok().and_then(|mut g| g.take()) {
                lr.set_gain_targets(&mut eng, &g);
                if let Some(f) = fade.as_mut() {
                    f.lr.set_gain_targets(&mut f.eng, &g);
                }
            }

            let block_abs = io.frames_produced;
            let want = fade.as_ref().map_or(a.block, |f| a.block.min(f.len - f.done));
            let range = lr.render_block(&mut eng, want);
            let old_start = fade.as_ref().map(|f| f.lr.pos());
            if let Some(f) = fade.as_mut() {
                f.lr.render_block(&mut f.eng, range.len());
            }
            let mut block_peak = [0.0f32; 2];
            let new_side = Framer::new(&lr, &layout);
            let old_side = fade.as_ref().map(|f| Framer::new(&f.lr, &layout));
            for (k, nf) in range.clone().enumerate() {
                new_side.frame(nf, &mut fb);
                let (mut ml, mut mr) = (lr.audio()[2 * nf], lr.audio()[2 * nf + 1]);
                if let (Some(f), Some(os), Some(old)) = (fade.as_ref(), old_start, old_side.as_ref()) {
                    let of = os + k;
                    old.frame(of, &mut fa);
                    let t = (f.done + k) as f64 / f.len as f64;
                    let g_new = (0.5 - 0.5 * (std::f64::consts::PI * t).cos()) as f32;
                    let g_old = 1.0 - g_new;
                    for c in 0..width {
                        fb[c] = g_old * fa[c] + g_new * fb[c];
                    }
                    ml = g_old * f.lr.audio()[2 * of] + g_new * ml;
                    mr = g_old * f.lr.audio()[2 * of + 1] + g_new * mr;
                }
                block_peak = [block_peak[0].max(ml.abs()), block_peak[1].max(mr.abs())];
                if io.push_frame(&fb).is_err() {
                    // Stopped mid-loop: this loop's MIDI already went out block by block, so the
                    // exit flush must see its held notes, not the loop-start snapshot's (ADR 0026).
                    snap.midi_out = lr.midi_states();
                    snap.note_tap = lr.tap_states();
                    break 'loops;
                }
            }
            io.frames_produced += range.len() as u64;
            let peak = block_peak[0].max(block_peak[1]);
            loop_peak = loop_peak.max(peak);
            for e in lr.take_new_midi() {
                let _ = io.midi_tx.send(TimedMidi { abs_frame: (origin + e.frame as i64).max(0) as u64, msg: e.msg });
            }
            for e in lr.take_new_taps() {
                let _ = io.tap_tx.send(TimedTap { abs_frame: (origin + e.frame as i64).max(0) as u64, inst: e.inst.clone(), msg: e.msg });
            }
            let _ = io.blocks_tx.try_send(BlockTelemetry {
                loop_id,
                abs_frame: block_abs,
                pos: range.start,
                total: lr.total(),
                frames: range.len(),
                peak,
                peak_lr: block_peak,
                meters: lr.take_meters(),
                song: song_t.as_ref().map(SongStamp::from),
            });
            if let Some(f) = fade.as_mut() {
                f.done += range.len();
                if f.done >= f.len {
                    fade = None;
                }
            }
            a.produced.store(io.frames_produced, Ordering::Release);
        }

        let mut r = lr.finish(&mut eng);
        emit_loop(
            &ctl,
            io,
            &mut r,
            LoopEmit { loop_id, loop_start, path: &p.path, delta: p.delta, harmonic_move: p.harmonic_move.clone(), categorical_move: p.categorical_move.clone(), song: song_t, op_value, peak: loop_peak },
        );
        ctl.rebase(played_doc);
        snap = r.next_snapshot;
        n += 1;
    }
    snap
}
