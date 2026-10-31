//! The render worker's per-loop free functions: device-frame composition (ADR 0027), the
//! per-loop render + emit, LiveEdit doc swaps, and the exit MIDI flush. The worker *loop* itself
//! stays in `Session::start` (session.rs).

use super::*;

/// Compose one device frame from a render (ADR 0027): with no stems, the master mix lands on
/// channels 1/2 (zeros beyond); with stems, 1/2 carries the rest-of-mix and each `out_channel`
/// stem its stereo pair — a stem whose pair doesn't fit `out.len()` folds back into 1/2, so the
/// music stays whole on a too-narrow device.
pub(super) fn compose_frame(r: &RenderResult, f: usize, out: &mut [f32]) {
    compose_parts(&r.audio, r.stems.as_ref(), f, out)
}

/// [`compose_frame`] over a render's parts (a whole loop's, or a [`crate::engine::LoopRender`]'s so far).
pub(super) fn compose_parts(audio: &[f32], stems: Option<&crate::engine::StemMix>, f: usize, out: &mut [f32]) {
    out.fill(0.0);
    match stems {
        None => {
            out[0] = audio[2 * f];
            out[1] = audio[2 * f + 1];
        }
        Some(sm) => {
            out[0] = sm.rest[2 * f];
            out[1] = sm.rest[2 * f + 1];
            for (&oc, buf) in &sm.stems {
                let c0 = oc as usize - 1; // 1-based pair start → 0-based index
                let (l, s_r) = (buf[2 * f], buf[2 * f + 1]);
                if c0 + 1 < out.len() {
                    out[c0] += l;
                    out[c0 + 1] += s_r;
                } else {
                    out[0] += l;
                    out[1] += s_r;
                }
            }
        }
    }
}

/// What one ring frame carries — fixed at session start.
#[derive(Clone, Debug)]
pub(super) enum FrameLayout {
    /// `n`-wide device frames (ADR 0027): the master on 1/2, or with `n > 2` the rest-of-mix on 1/2
    /// plus each `out_channel` stem on its pair ([`compose_frame`]).
    Device(usize),
    /// Stem export (ADR 0030): the exact master on 1/2, then one stereo pair per source in layout
    /// order. A source missing from a loop's render (e.g. an instrument disabled this loop) is
    /// silence on its pair; one absent from the layout (only a structural LiveEdit can add one)
    /// is dropped from the stems — the master pair stays whole either way.
    Sources(Arc<[SourceKey]>),
    /// Routed device frames (ADR 0044): each source on the device pair its [`SourceRouting`] slot
    /// names, and the residual — master − Σ slotted pairs — on the overflow pair.
    Routed(Arc<Routed>),
}

/// A [`SourceRouting`] with its slotted ids indexed for the per-block residual check.
#[derive(Debug)]
pub(super) struct Routed {
    pub(super) routing: SourceRouting,
    inst: std::collections::BTreeSet<String>,
    bus: std::collections::BTreeSet<String>,
}

impl Routed {
    pub(super) fn new(routing: SourceRouting) -> Self {
        let (mut inst, mut bus) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
        for key in routing.slots.iter().flatten() {
            match key {
                SourceKey::Inst(id) => inst.insert(id.clone()),
                SourceKey::Bus(id) => bus.insert(id.clone()),
            };
        }
        Self { routing, inst, bus }
    }
}

impl FrameLayout {
    pub(super) fn width(&self) -> usize {
        match self {
            FrameLayout::Device(n) => (*n).max(2),
            FrameLayout::Sources(keys) => 2 + 2 * keys.len(),
            FrameLayout::Routed(r) => r.routing.width,
        }
    }

    /// Arm `eng`'s observation taps for this layout (both off for plain stereo).
    pub(super) fn arm(&self, eng: &mut Engine) {
        eng.set_stem_capture(matches!(self, FrameLayout::Device(n) if *n > 2));
        eng.set_source_capture(matches!(self, FrameLayout::Sources(_) | FrameLayout::Routed(_)));
    }
}

