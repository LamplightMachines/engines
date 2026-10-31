//! Controller (CE-R8): per loop, decide what the operating point plays — the timbre axis (`drift`'s
//! bounded random walk, or `hold`/`live_edit` freezing it) and the movement axis (the Orrery's
//! generative harmony) — then re-base onto what was played.
//!
//! Modes (CE-R8.1): `drift` (bounded random walk with **directional momentum** — holds the same
//! param + sign for a short seeded run of loops so the operating point *travels* instead of
//! mean-reverting; the default), `hold` (no drift; the engine's free modulators still breathe), and
//! `live_edit` (no drift; the doc is swapped wholesale by an editor). Per loop, from the operating
//! point `Pₙ`, drift picks one controller-changeable base param and a signed step `δ`, plays
//! `Pₙ + δ`, and re-bases to it (CE-R8.3). Occasionally a categorical lever flips instead.
//!
//! **Guardrails (CE-R8.7, bounds subset):** `δ` is bounded by `max_change_per_loop`, and per-
//! param `[floor, ceiling]` (the Tier-2 safe range, guardrail floors/ceilings, the cumulative
//! `max_drift` band) bound every step — drift *reflects* off those edges (ADR 0032).

use crate::categorical::{categorical_candidates, value_label, CategoricalMove};
use crate::contract::{ControlPatch, HomeScope, LiveToggle, OptimizeMode, SessionControl};
use crate::hash::{hash64, hash_site, hash_to_unit};
use crate::loop_doc::{param_range, LoopDoc};
use crate::doc_value::DocValue;
use std::collections::BTreeMap;
use crate::orrery::{apply_octaves, drop_surface, enforce_ranges, pad_center_for, select_and_refit_phrases, vary_arp_pool, write_arp_density, write_oneshots, write_phrase, AdvanceOpts, ArrangePlan, Arranger, Gravity, HarmonicMove, HarmonicPreference, LeaderFit, OrreryConfig, Voyager};
use crate::theory::{pc_of_root, Pc, Scale};

mod candidates;
mod orrery_axis;
pub use candidates::{dither_candidates, dither_candidates_with, doc_swap_is_structural, path_targets_midi};
use candidates::capture_anchors;

/// Default per-loop probability of a categorical move instead of a numeric drift step.
pub const DEFAULT_CAT_CHANGE_PROB: f32 = 0.15;
/// Hash sub-stream salt for categorical rolls — disjoint from the numeric drift sub-keys.
const CAT_SALT: u64 = 0xCA7;

/// Drift directional momentum (CE-R8.1): a drift pick holds the same param + sign for a seeded
/// run of `DRIFT_RUN_MIN..=DRIFT_RUN_MAX` consecutive loops before re-picking, so the operating
/// point travels in one direction (more dynamic movement) instead of mean-reverting loop to loop.
const DRIFT_RUN_MIN: u32 = 2;
const DRIFT_RUN_MAX: u32 = 5;

/// Built-in per-loop step cap `|δ|` (normalized `0..1`) when neither the runtime nor the loop doc
/// authors one. Matches [`DitherConfig::default`].
pub const DEFAULT_MAX_CHANGE: f32 = 0.05;
/// Built-in **cumulative** drift half-width (normalized `0..1`): how far drift may move a param
/// from its authored anchor over the whole session, when the loop doc omits `max_drift` (ADR 0025).
pub const DEFAULT_MAX_DRIFT: f32 = 0.15;

#[derive(Clone, Copy, Debug)]
pub struct DitherConfig {
    /// Bound on |δ| per loop (CE-R8.7).
    pub max_change_per_loop: f32,
    /// Dither RNG seed (with `loop_id`, makes the choice reproducible, CE-R7.1).
    pub seed: u64,
    /// Probability, per loop, that the controller flips a categorical timbre lever (`wave`/
    /// `material`/`num_modes`/…) instead of taking a numeric drift step. `0.0` disables
    /// categorical moves (pure numeric). Skipped in `hold`.
    pub cat_change_prob: f32,
    /// MIDI drift (ADR 0026): when true, `type:"midi"` instrument params and matrix depths routed
    /// into them join the drift candidates, so drift also *moves* the external synth's CCs.
    /// Default false — the external synth's patch is the author's.
    pub include_midi: bool,
}
impl Default for DitherConfig {
    fn default() -> Self {
        Self { max_change_per_loop: 0.05, seed: 0x5EED, cat_change_prob: DEFAULT_CAT_CHANGE_PROB, include_midi: false }
    }
}

/// What the controller decided for one loop.
#[derive(Clone, Debug)]
pub struct LoopPick {
    /// The doc to play this loop: the operating point + this loop's move, stamped with `loop_id`.
    pub doc: LoopDoc,
    /// The canonical path the controller moved (empty if none).
    pub path: String,
    /// Signed native-unit step applied at `path` (0 for a categorical move or no move).
    pub delta: f32,
    /// The Orrery harmonic transfer applied this loop (Orrery mode, on phrase boundaries).
    pub harmonic_move: Option<HarmonicMove>,
    /// The categorical move applied this loop, if any (then `delta` is 0).
    pub categorical_move: Option<CategoricalMove>,
}

/// Remove a doc's own control settings (ADR 0033) — the session lifts them into its control.
fn strip_control(doc: &mut LoopDoc) {
    doc.optimize = None;
    doc.movement = None;
    doc.constraints.clear();
}

