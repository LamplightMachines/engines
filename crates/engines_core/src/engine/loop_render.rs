//! Block rendering (ADR 0035): one loop split into **begin → blocks → finish**, so a live player can
//! render a loop a block at a time and apply an edit **mid-loop** ([`LoopRender::retarget`]).
//!
//! [`Engine::begin_loop`] does everything `render` used to do before its per-sample loop (modulator
//! evaluation, the control/note plans, carried voice state, MIDI reconciliation, bus setup);
//! [`LoopRender::render_block`] runs the per-sample loop over the next `n` frames;
//! [`LoopRender::finish`] builds the next snapshot and the [`RenderResult`]. [`Engine::render`] is
//! exactly `begin_loop` + `render_block(total)` + `finish`, and any block split renders the same
//! samples (pinned by the golden hashes and a block-size invariance test) — events and seeds keep
//! using the frame position within the loop.

use super::*;
use crate::modulator::ModulatorOutput;
use std::fmt::Write as _;

/// One prob-rolled note event, sorted by frame.
#[derive(Clone, Debug)]
pub(super) struct NoteUpdate {
    frame: usize,
    instrument: String,
    source: String,
    priority: i32,
    budget: usize,
    voicing: Vec<crate::loop_doc::NoteVoice>,
}

/// A MIDI translator instrument (ADR 0026): prepared channel + CC routes + working emitted state.
#[derive(Clone, Debug)]
struct MidiInst {
    /// Wire channel `0..=15` (doc authors `1..=16`).
    channel: u8,
    velocity_default: u8,
    /// `(param, cc#, Some(cur_vals idx) when modulated, normalized base)`.
    cc_routes: Vec<(String, u8, Option<usize>, f32)>,
    state: MidiOutState,
}

/// A mirrored audio instrument (ADR 0048): its velocity default + working emitted notes.
#[derive(Clone, Debug)]
struct TapInst {
    velocity_default: u8,
    state: TapState,
}

/// What kind of edit a [`LoopRender::retarget`] was (the crossfade length follows it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    /// Same ids and shape — values only.
    Value,
    /// Instruments / routes / buses / modulators added or removed, a synth type or loop length changed.
    Structural,
}

/// A loop being rendered: everything `Engine::render` used to keep in locals.
#[derive(Clone)]
pub struct LoopRender {
    doc: LoopDoc,
    sr: f32,
    total: usize,
    /// The next frame to render (`0..=total`).
    pos: usize,
    /// The loop-start snapshot (modulator phases, seam, depths) — what a retarget re-evaluates from.
    start: PhaseSnapshot,
    audio: Vec<f32>,
    next: PhaseSnapshot,
    mod_out: BTreeMap<String, ModulatorOutput>,
    src_step_s: BTreeMap<String, f32>,
    index_of: BTreeMap<String, usize>,
    smoothed: Vec<Smoothed>,
    base_vals: Vec<f32>,
    events: Vec<Vec<(usize, f32)>>,
    cursors: Vec<usize>,
    cur_vals: Vec<f32>,
    note_updates: Vec<NoteUpdate>,
    ni: usize,
    inst_state: BTreeMap<String, BTreeMap<String, SourceVoices>>,
    ib_map: BTreeMap<String, InstBase>,
    midi_insts: BTreeMap<String, MidiInst>,
    midi_events: Vec<MidiEvent>,
    /// MIDI events already handed to the shell (live mode sends them block by block).
    midi_sent: usize,
    /// Mirrored instruments (ADR 0048) and their events; `tap_sent` as `midi_sent`.
    taps: BTreeMap<String, TapInst>,
    tap_events: Vec<TapEvent>,
    tap_sent: usize,
    root_st: f32,
    width_idx: Option<usize>,
    gain_idx: Option<usize>,
    sends_of: BTreeMap<String, Vec<(String, f32)>>,
    bus_param_routes: Vec<(String, String, String, usize)>,
    bus_return_routes: Vec<(String, usize)>,
    glide_info: BTreeMap<String, (f32, usize, usize)>,
    inst_sumsq: BTreeMap<String, f64>,
    viz_on: bool,
    viz_inst: Option<String>,
    viz_hop: usize,
    viz_master_hops: Vec<Vec<f32>>,
    viz_inst_dry_hops: Vec<Vec<f32>>,
    viz_eff_hops: Vec<BTreeMap<String, f32>>,
    cur_master: Vec<f32>,
    cur_inst: Vec<f32>,
    stem_route: BTreeMap<String, u16>,
    stem_mix: Option<StemMix>,
    src_stems: Option<SourceStems>,
    /// Live gains (mute/solo, ADR 0035): `(current, target)` per instrument, in `inst_state` order.
    gains: Vec<(f32, f32)>,
    gain_step: f32,
    /// Per-instrument block peaks (live meters), `[left, right]` in `inst_state` order; `None` =
    /// metering off.
    meters: Option<Vec<[f32; 2]>>,
    /// Reused per-voice map key buffer (no per-frame allocation).
    key_buf: String,
}

/// The key of a voice's DSP state (filters, anti-alias, glide, pitched-noise band): instrument, note
/// source and voice id. Authored voice ids (`v1`, …) repeat across sources, so two note routes into
/// one instrument would otherwise share — and double-advance — one voice's state. The per-sample
/// loop writes the same string into a reused buffer.
fn voice_key(instrument: &str, source: &str, voice_id: &str) -> String {
    format!("{instrument}::{source}::{voice_id}")
}

fn base_of(doc: &LoopDoc, dest: &str) -> f32 {
    let raw = match resolve_dest(dest) {
        Dest::Global { param } => match param.as_str() {
            "master_gain_db" => doc.global.master_gain_db,
            "stereo_width" => doc.global.stereo_width,
            _ => 0.0,
        },
        // An omitted param's route starts from its default, as the param plays unrouted.
        Dest::InstParam { inst, param } => {
            let n = doc.instruments.get(&inst).and_then(|i| i.norm(&param)).or_else(|| crate::params::default_norm(&param)).unwrap_or_else(|| param_norm(dest, 0.0));
            return if n == f32::NEG_INFINITY { n } else { n.clamp(0.0, 1.0) };
        }
        Dest::BusParam { bus, fx, param } => return doc.buses.get(&bus).and_then(|b| b.effects.get(&fx)).and_then(|e| e.norm(&param)).unwrap_or_else(|| param_norm(dest, 0.0)),
        Dest::BusReturn { bus } => doc.buses.get(&bus).map(|b| b.return_gain_db).unwrap_or(f32::NEG_INFINITY),
        _ => 0.0,
    };
    // The typed gain fields hold dB, the width its `0..1`.
    param_norm(dest, raw)
}

/// The desired note set of a midi instrument right now: every held (un-released) voice →
/// `(voice_id, (note#, velocity))`. Velocity: this event's authored hint, else what the note was
/// emitted with, else the instrument default.
fn midi_desired(root_st: f32, sources: Option<&BTreeMap<String, SourceVoices>>, prev: &BTreeMap<String, (u8, u8)>, vel_hint: &BTreeMap<String, u8>, vel_default: u8) -> BTreeMap<String, (u8, u8)> {
    let mut want = BTreeMap::new();
    if let Some(sources) = sources {
        for sv in sources.values() {
            for v in &sv.voices {
                if v.released_at_s.is_some() {
                    continue; // note-off at the release frame; tail is the external synth's
                }
                let note = midi_note_number(root_st, v.pitch_semitones);
                let vel = vel_hint.get(&v.voice_id).copied().or_else(|| prev.get(&v.voice_id).map(|&(_, vl)| vl)).unwrap_or(vel_default);
                want.insert(v.voice_id.clone(), (note, vel));
            }
        }
    }
    want
}

/// [`midi_desired`] for the mirror (ADR 0048): keyed `source::voice_id`, velocity hints keyed alike.
fn tap_desired(root_st: f32, sources: Option<&BTreeMap<String, SourceVoices>>, prev: &BTreeMap<String, (u8, u8)>, vel_hint: &BTreeMap<String, u8>, vel_default: u8) -> BTreeMap<String, (u8, u8)> {
    let mut want = BTreeMap::new();
    if let Some(sources) = sources {
        for (source, sv) in sources {
            for v in sv.voices.iter().filter(|v| v.released_at_s.is_none()) {
                let key = tap_key(source, &v.voice_id);
                let note = midi_note_number(root_st, v.pitch_semitones);
                let vel = vel_hint.get(&key).copied().or_else(|| prev.get(&key).map(|&(_, vl)| vl)).unwrap_or(vel_default);
                want.insert(key, (note, vel));
            }
        }
    }
    want
}

/// An event's authored velocities (ADR 0026/0028) by voice id — anonymous voices under the id
/// `apply_note_event` mints for them.
fn velocity_hints(nu: &NoteUpdate, frame: usize) -> Vec<(String, u8)> {
    nu.voicing
        .iter()
        .enumerate()
        .filter_map(|(i, nv)| nv.velocity.map(|v| (nv.id.clone().unwrap_or_else(|| synthetic_voice_id(&nu.source, frame, i)), v.clamp(1, 127))))
        .collect()
}

impl Engine {
    /// Render one loop in one go — `begin_loop` + `render_block(total)` + `finish`.
    pub fn render(&mut self, doc: &LoopDoc, snapshot: &PhaseSnapshot) -> RenderResult {
        let mut lr = self.begin_loop(doc, snapshot);
        let total = lr.total();
        lr.render_block(self, total);
        lr.finish(self)
    }

    /// Start a loop: everything before the per-sample loop.
    pub fn begin_loop(&mut self, doc: &LoopDoc, snapshot: &PhaseSnapshot) -> LoopRender {
        self.begin_at(doc, snapshot, 0)
    }