/// A render's frames composed for a [`FrameLayout::Routed`], with each slot's buffer looked up
/// once (per loop in the whole-loop worker, per block in live mode) rather than per frame.
pub(super) struct RoutedFrames<'a> {
    master: &'a [f32],
    views: Vec<Option<&'a [f32]>>,
    /// The overflow pair, when this render has audio no slot carries: a source the routing doesn't
    /// slot (added by a live edit, or past the device width) or a retiring bus tail. `None` keeps
    /// the overflow pair exactly silent and every slotted pair bit-exact to its stem.
    residual: Option<usize>,
}

impl<'a> RoutedFrames<'a> {
    pub(super) fn new(master: &'a [f32], sources: Option<&'a SourceStems>, routed: &Routed) -> Self {
        let views = routed.routing.slots.iter().map(|slot| slot.as_ref().and_then(|k| sources.and_then(|s| s.get(k)))).collect();
        let unslotted = sources.is_none_or(|s| s.uncaptured || s.inst.keys().any(|id| !routed.inst.contains(id)) || s.bus.keys().any(|id| !routed.bus.contains(id)));
        Self { master, views, residual: unslotted.then_some(routed.routing.residual) }
    }

    /// Write frame `f` into `out` (the routing's width).
    pub(super) fn frame(&self, f: usize, out: &mut [f32]) {
        out.fill(0.0);
        let (mut sum_l, mut sum_r) = (0.0f32, 0.0f32);
        for (pair, view) in self.views.iter().enumerate() {
            if let Some(b) = view {
                let (l, r) = (b[2 * f], b[2 * f + 1]);
                out[2 * pair] = l;
                out[2 * pair + 1] = r;
                sum_l += l;
                sum_r += r;
            }
        }
        if let Some(pair) = self.residual {
            out[2 * pair] += self.master[2 * f] - sum_l;
            out[2 * pair + 1] += self.master[2 * f + 1] - sum_r;
        }
    }
}

/// One loop's frames composed for a [`FrameLayout`], with the source lookups resolved once per
/// loop rather than per frame.
pub(super) struct Composer<'a> {
    r: &'a RenderResult,
    sources: Option<Vec<Option<&'a [f32]>>>,
    routed: Option<RoutedFrames<'a>>,
}

impl<'a> Composer<'a> {
    pub(super) fn new(r: &'a RenderResult, layout: &FrameLayout) -> Self {
        let (sources, routed) = match layout {
            FrameLayout::Device(_) => (None, None),
            FrameLayout::Sources(keys) => (Some(keys.iter().map(|k| r.sources.as_ref().and_then(|s| s.get(k))).collect()), None),
            FrameLayout::Routed(rt) => (None, Some(RoutedFrames::new(&r.audio, r.sources.as_ref(), rt))),
        };
        Self { r, sources, routed }
    }

    /// Write frame `f` into `out` (`layout.width()` long).
    pub(super) fn frame(&self, f: usize, out: &mut [f32]) {
        if let Some(rf) = &self.routed {
            return rf.frame(f, out);
        }
        let Some(views) = &self.sources else {
            return compose_frame(self.r, f, out);
        };
        out[0] = self.r.audio[2 * f];
        out[1] = self.r.audio[2 * f + 1];
        for (i, v) in views.iter().enumerate() {
            let (l, r) = v.map_or((0.0, 0.0), |b| (b[2 * f], b[2 * f + 1]));
            out[2 + 2 * i] = l;
            out[3 + 2 * i] = r;
        }
    }
}

/// Everything the worker writes to: the audio ring + its frame layout, the stop flag, and the
/// observation channels. Owned by the worker thread for the session's life.
pub(super) struct WorkerIo {
    pub(super) prod: Producer<f32>,
    pub(super) layout: FrameLayout,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) telemetry: Arc<Mutex<Option<Telemetry>>>,
    pub(super) viz_tx: mpsc::SyncSender<VizFrame>,
    pub(super) midi_tx: mpsc::Sender<TimedMidi>,
    /// The MIDI mirror (ADR 0048): its stream, which instruments, and whether records carry a note log.
    pub(super) tap_tx: mpsc::Sender<TimedTap>,
    pub(super) note_tap: BTreeSet<String>,
    pub(super) note_log: bool,
    pub(super) records_tx: mpsc::Sender<LoopRecord>,
    /// Audio frames pushed so far — the absolute play-cursor origin for viz, MIDI and records.
    pub(super) frames_produced: u64,
    /// The shell's revision of the doc now playing (stamped on each record; ADR 0034).
    pub(super) doc_rev: u64,
    /// Live mode (ADR 0035): mid-loop edits applied, and per-block telemetry (best-effort).
    pub(super) applied_tx: mpsc::Sender<crate::telemetry::Applied>,
    pub(super) blocks_tx: mpsc::SyncSender<crate::telemetry::BlockTelemetry>,
}