/// One step of a **reflecting** bounded walk in normalized space (ADR 0032): `base + want`, bounced
/// back by the overshoot at whichever edge of `[lo, hi]` it would cross (then clamped, for a step longer
/// than the band). A base already outside the band steps `|want|` back toward it. Returns the target
/// and, when the walk turned, the direction it now heads (`+1` up, `−1` down) — the drift streak adopts
/// it so momentum carries the param away from the edge instead of pinning it there. A zero-width band
/// pins the param (the way to freeze a lever: `safe_bounds` min == max).
pub(crate) fn reflect_step(base: f32, want: f32, (lo, hi): (f32, f32)) -> (f32, Option<f32>) {
    if hi - lo <= 1e-9 {
        return (base, None);
    }
    if base > hi {
        return ((base - want.abs()).max(lo), Some(-1.0));
    }
    if base < lo {
        return ((base + want.abs()).min(hi), Some(1.0));
    }
    let t = base + want;
    if t > hi {
        ((2.0 * hi - t).max(lo), Some(-1.0))
    } else if t < lo {
        ((2.0 * lo - t).min(hi), Some(1.0))
    } else {
        (t, None)
    }
}

/// An active drift streak (momentum): the candidate index + signed direction to hold, and how
/// many more loops to hold it. Seeded per streak off the streak-start `loop_id`, so a fresh
/// controller replaying loop ids in order reproduces the same voyage (CE-R7.1).
#[derive(Clone, Copy)]
struct DriftStreak {
    idx: usize,
    sign: f32,
    remaining: u32,
}

/// Moving-average window (in phrases) for the voice-change governor (Phase 5, WS5.3).
const VC_WINDOW: usize = 4;

/// Per-track organic return-home **hazard** params `(p0, k_n, k_t, min_dwell)` (Phase 5b; the
/// tension term `k_t` added in Phase 6a). Each phrase, a track returns home with probability
/// `clamp(p0 + k_n·(n−dwell) + k_t·tension, 0, HAZ_MAX)` once `n ≥ dwell`, where `n = loops_since_home`
/// and `tension` is the current chord's orbital tension (0..1) — so a return grows more likely both
/// the longer a track has been away **and** the more tense the harmony is (ADR 0017). Per the theory
/// review: bass = rare/long-dwell (a pedal persists), arp = frequent/short (a recurring motif),
/// one-hits = rare, chord = moderate (and cadence-gated in `advance`). Seeded ⇒ deterministic +
/// staggered across tracks.
const HAZ_MAX: f32 = 0.7;
const HAZ_BASS: (f32, f32, f32, u32) = (0.06, 0.04, 0.25, 4);
const HAZ_ARP: (f32, f32, f32, u32) = (0.20, 0.06, 0.30, 2);
const HAZ_ONESHOT: (f32, f32, f32, u32) = (0.06, 0.03, 0.20, 4);
const HAZ_CHORD: (f32, f32, f32, u32) = (0.12, 0.05, 0.35, 3);

/// Return-home hazard for `loops_since_home == n` at chord `tension`, given `(p0, k_n, k_t, dwell)`;
/// `0` before the dwell.
fn return_hazard((p0, k_n, k_t, dwell): (f32, f32, f32, u32), n: u32, tension: f32) -> f32 {
    if n < dwell {
        0.0
    } else {
        (p0 + k_n * (n - dwell) as f32 + k_t * tension).clamp(0.0, HAZ_MAX)
    }
}

/// Phase 6c **narrative drop**: at a tension peak (energy ≥ `DROP_ENERGY_MIN`, away from home, no
/// leader, no resolution pending) a prominent voice is silenced for one phrase so the texture breaks
/// down before the return lands — the producer's "thin before the drop". Per-phrase hazard
/// `clamp(p0 + k·(energy − min), 0, max)`, seeded ⇒ deterministic.
const DROP_ENERGY_MIN: f32 = 0.6;
const DROP_P0: f32 = 0.18;
const DROP_K: f32 = 0.6;
const DROP_MAX: f32 = 0.5;

/// A snapshot of the live Orrery voyage + arrangement, for telemetry/UI ([`Controller::orrery_telemetry`]).
#[derive(Clone, Debug)]
pub struct OrreryView {
    pub chord: String,
    pub tension: f32,
    pub loops_since_home: u32,
    /// Moving average of voice-changes-per-phrase (the Phase 5 governor's signal).
    pub voice_change_avg: f32,
    /// The active arrangement leader this phrase (`bass`/`arp`), if any (Phase 5b).
    pub leader: Option<String>,
    /// The Arranger's slow energy arc (0..1) and current arp density (euclid pulses) — Phase 6b.
    pub energy: f32,
    pub arp_pulses: u32,
    /// The role silenced by a narrative drop this phrase, if any (Phase 6c).
    pub dropped: Option<String>,
    /// Whether a homeward return fired this phrase (breach snap / chord cadence / track rest) — Phase 6d.
    pub returning_home: bool,
    /// The sequence-pool entry the primary arp's phrase arm is playing, if phrase-armed (ADR 0028).
    pub phrase_active: Option<String>,
    /// The pad voicing and the bass (semitone offsets from the root) and the chord's tones — what
    /// the editor panel's keyboard shows (ADR 0038).
    pub pad: Vec<i32>,
    pub bass: i32,
    pub tones: Vec<i32>,
}