    /// [`begin_loop`](Engine::begin_loop) positioned at frame `at`: MIDI flushes / reconciliation
    /// land at `at` (a retarget's edit point) instead of the loop top, and the seam's orphan release
    /// only happens at the top.
    fn begin_at(&mut self, doc: &LoopDoc, snapshot: &PhaseSnapshot, at: usize) -> LoopRender {
        let sr = self.sample_rate;
        let total = doc.total_frames(sr as f64);
        let audio = vec![0.0f32; total * 2];
        let mut next = PhaseSnapshot { prev_loop_id: doc.loop_id, ..Default::default() };
        let mut lr = LoopRender {
            doc: doc.clone(),
            sr,
            total,
            pos: at.min(total),
            start: snapshot.clone(),
            audio,
            next: PhaseSnapshot::default(),
            mod_out: BTreeMap::new(),
            src_step_s: BTreeMap::new(),
            index_of: BTreeMap::new(),
            smoothed: Vec::new(),
            base_vals: Vec::new(),
            events: Vec::new(),
            cursors: Vec::new(),
            cur_vals: Vec::new(),
            note_updates: Vec::new(),
            ni: 0,
            inst_state: BTreeMap::new(),
            ib_map: BTreeMap::new(),
            midi_insts: BTreeMap::new(),
            midi_events: Vec::new(),
            midi_sent: 0,
            taps: BTreeMap::new(),
            tap_events: Vec::new(),
            tap_sent: 0,
            root_st: 0.0,
            width_idx: None,
            gain_idx: None,
            sends_of: BTreeMap::new(),
            bus_param_routes: Vec::new(),
            bus_return_routes: Vec::new(),
            glide_info: BTreeMap::new(),
            inst_sumsq: BTreeMap::new(),
            viz_on: false,
            viz_inst: None,
            viz_hop: 1,
            viz_master_hops: Vec::new(),
            viz_inst_dry_hops: Vec::new(),
            viz_eff_hops: Vec::new(),
            cur_master: Vec::new(),
            cur_inst: Vec::new(),
            stem_route: BTreeMap::new(),
            stem_mix: None,
            src_stems: None,
            gains: Vec::new(),
            gain_step: 1.0 / (0.01 * sr).max(1.0),
            meters: None,
            key_buf: String::new(),
        };
        if total == 0 {
            lr.next = next;
            return lr;
        }

        // 1. Modulators → control/note events + carried phase.
        let mut mod_out = BTreeMap::new();
        for m in doc.modulators.values() {
            let ph = snapshot.modulator_phases.get(&m.id).cloned().unwrap_or_default();
            let out = evaluate_modulator(m, &ph, total, sr as f64, doc);
            next.modulator_phases.insert(m.id.clone(), out.next_phase.clone());
            mod_out.insert(m.id.clone(), out);
        }

        // Per-note-source step duration (seconds) — the note's own duration used by `t60_mode: "note"`
        // modal voices to couple their ring length to the rhythm (ADR 0009). `step_duration_frac` is a
        // fraction of the master loop; scale by the loop's frame count.
        let src_step_s: BTreeMap<String, f32> = doc
            .modulators
            .values()
            .filter(|m| m.output == "note")
            .map(|m| (m.id.clone(), step_duration_frac(m) * total as f32 / sr))
            .collect();

        // 2. Smoothed modulated destinations + their summed contribution timelines.
        //    smoothed[i] ← dest; base_vals[i] = authored base; events[i] = (frame, Σcontrib).
        let mut index_of: BTreeMap<String, usize> = BTreeMap::new();
        let mut smoothed: Vec<Smoothed> = Vec::new();
        let mut base_vals: Vec<f32> = Vec::new();
        let mut contrib: Vec<BTreeMap<usize, f32>> = Vec::new();

        let seam = snapshot.seam.as_ref().filter(|sm| sm.active());
        let mut seam_slots: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        for e in doc.matrix.values().filter(|e| e.kind == "mod") {
            let Some(out) = mod_out.get(&e.source) else { continue };
            // A route into a silent (`-Infinity`) gain is inaudible (−∞ + any dB is −∞, ADR 0054
            // P5): it gets no slot, so the gain stays silent and the smoothing never sees `-inf`.
            if base_of(doc, &e.dest) == f32::NEG_INFINITY {
                continue;
            }
            let depth = e.depth_norm();
            next.connection_depths.insert(e.id.clone(), depth);
            let idx = *index_of.entry(e.dest.clone()).or_insert_with(|| {
                smoothed.push(Smoothed::new(base_of(doc, &e.dest), CONTROL_HOP as u32));
                base_vals.push(base_of(doc, &e.dest));
                contrib.push(BTreeMap::new());
                smoothed.len() - 1
            });
            let prev_depth = if e.depth_slew_ms > 0.0 { snapshot.connection_depths.get(&e.id).copied().unwrap_or(depth) } else { depth };
            let slew_frames = if e.depth_slew_ms > 0.0 { (e.depth_slew_ms * sr / 1000.0) as usize } else { 0 };
            // A song seam (ADR 0029) glides this route's depth from the outgoing block's value
            // (0 for a new route) — it supersedes the per-loop `depth_slew_ms` slew.
            let seam_from = seam.and_then(|sm| sm.depth_from.get(&e.id).map(|&from| (sm, from)));
            if seam_from.is_some() {
                seam_slots.insert(idx);
            }
            for ce in &out.control_events {
                let depth = if let Some((sm, from)) = seam_from {
                    depth + sm.gain_at(ce.frame) * (from - depth)
                } else if slew_frames > 0 {
                    prev_depth + (ce.frame as f32 / slew_frames as f32).min(1.0) * (depth - prev_depth)
                } else {
                    depth
                };
                *contrib[idx].entry(ce.frame).or_insert(0.0) += depth * ce.value;
            }
        }
        // Song seam (ADR 0029): removed routes fade out at `depth · g`, and every param the new
        // block moved starts at the old value (`+ offset · g`). Contributions land ONLY on the
        // CONTROL_HOP grid — the engine sets `target = base + Σcontrib at that frame`, so an
        // off-grid seam event would wipe the routes' contributions (and vice versa). Slots are
        // appended (existing indices unchanged) before `InstBase`/bus/CC routes read `index_of`,
        // so unmodulated params glide through the same path as modulated ones.
        if let Some(sm) = seam {
            let slot_of = |dest: &str, index_of: &mut BTreeMap<String, usize>, smoothed: &mut Vec<Smoothed>, base_vals: &mut Vec<f32>, contrib: &mut Vec<BTreeMap<usize, f32>>| -> usize {
                *index_of.entry(dest.to_string()).or_insert_with(|| {
                    // A silent gain glides from or to its −60 dB floor (normalized 0), never `-inf`.
                    let base = base_of(doc, dest).max(0.0);
                    smoothed.push(Smoothed::new(base, CONTROL_HOP as u32));
                    base_vals.push(base);
                    contrib.push(BTreeMap::new());
                    smoothed.len() - 1
                })
            };
            for fr in sm.fading_routes.values() {
                let Some(out) = mod_out.get(&fr.source) else { continue };
                let idx = slot_of(&fr.dest, &mut index_of, &mut smoothed, &mut base_vals, &mut contrib);
                seam_slots.insert(idx);
                for ce in &out.control_events {
                    *contrib[idx].entry(ce.frame).or_insert(0.0) += fr.depth * sm.gain_at(ce.frame) * ce.value;
                }
            }
            for (dest, &off) in &sm.param_offsets {
                let idx = slot_of(dest, &mut index_of, &mut smoothed, &mut base_vals, &mut contrib);
                seam_slots.insert(idx);
                for f in (0..total).step_by(CONTROL_HOP) {
                    // emitted through the loop's end: once g = 0 this adds 0.0 and the slot sits
                    // exactly on its new base.
                    *contrib[idx].entry(f).or_insert(0.0) += off * sm.gain_at(f);
                }
            }
            // Start every seam-touched slot AT its frame-0 value (the old block's level) instead of
            // the new base — otherwise the usual 64-sample ramp from base would blip at the seam.
            for &idx in &seam_slots {
                let at0 = contrib[idx].get(&0).copied().unwrap_or(0.0);
                smoothed[idx].set_now(clamp01(base_vals[idx] + at0));
            }
        }
        // Flatten contribution maps to sorted vecs + cursors.
        let events: Vec<Vec<(usize, f32)>> = contrib.iter().map(|m| m.iter().map(|(&f, &v)| (f, v)).collect()).collect();
        let cursors = vec![0usize; events.len()];
        let cur_vals = base_vals.clone();

        // 3. Note updates (prob-rolled), sorted by frame.
        let mut note_updates: Vec<NoteUpdate> = Vec::new();
        for e in doc.matrix.values().filter(|e| e.kind == "note") {
            let Some(out) = mod_out.get(&e.source) else { continue };
            let Dest::Notes { inst } = resolve_dest(&e.dest) else { continue };
            for ne in &out.note_events {
                let mut rolled = Vec::new();
                for (vi, nv) in ne.voicing.iter().enumerate() {
                    if nv.release || nv.prob >= 1.0 {
                        rolled.push(nv.clone());
                    } else {
                        let h = hash_site(doc.loop_id, &e.source, ne.frame as u64, &vi.to_string());
                        if hash_to_unit(h) < nv.prob {
                            rolled.push(nv.clone());
                        }
                    }
                }
                note_updates.push(NoteUpdate {
                    frame: ne.frame,
                    instrument: inst.clone(),
                    source: e.source.clone(),
                    priority: e.priority,
                    budget: e.voices.max(1) as usize,
                    voicing: rolled,
                });
            }
        }
        note_updates.sort_by_key(|n| n.frame);

        // 4. Instrument runtime voice state, seeded from the snapshot, grouped by source.
        let mut prio_of: BTreeMap<(String, String), i32> = BTreeMap::new();
        for e in doc.matrix.values().filter(|e| e.kind == "note") {
            if let Dest::Notes { inst } = resolve_dest(&e.dest) {
                prio_of.insert((inst, e.source.clone()), e.priority);
            }
        }
        let mut inst_state: BTreeMap<String, BTreeMap<String, SourceVoices>> = BTreeMap::new();
        for (name, _) in doc.instruments.iter() {
            let mut sources: BTreeMap<String, SourceVoices> = BTreeMap::new();
            if let Some(avs) = snapshot.active_voices.get(&format!("{name}.notes")) {
                for av in avs {
                    let sv = sources.entry(av.source_id.clone()).or_insert_with(|| SourceVoices {
                        priority: prio_of.get(&(name.clone(), av.source_id.clone())).copied().unwrap_or(1),
                        voices: Vec::new(),
                    });
                    sv.voices.push(av.clone());
                }
            }
            inst_state.insert(name.clone(), sources);
        }
        // Song seam (ADR 0029): a carried voice whose (instrument, source) note route the new block
        // dropped would otherwise sustain forever (voices are carried regardless of routing) — give
        // it a note-off at frame 0 so its own release envelope (and filter) plays it out. Done
        // before the midi loop-start reconciliation so external synths get the Note-Off too.
        if let Some(sm) = &snapshot.seam {
            if at == 0 && sm.release_orphans && sm.elapsed_frames == 0 {
                for (name, sources) in inst_state.iter_mut() {
                    for (source, sv) in sources.iter_mut() {
                        if prio_of.contains_key(&(name.clone(), source.clone())) {
                            continue;
                        }
                        for v in sv.voices.iter_mut().filter(|v| v.released_at_s.is_none()) {
                            v.released_at_s = Some(v.elapsed_decay_s);
                        }
                    }
                }
            }
        }

        // 5. Precompute per-instrument base params + modulated-param indices.
        let mut ib_map: BTreeMap<String, InstBase> = BTreeMap::new();
        for (name, inst) in doc.instruments.iter() {
            let mut idx = BTreeMap::new();
            for &param in MODULATABLE_INST_PARAMS {
                if let Some(&i) = index_of.get(&format!("instruments.{name}.params.{param}")) {
                    idx.insert(param.to_string(), i);
                }
            }
            let kind = SynthKind::from_doc(inst);
            let is_event = name == "event" && matches!(kind, SynthKind::Subtractive);
            // A binaural pair is never noise (validate() refuses it); an internally-built doc that
            // asks anyway plays sines rather than taking the noise path's band filters.
            let noise = inst.wave() == Some("filtered_noise") && !kind.hard_lr();
            let wave = if inst.wave() == Some("filtered_noise") && kind.hard_lr() { "sine" } else { inst.wave().unwrap_or("sine") };
            let pitched_noise = inst.pitched_noise();
            let release_tail_s = match &kind {
                SynthKind::Modal(cfg) => cfg.release_tail_s(),
                // MIDI (ADR 0026): the Note-Off goes out at the release frame; the external
                // synth owns its own release tail, so the voice is pruned immediately.
                SynthKind::Midi => 0.0,
                _ if is_event => 0.0,
                _ => inst.native_or_default("release_s"),
            };
            ib_map.insert(
                name.clone(),
                InstBase {
                    kind,
                    release_tail_s,
                    wave: Wave::from_name(wave),
                    is_noise: noise,
                    pitched_noise,
                    is_event,
                    level: inst.norm_or_default("level_db"),
                    drive: inst.norm_or_default("drive_db"),
                    cutoff: inst.norm_or_default("cutoff_hz"),
                    resonance: inst.norm_or_default("resonance"),
                    attack: inst.norm_or_default("attack_s"),
                    decay: inst.norm_or_default("decay_s"),
                    sustain: inst.norm_or_default("sustain"),
                    release: inst.norm_or_default("release_s"),
                    band_lo: inst.norm_or_default("band_lo_hz"),
                    band_hi: inst.norm_or_default("band_hi_hz"),
                    detune: inst.norm_or_default("voices_detune_cents"),
                    shift_st: inst.native_or_default("shift_st"),
                    shift_mix: inst.norm_or_default("shift_mix"),
                    glide_frames: (inst.note_engine.glide_ms * sr / 1000.0) as usize,
                    idx,
                },
            );
        }
        // Reshape carried modal voice state if `num_modes` changed between loops (deterministic
        // truncate / zero-extend; ADR 0009). Fresh articulations are sized correctly below.
        for (name, ib) in ib_map.iter() {
            if let SynthKind::Modal(cfg) = &ib.kind {
                if let Some(sources) = inst_state.get_mut(name) {
                    for sv in sources.values_mut() {
                        for v in sv.voices.iter_mut() {
                            v.synth_state.reshape_modal(cfg.partials.len());
                        }
                    }
                }
            }
        }

        // 5b. MIDI translator instruments (ADR 0026): prepared channel + CC routes + working
        // emitted state. CC routes look up `index_of` directly — mapped params are free-form
        // names (e.g. `macro1`), not the fixed InstBase whitelist — and unmodulated params
        // fall back to their normalized authored base.
        let mut midi_events: Vec<MidiEvent> = Vec::new();
        let mut midi_insts: BTreeMap<String, MidiInst> = BTreeMap::new();
        for (name, inst) in doc.instruments.iter() {
            if !inst.is_midi() {
                continue;
            }
            let Some(routing) = &inst.routing else { continue };
            let ch = routing.channel.clamp(1, 16) - 1;
            let cc_routes = routing
                .cc
                .iter()
                .map(|(param, &cc)| {
                    let dest = format!("instruments.{name}.params.{param}");
                    (param.clone(), cc.min(127), index_of.get(&dest).copied(), base_of(doc, &dest))
                })
                .collect();
            let mut state = snapshot.midi_out.get(name).cloned().unwrap_or(MidiOutState { channel: ch, ..Default::default() });
            // A live channel edit: flush the held notes on the OLD channel, restart the CC
            // on-change state (the new channel has seen nothing yet).
            if state.channel != ch {
                flush_notes(&mut state, at, &mut midi_events);
                state.cc_last.clear();
                state.channel = ch;
            }
            midi_insts.insert(name.clone(), MidiInst { channel: ch, velocity_default: inst.velocity_default.unwrap_or(100), cc_routes, state });
        }
        // Emitted state for instruments that no longer translate (removed / retyped / doc
        // swap): flush their held notes at frame 0 and drop the state — never a hung note.
        for (name, st) in snapshot.midi_out.iter() {
            if !midi_insts.contains_key(name) {
                let mut st = st.clone();
                flush_notes(&mut st, at, &mut midi_events);
            }
        }
        let root_st = root_semitone(&doc.harmony.root);
        // Loop-start reconciliation (frame 0): a disabled instrument flushes its notes; an
        // enabled one diffs its carried voices against the emitted set — a steady sustained
        // drone reconciles to a no-op, a re-enable re-sounds its held voices.
        let no_hints: BTreeMap<String, u8> = BTreeMap::new();
        for (name, mi) in midi_insts.iter_mut() {
            let enabled = doc.instruments.get(name).map(|i| i.enabled).unwrap_or(false);
            if !enabled {
                flush_notes(&mut mi.state, at, &mut midi_events);
                continue;
            }
            let desired = midi_desired(root_st, inst_state.get(name), &mi.state.notes, &no_hints, mi.velocity_default);
            diff_notes(&mut mi.state.notes, &desired, mi.channel, at, &mut midi_events);
        }

        // 5c. The mirror (ADR 0048): the mirrored instruments this doc has, their carried emitted
        // notes; state for an instrument no longer mirrored is flushed. Then the same loop-start
        // reconciliation as 5b (a disabled instrument flushes; the seam's orphans end here).
        let mut tap_events: Vec<TapEvent> = Vec::new();
        let mut taps: BTreeMap<String, TapInst> = BTreeMap::new();
        for name in &self.note_tap {
            let Some(inst) = doc.instruments.get(name).filter(|i| i.can_tap_notes()) else { continue };
            let state = snapshot.note_tap.get(name).cloned().unwrap_or_default();
            taps.insert(name.clone(), TapInst { velocity_default: inst.velocity_default.unwrap_or(100).clamp(1, 127), state });
        }
        for (name, st) in snapshot.note_tap.iter() {
            if !taps.contains_key(name) {
                flush_tap(&mut st.clone(), name, at, &mut tap_events);
            }
        }
        let no_fresh = std::collections::BTreeSet::new();
        for (name, mi) in taps.iter_mut() {
            if !doc.instruments.get(name).is_some_and(|i| i.enabled) {
                flush_tap(&mut mi.state, name, at, &mut tap_events);
                continue;
            }
            let desired = tap_desired(root_st, inst_state.get(name), &mi.state.notes, &no_hints, mi.velocity_default);
            diff_tap(&mut mi.state.notes, &desired, &no_fresh, name, at, &mut tap_events);
        }

        let gidx = |param: &str| index_of.get(&format!("global.{param}")).copied();
        let (width_idx, gain_idx) = (gidx("stereo_width"), gidx("master_gain_db"));

        // Buses: build any newly-seen bus once (tails persist across loops), then apply the
        // authored/perturbed base params + return gain to every bus so doc changes take effect
        // without rebuilding (CE-R4.4). Modulated bus dests are overridden per-sample below.
        // A same-id effect-chain edit (effect added/removed/renamed/retyped) can't be reached by
        // params, so that bus is rebuilt fresh and the old one **retires**: no more sends, its tail
        // rings out (ADR 0035).
        for (bus_id, bus_doc) in &doc.buses {
            if bus_doc.is_external_return() {
                continue; // ADR 0026 phase D: session/shell config only — no audio enters the core render
            }
            let bus = self.buses.entry(bus_id.clone()).or_insert_with(|| Bus::from_doc(bus_doc, sr));
            if !bus.built_from(bus_doc) {
                let old = std::mem::replace(bus, Bus::from_doc(bus_doc, sr));
                self.retiring.push(Retiring::new(old));
            }
            bus.set_return_gain(bus_doc.return_amp());
            for (fx_id, fx) in &bus_doc.effects {
                for pname in fx.params.keys() {
                    if let Some(v) = fx.norm(pname) {
                        bus.set_effect_param(fx_id, pname, v);
                    }
                }
            }
        }
        // Retire buses no longer present in the doc — or retyped to `external_return`, which the
        // core never renders — so they stop taking sends; their tails ring out.
        let gone: Vec<String> = self.buses.keys().filter(|id| !doc.buses.get(*id).is_some_and(|b| !b.is_external_return())).cloned().collect();
        for id in gone {
            if let Some(b) = self.buses.remove(&id) {
                self.retiring.push(Retiring::new(b));
            }
        }

        // Per-instrument send routing: precompute (bus_id, amount) lists keyed by instrument.
        let sends_of: BTreeMap<String, Vec<(String, f32)>> = doc
            .instruments
            .iter()
            .map(|(name, inst)| {
                let routes = inst.sends_db.iter().filter(|(bus, _)| self.buses.contains_key(*bus)).map(|(bus, &db)| (bus.clone(), crate::params::db_to_amp(db))).collect();
                (name.clone(), routes)
            })
            .collect();

        // Modulated bus destinations: (bus, fx, param, cur_vals index) and return-gain routes.
        let mut bus_param_routes: Vec<(String, String, String, usize)> = Vec::new();
        let mut bus_return_routes: Vec<(String, usize)> = Vec::new();
        for (dest, &i) in index_of.iter() {
            match resolve_dest(dest) {
                Dest::BusParam { bus, fx, param } => bus_param_routes.push((bus, fx, param, i)),
                Dest::BusReturn { bus } => bus_return_routes.push((bus, i)),
                _ => {}
            }
        }

        // Live-viz hop accumulators (ADR 0024) — observation only, off the determinism path.
        let viz_on = self.viz_enabled;
        let viz_hop = (sr as f64 * 0.04).max(1.0) as usize; // 25 Hz / 40 ms frames

        // Multichannel stem split (ADR 0027) — observation only, like viz: allocated only when the
        // tap is on AND the doc routes an instrument via `out_channel`. The master mix accumulation
        // is untouched (stems are *additional* adds into separate buffers), so `audio` stays
        // byte-identical with the tap on or off (CE-R7.3).
        let stem_route: BTreeMap<String, u16> = doc.instruments.iter().filter(|(_, i)| !i.is_midi()).filter_map(|(n, i)| i.out_channel.map(|oc| (n.clone(), oc))).collect();
        let stem_mix: Option<StemMix> = if self.stem_capture && !stem_route.is_empty() {
            let mut m = StemMix { rest: vec![0.0; total * 2], stems: BTreeMap::new() };
            for &oc in stem_route.values() {
                m.stems.entry(oc).or_insert_with(|| vec![0.0; total * 2]);
            }
            Some(m)
        } else {
            None
        };

        // ADR 0030: the per-source tap — a buffer per instrument this render plays (enabled,
        // non-midi) and per internal bus that reaches the master (keyed from `self.buses`, i.e.
        // exactly what's summed below). Additional adds into separate buffers only.
        let src_stems: Option<SourceStems> = self.source_capture.then(|| SourceStems {
            inst: doc.instruments.iter().filter(|(_, i)| i.enabled && !i.is_midi()).map(|(id, _)| (id.clone(), vec![0.0; total * 2])).collect(),
            bus: self.buses.keys().map(|id| (id.clone(), vec![0.0; total * 2])).collect(),
            uncaptured: false,
        });

        // Live gains (mute/solo): carried on the engine so a ramp continues across the loop seam.
        let gains = inst_state.keys().map(|n| (self.live_gain_cur.get(n).copied().unwrap_or(1.0), self.live_gain_target.get(n).copied().unwrap_or(1.0))).collect();
        let meters = self.metering.then(|| vec![[0.0; 2]; inst_state.len()]);

        lr.next = next;
        lr.mod_out = mod_out;
        lr.src_step_s = src_step_s;
        lr.index_of = index_of;
        lr.smoothed = smoothed;
        lr.base_vals = base_vals;
        lr.events = events;
        lr.cursors = cursors;
        lr.cur_vals = cur_vals;
        lr.note_updates = note_updates;
        lr.inst_state = inst_state;
        lr.ib_map = ib_map;
        lr.midi_insts = midi_insts;
        lr.midi_events = midi_events;
        lr.taps = taps;
        lr.tap_events = tap_events;
        lr.root_st = root_st;
        lr.width_idx = width_idx;
        lr.gain_idx = gain_idx;
        lr.sends_of = sends_of;
        lr.bus_param_routes = bus_param_routes;
        lr.bus_return_routes = bus_return_routes;
        lr.viz_on = viz_on;
        lr.viz_inst = self.viz_inst.clone();
        lr.viz_hop = viz_hop;
        lr.stem_route = stem_route;
        lr.stem_mix = stem_mix;
        lr.src_stems = src_stems;
        lr.gains = gains;
        lr.meters = meters;
        lr
    }
}