impl WorkerIo {
    /// Arm a fresh engine for this session: the layout's observation taps, the MIDI mirror and the
    /// note log.
    pub(super) fn arm(&self, eng: &mut Engine) {
        self.layout.arm(eng);
        eng.set_note_tap(self.note_tap.clone());
        eng.set_note_log(self.note_log);
    }

    /// Push one composed device frame (`layout.width()` samples), pacing on ring backpressure.
    /// `Err(())` if `stop` was raised mid-push.
    pub(super) fn push_frame(&mut self, frame: &[f32]) -> Result<(), ()> {
        for &s in frame {
            while self.prod.push(s).is_err() {
                if self.stop.load(Ordering::Acquire) {
                    return Err(());
                }
                thread::sleep(Duration::from_micros(200));
            }
        }
        Ok(())
    }

    /// Push a rendered loop into the audio ring as `layout.width()`-wide interleaved frames.
    fn push_render(&mut self, r: &RenderResult) -> Result<(), ()> {
        let layout = self.layout.clone();
        let mut scratch = vec![0.0f32; layout.width()];
        let composer = Composer::new(r, &layout);
        for f in 0..r.num_frames {
            composer.frame(f, &mut scratch);
            self.push_frame(&scratch)?;
        }
        Ok(())
    }

    /// Re-base a loop's MIDI events onto the absolute sample clock for the shell scheduler.
    fn send_midi(&self, r: &RenderResult, loop_start: u64) {
        for e in &r.midi_events {
            let _ = self.midi_tx.send(TimedMidi { abs_frame: loop_start + e.frame as u64, msg: e.msg });
        }
        for e in &r.note_tap {
            let _ = self.tap_tx.send(TimedTap { abs_frame: loop_start + e.frame as u64, inst: e.inst.clone(), msg: e.msg });
        }
    }
}

/// Master sample peak |x| over a render.
fn peak_of(r: &RenderResult) -> f32 {
    r.audio.iter().fold(0.0f32, |m, x| m.max(x.abs()))
}

/// What one loop did, for its telemetry / record (the pick, the song position, the played peak).
pub(super) struct LoopEmit<'a> {
    pub(super) loop_id: u64,
    pub(super) loop_start: u64,
    pub(super) path: &'a str,
    pub(super) delta: f32,
    pub(super) harmonic_move: Option<crate::orrery::HarmonicMove>,
    pub(super) categorical_move: Option<crate::categorical::CategoricalMove>,
    pub(super) song: Option<SongTelemetry>,
    pub(super) op_value: serde_json::Value,
    pub(super) peak: f32,
}

