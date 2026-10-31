//! Send-bus / effects framework (CE-R4.4).
//!
//! Each [`Bus`] owns its effects chain as a plain `Vec` of hand-rolled nodes, ticked in series.
//! Instruments route a `send` amount into a bus; the bus runs its chain on the accumulated send
//! signal and returns the wet result to the master mix at `return_gain_db`.
//!
//! Live params are **plainly owned** by each node (all values normalized `0..1`; each effect
//! maps to native units internally); the engine sets them at control rate through
//! [`Bus::set_effect_param`]. The bus is built once and **reused** across loops so reverb tails
//! cross the loop seam (CE-R4.4); only coefficients change, never the buffers. Everything is
//! allocation-free and RNG-free in `tick`, so a bus render is bit-reproducible (CE-R7.3).
//!
//! History: this chain used to be a fundsp `Net` with `Shared` param handles. fundsp `Shared`
//! is an `Arc<AtomicU32>`, so **`Bus::clone` aliased the live params between the clones** — two
//! renders from cloned engines (then the learner's counterfactual render; now a live-edit fork
//! and its crossfade partner) raced each other's control-rate writes through the shared atoms, a
//! CE-R7.3 violation whenever a matrix route modulated a bus param. Owning the params by value
//! makes `Clone` genuinely deep; pinned by `bus_clone_is_deep_and_render_isolated` below.

use crate::dsp::{AllPass, Comb, ALLPASS_TUNINGS, COMB_TUNINGS, STEREO_SPREAD};
use crate::loop_doc;

/// Reverb pre-delay maximum in milliseconds (matches `loop_doc::PREDELAY_RANGE`).
const PREDELAY_MS_MAX: f32 = 100.0;
/// Fixed gain into the reverb tank (Freeverb convention) — tames the parallel-comb sum.
const REVERB_INPUT_GAIN: f32 = 0.015;
/// Shimmer pitch ratio: +12 semitones (one octave up).
const SHIMMER_RATIO: f32 = 2.0;
/// Shimmer grain buffer length in seconds.
const SHIMMER_GRAIN_S: f32 = 0.050;
/// Comb feedback at the back-compat default `room` (0.5): `0.5*0.28 + 0.7`. The reverb's output
/// is gain-compensated relative to this so `room` 0.5 is unchanged and higher `room` doesn't clip.
const FEEDBACK_REF: f32 = 0.84;