impl LoopRender {
    /// Frames in this loop.
    pub fn total(&self) -> usize {
        self.total
    }
    /// The next frame to render.
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn is_done(&self) -> bool {
        self.pos >= self.total
    }
    /// The doc this loop is rendering.
    pub fn doc(&self) -> &LoopDoc {
        &self.doc
    }
    /// The loop's master audio so far (interleaved stereo, `total` frames; frames past `pos` are 0).
    pub fn audio(&self) -> &[f32] {
        &self.audio
    }

    /// The multichannel stem split so far (ADR 0027), when the engine's stem tap is on.
    pub fn stems(&self) -> Option<&StemMix> {
        self.stem_mix.as_ref()
    }
    /// The per-source stems so far (ADR 0030), when the engine's source tap is on.
    pub fn sources(&self) -> Option<&SourceStems> {
        self.src_stems.as_ref()
    }

    /// Each midi instrument's output state as rendered so far (held notes, channel) — what a stop
    /// mid-loop must flush, since this loop's Note-Ons already went out block by block.
    pub(crate) fn midi_states(&self) -> BTreeMap<String, MidiOutState> {
        self.midi_insts.iter().map(|(n, mi)| (n.clone(), mi.state.clone())).collect()
    }

    /// MIDI events produced since the last call (frame-ordered, loop-relative frames).
    pub fn take_new_midi(&mut self) -> &[MidiEvent] {
        let from = self.midi_sent;
        self.midi_sent = self.midi_events.len();
        &self.midi_events[from..]
    }