/// Emit a finished loop's observations: its viz hops, the telemetry snapshot and its [`LoopRecord`].
/// Off the audio path — never affects rendering (determinism holds).
pub(super) fn emit_loop(ctl: &Controller, io: &mut WorkerIo, r: &mut RenderResult, e: LoopEmit<'_>) {
    // Live-viz: emit this loop's 25 Hz hops to the instrument screen (empty when the tap is off, so
    // this is a no-op for non-TUI runs). Best-effort `try_send` — the worker is never gated by the UI.
    // Each hop carries its absolute audio position so the TUI can reveal it in sync with playback.
    {
        let nframes = r.num_frames;
        let masters = std::mem::take(&mut r.viz_master_hops);
        let mut drys = std::mem::take(&mut r.viz_inst_dry_hops);
        let mut effs = std::mem::take(&mut r.viz_eff_hops);
        let mut hop_start = 0u64;
        for (i, master) in masters.into_iter().enumerate() {
            let audio_pos = e.loop_start + hop_start;
            hop_start += master.len() as u64;
            let inst_dry = if i < drys.len() { std::mem::take(&mut drys[i]) } else { Vec::new() };
            let eff = if i < effs.len() { std::mem::take(&mut effs[i]) } else { BTreeMap::new() };
            let _ = io.viz_tx.try_send(VizFrame { loop_id: e.loop_id, hop_index: i as u32, num_frames: nframes, audio_pos, master, inst_dry, eff });
        }
    }
    // Read-only telemetry snapshot.
    {
        let constraints = ctl
            .constraint_status()
            .into_iter()
            .map(|(path, value, min, max, in_range)| ConstraintStatus { path, value, min, max, in_range })
            .collect();
        let orrery = orrery_of(ctl);
        // Per-track audibility: join the rendered dry RMS (S1-h5) with each instrument's `enabled`,
        // so the TUI can flag a voice that is silent in the audio despite a full `level_db` knob.
        let tracks: Vec<TrackStatus> = ctl
            .operating_point()
            .instruments
            .iter()
            .map(|(name, inst)| {
                let rms = r.per_inst_rms.get(name).copied().unwrap_or(0.0);
                TrackStatus { name: name.clone(), enabled: inst.enabled, rms, audible: inst.enabled && rms > AUDIBLE_EPS }
            })
            .collect();
        if let Ok(mut g) = io.telemetry.lock() {
            *g = Some(Telemetry {
                loop_id: e.loop_id,
                revision: ctl.revision(),
                optimize: ctl.optimize_label().to_string(),
                chosen_path: e.path.to_string(),
                operating_point: e.op_value,
                constraints,
                orrery: orrery.clone(),
                tracks,
                song: e.song.clone(),
            });
        }
    }
    let _ = io.records_tx.send(LoopRecord {
        loop_id: e.loop_id,
        revision: ctl.revision(),
        optimize: ctl.optimize_label().to_string(),
        start_frame: e.loop_start,
        num_frames: r.num_frames,
        path: e.path.to_string(),
        delta: e.delta,
        harmonic_move: e.harmonic_move,
        categorical_move: e.categorical_move,
        song: e.song.as_ref().map(SongStamp::from),
        peak: e.peak,
        doc_rev: io.doc_rev,
        orrery: orrery_of(ctl),
        note_log: r.note_log.take(),
    });
}

/// Pick this loop's move, render it, push its audio into the ring (back-pressure paces us to real
/// time), emit MIDI / viz / telemetry / a [`LoopRecord`], and re-base. `Err(())` if `stop` was
/// raised mid-push (shutting down).
pub(super) fn render_next(
    ctl: &mut Controller,
    eng: &mut Engine,
    snap: &mut PhaseSnapshot,
    io: &mut WorkerIo,
    loop_id: u64,
    song: Option<SongTelemetry>,
) -> Result<(), ()> {
    let p = ctl.next(loop_id);
    let op_value = serde_json::to_value(ctl.operating_point()).unwrap_or_default();
    let mut r = eng.render(&p.doc, snap);

    // Absolute frame offset of this loop's first sample (the shell's frames-played origin).
    let loop_start = io.frames_produced;
    io.push_render(&r)?;
    io.frames_produced += r.num_frames as u64;
    io.send_midi(&r, loop_start);
    let peak = peak_of(&r);
    emit_loop(
        ctl,
        io,
        &mut r,
        LoopEmit { loop_id, loop_start, path: &p.path, delta: p.delta, harmonic_move: p.harmonic_move.clone(), categorical_move: p.categorical_move.clone(), song, op_value, peak },
    );
    ctl.rebase(p.doc);
    *snap = r.next_snapshot;
    Ok(())
}

/// Crossfade duration for a **structural** live-edit doc swap (ADR 0024). Equal-power; wall-clock so
/// it's independent of loop length. Short enough to feel immediate, long enough to mask an
/// added/removed voice's onset/tail.
const XFADE_SECS: f64 = 0.4;

