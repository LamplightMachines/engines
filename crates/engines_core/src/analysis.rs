//! Structure analysis of a loop doc (ADR 0033): the **polymeter** — how long each modulator's
//! pattern runs relative to the loop, and how many loops pass before every part lines up again.
//!
//! This is the "structure" lever made visible (messy ↔ predictable): a doc whose parts all
//! re-anchor at each loop top repeats every loop; free-running sequences of odd lengths and slow
//! LFOs at unrelated rates stretch the true repeat to minutes or hours. Pure arithmetic on the doc
//! (no render), so the LSP can show it as an inlay hint on every keystroke.
//!
//! - A **loop-synced** step sequence restarts at each loop top: it realigns every loop, whatever
//!   its own cycle (a cycle that doesn't divide the loop is cut short or wraps — `fits_loop`).
//! - A **free** step sequence carries its phase: one pattern lasts `length · step_duration` loops,
//!   an exact fraction `a/b` in lowest terms, so it is back at its start on a loop top after `a`
//!   loops (the denominator of its cycles-per-loop `b/a`).
//! - An **LFO** always free-runs at a float rate, so it only ever *nearly* realigns: the first loop
//!   count at which its phase is within [`PHASE_TOLERANCE`] of a cycle. Fast LFOs (above
//!   [`STRUCTURAL_LFO_MAX_HZ`]) are tremolo/vibrato texture, not form, and don't count.
//!
//! The doc realigns at the lcm of the exact parts, extended to the first multiple where every
//! structural LFO is also back in phase (searched up to [`MAX_REALIGN_LOOPS`]). Only modulators a
//! matrix route (top level or any song layer) actually uses count toward it.

use crate::loop_doc::{LoopDoc, Modulator};
use crate::modulator::parse_step_duration;
use serde::Serialize;

/// LFO phase error (fraction of a cycle) that still counts as "back in phase".
pub const PHASE_TOLERANCE: f64 = 0.02;
/// LFOs faster than this are texture (tremolo / audio-rate AM), not structure.
pub const STRUCTURAL_LFO_MAX_HZ: f64 = 1.0;
/// Search horizon for realignment; beyond it the doc "never" repeats for practical purposes.
pub const MAX_REALIGN_LOOPS: u64 = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Lfo,
    ControlSteps,
    NoteSteps,
    Euclid,
    Phrase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Clock {
    /// Restarts at every loop top (`sync: "loop"`).
    Loop,
    /// Carries its phase across loops (`sync: "free"`, and every LFO).
    Free,
}

/// One modulator's place in the polymeter.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PartCycle {
    pub id: String,
    pub kind: PartKind,
    pub clock: Clock,
    /// One pattern cycle, in loops (`INFINITY` for a 0 Hz LFO).
    pub cycle_loops: f64,
    /// An LFO's rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_hz: Option<f32>,
    /// `cycle_loops` as a reduced fraction `(num, den)` when it is exact (step sequences with
    /// integer `step_duration`s); `None` for LFOs.
    pub exact: Option<(u64, u64)>,
    /// Loops until this part is back at its start on a loop top (1 for loop-synced parts); `None`
    /// when it doesn't happen within [`MAX_REALIGN_LOOPS`].
    pub realign_loops: Option<u64>,
    /// For a loop-synced sequence: its cycle divides the loop evenly (no cut-short pattern).
    pub fits_loop: bool,
    /// Used by a matrix route (top level or a song layer).
    pub routed: bool,
    /// Counts toward the doc's realignment (routed, and not a fast LFO / static).
    pub structural: bool,
    /// Varies even when back in phase: seeded phrase selection, random pitch walk, or `prob < 1`
    /// ghost hits.
    pub stochastic: bool,
}

/// The doc's polymeter.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Polymeter {
    pub loop_seconds: f64,
    pub parts: Vec<PartCycle>,
    /// Loops until every structural part realigns; `None` beyond [`MAX_REALIGN_LOOPS`].
    pub realign_loops: Option<u64>,
    /// The realignment includes LFOs matched within [`PHASE_TOLERANCE`] (so it is "≈").
    pub approximate: bool,
}

impl Polymeter {
    pub fn realign_seconds(&self) -> Option<f64> {
        self.realign_loops.map(|n| n as f64 * self.loop_seconds)
    }
}