    /// Each mirrored instrument's emitted notes so far (ADR 0048) — what a stop mid-loop flushes.
    pub(crate) fn tap_states(&self) -> BTreeMap<String, TapState> {
        self.taps.iter().map(|(n, mi)| (n.clone(), mi.state.clone())).collect()
    }

    /// Mirror events produced since the last call (frame-ordered, loop-relative frames).
    pub fn take_new_taps(&mut self) -> &[TapEvent] {
        let from = self.tap_sent;
        self.tap_sent = self.tap_events.len();
        &self.tap_events[from..]
    }

    /// Per-instrument `[left, right]` peaks since the last call (live metering; empty unless the
    /// engine meters).
    pub fn take_meters(&mut self) -> Vec<(String, [f32; 2])> {
        let Some(m) = self.meters.as_mut() else { return Vec::new() };
        let out = self.inst_state.keys().cloned().zip(m.iter().copied()).collect();
        m.iter_mut().for_each(|x| *x = [0.0; 2]);
        out
    }

    /// Set live gain targets (mute/solo) for the rest of this loop; instruments not named go to 1.
    pub fn set_gain_targets(&mut self, eng: &mut Engine, targets: &BTreeMap<String, f32>) {
        eng.live_gain_target = targets.clone();
        for (i, name) in self.inst_state.keys().enumerate() {
            if let Some(g) = self.gains.get_mut(i) {
                g.1 = targets.get(name).copied().unwrap_or(1.0);
            }
        }
    }

