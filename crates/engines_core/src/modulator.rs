//! Modulator evaluation (CE-R5.3): `lfo` / `step_seq`, control & note domains, euclid
//! generator, `pitch_walk`, loop/free sync with phase carried in [`ModulatorPhase`].
//!
//! Control outputs are sampled on a fine grid ([`CONTROL_HOP`]) so the engine can ramp
//! them zipper-free via [`crate::dsp::Smoothed`] (CE-R4.5). Note outputs are discrete
//! events. Probability rolls happen in the engine (needs `loop_id` + voice index);
//! `pitch_walk` random order is seeded here via [`crate::hash`] (CE-R7.1).

use crate::dsp::{osc, Wave, TWO_PI};
use crate::engine::{ModulatorPhase, PendingNote};
use crate::euclid::euclidean_rhythm;
use crate::hash::{hash_site, hash_to_unit};
use crate::loop_doc::{LoopDoc, Modulator, NoteVoice, PhraseDef, PhraseRef};

/// Root seed for every phrase-arm hash site (selection + prob rolls + jitter). Deliberately
/// NOT `loop_id`: a tile can straddle the loop seam, and both sides must recompute identical
/// choices; variety comes from the ever-growing tile ordinal instead (ADR 0028).
const PHRASE_SEED: u64 = 0xAD0028;

// ── Feel (ADR 0028 — finally applying the parsed-but-dead CE-R5.2 fields) ───

/// Swing delay for the on-grid onset at absolute step `step_i`: odd steps are delayed by
/// `swing · step_dur/3`, so swing 1 puts the offbeat on the 2/3 triplet point. Fractional-`t`
/// (sub-step) onsets are authored micro-timing and never swing.
fn swing_delay(step_i: i64, swing: f32, step_dur_frames: usize) -> i64 {
    if swing <= 0.0 || step_i.rem_euclid(2) == 0 {
        0
    } else {
        (swing.min(1.0) * step_dur_frames as f32 / 3.0).round() as i64
    }
}

/// Seeded humanize offset: `±amp_frames`, uniform, deterministic per hash site `h`.
fn jitter_offset(amp_frames: f32, h: u64) -> i64 {
    if amp_frames <= 0.0 {
        0
    } else {
        ((2.0 * hash_to_unit(h) - 1.0) * amp_frames).round() as i64
    }
}

/// Control-rate sampling hop (samples). ~1.3 ms @ 48 kHz; the engine smooths between.
pub const CONTROL_HOP: usize = 64;

#[derive(Clone, Debug)]
pub struct ControlEvent {
    pub frame: usize,
    pub value: f32,
}

#[derive(Clone, Debug)]
pub struct NoteEventOut {
    pub frame: usize,
    pub voicing: Vec<NoteVoice>,
}

#[derive(Clone, Debug, Default)]
pub struct ModulatorOutput {
    pub control_events: Vec<ControlEvent>,
    pub note_events: Vec<NoteEventOut>,
    pub next_phase: ModulatorPhase,
    /// Where this note source's passes start in the loop (ADR 0048): one per phrase tile, or per
    /// cycle of a steps / generator pattern. Observation only.
    pub tiles: Vec<TileMark>,
}

/// The start of one pass of a note source (ADR 0048): a phrase arm's tile, or a cycle of a steps /
/// generator pattern. A pass ends where the source's next one starts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TileMark {
    /// Loop-relative frame of the pass's grid start. Negative for a free-run phrase tile that
    /// started last loop and straddles the seam (reported again: an Orrery refit at the loop top
    /// may have changed what it plays) — same `ordinal`.
    pub frame: i64,
    /// The pass's count: a phrase arm's tile ordinal, a pattern's cycle index.
    pub ordinal: i64,
    /// The pool entry the tile plays (phrase arms; the Orrery's refit names the entry it refits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The pass length in frames as started (the next pass's start is the authority).
    pub len_frames: u64,
    /// Jitter reach in frames: the pass's head may land this much before `frame`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub margin: u32,
    /// The tile plays the Orrery's chord-refit copy (`phrase.realized`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub realized: bool,
}

fn is_zero_u32(x: &u32) -> bool {
    *x == 0
}

/// Step duration as a fraction of the master loop (parses `"1/N"`, else `1/length`).
pub fn step_duration_frac(m: &Modulator) -> f32 {
    if let Some(sd) = &m.step_duration {
        if let Some(frac) = parse_step_duration(sd) {
            return frac;
        }
    }
    if m.length > 0 {
        1.0 / m.length as f32
    } else {
        1.0
    }
}

/// Parse an explicit `step_duration` (`"n/d"`, a fraction of the master loop) — `Some` only
/// for a positive, finite fraction (`"1/0"`, `"0/8"` and garbage are `None`; `validate()`
/// rejects those at the authoring boundary so the `1/length` fallback never masks a typo).
pub fn parse_step_duration(sd: &str) -> Option<f32> {
    let (n, d) = sd.split_once('/')?;
    let (n, d) = (n.trim().parse::<f32>().ok()?, d.trim().parse::<f32>().ok()?);
    (n > 0.0 && d > 0.0 && n.is_finite() && d.is_finite()).then(|| n / d)
}

/// Under loop-sync, how many host steps START inside one loop: the first `k` with
/// `k · step_duration ≥ 1 loop`, read from the authored fraction (`"n/d"` ⇒ ⌈d/n⌉, the `1/length`
/// default ⇒ `length`). The frame grid puts step `k` at `k · round(step · total)`; where that rounds
/// the step short (96 × 5333 = 511968 of a 512000-frame loop), step ⌈d/n⌉ would otherwise land a few
/// frames before the seam — the next loop's step 0 played twice (a re-struck note, or a whole extra
/// phrase tile carried over the seam onto the next loop's first one).
fn loop_sync_steps(m: &Modulator) -> i64 {
    let ratio = m.step_duration.as_deref().filter(|sd| parse_step_duration(sd).is_some()).and_then(|sd| {
        let (n, d) = sd.split_once('/')?;
        Some((n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?))
    });
    match ratio {
        Some((n, d)) => (d / n).ceil() as i64,
        None => m.length.max(1) as i64,
    }
}

/// Interpolated control value at a fractional local-step position (CE-R5.3 `entry_value`
/// continuity + `interp`).
fn control_value_at(m: &Modulator, frac_step: f32) -> f32 {
    let steps: Vec<(f32, f32)> = m
        .steps
        .iter()
        .filter_map(|s| s.value.map(|v| (s.step as f32, v)))
        .collect();
    if steps.is_empty() {
        return m.entry_value.unwrap_or(0.0);
    }
    let smooth = m.interp.as_deref() == Some("smooth");
    let entry = m.entry_value.unwrap_or(steps[0].1);

    let (first_s, first_v) = steps[0];
    if frac_step <= first_s {
        if smooth && first_s > 0.0 {
            let t = (frac_step / first_s).clamp(0.0, 1.0);
            return entry + t * (first_v - entry);
        }
        return if frac_step < first_s { entry } else { first_v };
    }
    let last = *steps.last().unwrap();
    if frac_step >= last.0 {
        return last.1;
    }
    for w in steps.windows(2) {
        let (s0, v0) = w[0];
        let (s1, v1) = w[1];
        if frac_step >= s0 && frac_step < s1 {
            if !smooth {
                return v0;
            }
            let t = (frac_step - s0) / (s1 - s0);
            return v0 + t * (v1 - v0);
        }
    }
    last.1
}