/// Per-session Orrery state held by the controller (Orrery mode only).
struct OrreryState {
    voyager: Voyager,
    gravity: Gravity,
    pref: HarmonicPreference,
    cfg: OrreryConfig,
    /// Read-only fixed-pitch context (held drones) the voyage stays consonant with (WS4.2).
    fixed: Vec<Pc>,
    /// Recent per-phrase voice-change counts (last `VC_WINDOW`) — the governor's moving average.
    vc_recent: Vec<u32>,
    /// Per-track `loops_since_home` for the non-chord surfaces (`bass`/`arp`/`oneshot`) — the Phase 5b
    /// organic-return clocks. The chord track uses `voyager.loops_since_home()`.
    track_home: BTreeMap<String, u32>,
    /// The active arrangement leader this phrase (`bass`/`arp`), if any (Phase 5b — for telemetry).
    last_leader: Option<String>,
    /// The role silenced by a narrative drop this phrase, if any (Phase 6c — for telemetry).
    last_drop: Option<String>,
    /// Whether a return toward home fired this phrase — a `tension_max` breach snap, an organic chord
    /// return, or any per-track return (Phase 6d telemetry; ⇒ some `loops_since_home` just reset).
    last_returned_home: bool,
    /// The Arranger — the producer brain driving register/density/form via a slow energy arc (Phase 6b).
    arranger: Arranger,
    /// The last arrangement plan it emitted (for telemetry: energy + arp density).
    last_plan: ArrangePlan,
    /// Loops processed so far (phrase boundary = every `cfg.phrase_loops`).
    loops_seen: u64,
}

/// The authored Orrery-writable state of one modulator (Phase 6a): everything the **movement axis**
/// may rewrite — note `steps`, the `pitch_walk` (pool + order), the euclid `generator`
/// (pulses + rotation), and a control-LFO `wave`. The axis seam (ADR 0017) guarantees the optimize
/// axis never touches any of these, so reverting them is a *total* return to authored content.
#[derive(Clone, Default)]
struct ModSnapshot {
    steps: Vec<crate::loop_doc::Step>,
    pitch_walk: Option<crate::loop_doc::PitchWalk>,
    generator: Option<crate::loop_doc::Generator>,
    wave: Option<String>,
    /// The phrase arm incl. authored `active` (ADR 0028) — restoring clears any
    /// Orrery-materialized `realized`/selection back to the authored state.
    phrase: Option<crate::loop_doc::PhraseRef>,
}

/// The authored "home" of all Orrery-writable content, captured once at `Controller::new` **before**
/// any voyage runs (Phase 5, widened to the full `modulators.*` footprint in Phase 6a). Restoring it
/// reverts the Orrery footprint **without** clobbering timbre drift / override progress —
/// those live on instrument params / mix leaves the optimize axis owns, never captured here.
#[derive(Clone, Default)]
struct HomeSnapshot {
    /// Per modulator id: its authored Orrery-writable state.
    modulators: BTreeMap<String, ModSnapshot>,
}

impl HomeSnapshot {
    fn capture(doc: &LoopDoc) -> Self {
        let modulators = doc
            .modulators
            .iter()
            .map(|(id, m)| {
                let snap = ModSnapshot {
                    steps: m.steps.clone(),
                    pitch_walk: m.pitch_walk.clone(),
                    generator: m.generator.clone(),
                    wave: m.wave.clone(),
                    phrase: m.phrase.clone(),
                };
                (id.clone(), snap)
            })
            .collect();
        Self { modulators }
    }
}

pub struct Controller {
    base: LoopDoc,
    /// Authored Orrery "home" (Phase 5) — the revert/return target.
    home: HomeSnapshot,
    cfg: DitherConfig,
    candidates: Vec<String>,
    /// The control in effect (CE-R8.2, objectives_v0.1): the standing control with the song
    /// block's patch applied (ADR 0055). `None` ⇒ pure `drift` default.
    control: Option<SessionControl>,
    /// The standing session control as last set from outside ([`set_control`](Self::set_control)):
    /// what the next song block's patch applies over.
    standing: Option<SessionControl>,
    /// The song block's control patch (ADR 0055), while a block with `control` plays.
    block_control: Option<ControlPatch>,
    /// The live panel's toggles since the block started (ADR 0055 D3): they beat the block's patch.
    live: LiveToggle,
    /// Generative harmonic layer (set when the movement axis is Orrery).
    orrery: Option<OrreryState>,
    /// Active drift momentum streak (CE-R8.1); `None` ⇒ re-pick on the next drift loop.
    drift_streak: Option<DriftStreak>,
    /// Per-candidate authored value (normalized `0..1`) = the center of the cumulative `max_drift`
    /// band (ADR 0025). Captured after the safe-clamp in `new`/`swap_doc`, and re-centered on a param
    /// when an `override` sets it (an override wins over `max_drift`).
    anchors: BTreeMap<String, f32>,
    /// Last-applied `overrides` overlay (Phase 1) — diffed so each override injects once-on-change.
    applied_overrides: BTreeMap<String, DocValue>,
    /// Song tail (ADR 0029): the last block is over — no drift, no Orrery steps; the operating
    /// point (note routes dropped) just plays out while voices release.
    song_tail: bool,
}