    /// Render the next `n` frames (fewer at the loop end); returns the frame range rendered.
    pub fn render_block(&mut self, eng: &mut Engine, n: usize) -> std::ops::Range<usize> {
        let start = self.pos;
        let end = (start + n).min(self.total);
        if start >= end {
            return start..start;
        }
        let LoopRender {
            doc,
            sr,
            audio,
            src_step_s,
            index_of,
            smoothed,
            base_vals,
            events,
            cursors,
            cur_vals,
            note_updates,
            ni,
            inst_state,
            ib_map,
            midi_insts,
            midi_events,
            taps,
            tap_events,
            root_st,
            width_idx,
            gain_idx,
            sends_of,
            bus_param_routes,
            bus_return_routes,
            glide_info,
            inst_sumsq,
            viz_on,
            viz_inst,
            viz_hop,
            viz_master_hops,
            viz_inst_dry_hops,
            viz_eff_hops,
            cur_master,
            cur_inst,
            stem_route,
            stem_mix,
            src_stems,
            gains,
            gain_step,
            meters,
            key_buf,
            ..
        } = self;
        let sr = *sr;
        let nyq = sr * 0.45;
        let (viz_on, viz_hop, root_st, gain_step) = (*viz_on, *viz_hop, *root_st, *gain_step);
        // Any binaural instrument (ADR 0047)? Its dry bypasses the master width; without one, every
        // path below is the unchanged mono-source one.
        let hard_lr = ib_map.values().any(|ib| ib.kind.hard_lr());
        let mut bin_taps: Vec<(Option<u16>, f32, f32)> = Vec::new();
        // Retired buses only appear at a loop start or a retarget, never mid-block, so checking here
        // covers every frame whose master holds a tail no source buffer gets (ADR 0044).
        if !eng.retiring.is_empty() {
            if let Some(s) = src_stems.as_mut() {
                s.uncaptured = true;
            }
        }

        for frame in start..end {
            // control: set smoothed targets for any due events
            for i in 0..events.len() {
                while cursors[i] < events[i].len() && events[i][cursors[i]].0 <= frame {
                    let target = clamp01(base_vals[i] + events[i][cursors[i]].1);
                    smoothed[i].set_target(target);
                    cursors[i] += 1;
                }
            }
            for i in 0..smoothed.len() {
                cur_vals[i] = smoothed[i].next();
            }

            // midi CC (ADR 0026): sample each mapped param's effective value on the
            // control-hop grid, quantize to 7 bits, emit **on change only** (an empty
            // snapshot's cc_last naturally yields the initial full sync burst). Emitted
            // before this frame's note diffs so a patch is set before a note sounds.
            if !midi_insts.is_empty() && frame % CONTROL_HOP == 0 {
                for (name, mi) in midi_insts.iter_mut() {
                    if !doc.instruments.get(name).map(|i| i.enabled).unwrap_or(false) {
                        continue;
                    }
                    for (param, cc, idx, base) in &mi.cc_routes {
                        let v7 = cc_value(idx.map(|i| cur_vals[i]).unwrap_or(*base));
                        if mi.state.cc_last.get(param) != Some(&v7) {
                            mi.state.cc_last.insert(param.clone(), v7);
                            midi_events.push(MidiEvent { frame, msg: MidiMsg::Cc { ch: mi.channel, cc: *cc, value: v7 } });
                        }
                    }
                }
            }

            // push modulated bus params (normalized 0..1) into their live handles
            for (bus, fx, param, i) in bus_param_routes.iter() {
                if let Some(b) = eng.buses.get_mut(bus) {
                    b.set_effect_param(fx, param, cur_vals[*i]);
                }
            }
            for (bus, i) in bus_return_routes.iter() {
                if let Some(b) = eng.buses.get_mut(bus) {
                    b.set_return_gain(crate::params::gain_amp(cur_vals[*i]));
                }
            }

            // note events due this frame
            while *ni < note_updates.len() && note_updates[*ni].frame <= frame {
                let nu = &note_updates[*ni];
                *ni += 1;
                let sources = inst_state.entry(nu.instrument.clone()).or_default();
                let cur = sources.get(&nu.source).map(|s| s.voices.clone()).unwrap_or_default();
                let mut vr = apply_note_event(&nu.voicing, &cur, &nu.source, nu.budget, frame);

                let is_noise = ib_map.get(&nu.instrument).map(|b| b.is_noise).unwrap_or(false);
                let gframes = ib_map.get(&nu.instrument).map(|b| b.glide_frames).unwrap_or(0);
                // midi translators are silent (ADR 0026): no per-voice filter/glide DSP to
                // book-keep — their note changes are diffed into Note-On/Off below instead.
                let is_midi = ib_map.get(&nu.instrument).map(|b| matches!(b.kind, SynthKind::Midi)).unwrap_or(false);
                if !is_midi {
                    for act in &vr.actions {
                        let key = voice_key(&nu.instrument, &nu.source, &act.voice.voice_id);
                        match act.action {
                            Action::Articulate => {
                                let hp = if is_noise { Some(Svf::new(sr)) } else { None };
                                eng.filters.insert(key.clone(), (Svf::new(sr), hp));
                                eng.aa_filters.remove(&key); // recreated fresh on first dual sample
                                eng.pitch_bands.remove(&key); // and on the first pitched-noise one
                                glide_info.remove(&key);
                            }
                            Action::Glide if gframes > 0 => {
                                glide_info.insert(key, (act.glide_from, frame, frame + gframes));
                            }
                            Action::Release => {
                                // keep the filter for the release tail; just stop any glide.
                                glide_info.remove(&key);
                            }
                            _ => {}
                        }
                    }
                }

                // stamp envelope clocks at this event's frame (CE-R4.3) — on the event's voice
                // list BEFORE it is installed and polyphony is enforced, so every voice
                // (including this event's fresh articulations) carries a comparable note-age
                // into the steal decision (`enforce_polyphony` steals released-first, then
                // oldest by these clocks; see matrix.rs). The amp envelope is note-relative:
                // the engine reads `t = elapsed_decay_s + frame/sr`, so a fresh articulation
                // sets `elapsed_decay_s = -(frame/sr)` to start its ADSR at t=0 (carries
                // across the loop seam, where loop-end adds total/sr). Without this a note
                // articulated mid-loop would read loop-global time and, with sustain:0, be
                // born already past its decay (silent). A release records its note-relative
                // tail start so the release fade measures from here.
                for act in &vr.actions {
                    match act.action {
                        Action::Articulate => {
                            if let Some(v) = vr.voices.iter_mut().find(|v| v.voice_id == act.voice.voice_id) {
                                v.elapsed_decay_s = -(frame as f32 / sr);
                                // reset per-synth-type DSP state to its deterministic zeroed
                                // initial (ADR 0009), alongside the filter reset above.
                                if let Some(ib) = ib_map.get(&nu.instrument) {
                                    v.synth_state = SynthVoiceState::init_for(&ib.kind);
                                }
                            }
                        }
                        Action::Release => {
                            if let Some(v) = vr.voices.iter_mut().find(|v| v.voice_id == act.voice.voice_id && v.released_at_s.is_none()) {
                                v.released_at_s = Some(v.elapsed_decay_s + frame as f32 / sr);
                            }
                        }
                        _ => {}
                    }
                }

                // The mirror re-strikes where this event articulated a voice (ADR 0048).
                let fresh: std::collections::BTreeSet<String> = if taps.contains_key(&nu.instrument) {
                    vr.actions.iter().filter(|a| a.action == Action::Articulate).map(|a| tap_key(&nu.source, &a.voice.voice_id)).collect()
                } else {
                    std::collections::BTreeSet::new()
                };
                sources.insert(nu.source.clone(), SourceVoices { priority: nu.priority, voices: vr.voices });
                let poly = doc.instruments.get(&nu.instrument).map(|i| i.note_engine.polyphony as usize).unwrap_or(1);
                enforce_polyphony(sources, poly);
                let enabled = doc.instruments.get(&nu.instrument).map(|i| i.enabled).unwrap_or(false);

                // midi note diff (ADR 0026): after the event has fully landed — voices
                // applied, polyphony enforced (steals!), releases stamped — reconcile the
                // instrument's desired note set against the emitted one. Steals and prunes
                // produce no `Action`, so the *set* diff (not the actions) is what
                // guarantees a Note-Off for every ended voice.
                if is_midi && enabled {
                    if let Some(mi) = midi_insts.get_mut(&nu.instrument) {
                        let vel_hint: BTreeMap<String, u8> = velocity_hints(nu, frame).into_iter().collect();
                        let desired = midi_desired(root_st, inst_state.get(&nu.instrument), &mi.state.notes, &vel_hint, mi.velocity_default);
                        diff_notes(&mut mi.state.notes, &desired, mi.channel, frame, midi_events);
                    }
                }
                // The mirror (ADR 0048): the same reconcile for a mirrored audio instrument, read
                // from the voices its synth plays.
                if enabled {
                    if let Some(mi) = taps.get_mut(&nu.instrument) {
                        let vel_hint: BTreeMap<String, u8> = velocity_hints(nu, frame).into_iter().map(|(id, v)| (tap_key(&nu.source, &id), v)).collect();
                        let desired = tap_desired(root_st, inst_state.get(&nu.instrument), &mi.state.notes, &vel_hint, mi.velocity_default);
                        diff_tap(&mut mi.state.notes, &desired, &fresh, &nu.instrument, frame, tap_events);
                    }
                }
            }

            // synthesize
            let mut mix_l = 0.0f32;
            let mut mix_r = 0.0f32;
            // binaural dry (ADR 0047): summed apart and added after the master width stage.
            let (mut bin_l, mut bin_r) = (0.0f32, 0.0f32);
            if hard_lr {
                bin_taps.clear();
            }
            let mut viz_dry = 0.0f32; // selected instrument's dry mono for this frame (viz only)
            for (ii, (inst_name, sources)) in inst_state.iter_mut().enumerate() {
                let Some(ib) = ib_map.get(inst_name) else { continue };
                if !doc.instruments.get(inst_name).map(|i| i.enabled).unwrap_or(false) {
                    continue;
                }
                // midi translators are silent by design (ADR 0026): skip the whole audio
                // path (source/filter/env/pan/sends). Their per_inst_rms entry stays absent
                // ("produced no dry signal"), which downstream reads as rms 0 / inaudible.
                if matches!(ib.kind, SynthKind::Midi) {
                    continue;
                }
                // Per-instrument sub-mix, so each instrument can route its own send amounts.
                let mut inst_l = 0.0f32;
                let mut inst_r = 0.0f32;
                let eff = |param: &str, base: f32| -> f32 { ib.idx.get(param).map(|&i| cur_vals[i]).unwrap_or(base) };
                let level = crate::params::gain_amp(eff("level_db", ib.level));
                let drive = eff("drive_db", ib.drive);
                let cutoff = eff("cutoff_hz", ib.cutoff);
                let resonance = eff("resonance", ib.resonance);
                let attack = eff("attack_s", ib.attack);
                let decay = eff("decay_s", ib.decay);
                let sustain = eff("sustain", ib.sustain);
                let release = eff("release_s", ib.release);
                let band_lo = eff("band_lo_hz", ib.band_lo);
                let band_hi = eff("band_hi_hz", ib.band_hi);
                // octave-pedal: `shift_st` is denormalized to semitones when modulated, else static.
                let shift_st = match ib.idx.get("shift_st") {
                    Some(&i) => {
                        let (lo, hi) = SHIFT_RANGE;
                        lo + cur_vals[i] * (hi - lo)
                    }
                    None => ib.shift_st,
                };
                let shift_mix = eff("shift_mix", ib.shift_mix);
                let cutoff_hz = cutoff_to_hz(cutoff, nyq);
                let band_lo_hz = band_to_hz(band_lo, nyq);
                let band_hi_hz = band_to_hz(band_hi, nyq);

                // Per-synth-type effective params (ADR 0009), read once per frame so they are
                // matrix-modulatable. `osc_b_ratio` is native-ranged, so denorm when modulated.
                let eff_native = |param: &str, base: f32| -> f32 {
                    match ib.idx.get(param) {
                        Some(&i) => param_denorm(param, cur_vals[i]),
                        None => base,
                    }
                };
                let dual_params = match &ib.kind {
                    SynthKind::Dual(c) => Some(DualParams {
                        wave_a: c.wave_a,
                        wave_b: c.wave_b,
                        osc_a_level: crate::params::gain_amp(eff("osc_a_level_db", c.osc_a_level)),
                        osc_b_level: crate::params::gain_amp(eff("osc_b_level_db", c.osc_b_level)),
                        ratio_b: eff_native("osc_b_ratio", c.ratio_b),
                        detune_b: eff("osc_b_detune_cents", c.detune_b),
                        fm_index: eff("fm_index", c.fm_index),
                        fm_feedback: eff("fm_feedback", c.fm_feedback),
                    }),
                    _ => None,
                };
                let modal_params = match &ib.kind {
                    SynthKind::Modal(c) => Some(ModalParams {
                        t60_scale: eff(c.t60_leaf(), c.t60_scale),
                        brightness: eff("brightness", c.brightness),
                        strike_pos: eff("strike_pos", c.strike_pos),
                        stretch: eff("stretch", c.stretch),
                        strike_norm: c.strike_norm,
                    }),
                    _ => None,
                };

                // Binaural (ADR 0047): real-Hz params, read exactly as authored; a route or seam moves
                // them as an offset from the authored value (no normalize round trip, so an idle
                // route still reads 109.5 as 109.5). A route into an unset `root_hz` is ignored —
                // the root keeps following the note.
                let bin_params = match &ib.kind {
                    SynthKind::Binaural(c) => {
                        let eff_hz = |param: &str, authored: f32| -> f32 {
                            match ib.idx.get(param) {
                                Some(&i) => {
                                    let (lo, hi) = crate::loop_doc::param_range(param);
                                    (authored + (cur_vals[i] - base_vals[i]) * (hi - lo)).clamp(lo, hi)
                                }
                                None => authored,
                            }
                        };
                        Some((c.root_hz.map(|r| eff_hz("root_hz", r)), eff_hz("spread_hz", c.spread_hz)))
                    }
                    _ => None,
                };

                let n_total: usize = sources.values().map(|s| s.voices.len()).sum();
                let mut vi = 0usize;
                for (src_id, src) in sources.iter_mut() {
                    for v in src.voices.iter_mut() {
                        key_buf.clear();
                        let _ = write!(key_buf, "{inst_name}::{src_id}::{}", v.voice_id); // = voice_key(…)
                        let key: &str = key_buf.as_str();

                        // glide-resolved pitch (shared by dual/modal; subtractive keeps its own
                        // verbatim path below for bit-identical output — ADR 0009).
                        let resolve_pitch = |target: f32| -> f32 {
                            match glide_info.get(key) {
                                Some(&(from, start, end)) if frame < end => {
                                    if frame <= start {
                                        from
                                    } else {
                                        let tt = (frame - start) as f32 / (end - start) as f32;
                                        from + tt * (target - from)
                                    }
                                }
                                _ => target,
                            }
                        };
                        // note-relative envelope/strike clock (CE-R4.3).
                        let t = v.elapsed_decay_s + frame as f32 / sr;

                        // source stage — dispatch on synth type (ADR 0009). `s_r` is the right side of
                        // a stereo source (binaural, ADR 0047); `None` for every mono type, whose
                        // path below is unchanged.
                        let mut s_r: Option<f32> = None;
                        let mut s = match &ib.kind {
                            SynthKind::Subtractive => {
                                if ib.is_noise {
                                    let h = hash_site(doc.loop_id, &v.voice_id, frame as u64, "noise");
                                    hash_to_unit(h) * 2.0 - 1.0
                                } else {
                                    let spread = voice_spread_cents(ib.detune, vi, n_total);
                                    let pitch = match glide_info.get(key) {
                                        Some(&(from, start, end)) if frame < end => {
                                            if frame <= start {
                                                from
                                            } else {
                                                let t = (frame - start) as f32 / (end - start) as f32;
                                                from + t * (v.pitch_semitones - from)
                                            }
                                        }
                                        _ => v.pitch_semitones,
                                    };
                                    let hz = pitch_to_hz(&doc.harmony, pitch, v.cents_offset + spread);
                                    v.phase_rad = (v.phase_rad + crate::dsp::TWO_PI * hz / sr).rem_euclid(crate::dsp::TWO_PI);
                                    let dry = osc(ib.wave, v.phase_rad);
                                    // octave-pedal crossfade: blend in a second oscillator shifted by
                                    // `shift_st` semitones (gliding in parallel). mix=0 ⇒ pure dry.
                                    if shift_mix > 1e-4 {
                                        let hz_w = pitch_to_hz(&doc.harmony, pitch + shift_st, v.cents_offset + spread);
                                        v.phase_rad_shift = (v.phase_rad_shift + crate::dsp::TWO_PI * hz_w / sr).rem_euclid(crate::dsp::TWO_PI);
                                        (1.0 - shift_mix) * dry + shift_mix * osc(ib.wave, v.phase_rad_shift)
                                    } else {
                                        dry
                                    }
                                }
                            }
                            SynthKind::Dual(_) => {
                                let dp = dual_params.as_ref().unwrap();
                                let spread = voice_spread_cents(ib.detune, vi, n_total);
                                let pitch = resolve_pitch(v.pitch_semitones);
                                let hz = pitch_to_hz(&doc.harmony, pitch, v.cents_offset + spread);
                                // osc + PM + drive run oversampled inside, then decimate (ADR 0009).
                                if !eng.aa_filters.contains_key(key) {
                                    eng.aa_filters.insert(key.to_string(), Aa2x::new());
                                }
                                let aa = eng.aa_filters.get_mut(key).unwrap();
                                dual_sample(dp, &mut v.synth_state, &mut v.phase_rad, aa, drive, hz, sr)
                            }
                            SynthKind::Modal(c) => {
                                let mpp = modal_params.as_ref().unwrap();
                                let pitch = resolve_pitch(v.pitch_semitones);
                                let hz = pitch_to_hz(&doc.harmony, pitch, v.cents_offset);
                                // `t60_mode: "note"` couples the ring to this note's source step.
                                let gate_s = if c.t60_from_note { src_step_s.get(&v.source_id).copied() } else { None };
                                modal_sample(&c.partials, mpp, &mut v.synth_state, t, v.released_at_s, gate_s, hz, sr)
                            }
                            SynthKind::Binaural(_) => {
                                let (root_hz, spread) = bin_params.unwrap();
                                let detune = voice_spread_cents(ib.detune, vi, n_total);
                                let pedal = shift_mix > 1e-4;
                                // centre (and octave-pedal) frequency: a pinned `root_hz`, the note's
                                // pitch otherwise (glide, cents and unison detune as subtractive).
                                let (hz, hz_shift) = match root_hz {
                                    Some(root) => {
                                        let hz = binaural_root(root, detune);
                                        (hz, pedal.then(|| hz * 2.0_f32.powf(shift_st / 12.0)))
                                    }
                                    None => {
                                        let pitch = resolve_pitch(v.pitch_semitones);
                                        let hz = pitch_to_hz(&doc.harmony, pitch, v.cents_offset + detune);
                                        (hz, pedal.then(|| pitch_to_hz(&doc.harmony, pitch + shift_st, v.cents_offset + detune)))
                                    }
                                };
                                let (l, r) = binaural_sample(ib.wave, &mut v.synth_state, &mut v.phase_rad, &mut v.phase_rad_shift, hz, hz_shift, spread, shift_mix, sr);
                                s_r = Some(r);
                                l
                            }
                            // unreachable — midi instruments are skipped above (ADR 0026);
                            // silence keeps the arm total without a panic path in the hot loop.
                            SynthKind::Midi => 0.0,
                        };

                        // `dual` already applied drive inside its oversampled source; others soft-clip
                        // here at base rate (ADR 0009).
                        if !ib.kind.drive_in_source() {
                            s = soft_clip(s, drive);
                            if let Some(r) = s_r.as_mut() {
                                *r = soft_clip(*r, drive);
                            }
                        }

                        // per-voice filter (subtractive/dual). Modal supplies its own decay, so it
                        // skips the resonant filter (ADR 0009).
                        if ib.kind.uses_engine_filter() {
                            if !eng.filters.contains_key(key) {
                                eng.filters.insert(key.to_string(), (Svf::new(sr), if ib.is_noise { Some(Svf::new(sr)) } else { None }));
                            }
                            let entry = eng.filters.get_mut(key).unwrap();
                            if ib.is_noise {
                                entry.0.set_cutoff(band_hi_hz);
                                s = entry.0.lowpass(s);
                                if let Some(hp) = entry.1.as_mut() {
                                    hp.set_cutoff(band_lo_hz);
                                    s = hp.process(s).hp;
                                }
                                // Pitched noise (ADR 0046): the source band, on through the note's band.
                                if ib.pitched_noise {
                                    let hz = pitch_to_hz(&doc.harmony, resolve_pitch(v.pitch_semitones), v.cents_offset);
                                    if !eng.pitch_bands.contains_key(key) {
                                        eng.pitch_bands.insert(key.to_string(), PitchBand::new(sr));
                                    }
                                    s = eng.pitch_bands.get_mut(key).unwrap().process(s, hz, resonance, band_lo_hz, band_hi_hz);
                                }
                            } else {
                                entry.0.set(cutoff_hz, resonance);
                                s = entry.0.lowpass(s);
                                // binaural right side: its own lowpass in the second slot (ADR 0047),
                                // created fresh after each articulation.
                                if let Some(r) = s_r.as_mut() {
                                    let rf = entry.1.get_or_insert_with(|| Svf::new(sr));
                                    rf.set(cutoff_hz, resonance);
                                    *r = rf.lowpass(*r);
                                }
                            }
                        }

                        // amplitude: ADSR (default) / legacy `event` bell / modal-intrinsic (ADR 0009).
                        let env = match ib.kind.amp_policy(ib.is_event) {
                            AmpPolicy::Bell => {
                                if decay > 0.001 {
                                    let decay_s = (decay * DECAY_MAX_S).max(0.001);
                                    (-3.0 * t / decay_s).exp()
                                } else {
                                    1.0
                                }
                            }
                            AmpPolicy::Adsr => adsr_env(t, v.released_at_s, attack * ATTACK_MAX_S, decay * DECAY_MAX_S, sustain, release * RELEASE_MAX_S),
                            AmpPolicy::Intrinsic => 1.0,
                        };

                        s *= env * level;
                        if let Some(r) = s_r.as_mut() {
                            *r *= env * level;
                        }
                        match s_r {
                            None => {
                                let pan = (hash_to_unit(hash64(0, fnv1a_bytes(v.voice_id.as_bytes()), 0, 1)) - 0.5) * 0.4;
                                inst_l += s * (1.0 - pan.max(0.0));
                                inst_r += s * (1.0 + pan.min(0.0));
                            }
                            // binaural: hard L / hard R, no voice pan (ADR 0047).
                            Some(r) => {
                                inst_l += s;
                                inst_r += r;
                            }
                        }
                        vi += 1;
                    }
                }

                // Live gain (mute/solo, ADR 0035): a ~10 ms ramp before the dry mix and the sends.
                // Exactly 1.0 (the default, and every offline render) skips the multiply.
                if let Some(g) = gains.get_mut(ii) {
                    if g.0 != 1.0 || g.1 != 1.0 {
                        g.0 = if g.0 < g.1 { (g.0 + gain_step).min(g.1) } else { (g.0 - gain_step).max(g.1) };
                        inst_l *= g.0;
                        inst_r *= g.0;
                    }
                }
                if let Some(m) = meters.as_mut() {
                    m[ii][0] = m[ii][0].max(inst_l.abs());
                    m[ii][1] = m[ii][1].max(inst_r.abs());
                }

                // dry to master + per-instrument sends into buses. A binaural pair's dry skips the
                // master width (ADR 0047): it joins after the mid/side stage, gain only.
                let bypass = ib.kind.hard_lr();
                if bypass {
                    bin_l += inst_l;
                    bin_r += inst_r;
                } else {
                    mix_l += inst_l;
                    mix_r += inst_r;
                }
                // ADR 0027: mirror the dry into its `out_channel` stem (or the rest-of-mix).
                // Additional adds into separate buffers only — the master math above is untouched.
                // A binaural dry is tapped and added after the stems' width stage, like the master's.
                if bypass {
                    if stem_mix.is_some() {
                        bin_taps.push((stem_route.get(inst_name).copied(), inst_l, inst_r));
                    }
                } else if let Some(sm) = stem_mix.as_mut() {
                    let buf = match stem_route.get(inst_name) {
                        Some(oc) => sm.stems.get_mut(oc).unwrap(),
                        None => &mut sm.rest,
                    };
                    buf[2 * frame] += inst_l;
                    buf[2 * frame + 1] += inst_r;
                }
                if let Some(buf) = src_stems.as_mut().and_then(|s| s.inst.get_mut(inst_name)) {
                    buf[2 * frame] = inst_l;
                    buf[2 * frame + 1] = inst_r;
                }
                // per-track loudness (dry, pre-bus/width/gain) — observation only, never fed back.
                *inst_sumsq.entry(inst_name.clone()).or_insert(0.0) += 0.5 * (inst_l * inst_l + inst_r * inst_r) as f64;
                if viz_on && viz_inst.as_deref() == Some(inst_name.as_str()) {
                    viz_dry = 0.5 * (inst_l + inst_r); // selected instrument's dry hop sample
                }
                if let Some(routes) = sends_of.get(inst_name) {
                    for (bus, amt) in routes {
                        if let Some(b) = eng.buses.get_mut(bus) {
                            b.add_send(inst_l, inst_r, *amt);
                        }
                    }
                }
            }

            // globals: buses (effects chains, persistent tails) return to the master mix, then
            // master stereo width → gain.
            let width = width_idx.map(|i| cur_vals[i]).unwrap_or(doc.global.stereo_width);
            let gain = gain_idx.map_or_else(|| doc.global.master_amp(), |i| crate::params::gain_amp(cur_vals[i]));

            let mut l = mix_l;
            let mut r = mix_r;
            let (mut wet_sum_l, mut wet_sum_r) = (0.0f32, 0.0f32);
            for (bus_id, bus) in eng.buses.iter_mut() {
                let (wet_l, wet_r) = bus.process();
                l += wet_l;
                r += wet_r;
                wet_sum_l += wet_l;
                wet_sum_r += wet_r;
                if let Some(buf) = src_stems.as_mut().and_then(|s| s.bus.get_mut(bus_id)) {
                    buf[2 * frame] = wet_l;
                    buf[2 * frame + 1] = wet_r;
                }
            }
            // Retired buses (ADR 0035): no sends, their tails ring out until silent.
            for rb in eng.retiring.iter_mut() {
                let (wet_l, wet_r) = rb.process();
                l += wet_l;
                r += wet_r;
                wet_sum_l += wet_l;
                wet_sum_r += wet_r;
            }

            let side_gain = width * 2.0;
            let mid = (l + r) * 0.5;
            let side = (r - l) * 0.5;
            l = mid - side * side_gain;
            r = mid + side * side_gain;
            // ADR 0047: the binaural dry, outside the width (only when the doc has one, so no
            // `x + 0.0` ever flips a −0.0 in a doc without it).
            if hard_lr {
                l += bin_l;
                r += bin_r;
            }

            // Master out is a plain `mix · master_gain` — **no master limiter in renders** (ADR 0021,
            // 0031): renders feed stems / a DAW, where dynamics are the mastering engineer's call (a
            // live-only protection limiter lives in the player shell). Headroom is the doc author's
            // responsibility (per-voice `drive_db` soft-clips locally); bundled fixtures are gain-staged
            // to stay ≤ 1.0 FS (see `fixtures_do_not_hard_clip`).
            audio[2 * frame] = l * gain;
            audio[2 * frame + 1] = r * gain;

            // ADR 0027: finish this frame's stems — bus wet lands in the rest-of-mix, then every
            // stem gets the SAME per-sample width/gain chain the master got (the mid/side matrix is
            // linear, so `rest + Σ stems` reconstructs the master up to float association).
            if let Some(sm) = stem_mix.as_mut() {
                sm.rest[2 * frame] += wet_sum_l;
                sm.rest[2 * frame + 1] += wet_sum_r;
                width_gain_in_place(&mut sm.rest, frame, side_gain, gain);
                for b in sm.stems.values_mut() {
                    width_gain_in_place(b, frame, side_gain, gain);
                }
                // binaural dry (ADR 0047): after the width, gain only — as on the master, so
                // `rest + Σ stems` still sums to it (the mid/side map is linear).
                for &(oc, bl, br) in bin_taps.iter() {
                    let buf = match oc {
                        Some(oc) => sm.stems.get_mut(&oc).unwrap(),
                        None => &mut sm.rest,
                    };
                    buf[2 * frame] += bl * gain;
                    buf[2 * frame + 1] += br * gain;
                }
            }
            // ADR 0030: the same width/gain chain on every per-source stem — a binaural pair's gets
            // the gain only, as its dry got on the master (ADR 0047).
            if let Some(src) = src_stems.as_mut() {
                if hard_lr {
                    for (id, b) in src.inst.iter_mut() {
                        if ib_map.get(id).is_some_and(|ib| ib.kind.hard_lr()) {
                            gain_in_place(b, frame, gain);
                        } else {
                            width_gain_in_place(b, frame, side_gain, gain);
                        }
                    }
                    for b in src.bus.values_mut() {
                        width_gain_in_place(b, frame, side_gain, gain);
                    }
                } else {
                    for b in src.inst.values_mut().chain(src.bus.values_mut()) {
                        width_gain_in_place(b, frame, side_gain, gain);
                    }
                }
            }

            // Live-viz: accumulate the master + selected-dry mono hop; flush a 25 Hz frame + the
            // normalized effective-param snapshot when the hop fills. Reads only — never feeds back.
            if viz_on {
                cur_master.push(0.5 * (audio[2 * frame] + audio[2 * frame + 1]));
                cur_inst.push(viz_dry);
                if cur_master.len() >= viz_hop {
                    let eff: BTreeMap<String, f32> = index_of.iter().map(|(d, &i)| (d.clone(), cur_vals[i])).collect();
                    viz_master_hops.push(std::mem::take(cur_master));
                    viz_inst_dry_hops.push(std::mem::take(cur_inst));
                    viz_eff_hops.push(eff);
                }
            }
        }
        eng.retiring.retain(|rb| !rb.finished());
        self.pos = end;
        start..end
    }