/// Evaluate one modulator across one loop: control events (CONTROL_HOP grid) or note
/// events, plus the phase to carry into the next loop. Takes the whole doc for `loop_id`
/// plus the phrase arm's `sequence_pools` (and, later, transport feel — ADR 0028).
pub fn evaluate_modulator(
    m: &Modulator,
    phase: &ModulatorPhase,
    total_frames: usize,
    sample_rate: f64,
    doc: &LoopDoc,
) -> ModulatorOutput {
    let loop_id = doc.loop_id;
    let mut out = ModulatorOutput { next_phase: phase.clone(), ..Default::default() };
    if total_frames == 0 {
        return out;
    }

    // ── LFO (control) ──────────────────────────────────────────────────────
    if m.kind == "lfo" {
        let rate = m.rate_hz.unwrap_or(1.0);
        let wave = Wave::from_name(m.wave.as_deref().unwrap_or("sine"));
        let unipolar = m.polarity.as_deref() == Some("unipolar");
        let phase_inc = (TWO_PI as f64 * rate as f64 / sample_rate) as f32;

        let mut frame = 0usize;
        let mut last = 0.0;
        while frame < total_frames {
            let ph = (phase.lfo_phase + phase_inc * frame as f32).rem_euclid(TWO_PI);
            let mut v = osc(wave, ph);
            if unipolar {
                v = 0.5 + 0.5 * v;
            }
            out.control_events.push(ControlEvent { frame, value: v });
            last = v;
            frame += CONTROL_HOP;
        }
        out.next_phase.lfo_phase =
            (phase.lfo_phase + phase_inc * total_frames as f32).rem_euclid(TWO_PI);
        out.next_phase.last_output = last;
        return out;
    }

    if m.kind != "step_seq" {
        return out;
    }

    let step_dur_frac = step_duration_frac(m);
    let step_dur_frames = ((step_dur_frac * total_frames as f32).round() as usize).max(1);
    let is_free = m.sync == "free";
    let length = m.length.max(1) as i64;

    // ── step_seq control ───────────────────────────────────────────────────
    if m.output == "control" {
        let base_frac = if is_free {
            phase.step_index as f32 + phase.frames_into_step as f32 / step_dur_frames as f32
        } else {
            0.0
        };
        let mut frame = 0usize;
        let mut last = 0.0;
        while frame < total_frames {
            let abs = base_frac + frame as f32 / step_dur_frames as f32;
            let local = abs.rem_euclid(length as f32);
            let v = control_value_at(m, local);
            out.control_events.push(ControlEvent { frame, value: v });
            last = v;
            frame += CONTROL_HOP;
        }
        out.next_phase.last_output = last;
        if is_free {
            let consumed = phase.frames_into_step as usize + total_frames;
            out.next_phase.step_index = phase.step_index + (consumed / step_dur_frames) as i64;
            out.next_phase.frames_into_step = (consumed % step_dur_frames) as i64;
        }
        return out;
    }

    // ── step_seq note ──────────────────────────────────────────────────────
    if m.output == "note" {
        // Events carried over the seam (ADR 0028) fire first; anything still past this
        // loop's window (e.g. after a live loop-length edit) re-carries.
        out.next_phase.pending_notes.clear();
        for p in &phase.pending_notes {
            if (p.frame as usize) < total_frames {
                out.note_events.push(NoteEventOut { frame: p.frame.max(0) as usize, voicing: p.voicing.clone() });
            } else {
                out.next_phase.pending_notes.push(PendingNote { frame: p.frame - total_frames as i64, voicing: p.voicing.clone() });
            }
        }

        if let Some(ph) = &m.phrase {
            let jitter_amp =
                (doc.transport.timing_jitter_ms + m.humanize_ms).max(0.0) * sample_rate as f32 / 1000.0;
            expand_phrase(m, ph, doc, phase, is_free, step_dur_frames, total_frames, doc.transport.swing, jitter_amp, &mut out);
            // Advance the generic free-run clock arithmetically (the control-branch form —
            // the phrase arm has no boundary walk of its own).
            if is_free {
                let consumed = phase.frames_into_step as usize + total_frames;
                out.next_phase.step_index = phase.step_index + (consumed / step_dur_frames) as i64;
                out.next_phase.frames_into_step = (consumed % step_dur_frames) as i64;
            }
            return out;
        }

        // Free-run carries `frames_into_step` = frames since the last fired boundary (mod the step),
        // so the next one is due `step - frames_into_step` in — and 0 means due NOW: the first loop
        // (an empty phase) and a grid that divides the loop. Read as a whole step still to wait, it
        // started every free walk one step late and slipped one more at each such seam.
        let mut step_i: i64 = if is_free { phase.step_index } else { 0 };
        let mut frame: i64 = if is_free {
            (step_dur_frames as i64 - phase.frames_into_step).rem_euclid(step_dur_frames as i64)
        } else {
            0
        };

        // Feel (ADR 0028): swing bends on-grid onsets; jitter humanizes every strike.
        let swing = doc.transport.swing;
        let jitter_amp = (doc.transport.timing_jitter_ms + m.humanize_ms).max(0.0) * sample_rate as f32 / 1000.0;
        // Loop-sync: the seam is the next loop's step 0, however the frame grid rounds.
        let step_end = if is_free { i64::MAX } else { loop_sync_steps(m) };

        if let Some(g) = &m.generator {
            let rhythm = euclidean_rhythm(g.pulses, m.length, g.rotation);
            let pool: Vec<f32> = m.pitch_walk.as_ref().map(|p| p.pool.clone()).unwrap_or_default();
            let order = m.pitch_walk.as_ref().map(|p| p.order.as_str().to_string()).unwrap_or_else(|| "up".into());
            let mut pool_idx: i64 = 0;
            while (frame as usize) < total_frames && step_i < step_end {
                let local = step_i.rem_euclid(length) as usize;
                if local == 0 {
                    out.tiles.push(cycle_mark(frame, step_i, length, step_dur_frames, jitter_amp));
                }
                if local < rhythm.len() && rhythm[local] == 1 {
                    let mut nv = NoteVoice { id: None, offset: 0.0, cents_offset: 0.0, prob: 1.0, release: false, velocity: None, t: 0.0 };
                    if !pool.is_empty() {
                        let idx = match order.as_str() {
                            "random" => {
                                let h = hash_site(loop_id, &m.id, step_i.max(0) as u64, "pw");
                                (hash_to_unit(h) * pool.len() as f32) as usize % pool.len()
                            }
                            _ => pool_idx.rem_euclid(pool.len() as i64) as usize,
                        };
                        nv.offset = pool[idx];
                        match order.as_str() {
                            "down" => pool_idx -= 1,
                            "up" => pool_idx += 1,
                            _ => {}
                        }
                    }
                    let f = (frame
                        + swing_delay(step_i, swing, step_dur_frames)
                        + jitter_offset(jitter_amp, hash_site(loop_id, &m.id, step_i.max(0) as u64, "hum")))
                    .max(0);
                    if (f as usize) < total_frames {
                        out.note_events.push(NoteEventOut { frame: f as usize, voicing: vec![nv] });
                    } else {
                        out.next_phase.pending_notes.push(PendingNote { frame: f - total_frames as i64, voicing: vec![nv] });
                    }
                }
                frame += step_dur_frames as i64;
                step_i += 1;
            }
        } else {
            while (frame as usize) < total_frames && step_i < step_end {
                let local = step_i.rem_euclid(length);
                if local == 0 {
                    out.tiles.push(cycle_mark(frame, step_i, length, step_dur_frames, jitter_amp));
                }
                if let Some(s) = m.steps.iter().find(|s| s.step as i64 == local && !s.voicing.is_empty()) {
                    // v0.7: voicing is an id-keyed object → iterate by sorted id. Voices sharing a
                    // sub-step onset `t` (ADR 0028) group into one event at `step + t`, groups
                    // ascending by t — an all-t=0 voicing emits the same single event as before.
                    let mut groups: Vec<(i64, Vec<NoteVoice>)> = Vec::new();
                    for nv in s.voicing.values() {
                        let bucket = (nv.t * 1e6).round() as i64;
                        let mut nv = nv.clone();
                        nv.t = 0.0; // the event frame encodes the onset; emitted copies stay canonical
                        match groups.binary_search_by_key(&bucket, |(b, _)| *b) {
                            Ok(gi) => groups[gi].1.push(nv),
                            Err(gi) => groups.insert(gi, (bucket, vec![nv])),
                        }
                    }
                    for (bucket, voicing) in groups {
                        let sub = frame
                            + ((bucket as f32 / 1e6) * step_dur_frames as f32).round() as i64
                            // Only the on-grid group swings — fractional `t` is authored
                            // micro-timing and passes through untouched.
                            + if bucket == 0 { swing_delay(step_i, swing, step_dur_frames) } else { 0 }
                            + jitter_offset(jitter_amp, hash_site(loop_id, &m.id, step_i.max(0) as u64, &format!("hum{bucket}")));
                        let sub = sub.max(0);
                        if (sub as usize) < total_frames {
                            out.note_events.push(NoteEventOut { frame: sub as usize, voicing });
                        } else {
                            // The boundary sits inside this loop but the onset spills past the
                            // seam (free-run non-divisible grid, or feel offsets at the loop
                            // edge): carry it so the hit lands where the timeline puts it.
                            out.next_phase.pending_notes.push(PendingNote { frame: sub - total_frames as i64, voicing });
                        }
                    }
                }
                frame += step_dur_frames as i64;
                step_i += 1;
            }
        }

        // Carry phase for free-running note sources.
        if is_free {
            let last_fire = frame - step_dur_frames as i64;
            let consumed = total_frames as i64 - last_fire;
            out.next_phase.frames_into_step = consumed.rem_euclid(step_dur_frames as i64);
            out.next_phase.step_index = step_i;
        }
        return out;
    }

    out
}

/// A steps / generator pattern's cycle start (ADR 0048) at grid `frame`, absolute step `step_i`.
fn cycle_mark(frame: i64, step_i: i64, length: i64, step_dur_frames: usize, jitter_amp: f32) -> TileMark {
    TileMark {
        frame,
        ordinal: step_i.div_euclid(length),
        entry: None,
        len_frames: (length * step_dur_frames as i64) as u64,
        margin: jitter_amp.ceil().max(0.0) as u32,
        realized: false,
    }
}