/// Analyse `doc`'s polymeter.
pub fn polymeter(doc: &LoopDoc) -> Polymeter {
    let loop_seconds = crate::song::loop_seconds(doc);
    let routed = routed_sources(doc);
    let parts: Vec<PartCycle> = doc
        .modulators
        .iter()
        .filter_map(|(id, m)| part(id, m, loop_seconds, routed.contains(&id.as_str())))
        .collect();

    let structural: Vec<&PartCycle> = parts.iter().filter(|p| p.structural).collect();
    let mut exact: Option<u64> = Some(1);
    for p in structural.iter().filter(|p| p.kind != PartKind::Lfo) {
        exact = match (exact, p.realign_loops) {
            (Some(a), Some(b)) => Some(lcm(a, b)).filter(|&n| n <= MAX_REALIGN_LOOPS),
            _ => None,
        };
    }
    let lfo_rates: Vec<f64> = structural.iter().filter(|p| p.kind == PartKind::Lfo).map(|p| 1.0 / p.cycle_loops).collect();
    let realign_loops = exact.and_then(|step| {
        (1..=MAX_REALIGN_LOOPS / step).map(|k| k * step).find(|&n| lfo_rates.iter().all(|&c| in_phase(c, n)))
    });
    Polymeter { loop_seconds, parts, realign_loops, approximate: !lfo_rates.is_empty() }
}

fn part(id: &str, m: &Modulator, loop_seconds: f64, routed: bool) -> Option<PartCycle> {
    if m.kind == "lfo" {
        let rate = m.rate_hz.unwrap_or(1.0) as f64;
        let cycles_per_loop = rate * loop_seconds;
        let cycle_loops = if cycles_per_loop > 0.0 { 1.0 / cycles_per_loop } else { f64::INFINITY };
        let realign = (cycles_per_loop > 0.0).then(|| (1..=MAX_REALIGN_LOOPS).find(|&n| in_phase(cycles_per_loop, n))).flatten();
        return Some(PartCycle {
            id: id.to_string(),
            kind: PartKind::Lfo,
            clock: Clock::Free,
            cycle_loops,
            rate_hz: m.rate_hz.or(Some(1.0)),
            exact: None,
            realign_loops: realign,
            fits_loop: false,
            routed,
            structural: routed && rate > 0.0 && rate <= STRUCTURAL_LFO_MAX_HZ,
            stochastic: false,
        });
    }
    if m.kind != "step_seq" {
        return None;
    }
    let kind = if m.output == "control" {
        PartKind::ControlSteps
    } else if m.phrase.is_some() {
        PartKind::Phrase
    } else if m.generator.is_some() {
        PartKind::Euclid
    } else {
        PartKind::NoteSteps
    };
    let length = m.length.max(1) as u64;
    // One cycle = length × step_duration loops; the default step is 1/length (exactly one loop).
    let exact = match &m.step_duration {
        None => Some((1, 1)),
        // An overflowing cycle (a huge numerator) falls back to the approximate branch below.
        Some(sd) => integer_fraction(sd).and_then(|(n, d)| Some(reduce(length.checked_mul(n)?, d))),
    };
    let cycle_loops = match (&exact, &m.step_duration) {
        (Some((a, b)), _) => *a as f64 / *b as f64,
        (None, Some(sd)) => length as f64 * parse_step_duration(sd).unwrap_or(1.0 / length as f32) as f64,
        (None, None) => 1.0,
    };
    let free = m.sync == "free";
    let realign = if free { exact.map(|(a, _)| a).filter(|&a| a <= MAX_REALIGN_LOOPS) } else { Some(1) };
    let fits_loop = !free && exact.is_some_and(|(a, _)| a == 1);
    let ghosted = m.steps.iter().flat_map(|s| s.voicing.values()).any(|v| v.prob < 1.0);
    let random_walk = m.pitch_walk.as_ref().is_some_and(|w| w.order == "random");
    let drawn = m.phrase.as_ref().is_some_and(|p| p.active.is_none() && p.realized.is_none() && p.select != "cycle" && p.pool.len() > 1);
    Some(PartCycle {
        id: id.to_string(),
        kind,
        clock: if free { Clock::Free } else { Clock::Loop },
        cycle_loops,
        rate_hz: None,
        exact,
        realign_loops: realign,
        fits_loop,
        routed,
        structural: routed,
        stochastic: ghosted || random_walk || drawn,
    })
}