    /// Finish the loop (rendering any frames left): the next snapshot + the [`RenderResult`].
    pub fn finish(mut self, eng: &mut Engine) -> RenderResult {
        let rest = self.total - self.pos;
        if rest > 0 {
            self.render_block(eng, rest);
        }
        let sr = self.sr;
        let total = self.total;
        let mut next = std::mem::take(&mut self.next);
        if total == 0 {
            return RenderResult { audio: self.audio, channels: 2, num_frames: 0, next_snapshot: next, effective_state: BTreeMap::new(), per_inst_rms: BTreeMap::new(), viz_master_hops: Vec::new(), viz_inst_dry_hops: Vec::new(), viz_eff_hops: Vec::new(), midi_events: Vec::new(), note_tap: Vec::new(), note_log: None, stems: None, sources: None };
        }

        // 7. Next snapshot: carry voices (accumulate decay) + prune stale filters.
        let mut present_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (name, sources) in self.inst_state.iter() {
            // release-tail length: ADSR release (subtractive/dual), 0 for the `event` bell, the
            // modal ring length for modal (so its intrinsic tail isn't truncated — ADR 0009).
            let ib = self.ib_map.get(name);
            let base_release_s = ib.map(|b| b.release_tail_s).unwrap_or(0.0);
            // `t60_mode: "note"` modal rings are per-voice (they depend on the driving source's step),
            // so the prune tail is computed per-voice from that step rather than the instrument default.
            let note_modal = match ib.map(|b| &b.kind) {
                Some(SynthKind::Modal(c)) if c.t60_from_note => Some(c),
                _ => None,
            };
            let mut avs = Vec::new();
            for (source_id, src) in sources.iter() {
                let release_s = match note_modal {
                    Some(c) => self.src_step_s.get(source_id).map(|&step| c.release_tail_for_note(step)).unwrap_or(base_release_s),
                    None => base_release_s,
                };
                for v in &src.voices {
                    let mut av = v.clone();
                    av.elapsed_decay_s += total as f32 / sr;
                    // drop voices whose release tail has finished (filter pruned via present_keys).
                    if let Some(rel) = av.released_at_s {
                        if av.elapsed_decay_s - rel >= release_s {
                            continue;
                        }
                    }
                    present_keys.insert(voice_key(name, source_id, &v.voice_id));
                    avs.push(av);
                }
            }
            if !avs.is_empty() {
                next.active_voices.insert(format!("{name}.notes"), avs);
            }
        }
        eng.filters.retain(|k, _| present_keys.contains(k));
        eng.aa_filters.retain(|k, _| present_keys.contains(k));
        eng.pitch_bands.retain(|k, _| present_keys.contains(k));
        // Carry each midi instrument's emitted state (notes still sounding externally +
        // last-sent CC values) across the seam (ADR 0026). Carried even while disabled —
        // cc_last reflects what the external synth last received.
        for (name, mi) in std::mem::take(&mut self.midi_insts) {
            next.midi_out.insert(name, mi.state);
        }
        // The note log (ADR 0048): the sources feeding each mirrored instrument, and their passes.
        let note_log = eng.note_log.then(|| {
            let mut feeds: BTreeMap<String, std::collections::BTreeSet<String>> = self.taps.keys().map(|n| (n.clone(), Default::default())).collect();
            for e in self.doc.matrix.values().filter(|e| e.kind == "note") {
                if let Dest::Notes { inst } = resolve_dest(&e.dest) {
                    if let Some(set) = feeds.get_mut(&inst) {
                        set.insert(e.source.clone());
                    }
                }
            }
            let tiles = feeds.values().flatten().filter_map(|src| self.mod_out.get(src).map(|o| (src.clone(), o.tiles.clone()))).collect();
            LoopNoteLog { tiles, feeds: feeds.into_iter().map(|(n, set)| (n, set.into_iter().collect())).collect() }
        });
        // The mirror's emitted notes carry the same way (ADR 0048).
        for (name, mi) in std::mem::take(&mut self.taps) {
            next.note_tap.insert(name, mi.state);
        }
        // A glide longer than this loop continues into the next render (orphans were released
        // already, so that flag doesn't carry).
        if let Some(sm) = &self.start.seam {
            let elapsed = sm.elapsed_frames + total as u64;
            if elapsed < sm.total_frames {
                next.seam = Some(Seam { elapsed_frames: elapsed, release_orphans: false, ..sm.clone() });
            }
        }
        // Live gains: the ramp position carries into the next loop.
        for (name, &(cur, _)) in self.inst_state.keys().zip(self.gains.iter()) {
            if cur == 1.0 {
                eng.live_gain_cur.remove(name);
            } else {
                eng.live_gain_cur.insert(name.clone(), cur);
            }
        }

        // 8. Effective state (loop-end values).
        let mut effective_state = BTreeMap::new();
        for (dest, &i) in self.index_of.iter() {
            effective_state.insert(dest.clone(), self.cur_vals[i]);
        }
        let doc = &self.doc;
        effective_state.entry("global.stereo_width".into()).or_insert(doc.global.stereo_width);
        effective_state.entry("global.master_gain_db".into()).or_insert(crate::params::GAIN.inverse(doc.global.master_gain_db));
        for (id, out) in &self.mod_out {
            effective_state.insert(format!("mod.{id}.last_output"), out.next_phase.last_output);
        }

        let per_inst_rms = std::mem::take(&mut self.inst_sumsq).into_iter().map(|(name, sumsq)| (name, (sumsq / total.max(1) as f64).sqrt() as f32)).collect();

        // Flush a trailing partial hop so the TUI sees the whole loop (it handles the short hop).
        if self.viz_on && !self.cur_master.is_empty() {
            let eff: BTreeMap<String, f32> = self.index_of.iter().map(|(d, &i)| (d.clone(), self.cur_vals[i])).collect();
            self.viz_master_hops.push(std::mem::take(&mut self.cur_master));
            self.viz_inst_dry_hops.push(std::mem::take(&mut self.cur_inst));
            self.viz_eff_hops.push(eff);
        }

        RenderResult {
            audio: self.audio,
            channels: 2,
            num_frames: total,
            next_snapshot: next,
            effective_state,
            per_inst_rms,
            viz_master_hops: self.viz_master_hops,
            viz_inst_dry_hops: self.viz_inst_dry_hops,
            viz_eff_hops: self.viz_eff_hops,
            midi_events: self.midi_events,
            note_tap: self.tap_events,
            note_log,
            stems: self.stem_mix,
            sources: self.src_stems,
        }
    }