/// Apply a pending loop-doc swap at the loop boundary (LiveEdit author mode, ADR 0024).
///
/// - **Same-id edit** (`doc_swap_is_structural` ⇒ false): swap in place via [`Controller::swap_doc`]
///   and return `Ok(None)` — the caller renders the new doc normally next loop, seamlessly (the
///   carried [`PhaseSnapshot`] keeps DSP tails for the unchanged ids).
/// - **Structural edit**: build a fresh standby `Engine`/`Controller` from `new_doc`, render one
///   transition loop from **the same** pre-swap `snap` on both engines (so unchanged voices are
///   bit-identical and don't blur), equal-power crossfade the first [`XFADE_SECS`] from old→new, push
///   the blended loop, advance `snap`, and return `Ok(Some((new_eng, new_ctl)))` to adopt.
///
/// Returns `Err(())` only if `stop` is raised mid-push (shutting down).
#[allow(clippy::too_many_arguments, clippy::result_unit_err)]
pub(super) fn apply_doc_swap(
    eng: &mut Engine,
    ctl: &mut Controller,
    new_doc: LoopDoc,
    sr: f64,
    snap: &mut PhaseSnapshot,
    io: &mut WorkerIo,
    loop_id: u64,
) -> Result<Option<(Engine, Controller)>, ()> {
    if !doc_swap_is_structural(ctl.operating_point(), &new_doc) {
        ctl.swap_doc(new_doc);
        return Ok(None);
    }
    // Render both from the SAME pre-swap snapshot: unchanged instruments come out bit-identical, so
    // the crossfade only masks the parts that actually changed (added voice fades in, removed fades
    // out). The old engine is discarded after, so mutating it here is harmless.
    let gains = eng.live_gain_state(); // as they stand before this loop, for the standby to continue
    let old_res = eng.render(ctl.operating_point(), snap);
    // Standby engine + controller from the new doc, inheriting the live control (stays LiveEdit)
    // and the Orrery's voyage (moved out of the old controller, which is replaced).
    let new_ctl = ctl.rebuilt(new_doc);
    let mut new_eng = Engine::new(sr);
    io.arm(&mut new_eng);
    new_eng.adopt_live_gains(gains);
    let new_res = new_eng.render(new_ctl.operating_point(), snap);

    // Equal-power crossfade over the first XFADE region, blended per composed device frame
    // (ADR 0027) so `out_channel` stems — and ADR 0030 source stems — crossfade on their own pairs,
    // then push (back-pressure paces us; bail on stop).
    let layout = io.layout.clone();
    let n = layout.width();
    let (old_c, new_c) = (Composer::new(&old_res, &layout), Composer::new(&new_res, &layout));
    let xf = ((sr * XFADE_SECS) as usize).min(old_res.num_frames).min(new_res.num_frames);
    let loop_start = io.frames_produced;
    let mut sa = vec![0.0f32; n];
    let mut sb = vec![0.0f32; n];
    // The record's peak is the MASTER as played — the crossfaded blend, which can exceed both inputs.
    let mut peak = 0.0f32;
    for f in 0..new_res.num_frames {
        new_c.frame(f, &mut sb);
        let master = |r: &RenderResult, c: usize| r.audio.get(2 * f + c).copied().unwrap_or(0.0);
        if f < xf {
            old_c.frame(f, &mut sa);
            let a = (f as f64 / xf as f64) * std::f64::consts::FRAC_PI_2;
            let (g_old, g_new) = (a.cos() as f32, a.sin() as f32);
            for c in 0..n {
                sb[c] = g_old * sa[c] + g_new * sb[c];
            }
            for c in 0..2 {
                peak = peak.max((g_old * master(&old_res, c) + g_new * master(&new_res, c)).abs());
            }
        } else {
            peak = peak.max(master(&new_res, 0).abs()).max(master(&new_res, 1).abs());
        }
        io.push_frame(&sb)?;
    }
    io.frames_produced += new_res.num_frames as u64; // keep the viz play-cursor aligned across the swap
    // MIDI-out (ADR 0026): the ADOPTED (new) render's events are the ones consistent with the
    // adopted snapshot (both renders diffed against the same pre-swap emitted state); the old
    // engine's transition render is a blend source only, its events discarded with it.
    io.send_midi(&new_res, loop_start);
    let _ = io.records_tx.send(LoopRecord {
        loop_id,
        revision: new_ctl.revision(),
        optimize: new_ctl.optimize_label().to_string(),
        start_frame: loop_start,
        num_frames: new_res.num_frames,
        path: String::new(),
        delta: 0.0,
        harmonic_move: None,
        categorical_move: None,
        song: None,
        peak,
        doc_rev: io.doc_rev,
        orrery: orrery_of(&new_ctl),
        note_log: new_res.note_log.clone(),
    });
    *snap = new_res.next_snapshot;
    Ok(Some((new_eng, new_ctl)))
}