/// Pick the tile's phrase: `realized` (the Orrery's materialized refit copy) ▸ the `active`
/// pin ▸ per-tile `select` over the tag-filtered pool, seeded by the tile ordinal.
/// Also returns the pool entry's id (for a `realized` copy, the `active` entry it refits).
fn pick_phrase<'a>(m_id: &str, ph: &'a PhraseRef, doc: &'a LoopDoc, ordinal: i64) -> Option<(Option<&'a str>, &'a PhraseDef)> {
    if let Some(r) = &ph.realized {
        return Some((ph.active.as_deref(), r.as_ref()));
    }
    if let Some(a) = &ph.active {
        return crate::phrase::resolve(a, doc).map(|d| (Some(a.as_str()), d));
    }
    let filtered: Vec<(Option<&str>, &PhraseDef)> = ph
        .pool
        .iter()
        .filter_map(|pid| crate::phrase::resolve(pid, doc).map(|d| (Some(pid.as_str()), d)))
        .filter(|(_, d)| ph.tags.iter().all(|t| d.tags.contains(t)))
        .collect();
    if filtered.is_empty() {
        return None; // statically rejected by validate(); belt and braces
    }
    let idx = match ph.select.as_str() {
        "cycle" => ordinal.rem_euclid(filtered.len() as i64) as usize,
        _ => {
            let u = hash_to_unit(hash_site(PHRASE_SEED, m_id, ordinal.max(0) as u64, "sel"));
            ((u * filtered.len() as f32) as usize).min(filtered.len() - 1)
        }
    };
    Some(filtered[idx])
}