/// The Tier-2 range the controller enforces for `path` (normalized): each side of an explicit doc
/// `safe_bounds` band is a hard limit; a side left to the canonical default table only bounds what
/// the controller *does* — it widens to include the `authored` value, so authoring outside the
/// default (a `level_db: -Infinity` mute, a quiet `master_gain_db`) is honoured and drift simply can't go further.
pub(crate) fn tier2_norm(doc: &LoopDoc, path: &str, authored: Option<f32>) -> (f32, f32) {
    let (lo, hi) = doc.safe_range_norm(path);
    let band = doc.safe_bounds.get(path);
    let Some(a) = authored else { return (lo, hi) };
    let lo = if band.and_then(|b| b.min).is_some() { lo } else { lo.min(a) };
    let hi = if band.and_then(|b| b.max).is_some() { hi } else { hi.max(a) };
    (lo, hi)
}

/// Clamp `v` into `[lo, hi]` without `f32::clamp`'s panic on an inverted band (`lo > hi`, e.g. a
/// both-sided `safe_bounds` the checker refuses, or a guardrail ceiling under the safe floor): the
/// ceiling wins.
fn clamp_band(v: f32, (lo, hi): (f32, f32)) -> f32 {
    v.max(lo).min(hi)
}

impl Controller {
    pub fn new(mut doc: LoopDoc, cfg: DitherConfig) -> Self {
        // A song (ADR 0029) is the session driver's business, and the doc's own control settings
        // (ADR 0033) were already lifted into the session control: the operating point carries
        // neither (it is serialized into every telemetry frame and cloned by every move).
        doc.song = None;
        strip_control(&mut doc);
        let candidates = dither_candidates_with(&doc, cfg.include_midi);
        // Clamp any authored value sitting outside an explicit `safe_bounds` side into it (ADR 0010),
        // so the controller never *starts* out of bounds. The canonical default range only bounds
        // drift — it widens to keep the authored value (an authored `level_db: -Infinity` stays silent; ADR 0032
        // amendment). A no-op for in-range docs (byte-identical).
        let mut base = doc;
        for path in &candidates {
            let v = base.get_param_norm(path).unwrap_or(0.5);
            let clamped = clamp_band(v, tier2_norm(&base, path, Some(v)));
            if (clamped - v).abs() > 1e-9 {
                base = base.with_param_norm(path, clamped);
            }
        }
        // Capture the authored Orrery "home" before any voyage can rewrite it (Phase 5). Clamping
        // above never touches note-modulator steps/pools or instrument `enabled`, so this is authored.
        let home = HomeSnapshot::capture(&base);
        let anchors = capture_anchors(&base, &candidates);
        Self {
            base,
            home,
            cfg,
            candidates,
            control: None,
            standing: None,
            block_control: None,
            live: LiveToggle::default(),
            orrery: None,
            drift_streak: None,
            anchors,
            applied_overrides: BTreeMap::new(),
            song_tail: false,
        }
    }

    /// Song block reset (ADR 0029): start block `doc` as if the session had just begun on it —
    /// operating point back to the authored block (drift travel undone, safe-clamped), drift streak
    /// cleared, `max_drift` anchors / candidates / Orrery home recaptured, overrides re-applied,
    /// and the **Orrery rebuilt from home** (a fresh voyage at the tonic, phrase clock + Arranger
    /// reset). `seed` re-keys every seeded site (drift, categorical, all Orrery draws). Kept: the
    /// standing session control (including live edits to it), with the block's control `patch`
    /// applied over it (ADR 0055) — the patch lasts until the next block start.
    pub fn begin_block(&mut self, doc: LoopDoc, seed: u64, patch: Option<ControlPatch>) {
        let standing = self.standing.take();
        let cfg = DitherConfig { seed, ..self.cfg };
        *self = Controller::new(doc, cfg);
        self.standing = standing;
        self.block_control = patch;
        self.install_control(self.effective_control());
    }

    /// The standing control with the song block's patch, then the live toggles, applied.
    fn effective_control(&self) -> Option<SessionControl> {
        let c = match &self.block_control {
            Some(p) => p.apply(self.standing.as_ref()),
            None => self.standing.clone(),
        };
        self.live.apply(c)
    }

    /// Song tail (ADR 0029): after the last block, drop every note route (the engine releases the
    /// orphaned voices) and freeze — `next` returns the unmoved operating point while it decays.
    pub fn begin_tail(&mut self) {
        self.base.matrix.retain(|_, e| e.kind != "note");
        self.drift_streak = None;
        self.song_tail = true;
    }