/// Session-end MIDI safety flush (ADR 0026): Note-Off every note still held externally, plus
/// **CC 123 (All Notes Off)** on every channel a midi instrument used, stamped at the end of the
/// produced audio — and a Note-Off for every note the mirror holds (ADR 0048; its channels are
/// the shell's, which sends its own CC 123). Runs on every worker exit path (bounded run, `stop`),
/// so an external synth is never left hanging; shells additionally flush on their own hard-kill
/// paths.
pub(super) fn flush_midi_on_exit(snap: &PhaseSnapshot, io: &WorkerIo) {
    let (midi_tx, frames_produced) = (&io.midi_tx, io.frames_produced);
    for (inst, st) in &snap.note_tap {
        let mut out = Vec::new();
        flush_tap(&mut st.clone(), inst, 0, &mut out);
        for e in out {
            let _ = io.tap_tx.send(TimedTap { abs_frame: frames_produced, inst: e.inst, msg: e.msg });
        }
    }
    let mut channels: std::collections::BTreeSet<u8> = std::collections::BTreeSet::new();
    for st in snap.midi_out.values() {
        channels.insert(st.channel);
        let mut st = st.clone();
        let mut out = Vec::new();
        flush_notes(&mut st, 0, &mut out);
        for e in out {
            let _ = midi_tx.send(TimedMidi { abs_frame: frames_produced, msg: e.msg });
        }
    }
    for ch in channels {
        let _ = midi_tx.send(TimedMidi { abs_frame: frames_produced, msg: MidiMsg::Cc { ch, cc: 123, value: 0 } });
    }
}

/// The song hook at the loop top (ADR 0029): block starts reset the controller + arm the seam,
/// the tail start freezes it. `None` for a song-less session.
pub(super) fn song_loop_top(song: Option<&mut SongDriver>, n: usize, ctl: &mut Controller, snap: &mut PhaseSnapshot) -> Option<SongTelemetry> {
    song.map(|s| s.on_loop_top(n, ctl, snap))
}

/// Take a pending LiveEdit doc swap — unless a song is playing: the song owns the matrix (and a
/// structural swap's crossfade loop would skip the song hook), so the swap is dropped with a
/// one-shot warning instead.
pub(super) fn take_pending_doc(pending: &Mutex<Option<(LoopDoc, Option<u64>)>>, song_active: bool, warned: &mut bool) -> Option<(LoopDoc, Option<u64>)> {
    let doc = pending.lock().ok().and_then(|mut g| g.take())?;
    if song_active {
        if !*warned {
            eprintln!("engines_core::session: a song is playing — LiveEdit doc swaps are ignored (ADR 0029)");
            *warned = true;
        }
        return None;
    }
    Some(doc)
}

/// The Orrery's state after this loop's pick, for the telemetry snapshot and the loop's record.
fn orrery_of(ctl: &Controller) -> Option<OrreryTelemetry> {
    ctl.orrery_telemetry().map(|v| OrreryTelemetry {
        chord: v.chord,
        tension: v.tension,
        loops_since_home: v.loops_since_home,
        voice_change_avg: v.voice_change_avg,
        leader: v.leader,
        energy: v.energy,
        arp_pulses: v.arp_pulses,
        dropped: v.dropped,
        returning_home: v.returning_home,
        phrase_active: v.phrase_active,
        pad: v.pad,
        bass: v.bass,
        tones: v.tones,
    })
}