    /// Mid-loop apply (ADR 0035): this loop, continued from the current position under `new_doc`.
    /// `eng` must be a **fork** of the engine this loop renders on (the caller crossfades the two).
    ///
    /// The new loop is re-evaluated from the loop-start phases (modulators are pure) and seeded with
    /// the *current* runtime state: carried voices (their envelope clocks continuous; voices whose
    /// note route is gone released here; a changed synth type re-struck), MIDI emitted state,
    /// glides, and every smoothed destination by path (so a value moves from where it is). Where a
    /// note source's current voicing differs from what the new doc says should be sounding, an
    /// event at the edit point applies it now (same prob rolls — same `loop_id` + frames). A changed
    /// loop length keeps the beat position (a tempo edit scales the frame; extra steps don't move it). Buses reconcile on the fork (a changed chain
    /// retires the old bus so its tail rings out).
    pub fn retarget(&self, eng: &mut Engine, new_doc: &LoopDoc) -> (LoopRender, EditKind) {
        let mut doc = new_doc.clone();
        doc.loop_id = self.doc.loop_id;
        let new_total = doc.total_frames(self.sr as f64);
        let pos = self.pos;
        // Keep the BEAT position: a tempo edit scales the frame, a step-count edit at the same tempo
        // keeps it (step 4 stays step 4). Past the end of a shortened loop ⇒ the loop ends now.
        let beats = |d: &LoopDoc| d.transport.master_steps.max(1) as u128;
        let new_pos = if new_total == self.total || self.total == 0 {
            pos
        } else {
            ((pos as u128 * new_total as u128 * beats(&self.doc)) / (self.total as u128 * beats(&doc))) as usize
        };
        let new_pos = new_pos.min(new_total);
        let shift_s = (pos as f32 - new_pos as f32) / self.sr;

        // Seed: the loop-start snapshot, with the CURRENT voices and MIDI state.
        let mut snap = self.start.clone();
        snap.active_voices.clear();
        for (name, sources) in &self.inst_state {
            let mut avs: Vec<ActiveVoice> = sources.values().flat_map(|sv| sv.voices.iter().cloned()).collect();
            for v in avs.iter_mut() {
                v.elapsed_decay_s += shift_s; // keep `elapsed + frame/sr` continuous across a length change
            }
            if !avs.is_empty() {
                snap.active_voices.insert(format!("{name}.notes"), avs);
            }
        }
        snap.midi_out = self.midi_insts.iter().map(|(n, mi)| (n.clone(), mi.state.clone())).collect();
        snap.note_tap = self.tap_states();
        if let Some(sm) = snap.seam.as_mut() {
            sm.release_orphans = false;
        }
        let mut lr = eng.begin_at(&doc, &snap, new_pos);
        lr.start = self.start.clone();
        lr.meters = self.meters.as_ref().map(|_| vec![[0.0; 2]; lr.inst_state.len()]);

        let mut structural = new_total != self.total || crate::controller::doc_swap_is_structural(&self.doc, &doc);

        // Voices: a changed synth type re-strikes its held voices; a vanished note route releases.
        let mut prio: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
        for e in doc.matrix.values().filter(|e| e.kind == "note") {
            if let Dest::Notes { inst } = resolve_dest(&e.dest) {
                prio.insert((inst, e.source.clone()));
            }
        }
        let t_now = new_pos as f32 / lr.sr;
        for (name, sources) in lr.inst_state.iter_mut() {
            let kind_changed = match (self.ib_map.get(name), lr.ib_map.get(name)) {
                (Some(a), Some(b)) => std::mem::discriminant(&a.kind) != std::mem::discriminant(&b.kind),
                _ => false,
            };
            for (source, sv) in sources.iter_mut() {
                if kind_changed {
                    structural = true;
                    sv.voices.retain(|v| v.released_at_s.is_none());
                    for v in sv.voices.iter_mut() {
                        v.elapsed_decay_s = -t_now;
                        if let Some(ib) = lr.ib_map.get(name) {
                            v.synth_state = SynthVoiceState::init_for(&ib.kind);
                        }
                        let key = voice_key(name, source, &v.voice_id);
                        eng.filters.remove(&key);
                        eng.aa_filters.remove(&key);
                        eng.pitch_bands.remove(&key);
                    }
                } else if !prio.contains(&(name.clone(), source.clone())) {
                    for v in sv.voices.iter_mut().filter(|v| v.released_at_s.is_none()) {
                        v.released_at_s = Some(v.elapsed_decay_s + t_now);
                    }
                }
            }
        }
        // The voices released just above end in MIDI at the edit point too: `begin_at` reconciled
        // there before they were released, so their Note-Offs would otherwise wait for the next
        // note event or the loop top.
        {
            let no_hints: BTreeMap<String, u8> = BTreeMap::new();
            let LoopRender { doc, inst_state, midi_insts, midi_events, taps, tap_events, root_st, .. } = &mut lr;
            for (name, mi) in midi_insts.iter_mut() {
                if doc.instruments.get(name).is_some_and(|i| i.enabled) {
                    let desired = midi_desired(*root_st, inst_state.get(name), &mi.state.notes, &no_hints, mi.velocity_default);
                    diff_notes(&mut mi.state.notes, &desired, mi.channel, new_pos, midi_events);
                }
            }
            let no_fresh = std::collections::BTreeSet::new();
            for (name, mi) in taps.iter_mut() {
                if doc.instruments.get(name).is_some_and(|i| i.enabled) {
                    let desired = tap_desired(*root_st, inst_state.get(name), &mi.state.notes, &no_hints, mi.velocity_default);
                    diff_tap(&mut mi.state.notes, &desired, &no_fresh, name, new_pos, tap_events);
                }
            }
        }

        // Smoothed destinations carry by path; cursors seek to the edit point with the target set
        // from the latest contribution before it.
        for (dest, &i) in lr.index_of.iter() {
            if let Some(&j) = self.index_of.get(dest) {
                lr.smoothed[i] = self.smoothed[j].clone();
            }
            let evs = &lr.events[i];
            let c = evs.partition_point(|&(f, _)| f < new_pos);
            lr.cursors[i] = c;
            let target = clamp01(lr.base_vals[i] + if c > 0 { evs[c - 1].1 } else { 0.0 });
            lr.smoothed[i].set_target(target);
            lr.cur_vals[i] = lr.smoothed[i].value();
        }

        // Notes: skip what already played; where a source's sounding voicing differs from the new
        // doc's latest event before the edit point, apply the new voicing right here.
        let last_before = |ups: &[NoteUpdate], at: usize, inst: &str, src: &str| -> Option<NoteUpdate> {
            ups.iter().filter(|u| u.frame < at && u.instrument == inst && u.source == src).last().cloned()
        };
        let mut injected: Vec<NoteUpdate> = Vec::new();
        for (inst, src) in &prio {
            let Some(new_u) = last_before(&lr.note_updates, new_pos, inst, src) else { continue };
            let old_u = last_before(&self.note_updates, pos, inst, src);
            if old_u.as_ref().is_none_or(|o| o.voicing != new_u.voicing) {
                injected.push(NoteUpdate { frame: new_pos, ..new_u });
            }
        }
        let first = lr.note_updates.partition_point(|u| u.frame < new_pos);
        let tail: Vec<NoteUpdate> = lr.note_updates.split_off(first);
        lr.note_updates.extend(injected);
        lr.note_updates.extend(tail);
        lr.ni = first;

        // Glides in flight keep going (shifted with a length change).
        let df = new_pos as i64 - pos as i64;
        lr.glide_info = self.glide_info.iter().map(|(k, &(from, s, e))| (k.clone(), (from, (s as i64 + df).max(0) as usize, (e as i64 + df).max(0) as usize))).collect();

        // What already rendered stays in the loop's record (observation only).
        let keep = new_pos.min(pos).min(lr.total);
        lr.audio[..2 * keep].copy_from_slice(&self.audio[..2 * keep]);
        lr.inst_sumsq = self.inst_sumsq.clone();
        let mut midi = self.midi_events.clone();
        midi.extend(lr.midi_events.drain(..));
        lr.midi_events = midi;
        lr.midi_sent = self.midi_sent;
        let mut taps = self.tap_events.clone();
        taps.extend(lr.tap_events.drain(..));
        lr.tap_events = taps;
        lr.tap_sent = self.tap_sent;
        lr.viz_master_hops = self.viz_master_hops.clone();
        lr.viz_inst_dry_hops = self.viz_inst_dry_hops.clone();
        lr.viz_eff_hops = self.viz_eff_hops.clone();
        lr.cur_master = self.cur_master.clone();
        lr.cur_inst = self.cur_inst.clone();
        // Live gains continue from where they are.
        for (i, name) in lr.inst_state.keys().enumerate() {
            if let Some(j) = self.inst_state.keys().position(|n| n == name) {
                lr.gains[i] = self.gains[j];
            }
        }
        (lr, if structural { EditKind::Structural } else { EditKind::Value })
    }
}

/// A send bus taken out of service (removed, or its effect chain edited): it takes no sends and
/// rings out, then is dropped once silent (or after a cap).
#[derive(Clone)]
pub(super) struct Retiring {
    bus: Bus,
    quiet: usize,
    age: usize,
}

impl Retiring {
    /// Silence threshold, how long it must hold, and the absolute cap (frames at any rate ~48 k).
    const SILENT: f32 = 1e-5;
    const QUIET_FRAMES: usize = 24_000;
    const MAX_FRAMES: usize = 48_000 * 15;

    pub(super) fn new(bus: Bus) -> Self {
        Self { bus, quiet: 0, age: 0 }
    }
    fn process(&mut self) -> (f32, f32) {
        let (l, r) = self.bus.process();
        self.age += 1;
        if l.abs().max(r.abs()) < Self::SILENT {
            self.quiet += 1;
        } else {
            self.quiet = 0;
        }
        (l, r)
    }
    fn finished(&self) -> bool {
        self.quiet >= Self::QUIET_FRAMES || self.age >= Self::MAX_FRAMES
    }
}