    pub fn operating_point(&self) -> &LoopDoc {
        &self.base
    }
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// Timbre-axis label (`"hold"`/`"drift"`/`"live_edit"`) for telemetry/UI.
    pub fn optimize_label(&self) -> &'static str {
        match self.optimize() {
            OptimizeMode::Hold => "hold",
            OptimizeMode::Drift => "drift",
            OptimizeMode::LiveEdit => "live_edit",
        }
    }

    /// Per-constrained-param status (native value vs band, in-range flag) for telemetry.
    pub fn constraint_status(&self) -> Vec<(String, f32, Option<f32>, Option<f32>, bool)> {
        let Some(c) = self.control.as_ref() else { return Vec::new() };
        c.constraints
            .iter()
            .map(|(path, band)| {
                let v = self.base.get_param(path).unwrap_or(0.0);
                let in_range = band.min.is_none_or(|m| v >= m) && band.max.is_none_or(|m| v <= m);
                (path.clone(), v, band.min, band.max, in_range)
            })
            .collect()
    }

    /// Current Orrery voyage + arrangement state for telemetry; `None` when the movement axis is static.
    pub fn orrery_telemetry(&self) -> Option<OrreryView> {
        self.orrery.as_ref().map(|o| {
            let voice_change_avg = if o.vc_recent.is_empty() {
                0.0
            } else {
                o.vc_recent.iter().sum::<u32>() as f32 / o.vc_recent.len() as f32
            };
            let plan = o.voyager.plan();
            OrreryView {
                chord: o.voyager.current_chord(),
                tension: o.voyager.current_tension(),
                loops_since_home: o.voyager.loops_since_home(),
                voice_change_avg,
                leader: o.last_leader.clone(),
                energy: o.last_plan.energy,
                arp_pulses: o.last_plan.arp_pulses,
                dropped: o.last_drop.clone(),
                returning_home: o.last_returned_home,
                // `None` while a narrative drop has materialized an EMPTY realized tile — the
                // pinned entry is not sounding, so the TUI must not show ♫entry beside ⊘drop.
                phrase_active: self
                    .base
                    .modulators
                    .get(&o.cfg.arp_mod)
                    .and_then(|m| m.phrase.as_ref())
                    .filter(|p| !p.realized.as_ref().is_some_and(|r| r.notes.is_empty()))
                    .and_then(|p| p.active.clone()),
                pad: plan.pad.voices.iter().map(|(_, off)| *off).collect(),
                bass: plan.bass,
                tones: plan.chord_tones,
            }
        })
    }

    /// Set/replace the session control (CE-R8.2), applied at a loop boundary with **re-base** — the
    /// operating point is preserved, never jumped (used both at session start and for runtime edits,
    /// Phase 1). The Orrery voyage is **rebuilt only when the movement axis turns on or its binding
    /// changes**; an optimize-/constraint-only edit preserves the running voyage (just refreshes the
    /// harmonic preference). `overrides` are injected once-on-change. A song block's control patch
    /// still applies over it (ADR 0055).
    pub fn set_control(&mut self, control: Option<SessionControl>) {
        self.set_live_control(control, LiveToggle::default());
    }

    /// [`set_control`](Self::set_control) from the live panel: `toggle` names the switches the user
    /// just flipped, which beat the song block's control until the next block start (ADR 0055 D3).
    pub fn set_live_control(&mut self, control: Option<SessionControl>, toggle: LiveToggle) {
        self.standing = control;
        self.live = self.live.merged(toggle);
        self.install_control(self.effective_control());
    }

    /// Make `control` the control in effect (see [`set_control`](Self::set_control)).
    fn install_control(&mut self, control: Option<SessionControl>) {
        let was_orrery = self.is_orrery();
        let prev_bind = self.control.as_ref().map(|c| (c.movement.bind.clone(), c.movement.phrase_loops));
        self.control = control;
        self.drift_streak = None;
        let new_bind = self.control.as_ref().map(|c| (c.movement.bind.clone(), c.movement.phrase_loops));

        if !self.is_orrery() {
            // Orrery just turned off → revert its full footprint (all bound note sequences, euclid
            // generators, pitch-walk order, LFO wave) to the authored home, so the next loop plays the
            // **original non-orreried** material (WS5.2 + ADR 0017). A no-op when it was never on.
            if was_orrery {
                self.restore_home();
            }
            self.orrery = None;
        } else if !was_orrery || prev_bind != new_bind || self.orrery.is_none() {
            // movement turned on, or its binding changed → (re)build the voyage from home.
            self.start_voyage();
        } else {
            // voyage already running with the same binding → preserve it; refresh the preference.
            let pref = self.harmonic_pref();
            if let Some(orr) = self.orrery.as_mut() {
                orr.pref = pref;
            }
        }
        self.apply_overrides();
    }

    /// Inject the control's `overrides` overlay as **once-on-change set-points**: each newly-added or
    /// changed JSONPath→value is written into the operating point (numeric via native units; bool/
    /// string via the raw value). The controller may then move it — to *pin* a value, use a
    /// `constraints` band with `min==max` or `dither_disable`.
    fn apply_overrides(&mut self) {
        let all: Vec<(String, DocValue)> = self
            .control
            .as_ref()
            .map(|c| c.overrides.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        for (path, val) in all {
            if self.applied_overrides.get(&path) == Some(&val) {
                continue; // unchanged since last applied
            }
            self.base = match val.as_f64() {
                Some(n) => self.base.with_param(&path, n as f32),
                None => self.base.with_value(&path, val.clone()),
            };
            // An override wins over `max_drift`: re-anchor the drift band onto the overridden value so
            // drift then moves ±max_drift *around* it rather than pulling it back (ADR 0025). Only
            // numeric candidate params carry an anchor.
            if self.anchors.contains_key(&path) {
                if let Some(n) = self.base.get_param_norm(&path) {
                    self.anchors.insert(path.clone(), n);
                }
            }
            self.applied_overrides.insert(path, val);
        }
    }

    /// Replace the authored loop doc wholesale (LiveEdit author mode, ADR 0024). Used for **same-id**
    /// edits (leaf values, step/voicing content, euclid pulses, LFO rates): the engine reconciles the
    /// carried `PhaseSnapshot` by id, so DSP tails carry across the seam and the swap is seamless.
    /// Structural changes (add/remove/retype an instrument or modulator, or a loop-length change) are
    /// handled by the session's standby-engine crossfade instead (see [`doc_swap_is_structural`]).
    ///
    /// Clamps authored values into their Tier-2 safe range (mirrors [`Controller::new`]), recomputes
    /// drift candidates (so the new doc's params move if the user switches back to drift),
    /// re-captures the Orrery "home", and re-applies the standing `overrides` overlay.
    pub fn swap_doc(&mut self, mut new_doc: LoopDoc) {
        new_doc.song = None; // see `new`
        strip_control(&mut new_doc);
        let candidates = dither_candidates_with(&new_doc, self.cfg.include_midi);
        let mut base = new_doc;
        for path in &candidates {
            let v = base.get_param_norm(path).unwrap_or(0.5);
            let clamped = clamp_band(v, tier2_norm(&base, path, Some(v)));
            if (clamped - v).abs() > 1e-9 {
                base = base.with_param_norm(path, clamped);
            }
        }
        self.anchors = capture_anchors(&base, &candidates);
        let old_base = std::mem::replace(&mut self.base, base);
        // The streak's index points into the old candidate list: an edit that adds or removes a
        // numeric param would continue it on a different param. Re-pick next drift loop instead.
        if self.candidates != candidates {
            self.drift_streak = None;
        }
        self.candidates = candidates;
        let old_home = std::mem::replace(&mut self.home, HomeSnapshot::capture(&self.base));
        // The Orrery keeps wandering through the edit unless it moved the key or the chord/bass.
        let running = self.orrery.take();
        self.follow_edit(&old_base, &old_home, running);
        // Force the overlay to re-inject onto the fresh base (paths/values may have changed); this
        // also re-anchors any overridden param onto its override value (see `apply_overrides`).
        self.applied_overrides.clear();
        self.apply_overrides();
    }

    /// The standing control's revision (0 without a control) — recorded per loop.
    pub fn revision(&self) -> u64 {
        self.control.as_ref().map_or(0, |c| c.revision)
    }

    /// Timbre axis; `Drift` when there is no control.
    fn optimize(&self) -> OptimizeMode {
        self.control.as_ref().map(|c| c.optimize).unwrap_or(OptimizeMode::Drift)
    }

    /// Movement axis: is the generative Orrery harmonic layer active?
    fn is_orrery(&self) -> bool {
        self.control.as_ref().is_some_and(|c| c.movement.is_orrery())
    }

    /// `Hold` and `LiveEdit` freeze the autonomous layers — the categorical layer and the drift
    /// pick are skipped. (`LiveEdit` is driven by doc swaps instead.) The **Tier-3
    /// constraint attractor is NOT gated by this** — it's an explicit user guardrail enforced in
    /// LiveEdit too (only pure `Hold` leaves it frozen; see `next`, ADR 0024 amended).
    fn freezes(&self) -> bool {
        matches!(self.optimize(), OptimizeMode::Hold | OptimizeMode::LiveEdit)
    }

    /// The standing session control, if any.
    pub fn control_ref(&self) -> Option<&SessionControl> {
        self.control.as_ref()
    }

    /// Clone the standing session control — used to seed a standby controller during a structural
    /// doc-swap crossfade so the replacement keeps the same mode/constraints (ADR 0024).
    pub fn control_clone(&self) -> Option<SessionControl> {
        self.control.clone()
    }

    /// A controller for a **structurally** edited doc (live edit; see [`doc_swap_is_structural`]):
    /// fresh (drift travel undone, as a new doc deserves) with the same control — but the running
    /// Orrery voyage carries over unless the edit changed the key or restructured the chord/bass
    /// modulators (ADR 0035 amendment). `self` is being replaced: its voyage moves to the new
    /// controller (it keeps playing its operating point, without an Orrery). Only live sessions edit
    /// docs; renders never come here.
    pub fn rebuilt(&mut self, new_doc: LoopDoc) -> Controller {
        let mut fresh = Controller::new(new_doc, self.cfg);
        fresh.set_control(self.control.clone());
        if fresh.orrery.is_some() {
            fresh.follow_edit(&self.base, &self.home, self.orrery.take());
        }
        fresh
    }

    /// `δ` bound: the tighter of the dither config and any control guardrail.
    fn max_change(&self) -> f32 {
        let g = self.control.as_ref().map(|c| c.guardrails.max_change_per_loop).filter(|&m| m > 0.0);
        g.map(|m| m.min(self.cfg.max_change_per_loop)).unwrap_or(self.cfg.max_change_per_loop)
    }

    /// Cumulative drift half-width (normalized `0..1`) from the loop doc, else `DEFAULT_MAX_DRIFT`
    /// (ADR 0025). Runtime-`max_change` precedence is resolved in the binaries; `max_drift` is
    /// authored in the doc only.
    fn max_drift(&self) -> f32 {
        self.base.max_drift.unwrap_or(DEFAULT_MAX_DRIFT)
    }

    /// `[floor, ceiling]` for a path (normalized): the Tier-2 **safe range** (ADR 0010), tightened
    /// by any guardrail floor/ceiling. Replaces the old implicit `[0, 1]` default — so the
    /// controller can no longer drive e.g. `master_gain_db` to silence. The *hard* cap; the Tier-3
    /// constraint attractor clamps to this so it can always drive back in-band.
    fn bounds(&self, path: &str) -> (f32, f32) {
        let (safe_lo, safe_hi) = tier2_norm(&self.base, path, self.anchors.get(path).copied());
        let g = self.control.as_ref().map(|c| &c.guardrails);
        // Guardrails are native (as authored, from a flag, a file or the doc): normalize through the seam.
        let floor = g.and_then(|g| g.floors.get(path)).map(|&v| self.base.norm_of(path, v)).unwrap_or(safe_lo).max(safe_lo);
        let ceil = g.and_then(|g| g.ceilings.get(path)).map(|&v| self.base.norm_of(path, v)).unwrap_or(safe_hi).min(safe_hi);
        (floor, ceil)
    }

    /// [`bounds`] further tightened by the **cumulative drift band** (ADR 0025): drift may move
    /// a param only `±max_drift` from its authored anchor over the session. Precedence (per user): a
    /// param-specific objectives `constraints` band **replaces** the `±max_drift` band — honored even
    /// when *wider* (capped only by the safe `bounds`); an `override` re-anchors the band
    /// (see [`apply_overrides`]). This is what every drift pick clamps to.
    fn drift_bounds(&self, path: &str) -> (f32, f32) {
        let (mut floor, mut ceil) = self.bounds(path);
        // A param-specific constraint replaces the max_drift band entirely (even if less conservative).
        if let Some(band) = self.control.as_ref().and_then(|c| c.constraints.get(path)) {
            if let Some(lo) = band.min {
                floor = floor.max(self.base.norm_of(path, lo));
            }
            if let Some(hi) = band.max {
                ceil = ceil.min(self.base.norm_of(path, hi));
            }
            return (floor, ceil);
        }
        if let Some(&anchor) = self.anchors.get(path) {
            let d = self.max_drift() * self.drift_scale(path);
            floor = floor.max(anchor - d);
            ceil = ceil.min(anchor + d);
        }
        (floor, ceil)
    }

    /// Seeded drift pick (CE-R8.1 `drift`) → `(path, signed δ)`. Holds the same param + sign for a
    /// seeded run of `DRIFT_RUN_MIN..=DRIFT_RUN_MAX` loops (directional momentum) so the operating
    /// point *travels* (re-base accumulates the steps) instead of mean-reverting, then re-picks;
    /// the magnitude still varies per loop within the run.
    fn drift_pick(&mut self, loop_id: u64) -> (String, f32) {
        let n = self.candidates.len();
        let need_new = self.drift_streak.is_none_or(|s| s.remaining == 0 || s.idx >= n);
        if need_new {
            let idx = (hash_to_unit(hash64(self.cfg.seed, loop_id, 0, 0)) * n as f32) as usize % n;
            let sign = if hash_to_unit(hash64(self.cfg.seed, loop_id, 1, 0)) < 0.5 { -1.0 } else { 1.0 };
            let span = (DRIFT_RUN_MAX - DRIFT_RUN_MIN + 1) as f32;
            let run = (DRIFT_RUN_MIN + (hash_to_unit(hash64(self.cfg.seed, loop_id, 3, 0)) * span) as u32).min(DRIFT_RUN_MAX);
            self.drift_streak = Some(DriftStreak { idx, sign, remaining: run });
        }
        let s = self.drift_streak.as_mut().expect("streak set above");
        s.remaining = s.remaining.saturating_sub(1);
        let (idx, sign) = (s.idx, s.sign);
        let path = self.candidates[idx].clone();
        let mag = self.max_change() * self.drift_scale(&path) * (0.5 + 0.5 * hash_to_unit(hash64(self.cfg.seed, loop_id, 2, 0)));
        (path, sign * mag)
    }

    /// The caps' scale on `path`'s curve (ADR 0054 P2): 0.3 for a gain (or a route depth into one),
    /// so a step of `max_change` 0.05 is about 1 dB; 1 elsewhere.
    fn drift_scale(&self, path: &str) -> f32 {
        let curve = match self.base.route_of_depth(path) {
            Some(e) => crate::params::spec_for_path(&e.dest).and_then(|s| s.curve),
            None => crate::params::spec_for_path(path).and_then(|s| s.curve),
        };
        curve.map_or(1.0, |c| c.drift_scale())
    }

    /// Try to pick a categorical move for `loop_id` → `(path, from, to)`. `None` ⇒ this loop
    /// stays numeric (probability not hit, no candidate/alternative, or the silence guard tripped).
    /// All rolls use a hash sub-stream disjoint from the numeric picks, so enabling categorical
    /// dither never disturbs numeric determinism.
    fn categorical_pick(&self, loop_id: u64) -> Option<(String, DocValue, DocValue)> {
        if self.cfg.cat_change_prob <= 0.0 {
            return None;
        }
        if hash_to_unit(hash64(self.cfg.seed, loop_id, CAT_SALT, 0)) >= self.cfg.cat_change_prob {
            return None;
        }
        let cands = categorical_candidates(&self.base);
        if cands.is_empty() {
            return None;
        }
        let ci = (hash_to_unit(hash64(self.cfg.seed, loop_id, CAT_SALT, 1)) * cands.len() as f32) as usize % cands.len();
        let cand = &cands[ci];
        let from = self.base.get_value(&cand.path)?;
        let alts: Vec<&DocValue> = cand.options.iter().filter(|o| **o != from).collect();
        if alts.is_empty() {
            return None;
        }
        let ai = (hash_to_unit(hash64(self.cfg.seed, loop_id, CAT_SALT, 2)) * alts.len() as f32) as usize % alts.len();
        let to = alts[ai].clone();
        // No silence guard needed: categorical candidates are sound-axis timbre leaves only —
        // `enabled` is off this axis entirely (ADR 0017), so a flip can never mute an instrument.
        Some((cand.path.clone(), from, to))
    }

    /// A numeric move on `path` to the normalized value `target_n` — the shared tail of `next`, with
    /// δ reported in native units.
    fn numeric_move(&self, loop_id: u64, path: String, target_n: f32, harmonic_move: Option<HarmonicMove>) -> LoopPick {
        let doc = self.base.with_param_norm(&path, target_n).with_loop_id(loop_id);
        // δ in native units: `doc = base_native + delta` (Hz, dB, s, … as the doc writes them).
        let base_native = self.base.get_param(&path).unwrap_or(0.5);
        let delta = doc.get_param(&path).unwrap_or(base_native) - base_native;
        LoopPick { doc, path, delta, harmonic_move, categorical_move: None }
    }

    /// The unmoved operating point for `loop_id`.
    fn no_move(&self, loop_id: u64, harmonic_move: Option<HarmonicMove>) -> LoopPick {
        LoopPick { doc: self.base.clone().with_loop_id(loop_id), path: String::new(), delta: 0.0, harmonic_move, categorical_move: None }
    }

    /// Tier-3 constraint attractor (Phase 1): the most-out-of-range constrained param + the sign to
    /// move it toward its band, or `None` if every constrained param is in range. Bands are authored
    /// in native units; compared in the controller's normalized space.
    fn constraint_violation(&self) -> Option<(String, f32)> {
        let c = self.control.as_ref()?;
        let mut worst: Option<(String, f32, f32)> = None; // (path, sign, distance)
        for (path, band) in &c.constraints {
            let Some(v) = self.base.get_param_norm(path) else { continue };
            let lo = band.min.map(|m| self.base.norm_of(path, m));
            let hi = band.max.map(|m| self.base.norm_of(path, m));
            let (sign, dist) = if let Some(lo) = lo.filter(|&lo| v < lo - 1e-6) {
                (1.0, lo - v)
            } else if let Some(hi) = hi.filter(|&hi| v > hi + 1e-6) {
                (-1.0, v - hi)
            } else {
                continue;
            };
            if worst.as_ref().is_none_or(|&(_, _, d)| dist > d) {
                worst = Some((path.clone(), sign, dist));
            }
        }
        worst.map(|(p, s, _)| (p, s))
    }

    /// Decide what plays at `loop_id` (the caller renders `doc`, then [`rebase`](Self::rebase)s).
    pub fn next(&mut self, loop_id: u64) -> LoopPick {
        // Song tail (ADR 0029): nothing moves while the last notes release and the reverb rings out.
        if self.song_tail {
            return self.no_move(loop_id, None);
        }

        // Orrery slow layer first (movement axis, independent of the timbre axis): evolve the
        // operating point's harmony/melody/arrangement so this loop's drift step shares the chord.
        let harmonic_move = if self.is_orrery() { self.orrery_step() } else { None };

        // Tier-3 constraint attractor: if a constrained param sits outside its band, drive the
        // most-out-of-range one toward the band this loop (priority over drift/categorical)
        // until it is back in range. Runs in **LiveEdit** too (ADR 0024 amended) — a constraint is an
        // explicit user guardrail, so it's enforced even while hand-authoring; only pure **Hold** (a
        // deliberate full pause) leaves it frozen. Drift already includes it.
        if self.optimize() != OptimizeMode::Hold {
            if let Some((path, sign)) = self.constraint_violation() {
                // The attractor clamps to the looser safe `bounds` (not the drift band) so it can
                // always cross back into an objectives band, even from beyond `±max_drift` (ADR 0025).
                let base_n = self.base.get_param_norm(&path).unwrap_or(0.5);
                let target = clamp_band(base_n + sign * self.max_change() * self.drift_scale(&path), self.bounds(&path));
                return self.numeric_move(loop_id, path, target, harmonic_move);
            }
        }

        // Categorical layer: occasionally flip a non-numeric timbre lever (wave/material/…) this
        // loop instead of a numeric drift step. Skipped in Hold/LiveEdit, which freeze it.
        if !self.freezes() {
            if let Some((path, from, to)) = self.categorical_pick(loop_id) {
                let doc = self.base.with_value(&path, to.clone()).with_loop_id(loop_id);
                let categorical_move = Some(CategoricalMove { path: path.clone(), from: value_label(&from), to: value_label(&to) });
                return LoopPick { doc, path, delta: 0.0, harmonic_move, categorical_move };
            }
        }

        // Timbre axis only — fully decoupled from the movement (Orrery) axis above.
        let drifting = !self.candidates.is_empty() && self.optimize() == OptimizeMode::Drift;
        if !drifting {
            return self.no_move(loop_id, harmonic_move);
        }
        let (path, want_delta) = self.drift_pick(loop_id);
        // A reflecting walk inside the drift band (cumulative travel ≤ ±max_drift, ADR 0025; ADR
        // 0032): a step that would cross an edge bounces back, and the streak turns around with it.
        let base_n = self.base.get_param_norm(&path).unwrap_or(0.5);
        let (target, turn) = reflect_step(base_n, want_delta, self.drift_bounds(&path));
        if let (Some(dir), Some(streak)) = (turn, self.drift_streak.as_mut()) {
            streak.sign = dir;
        }
        self.numeric_move(loop_id, path, target, harmonic_move)
    }

    /// Re-base the operating point to the actually-played doc (CE-R8.3).
    pub fn rebase(&mut self, played: LoopDoc) {
        self.base = played;
    }
}

#[cfg(test)]
mod tests;