/// Expand the phrase arm (ADR 0028): tile the selected entries across this loop's window,
/// emitting sub-step onsets and gates. Every note owns the deterministic voice id
/// `p{k}` (`k` = its legato-chain head's index), so gated notes (`dur`) release and
/// re-strike per tile, dur-less notes hold/glide across tiles, and an event that falls in
/// the next loop's window is simply recomputed there — nothing to carry but the tile clock.
#[allow(clippy::too_many_arguments)]
fn expand_phrase(
    m: &Modulator,
    ph: &PhraseRef,
    doc: &LoopDoc,
    phase: &ModulatorPhase,
    is_free: bool,
    step_dur_frames: usize,
    total_frames: usize,
    swing: f32,
    jitter_amp: f32,
    out: &mut ModulatorOutput,
) {
    let step_dur = step_dur_frames as i64;
    let total = total_frames as i64;

    // The tile clock: `anchor` = absolute host-step where the current tile starts (free-run
    // carries it; loop-sync re-anchors at 0); `ordinal` counts tiles started and seeds
    // selection + prob in BOTH modes. The anchor's frame comes from the generic free-run clock
    // (the control branch's, advanced below in `evaluate_modulator`): step `step_index` began
    // `frames_into_step` frames before this loop's frame 0 — so an empty phase starts step 0 on
    // frame 0, as loop-sync does (it used to read that boundary one step later).
    let (mut anchor, mut ordinal) = if is_free {
        (phase.phrase_anchor_step, phase.phrase_ordinal)
    } else {
        (0, phase.phrase_ordinal)
    };
    let mut tile_start: i64 = if is_free {
        -phase.frames_into_step + (anchor - phase.step_index) * step_dur
    } else {
        0
    };

    // Static-harmony fit (ADR 0028, amended 2026-07-09): when no controller has materialized
    // `realized`, a fit:"chord" entry still lands in-key — snapped against the doc's authored
    // `harmony.mode` scale (the library's major-triad contours would otherwise play literally,
    // e.g. a major third over a dorian doc). `root`/`literal` stay verbatim: there is no chord
    // to follow, and literal is the author's escape hatch. Pure function of the doc — CE-R7.3
    // determinism unaffected.
    let scale_degrees = crate::theory::scale::Mode::from_name(&doc.harmony.mode).degrees();

    // Jitter reach (frames): the next tile's strikes may land up to `margin` early, so every
    // event of THIS tile must land before `tile_end - margin` (D4/D7: a gate ends inside its
    // own tile, jitter included). 0 without jitter ⇒ the plain `tile_end - 1` bound.
    let margin = if jitter_amp > 0.0 { jitter_amp.ceil() as i64 } else { 0 };
    // Loop-sync re-anchors at step 0 on the seam: no tile starts on (or, through the rounded frame
    // grid, just before) it — that tile is the next loop's first.
    let step_end = if is_free { i64::MAX } else { loop_sync_steps(m) };

    while tile_start < total && anchor < step_end {
        let Some((entry, picked)) = pick_phrase(&m.id, ph, doc, ordinal) else { break };
        let fitted;
        let def: &PhraseDef = if ph.realized.is_none() && matches!(picked.fit.as_str(), "chord" | "arp") {
            fitted = crate::theory::phrase_fit::refit_static(picked, scale_degrees);
            &fitted
        } else {
            picked
        };
        let len = def.length.max(1) as i64;
        let tile_end = tile_start + len * step_dur;
        out.tiles.push(TileMark {
            frame: tile_start,
            ordinal,
            entry: entry.map(str::to_string),
            len_frames: (len * step_dur) as u64,
            margin: margin as u32,
            realized: ph.realized.is_some(),
        });

        // Notes ascending by t; a legato run shares its head's voice id.
        let mut order: Vec<usize> = (0..def.notes.len()).collect();
        order.sort_by(|&a, &b| def.notes[a].t.partial_cmp(&def.notes[b].t).unwrap_or(std::cmp::Ordering::Equal));
        let mut head_of = vec![0usize; def.notes.len()];
        for (pos, &k) in order.iter().enumerate() {
            head_of[k] = if def.notes[k].legato && pos > 0 { head_of[order[pos - 1]] } else { k };
        }

        for &k in &order {
            let n = &def.notes[k];
            let roll = hash_to_unit(hash_site(PHRASE_SEED, &m.id, ordinal.max(0) as u64, &format!("p{k}")));
            if roll > n.prob {
                continue; // pair-skip: neither the strike nor its note-off fires
            }
            let id = format!("p{}", head_of[k]);
            // Feel: only whole-step onsets swing (fractional t = authored micro-timing);
            // jitter is seeded by (ordinal, note) — loop_id-free like every phrase site,
            // so a seam-straddling tile recomputes the identical placement next loop.
            let on_grid_step = (n.t.fract() == 0.0).then(|| anchor + n.t as i64);
            let f = tile_start
                + (n.t * step_dur as f32).round() as i64
                + on_grid_step.map_or(0, |s| swing_delay(s, swing, step_dur_frames))
                + jitter_offset(jitter_amp, hash_site(PHRASE_SEED, &m.id, ordinal.max(0) as u64, &format!("hum{k}")));
            // Routing an event around the loop window differs by sync (ADR 0028):
            //  free  — f < 0 was emitted by the previous loop's re-walk (a seam-aligned
            //          tile's early head is clamped below instead); f ≥ total will be
            //          recomputed by the next one (ordinal-stable seeds ⇒ identical result).
            //  loop  — tiles re-anchor per loop so nothing recomputes: clamp an early-
            //          jittered edge note to 0 and CARRY overflow via pending_notes,
            //          exactly like the walk arms.
            let mut route = |f: i64, voicing: Vec<NoteVoice>| {
                if (0..total).contains(&f) {
                    out.note_events.push(NoteEventOut { frame: f as usize, voicing });
                } else if !is_free {
                    if f < 0 {
                        out.note_events.push(NoteEventOut { frame: 0, voicing });
                    } else {
                        out.next_phase.pending_notes.push(PendingNote { frame: f - total, voicing });
                    }
                }
            };
            // The latest frame this tile may use — the next tile's earliest possible strike is
            // `tile_end - margin`. A late-jittered onset is pulled in to leave room for its off.
            let latest = tile_end - margin - 1;
            let f = f.min(latest - 1);
            // An early-jittered onset before frame 0 is clamped there when THIS loop owns the
            // tile: always under loop-sync (see `route`), and in free-run when the tile starts
            // in this window (a seam-aligned tile — the previous loop never walked it, so its
            // early head was nobody's). A free-run tile that started last loop (tile_start < 0)
            // already emitted its f < 0 events there. The gate is measured from where the
            // strike actually LANDS, so `dur` is preserved (D7).
            let onset = if f < 0 && (!is_free || tile_start >= 0) { 0 } else { f };
            route(
                onset,
                vec![NoteVoice {
                    id: Some(id.clone()),
                    offset: n.offset,
                    cents_offset: n.cents_offset,
                    prob: 1.0, // rolled above — the engine must not re-roll
                    release: false,
                    velocity: n.velocity,
                    t: 0.0,
                }],
            );
            if let Some(d) = n.dur {
                // The off never passes `latest`: validation keeps `t + dur ≤ length`, and this
                // clamp absorbs rounding / jitter so a release can never coincide with (or
                // follow) the next tile's — possibly early-jittered — strike of the same id,
                // nor fall past a seam-ending tile in free-run, where it would never be
                // recomputed.
                let off = (onset + ((d * step_dur as f32).round() as i64).max(1)).min(latest.max(onset + 1));
                route(
                    off,
                    vec![NoteVoice { id: Some(id), offset: 0.0, cents_offset: 0.0, prob: 1.0, release: true, velocity: None, t: 0.0 }],
                );
            }
        }

        if tile_end >= total {
            if tile_end == total {
                // Clean tile boundary at the seam: next loop starts the NEXT tile.
                anchor += len;
                ordinal += 1;
            } else if !is_free {
                // Loop-sync: a straddling tile was STARTED (its overflow is carried via
                // pending_notes, and next loop re-anchors at step 0), so it counts — the
                // ordinal accumulates per started tile (D5) and the next loop's first tile
                // must not replay this one's selection / prob rolls.
                ordinal += 1;
            }
            // Free-run: a straddling tile keeps its anchor/ordinal and recomputes its tail.
            break;
        }
        anchor += len;
        ordinal += 1;
        tile_start = tile_end;
    }

    out.next_phase.phrase_anchor_step = if is_free { anchor } else { 0 };
    out.next_phase.phrase_ordinal = ordinal;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_doc::{LoopDoc, Step};

    use crate::test_support::fixture;

    /// A hand-built id'd voice (tests bypass `backfill_ids`, so `id` is set explicitly).
    fn voice(id: &str, offset: f32, t: f32) -> (String, NoteVoice) {
        (
            id.to_string(),
            NoteVoice { id: Some(id.to_string()), offset, cents_offset: 0.0, prob: 1.0, release: false, velocity: None, t },
        )
    }

    /// The canonical fixture with feel zeroed (it authors timing_jitter_ms 8, live since
    /// ADR 0028) — for frame-exact grid asserts.
    fn still() -> LoopDoc {
        let mut d = fixture();
        d.transport.timing_jitter_ms = 0.0;
        d.transport.swing = 0.0;
        d
    }

    #[test]
    fn lfo_control_in_range_and_advances_phase() {
        let d = fixture();
        let lfo = d.find_modulator("lfo_breath").unwrap();
        let p0 = ModulatorPhase::default();
        let out = evaluate_modulator(lfo, &p0, 48_000, 48_000.0, &d);
        assert!(!out.control_events.is_empty());
        for e in &out.control_events {
            assert!((-1.0..=1.0).contains(&e.value), "bipolar LFO out of range: {}", e.value);
        }
        assert_ne!(out.next_phase.lfo_phase, p0.lfo_phase, "phase must advance");
    }

    #[test]
    fn control_step_seq_interpolates_between_points() {
        let d = fixture();
        let swell = d.find_modulator("seq_swell").unwrap(); // steps 0=.44, 8=.54, 15=.47, smooth
        let out = evaluate_modulator(swell, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        let first = out.control_events.first().unwrap().value;
        assert!((first - 0.44).abs() < 0.02, "entry near 0.44, got {first}");
        let peak = out.control_events.iter().map(|e| e.value).fold(f32::MIN, f32::max);
        assert!(peak > 0.50 && peak <= 0.541, "smooth interp should approach 0.54 peak, got {peak}");
    }

    #[test]
    fn euclid_note_count_matches_pulses_per_loop() {
        let d = fixture();
        let arp = d.find_modulator("arp_shimmer").unwrap(); // euclid 17 over 44, loop-synced
        let out = evaluate_modulator(arp, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        assert_eq!(out.note_events.len(), 17, "one loop fires `pulses` notes");
        // pitch_walk "up" cycles the pool [12,16,19,24]
        assert_eq!(out.note_events[0].voicing[0].offset, 12.0);
        assert_eq!(out.note_events[1].voicing[0].offset, 16.0);
    }

    #[test]
    fn explicit_note_steps_fire_at_step_boundaries() {
        // The fixture authors transport.timing_jitter_ms = 8, live since ADR 0028: onsets
        // sit within a deterministic ±8 ms band of their grid boundaries.
        let d = fixture();
        let chords = d.find_modulator("chords").unwrap(); // steps at 0,8,12 over 16
        let out = evaluate_modulator(chords, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        assert_eq!(out.note_events.len(), 3);
        let band = (8.0f32 * 48.0).round() as i64; // ±8 ms @ 48 kHz
        assert!(out.note_events[0].frame as i64 <= band, "step 0 within the jitter band (clamped ≥ 0)");
        // step 8 of 16 → halfway through the loop
        assert!((out.note_events[1].frame as i64 - 24_000).abs() <= band);
        assert_eq!(out.note_events[0].voicing.len(), 4);
        // …and the jitter is seeded: a second evaluation reproduces the frames exactly.
        let again = evaluate_modulator(chords, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        assert_eq!(
            out.note_events.iter().map(|e| e.frame).collect::<Vec<_>>(),
            again.note_events.iter().map(|e| e.frame).collect::<Vec<_>>(),
        );
        // With feel zeroed the grid is exact (the pre-0028 behavior).
        let d0 = still();
        let out0 = evaluate_modulator(d0.find_modulator("chords").unwrap(), &ModulatorPhase::default(), 48_000, 48_000.0, &d0);
        assert_eq!(out0.note_events.iter().map(|e| e.frame).collect::<Vec<_>>(), [0, 24_000, 36_000]);
    }

    // ── ADR 0028: sub-step onsets (`NoteVoice.t`) ────────────────────────────

    #[test]
    fn substep_voice_t_fires_between_step_boundaries() {
        // A voice with `t: 0.5` strikes on the "and" of its step — here step 8 of a
        // 16-step grid over a 48000-frame loop (step = 3000 frames → frame 25500).
        let d = still();
        let mut m = d.find_modulator("chords").unwrap().clone();
        m.steps = vec![Step { step: 8, value: None, voicing: [voice("v1", 0.0, 0.5)].into_iter().collect() }];
        let out = evaluate_modulator(&m, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        assert_eq!(out.note_events.len(), 1);
        assert_eq!(out.note_events[0].frame, 8 * 3000 + 1500);
        assert_eq!(out.note_events[0].voicing[0].t, 0.0, "emitted copies are canonical — the frame encodes the onset");
    }

    #[test]
    fn substep_voices_group_and_preserve_sorted_id_order() {
        // Voices sharing an onset group into one event (sorted-id order preserved);
        // distinct onsets split into separate events, ascending by `t`.
        let d = still();
        let mut m = d.find_modulator("chords").unwrap().clone();
        m.steps = vec![Step {
            step: 0,
            value: None,
            voicing: [voice("a", 0.0, 0.0), voice("b", 7.0, 0.5), voice("c", 4.0, 0.0)].into_iter().collect(),
        }];
        let out = evaluate_modulator(&m, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        assert_eq!(out.note_events.len(), 2, "two onset groups: the beat and the and");
        assert_eq!(out.note_events[0].frame, 0);
        let ids: Vec<&str> = out.note_events[0].voicing.iter().map(|v| v.id.as_deref().unwrap()).collect();
        assert_eq!(ids, ["a", "c"], "sorted-id order within the on-beat group");
        assert_eq!(out.note_events[1].frame, 1500);
        assert_eq!(out.note_events[1].voicing[0].id.as_deref(), Some("b"));
    }

    #[test]
    fn free_run_substep_carries_phase_across_seam() {
        // A free-running pattern on a non-divisible 1/7 grid with an off-beat voice:
        // hits stay exactly one pattern-period apart across the loop seam, i.e. the
        // sub-step onset rides the carried `frames_into_step` like the boundary does.
        let d = still();
        let mut m = d.find_modulator("chords").unwrap().clone();
        m.sync = "free".into();
        m.length = 3;
        m.step_duration = Some("1/7".into());
        m.steps = vec![Step { step: 1, value: None, voicing: [voice("v", 0.0, 0.75)].into_iter().collect() }];
        let total = 48_000usize;
        let l1 = evaluate_modulator(&m, &ModulatorPhase::default(), total, 48_000.0, &d);
        let l2 = evaluate_modulator(&m, &l1.next_phase, total, 48_000.0, &d);
        let l3 = evaluate_modulator(&m, &l2.next_phase, total, 48_000.0, &d);
        let abs: Vec<i64> = l1
            .note_events
            .iter()
            .map(|e| e.frame as i64)
            .chain(l2.note_events.iter().map(|e| e.frame as i64 + total as i64))
            .chain(l3.note_events.iter().map(|e| e.frame as i64 + 2 * total as i64))
            .collect();
        assert!(abs.len() >= 5, "hits in all three loops: {abs:?}");
        // In loop 1 step 7's boundary sits just inside the loop (frame 47999) while its onset
        // spills past the seam — that hit must be CARRIED (pending), not clamped early or dropped.
        assert_eq!(l1.next_phase.pending_notes.len(), 1, "the spilled onset is carried over the seam");
        let step_dur = (total as f32 / 7.0).round() as i64;
        let sub = (0.75 * step_dur as f32).round() as i64;
        assert_eq!(abs[0], step_dur + sub, "first hit = step-1 boundary + sub-step offset");
        for w in abs.windows(2) {
            assert_eq!(w[1] - w[0], 3 * step_dur, "one pattern-period apart across every seam: {abs:?}");
        }
    }

    /// Absolute frames (`loop · total + frame`) of a modulator's strikes over `loops` loops, the phase
    /// carried from loop to loop as the engine does.
    fn strikes_over(m: &Modulator, d: &LoopDoc, total: usize, loops: usize) -> Vec<i64> {
        let mut phase = ModulatorPhase::default();
        let mut abs = Vec::new();
        for l in 0..loops {
            let out = evaluate_modulator(m, &phase, total, 48_000.0, d);
            abs.extend(out.note_events.iter().filter(|e| e.voicing.iter().any(|v| !v.release)).map(|e| (l * total + e.frame) as i64));
            phase = out.next_phase;
        }
        abs
    }

    #[test]
    fn a_free_walk_arm_starts_on_frame_zero_and_keeps_its_place_across_seams() {
        // The benchmark's `ninety-lead` candidate (2026-10-24): a free 64-step `steps` arm at "1/16"
        // of a 16-beat loop — 512000 frames, a step of exactly 32000. Step k belongs at absolute
        // frame k · 32000 whatever loop it falls in; it started one step late and slipped one more
        // step at every seam (a grid that divides the loop carried "0 frames into the step" as a
        // whole step still to wait).
        let d = LoopDoc::from_json_str(
            r#"{ "schema_version": "0.9", "transport": { "bpm": 90, "master_steps": 16 },
                 "harmony": { "root": "D", "mode": "lydian" },
                 "modulators": { "line": { "type": "step_seq", "output": "note", "sync": "free", "length": 64, "step_duration": "1/16",
                     "steps": [ { "step": 0, "voicing": { "lo": { "offset": 0 } } }, { "step": 18, "voicing": { "t1": { "offset": 6 } } },
                                { "step": 20, "voicing": { "hi": { "offset": 12 } } }, { "step": 48, "voicing": { "lo": { "offset": 0 } } } ] } } }"#,
        )
        .unwrap();
        let total = d.total_frames(48_000.0);
        assert_eq!(total, 512_000);
        let steps: Vec<i64> = strikes_over(d.find_modulator("line").unwrap(), &d, total, 5).iter().map(|f| f / 32_000).collect();
        assert_eq!(steps, [0, 18, 20, 48, 64], "steps on their own grid, the pattern again after 4 loops");
    }

    #[test]
    fn a_free_phrase_starts_on_frame_zero() {
        // The free phrase arm read the same carried clock one step late: tile 0 at frame 12000.
        let d = LoopDoc::from_json_str(
            r#"{ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 8 },
                 "harmony": { "root": "C", "mode": "ionian" },
                 "modulators": { "f": { "type": "step_seq", "output": "note", "sync": "free", "length": 3, "step_duration": "1/8",
                     "phrase": { "pool": ["a"] } } },
                 "sequence_pools": { "a": { "fit": "literal", "length": 3, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }] } } }"#,
        )
        .unwrap();
        let abs = strikes_over(d.find_modulator("f").unwrap(), &d, 96_000, 3);
        assert_eq!(abs, (0..8).map(|k| k * 36_000).collect::<Vec<i64>>(), "one 3-step tile every 36000 frames from frame 0");
    }

    // ── ADR 0028: feel — swing + humanize (the CE-R5.2 debt, finally applied) ─

    #[test]
    fn swing_delays_odd_steps_by_a_third_of_step() {
        // swing 1.0 on a 16-step grid (step = 3000): odd steps land step/3 = 1000 late
        // (the 2/3 triplet point); even steps stay on the grid.
        let mut d = still();
        d.transport.swing = 1.0;
        let mut m = d.find_modulator("chords").unwrap().clone();
        m.steps = (0..4)
            .map(|s| Step { step: s, value: None, voicing: [voice("v", 0.0, 0.0)].into_iter().collect() })
            .collect();
        let out = evaluate_modulator(&m, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        assert_eq!(out.note_events.iter().map(|e| e.frame).collect::<Vec<_>>(), [0, 4_000, 6_000, 10_000]);
    }

    #[test]
    fn swing_leaves_fractional_t_untouched() {
        // On a swung odd step, the on-grid voice moves but an authored "and" (t 0.5)
        // keeps its exact micro-timing.
        let mut d = still();
        d.transport.swing = 1.0;
        let mut m = d.find_modulator("chords").unwrap().clone();
        m.steps = vec![Step {
            step: 1,
            value: None,
            voicing: [voice("a", 0.0, 0.0), voice("b", 7.0, 0.5)].into_iter().collect(),
        }];
        let out = evaluate_modulator(&m, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        let frames: Vec<usize> = out.note_events.iter().map(|e| e.frame).collect();
        assert_eq!(frames, [4_000, 4_500], "swung on-grid voice vs untouched sub-step voice");
    }

    #[test]
    fn humanize_jitter_is_deterministic_and_bounded() {
        // transport 10 ms + modulator humanize 5 ms → each onset within ±15 ms of its
        // boundary, reproduced exactly on re-evaluation (hash-seeded, CE-R7.3).
        let mut d = still();
        d.transport.timing_jitter_ms = 10.0;
        let mut m = d.find_modulator("chords").unwrap().clone();
        m.humanize_ms = 5.0;
        m.steps = (0..8)
            .map(|s| Step { step: 2 * s, value: None, voicing: [voice("v", 0.0, 0.0)].into_iter().collect() })
            .collect();
        let a = evaluate_modulator(&m, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        let b = evaluate_modulator(&m, &ModulatorPhase::default(), 48_000, 48_000.0, &d);
        let band = (15.0f32 * 48.0).round() as i64;
        let mut moved = 0;
        for e in &a.note_events {
            let nominal = (e.frame as i64 + band).div_euclid(6_000) * 6_000; // nearest even-step boundary
            assert!((e.frame as i64 - nominal).abs() <= band, "frame {} vs nominal {nominal}", e.frame);
            if e.frame as i64 != nominal {
                moved += 1;
            }
        }
        assert!(moved > 0, "jitter actually displaces onsets");
        assert_eq!(
            a.note_events.iter().map(|e| e.frame).collect::<Vec<_>>(),
            b.note_events.iter().map(|e| e.frame).collect::<Vec<_>>(),
            "seeded jitter is bit-deterministic"
        );
    }

    #[test]
    fn jitter_moves_paired_release_with_its_articulate() {
        // A phrase gate under jitter: the note-off rides its (jittered) onset, so the
        // sounding duration stays exactly dur · step_dur.
        let mut d = phrase_fixture();
        d.transport.timing_jitter_ms = 6.0;
        let m = d.find_modulator("ph_midi").unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let evs = ev(&out);
        let strike = evs.iter().find(|(_, o, rel, _)| *o == 12.0 && !rel).expect("p2 strike");
        let release = evs.iter().find(|(_, _, rel, id)| *rel && id == "p2").expect("p2 release");
        assert_eq!(release.0 - strike.0, 12_000, "dur 1.0 × step 12000, jitter riding along");
        assert_ne!(strike.0, 24_000, "the onset itself did move off the grid");
    }

    // ── ADR 0028: the phrase arm (sequence pools) ────────────────────────────

    fn phrase_fixture() -> LoopDoc {
        let p = crate::test_support::fixtures_dir().join("loop_phrase_pools.json");
        LoopDoc::from_json_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// (frame, offset, release, id) tuples for terse asserts.
    fn ev(out: &ModulatorOutput) -> Vec<(usize, f32, bool, String)> {
        out.note_events
            .iter()
            .map(|e| (e.frame, e.voicing[0].offset, e.voicing[0].release, e.voicing[0].id.clone().unwrap()))
            .collect()
    }

    #[test]
    fn phrase_expands_notes_at_fractional_frames_with_paired_releases() {
        // ph_midi: loop-sync, active "cell_a" (tile 4 host steps), host grid 1/8 of a
        // 96000-frame loop → step = 12000, two tiles at 0 and 48000. The t:1.5 note is
        // the "and" of step 1 → frame 18000; its dur 0.5 releases at 24000.
        let d = phrase_fixture();
        let m = d.find_modulator("ph_midi").unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let evs = ev(&out);
        for (f, off) in [(0usize, 0.0f32), (18_000, 7.0), (24_000, 12.0), (48_000, 0.0), (66_000, 7.0), (72_000, 12.0)] {
            assert!(
                evs.iter().any(|(ef, eo, rel, _)| *ef == f && *eo == off && !rel),
                "articulate {off} at {f} missing: {evs:?}"
            );
        }
        for (f, id) in [(6_000usize, "p0"), (24_000, "p1"), (36_000, "p2"), (54_000, "p0")] {
            assert!(
                evs.iter().any(|(ef, _, rel, eid)| *ef == f && *rel && eid == id),
                "release {id} at {f} missing: {evs:?}"
            );
        }
        // emitted phrase voices carry prob 1.0 — rolled here, never re-rolled by the engine
        assert!(out.note_events.iter().all(|e| e.voicing[0].prob == 1.0));
    }

    #[test]
    fn phrase_failed_prob_skips_strike_and_release_as_a_pair() {
        // cell_a's t:3.5 note (offset 16, dur 0.4, prob 0.5): per tile, either both the
        // strike (tile+42000) and its note-off (+4800) fire, or neither does.
        let d = phrase_fixture();
        let m = d.find_modulator("ph_midi").unwrap();
        let mut phase = ModulatorPhase::default();
        let (mut fired, mut skipped) = (0, 0);
        for _ in 0..4 {
            let out = evaluate_modulator(m, &phase, 96_000, 48_000.0, &d);
            let evs = ev(&out);
            for tile in [0usize, 48_000] {
                let strike = evs.iter().any(|(f, _, rel, _)| *f == tile + 42_000 && !rel);
                let off = evs.iter().any(|(f, _, rel, _)| *f == tile + 46_800 && *rel);
                assert_eq!(strike, off, "prob must skip the strike AND its note-off together");
                if strike { fired += 1 } else { skipped += 1 }
            }
            phase = out.next_phase;
        }
        assert!(fired > 0 && skipped > 0, "prob 0.5 over 8 tiles varies (fired {fired}, skipped {skipped})");
    }

    #[test]
    fn phrase_cycle_reselects_per_tile_and_recomputes_across_the_seam() {
        // "ph": free-run, select "cycle" over [cell_a, cell_b], tiles 4 steps = 48000 frames, on a
        // free clock one step behind the loop (tile 0 at frame 12000). Tile 1 (cell_b) straddles
        // the seam: its t:2.5 note strikes at 90000 with dur 0.5 → the note-off lands exactly at
        // the next loop's frame 0 and must be RECOMPUTED there (deterministic p-ids, no carry).
        let d = phrase_fixture();
        let m = d.find_modulator("ph").unwrap();
        let l1 = evaluate_modulator(m, &step_behind(), 96_000, 48_000.0, &d);
        let e1 = ev(&l1);
        assert!(e1.iter().any(|(f, o, rel, _)| (*f, *o, *rel) == (12_000, 0.0, false)), "tile 0 = cell_a: {e1:?}");
        assert!(e1.iter().any(|(f, o, rel, _)| (*f, *o, *rel) == (66_000, 4.0, false)), "tile 1 = cell_b: {e1:?}");
        assert!(e1.iter().any(|(f, o, rel, _)| (*f, *o, *rel) == (90_000, 11.0, false)), "cell_b strike near the seam");
        assert!(!e1.iter().any(|(f, _, _, _)| *f >= 96_000), "nothing past the loop");
        assert_eq!(l1.next_phase.phrase_ordinal, 1, "the straddling tile keeps its ordinal");

        let l2 = evaluate_modulator(m, &l1.next_phase, 96_000, 48_000.0, &d);
        let e2 = ev(&l2);
        assert!(
            e2.iter().any(|(f, _, rel, id)| (*f, *rel, id.as_str()) == (0, true, "p1")),
            "the spilled note-off is recomputed at the next loop's frame 0: {e2:?}"
        );
        assert!(e2.iter().any(|(f, o, rel, _)| (*f, *o, *rel) == (12_000, 0.0, false)), "cycle continues: tile 2 = cell_a");
        assert!(
            !e2.iter().any(|(f, o, rel, _)| *f == 12_000 && *o == 4.0 && !rel),
            "tile 2 is NOT cell_b — ordinal parity carried across the seam"
        );
    }

    /// A free-run clock one step behind the loop (step 0 begins at the first step boundary, frame
    /// `step` in), so that tiles which would land on the seam from an empty phase straddle it.
    fn step_behind() -> ModulatorPhase {
        ModulatorPhase { step_index: -1, ..Default::default() }
    }

    // ── ADR 0048: where each pass starts ─────────────────────────────────────

    /// (frame, ordinal, entry, len_frames, realized) per mark.
    fn marks(out: &ModulatorOutput) -> Vec<(i64, i64, Option<String>, u64, bool)> {
        out.tiles.iter().map(|t| (t.frame, t.ordinal, t.entry.clone(), t.len_frames, t.realized)).collect()
    }

    #[test]
    fn phrase_tiles_are_marked_with_their_entries() {
        // ph_midi: loop-sync, active cell_a (4 steps × 12000) ⇒ tiles at 0 and 48000.
        let d = phrase_fixture();
        let m = d.find_modulator("ph_midi").unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let a = Some("cell_a".to_string());
        assert_eq!(marks(&out), vec![(0, 0, a.clone(), 48_000, false), (48_000, 1, a, 48_000, false)]);
    }

    #[test]
    fn a_free_run_tile_straddling_the_seam_is_marked_again_as_a_continuation() {
        // "ph": free-run cycle a/b from frame 12000 (a clock a step behind the loop); tile 1
        // (cell_b) straddles the seam and is reported again next loop at its (negative) start
        // with the same ordinal.
        let d = phrase_fixture();
        let m = d.find_modulator("ph").unwrap();
        let l1 = evaluate_modulator(m, &step_behind(), 96_000, 48_000.0, &d);
        let (a, b) = (Some("cell_a".to_string()), Some("cell_b".to_string()));
        assert_eq!(marks(&l1), vec![(12_000, 0, a.clone(), 48_000, false), (60_000, 1, b.clone(), 48_000, false)]);
        let l2 = evaluate_modulator(m, &l1.next_phase, 96_000, 48_000.0, &d);
        assert_eq!(marks(&l2), vec![(-36_000, 1, b.clone(), 48_000, false), (12_000, 2, a, 48_000, false), (60_000, 3, b, 48_000, false)]);
    }

    #[test]
    fn loop_sync_tiles_restart_marked_each_loop() {
        let d = loop_sync_phrase_doc(
            "ls",
            "cycle",
            r#""a": { "fit": "literal", "length": 5, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }] },
               "b": { "fit": "literal", "length": 5, "notes": [{ "t": 0, "offset": 4, "dur": 0.5 }] }"#,
        );
        let m = d.find_modulator("ls").unwrap();
        let l1 = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        assert_eq!(marks(&l1), vec![(0, 0, Some("a".into()), 60_000, false), (60_000, 1, Some("b".into()), 60_000, false)]);
        let l2 = evaluate_modulator(m, &l1.next_phase, 96_000, 48_000.0, &d);
        assert_eq!(marks(&l2)[0], (0, 2, Some("a".into()), 60_000, false));
    }

    #[test]
    fn a_realized_tile_names_the_entry_it_refits() {
        let mut d = phrase_fixture();
        let def = crate::phrase::resolve("cell_a", &d).unwrap().clone();
        let m = d.modulators.get_mut("ph_midi").unwrap();
        m.phrase.as_mut().unwrap().realized = Some(Box::new(def));
        let m = d.find_modulator("ph_midi").unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        assert!(out.tiles.iter().all(|t| t.realized && t.entry.as_deref() == Some("cell_a")), "{:?}", out.tiles);
    }

    #[test]
    fn step_patterns_are_marked_at_each_cycle_start() {
        // Loop-sync, 16 steps over the loop ⇒ one mark at 0 per loop.
        let d = LoopDoc::from_json_str(
            r#"{ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 8 },
                 "harmony": { "root": "C", "mode": "ionian" },
                 "modulators": { "s": { "type": "step_seq", "output": "note", "sync": "loop", "length": 16,
                     "steps": [{ "step": 0, "voicing": { "v": { "offset": 0 } } }] },
                   "f": { "type": "step_seq", "output": "note", "sync": "free", "length": 3, "step_duration": "1/8",
                     "steps": [{ "step": 0, "voicing": { "v": { "offset": 0 } } }] } } }"#,
        )
        .unwrap();
        let s = d.find_modulator("s").unwrap();
        let out = evaluate_modulator(s, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        assert_eq!(marks(&out), vec![(0, 0, None, 96_000, false)]);
        // Free-run, 3 steps × 12000 from frame 0: cycles start at steps 0, 3, 6 … ⇒ every 36000
        // frames, numbered by step / 3.
        let f = d.find_modulator("f").unwrap();
        let l1 = evaluate_modulator(f, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let starts: Vec<(i64, i64)> = l1.tiles.iter().map(|t| (t.frame, t.ordinal)).collect();
        assert_eq!(starts, vec![(0, 0), (36_000, 1), (72_000, 2)]);
        let l2 = evaluate_modulator(f, &l1.next_phase, 96_000, 48_000.0, &d);
        let starts: Vec<(i64, i64)> = l2.tiles.iter().map(|t| (t.frame, t.ordinal)).collect();
        // The next loop opens on step 8 (frame 0: 8 × 12000 is the seam); the cycle count runs on —
        // steps 9, 12 and 15 start cycles 3, 4 and 5, still 36000 frames apart.
        assert_eq!(starts, vec![(12_000, 3), (48_000, 4), (84_000, 5)], "the cycle clock runs on across the seam");
    }

    /// A loop-synced phrase doc on the 96000-frame / 8-step ("1/8" → step 12000) test grid.
    fn loop_sync_phrase_doc(mod_id: &str, select: &str, pools: &str) -> LoopDoc {
        LoopDoc::from_json_str(&format!(
            r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 8 }},
                 "harmony": {{ "root": "C", "mode": "ionian" }},
                 "modulators": {{ "{mod_id}": {{ "type": "step_seq", "output": "note", "sync": "loop",
                     "length": 8, "step_duration": "1/8",
                     "phrase": {{ "pool": ["a", "b"], "select": "{select}" }} }} }},
                 "sequence_pools": {{ {pools} }} }}"#
        ))
        .unwrap()
    }

    /// The engines-listen benchmark's `ninety-arp-m` grid (2026-10-24): 96 host steps of "1/96" fill a
    /// 512000-frame loop (135 bpm × 24 beats), but the step rounds to 5333 frames, so 96 steps end
    /// at 511968 — 32 frames before the seam.
    fn loop_filling_grid_doc(arm: &str) -> LoopDoc {
        let notes: Vec<String> = (0..12).map(|k| format!(r#"{{ "t": {k}, "offset": {}, "dur": 1 }}"#, k * 2)).collect();
        LoopDoc::from_json_str(&format!(
            r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 135, "master_steps": 24 }},
                 "harmony": {{ "root": "D", "mode": "ionian" }},
                 "modulators": {{ "s": {{ "type": "step_seq", "output": "note", "sync": "loop",
                     "length": 96, "step_duration": "1/96", {arm} }} }},
                 "sequence_pools": {{ "up": {{ "fit": "literal", "length": 12, "notes": [{}] }} }} }}"#,
            notes.join(", ")
        ))
        .unwrap()
    }

    #[test]
    fn a_loop_sync_grid_that_fills_the_loop_starts_nothing_on_the_seam() {
        // Under loop-sync the seam is step 96 = the next loop's step 0. The walk must not start a
        // 97th step (a re-struck step 0) or a 9th tile 32 frames before it: that tile's notes were
        // carried over the seam on top of the next loop's own first tile, the same voice ids 32
        // frames apart, and a modal voice re-pitched that way burst to +27 dB.
        let phrase = loop_filling_grid_doc(r#""phrase": { "pool": ["up"] }"#);
        let total = phrase.total_frames(48_000.0);
        assert_eq!((total, (total as f32 / 96.0).round() as usize * 96), (512_000, 511_968), "the grid rounds short");
        let m = phrase.find_modulator("s").unwrap();
        let l1 = evaluate_modulator(m, &ModulatorPhase::default(), total, 48_000.0, &phrase);
        let strikes = ev(&l1).iter().filter(|e| !e.2).count();
        assert_eq!((l1.tiles.len(), strikes), (8, 96), "eight 12-step tiles, one strike per step");
        assert!(l1.next_phase.pending_notes.is_empty(), "nothing carried over the seam: {:?}", l1.next_phase.pending_notes);
        let l2 = evaluate_modulator(m, &l1.next_phase, total, 48_000.0, &phrase);
        assert_eq!(ev(&l2), ev(&l1), "every loop plays the same eight tiles");

        let steps = loop_filling_grid_doc(r#""steps": [{ "step": 0, "voicing": { "v": { "offset": 0 } } }]"#);
        let m = steps.find_modulator("s").unwrap();
        let l1 = evaluate_modulator(m, &ModulatorPhase::default(), total, 48_000.0, &steps);
        assert_eq!(l1.note_events.iter().map(|e| e.frame).collect::<Vec<_>>(), [0], "step 0 once, at the loop top");
        assert_eq!(l1.tiles.len(), 1, "one cycle per loop");
    }

    #[test]
    fn loop_sync_started_straddling_tile_advances_the_ordinal() {
        // Tiles of 5 steps on an 8-step loop: tile 0 [0,5) = a, tile 1 [5,10) = b STRADDLES the
        // seam. It was started, so the ordinal must count it (D5) — the next loop re-anchors
        // at step 0 as ordinal 2 (cycle ⇒ a again), not a replay of ordinal 1 (b).
        let d = loop_sync_phrase_doc(
            "ls",
            "cycle",
            r#""a": { "fit": "literal", "length": 5, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }] },
               "b": { "fit": "literal", "length": 5, "notes": [{ "t": 0, "offset": 4, "dur": 0.5 }] }"#,
        );
        let m = d.find_modulator("ls").unwrap();
        let l1 = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let e1 = ev(&l1);
        assert!(e1.contains(&(0, 0.0, false, "p0".into())), "tile 0 = a: {e1:?}");
        assert!(e1.contains(&(60_000, 4.0, false, "p0".into())), "tile 1 = b at step 5: {e1:?}");
        assert_eq!(l1.next_phase.phrase_ordinal, 2, "both started tiles count");
        let l2 = evaluate_modulator(m, &l1.next_phase, 96_000, 48_000.0, &d);
        let e2 = ev(&l2);
        assert!(e2.contains(&(0, 0.0, false, "p0".into())), "loop 2 tile 0 is ordinal 2 ⇒ a: {e2:?}");
        assert!(!e2.contains(&(0, 4.0, false, "p0".into())), "not a replay of tile 1's selection");
    }

    #[test]
    fn loop_sync_early_jittered_onset_keeps_its_gate_length() {
        // Pick a modulator id whose seeded jitter for (ordinal 0, note 0) is NEGATIVE, so the
        // tile-0 onset is clamped to frame 0; its release must ride the CLAMPED onset (D7):
        // exactly dur · step after frame 0, not dur · step after the unclamped (negative) frame.
        let amp = 40.0 * 48_000.0 / 1000.0;
        let id = (0..64)
            .map(|i| format!("j{i}"))
            .find(|id| jitter_offset(amp, hash_site(PHRASE_SEED, id, 0, "hum0")) < 0)
            .expect("some id jitters early");
        let mut d = loop_sync_phrase_doc(
            &id,
            "cycle",
            r#""a": { "fit": "literal", "length": 1, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }] },
               "b": { "fit": "literal", "length": 1, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }] }"#,
        );
        d.transport.timing_jitter_ms = 40.0;
        let m = d.find_modulator(&id).unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let evs = ev(&out);
        assert_eq!(evs[0], (0, 0.0, false, "p0".into()), "early-jittered tile-0 strike clamps to frame 0: {evs:?}");
        assert_eq!(evs[1], (6_000, 0.0, true, "p0".into()), "its release is dur 0.5 × 12000 after the CLAMPED onset: {evs:?}");
    }

    #[test]
    fn phrase_gate_ending_at_the_tile_edge_releases_before_the_next_strike() {
        // t + dur == length is legal; the off must land strictly before the next tile's
        // frame-0 strike of the (shared) p-id — never on it, never after it.
        let d = loop_sync_phrase_doc(
            "edge",
            "cycle",
            r#""a": { "fit": "literal", "length": 4, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }, { "t": 3.5, "offset": 7, "dur": 0.5 }] },
               "b": { "fit": "literal", "length": 4, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }, { "t": 3.5, "offset": 7, "dur": 0.5 }] }"#,
        );
        let m = d.find_modulator("edge").unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let evs = ev(&out);
        assert!(evs.contains(&(42_000, 7.0, false, "p1".into())), "the t:3.5 strike: {evs:?}");
        assert!(evs.contains(&(47_999, 0.0, true, "p1".into())), "its off is pulled to the tile's last frame: {evs:?}");
        assert!(evs.contains(&(48_000, 0.0, false, "p0".into())), "next tile strikes at frame 48000: {evs:?}");
        let off_pos = evs.iter().position(|e| e.0 == 47_999).unwrap();
        let next_pos = evs.iter().position(|e| e.0 == 48_000).unwrap();
        assert!(off_pos < next_pos, "release precedes the next tile's strike in dispatch order");
    }

    /// Frame-sorted (strike/release) sequence of one voice id, dispatch order breaking ties.
    fn id_timeline(out: &ModulatorOutput, id: &str) -> Vec<(usize, bool)> {
        let mut v: Vec<(usize, bool)> = ev(out).into_iter().filter(|e| e.3 == id).map(|e| (e.0, e.2)).collect();
        v.sort_by_key(|e| e.0); // stable: same-frame events keep dispatch order
        v
    }

    #[test]
    fn jittered_next_tile_strike_never_precedes_this_tiles_release() {
        // A full-tile gate (t 0 + dur 4 = length) ends at the tile edge; the next tile
        // re-strikes the same p0 at t 0. With jitter the NEXT strike can land up to `amp`
        // frames early — the release must still come first, or it would silence the new
        // note (ADR 0028 D4: a gate ends inside its own tile, jitter included).
        let amp = 40.0 * 48_000.0 / 1000.0;
        let id = (0..64)
            .map(|i| format!("g{i}"))
            .find(|id| jitter_offset(amp, hash_site(PHRASE_SEED, id, 1, "hum0")) < -2)
            .expect("some id jitters tile 1's head early");
        let mut d = loop_sync_phrase_doc(
            &id,
            "cycle",
            r#""a": { "fit": "literal", "length": 4, "notes": [{ "t": 0, "offset": 0, "dur": 4 }] },
               "b": { "fit": "literal", "length": 4, "notes": [{ "t": 0, "offset": 0, "dur": 4 }] }"#,
        );
        d.transport.timing_jitter_ms = 40.0;
        let m = d.find_modulator(&id).unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let tl = id_timeline(&out, "p0");
        let pattern: Vec<bool> = tl.iter().map(|e| e.1).collect();
        assert_eq!(pattern, vec![false, true, false, true], "strike/release must alternate: {tl:?}");
        assert!(tl[1].0 < tl[2].0, "tile 0's release strictly precedes tile 1's early strike: {tl:?}");
    }

    #[test]
    fn free_run_seam_aligned_tile_keeps_its_early_jittered_head() {
        // Free-run, 1-step tiles on the 12000-frame grid: loop 1's tiles run 0..96000 and end
        // exactly on the seam, so loop 2's first tile starts at frame 0. If its head jitters
        // early, nothing emitted it last loop (no look-ahead) — it must clamp to frame 0 like a
        // loop-sync edge note, not vanish and leave an orphan note-off.
        let amp = 40.0 * 48_000.0 / 1000.0;
        let id = (0..64)
            .map(|i| format!("f{i}"))
            .find(|id| jitter_offset(amp, hash_site(PHRASE_SEED, id, 8, "hum0")) < 0)
            .expect("some id jitters ordinal 8's head early");
        let d = LoopDoc::from_json_str(&format!(
            r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 8, "timing_jitter_ms": 40 }},
                 "harmony": {{ "root": "C", "mode": "ionian" }},
                 "modulators": {{ "{id}": {{ "type": "step_seq", "output": "note", "sync": "free",
                     "length": 8, "step_duration": "1/8", "phrase": {{ "pool": ["a"] }} }} }},
                 "sequence_pools": {{ "a": {{ "fit": "literal", "length": 1, "notes": [{{ "t": 0, "offset": 0, "dur": 0.5 }}] }} }} }}"#
        ))
        .unwrap();
        let m = d.find_modulator(&id).unwrap();
        let l1 = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        assert_eq!(l1.next_phase.phrase_ordinal, 8, "eight tiles, the last ending on the seam");
        let l2 = evaluate_modulator(m, &l1.next_phase, 96_000, 48_000.0, &d);
        let tl = id_timeline(&l2, "p0");
        assert_eq!(tl.first(), Some(&(0, false)), "the early-jittered seam tile head clamps to frame 0: {tl:?}");
        let strikes = tl.iter().filter(|e| !e.1).count();
        let releases = tl.iter().filter(|e| e.1).count();
        assert_eq!((strikes, releases), (8, 8), "every tile of loop 2 strikes and releases: {tl:?}");
        let pattern: Vec<bool> = tl.iter().map(|e| e.1).collect();
        assert!(pattern.chunks(2).all(|c| c == [false, true]), "alternating, no orphan release: {tl:?}");
    }

    #[test]
    fn phrase_active_pins_selection() {
        // ph_midi pins active = cell_a: cell_b's offsets (4, 11) never appear.
        let d = phrase_fixture();
        let m = d.find_modulator("ph_midi").unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        assert!(!ev(&out).iter().any(|(_, o, rel, _)| !rel && (*o == 4.0 || *o == 11.0)));
    }

    #[test]
    fn phrase_chord_fit_snaps_to_the_authored_harmony_without_the_orrery() {
        // ADR 0028 amendment (2026-07-09): fit:"chord" (the default) is no longer inert
        // under a plain render — cell_a's offset-16 note (an octave + MAJOR third) snaps
        // into a dorian doc's scale (→ 15) at expansion. fit:"literal" and an
        // Orrery-materialized `realized` copy still play verbatim.
        let strikes = |d: &LoopDoc| -> Vec<f32> {
            let m = d.find_modulator("ph_midi").unwrap();
            let mut phase = ModulatorPhase::default();
            let mut offs = vec![];
            for _ in 0..4 {
                let out = evaluate_modulator(m, &phase, 96_000, 48_000.0, d);
                offs.extend(ev(&out).iter().filter(|(_, _, rel, _)| !rel).map(|(_, o, _, _)| *o));
                phase = out.next_phase;
            }
            offs
        };

        let mut dorian = phrase_fixture();
        dorian.harmony.mode = "dorian".into();
        let offs = strikes(&dorian);
        assert!(offs.contains(&15.0), "the major third snapped into the scale: {offs:?}");
        assert!(!offs.contains(&16.0), "no out-of-scale pitch survives chord fit: {offs:?}");

        let mut literal = dorian.clone();
        literal.sequence_pools.get_mut("cell_a").unwrap().fit = "literal".into();
        let offs = strikes(&literal);
        assert!(offs.contains(&16.0) && !offs.contains(&15.0), "literal plays as written: {offs:?}");

        let mut realized = dorian.clone();
        realized.modulators.get_mut("ph_midi").unwrap().phrase.as_mut().unwrap().realized =
            Some(Box::new(crate::loop_doc::PhraseDef {
                fit: "chord".into(),
                tags: vec![],
                length: 4,
                notes: vec![crate::loop_doc::PhraseNote {
                    t: 0.0,
                    offset: 16.0,
                    dur: Some(0.5),
                    legato: false,
                    prob: 1.0,
                    velocity: None,
                    cents_offset: 0.0,
                }],
            }));
        let offs = strikes(&realized);
        assert!(
            offs.contains(&16.0) && !offs.contains(&15.0),
            "the Orrery's realized copy is trusted verbatim, never re-fit: {offs:?}"
        );
    }

    #[test]
    fn phrase_legato_chain_shares_the_head_voice_and_holds_hold_notes() {
        // "slurred" (slur): head holds (no dur), two legato followers share its p0 id —
        // the engine glides them — and only the last (dur 2.0) releases.
        let d = phrase_fixture();
        let m = d.find_modulator("slurred").unwrap();
        let out = evaluate_modulator(m, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let evs = ev(&out);
        let strikes: Vec<_> = evs.iter().filter(|(_, _, rel, _)| !rel).collect();
        assert_eq!(strikes.len(), 3, "{evs:?}");
        assert!(strikes.iter().all(|(_, _, _, id)| id == "p0"), "chain shares the head id: {evs:?}");
        assert_eq!(
            strikes.iter().map(|(f, o, _, _)| (*f, *o)).collect::<Vec<_>>(),
            [(0, 12.0), (24_000, 7.0), (48_000, 14.0)]
        );
        let offs: Vec<_> = evs.iter().filter(|(_, _, rel, _)| *rel).collect();
        assert_eq!(offs.len(), 1);
        assert_eq!((offs[0].0, offs[0].3.as_str()), (72_000, "p0"));

        // "pad_hold" (hold): one dur-less note per tile — the SAME p0 id + offset every
        // loop, so the engine sustains it instead of stacking fresh voices.
        let mh = d.find_modulator("pad_hold").unwrap();
        let h1 = evaluate_modulator(mh, &ModulatorPhase::default(), 96_000, 48_000.0, &d);
        let h2 = evaluate_modulator(mh, &h1.next_phase, 96_000, 48_000.0, &d);
        for h in [&h1, &h2] {
            let evs = ev(h);
            assert_eq!(evs.len(), 1);
            assert_eq!((evs[0].1, evs[0].2, evs[0].3.as_str()), (-12.0, false, "p0"));
        }
    }

    #[test]
    fn free_running_note_carries_phase_across_loops() {
        let d = fixture();
        let rhy = d.find_modulator("rhy_noise").unwrap(); // sync free, 23 steps @ 1/16
        let p0 = ModulatorPhase::default();
        let l1 = evaluate_modulator(rhy, &p0, 48_000, 48_000.0, &d);
        let l2 = evaluate_modulator(rhy, &l1.next_phase, 48_000, 48_000.0, &d);
        // free, polymetric → phase advances; not a clean reset to 0
        assert_ne!(l1.next_phase.step_index, 0);
        assert_ne!(l1.next_phase.step_index, l2.next_phase.step_index);
    }
}