/// Every modulator id a matrix route uses — the top level and every song layer (a layer entry
/// may be a partial patch or `null`, so read `source` loosely).
fn routed_sources(doc: &LoopDoc) -> Vec<&str> {
    let mut ids: Vec<&str> = doc.matrix.values().map(|e| e.source.as_str()).collect();
    if let Some(song) = &doc.song {
        for layer in song.matrices.values() {
            ids.extend(layer.values().filter_map(|v| v.get("source")?.as_str()));
        }
    }
    ids
}

/// `n` loops of an LFO running `cycles_per_loop` land within [`PHASE_TOLERANCE`] of a whole cycle
/// — at least one: a very slow LFO that has barely moved isn't "back in phase".
fn in_phase(cycles_per_loop: f64, n: u64) -> bool {
    let x = cycles_per_loop * n as f64;
    x.round() >= 1.0 && (x - x.round()).abs() <= PHASE_TOLERANCE
}

/// `"n/d"` with positive integer parts.
fn integer_fraction(sd: &str) -> Option<(u64, u64)> {
    let (n, d) = sd.split_once('/')?;
    let (n, d) = (n.trim().parse::<u64>().ok()?, d.trim().parse::<u64>().ok()?);
    (n > 0 && d > 0).then_some((n, d))
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn lcm(a: u64, b: u64) -> u64 {
    a / gcd(a, b) * b
}

fn reduce(n: u64, d: u64) -> (u64, u64) {
    let g = gcd(n, d);
    (n / g, d / g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 60 bpm × 4 beats ⇒ a 4 s loop.
    fn doc(modulators: serde_json::Value, matrix: serde_json::Value) -> LoopDoc {
        LoopDoc::from_value(json!({
            "schema_version": "0.9",
            "transport": { "bpm": 60, "master_steps": 4 },
            "harmony": { "root": "D", "mode": "dorian" },
            "instruments": { "pad": { "note_engine": { "polyphony": 2 }, "params": { "wave": "saw_soft", "level_db": -7.96, "cutoff_hz": 800 } } },
            "modulators": modulators,
            "matrix": matrix,
        }))
        .unwrap()
    }

    fn note_route(id: &str) -> serde_json::Value {
        json!({ "source": id, "dest": "instruments.pad.notes", "kind": "note" })
    }

    fn steps(sync: &str, length: u32, sd: Option<&str>) -> serde_json::Value {
        let mut m = json!({ "type": "step_seq", "output": "note", "sync": sync, "length": length,
                            "steps": [ { "step": 0, "voicing": { "a": { "offset": 0 } } } ] });
        if let Some(sd) = sd {
            m["step_duration"] = json!(sd);
        }
        m
    }

    #[test]
    fn loop_synced_parts_realign_every_loop() {
        let d = doc(json!({ "a": steps("loop", 16, None), "b": steps("loop", 12, Some("1/16")) }), json!({ "ma": note_route("a"), "mb": note_route("b") }));
        let p = polymeter(&d);
        assert_eq!(p.realign_loops, Some(1));
        assert!(!p.approximate);
        let b = p.parts.iter().find(|x| x.id == "b").unwrap();
        assert_eq!(b.exact, Some((3, 4)), "12 sixteenths = 3/4 loop");
        assert!(!b.fits_loop, "a 3/4-loop pattern is cut short at the loop top");
        assert!(p.parts.iter().find(|x| x.id == "a").unwrap().fits_loop);
    }

    #[test]
    fn free_sequences_realign_at_the_lcm_of_their_cycle_numerators() {
        // 23 steps of 1/24 = 23/24 loop → back on a loop top after 23 loops; 5 of 1/8 = 5/8 → 5.
        let d = doc(json!({ "a": steps("free", 23, Some("1/24")), "b": steps("free", 5, Some("1/8")) }), json!({ "ma": note_route("a"), "mb": note_route("b") }));
        let p = polymeter(&d);
        let get = |id: &str| p.parts.iter().find(|x| x.id == id).unwrap().clone();
        assert_eq!(get("a").realign_loops, Some(23));
        assert_eq!(get("b").realign_loops, Some(5));
        assert_eq!(p.realign_loops, Some(115));
        assert_eq!(p.realign_seconds(), Some(460.0));
    }

    #[test]
    fn slow_lfos_realign_approximately_and_fast_ones_dont_count() {
        // 4 s loop: 0.05 Hz = 0.2 cycles/loop (every 5 loops); 0.0625 Hz = 0.25 (every 4) → 20.
        let lfo = |hz: f64| json!({ "type": "lfo", "output": "control", "rate_hz": hz });
        let cut = |id: &str| json!({ "source": id, "dest": "instruments.pad.params.cutoff_hz", "kind": "mod", "depth_oct": 0.664 });
        let d = doc(
            json!({ "slow": lfo(0.05), "slower": lfo(0.0625), "tremolo": lfo(27.5), "unrouted": lfo(0.0123) }),
            json!({ "m1": cut("slow"), "m2": cut("slower"), "m3": cut("tremolo") }),
        );
        let p = polymeter(&d);
        assert_eq!(p.realign_loops, Some(20));
        assert!(p.approximate);
        let get = |id: &str| p.parts.iter().find(|x| x.id == id).unwrap().clone();
        assert!(!get("tremolo").structural && get("tremolo").routed);
        assert!(!get("unrouted").structural && !get("unrouted").routed);
        assert_eq!(get("slow").realign_loops, Some(5));
    }

    #[test]
    fn a_very_slow_lfo_needs_a_whole_cycle_to_realign() {
        // 0.001 Hz over a 4 s loop is 0.004 cycle/loop: back in phase only after ≈ 1 cycle (≥ 245
        // loops), not "every loop" because it has barely moved.
        let d = doc(json!({ "swell": { "type": "lfo", "output": "control", "rate_hz": 0.001 } }), json!({ "m": { "source": "swell", "dest": "instruments.pad.params.cutoff_hz", "kind": "mod", "depth_oct": 0.664 } }));
        let p = polymeter(&d);
        assert_eq!(p.parts[0].realign_loops, Some(245));
        assert_eq!(p.realign_loops, Some(245));
    }

    #[test]
    fn a_huge_step_duration_does_not_overflow() {
        let d = doc(
            json!({ "seq": { "type": "step_seq", "output": "note", "sync": "free", "length": 7, "step_duration": "18446744073709551615/7", "steps": [ { "step": 0, "voicing": { "v1": { "offset": 0 } } } ] } }),
            json!({ "m": { "source": "seq", "dest": "instruments.pad.notes", "kind": "note", "voices": 1 } }),
        );
        let p = polymeter(&d);
        assert!(p.parts[0].exact.is_none() && p.realign_loops.is_none(), "{:?}", p.parts[0]);
    }

    #[test]
    fn a_free_sequence_and_an_lfo_combine() {
        // 3/4-loop free sequence (realigns at 3) × a 0.05 Hz LFO (every 5) → 15.
        let d = doc(
            json!({ "a": steps("free", 12, Some("1/16")), "l": { "type": "lfo", "output": "control", "rate_hz": 0.05 } }),
            json!({ "ma": note_route("a"), "ml": { "source": "l", "dest": "instruments.pad.params.cutoff_hz", "kind": "mod", "depth_oct": 0.664 } }),
        );
        assert_eq!(polymeter(&d).realign_loops, Some(15));
    }

    #[test]
    fn ghost_hits_and_drawn_phrases_are_stochastic() {
        let mut ghost = steps("loop", 4, None);
        ghost["steps"][0]["voicing"]["a"]["prob"] = json!(0.5);
        let d = doc(json!({ "g": ghost, "p": steps("loop", 4, None) }), json!({ "mg": note_route("g"), "mp": note_route("p") }));
        let p = polymeter(&d);
        assert!(p.parts.iter().find(|x| x.id == "g").unwrap().stochastic);
        assert!(!p.parts.iter().find(|x| x.id == "p").unwrap().stochastic);
    }

    #[test]
    fn the_drone_fixture_has_a_long_lfo_polymeter() {
        let src = std::fs::read_to_string(crate::test_support::fixtures_dir().join("loop_drone_d_lydian.json5")).unwrap();
        let p = polymeter(&LoopDoc::from_json_str(&src).unwrap());
        assert!(p.approximate);
        let flutter = p.parts.iter().find(|x| x.id == "lfo_flutter").unwrap();
        assert!(!flutter.structural, "a 21 Hz flutter is texture");
        let n = p.realign_loops.expect("realigns within the horizon");
        assert!(n > 50, "swell / grain / glint at unrelated slow rates stretch the repeat: {n} loops");
    }
}