// ──────────────────────────────────────────────────────────────────────────
// Shimmer pitch-shifter — deterministic time-domain octave-up via a dual-read-
// pointer delay line with raised-cosine overlap-add. No RNG/wallclock; state
// (buffer + pointers) persists so the shimmer tail crosses the loop seam.
// ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) struct ShimmerShift {
    buf: Vec<f32>,
    wpos: usize,
    /// Read delay as a fraction of the buffer, swept to produce the pitch ratio.
    frac: f32,
}
impl ShimmerShift {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(4)], wpos: 0, frac: 0.0 }
    }
    #[inline]
    fn read(&self, delay_frac: f32) -> f32 {
        let n = self.buf.len();
        let d = delay_frac * (n as f32 - 1.0);
        let mut rp = self.wpos as f32 - d;
        if rp < 0.0 {
            rp += n as f32;
        }
        let i0 = (rp.floor() as usize) % n;
        let i1 = (i0 + 1) % n;
        let g = rp - rp.floor();
        self.buf[i0] * (1.0 - g) + self.buf[i1] * g
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let n = self.buf.len();
        self.buf[self.wpos] = x;
        let f1 = self.frac;
        let f2 = (self.frac + 0.5).fract();
        // Raised-cosine crossfade; the two taps are a half-buffer apart so each is windowed to
        // ~0 across its wrap discontinuity, and the windows sum to ~1.
        let w1 = 0.5 - 0.5 * (std::f32::consts::TAU * f1).cos();
        let w2 = 0.5 - 0.5 * (std::f32::consts::TAU * f2).cos();
        let out = self.read(f1) * w1 + self.read(f2) * w2;
        // Octave up ⇒ delay shrinks over time (read advances faster than write).
        self.frac -= (SHIMMER_RATIO - 1.0) / (n as f32 - 1.0);
        if self.frac < 0.0 {
            self.frac += 1.0;
        }
        self.wpos = (self.wpos + 1) % n;
        out
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Reverb effect — a Freeverb tank (reused dsp.rs primitives, live coefficients)
// with a pre-delay and an octave-up shimmer feedback path. A plain struct with
// owned state, ticked directly by the bus chain (deep `Clone`, no shared atoms).
// ──────────────────────────────────────────────────────────────────────────

/// The reverb's doc-param leaves, in declaration order (the authoring surface).
pub const REVERB_PARAM_LEAVES: [&str; 5] = ["room", "damping", "shimmer", "width", "predelay_ms"];

#[derive(Clone)]
pub(crate) struct ReverbNode {
    sample_rate: f32,
    // Live params, normalized 0..1 (engine/bus write these via `set_param`; tick reads them).
    room: f32,
    damping: f32,
    shimmer: f32,
    width: f32,
    predelay: f32,
    // Freeverb tank.
    combs_l: Vec<Comb>,
    combs_r: Vec<Comb>,
    aps_l: Vec<AllPass>,
    aps_r: Vec<AllPass>,
    // Pre-delay ring.
    pre_l: Vec<f32>,
    pre_r: Vec<f32>,
    pre_idx: usize,
    // Shimmer shifters + the wet output fed back into them.
    shift_l: ShimmerShift,
    shift_r: ShimmerShift,
    fb_l: f32,
    fb_r: f32,
}

impl ReverbNode {
    fn new(sample_rate: f32) -> Self {
        let mut node = Self {
            sample_rate: 0.0,
            room: 0.5,
            damping: 0.5,
            shimmer: 0.0,
            width: 0.5,
            predelay: 0.0,
            combs_l: Vec::new(),
            combs_r: Vec::new(),
            aps_l: Vec::new(),
            aps_r: Vec::new(),
            pre_l: Vec::new(),
            pre_r: Vec::new(),
            pre_idx: 0,
            shift_l: ShimmerShift::new(4),
            shift_r: ShimmerShift::new(4),
            fb_l: 0.0,
            fb_r: 0.0,
        };
        node.rebuild_buffers(sample_rate);
        node
    }

    /// (Re)allocate the sample-rate-dependent buffers, clearing tail state but **preserving** the
    /// live param values (so externally-set params survive a sample-rate change).
    fn rebuild_buffers(&mut self, sample_rate: f32) {
        let scale = sample_rate / 44100.0;
        let s = |n: usize| ((n as f32) * scale) as usize;
        let pre_len = ((PREDELAY_MS_MAX * 0.001 * sample_rate) as usize).max(1) + 1;
        let grain = ((SHIMMER_GRAIN_S * sample_rate) as usize).max(4);
        self.sample_rate = sample_rate;
        self.combs_l = COMB_TUNINGS.iter().map(|&t| Comb::new(s(t))).collect();
        self.combs_r = COMB_TUNINGS.iter().map(|&t| Comb::new(s(t) + s(STEREO_SPREAD))).collect();
        self.aps_l = ALLPASS_TUNINGS.iter().map(|&t| AllPass::new(s(t))).collect();
        self.aps_r = ALLPASS_TUNINGS.iter().map(|&t| AllPass::new(s(t) + s(STEREO_SPREAD))).collect();
        for ap in self.aps_l.iter_mut().chain(self.aps_r.iter_mut()) {
            ap.feedback = 0.5;
        }
        self.pre_l = vec![0.0; pre_len];
        self.pre_r = vec![0.0; pre_len];
        self.pre_idx = 0;
        self.shift_l = ShimmerShift::new(grain);
        self.shift_r = ShimmerShift::new(grain);
        self.fb_l = 0.0;
        self.fb_r = 0.0;
    }

    /// Set one live param (normalized 0..1) by its doc leaf name. Unknown leaves are ignored.
    #[inline]
    fn set_param(&mut self, leaf: &str, v01: f32) {
        let v = v01.clamp(0.0, 1.0);
        match leaf {
            "room" => self.room = v,
            "damping" => self.damping = v,
            "shimmer" => self.shimmer = v,
            "width" => self.width = v,
            "predelay_ms" => self.predelay = v,
            _ => {}
        }
    }

    /// One stereo sample through the reverb. Allocation- and RNG-free (CE-R7.3).
    #[inline]
    fn tick(&mut self, input: [f32; 2]) -> [f32; 2] {
        let room = self.room;
        let damping = self.damping;
        let shimmer = self.shimmer;
        let width = self.width;
        let predelay = self.predelay;

        // Live tank coefficients (cheap: 16 combs).
        let feedback = room * 0.28 + 0.7;
        let damp1 = damping * 0.4;
        let damp2 = 1.0 - damp1;
        for c in self.combs_l.iter_mut().chain(self.combs_r.iter_mut()) {
            c.feedback = feedback;
            c.damp1 = damp1;
            c.damp2 = damp2;
        }

        // Pre-delay (integer samples, deterministic).
        let n_pre = self.pre_l.len();
        self.pre_l[self.pre_idx] = input[0];
        self.pre_r[self.pre_idx] = input[1];
        let pd = ((predelay * PREDELAY_MS_MAX * 0.001 * self.sample_rate) as usize).min(n_pre - 1);
        let rp = (self.pre_idx + n_pre - pd) % n_pre;
        let dry_l = self.pre_l[rp];
        let dry_r = self.pre_r[rp];
        self.pre_idx = (self.pre_idx + 1) % n_pre;

        // Octave-up shimmer feedback from the previous wet output (clamped for stability).
        let inj_l = self.shift_l.process(self.fb_l.clamp(-4.0, 4.0));
        let inj_r = self.shift_r.process(self.fb_r.clamp(-4.0, 4.0));

        // Freeverb mono input = pre-delayed dry + shimmer injection. The outer shimmer loop gain
        // is ~0.75*shimmer (< 1), so the cascade decays.
        let inp = ((dry_l + dry_r) + shimmer * (inj_l + inj_r)) * REVERB_INPUT_GAIN;
        let mut wet_l = 0.0;
        let mut wet_r = 0.0;
        for c in self.combs_l.iter_mut() {
            wet_l += c.process(inp);
        }
        for c in self.combs_r.iter_mut() {
            wet_r += c.process(inp);
        }
        for ap in self.aps_l.iter_mut() {
            wet_l = ap.process(wet_l);
        }
        for ap in self.aps_r.iter_mut() {
            wet_r = ap.process(wet_r);
        }

        // Room-compensation: the parallel-comb steady-state gain scales ~1/(1-feedback), so the
        // wet level rises steeply with `room` (near self-oscillation at room→1 → clipping). Scale
        // the output by (1-feedback)/(1-FEEDBACK_REF) so wet energy stays roughly constant across
        // `room`. Clamped to ≤1 so `room` at/below the back-compat default (0.5 → FEEDBACK_REF) is
        // unchanged and only long tails are tamed. Applied before the shimmer feedback tap so the
        // octave cascade can't run away at high room either.
        let comp = ((1.0 - feedback) / (1.0 - FEEDBACK_REF)).min(1.0);
        wet_l *= comp;
        wet_r *= comp;
        self.fb_l = wet_l;
        self.fb_r = wet_r;

        // Reverb-internal stereo width (mid/side); master stereo_width applies later, downstream.
        let side_gain = width * 2.0;
        let mid = (wet_l + wet_r) * 0.5;
        let side = (wet_r - wet_l) * 0.5;
        [mid - side * side_gain, mid + side * side_gain]
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Bus — a hand-rolled effects chain + send accumulator + wet return.
// ──────────────────────────────────────────────────────────────────────────

/// One effect in a bus chain (an enum so more kinds — delay, chorus, … — slot in). Every
/// variant owns its state and params by value, so `Bus: Clone` is a **deep** clone — two bus
/// clones can render concurrently with zero interaction (engine forks rely on it).
#[derive(Clone)]
enum FxNode {
    Reverb(ReverbNode),
}

impl FxNode {
    #[inline]
    fn tick(&mut self, input: [f32; 2]) -> [f32; 2] {
        match self {
            FxNode::Reverb(r) => r.tick(input),
        }
    }
    #[inline]
    fn set_param(&mut self, leaf: &str, v01: f32) {
        match self {
            FxNode::Reverb(r) => r.set_param(leaf, v01),
        }
    }
}

#[derive(Clone)]
pub struct Bus {
    /// The effects chain, ticked in sorted-effect-id order (empty ⇒ stereo passthrough).
    chain: Vec<(String, FxNode)>,
    /// The doc's `(effect id, effect type)` chain this bus was built from — unknown types
    /// included, though they build no node — so the engine can spot a chain edit (see
    /// [`Bus::built_from`]).
    shape: Vec<(String, String)>,
    return_gain: f32,
    acc_l: f32,
    acc_r: f32,
}

impl Bus {
    /// Build a bus from its doc definition. Effects are chained in sorted-id order (`BTreeMap`
    /// iteration). Each effect's params are seeded from the doc (normalized).
    pub fn from_doc(bus: &loop_doc::Bus, sample_rate: f32) -> Self {
        let mut chain: Vec<(String, FxNode)> = Vec::new();
        for (fx_id, fx) in &bus.effects {
            // `match` (not `if`) intentionally: more effect kinds (delay, chorus, …) slot in here.
            #[allow(clippy::single_match)]
            match fx.kind.as_str() {
                "reverb" => {
                    let mut node = ReverbNode::new(sample_rate);
                    for leaf in REVERB_PARAM_LEAVES {
                        node.set_param(leaf, fx.norm(leaf).unwrap_or(0.0));
                    }
                    chain.push((fx_id.clone(), FxNode::Reverb(node)));
                }
                // Unknown effect type → skip (forward-compatible).
                _ => {}
            }
        }
        let shape = bus.effect_chain().map(|(id, kind)| (id.to_string(), kind.to_string())).collect();
        Self { chain, shape, return_gain: bus.return_amp(), acc_l: 0.0, acc_r: 0.0 }
    }

    /// Whether this bus was built from `bus`'s effect chain (same effect ids and types, same
    /// order). `false` ⇒ the chain was edited under the same bus id and the engine must rebuild:
    /// params alone can't turn the built nodes into the new chain.
    pub fn built_from(&self, bus: &loop_doc::Bus) -> bool {
        self.shape.iter().map(|(id, kind)| (id.as_str(), kind.as_str())).eq(bus.effect_chain())
    }

    /// Accumulate an instrument's send into this bus (called per sample, per sending instrument).
    #[inline]
    pub fn add_send(&mut self, l: f32, r: f32, amt: f32) {
        self.acc_l += l * amt;
        self.acc_r += r * amt;
    }

    /// Run the chain on the accumulated send signal, scale by the return (`return_gain_db` as an amplitude), and clear the
    /// accumulator. Returns the wet stereo signal to sum into the master mix. An empty chain
    /// is a stereo passthrough, so the bus still returns its dry send.
    #[inline]
    pub fn process(&mut self) -> (f32, f32) {
        let mut frame = [self.acc_l, self.acc_r];
        for (_, node) in self.chain.iter_mut() {
            frame = node.tick(frame);
        }
        self.acc_l = 0.0;
        self.acc_r = 0.0;
        (frame[0] * self.return_gain, frame[1] * self.return_gain)
    }

    /// Set a live effect param (normalized 0..1). No-op for unknown effect/param.
    #[inline]
    pub fn set_effect_param(&mut self, fx: &str, param: &str, v01: f32) {
        if let Some((_, node)) = self.chain.iter_mut().find(|(id, _)| id == fx) {
            node.set_param(param, v01);
        }
    }

    /// The return's amplitude (`return_gain_db` through `db_to_amp`: up to the +6 dB ceiling).
    #[inline]
    pub fn set_return_gain(&mut self, amp: f32) {
        self.return_gain = amp.max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn reverb_bus(shimmer: f32) -> Bus {
        let mut effects = BTreeMap::new();
        let mut params = BTreeMap::new();
        params.insert("room".to_string(), serde_json::json!(0.6).into());
        params.insert("damping".to_string(), serde_json::json!(0.3).into());
        params.insert("shimmer".to_string(), serde_json::json!(shimmer).into());
        params.insert("width".to_string(), serde_json::json!(0.5).into());
        params.insert("predelay_ms".to_string(), serde_json::json!(0.0).into());
        effects.insert(
            "verb".to_string(),
            loop_doc::BusEffect { id: "verb".to_string(), kind: "reverb".to_string(), params },
        );
        let bus = loop_doc::Bus { id: "rv".to_string(), kind: None, return_gain_db: 0.0, latency_ms: 0.0, effects };
        Bus::from_doc(&bus, 48_000.0)
    }

    #[test]
    fn reverb_bus_produces_decaying_persistent_tail() {
        let mut bus = reverb_bus(0.0);
        bus.add_send(1.0, 1.0, 1.0);
        let (mut l, mut r) = bus.process();
        let early = (l.abs() + r.abs()).max(1e-9);
        let mut tail = 0.0f64;
        for _ in 0..24_000 {
            let (nl, nr) = bus.process(); // silence in
            l = nl;
            r = nr;
            tail += (l as f64).powi(2) + (r as f64).powi(2);
        }
        assert!(tail > 0.0, "reverb bus should ring out after the impulse");
        assert!(early.is_finite() && l.is_finite() && r.is_finite());
    }

    /// Octave-up shimmer injects energy at 2×f0 that a *linear* reverb cannot create from a pure
    /// tone — so the octave band gains far more energy with shimmer than without.
    #[test]
    fn shimmer_injects_octave_energy() {
        let f0 = 200.0f32;
        let octave_energy = |shimmer: f32| -> f64 {
            let mut bus = reverb_bus(shimmer);
            let mut tail = Vec::with_capacity(48_000);
            for i in 0..48_000 {
                if i < 4_000 {
                    let x = (std::f32::consts::TAU * f0 * i as f32 / 48_000.0).sin() * 0.5;
                    bus.add_send(x, x, 1.0);
                }
                let (l, r) = bus.process();
                if i >= 24_000 {
                    tail.push((l + r) * 0.5);
                }
            }
            band_energy(&tail, 48_000.0, 2.0 * f0, 40.0)
        };
        let dry = octave_energy(0.0);
        let wet = octave_energy(0.7);
        assert!(wet > dry * 4.0, "shimmer should add octave energy: dry={dry} shimmer={wet}");
    }

    /// Room-compensation keeps a high-`room` reverb bounded under sustained input (no runaway /
    /// clipping), where the raw near-unity comb feedback would otherwise ring up enormously.
    #[test]
    fn high_room_output_stays_bounded() {
        let mut bus = reverb_bus(0.5);
        bus.set_effect_param("verb", "room", 0.95);
        bus.set_effect_param("verb", "shimmer", 0.6);
        let mut peak = 0.0f32;
        for i in 0..96_000 {
            // sustained tone (worst case for buildup)
            let x = (std::f32::consts::TAU * 200.0 * i as f32 / 48_000.0).sin() * 0.5;
            bus.add_send(x, x, 1.0);
            let (l, r) = bus.process();
            peak = peak.max(l.abs()).max(r.abs());
        }
        assert!(peak.is_finite() && peak < 4.0, "high-room reverb should stay bounded (peak={peak})");
    }

    /// Regression (see module docs): `Bus::clone` must be a **deep** clone — writing a param on
    /// the clone must not leak into the original. The fundsp-`Shared` version aliased the live
    /// param atoms across clones, so two cloned renders raced each other's control-rate writes
    /// and contaminated both (CE-R7.3).
    #[test]
    fn bus_clone_is_deep_and_render_isolated() {
        let render = |bus: &mut Bus| -> Vec<f32> {
            let mut out = Vec::with_capacity(2 * 4_000);
            for i in 0..4_000 {
                let x = if i < 100 { 0.7 } else { 0.0 };
                bus.add_send(x, x, 1.0);
                let (l, r) = bus.process();
                out.push(l);
                out.push(r);
            }
            out
        };
        let mut control = reverb_bus(0.5);
        let mut original = reverb_bus(0.5);
        let mut clone = original.clone();
        // Divergent live writes on the clone (what a concurrent forked render does)…
        clone.set_effect_param("verb", "room", 0.95);
        clone.set_effect_param("verb", "shimmer", 0.9);
        // …must leave the original's render bit-identical to a never-cloned control.
        assert_eq!(render(&mut original), render(&mut control), "clone writes leaked into the original");
        // And the clone really did change (the writes went somewhere).
        assert_ne!(render(&mut clone), render(&mut control), "clone params should differ");
    }

    /// Two fresh buses, identical input ⇒ bit-identical output (CE-R7.3).
    #[test]
    fn bus_render_is_bit_reproducible() {
        let render = || -> Vec<f32> {
            let mut bus = reverb_bus(0.5);
            let mut out = Vec::with_capacity(2 * 8_000);
            for i in 0..8_000 {
                let x = if i < 100 { 0.7 } else { 0.0 };
                bus.add_send(x, x, 1.0);
                let (l, r) = bus.process();
                out.push(l);
                out.push(r);
            }
            out
        };
        assert_eq!(render(), render());
    }

    /// Summed magnitude in a frequency band [center-half_width, center+half_width].
    fn band_energy(x: &[f32], sr: f32, center: f32, half_width: f32) -> f64 {
        use realfft::RealFftPlanner;
        let n = x.len();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let mut input = x.to_vec();
        let mut spectrum = fft.make_output_vec();
        fft.process(&mut input, &mut spectrum).unwrap();
        let bin_hz = sr as f64 / n as f64;
        let mut sum = 0.0f64;
        for (k, c) in spectrum.iter().enumerate() {
            let freq = k as f64 * bin_hz;
            if (freq - center as f64).abs() <= half_width as f64 {
                sum += c.norm() as f64;
            }
        }
        sum
    }
}

