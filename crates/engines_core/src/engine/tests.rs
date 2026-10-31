use super::*;

#[test]
fn hz_inverse_maps_round_trip_the_forward_maps() {
    // `hz_to_cutoff`/`hz_to_band` invert `cutoff_to_hz`/`band_to_hz` (ADR 0024 native-unit input).
    let nyq = 24_000.0;
    for &c in &[0.0_f32, 0.2, 0.5, 0.8, 1.0] {
        let back = hz_to_cutoff(cutoff_to_hz(c, nyq));
        assert!((back - c).abs() < 1e-4, "cutoff round-trip c={c} → {back}");
        let bb = hz_to_band(band_to_hz(c, nyq));
        assert!((bb - c).abs() < 1e-4, "band round-trip c={c} → {bb}");
    }
    // Out-of-curve Hz clamps to the 0..1 range (≤80 Hz → 0, way-high → 1).
    assert_eq!(hz_to_cutoff(40.0), 0.0);
    assert_eq!(hz_to_cutoff(100_000.0), 1.0);
}

use crate::test_support::fixture;

fn smoke() -> LoopDoc {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_modulation_smoke.json");
    LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn dual_fixture() -> LoopDoc {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_dual_basic.json");
    LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn modal_fixture() -> LoopDoc {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_modal_marimba.json");
    LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn binaural_fixture() -> LoopDoc {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_binaural_basic.json");
    LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// RMS of the mono downmix over `n` equal time windows.
fn windowed_rms(audio: &[f32], n: usize) -> Vec<f32> {
    let frames = audio.len() / 2;
    let w = (frames / n).max(1);
    (0..n)
        .map(|i| {
            let (lo, hi) = (i * w, ((i + 1) * w).min(frames));
            let mut acc = 0.0f64;
            for f in lo..hi {
                let m = 0.5 * (audio[2 * f] + audio[2 * f + 1]);
                acc += (m as f64).powi(2);
            }
            (acc / (hi - lo).max(1) as f64).sqrt() as f32
        })
        .collect()
}

fn rms_ratio(rms: &[f32]) -> f32 {
    let mn = rms.iter().cloned().filter(|&x| x > 1e-9).fold(f32::MAX, f32::min);
    let mx = rms.iter().cloned().fold(0.0f32, f32::max);
    if mn.is_finite() && mn > 0.0 { mx / mn } else { f32::INFINITY }
}

// Integration: a `mod` matrix entry on an instrument param must actually modulate the
// OUTPUT AUDIO (not just the effective-state JSON). amp_lfo → mono.level should swing
// loudness across the loop; removing the routing should leave loudness ~flat (proving
// the envelope is caused by the modulation).
#[test]
fn level_modulation_reaches_the_audio() {
    let r = Engine::new(16_000.0).render(&smoke(), &PhaseSnapshot::empty());
    let rms = windowed_rms(&r.audio, 8);
    assert!(rms_ratio(&rms) > 2.0, "amp_lfo → mono.level should swing loudness: {rms:?}");

    let mut d0 = smoke();
    d0.matrix.remove("m_amp"); // drop the level modulation
    let r0 = Engine::new(16_000.0).render(&d0, &PhaseSnapshot::empty());
    let rms0 = windowed_rms(&r0.audio, 8);
    assert!(rms_ratio(&rms0) < 1.3, "unmodulated loudness should be ~flat: {rms0:?}");
}

// S1-h5: per-instrument dry RMS surfaces audibility independent of the `level_db` knob — an enabled,
// full-`level_db` instrument with no audio (here: disabled) is not audible, so the TUI can flag it.
#[test]
fn per_inst_rms_flags_audible_vs_silent_tracks() {
    let doc = fixture();
    let r = Engine::new(48_000.0).render(&doc, &PhaseSnapshot::empty());
    assert!(r.per_inst_rms.values().any(|&v| v > 1e-4), "some track is audible: {:?}", r.per_inst_rms);

    // Disable every instrument (the `level_db` knobs are untouched / still full) ⇒ no dry signal.
    let mut muted = doc.clone();
    for inst in muted.instruments.values_mut() {
        inst.enabled = false;
    }
    let r2 = Engine::new(48_000.0).render(&muted, &PhaseSnapshot::empty());
    assert!(
        r2.per_inst_rms.values().all(|&v| v <= 1e-4),
        "all instruments disabled ⇒ no per-track dry signal despite full level knobs: {:?}",
        r2.per_inst_rms
    );
}

// ADSR (CE-R4.3): a slow attack makes the onset quieter than the settled portion.
#[test]
fn adsr_attack_ramps_up_in_the_audio() {
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":120,"master_steps":8,"step_unit":"beat"},
        "harmony":{"root":"A","mode":"major"},
        "global":{"master_gain_db":0,"stereo_width":0.5},
        "instruments":{"poly":{"enabled":true,"pitched":true,"note_engine":{"polyphony":1,"glide_ms":0},
            "params":{"wave":"saw_soft","level_db":-1.94,"cutoff_hz":2009.5,"attack_s":1.8,"decay_s":0,"sustain":1.0,"release_s":0}, "sends_db": { "rv": 0 }}},
        "modulators":{"seq":{"type":"step_seq","output":"note","sync":"loop","length":8,
            "steps":[{"step":0,"voicing":{"v1":{"offset":0}}}]}},
        "matrix":{"m":{"source":"seq","dest":"instruments.poly.notes","kind":"note","voices":1,"priority":1}},
        "buses": { "rv": { "return_gain_db": -Infinity, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 } } } } }
    }"#;
    let d = LoopDoc::from_json_str(json).unwrap();
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert!(r.audio.iter().all(|x| x.is_finite()));
    // attack 0.6 → 1.8 s; loop = 4 s → the first window (onset) is far quieter than a settled one.
    let rms = windowed_rms(&r.audio, 8);
    assert!(rms[0] < rms[5] * 0.6, "slow attack should ramp the onset up: {rms:?}");
}

// Regression (CE-R4.3): the amp envelope is NOTE-relative, not loop-global. A pluck
// (attack 0, short decay, sustain 0) articulated MID-loop must sound — before the fix its
// envelope was read at loop-global time `frame/sr`, so a note born at 4 s was already past
// its 0.3 s decay ⇒ env = sustain = 0 ⇒ silent everywhere.
#[test]
fn midloop_articulation_envelope_is_note_relative() {
    // bpm 120, 16 beats ⇒ 8 s loop; one note at step 8 (= 4 s in). sr 16 kHz.
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":120,"master_steps":16,"step_unit":"beat"},
        "harmony":{"root":"A","mode":"major"},
        "global":{"master_gain_db":0,"stereo_width":0.0},
        "instruments":{"pluck":{"enabled":true,"pitched":true,"note_engine":{"polyphony":1,"glide_ms":0},
            "params":{"wave":"saw_soft","level_db":-1.94,"cutoff_hz":3184.9,"attack_s":0,"decay_s":0.3,"sustain":0.0,"release_s":0.2}, "sends_db": { "rv": 0 }}},
        "modulators":{"seq":{"type":"step_seq","output":"note","sync":"loop","length":16,
            "steps":[{"step":8,"voicing":{"v1":{"offset":0}}}]}},
        "matrix":{"m":{"source":"seq","dest":"instruments.pluck.notes","kind":"note","voices":1,"priority":1}},
        "buses": { "rv": { "return_gain_db": -Infinity, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 } } } } }
    }"#;
    let d = LoopDoc::from_json_str(json).unwrap();
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert!(r.audio.iter().all(|x| x.is_finite()));
    let frames = r.num_frames;
    let rms = |lo: usize, hi: usize| -> f32 {
        let s: f64 = (lo..hi).map(|f| (r.audio[2 * f] as f64).powi(2)).sum();
        (s / (hi - lo).max(1) as f64).sqrt() as f32
    };
    let art = frames / 2; // step 8 of 16 ⇒ halfway
    let before = rms(frames / 8, art - frames / 16);
    let onset = rms(art, art + frames / 16);
    let tail = rms(frames - frames / 16, frames);
    assert!(before < 1e-4, "no note before step 8: {before}");
    assert!(onset > 1e-2, "mid-loop pluck must sound after step 8 (note-relative envelope): {onset}");
    assert!(onset > tail * 5.0, "sustain:0 pluck should decay away after its onset: onset={onset} tail={tail}");
}

// Regression for the reported case: seq_swell → instruments.poly.params.level_db. The swing
// is subtle (depth 0.20 × a 0.44–0.54 source) but must be non-constant and in-band.
#[test]
fn v07_example_modulates_poly_level_across_the_loop() {
    let mut eng = Engine::new(16_000.0);
    eng.set_viz(true);
    let r = eng.render(&fixture(), &PhaseSnapshot::empty());
    let levels = eff_series(&r, "instruments.poly.params.level_db");
    assert!(levels.len() > 4, "25 Hz effective-param hops");
    let span = levels.iter().cloned().fold(f32::MIN, f32::max) - levels.iter().cloned().fold(f32::MAX, f32::min);
    assert!(span > 1e-4, "seq_swell → poly.level_db must vary across the loop: {levels:?}");
    // Normalized dB (ADR 0054 P2): the −5.19 dB base plus up to the swell's +2.69 dB × its source.
    let (base, top) = (crate::params::GAIN.inverse(-5.19), crate::params::GAIN.inverse(-5.19 + 2.69));
    assert!(levels.iter().all(|&v| (base - 1e-4..=top + 1e-4).contains(&v)), "modulated poly.level_db band: {levels:?}");
}

// ── Pitched noise (ADR 0046) ─────────────────────────────────────────────
/// One `filtered_noise` voice over the whole source band (80 Hz – 4 kHz) holding `offset` semitones
/// above A4 (0 ⇒ 440 Hz, 12 ⇒ A5), with `pitched` as written (`""` leaves it out), at `resonance`.
fn noise_doc(pitched: &str, offset: i32, resonance: f32) -> LoopDoc {
    let json = format!(
        r#"{{
            "schema_version":"0.9","loop_id":1,
            "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
            "harmony":{{"root":"A","mode":"major"}},
            "global":{{"master_gain_db":0,"stereo_width":0.0}},
            "instruments":{{"hiss":{{"enabled":true,{pitched}"note_engine":{{"polyphony":1,"glide_ms":0}},
                "params":{{"source":"filtered_noise","band_lo_hz":80,"band_hi_hz":4000,"resonance":{resonance},"level_db":-1.94,"attack_s":0,"decay_s":0,"sustain":1.0,"release_s":0}}}}}},
            "modulators":{{"seq":{{"type":"step_seq","output":"note","sync":"loop","length":8,
                "steps":[{{"step":0,"voicing":{{"v1":{{"offset":{offset}}}}}}}]}}}},
            "matrix":{{"m":{{"source":"seq","dest":"instruments.hiss.notes","kind":"note","voices":1,"priority":1}}}}
        }}"#
    );
    LoopDoc::from_json_str(&json).unwrap()
}

/// The mono mix's spectral power within `±width` Hz of each of `centres` (after 15% settling).
fn band_powers(audio: &[f32], sr: f32, centres: &[f32], width: f32) -> Vec<f64> {
    spectrum_powers(audio, |f| 0.5 * (audio[2 * f] + audio[2 * f + 1]), sr, centres, width)
}

/// [`band_powers`] of one channel (`ch` 0 = left, 1 = right).
fn channel_band_powers(audio: &[f32], ch: usize, sr: f32, centres: &[f32], width: f32) -> Vec<f64> {
    spectrum_powers(audio, |f| audio[2 * f + ch], sr, centres, width)
}

/// Spectral power of `sample(frame)` within `±width` Hz of each of `centres` (after 15% settling).
fn spectrum_powers(audio: &[f32], sample: impl Fn(usize) -> f32, sr: f32, centres: &[f32], width: f32) -> Vec<f64> {
    use realfft::RealFftPlanner;
    let frames = audio.len() / 2;
    let start = frames / 7;
    let n = (frames - start) & !1;
    let mut x: Vec<f32> = (start..start + n).map(sample).collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut spec = fft.make_output_vec();
    fft.process(&mut x, &mut spec).unwrap();
    let bin = sr as f64 / n as f64;
    centres.iter().map(|&c| spec.iter().enumerate().filter(|(k, _)| ((*k as f64 * bin) - c as f64).abs() <= width as f64).map(|(_, z)| z.norm_sqr() as f64).sum()).collect()
}

// A pitched noise voice's band sits on its note and moves with it: A4's noise is loudest around
// 440 Hz (an octave either side is far down), A5's around 880 Hz.
#[test]
fn pitched_noise_puts_its_band_on_the_note() {
    let sr = 16_000.0;
    let a4 = Engine::new(sr as f64).render(&noise_doc(PITCHED, 0, 0.0), &PhaseSnapshot::empty());
    let p = band_powers(&a4.audio, sr, &[220.0, 440.0, 880.0], 30.0);
    assert!(p[1] > 10.0 * p[0] && p[1] > 10.0 * p[2], "A4: 220 / 440 / 880 Hz = {p:?}");
    let a5 = Engine::new(sr as f64).render(&noise_doc(PITCHED, 12, 0.0), &PhaseSnapshot::empty());
    let q = band_powers(&a5.audio, sr, &[440.0, 880.0, 1760.0], 60.0);
    assert!(q[1] > 10.0 * q[0] && q[1] > 10.0 * q[2], "A5: 440 / 880 / 1760 Hz = {q:?}");
}

const PITCHED: &str = r#""pitched":true,"#;

// Unpitched noise — `pitched: false` or left out — ignores the note (bit for bit): the path every
// existing noise voice takes.
#[test]
fn unpitched_noise_ignores_the_note() {
    let a4 = Engine::new(16_000.0).render(&noise_doc(r#""pitched":false,"#, 0, 0.0), &PhaseSnapshot::empty());
    let a5 = Engine::new(16_000.0).render(&noise_doc(r#""pitched":false,"#, 12, 0.9), &PhaseSnapshot::empty());
    assert_eq!(a4.audio, a5.audio, "no note band, and resonance does nothing");
    let unset = Engine::new(16_000.0).render(&noise_doc("", 12, 0.9), &PhaseSnapshot::empty());
    assert_eq!(unset.audio, a4.audio, "pitched noise is opt-in: leaving `pitched` out is unpitched");
}

// Resonance gathers the noise toward the note: more of it within a quarter tone of 440 Hz.
#[test]
fn resonance_on_pitched_noise_gathers_it_on_the_note() {
    let sr = 16_000.0;
    let share = |res: f32| {
        let r = Engine::new(sr as f64).render(&noise_doc(PITCHED, 0, res), &PhaseSnapshot::empty());
        let p = band_powers(&r.audio, sr, &[440.0, 2000.0], 6.5); // ±¼ tone at 440; the 2 kHz bin is a far reference
        let all = band_powers(&r.audio, sr, &[4000.0], 4000.0)[0];
        p[0] / all
    };
    let (flat, res) = (share(0.0), share(1.0));
    assert!(res > 4.0 * flat, "the note's share of the noise: {flat:.3} → {res:.3} at resonance 1");
}

/// A `pitched` noise voice in D holding `offset` semitones from D4 through a `lo`…4000 Hz source
/// band, at resonance 0.8, no drive and −10 dB: the D2 thump that re-anchoring the band at 20 Hz fixed (ADR 0046's amendment).
fn low_noise_doc(offset: i32, lo: f32) -> LoopDoc {
    let json = format!(
        r#"{{
            "schema_version":"0.9","loop_id":1,
            "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
            "harmony":{{"root":"D","mode":"major"}},
            "global":{{"master_gain_db":0,"stereo_width":0.0}},
            "instruments":{{"heart":{{"enabled":true,"pitched":true,"note_engine":{{"polyphony":1,"glide_ms":0}},
                "params":{{"source":"filtered_noise","band_lo_hz":{lo},"band_hi_hz":4000,"resonance":0.8,"drive_db":0,"level_db":-10,"attack_s":0,"decay_s":0,"sustain":1.0,"release_s":0}}}}}},
            "modulators":{{"seq":{{"type":"step_seq","output":"note","sync":"loop","length":8,
                "steps":[{{"step":0,"voicing":{{"v1":{{"offset":{offset}}}}}}}]}}}},
            "matrix":{{"m":{{"source":"seq","dest":"instruments.heart.notes","kind":"note","voices":1,"priority":1}}}}
        }}"#
    );
    LoopDoc::from_json_str(&json).unwrap()
}

// Pitched noise reaches low notes once the band opens under them: through a 20 Hz – 4 kHz
// band, D2 and B0 are within a few dB of D3 and their strongest frequency is the note's. Under the
// old 80 Hz floor B0 found almost no noise.
#[test]
fn pitched_noise_sits_a_low_note_in_a_band_opened_below_it() {
    let sr = 16_000.0;
    let render = |offset: i32, lo: f32| Engine::new(sr as f64).render(&low_noise_doc(offset, lo), &PhaseSnapshot::empty()).audio;
    let power = |a: &[f32]| band_powers(a, sr, &[4000.0], 4000.0)[0];
    let db = |a: &[f32], b: &[f32]| 10.0 * (power(a) / power(b)).log10();
    // The strongest frequency on a quarter-tone grid from 25 Hz to 1 kHz, each point the mean power
    // density within ±1.5% of it (noise is ragged bin by bin).
    let strongest = |a: &[f32]| {
        let grid: Vec<f32> = (0..128).map(|i| 25.0 * 2f32.powf(i as f32 / 24.0)).filter(|&f| f < 1000.0).collect();
        let density: Vec<f64> = grid.iter().map(|&f| band_powers(a, sr, &[f], 0.015 * f)[0] / f as f64).collect();
        grid[density.iter().enumerate().max_by(|x, y| x.1.total_cmp(y.1)).unwrap().0]
    };
    let d3 = render(-12, 20.0);
    assert!((strongest(&d3) / 146.83 - 1.0).abs() < 0.04, "D3's strongest frequency: {:.1} Hz", strongest(&d3));
    for (name, offset, hz) in [("D2", -24, 73.42), ("B0", -39, 30.87)] {
        let low = render(offset, 20.0);
        assert!(db(&low, &d3).abs() < 3.0, "{name} vs D3 through 20–4000 Hz: {:+.2} dB", db(&low, &d3));
        let f = strongest(&low);
        assert!((f / hz - 1.0).abs() < 0.04, "{name}'s strongest frequency: {f:.1} Hz");
    }
    let (b0, b0_floor) = (render(-39, 20.0), render(-39, 80.0));
    assert!(db(&b0, &b0_floor) > 10.0, "the old 80 Hz floor starved B0: {:+.2} dB", db(&b0, &b0_floor));
}

/// One instrument (`params`) fed by note routes `a` (A4) and/or `b` (A5) — each authors its voice
/// as `v1`, so the voice ids collide across the two sources.
fn two_source_doc(params: &str, a: bool, b: bool) -> LoopDoc {
    let route = |id: &str, on: bool| if on { format!(r#","m_{id}":{{"source":"{id}","dest":"instruments.x.notes","kind":"note","voices":1}}"#) } else { String::new() };
    let json = format!(
        r#"{{
            "schema_version":"0.9","loop_id":1,
            "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
            "harmony":{{"root":"A","mode":"major"}},
            "global":{{"master_gain_db":0,"stereo_width":0.0}},
            "instruments":{{"x":{{"enabled":true,"pitched":true,"note_engine":{{"polyphony":2,"glide_ms":0}},"params":{{{params}}}}}}},
            "modulators":{{
                "a":{{"type":"step_seq","output":"note","sync":"loop","length":8,"steps":[{{"step":0,"voicing":{{"v1":{{"offset":0}}}}}}]}},
                "b":{{"type":"step_seq","output":"note","sync":"loop","length":8,"steps":[{{"step":0,"voicing":{{"v1":{{"offset":12}}}}}}]}}}},
            "matrix":{{"m_x":{{"source":"a","dest":"instruments.x.params.level_db","kind":"mod","depth_db":0.0}}{}{}}}
        }}"#,
        route("a", a),
        route("b", b)
    );
    LoopDoc::from_json_str(&json).unwrap()
}

// Two note routes into one instrument that both author `v1` keep separate voice state (filters,
// pitched-noise bands): playing both is the sum of playing each — a shared key would advance one
// filter twice per frame and break that. (Everything past the voices is linear here.)
#[test]
fn voices_from_two_sources_with_the_same_id_keep_their_own_dsp_state() {
    for params in [
        r#""wave":"saw_soft","cutoff_hz":800,"resonance":0.5,"level_db":-7.96,"attack_s":0,"decay_s":0,"sustain":1.0,"release_s":0"#,
        r#""source":"filtered_noise","band_lo_hz":80,"band_hi_hz":4000,"resonance":0.7,"level_db":-7.96,"attack_s":0,"decay_s":0,"sustain":1.0,"release_s":0"#,
    ] {
        let render = |a, b| Engine::new(16_000.0).render(&two_source_doc(params, a, b), &PhaseSnapshot::empty()).audio;
        let (both, a, b) = (render(true, true), render(true, false), render(false, true));
        let worst = both.iter().zip(a.iter().zip(&b)).map(|(&s, (&x, &y))| (s - (x + y)).abs()).fold(0.0f32, f32::max);
        let peak = both.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak > 1e-3 && worst < 1e-4 * peak.max(1.0), "{params}: both ≠ a + b (worst {worst:.2e}, peak {peak:.3})");
    }
}

// ── Octave-pedal `shift_st` / `shift_mix` ──────────────────────────────────
/// A minimal one-voice sine doc (A4=440Hz, high cutoff, no envelope) with the given octave
/// pedal settings; optionally an LFO→shift mod at `mod_depth`.
fn shift_doc(shift: f32, shift_mix: f32, mod_depth: Option<f32>) -> LoopDoc {
    let (mods, mtx) = match mod_depth {
        Some(_) => (
            r#","shift_lfo":{"type":"lfo","output":"control","wave":"sine","sync":"free","rate_hz":0.5,"polarity":"bipolar"}"#,
            // `mod_depth` is a fraction of the ±24 st range, written as semitones.
            format!(r#","m_shift":{{"source":"shift_lfo","dest":"instruments.poly.params.shift_st","kind":"mod","depth_st":{},"mode":"add"}}"#, mod_depth.unwrap() * 48.0),
        ),
        None => ("", String::new()),
    };
    let json = format!(
        r#"{{
            "schema_version":"0.9","loop_id":1,
            "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
            "harmony":{{"root":"A","mode":"major"}},
            "global":{{"master_gain_db":0,"stereo_width":0.0}},
            "instruments":{{"poly":{{"enabled":true,"pitched":true,"note_engine":{{"polyphony":1,"glide_ms":0}},
                "params":{{"wave":"sine","level_db":-1.94,"cutoff_hz":8000,"attack_s":0,"decay_s":0,"sustain":1.0,"release_s":0,"shift_st":{shift},"shift_mix":{shift_mix}}}}}}},
            "modulators":{{"seq":{{"type":"step_seq","output":"note","sync":"loop","length":8,
                "steps":[{{"step":0,"voicing":{{"v1":{{"offset":0}}}}}}]}}{mods}}},
            "matrix":{{"m":{{"source":"seq","dest":"instruments.poly.notes","kind":"note","voices":1,"priority":1}}{mtx}}}
        }}"#
    );
    LoopDoc::from_json_str(&json).unwrap()
}

/// Zero-crossing count of the mono downmix, skipping the first 15% (filter settling).
fn zero_crossings(audio: &[f32]) -> usize {
    let frames = audio.len() / 2;
    let start = frames / 7;
    let mono = |f: usize| 0.5 * (audio[2 * f] + audio[2 * f + 1]);
    (start + 1..frames).filter(|&f| (mono(f - 1) < 0.0) != (mono(f) < 0.0)).count()
}

// Full-wet `shift = +12` plays an octave up ⇒ the fundamental doubles ⇒ ~2× zero crossings.
#[test]
fn shift_mix_full_wet_raises_an_octave() {
    let dry = Engine::new(16_000.0).render(&shift_doc(12.0, 0.0, None), &PhaseSnapshot::empty());
    let wet = Engine::new(16_000.0).render(&shift_doc(12.0, 1.0, None), &PhaseSnapshot::empty());
    let (zd, zw) = (zero_crossings(&dry.audio), zero_crossings(&wet.audio));
    assert!(zd > 0, "dry tone should oscillate: {zd}");
    let ratio = zw as f32 / zd as f32;
    assert!((ratio - 2.0).abs() < 0.15, "full-wet +12st should ~double the pitch: {zd} → {zw} (×{ratio:.3})");
}

// `shift_mix = 0` is the dry path: a doc with `shift_st` set but mix 0 is bit-identical to one
// with no shift fields at all (backward compat + determinism, CE-R7.3).
#[test]
fn shift_mix_zero_is_bit_identical() {
    let with_shift = Engine::new(16_000.0).render(&shift_doc(12.0, 0.0, None), &PhaseSnapshot::empty());
    let no_shift = Engine::new(16_000.0).render(&shift_doc(0.0, 0.0, None), &PhaseSnapshot::empty());
    assert_eq!(with_shift.audio, no_shift.audio, "shift_mix=0 must be the untouched dry path");
}

// An LFO → instruments.poly.params.shift_st must sweep the register: the effective `shift_st`
// (denormed to semitones) varies across the loop.
#[test]
fn matrix_modulated_shift_wobbles_pitch() {
    let mut eng = Engine::new(16_000.0);
    eng.set_viz(true);
    let r = eng.render(&shift_doc(0.0, 1.0, Some(0.3)), &PhaseSnapshot::empty());
    let dest = "instruments.poly.params.shift_st";
    let shifts: Vec<f32> = eff_series(&r, dest).into_iter().map(|n| param_denorm(dest, n)).collect();
    let span = shifts.iter().cloned().fold(f32::MIN, f32::max) - shifts.iter().cloned().fold(f32::MAX, f32::min);
    assert!(span > 1.0, "LFO→shift should sweep the register (semitones): {shifts:?}");
}

#[test]
fn snapshot_round_trips() {
    let mut s = PhaseSnapshot::empty();
    s.prev_loop_id = 413;
    s.modulator_phases.insert("lfo".into(), ModulatorPhase { lfo_phase: 1.23, ..Default::default() });
    let j = serde_json::to_string(&s).unwrap();
    assert_eq!(s, serde_json::from_str::<PhaseSnapshot>(&j).unwrap());
}

// ── Synth types (ADR 0009) ──────────────────────────────────────────────

// An absent `type` must render bit-identically to an explicit `type:"subtractive"`.
#[test]
fn default_type_is_bit_identical_to_explicit_subtractive() {
    let d0 = fixture();
    let mut d1 = fixture();
    for inst in d1.instruments.values_mut() {
        inst.kind = Some("subtractive".into());
    }
    let a = Engine::new(48_000.0).render(&d0, &PhaseSnapshot::empty());
    let b = Engine::new(48_000.0).render(&d1, &PhaseSnapshot::empty());
    assert_eq!(a.audio, b.audio, "default type ≡ explicit subtractive (byte-identical)");
}

// Per-synth-type voice state round-trips through the snapshot (CE-R7.3).
#[test]
fn snapshot_round_trips_synth_state() {
    let mut s = PhaseSnapshot::empty();
    s.active_voices.insert(
        "d.notes".into(),
        vec![ActiveVoice {
            voice_id: "v1".into(),
            synth_state: SynthVoiceState::Dual { phase_b: 1.0, fb1: 0.2, fb2: -0.1 },
            ..Default::default()
        }],
    );
    s.active_voices.insert(
        "m.notes".into(),
        vec![ActiveVoice {
            voice_id: "x".into(),
            synth_state: SynthVoiceState::Modal { y1: vec![0.1, 0.2, -0.3], y2: vec![0.0, -0.1, 0.05] },
            ..Default::default()
        }],
    );
    let j = serde_json::to_string(&s).unwrap();
    assert_eq!(s, serde_json::from_str::<PhaseSnapshot>(&j).unwrap());
}

// A subtractive voice serializes with NO `synth_state` key (legacy snapshot bytes unchanged).
#[test]
fn subtractive_voice_omits_synth_state_in_json() {
    let v = ActiveVoice { voice_id: "v".into(), ..Default::default() };
    let j = serde_json::to_string(&v).unwrap();
    assert!(!j.contains("synth_state"), "subtractive voice must not serialize synth_state: {j}");
}

// Dual + modal fixtures parse and render finite, non-silent, un-clipped audio.
#[test]
fn synth_type_fixtures_render_finite_nonzero() {
    for d in [dual_fixture(), modal_fixture(), binaural_fixture()] {
        let r = Engine::new(48_000.0).render(&d, &PhaseSnapshot::empty());
        assert!(r.audio.iter().all(|x| x.is_finite()), "audio must be finite");
        assert!(r.audio.iter().any(|&x| x.abs() > 1e-4), "audio must be non-silent");
        let peak = r.audio.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak < 4.0, "peak {peak} unreasonable");
    }
}

// Gain-staging (ADR 0021/0031): the engine ships **no master limiter** (one would dynamically colour the
// mix that stems and the DAW master pick up), so any doc meant to be *auditioned* must be authored to stay within
// ≤ 1.0 FS on its own. This pins that contract for the **playback/showcase** fixtures: render each and
// assert the master output is finite and never hard-clips. (The feature-demonstration / correctness
// rigs — `loop_contract_v0.9`, `loop_contract_ambient`, `loop_modulation_smoke`, the single-voice
// `dual`/`modal` fixtures — deliberately stack voices at full level to exercise the renderer and are
// *not* headroom-mixed, so they're excluded; their hot peaks are tracked as a content-cleanup backlog
// item, not a limiter bug.)
//
// One loop isn't enough. Each fixture renders raw (what `engines-render` plays) for ≥ 180 s and
// ≥ 4 loops through ONE engine, the snapshot carried across the seam, and every loop must stay under
// the ceiling. That catches the two ways a fixture clips late: a feedback path that runs away after a
// few loops (the shimmer reverb above ~0.65 shimmer self-oscillates; the old shimmer showcase reached
// +8 dBFS at loop 3), and a rare peak where free-running phases line up (`loop_buses_shimmer` crossed
// 1.0 FS once in 19 loops). A fixture whose last third still peaks > 1 dB above the rest is still
// building up and fails too. The long pass runs at 24 kHz, in parallel, to stay affordable in a debug
// build; a 48 kHz render's sample peaks land up to ~0.4 dB higher (`loop_buses_shimmer`: 0.995 → 1.035
// FS), so it keeps 0.5 dB in hand under the ceiling.
//
// The first loop also renders from the controller's operating point (what `engines-play`, the TUI and
// `engines-song` play). `Controller::new` safe-clamps every candidate into its Tier-2 range (ADR 0010), so a
// gain authored under its floor could be silently raised for every live session while the raw render
// still passes. The clamped start must not be louder than the raw first loop (author the gain in range,
// or a `safe_bounds` band around it) and must stay under the fixture's ceiling: 1.0 FS, or 0.7 FS (≥ 3 dB
// of drift headroom, as for the song blocks below) for the house-style reference doc the
// `ambient-loop-doc` skill copies. That's a static clamp, not a build-up, so one loop covers it.
#[test]
fn playback_fixtures_do_not_hard_clip() {
    const SR: f64 = 24_000.0;
    const MIN_SECS: f64 = 180.0;
    const MIN_LOOPS: usize = 4;
    const RATE_MARGIN: f32 = 0.944; // −0.5 dB: the 24 kHz guard vs a 48 kHz render's peaks
    const RISE: f32 = 1.122; // +1 dB: the last third may not outgrow the rest
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/");
    let peak = |audio: &[f32]| audio.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    let check = |name: &str, ceiling: f32| -> Result<(), String> {
        let d = LoopDoc::from_json_str(&std::fs::read_to_string(format!("{dir}{name}")).unwrap()).unwrap();
        let mut eng = Engine::new(SR);
        let mut snap = PhaseSnapshot::empty();
        let (mut peaks, mut secs) = (Vec::new(), 0.0);
        while peaks.len() < MIN_LOOPS || secs < MIN_SECS {
            let r = eng.render(&d, &snap);
            if !r.audio.iter().all(|x| x.is_finite()) {
                return Err(format!("{name}: loop {} isn't finite", peaks.len() + 1));
            }
            peaks.push(peak(&r.audio));
            secs += r.num_frames as f64 / SR;
            snap = r.next_snapshot;
        }
        let (worst, at) = peaks.iter().enumerate().fold((0.0f32, 0), |m, (i, &p)| if p > m.0 { (p, i + 1) } else { m });
        if worst > ceiling * RATE_MARGIN {
            return Err(format!(
                "{name} peaks at {worst:.3} FS in loop {at} of {} (> {ceiling} FS less 0.5 dB at 24 kHz) — re-gain-stage it \
                 (no master limiter, ADR 0021); per loop: {peaks:.2?}",
                peaks.len()
            ));
        }
        let split = peaks.len() - peaks.len() / 3;
        let (early, late) = (peaks[..split].iter().fold(0.0f32, |a, &b| a.max(b)), peaks[split..].iter().fold(0.0f32, |a, &b| a.max(b)));
        if late > early * RISE {
            return Err(format!("{name} is still building up after {secs:.0} s ({early:.3} → {late:.3} FS); per loop: {peaks:.2?}"));
        }
        let raw = peaks[0];
        let ctl = crate::controller::Controller::new(d, Default::default());
        let live = peak(&Engine::new(SR).render(ctl.operating_point(), &PhaseSnapshot::empty()).audio);
        if live > raw * 1.001 {
            return Err(format!(
                "{name}: the controller's safe-clamp plays it LOUDER than it renders raw ({raw:.3} → {live:.3} FS) — an \
                 authored level under its Tier-2 floor (e.g. global.master_gain_db < -8); author it in range or add a safe_bounds band"
            ));
        }
        if live > ceiling * RATE_MARGIN {
            return Err(format!("{name} peaks at {live:.3} FS from the controller's operating point (> {ceiling}) — re-gain-stage it (no master limiter, ADR 0021)"));
        }
        Ok(())
    };
    let fixtures = [
        ("loop_ambient_showcase.json", 1.0),
        ("loop_buses_shimmer.json", 1.0),
        ("loop_shimmer_showcase.json", 1.0),
        ("loop_orrery_dorian.json", 1.0),
        ("loop_orrery_lydian.json", 1.0),
        ("loop_orrery_roles.json", 1.0),
        ("loop_orrery_sportscar_dlydian.json", 1.0),
        ("loop_phrase_pools.json", 1.0),
        ("loop_orrery_phrase.json", 1.0),
        ("loop_binaural_basic.json", 1.0),
        ("loop_drone_d_lydian.json5", 0.7), // ADR 0028 showcase in the house style (JSON5, ADR 0023)
    ];
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = fixtures.iter().map(|&(name, ceiling)| s.spawn(move || check(name, ceiling))).collect();
        runs.into_iter().filter_map(|r| r.join().expect("fixture check panicked").err()).collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The README's authoring guide shows whole docs: each must check clean (no errors, no warnings)
/// and play like the guide says — under 0.6 FS raw and as the controller starts it.
#[test]
fn readme_example_docs_check_clean_and_do_not_clip() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap();
    let docs: Vec<&str> = readme
        .split("```json5\n")
        .skip(1)
        .filter_map(|b| b.split("```").next())
        .filter(|b| b.trim_start().starts_with('{') && b.contains("schema_version"))
        .collect();
    assert!(!docs.is_empty(), "the README's minimal loop is a ```json5 block");
    for (i, src) in docs.iter().enumerate() {
        let c = crate::check::check_str(src);
        let diags: Vec<String> = c.errors().chain(c.warnings()).map(|d| d.to_string()).collect();
        assert!(diags.is_empty(), "README doc #{i} doesn't check clean:\n{}", diags.join("\n"));
        let doc = c.doc.unwrap();
        let peak = |d: &LoopDoc| Engine::new(24_000.0).render(d, &PhaseSnapshot::empty()).audio.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        let raw = peak(&doc);
        let live = peak(crate::controller::Controller::new(doc, Default::default()).operating_point());
        assert!(raw <= 0.6 && live <= 0.6, "README doc #{i} peaks at {raw:.3} FS raw / {live:.3} FS as played (> 0.6)");
    }
}

/// ADR 0029: a song is gain-staged PER BLOCK — every block of the demo song, as the controller
/// starts it (safe-clamped operating point), renders without hard-clipping and with drift headroom.
#[test]
fn song_fixture_blocks_do_not_hard_clip() {
    // (fixture, expected length range in hours)
    for (name, hours) in [("song_lydian_tide.json5", 0.95..1.05), ("song_glass_garden.json5", 0.95..1.05)] {
        song_blocks_have_headroom(name, hours);
    }
}

fn song_blocks_have_headroom(name: &str, hours: std::ops::Range<f64>) {
    let path = format!("{}/../../lab/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let doc = LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(doc.lint().iter().all(|w| !w.starts_with("song:")), "{name}: {:?}", doc.lint());
    let spec = doc.song.clone().expect("the demo is a song");
    let plan = crate::song::plan(&doc, &spec, 0x5EED).unwrap();
    assert!((hours.start * 3600.0..hours.end * 3600.0).contains(&plan.total_s), "{name}: {} long", crate::song::fmt_hms(plan.total_s));
    for i in 0..spec.sequence.len() {
        let block = crate::song::materialize(&doc, &spec, i).unwrap();
        let ctl = crate::controller::Controller::new(block, Default::default());
        let r = Engine::new(48_000.0).render(ctl.operating_point(), &PhaseSnapshot::empty());
        let peak = r.audio.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(r.audio.iter().all(|x| x.is_finite()));
        // ≥ 3 dB at the block start: over hours, drift + Orrery transients were measured up to ~3 dB
        // above a block's start-of-block peak (the fixture also caps loudness drift via safe_bounds).
        assert!(peak <= 0.7, "{name} block {i} ({}) peaks at {peak} FS — keep ≥ 3 dB of headroom per block for hours of drift", spec.label(i));
    }
}

// Determinism across the loop seam for typed synths: two engines that each render two loops
// (carrying the snapshot) must produce bit-identical second loops — proves dual `phase_b`/fb and
// modal resonator state round-trip deterministically (CE-R7.3).
#[test]
fn synth_type_determinism_across_seam() {
    for d in [dual_fixture(), modal_fixture(), binaural_fixture()] {
        let mut e1 = Engine::new(48_000.0);
        let a1 = e1.render(&d, &PhaseSnapshot::empty());
        let a2 = e1.render(&d, &a1.next_snapshot);
        let mut e2 = Engine::new(48_000.0);
        let b1 = e2.render(&d, &PhaseSnapshot::empty());
        let b2 = e2.render(&d, &b1.next_snapshot);
        assert_eq!(a1.audio, b1.audio, "loop 1 deterministic");
        assert_eq!(a2.audio, b2.audio, "loop 2 (across seam) deterministic");
    }
}

/// A minimal single-instrument dual doc; `fm_index` controls osc-B→osc-A PM depth.
/// An amplitude as the dB a doc writes (`0` ⇒ `-Infinity`).
fn db_text(amp: f32) -> String {
    if amp <= 0.0 {
        "-Infinity".into()
    } else {
        format!("{}", 20.0 * amp.log10())
    }
}

fn dual_doc(fm_index: f32, osc_b_level: f32) -> LoopDoc {
    let osc_b_level = db_text(osc_b_level);
    let json = format!(
        r#"{{
        "schema_version":"0.9","loop_id":1,
        "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
        "harmony":{{"root":"A","mode":"major"}},
        "global":{{"master_gain_db":0,"stereo_width":0.0}},
        "instruments":{{"d":{{"type":"dual","enabled":true,"pitched":true,"note_engine":{{"polyphony":1,"glide_ms":0}},
            "params":{{"osc_a_wave":"sine","osc_b_wave":"sine","osc_a_level_db":-0.92,"osc_b_level_db":{osc_b_level},
                "osc_b_ratio":2.0,"fm_index":{fm_index},"fm_feedback":0.0,
                "attack_s":0,"decay_s":0,"sustain":1.0,"release_s":0,"level_db":-1.94,"cutoff_hz":8000}}, "sends_db": {{ "rv": 0 }}}}}},
        "modulators":{{"seq":{{"type":"step_seq","output":"note","sync":"loop","length":8,
            "steps":[{{"step":0,"voicing":{{"v1":{{"offset":0}}}}}}]}}}},
        "matrix":{{"m":{{"source":"seq","dest":"instruments.d.notes","kind":"note","voices":1,"priority":1}}}},
        "buses": {{ "rv": {{ "return_gain_db": -Infinity, "effects": {{ "verb": {{ "type": "reverb", "params": {{ "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 }} }} }} }} }}
    }}"#
    );
    LoopDoc::from_json_str(&json).unwrap()
}

// higher fm_index ⇒ richer spectrum (more high-frequency content) in the rendered audio.
#[test]
fn dual_fm_index_raises_brightness_at_render() {
    let brightness = |fm_index: f32| -> f32 {
        let r = Engine::new(48_000.0).render(&dual_doc(fm_index, 0.0), &PhaseSnapshot::empty());
        let mono: Vec<f32> = r.audio.chunks(2).map(|c| c[0]).collect();
        let denom: f32 = mono.iter().map(|x| x.abs()).sum::<f32>().max(1e-9);
        let diff: f32 = mono.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        diff / denom
    };
    let lo = brightness(0.0);
    let hi = brightness(0.8);
    assert!(hi > lo * 1.3, "fm_index should add sidebands: lo={lo} hi={hi}");
}

// osc B reaches the mixer independently of FM (fm_index=0 but osc_b_level>0 changes the audio).
#[test]
fn dual_osc_b_level_reaches_mix_without_fm() {
    let a = Engine::new(48_000.0).render(&dual_doc(0.0, 0.0), &PhaseSnapshot::empty());
    let b = Engine::new(48_000.0).render(&dual_doc(0.0, 0.6), &PhaseSnapshot::empty());
    let diff: f32 = a.audio.iter().zip(&b.audio).map(|(x, y)| (x - y).abs()).sum();
    assert!(diff > 1.0, "osc B should reach the mix with no FM (diff={diff})");
}

// Modal: a single strike rings then decays away (intrinsic decay, no ADSR) within the loop.
#[test]
fn modal_strike_decays_within_loop() {
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":60,"master_steps":4,"step_unit":"beat"},
        "harmony":{"root":"A","mode":"major"},
        "global":{"master_gain_db":0,"stereo_width":0.0},
        "instruments":{"m":{"type":"modal","enabled":true,"pitched":true,"note_engine":{"polyphony":1,"glide_ms":0},
            "params":{"material":"marimba","num_modes":3,"t60_s":0.229,"brightness":0.8,"strike_pos":0.3,"level_db":-0.92}, "sends_db": { "rv": 0 }}},
        "modulators":{"seq":{"type":"step_seq","output":"note","sync":"loop","length":4,
            "steps":[{"step":0,"voicing":{"v1":{"offset":0}}}]}},
        "matrix":{"m":{"source":"seq","dest":"instruments.m.notes","kind":"note","voices":1,"priority":1}},
        "buses": { "rv": { "return_gain_db": -Infinity, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 } } } } }
    }"#;
    let d = LoopDoc::from_json_str(json).unwrap();
    let r = Engine::new(48_000.0).render(&d, &PhaseSnapshot::empty());
    assert!(r.audio.iter().all(|x| x.is_finite()));
    let rms = windowed_rms(&r.audio, 8);
    assert!(rms[0] > 1e-3, "strike must sound: {rms:?}");
    assert!(rms[7] < rms[0] * 0.4, "modal strike should decay over the loop: {rms:?}");
}

// The engines-listen blind benchmark (2026-10-24) measured ~30 % of modal strikes at exactly +6 dB with
// identical partials — two impulses a sample apart, at loop positions that repeated every loop. One
// note, one pitch, one voice id, struck 64 times a loop: every strike must open the same.
#[test]
fn every_modal_strike_in_a_loop_opens_at_the_same_level() {
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":60,"master_steps":16,"step_unit":"beat"},
        "harmony":{"root":"A","mode":"mixolydian"},
        "global":{"master_gain_db":0,"stereo_width":0.0},
        "instruments":{"m":{"type":"modal","enabled":true,"pitched":true,"note_engine":{"polyphony":1,"glide_ms":0},
            "params":{"material":"resonators","num_modes":5,"t60_s":0.229,"brightness":0.45,"strike_pos":0.22,"level_db":-6.02}}},
        "modulators":{"seq":{"type":"step_seq","output":"note","sync":"loop","length":64,"step_duration":"1/64",
            "phrase":{"pool":["tick"]}}},
        "sequence_pools":{"tick":{"fit":"literal","length":1,"notes":[{"t":0,"offset":-24,"dur":0.5}]}},
        "matrix":{"m":{"source":"seq","dest":"instruments.m.notes","kind":"note","voices":1,"priority":1}}
    }"#;
    let d = LoopDoc::from_json_str(json).unwrap();
    let r = Engine::new(48_000.0).render(&d, &PhaseSnapshot::empty());
    let step = r.num_frames / 64; // 12000: exact
    let opening = |k: usize| (k * step..k * step + 240).map(|f| r.audio[2 * f].abs()).fold(0.0f32, f32::max);
    let peaks: Vec<f32> = (0..64).map(opening).collect();
    let loud: Vec<usize> = (0..64).filter(|&k| peaks[k] > peaks[0] * 1.01 || peaks[k] < peaks[0] * 0.99).collect();
    assert!(peaks[0] > 1e-3, "the strike sounds: {}", peaks[0]);
    assert!(loud.is_empty(), "strikes {loud:?} open at {:?} vs {} at step 0", loud.iter().map(|&k| peaks[k]).collect::<Vec<_>>(), peaks[0]);
}

// The benchmark's `ninety-arp-m` candidate (2026-10-24): a loop-synced phrase arm of 12-step up/down
// arpeggios over 96 "1/96" host steps on a vibraphone. The step rounds to 5333 frames (96 × 5333 =
// 511968 of 512000), so a 9th tile started 32 frames before the seam and its notes landed on the
// next loop's first tile under the same voice ids: held vibraphone voices were re-pitched (four
// octaves down at worst) instead of struck, and the seam burst to +27 dB. `cycle` makes the 9th tile
// a falling one and loop 2 open rising (D7 → D3 on `p0`). Loop 2 must open no louder than loop 1 plays.
#[test]
fn a_loop_synced_modal_arpeggio_does_not_burst_at_the_seam() {
    let notes = |offsets: [i32; 12]| offsets.iter().enumerate().map(|(t, o)| format!(r#"{{"t":{t},"offset":{o},"dur":1}}"#)).collect::<Vec<_>>().join(",");
    let json = format!(
        r#"{{
        "schema_version":"0.9","loop_id":7,
        "transport":{{"bpm":135,"master_steps":24,"step_unit":"beat"}},
        "harmony":{{"root":"D","mode":"ionian"}},
        "global":{{"master_gain_db":-6.02,"stereo_width":0.5}},
        "instruments":{{"vib":{{"type":"modal","enabled":true,"pitched":true,"note_engine":{{"polyphony":8,"glide_ms":0}},
            "params":{{"material":"vibraphone","num_modes":3,"t60_mode":"fixed","t60_s":0.157,"brightness":0.5,"strike_pos":0.25,"level_db":-15.39}}}}}},
        "modulators":{{"arp":{{"type":"step_seq","output":"note","sync":"loop","length":96,"step_duration":"1/96",
            "phrase":{{"pool":["down","up"],"select":"cycle"}}}}}},
        "sequence_pools":{{
            "down":{{"fit":"literal","length":12,"notes":[{}]}},
            "up":{{"fit":"literal","length":12,"notes":[{}]}}}},
        "matrix":{{"m":{{"source":"arp","dest":"instruments.vib.notes","kind":"note","voices":8,"priority":1}}}}
    }}"#,
        notes([36, 31, 28, 24, 19, 16, 12, 7, 4, 0, -5, -8]),
        notes([-12, -8, -5, 0, 4, 7, 12, 16, 19, 24, 28, 31]),
    );
    let d = LoopDoc::from_json_str(&json).unwrap();
    let mut eng = Engine::new(48_000.0);
    let r1 = eng.render(&d, &PhaseSnapshot::empty());
    let r2 = eng.render(&d, &r1.next_snapshot);
    let peak = |a: &[f32]| a.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let tail = 2 * 2_400; // the last 50 ms of loop 1 belong to the seam
    let (body, seam) = (peak(&r1.audio[..r1.audio.len() - tail]), peak(&r1.audio[r1.audio.len() - tail..]).max(peak(&r2.audio)));
    assert!(body > 1e-3, "the arpeggio sounds: {body}");
    assert!(seam < body * 1.5, "the seam and loop 2 peak at {seam} vs {body} in loop 1 ({:+.1} dB)", 20.0 * (seam / body).log10());
}

/// Two-instrument doc with one `out_channel: 3` route + a reverb send (ADR 0027).
fn stem_doc() -> LoopDoc {
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":60,"master_steps":4,"step_unit":"beat"},
        "harmony":{"root":"A","mode":"dorian"},
        "global":{"master_gain_db":-1.94,"stereo_width":0.6},
        "instruments":{
            "pad":{"enabled":true,"pitched":true,"note_engine":{"polyphony":2,"glide_ms":0},
                "params":{"wave":"sine","attack_s":0.03,"decay_s":0.6,"sustain":0.9,"release_s":1.2,"level_db":-6.02,"cutoff_hz":2009.5},
                "sends_db":{"rv":-6.02}},
            "solo":{"enabled":true,"pitched":true,"out_channel":3,"note_engine":{"polyphony":1,"glide_ms":0},
                "params":{"wave":"triangle","attack_s":0.03,"decay_s":0.6,"sustain":0.9,"release_s":1.2,"level_db":-6.02,"cutoff_hz":2009.5},
                "sends_db":{"rv":-6.02}}
        },
        "modulators":{"seq":{"type":"step_seq","output":"note","sync":"loop","length":4,
            "steps":[{"step":0,"voicing":{"a":{"offset":0},"b":{"offset":7}}}]}},
        "matrix":{
            "m1":{"source":"seq","dest":"instruments.pad.notes","kind":"note","voices":2,"priority":1},
            "m2":{"source":"seq","dest":"instruments.solo.notes","kind":"note","voices":2,"priority":1}
        },
        "buses":{"rv":{"return_gain_db":-7.96,"effects":{"verb":{"type":"reverb","params":{"room":0.5,"damping":0.4,"shimmer":0.2,"width":0.6,"predelay_ms":10.0}}}}}
    }"#;
    LoopDoc::from_json_str(json).unwrap()
}

#[test]
fn stem_capture_splits_without_touching_the_master_mix() {
    // ADR 0027: the stem tap is observation-only — the master mix must be BIT-identical with
    // the tap on or off (it's what renders and the speakers get), and `rest + Σ stems` must
    // reconstruct it (the width/gain chain is linear, so the split is exact up to float
    // association). The routed instrument's dry lives ONLY in its stem; its reverb send's
    // wet stays in the rest (buses are a shared field).
    let d = stem_doc();
    let r_off = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert!(r_off.stems.is_none(), "tap off ⇒ no stems");

    let mut eng = Engine::new(16_000.0);
    eng.set_stem_capture(true);
    let r_on = eng.render(&d, &PhaseSnapshot::empty());
    assert_eq!(r_on.audio, r_off.audio, "master mix must be bit-identical with the stem tap on");

    let sm = r_on.stems.as_ref().expect("tap on + out_channel routes ⇒ stems");
    assert_eq!(sm.stems.keys().copied().collect::<Vec<_>>(), vec![3], "one stem, on pair 3/4");
    let stem = &sm.stems[&3];
    assert_eq!(stem.len(), r_on.audio.len());
    assert_eq!(sm.rest.len(), r_on.audio.len());
    let energy = |b: &[f32]| b.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>();
    assert!(energy(stem) > 1e-3, "routed instrument sounds in its stem");
    assert!(energy(&sm.rest) > 1e-3, "un-routed instrument + reverb wet sound in the rest");
    // reconstruction: rest + stem == master (linear split; float-association tolerance)
    let mut worst = 0.0f32;
    for i in 0..r_on.audio.len() {
        worst = worst.max((sm.rest[i] + stem[i] - r_on.audio[i]).abs());
    }
    assert!(worst < 1e-5, "rest + stem must reconstruct the master: worst diff {worst}");
}

/// Every kind of source the per-source tap must handle (ADR 0030): a plain instrument, one with
/// an ADR 0027 `out_channel`, an authored-disabled one, a midi translator (no audio), a reverb
/// bus, an empty-chain bus (passes its sends through × its return), and an external return
/// (never built in the core) — with an instrument and a bus sharing the id `rv`.
fn source_doc() -> LoopDoc {
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":60,"master_steps":4,"step_unit":"beat"},
        "harmony":{"root":"A","mode":"dorian"},
        "global":{"master_gain_db":-1.94,"stereo_width":0.6},
        "instruments":{
            "pad":{"enabled":true,"pitched":true,"note_engine":{"polyphony":2,"glide_ms":0},
                "params":{"wave":"sine","attack_s":0.03,"decay_s":0.6,"sustain":0.9,"release_s":1.2,"level_db":-6.02,"cutoff_hz":2009.5},
                "sends_db":{"rv":-6.02,"dly":-10.46}},
            "rv":{"enabled":true,"pitched":true,"out_channel":3,"note_engine":{"polyphony":1,"glide_ms":0},
                "params":{"wave":"triangle","attack_s":0.03,"decay_s":0.6,"sustain":0.9,"release_s":1.2,"level_db":-6.02,"cutoff_hz":2009.5},
                "sends_db":{"rv":-6.02}},
            "ghost":{"enabled":false,"pitched":true,"note_engine":{"polyphony":1,"glide_ms":0},
                "params":{"wave":"saw_soft","level_db":-6.02,"cutoff_hz":2009.5}},
            "ext":{"enabled":true,"pitched":true,"type":"midi","note_engine":{"polyphony":1,"glide_ms":0},
                "params":{"cutoff":0.5},"routing":{"channel":1,"cc":{}}}
        },
        "modulators":{"seq":{"type":"step_seq","output":"note","sync":"loop","length":4,
            "steps":[{"step":0,"voicing":{"a":{"offset":0},"b":{"offset":7}}}]}},
        "matrix":{
            "m1":{"source":"seq","dest":"instruments.pad.notes","kind":"note","voices":2,"priority":1},
            "m2":{"source":"seq","dest":"instruments.rv.notes","kind":"note","voices":2,"priority":1},
            "m3":{"source":"seq","dest":"instruments.ghost.notes","kind":"note","voices":1,"priority":1},
            "m4":{"source":"seq","dest":"instruments.ext.notes","kind":"note","voices":1,"priority":1}
        },
        "buses":{
            "rv":{"return_gain_db":-7.96,"effects":{"verb":{"type":"reverb","params":{"room":0.5,"damping":0.4,"shimmer":0.2,"width":0.6,"predelay_ms":10.0}}}},
            "dly":{"return_gain_db":-6.02},
            "daw":{"type":"external_return","return_gain_db":-1.94,"latency_ms":120}
        }
    }"#;
    LoopDoc::from_json_str(json).unwrap()
}

#[test]
fn source_layout_lists_audio_instruments_then_internal_buses() {
    let keys: Vec<String> = source_layout(&source_doc()).iter().map(|k| k.to_string()).collect();
    // ghost (authored-disabled) is IN — an objectives override can enable it; ext (midi) and
    // daw (external_return) are out; same-id instrument/bus `rv` stay distinct.
    assert_eq!(keys, ["inst:ghost", "inst:pad", "inst:rv", "bus:dly", "bus:rv"]);
}

#[test]
fn source_capture_splits_without_touching_the_master_mix() {
    // ADR 0030: observation-only like the ADR 0027 stem tap — the master is BIT-identical with the
    // tap on, and Σ instrument dry + Σ bus wet (each through the master's width/gain) rebuilds it.
    let d = source_doc();
    let r_off = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert!(r_off.sources.is_none(), "tap off ⇒ no sources");

    let mut eng = Engine::new(16_000.0);
    eng.set_source_capture(true);
    let r = eng.render(&d, &PhaseSnapshot::empty());
    assert_eq!(r.audio, r_off.audio, "master mix must be bit-identical with the source tap on");
    let src = r.sources.as_ref().expect("tap on ⇒ sources");
    assert_eq!(src.inst.keys().map(String::as_str).collect::<Vec<_>>(), ["pad", "rv"], "only instruments this render played");
    assert_eq!(src.bus.keys().map(String::as_str).collect::<Vec<_>>(), ["dly", "rv"], "internal buses only");
    let energy = |b: &[f32]| b.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>();
    for (k, b) in src.inst.iter().chain(src.bus.iter()) {
        assert_eq!(b.len(), r.audio.len(), "{k}");
        assert!(energy(b) > 1e-4, "{k} carries signal");
    }
    assert_eq!(src.get(&SourceKey::Inst("ghost".into())), None);
    assert!(src.get(&SourceKey::Bus("rv".into())).is_some() && src.get(&SourceKey::Inst("rv".into())).is_some());
    let mut worst = 0.0f32;
    for i in 0..r.audio.len() {
        let sum: f32 = src.inst.values().chain(src.bus.values()).map(|b| b[i]).sum();
        worst = worst.max((sum - r.audio[i]).abs());
    }
    assert!(worst < 1e-5, "Σ sources must reconstruct the master: worst diff {worst}");
}

#[test]
fn source_and_stem_taps_are_independent() {
    let d = source_doc();
    let run = |stems: bool, sources: bool| {
        let mut e = Engine::new(16_000.0);
        e.set_stem_capture(stems);
        e.set_source_capture(sources);
        e.render(&d, &PhaseSnapshot::empty())
    };
    let (both, stems_only, sources_only) = (run(true, true), run(true, false), run(false, true));
    assert_eq!(both.audio, stems_only.audio);
    assert_eq!(both.stems.as_ref().unwrap().stems, stems_only.stems.as_ref().unwrap().stems);
    assert_eq!(both.stems.as_ref().unwrap().rest, stems_only.stems.as_ref().unwrap().rest);
    assert_eq!(both.sources.as_ref().unwrap().inst, sources_only.sources.as_ref().unwrap().inst);
    assert_eq!(both.sources.as_ref().unwrap().bus, sources_only.sources.as_ref().unwrap().bus);
}

#[test]
fn new_strikes_survive_polyphony_pressure_from_carried_voices() {
    // Regression (the "muted bell", field report 2026-07-04): an arp's anonymous voices are
    // never released — they sustain across loops as silent (rung-down) squatters filling the
    // polyphony. When the arp pattern then CHANGES between loops (a live-edit step_duration
    // tweak, an Orrery pulses rewrite), the new strikes land at different frames ⇒ different
    // synthetic ids ⇒ no id collision with the squatters — and the old positional steal
    // (`voices.remove(0)`) killed each newborn strike in favor of the squatters, leaving the
    // instrument PERMANENTLY silent. The steal must evict the oldest squatter instead.
    let doc = |pulses: u32| {
        let json = format!(
            r#"{{
            "schema_version":"0.9","loop_id":1,
            "transport":{{"bpm":56,"master_steps":16,"step_unit":"beat"}},
            "harmony":{{"root":"D","mode":"dorian"}},
            "global":{{"master_gain_db":0,"stereo_width":0.0}},
            "instruments":{{"bell":{{"type":"modal","enabled":true,"pitched":true,"note_engine":{{"polyphony":2,"glide_ms":0}},
                "params":{{"material":"tuning_fork","num_modes":3,"ring_ratio":1,"t60_mode":"note","brightness":0.86,"strike_pos":0.24,"level_db":-0.92}}, "sends_db": {{ "rv": 0 }}}}}},
            "modulators":{{"arp":{{"type":"step_seq","output":"note","sync":"loop","length":16,"step_duration":"1/8",
                "generator":{{"kind":"euclid","pulses":{pulses},"rotation":1}},
                "pitch_walk":{{"pool":[12,15,7,19],"order":"random"}}}}}},
            "matrix":{{"m":{{"source":"arp","dest":"instruments.bell.notes","kind":"note","voices":3,"priority":1}}}},
            "buses": {{ "rv": {{ "return_gain_db": -Infinity, "effects": {{ "verb": {{ "type": "reverb", "params": {{ "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 }} }} }} }} }}
        }}"#
        );
        LoopDoc::from_json_str(&json).unwrap()
    };
    let mut eng = Engine::new(48_000.0);
    // Loop 1: pulses=4 — fills the 2-voice polyphony with anonymous strikes that then
    // sustain (un-released) into the next loop's snapshot.
    let r1 = eng.render(&doc(4), &PhaseSnapshot::empty());
    let rms1 = r1.per_inst_rms.get("bell").copied().unwrap_or(0.0);
    assert!(rms1 > 1e-3, "loop 1 strikes sound: {rms1}");
    // Loop 2: the pattern changes (pulses=5 ⇒ different strike frames/ids). The new strikes
    // must survive the steal against the carried squatters and be audible.
    let r2 = eng.render(&doc(5), &r1.next_snapshot);
    let rms2 = r2.per_inst_rms.get("bell").copied().unwrap_or(0.0);
    assert!(rms2 > 1e-3, "loop 2 strikes must survive carried-voice polyphony pressure: {rms2}");
}

#[test]
fn modal_t60_mode_note_couples_ring_to_step_duration() {
    // `t60_mode: "note"` derives the ring from the driving source's step. A longer step_duration
    // ⇒ longer ring ⇒ more late-window energy, even with the SAME t60_scale (ADR 0009).
    let doc = |step: &str| {
        let json = format!(
            r#"{{
            "schema_version":"0.9","loop_id":1,
            "transport":{{"bpm":60,"master_steps":8,"step_unit":"beat"}},
            "harmony":{{"root":"A","mode":"major"}},
            "global":{{"master_gain_db":0,"stereo_width":0.0}},
            "instruments":{{"m":{{"type":"modal","enabled":true,"pitched":true,"note_engine":{{"polyphony":1,"glide_ms":0}},
                "params":{{"material":"tuning_fork","num_modes":3,"ring_ratio":1,"t60_mode":"note","brightness":0.86,"strike_pos":0.24,"level_db":-0.92}}}}}},
            "modulators":{{"seq":{{"type":"step_seq","output":"note","sync":"loop","length":8,"step_duration":"{step}",
                "steps":[{{"step":0,"voicing":{{"v1":{{"offset":0}}}}}}]}}}},
            "matrix":{{"m":{{"source":"seq","dest":"instruments.m.notes","kind":"note","voices":1,"priority":1}}}}
        }}"#
        );
        LoopDoc::from_json_str(&json).unwrap()
    };
    // total energy in the 0.5–1.0 s window after the (single) strike at loop start.
    let late = |step: &str| -> f64 {
        let r = Engine::new(48_000.0).render(&doc(step), &PhaseSnapshot::empty());
        let (lo, hi) = (24_000usize, 48_000usize);
        (lo..hi).map(|f| (r.audio[2 * f] as f64).powi(2)).sum()
    };
    let short = late("1/8"); // 1/8 of an 8-beat loop = 1 beat = 1 s step
    let long = late("1/2"); // 4 s step ⇒ much longer ring
    assert!(short > 0.0 && long > short * 2.0, "longer step rings longer: short={short} long={long}");
}

#[test]
fn renders_fixture_audio_finite_nonzero() {
    let d = fixture();
    let r = Engine::new(48_000.0).render(&d, &PhaseSnapshot::empty());
    assert_eq!(r.num_frames, d.total_frames(48_000.0));
    assert_eq!(r.audio.len(), r.num_frames * 2);
    assert!(r.audio.iter().all(|x| x.is_finite()), "audio must be finite");
    assert!(r.audio.iter().any(|&x| x != 0.0), "audio must be non-zero");
    let peak = r.audio.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(peak < 100.0, "peak {peak} unreasonable");
}

#[test]
fn determinism_two_fresh_engines_bit_identical() {
    let d = fixture();
    let a = Engine::new(48_000.0).render(&d, &PhaseSnapshot::empty());
    let b = Engine::new(48_000.0).render(&d, &PhaseSnapshot::empty());
    assert_eq!(a.audio, b.audio, "fresh engines must be bit-identical (CE-R7.3)");
}

#[test]
fn second_loop_continues_and_is_finite() {
    let d = fixture();
    let mut eng = Engine::new(48_000.0);
    let r1 = eng.render(&d, &PhaseSnapshot::empty());
    let r2 = eng.render(&d, &r1.next_snapshot);
    assert!(r2.audio.iter().all(|x| x.is_finite()));
    // modulator phases advanced
    assert_ne!(
        r1.next_snapshot.modulator_phases["lfo_breath"].lfo_phase,
        r2.next_snapshot.modulator_phases["lfo_breath"].lfo_phase
    );
}

#[test]
fn effective_state_reports_modulated_dests_and_globals() {
    let r = Engine::new(48_000.0).render(&fixture(), &PhaseSnapshot::empty());
    assert!(r.effective_state.contains_key("instruments.poly.params.cutoff_hz"), "{:?}", r.effective_state.keys());
    assert!(r.effective_state.contains_key("global.master_gain_db"));
    assert!(r.effective_state.contains_key("global.stereo_width"));
}

#[test]
fn cutoff_raises_spectral_centroid_monotonically() {
    // CE-R3.1.2 monotonic param response (the legibility bar), like M2b.
    let centroid = |cutoff: f32| -> f32 {
        let mut d = fixture();
        // isolate a single saw instrument by muting others
        for (name, inst) in d.instruments.iter_mut() {
            inst.enabled = name == "poly";
        }
        d.instruments.get_mut("poly").unwrap().params.insert("cutoff_hz".into(), serde_json::json!(crate::params::CUTOFF.eval(cutoff)).into());
        // drop modulators that move poly.cutoff_hz so the sweep is clean
        d.matrix.retain(|_, e| e.dest != "instruments.poly.params.cutoff_hz");
        let r = Engine::new(48_000.0).render(&d, &PhaseSnapshot::empty());
        let mono: Vec<f32> = r.audio.chunks(2).map(|c| c[0]).collect();
        // crude high-freq energy proxy: mean abs first difference / mean abs
        let denom: f32 = mono.iter().map(|x| x.abs()).sum::<f32>().max(1e-9);
        let diff: f32 = mono.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        diff / denom
    };
    let lo = centroid(0.1);
    let mid = centroid(0.5);
    let hi = centroid(0.9);
    assert!(lo < mid && mid < hi, "cutoff should raise brightness monotonically: {lo} {mid} {hi}");
}

fn bus_doc(shimmer: f32) -> LoopDoc {
    let json = format!(
        r#"{{
        "schema_version":"0.9","loop_id":1,
        "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
        "harmony":{{"root":"A","mode":"major"}},
        "global":{{"master_gain_db":0,"stereo_width":0.5}},
        "instruments":{{"poly":{{"enabled":true,"pitched":true,"note_engine":{{"polyphony":1,"glide_ms":0}},
            "params":{{"wave":"saw_soft","level_db":-1.94,"cutoff_hz":2009.5,"attack_s":0,"decay_s":0,"sustain":1.0,"release_s":0.8}},
            "sends_db":{{"rv":0}}}}}},
        "modulators":{{"seq":{{"type":"step_seq","output":"note","sync":"loop","length":8,
            "steps":[{{"step":0,"voicing":{{"v1":{{"offset":0}}}}}}]}}}},
        "matrix":{{"m":{{"source":"seq","dest":"instruments.poly.notes","kind":"note","voices":1,"priority":1}}}},
        "buses":{{"rv":{{"return_gain_db":-1.94,"effects":{{"verb":{{"type":"reverb",
            "params":{{"room":0.7,"damping":0.3,"shimmer":{shimmer},"width":0.5,"predelay_ms":0.0}}}}}}}}}}
    }}"#
    );
    LoopDoc::from_json_str(&json).unwrap()
}

// The authored bus shimmer param must reach the rendered audio (doc → bus param plumbing).
#[test]
fn bus_shimmer_param_reaches_render() {
    let dry = Engine::new(48_000.0).render(&bus_doc(0.0), &PhaseSnapshot::empty());
    let wet = Engine::new(48_000.0).render(&bus_doc(0.8), &PhaseSnapshot::empty());
    assert!(dry.audio.iter().all(|x| x.is_finite()) && wet.audio.iter().all(|x| x.is_finite()));
    let diff: f32 = dry.audio.iter().zip(&wet.audio).map(|(a, b)| (a - b).abs()).sum();
    assert!(diff > 1.0, "shimmer should change the rendered audio (diff={diff})");
}

// A bus's reverb tail persists in the Engine across loops (CE-R4.4): after a loud loop, a
// following loop with all instruments disabled is still non-silent from the carried tail.
#[test]
fn bus_tail_crosses_loop_seam() {
    let mut eng = Engine::new(48_000.0);
    let a = eng.render(&bus_doc(0.0), &PhaseSnapshot::empty());
    // Loop B: same doc but silent (instruments disabled) → only the carried reverb tail sounds.
    let mut b_doc = bus_doc(0.0);
    for inst in b_doc.instruments.values_mut() {
        inst.enabled = false;
    }
    let b = eng.render(&b_doc, &a.next_snapshot);
    let energy: f64 = b.audio.iter().map(|x| (*x as f64).powi(2)).sum();
    assert!(energy > 1e-6, "reverb tail should cross the loop seam (energy={energy})");
}

/// `bus_doc(0.0)` with its bus `rv` edited in place (same id) — a LiveEdit-style doc edit.
fn bus_doc_edited(edit: impl FnOnce(&mut crate::loop_doc::Bus)) -> LoopDoc {
    let mut d = bus_doc(0.0);
    edit(d.buses.get_mut("rv").unwrap());
    d
}

/// Render `a` then `b` on one engine, carrying the snapshot (an in-place doc swap), and `b` on a
/// **twin** engine that played `a` with no buses at all. The twin's voices and filters are warmed
/// identically (the dry path never reads a bus) — a cold engine's filters would differ on the held
/// voice — so it is a bit-exact "never built that bus" reference: any difference between the two
/// loop-`b` renders is bus state the first engine carried. Returns `(carried, twin)`.
fn render_swap(a: &LoopDoc, b: &LoopDoc) -> (RenderResult, RenderResult) {
    let mut eng = Engine::new(48_000.0);
    let ra = eng.render(a, &PhaseSnapshot::empty());
    let mut a_dry = a.clone();
    a_dry.buses.clear();
    let mut twin = Engine::new(48_000.0);
    twin.render(&a_dry, &PhaseSnapshot::empty());
    (eng.render(b, &ra.next_snapshot), twin.render(b, &ra.next_snapshot))
}

/// Largest per-sample difference between two renders (`assert_eq!` on whole buffers is unreadable).
fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// A retired bus (ADR 0035) takes no new sends — the held note keeps sending, so if the stale chain
/// still received them the difference from a never-built-bus twin would keep sounding — and its tail
/// rings out; once it's silent the engine drops it and the two renders converge bit-exactly.
fn assert_retires_then_converges(what: &str, a: &LoopDoc, b: &LoopDoc) {
    let mut eng = Engine::new(48_000.0);
    let ra = eng.render(a, &PhaseSnapshot::empty());
    let mut a_dry = a.clone();
    a_dry.buses.clear();
    let mut twin = Engine::new(48_000.0);
    twin.render(&a_dry, &PhaseSnapshot::empty());
    let (mut snap, mut twin_snap) = (ra.next_snapshot.clone(), ra.next_snapshot);
    let carried = eng.render(b, &snap);
    let reference = twin.render(b, &twin_snap);
    let n = carried.num_frames;
    let w = n / 10;
    let rms = |from: usize, to: usize| {
        let s: f64 = (2 * from..2 * to).map(|i| ((carried.audio[i] - reference.audio[i]) as f64).powi(2)).sum();
        (s / (2 * (to - from)) as f64).sqrt()
    };
    let (head, tail) = (rms(0, w), rms(n - w, n));
    assert!(head > 1e-4, "{what}: the old bus's tail rings out instead of being cut (head rms {head})");
    assert!(tail < 0.1 * head, "{what}: the retired bus takes no new sends — the difference decays (head {head}, tail {tail})");
    snap = carried.next_snapshot;
    twin_snap = reference.next_snapshot;
    for _ in 0..5 {
        let (c, r) = (eng.render(b, &snap), twin.render(b, &twin_snap));
        if max_abs_diff(&c.audio, &r.audio) == 0.0 {
            return;
        }
        snap = c.next_snapshot;
        twin_snap = r.next_snapshot;
    }
    panic!("{what}: the retired bus was never dropped (renders never converged)");
}

// Retyping a live bus id to `external_return` (which the core never renders) retires the built
// internal bus: its tail rings out, the sends that still name the id feed nothing.
#[test]
fn retyping_a_bus_to_external_return_retires_it() {
    let b_doc = bus_doc_edited(|b| {
        b.kind = Some("external_return".into());
        b.effects.clear();
    });
    b_doc.validate().unwrap();
    assert!(b_doc.instruments["poly"].sends_db.contains_key("rv"), "the send still names the retyped bus");
    assert_retires_then_converges("retyped", &bus_doc(0.0), &b_doc);
}

// A same-id effect-chain edit (effect removed / renamed) can't be reached by params, so the engine
// builds the bus fresh and retires the stale one (no sends, tail rings out) instead of running the
// stale chain. A param-only edit keeps the built bus, so its tail still crosses the seam.
#[test]
fn editing_a_bus_effect_chain_rebuilds_the_bus() {
    let removed = bus_doc_edited(|b| b.effects.clear());
    let renamed = bus_doc_edited(|b| {
        let fx = b.effects.remove("verb").unwrap();
        b.effects.insert("hall".into(), fx);
    });
    for (what, b_doc) in [("removed", removed), ("renamed", renamed)] {
        assert_retires_then_converges(what, &bus_doc(0.0), &b_doc);
    }
    let (carried, twin) = render_swap(&bus_doc(0.0), &bus_doc(0.6));
    let d = max_abs_diff(&carried.audio, &twin.audio);
    assert!(d > 1e-3, "a param-only edit must keep the built bus and carry its tail (max diff {d})");
}


#[test]
fn viz_tap_is_byte_identical_when_off_vs_on() {
    // CE-R7.3 keystone (ADR 0024): enabling the live-viz tap must not perturb a single rendered
    // sample or any existing observation output — it is pure observation, off the audio path.
    let d = fixture();
    let snap = PhaseSnapshot::empty();
    let r_off = Engine::new(48_000.0).render(&d, &snap);
    let mut e_on = Engine::new(48_000.0);
    e_on.set_viz(true);
    e_on.set_viz_instrument(d.instruments.keys().next().cloned());
    let r_on = e_on.render(&d, &snap);
    assert_eq!(r_off.audio, r_on.audio, "viz tap changed the audio");
    assert_eq!(r_off.per_inst_rms, r_on.per_inst_rms);
    assert_eq!(r_off.effective_state, r_on.effective_state);
    assert_eq!(
        serde_json::to_value(&r_off.next_snapshot).unwrap(),
        serde_json::to_value(&r_on.next_snapshot).unwrap(),
        "viz tap changed the next snapshot (next-loop determinism)"
    );
    // The tap produced 25 Hz hops covering every frame exactly once.
    assert!(!r_on.viz_master_hops.is_empty(), "viz produced no master hops");
    let covered: usize = r_on.viz_master_hops.iter().map(|h| h.len()).sum();
    assert_eq!(covered, r_on.num_frames, "hops must cover the whole loop exactly once");
    assert_eq!(r_on.viz_inst_dry_hops.len(), r_on.viz_master_hops.len());
    assert_eq!(r_on.viz_eff_hops.len(), r_on.viz_master_hops.len());
}

#[test]
fn render_reconciles_structural_doc_change_across_snapshot() {
    // The crossfade / swap_doc paths (ADR 0024) rely on render() gracefully reconciling a carried
    // snapshot when ids change: new ids init fresh, stale ids drop — no panic, finite audio.
    let d = fixture();
    let mut e = Engine::new(48_000.0);
    let snap = e.render(&d, &PhaseSnapshot::empty()).next_snapshot;
    // Remove an instrument: its snapshot voices/filters must be dropped, not indexed into.
    let victim = d.instruments.keys().next().cloned().unwrap();
    let mut d_removed = d.clone();
    d_removed.instruments.remove(&victim);
    let r1 = e.clone().render(&d_removed, &snap);
    assert!(r1.audio.iter().all(|s| s.is_finite()));
    assert!(!r1.per_inst_rms.contains_key(&victim), "removed instrument still produced dry audio");
    // Add a brand-new instrument id: it starts fresh from the carried snapshot, no panic.
    let mut d_added = d.clone();
    let inst = d_added.instruments.values().next().cloned().unwrap();
    d_added.instruments.insert("zz_brand_new_voice".into(), inst);
    let r2 = e.render(&d_added, &snap);
    assert!(r2.audio.iter().all(|s| s.is_finite()));
}

// ── MIDI translator instruments (ADR 0026) ───────────────────────────────

/// A D-dorian doc with one `type:"midi"` instrument (`ext`, routing channel 3 ⇒ wire 2,
/// `cutoff → CC 74`) fed by a 16-step note seq built from `steps_json`. bpm 120 ×
/// 8 beats = 4 s ⇒ at 16 kHz step k fires at frame `k · 4000`. `lfo` adds a free
/// bipolar sine → cutoff mod (depth 0.3) for the CC path.
fn midi_doc(steps_json: &str, polyphony: u32, lfo: bool) -> LoopDoc {
    let (lfo_mod, lfo_mtx) = if lfo {
        (
            r#","breath":{"type":"lfo","output":"control","wave":"sine","sync":"free","rate_hz":0.25,"polarity":"bipolar"}"#,
            r#","m_cc":{"source":"breath","dest":"instruments.ext.params.cutoff","kind":"mod","depth":0.3,"mode":"add"}"#,
        )
    } else {
        ("", "")
    };
    let json = format!(
        r#"{{
        "schema_version":"0.9","loop_id":1,
        "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
        "harmony":{{"root":"D","mode":"dorian"}},
        "global":{{"master_gain_db":0,"stereo_width":0.0}},
        "instruments":{{"ext":{{"enabled":true,"pitched":true,"type":"midi",
            "note_engine":{{"polyphony":{polyphony},"glide_ms":0}},
            "params":{{"cutoff":0.5}},
            "routing":{{"channel":3,"cc":{{"cutoff":74}}}}}}}},
        "modulators":{{"chords":{{"type":"step_seq","output":"note","sync":"loop","length":16,
            "steps":[{steps_json}]}}{lfo_mod}}},
        "matrix":{{"m_n":{{"source":"chords","dest":"instruments.ext.notes","kind":"note","voices":4,"priority":1}}{lfo_mtx}}}
    }}"#
    );
    LoopDoc::from_json_str(&json).unwrap()
}

fn note_events(r: &RenderResult) -> Vec<(usize, MidiMsg)> {
    r.midi_events.iter().filter(|e| !matches!(e.msg, MidiMsg::Cc { .. })).map(|e| (e.frame, e.msg)).collect()
}
fn cc_events(r: &RenderResult) -> Vec<(usize, u8)> {
    r.midi_events
        .iter()
        .filter_map(|e| match e.msg {
            MidiMsg::Cc { cc: 74, value, .. } => Some((e.frame, value)),
            _ => None,
        })
        .collect()
}

#[test]
fn midi_articulation_emits_note_on_with_velocity_and_default() {
    // v1 authors velocity 96; v2 omits it ⇒ the instrument default (100). D root ⇒
    // offset 0 = D4 = 62 (the pitch_to_hz anchor), offset 7 = A4 = 69. Channel 3 ⇒ wire 2.
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0,"velocity":96},"v2":{"offset":7}}}"#, 4, false);
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert_eq!(
        note_events(&r),
        vec![
            (0, MidiMsg::NoteOn { ch: 2, note: 62, vel: 96 }),
            (0, MidiMsg::NoteOn { ch: 2, note: 69, vel: 100 }),
        ]
    );
    // and the initial CC sync burst fired before the notes (patch set before it sounds)
    assert_eq!(r.midi_events[0].msg, MidiMsg::Cc { ch: 2, cc: 74, value: 64 }, "{:?}", r.midi_events);
    // emitted state is carried for the next loop's diff
    assert_eq!(r.next_snapshot.midi_out["ext"].notes.len(), 2);
    assert_eq!(r.next_snapshot.midi_out["ext"].cc_last["cutoff"], 64);
}

#[test]
fn midi_sustained_voices_across_loops_emit_nothing() {
    // Loop 2 replays the same step-0 voicing; both voices are held (absent/same-pitch ⇒
    // sustain, design-rationale §2.5) and the CC is unchanged ⇒ zero events. The
    // external synth just keeps sounding — the hung-note rule working as intended.
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0,"velocity":96},"v2":{"offset":7}}}"#, 4, false);
    let mut e = Engine::new(16_000.0);
    let r1 = e.render(&d, &PhaseSnapshot::empty());
    let r2 = e.render(&d.clone().with_loop_id(2), &r1.next_snapshot);
    assert!(r2.midi_events.is_empty(), "steady drone must be silent on the wire: {:?}", r2.midi_events);
}

#[test]
fn midi_pitch_change_emits_legato_on_before_off() {
    // v1 moves 0 → 2 at step 8 (frame 32000). Plain MIDI can't glide one channel-note,
    // so the engine emits On(new) *then* Off(old) — the overlap lets the external
    // synth's portamento/legato do the slide (user decision; MPE later). Velocity
    // sticks to the articulation's (96) since step 8 authors none.
    let d = midi_doc(
        r#"{"step":0,"voicing":{"v1":{"offset":0,"velocity":96}}},{"step":8,"voicing":{"v1":{"offset":2}}}"#,
        4,
        false,
    );
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert_eq!(
        note_events(&r),
        vec![
            (0, MidiMsg::NoteOn { ch: 2, note: 62, vel: 96 }),
            (32_000, MidiMsg::NoteOn { ch: 2, note: 64, vel: 96 }),
            (32_000, MidiMsg::NoteOff { ch: 2, note: 62 }),
        ]
    );
}

#[test]
fn midi_release_emits_note_off_at_the_event_frame() {
    // release:true is the explicit note-off (§2.5). The Off goes out at the release
    // frame — no engine release tail for midi (the external synth owns its own).
    let d = midi_doc(
        r#"{"step":0,"voicing":{"v1":{"offset":0},"v2":{"offset":7}}},{"step":8,"voicing":{"v1":{"release":true}}}"#,
        4,
        false,
    );
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    let notes = note_events(&r);
    assert_eq!(notes.last(), Some(&(32_000, MidiMsg::NoteOff { ch: 2, note: 62 })));
    // v2 (69) sustains — only the released note went off.
    assert_eq!(notes.iter().filter(|(_, m)| matches!(m, MidiMsg::NoteOff { .. })).count(), 1);
    assert_eq!(r.next_snapshot.midi_out["ext"].notes.len(), 1);
}

#[test]
fn midi_voice_steal_emits_note_off_for_the_stolen_voice() {
    // Two note sources on a polyphony-1 midi instrument: `pad` (priority 2) holds v1;
    // `lead` (priority 1) articulates w1 at step 8 ⇒ enforce_polyphony steals pad's v1.
    // Steals produce no voice Action — only the set diff can emit the Off (the core
    // hung-note case, ADR 0026).
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":120,"master_steps":8,"step_unit":"beat"},
        "harmony":{"root":"D","mode":"dorian"},
        "global":{"master_gain_db":0,"stereo_width":0.0},
        "instruments":{"ext":{"enabled":true,"pitched":true,"type":"midi",
            "note_engine":{"polyphony":1,"glide_ms":0},
            "params":{"cutoff":0.5},
            "routing":{"channel":1,"cc":{}}}},
        "modulators":{
            "pad":{"type":"step_seq","output":"note","sync":"loop","length":16,
                "steps":[{"step":0,"voicing":{"v1":{"offset":0}}}]},
            "lead":{"type":"step_seq","output":"note","sync":"loop","length":16,
                "steps":[{"step":8,"voicing":{"w1":{"offset":5}}}]}},
        "matrix":{
            "m_pad":{"source":"pad","dest":"instruments.ext.notes","kind":"note","voices":4,"priority":2},
            "m_lead":{"source":"lead","dest":"instruments.ext.notes","kind":"note","voices":4,"priority":1}},
        "buses": { "rv": { "return_gain_db": -Infinity, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 } } } } }
    }"#;
    let d = LoopDoc::from_json_str(json).unwrap();
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert_eq!(
        note_events(&r),
        vec![
            (0, MidiMsg::NoteOn { ch: 0, note: 62, vel: 100 }),
            (32_000, MidiMsg::NoteOn { ch: 0, note: 67, vel: 100 }),
            (32_000, MidiMsg::NoteOff { ch: 0, note: 62 }),
        ],
        "the stolen pad voice must go off even though stealing emits no Action"
    );
}

#[test]
fn midi_disable_flushes_offs_and_reenable_resounds() {
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0,"velocity":96}}}"#, 4, false);
    let mut e = Engine::new(16_000.0);
    let r1 = e.render(&d, &PhaseSnapshot::empty());

    // Loop 2: instrument disabled ⇒ loop-start flush (no hung note), and no CC either.
    let mut d_off = d.clone().with_loop_id(2);
    d_off.instruments.get_mut("ext").unwrap().enabled = false;
    let r2 = e.render(&d_off, &r1.next_snapshot);
    assert_eq!(r2.midi_events, vec![MidiEvent { frame: 0, msg: MidiMsg::NoteOff { ch: 2, note: 62 } }]);

    // Loop 3: re-enabled ⇒ the carried (still-held) voice re-sounds at loop start.
    // Velocity falls back to the default — the emitted state was flushed on disable.
    let r3 = e.render(&d.clone().with_loop_id(3), &r2.next_snapshot);
    assert_eq!(
        note_events(&r3).first(),
        Some(&(0, MidiMsg::NoteOn { ch: 2, note: 62, vel: 100 })),
        "{:?}",
        r3.midi_events
    );
}

#[test]
fn midi_removed_instrument_flushes_offs_from_stale_snapshot_state() {
    // The instrument vanishes from the doc entirely (doc swap): its snapshot midi state
    // must flush Offs once and then drop — never a hung note, never a stale carry.
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4, false);
    let mut e = Engine::new(16_000.0);
    let r1 = e.render(&d, &PhaseSnapshot::empty());
    let mut d_gone = d.clone().with_loop_id(2);
    d_gone.instruments.remove("ext");
    d_gone.matrix.remove("m_n");
    let r2 = e.render(&d_gone, &r1.next_snapshot);
    assert_eq!(r2.midi_events, vec![MidiEvent { frame: 0, msg: MidiMsg::NoteOff { ch: 2, note: 62 } }]);
    assert!(r2.next_snapshot.midi_out.is_empty(), "stale state must not be carried");
}

#[test]
fn midi_channel_edit_flushes_old_channel_and_resounds_on_new() {
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0,"velocity":96}}}"#, 4, false);
    let mut e = Engine::new(16_000.0);
    let r1 = e.render(&d, &PhaseSnapshot::empty());
    let mut d2 = d.clone().with_loop_id(2);
    d2.instruments.get_mut("ext").unwrap().routing.as_mut().unwrap().channel = 5; // wire 4
    let r2 = e.render(&d2, &r1.next_snapshot);
    let notes = note_events(&r2);
    assert_eq!(notes[0], (0, MidiMsg::NoteOff { ch: 2, note: 62 }), "old channel flushed: {notes:?}");
    assert!(notes.contains(&(0, MidiMsg::NoteOn { ch: 4, note: 62, vel: 100 })), "re-sounds on the new channel");
    // CC on-change state restarted for the new channel ⇒ fresh sync burst.
    assert_eq!(cc_events(&r2), vec![(0, 64)]);
}

#[test]
fn midi_cc_initial_burst_then_on_change_only() {
    // Static (unmodulated) cutoff 0.5: exactly one CC in loop 1 (the sync burst,
    // 0.5 → 64), zero in loop 2 — and a base edit (what an objectives `override`
    // rewrites) emits exactly the one changed value.
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4, false);
    let mut e = Engine::new(16_000.0);
    let r1 = e.render(&d, &PhaseSnapshot::empty());
    assert_eq!(cc_events(&r1), vec![(0, 64)]);
    let r2 = e.render(&d.clone().with_loop_id(2), &r1.next_snapshot);
    assert!(cc_events(&r2).is_empty());
    let d3 = d.with_param("instruments.ext.params.cutoff", 0.9).with_loop_id(3);
    let r3 = e.render(&d3, &r2.next_snapshot);
    assert_eq!(cc_events(&r3), vec![(0, cc_value(0.9))]);
}

#[test]
fn midi_cc_tracks_a_free_lfo_sparsely() {
    // A free bipolar sine (0.25 Hz, depth 0.3) on cutoff 0.5 sweeps ~0.2..0.8 once over
    // the 4 s loop. CC must follow it: one event per *7-bit value change* (~150), on the
    // control-hop grid, deduplicated — far fewer than the ~1000 hops sampled.
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4, true);
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    let ccs = cc_events(&r);
    assert!(ccs.len() > 20, "LFO must reach the wire: {} events", ccs.len());
    assert_eq!(ccs[0].0, 0, "initial sync burst at frame 0");
    let hops = r.num_frames / CONTROL_HOP;
    assert!(ccs.len() < hops / 2, "on-change thinning: {} events for {} hops", ccs.len(), hops);
    for w in ccs.windows(2) {
        assert_ne!(w[0].1, w[1].1, "consecutive CC values must differ (on-change)");
        assert_eq!(w[1].0 % CONTROL_HOP, 0, "CC lands on the control-hop grid");
    }
    // spans a real range (the LFO actually moves the value)
    let (mn, mx) = ccs.iter().fold((255u8, 0u8), |(a, b), &(_, v)| (a.min(v), b.max(v)));
    assert!(mx - mn > 40, "CC should sweep with the LFO: {mn}..{mx}");
}

#[test]
fn midi_events_are_bit_reproducible_and_snapshot_resumable() {
    // CE-R7.3 extended to MIDI: same (doc, snapshot, loop_id) ⇒ identical events; and
    // a two-loop chain replayed from the same snapshots matches exactly.
    let d = midi_doc(
        r#"{"step":0,"voicing":{"v1":{"offset":0,"velocity":96},"v2":{"offset":7}}},{"step":8,"voicing":{"v1":{"offset":2}}}"#,
        4,
        true,
    );
    let mut e1 = Engine::new(16_000.0);
    let r1a = e1.render(&d, &PhaseSnapshot::empty());
    let r2a = e1.render(&d.clone().with_loop_id(2), &r1a.next_snapshot);

    let mut e2 = Engine::new(16_000.0);
    let r1b = e2.render(&d, &PhaseSnapshot::empty());
    let r2b = e2.render(&d.clone().with_loop_id(2), &r1b.next_snapshot);

    assert_eq!(r1a.midi_events, r1b.midi_events);
    assert_eq!(r2a.midi_events, r2b.midi_events);
    assert_eq!(r1a.next_snapshot.midi_out, r1b.next_snapshot.midi_out);
    assert_eq!(r1a.audio, r1b.audio, "audio determinism unchanged");
}

#[test]
fn midi_instrument_is_silent_and_costs_the_mix_nothing() {
    // The translator produces no audio at all — and no per-track dry RMS entry
    // (absent ⇒ "produced no dry signal", which downstream reads as inaudible).
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0},"v2":{"offset":7}}}"#, 4, true);
    let r = Engine::new(16_000.0).render(&d, &PhaseSnapshot::empty());
    assert!(r.audio.iter().all(|&s| s == 0.0), "midi-only doc must render digital silence");
    assert!(!r.per_inst_rms.contains_key("ext"));
    assert!(!r.midi_events.is_empty(), "…but the MIDI stream is live");
}

#[test]
fn midi_legacy_snapshot_serialization_unchanged() {
    // A doc with no midi instruments must keep its snapshot JSON byte-identical to
    // pre-ADR-0026 (the `midi_out` field is skipped when empty).
    let r = Engine::new(48_000.0).render(&fixture(), &PhaseSnapshot::empty());
    let v = serde_json::to_value(&r.next_snapshot).unwrap();
    assert!(v.get("midi_out").is_none(), "empty midi_out must not serialize");
}

// ── ADR 0048: the MIDI mirror of an audio instrument ─────────────────────────

/// `midi_doc`'s timing (16 kHz ⇒ step k at frame `k · 4000`) with an audio instrument `pad`
/// (subtractive sine) fed by `steps_json` instead of the translator.
fn mirror_doc(steps_json: &str, polyphony: u32) -> LoopDoc {
    let json = format!(
        r#"{{
        "schema_version":"0.9","loop_id":1,
        "transport":{{"bpm":120,"master_steps":8,"step_unit":"beat"}},
        "harmony":{{"root":"D","mode":"dorian"}},
        "global":{{"master_gain_db":0,"stereo_width":0.0}},
        "instruments":{{"pad":{{"enabled":true,"pitched":true,
            "note_engine":{{"polyphony":{polyphony},"glide_ms":0}},
            "params":{{"wave":"sine","level_db":-10.46,"sustain":1.0,"release_s":2}}, "sends_db": {{ "rv": 0 }}}}}},
        "modulators":{{"chords":{{"type":"step_seq","output":"note","sync":"loop","length":16,
            "steps":[{steps_json}]}}}},
        "matrix":{{"m_n":{{"source":"chords","dest":"instruments.pad.notes","kind":"note","voices":4,"priority":1}}}},
        "buses": {{ "rv": {{ "return_gain_db": -Infinity, "effects": {{ "verb": {{ "type": "reverb", "params": {{ "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 }} }} }} }} }}
    }}"#
    );
    LoopDoc::from_json_str(&json).unwrap()
}

fn mirroring(ids: &[&str]) -> Engine {
    let mut e = Engine::new(16_000.0);
    e.set_note_tap(ids.iter().map(|s| s.to_string()).collect());
    e
}

fn mirror_notes(r: &RenderResult) -> Vec<(usize, TapMsg)> {
    r.note_tap.iter().map(|e| (e.frame, e.msg)).collect()
}

use crate::midi::TapMsg;

#[test]
fn the_mirror_leaves_every_fixture_bit_identical() {
    // It only reads the voices the synths read: audio, and every other snapshot field, are the same
    // with every instrument mirrored as with none.
    let mut any = false;
    for (name, doc) in block_fixtures() {
        let mut off = Engine::new(4_000.0);
        let mut on = Engine::new(4_000.0);
        on.set_note_tap(doc.instruments.keys().cloned().collect());
        let (mut s_off, mut s_on) = (PhaseSnapshot::empty(), PhaseSnapshot::empty());
        for l in 1..=2 {
            let d = doc.clone().with_loop_id(l);
            let (a, b) = (off.render(&d, &s_off), on.render(&d, &s_on));
            assert!(a.audio == b.audio, "{name}: loop {l} audio moved with the mirror on");
            any |= !b.note_tap.is_empty();
            let mut stripped = b.next_snapshot.clone();
            stripped.note_tap.clear();
            assert_eq!(a.next_snapshot, stripped, "{name}: loop {l} snapshot moved beyond `note_tap`");
            (s_off, s_on) = (a.next_snapshot, b.next_snapshot);
        }
    }
    assert!(any, "some fixture plays notes the mirror reports");
}

#[test]
fn an_unmirrored_snapshot_has_no_mirror_field() {
    let r = Engine::new(48_000.0).render(&fixture(), &PhaseSnapshot::empty());
    let v = serde_json::to_value(&r.next_snapshot).unwrap();
    assert!(v.get("note_tap").is_none(), "empty note_tap must not serialize");
    assert!(r.note_tap.is_empty());
}

#[test]
fn mirror_events_are_reproducible_resumable_and_block_split_invariant() {
    let doc = phrase_fixture();
    let all: Vec<&str> = doc.instruments.keys().map(|s| s.as_str()).collect();
    let (mut e1, mut e2) = (mirroring(&all), mirroring(&all));
    let r1a = e1.render(&doc, &PhaseSnapshot::empty());
    let r1b = e2.render(&doc, &PhaseSnapshot::empty());
    assert!(!r1a.note_tap.is_empty());
    assert_eq!(r1a.note_tap, r1b.note_tap);
    // Resumed from a JSON round trip of the snapshot, loop 2 is the same.
    let j = serde_json::to_string(&r1a.next_snapshot).unwrap();
    assert!(j.contains("\"note_tap\""));
    let back: PhaseSnapshot = serde_json::from_str(&j).unwrap();
    let d2 = doc.clone().with_loop_id(2);
    let r2a = e1.render(&d2, &r1a.next_snapshot);
    let r2b = e2.render(&d2, &back);
    assert_eq!(r2a.note_tap, r2b.note_tap);
    // Any block split reports the same events.
    let whole = loop_in_blocks(&e1, &d2, &r2a.next_snapshot, usize::MAX);
    for block in [1usize, 97, 4096] {
        let split = loop_in_blocks(&e1, &d2, &r2a.next_snapshot, block);
        assert_eq!(whole.note_tap, split.note_tap, "{block}-frame blocks");
        assert_eq!(whole.next_snapshot.note_tap, split.next_snapshot.note_tap);
    }
}

#[test]
fn mirror_notes_follow_the_voices_with_velocities() {
    // v1 authors velocity 90; v2 none ⇒ 100. Held across the next loop ⇒ nothing more.
    let d = mirror_doc(r#"{"step":0,"voicing":{"v1":{"offset":0,"velocity":90},"v2":{"offset":7}}}"#, 4);
    let mut e = mirroring(&["pad"]);
    let r1 = e.render(&d, &PhaseSnapshot::empty());
    assert_eq!(mirror_notes(&r1), vec![(0, TapMsg::NoteOn { note: 62, vel: 90 }), (0, TapMsg::NoteOn { note: 69, vel: 100 })]);
    assert!(r1.note_tap.iter().all(|e| e.inst == "pad"));
    let r2 = e.render(&d.clone().with_loop_id(2), &r1.next_snapshot);
    assert!(r2.note_tap.is_empty(), "sustained voices send nothing: {:?}", r2.note_tap);
    // The instrument's velocity_default stands in for an unauthored velocity.
    let mut d = d.clone();
    d.instruments.get_mut("pad").unwrap().velocity_default = Some(64);
    let r = mirroring(&["pad"]).render(&d, &PhaseSnapshot::empty());
    assert_eq!(mirror_notes(&r)[1], (0, TapMsg::NoteOn { note: 69, vel: 64 }));
}

#[test]
fn the_mirror_ends_a_note_at_its_release_not_at_the_end_of_its_tail() {
    let d = mirror_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}},{"step":8,"voicing":{"v1":{"release":true}}}"#, 4);
    let r = mirroring(&["pad"]).render(&d, &PhaseSnapshot::empty());
    assert_eq!(mirror_notes(&r), vec![(0, TapMsg::NoteOn { note: 62, vel: 100 }), (32_000, TapMsg::NoteOff { note: 62 })]);
    assert!(!r.next_snapshot.active_voices.is_empty() || r.num_frames > 32_000, "the audio voice rings on in its tail");
}

#[test]
fn the_mirror_follows_steals_and_re_strikes_a_note_a_fresh_voice_takes() {
    // Polyphony 1: v2 (A4) steals v1 (D4) ⇒ On(new) then Off(old).
    let d = mirror_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}},{"step":4,"voicing":{"v2":{"offset":7}}}"#, 1);
    let r = mirroring(&["pad"]).render(&d, &PhaseSnapshot::empty());
    assert_eq!(
        mirror_notes(&r),
        vec![(0, TapMsg::NoteOn { note: 62, vel: 100 }), (16_000, TapMsg::NoteOn { note: 69, vel: 100 }), (16_000, TapMsg::NoteOff { note: 62 })]
    );
    // v2 takes v1's own note: the audio re-attacks, so the mirror re-strikes (the translator holds).
    let d = mirror_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}},{"step":4,"voicing":{"v2":{"offset":0}}}"#, 1);
    let r = mirroring(&["pad"]).render(&d, &PhaseSnapshot::empty());
    assert_eq!(
        mirror_notes(&r),
        vec![(0, TapMsg::NoteOn { note: 62, vel: 100 }), (16_000, TapMsg::NoteOff { note: 62 }), (16_000, TapMsg::NoteOn { note: 62, vel: 100 })]
    );
}

#[test]
fn the_mirror_keys_voices_by_source() {
    // Two sources both author `v1`: two notes, not one.
    let json = r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":120,"master_steps":8,"step_unit":"beat"},
        "harmony":{"root":"D","mode":"dorian"},
        "global":{"master_gain_db":0,"stereo_width":0.0},
        "instruments":{"pad":{"enabled":true,"pitched":true,"note_engine":{"polyphony":4,"glide_ms":0},
            "params":{"wave":"sine","level_db":-10.46,"sustain":1.0,"release_s":2}, "sends_db": { "rv": 0 }}},
        "modulators":{
            "a":{"type":"step_seq","output":"note","sync":"loop","length":16,"steps":[{"step":0,"voicing":{"v1":{"offset":0}}}]},
            "b":{"type":"step_seq","output":"note","sync":"loop","length":16,"steps":[{"step":0,"voicing":{"v1":{"offset":7}}}]}},
        "matrix":{
            "m_a":{"source":"a","dest":"instruments.pad.notes","kind":"note","voices":2,"priority":1},
            "m_b":{"source":"b","dest":"instruments.pad.notes","kind":"note","voices":2,"priority":1}},
        "buses": { "rv": { "return_gain_db": -Infinity, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 } } } } }
    }"#;
    let d = LoopDoc::from_json_str(json).unwrap();
    let r = mirroring(&["pad"]).render(&d, &PhaseSnapshot::empty());
    let mut ons: Vec<u8> = r.note_tap.iter().filter_map(|e| match e.msg {
        TapMsg::NoteOn { note, .. } => Some(note),
        _ => None,
    }).collect();
    ons.sort();
    assert_eq!(ons, vec![62, 69]);
    assert_eq!(r.next_snapshot.note_tap["pad"].notes.keys().cloned().collect::<Vec<_>>(), vec!["a::v1", "b::v1"]);
}

#[test]
fn the_mirror_flushes_on_disable_set_change_and_route_loss() {
    let d = mirror_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4);
    let mut e = mirroring(&["pad"]);
    let r1 = e.render(&d, &PhaseSnapshot::empty());
    // Disabled ⇒ Off at the loop top; re-enabled ⇒ the held voice sounds again.
    let mut off = d.clone().with_loop_id(2);
    off.instruments.get_mut("pad").unwrap().enabled = false;
    let r2 = e.render(&off, &r1.next_snapshot);
    assert_eq!(mirror_notes(&r2), vec![(0, TapMsg::NoteOff { note: 62 })]);
    let r3 = e.render(&d.clone().with_loop_id(3), &r2.next_snapshot);
    assert_eq!(mirror_notes(&r3), vec![(0, TapMsg::NoteOn { note: 62, vel: 100 })]);
    // No longer mirrored ⇒ Off at the loop top, the state dropped.
    e.set_note_tap(Default::default());
    let r4 = e.render(&d.clone().with_loop_id(4), &r3.next_snapshot);
    assert_eq!(mirror_notes(&r4), vec![(0, TapMsg::NoteOff { note: 62 })]);
    assert!(r4.next_snapshot.note_tap.is_empty());
    // A song seam that drops the route ⇒ Off at frame 0.
    let mut e = mirroring(&["pad"]);
    let r1 = e.render(&d, &PhaseSnapshot::empty());
    let mut b = d.clone();
    b.matrix.clear();
    let mut snap = r1.next_snapshot.clone();
    snap.seam = Some(Seam::release_only());
    let r2 = e.render(&b, &snap);
    assert_eq!(mirror_notes(&r2), vec![(0, TapMsg::NoteOff { note: 62 })]);
}

#[test]
fn a_live_edit_that_removes_the_route_ends_the_mirrored_note_at_the_edit_point() {
    let a = mirror_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4);
    let mut b = a.clone();
    b.matrix.clear();
    let mut eng = mirroring(&["pad"]);
    let warm = eng.render(&a, &PhaseSnapshot::empty());
    let mut lr = eng.begin_loop(&a, &warm.next_snapshot);
    lr.render_block(&mut eng, 10_000);
    let mut fork = eng.clone();
    let (lr2, _) = lr.retarget(&mut fork, &b);
    let r = lr2.finish(&mut fork);
    assert_eq!(mirror_notes(&r), vec![(10_000, TapMsg::NoteOff { note: 62 })]);
}

#[test]
fn mute_and_solo_never_touch_the_mirror() {
    let d = mirror_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}},{"step":8,"voicing":{"v2":{"offset":3}}}"#, 4);
    let plain = mirroring(&["pad"]).render(&d, &PhaseSnapshot::empty());
    let mut muted = mirroring(&["pad"]);
    muted.set_live_gains([("pad".to_string(), 0.0)].into_iter().collect());
    let r = muted.render(&d, &PhaseSnapshot::empty());
    assert_eq!(r.note_tap, plain.note_tap);
    assert!(r.audio != plain.audio, "the mute did reach the audio");
}

#[test]
fn translators_and_pinned_binaural_pairs_are_not_mirrored() {
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4, false);
    let r = mirroring(&["ext"]).render(&d, &PhaseSnapshot::empty());
    assert!(r.note_tap.is_empty(), "a `midi` instrument is MIDI already");
    assert!(!r.midi_events.is_empty());
    let mut b = binaural_fixture();
    let ids: Vec<String> = b.instruments.keys().cloned().collect();
    let mut e = Engine::new(16_000.0);
    e.set_note_tap(ids.iter().cloned().collect());
    let free = e.render(&b, &PhaseSnapshot::empty());
    assert!(free.note_tap.iter().any(|ev| ev.inst == "bin"), "a binaural pair that follows its notes is mirrored");
    b.instruments.get_mut("bin").unwrap().params.insert("root_hz".into(), serde_json::json!(110.0).into());
    let mut e = Engine::new(16_000.0);
    e.set_note_tap(ids.into_iter().collect());
    let pinned = e.render(&b, &PhaseSnapshot::empty());
    assert!(!pinned.note_tap.iter().any(|ev| ev.inst == "bin"), "a pinned root_hz pair is not");
}

// ── ADR 0028: the phrase arm, end-to-end ─────────────────────────────────────

fn phrase_fixture() -> LoopDoc {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_phrase_pools.json");
    LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn phrase_render_bit_identical_and_snapshot_resumable() {
    // CE-R7.3 over the phrase arm: fresh engines agree bit-for-bit across a two-loop
    // chain, and a snapshot that round-trips through JSON (pinning the new
    // pending_notes / phrase_anchor_step / phrase_ordinal serde) resumes identically.
    let d = phrase_fixture();
    let mut e1 = Engine::new(16_000.0);
    let r1a = e1.render(&d, &PhaseSnapshot::empty());
    let r2a = e1.render(&d.clone().with_loop_id(78), &r1a.next_snapshot);

    let mut e2 = Engine::new(16_000.0);
    let r1b = e2.render(&d, &PhaseSnapshot::empty());
    let snap_json = serde_json::to_string(&r1b.next_snapshot).unwrap();
    let snap: PhaseSnapshot = serde_json::from_str(&snap_json).unwrap();
    let r2b = e2.render(&d.clone().with_loop_id(78), &snap);

    assert_eq!(r1a.audio, r1b.audio, "loop 1 bit-identical");
    assert_eq!(r2a.audio, r2b.audio, "loop 2 bit-identical through a JSON'd snapshot");
    assert_eq!(r1a.midi_events, r1b.midi_events);
    assert!(r1a.audio.iter().any(|&x| x != 0.0), "the routed phrase sounds");
    assert!(r2a.audio.iter().all(|x| x.is_finite()));
    // the tile clock advanced and serializes (free-run "ph" straddles its second tile)
    assert!(r1a.next_snapshot.modulator_phases["ph"].phrase_ordinal >= 1);
}

#[test]
fn phrase_velocity_reaches_midi_note_on_and_offs_balance() {
    // ph_midi → ext (ch 1): authored velocities land on Note-Ons (112 then 64), an
    // unmarked note gets the instrument's velocity_default (96), and every gated note
    // closes — Ons and Offs balance within each loop (no hangs).
    let d = phrase_fixture();
    let mut eng = Engine::new(16_000.0);
    let r1 = eng.render(&d, &PhaseSnapshot::empty());
    let r2 = eng.render(&d.clone().with_loop_id(78), &r1.next_snapshot);
    let vel_at = |r: &RenderResult, f: usize| {
        r.midi_events.iter().find_map(|e| match e.msg {
            MidiMsg::NoteOn { ch: 0, vel, .. } if e.frame == f => Some(vel), // wire ch = authored 1 − 1
            _ => None,
        })
    };
    // 32000-frame loop @16k, host step 4000: t0 → 0, t1.5 → 6000, t2 → 8000
    assert_eq!(vel_at(&r1, 0), Some(112), "authored velocity on the beat");
    assert_eq!(vel_at(&r1, 6_000), Some(64), "authored velocity on the and");
    assert_eq!(vel_at(&r1, 8_000), Some(96), "unmarked note falls back to velocity_default");
    for r in [&r1, &r2] {
        let ons = r.midi_events.iter().filter(|e| matches!(e.msg, MidiMsg::NoteOn { ch: 0, .. })).count();
        let offs = r.midi_events.iter().filter(|e| matches!(e.msg, MidiMsg::NoteOff { ch: 0, .. })).count();
        assert_eq!(ons, offs, "every gated phrase note closes within the loop");
        assert!(ons > 0);
    }
}

#[test]
fn phrase_hold_note_sustains_across_loops_instead_of_stacking() {
    // pad_hold (a dur-less note, voices budget 1): the stable p0 id makes loop 2 a
    // SUSTAIN, not a fresh strike — exactly one held voice survives, no pileup/steal.
    let d = phrase_fixture();
    let mut eng = Engine::new(16_000.0);
    let r1 = eng.render(&d, &PhaseSnapshot::empty());
    let r2 = eng.render(&d.clone().with_loop_id(78), &r1.next_snapshot);
    for r in [&r1, &r2] {
        let held: Vec<_> = r.next_snapshot.active_voices["poly.notes"]
            .iter()
            .filter(|v| v.source_id == "pad_hold")
            .collect();
        assert_eq!(held.len(), 1, "one hold voice, never stacked");
        assert_eq!(held[0].voice_id, "p0");
        assert!(held[0].released_at_s.is_none(), "still held");
    }
    // and the envelope clock kept accumulating across the seam (sustain, not re-strike)
    let t1 = r1.next_snapshot.active_voices["poly.notes"].iter().find(|v| v.source_id == "pad_hold").unwrap().elapsed_decay_s;
    let t2 = r2.next_snapshot.active_voices["poly.notes"].iter().find(|v| v.source_id == "pad_hold").unwrap().elapsed_decay_s;
    assert!(t2 > t1, "sustain accumulates ({t1} → {t2}); a re-strike would reset it");
}

#[test]
fn pending_phrase_voices_keep_their_ids_through_a_json_snapshot() {
    // A loop-synced tile of 3 steps on a 4-step loop straddles the seam: its t:1 / t:2 notes
    // (and their paired releases) are CARRIED via pending_notes. `NoteVoice.id` is never
    // serialized in a doc (the voicing map key holds it), so a pending voicing must carry
    // its `p{k}` ids explicitly — otherwise a JSON'd snapshot resumed the strike anonymous
    // and dropped the release (a stuck gate), breaking CE-R7.3 on resume.
    let src = r#"{ "schema_version": "0.9", "transport": { "bpm": 120, "master_steps": 4 },
        "harmony": { "root": "C", "mode": "ionian" }, "global": { "master_gain_db": -6.02 },
        "instruments": { "poly": { "enabled": true, "pitched": true, "note_engine": { "polyphony": 4 },
                                   "params": { "wave": "sine", "level_db": -6.02, "cutoff_hz": 1267.9, "attack_s": 0, "decay_s": 0.3, "sustain": 0.8, "release_s": 0.2 } } },
        "modulators": { "ph": { "type": "step_seq", "output": "note", "sync": "loop", "length": 4, "step_duration": "1/4",
                                "phrase": { "pool": ["cell"] } } },
        "sequence_pools": { "cell": { "fit": "literal", "length": 3,
            "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }, { "t": 1, "offset": 4, "dur": 0.5 }, { "t": 2, "offset": 7, "dur": 0.5 }] } },
        "matrix": { "m": { "source": "ph", "dest": "instruments.poly.notes", "kind": "note", "voices": 4, "priority": 1 } } }"#;
    let d = LoopDoc::from_json_str(src).unwrap();
    let mut e1 = Engine::new(16_000.0);
    let r1 = e1.render(&d, &PhaseSnapshot::empty());
    let pending = &r1.next_snapshot.modulator_phases["ph"].pending_notes;
    assert!(pending.len() >= 2, "the straddling tile carries strikes + releases: {pending:?}");
    assert!(pending.iter().any(|p| p.voicing.iter().any(|v| v.release)), "a carried release");

    let json = serde_json::to_string(&r1.next_snapshot).unwrap();
    assert!(json.contains("\"id\":\"p1\""), "pending voicings serialize their ids: {json}");
    let snap: PhaseSnapshot = serde_json::from_str(&json).unwrap();
    for p in &snap.modulator_phases["ph"].pending_notes {
        for v in &p.voicing {
            assert!(v.id.is_some(), "id survives the round trip: {p:?}");
        }
    }
    assert_eq!(snap, r1.next_snapshot, "the snapshot round-trips exactly");

    let r2a = e1.render(&d.clone().with_loop_id(9), &r1.next_snapshot);
    let mut e2 = Engine::new(16_000.0);
    let _ = e2.render(&d, &PhaseSnapshot::empty());
    let r2b = e2.render(&d.clone().with_loop_id(9), &snap);
    assert_eq!(r2a.audio, r2b.audio, "loop 2 is bit-identical resumed from JSON (CE-R7.3)");
}

#[test]
fn modulator_phase_default_serialization_unchanged() {
    // Pre-ADR-0028 snapshot bytes: the new phrase/pending fields are default-skipped,
    // so phrase-less docs keep byte-identical ModulatorPhase JSON.
    let v = serde_json::to_value(ModulatorPhase::default()).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
    assert_eq!(keys, ["frames_into_step", "last_output", "lfo_phase", "step_index"]);
}

// ── Song seams (ADR 0029) ─────────────────────────────────────────────────────────────────────

const SEAM_SR: f64 = 8_000.0;

/// One held sine note on `poly` (sustain 1, release 0.25 ⇒ 1 s tail) + an optional LFO → cutoff
/// route. 8 beats @ 120 bpm = 4 s loops. Dry only (no sends) so the audio is a clean probe.
fn seam_doc(level: f32, lfo_depth: Option<f32>, note_routed: bool) -> LoopDoc {
    let mut matrix = serde_json::Map::new();
    if note_routed {
        matrix.insert("m_n".into(), serde_json::json!({ "source": "seq", "dest": "instruments.poly.notes", "kind": "note", "voices": 1 }));
    }
    if let Some(d) = lfo_depth {
        // `d` is a fraction of the cutoff's range, written in octaves.
        matrix.insert("m_lfo".into(), serde_json::json!({ "source": "wob", "dest": "instruments.poly.params.cutoff_hz", "kind": "mod", "depth_oct": d * 100f32.log2() }));
    }
    let v = serde_json::json!({
        "schema_version": "0.9", "loop_id": 1,
        "transport": { "bpm": 120, "master_steps": 8, "step_unit": "beat" },
        "harmony": { "root": "A", "mode": "major" },
        "global": { "master_gain_db": 0, "stereo_width": 0.0 },
        "buses": {},
        "instruments": { "poly": { "note_engine": { "polyphony": 1 },
            "params": { "wave": "sine", "level_db": crate::params::amp_to_db(level), "cutoff_hz": 800, "sustain": 1.0, "release_s": 1 } } },
        "modulators": {
            "seq": { "type": "step_seq", "output": "note", "sync": "loop", "length": 8,
                     "steps": [ { "step": 0, "voicing": { "v1": { "offset": 0 } } } ] },
            "wob": { "type": "lfo", "output": "control", "wave": "sine", "sync": "free", "rate_hz": 2.0 }
        },
        "matrix": matrix
    });
    LoopDoc::from_json_str(&v.to_string()).unwrap()
}

/// An amplitude's normalized gain (dB over −60..+6, ADR 0054 P2): what `seam_doc(level)` plays.
fn lvl_n(amp: f32) -> f32 {
    crate::params::GAIN.inverse(crate::params::amp_to_db(amp))
}

/// Peak |sample| per `w`-frame block of the mono downmix.
fn block_peaks(audio: &[f32], w: usize) -> Vec<f32> {
    audio.chunks(2 * w).map(|c| c.chunks(2).map(|f| (0.5 * (f[0] + f[1])).abs()).fold(0.0, f32::max)).collect()
}

/// Render `a` once, then `b` from its snapshot with an optional seam; returns (a, b) renders.
fn render_across_seam(a: &LoopDoc, b: &LoopDoc, seam: Option<Seam>) -> (RenderResult, RenderResult) {
    let mut eng = Engine::new(SEAM_SR);
    eng.set_viz(true);
    let ra = eng.render(a, &PhaseSnapshot::empty());
    let mut snap = ra.next_snapshot.clone();
    snap.seam = seam;
    let rb = eng.render(b, &snap);
    (ra, rb)
}

fn eff_series(r: &RenderResult, dest: &str) -> Vec<f32> {
    r.viz_eff_hops.iter().map(|h| h.get(dest).copied().unwrap_or(f32::NAN)).collect()
}

#[test]
fn no_seam_means_no_seam_key_in_the_snapshot() {
    let r = Engine::new(SEAM_SR).render(&seam_doc(0.5, Some(0.2), true), &PhaseSnapshot::empty());
    assert!(r.next_snapshot.seam.is_none());
    let j = serde_json::to_string(&r.next_snapshot).unwrap();
    assert!(!j.contains("seam"), "non-song snapshots stay byte-identical: {j}");
}

#[test]
fn seam_glides_an_unmodulated_param_instead_of_stepping() {
    let (a, b) = (seam_doc(0.9, None, true), seam_doc(0.3, None, true));
    let half_loop = (b.total_frames(SEAM_SR) / 2) as u64;
    let seam = crate::song::seam_between(&a, &b, half_loop);
    // In normalized dB (ADR 0054 P2): −0.92 → −10.46 dB is 9.5 dB of the 66 dB span.
    let (from, to) = (lvl_n(0.9), lvl_n(0.3));
    assert!(seam.param_offsets.get("instruments.poly.params.level_db").is_some_and(|o| (o - (from - to)).abs() < 1e-5), "{:?}", seam.param_offsets);

    let (ra, glide) = render_across_seam(&a, &b, Some(seam));
    let (_, cut) = render_across_seam(&a, &b, None);
    // The glide's effective level starts at the OLD value and lands exactly on the new one.
    let lvl = eff_series(&glide, "instruments.poly.params.level_db");
    assert!((lvl[0] - from).abs() < 0.003, "first hop {}", lvl[0]);
    assert!(lvl.windows(2).all(|w| w[1] <= w[0] + 1e-6), "monotone ease-down");
    assert_eq!(*lvl.last().unwrap(), to, "settles exactly on the new base");
    // Audio: no step at the seam with the glide; a ~3× step on a hard cut.
    let w = 256;
    let last_a = *block_peaks(&ra.audio, w).last().unwrap();
    let ratio = |rb: &RenderResult| last_a / block_peaks(&rb.audio, w)[0];
    assert!((ratio(&glide) - 1.0).abs() < 0.05, "glide seam ratio {}", ratio(&glide));
    assert!(ratio(&cut) > 2.5, "hard-cut ratio {}", ratio(&cut));
    // Once the glide is over the loop is the new block, sample for sample (no hidden state).
    let n = glide.audio.len();
    let tail = 3 * n / 4..n;
    let diff = glide.audio[tail.clone()].iter().zip(&cut.audio[tail]).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
    assert!(diff < 1e-5, "post-glide tail differs by {diff}");
}

#[test]
fn removed_mod_route_fades_out_to_exact_base() {
    let (a, b) = (seam_doc(0.5, Some(0.4), true), seam_doc(0.5, None, true));
    let t = (b.total_frames(SEAM_SR) / 2) as u64;
    let seam = crate::song::seam_between(&a, &b, t);
    assert!(seam.fading_routes.contains_key("m_lfo"));
    let (_, rb) = render_across_seam(&a, &b, Some(seam));
    let cut = eff_series(&rb, "instruments.poly.params.cutoff_hz");
    let half = cut.len() / 2;
    let early = cut[..half / 2].iter().map(|v| (v - 0.5).abs()).fold(0.0, f32::max);
    assert!(early > 0.1, "the old LFO is still audible early in the seam ({early})");
    assert!(cut[half + 2..].iter().all(|&v| v == 0.5), "after the glide the cutoff is exactly its base: {:?}", &cut[half..]);
}

#[test]
fn changed_and_new_route_depths_glide() {
    let (a, b) = (seam_doc(0.5, Some(0.1), true), seam_doc(0.5, Some(0.45), true));
    let t = (b.total_frames(SEAM_SR) / 2) as u64;
    let seam = crate::song::seam_between(&a, &b, t);
    assert!(seam.depth_from.get("m_lfo").is_some_and(|d| (d - 0.1).abs() < 1e-6), "a depth glides as its normalized distance");
    let (_, rb) = render_across_seam(&a, &b, Some(seam));
    let cut = eff_series(&rb, "instruments.poly.params.cutoff_hz");
    let q = cut.len() / 8;
    let swing = |s: &[f32]| s.iter().map(|v| (v - 0.5).abs()).fold(0.0, f32::max);
    assert!(swing(&cut[..q]) < 0.13, "starts near the old depth: {}", swing(&cut[..q]));
    assert!(swing(&cut[5 * q..]) > 0.4, "ends at the new depth: {}", swing(&cut[5 * q..]));
    // The next loop's ordinary slew bookkeeping sees the target depth (no snap back).
    assert!((rb.next_snapshot.connection_depths["m_lfo"] - 0.45).abs() < 1e-6);

    // A brand-new route fades IN from zero.
    let (a, b) = (seam_doc(0.5, None, true), seam_doc(0.5, Some(0.45), true));
    let seam = crate::song::seam_between(&a, &b, t);
    assert_eq!(seam.depth_from.get("m_lfo"), Some(&0.0));
    let (_, rb) = render_across_seam(&a, &b, Some(seam));
    let cut = eff_series(&rb, "instruments.poly.params.cutoff_hz");
    assert!(swing(&cut[..2]) < 0.02, "new route starts silent: {:?}", &cut[..2]);
}

#[test]
fn a_seam_longer_than_a_loop_carries_then_clears() {
    let (a, b) = (seam_doc(0.9, None, true), seam_doc(0.3, None, true));
    let loop_frames = b.total_frames(SEAM_SR) as u64;
    let seam = crate::song::seam_between(&a, &b, loop_frames * 3 / 2);
    let (_, b1) = render_across_seam(&a, &b, Some(seam));
    let carried = b1.next_snapshot.seam.clone().expect("glide continues into the next loop");
    assert_eq!(carried.elapsed_frames, loop_frames);
    assert!(!carried.release_orphans, "orphans release once, at the seam's first frame");
    let mut eng = Engine::new(SEAM_SR);
    eng.set_viz(true);
    let b2 = eng.render(&b, &b1.next_snapshot);
    assert!(b2.next_snapshot.seam.is_none(), "cleared once the glide is done");
    let (l1, l2) = (eff_series(&b1, "instruments.poly.params.level_db"), eff_series(&b2, "instruments.poly.params.level_db"));
    assert!((l1.last().unwrap() - l2[0]).abs() < 0.003, "continuous across the loop seam: {} → {}", l1.last().unwrap(), l2[0]);
    assert!(l1.last().unwrap() > &(lvl_n(0.3) + 0.01), "still gliding at the end of loop 1");
    assert_eq!(*l2.last().unwrap(), lvl_n(0.3));
}

#[test]
fn notes_that_lose_their_route_release_instead_of_hanging() {
    let (a, b) = (seam_doc(0.5, None, true), seam_doc(0.5, None, false));
    let voices = |r: &RenderResult| r.next_snapshot.active_voices.get("poly.notes").map(|v| v.len()).unwrap_or(0);
    // Pre-ADR-0029 behaviour (no seam): the un-routed voice sustains forever.
    let (_, hang) = render_across_seam(&a, &b, None);
    assert_eq!(voices(&hang), 1, "the documented hang without a seam");
    assert!(block_peaks(&hang.audio, 256).last().unwrap() > &0.1);
    // With the seam's orphan release (hard cut or glide alike): note-off at frame 0, 1 s tail.
    for seam in [Seam::release_only(), crate::song::seam_between(&a, &b, 4_000)] {
        assert!(seam.release_orphans);
        let (_, rel) = render_across_seam(&a, &b, Some(seam));
        assert_eq!(voices(&rel), 0, "released + pruned within the loop");
        let peaks = block_peaks(&rel.audio, 256);
        assert!(peaks[0] > 0.1, "fades (not cuts): {}", peaks[0]);
        assert!(peaks[peaks.len() / 2..].iter().all(|&p| p < 1e-6), "silent once the release is done");
    }
}

#[test]
fn midi_notes_that_lose_their_route_get_a_note_off() {
    let d = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4, false);
    let mut eng = Engine::new(16_000.0);
    let r1 = eng.render(&d, &PhaseSnapshot::empty());
    let mut b = d.clone();
    b.matrix.clear();
    let mut snap = r1.next_snapshot.clone();
    snap.seam = Some(Seam::release_only());
    let r2 = eng.render(&b, &snap);
    assert_eq!(note_events(&r2), vec![(0, MidiMsg::NoteOff { ch: 2, note: 62 })]);
}

#[test]
fn a_snapshot_mid_seam_replays_bit_identically_from_json() {
    let (a, b) = (seam_doc(0.9, Some(0.3), true), seam_doc(0.3, None, true));
    let mut eng = Engine::new(SEAM_SR);
    let ra = eng.render(&a, &PhaseSnapshot::empty());
    let mut snap = ra.next_snapshot.clone();
    snap.seam = Some(crate::song::seam_between(&a, &b, 20_000));
    let j = serde_json::to_string(&snap).unwrap();
    assert!(j.contains("\"seam\""));
    let back: PhaseSnapshot = serde_json::from_str(&j).unwrap();
    assert_eq!(back, snap);
    let x = eng.clone().render(&b, &snap);
    let y = eng.clone().render(&b, &back);
    assert_eq!(x.audio, y.audio, "CE-R7.3: (doc, snapshot) fully determines the seam loop");
}

#[test]
fn hard_cut_seam_only_releases() {
    let (a, b) = (seam_doc(0.9, Some(0.3), true), seam_doc(0.3, None, false));
    assert_eq!(crate::song::seam_between(&a, &b, 0), Seam::release_only());
}

#[test]
fn seam_offsets_only_cover_glideable_params() {
    let a = seam_doc(0.9, None, true);
    let mut b = seam_doc(0.3, None, true);
    // A categorical (`wave`) and a non-modulatable numeric (`voices_detune_cents`) step; they never
    // become offsets. `master_gain_db` glides.
    b.instruments.get_mut("poly").unwrap().params.insert("wave".into(), serde_json::json!("saw").into());
    b.instruments.get_mut("poly").unwrap().params.insert("voices_detune_cents".into(), serde_json::json!(40).into());
    let mut a2 = a.clone();
    a2.instruments.get_mut("poly").unwrap().params.insert("voices_detune_cents".into(), serde_json::json!(10).into());
    b.global.master_gain_db = -1.94;
    let s = crate::song::seam_between(&a2, &b, 1000);
    let keys: Vec<&str> = s.param_offsets.keys().map(|k| k.as_str()).collect();
    assert_eq!(keys, vec!["global.master_gain_db", "instruments.poly.params.level_db"]);
}

// ── Block rendering (ADR 0035) ─────────────────────────────────────────────

/// A fixture per engine feature (subtractive + LFO matrix, dual, modal, sequence pools / phrases,
/// shimmer buses, the house drone), parsed with any song stripped.
fn block_fixtures() -> Vec<(String, LoopDoc)> {
    [
        "loop_contract_v0.9.json",
        "loop_dual_basic.json",
        "loop_modal_marimba.json",
        "loop_binaural_basic.json",
        "loop_phrase_pools.json",
        "loop_buses_shimmer.json",
        "loop_drone_d_lydian.json5",
    ]
        .iter()
        .map(|name| {
            let src = std::fs::read_to_string(crate::test_support::fixtures_dir().join(name)).unwrap();
            let mut doc = LoopDoc::from_json_str(&src).unwrap();
            doc.song = None;
            (name.to_string(), doc)
        })
        .collect()
}

/// A warm snapshot (one loop rendered), then the next loop split into `block`-frame blocks.
fn loop_in_blocks(eng: &Engine, doc: &LoopDoc, snap: &PhaseSnapshot, block: usize) -> RenderResult {
    let mut eng = eng.clone();
    let mut lr = eng.begin_loop(doc, snap);
    while !lr.is_done() {
        lr.render_block(&mut eng, block);
    }
    lr.finish(&mut eng)
}

#[test]
fn any_block_split_renders_the_same_samples() {
    // Events and seeds use the frame position within the loop, so how a loop is cut into blocks
    // can't change a sample (a low rate keeps the debug-build cost down; the code path is the same).
    for (name, doc) in block_fixtures() {
        let mut eng = Engine::new(4_000.0);
        let warm = eng.render(&doc, &PhaseSnapshot::empty());
        let whole = loop_in_blocks(&eng, &doc, &warm.next_snapshot, usize::MAX);
        for block in [1usize, 7, 64, 4096] {
            let split = loop_in_blocks(&eng, &doc, &warm.next_snapshot, block);
            assert!(whole.audio == split.audio, "{name}: {block}-frame blocks differ from the whole loop (max diff {})", max_abs_diff(&whole.audio, &split.audio));
            assert_eq!(whole.next_snapshot, split.next_snapshot, "{name}: {block}-frame blocks changed the next snapshot");
        }
    }
}

#[test]
fn retargeting_to_the_same_doc_mid_loop_is_bit_identical() {
    for (name, doc) in block_fixtures() {
        let mut eng = Engine::new(4_000.0);
        let warm = eng.render(&doc, &PhaseSnapshot::empty());
        let reference = eng.clone().render(&doc, &warm.next_snapshot);
        let total = reference.num_frames;
        for at in [0usize, total / 3, total - 1] {
            let mut e = eng.clone();
            let mut lr = e.begin_loop(&doc, &warm.next_snapshot);
            lr.render_block(&mut e, at);
            let mut fork = e.clone();
            let (lr2, kind) = lr.retarget(&mut fork, &doc);
            assert_eq!(kind, EditKind::Value, "{name}");
            let r = lr2.finish(&mut fork);
            assert!(r.audio == reference.audio, "{name}: retarget at {at} changed the audio (max diff {})", max_abs_diff(&r.audio, &reference.audio));
            assert_eq!(r.next_snapshot, reference.next_snapshot, "{name}: retarget at {at} changed the next snapshot");
        }
    }
}

/// A held sine (one step at the loop top, sustain 1) for edit tests: 1 s loop at 60 bpm × 1 beat.
fn held_doc(offset: i32) -> LoopDoc {
    LoopDoc::from_value(serde_json::json!({
        "schema_version": "0.9", "loop_id": 1,
        "transport": { "bpm": 60, "master_steps": 2 },
        "harmony": { "root": "A", "mode": "major" },
        "global": { "master_gain_db": 0 },
        "instruments": { "pad": { "note_engine": { "polyphony": 1 }, "params": { "wave": "sine", "level_db": -6.02, "sustain": 1.0, "release_s": 0.2 } } },
        "modulators": { "seq": { "type": "step_seq", "output": "note", "length": 1, "steps": [ { "step": 0, "voicing": { "v": { "offset": offset } } } ] } },
        "matrix": { "m": { "source": "seq", "dest": "instruments.pad.notes", "kind": "note", "voices": 1 } }
    }))
    .unwrap()
}

/// Upward zero crossings of the left channel in frames `a..b`, per second.
fn crossings_hz(audio: &[f32], sr: f32, a: usize, b: usize) -> f32 {
    let n = (a + 1..b).filter(|&f| audio[2 * (f - 1)] < 0.0 && audio[2 * f] >= 0.0).count();
    n as f32 * sr / (b - a) as f32
}

/// Render one loop of `a`, retargeting to `b` after `at` frames (on a fork, as the live player does).
fn edit_mid_loop(a: &LoopDoc, b: &LoopDoc, at: usize) -> (RenderResult, EditKind) {
    let mut eng = Engine::new(8_000.0);
    let warm = eng.render(a, &PhaseSnapshot::empty());
    let mut lr = eng.begin_loop(a, &warm.next_snapshot);
    lr.render_block(&mut eng, at);
    let mut fork = eng.clone();
    let (lr2, kind) = lr.retarget(&mut fork, b);
    (lr2.finish(&mut fork), kind)
}

#[test]
fn an_offset_edit_is_audible_from_the_edit_point() {
    // A4 (440 Hz) held; mid-loop the step's offset becomes +12 → A5 from there on, same voice (glide 0).
    let (r, kind) = edit_mid_loop(&held_doc(0), &held_doc(12), 8_000);
    assert_eq!(kind, EditKind::Value);
    let before = crossings_hz(&r.audio, 8_000.0, 1_000, 7_900);
    let after = crossings_hz(&r.audio, 8_000.0, 8_200, 15_900);
    assert!((before - 440.0).abs() < 10.0, "before the edit: {before} Hz");
    assert!((after - 880.0).abs() < 15.0, "after the edit the new offset sounds: {after} Hz");
    // Frames before the edit are untouched.
    let mut eng = Engine::new(8_000.0);
    let warm = eng.render(&held_doc(0), &PhaseSnapshot::empty());
    let unedited = eng.render(&held_doc(0), &warm.next_snapshot);
    assert!(r.audio[..2 * 8_000] == unedited.audio[..2 * 8_000]);
}

#[test]
fn removing_a_note_route_releases_its_voice_at_the_edit_point() {
    let mut b = held_doc(0);
    b.matrix.clear();
    let (r, kind) = edit_mid_loop(&held_doc(0), &b, 8_000);
    assert_eq!(kind, EditKind::Structural);
    let rms = |a: usize, z: usize| (r.audio[2 * a..2 * z].iter().map(|x| x * x).sum::<f32>() / (2 * (z - a)) as f32).sqrt();
    assert!(rms(7_000, 8_000) > 0.1, "sounding before the edit");
    assert!(rms(10_000, 16_000) < 1e-3, "released (0.2 s tail) after the edit: {}", rms(10_000, 16_000));
    assert!(r.next_snapshot.active_voices.is_empty(), "the released voice is pruned at the loop end");
}

#[test]
fn a_midi_note_whose_route_an_edit_removes_ends_at_the_edit_point() {
    // A held D4 on the translator; mid-loop the note route goes. The Note-Off goes out at the edit
    // point, not at the next loop top.
    let a = midi_doc(r#"{"step":0,"voicing":{"v1":{"offset":0}}}"#, 4, false);
    let mut b = a.clone();
    b.matrix.clear();
    let mut eng = Engine::new(16_000.0);
    let warm = eng.render(&a, &PhaseSnapshot::empty());
    let mut lr = eng.begin_loop(&a, &warm.next_snapshot);
    lr.render_block(&mut eng, 10_000);
    let mut fork = eng.clone();
    let (lr2, _) = lr.retarget(&mut fork, &b);
    let r = lr2.finish(&mut fork);
    assert_eq!(note_events(&r), vec![(10_000, MidiMsg::NoteOff { ch: 2, note: 62 })]);
    assert!(r.next_snapshot.midi_out["ext"].notes.is_empty(), "nothing left sounding");
}

#[test]
fn a_loop_length_edit_maps_the_position_and_keeps_envelopes_continuous() {
    // 2 beats → 4 beats at 60 bpm: the beat position (frame 8 000 = beat 1) is kept, so the edit
    // point is frame 8 000 of the longer loop.
    let mut b = held_doc(0);
    b.transport.master_steps = 4;
    let (r, kind) = edit_mid_loop(&held_doc(0), &b, 8_000);
    assert_eq!(kind, EditKind::Structural);
    assert_eq!(r.num_frames, 32_000);
    // The held note keeps sounding straight through the edit (no re-attack gap, no release).
    let peak_window = |a: usize, z: usize| r.audio[2 * a..2 * z].iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(peak_window(8_000, 8_400) > 0.3, "still sounding right after the edit point");
    // A tempo edit (60 → 120 bpm) instead scales the frame: beat 1 of 2 at 60 bpm = frame 4 000.
    let mut fast = held_doc(0);
    fast.transport.bpm = 120.0;
    let (r2, _) = edit_mid_loop(&held_doc(0), &fast, 8_000);
    assert_eq!(r2.num_frames, 8_000);
    assert!(r2.audio[2 * 4_000..].iter().any(|x| x.abs() > 0.3), "rendered from the scaled point on");
    assert!(peak_window(30_000, 32_000) > 0.3, "and to the end of the longer loop");
}

#[test]
fn a_live_gain_mutes_an_instrument_with_a_short_ramp_and_unity_is_bit_identical() {
    let doc = held_doc(0);
    let mut eng = Engine::new(8_000.0);
    let warm = eng.render(&doc, &PhaseSnapshot::empty());
    let reference = eng.clone().render(&doc, &warm.next_snapshot);
    // Unity targets ⇒ bit-identical.
    let mut unity = eng.clone();
    unity.set_live_gains([("pad".to_string(), 1.0)].into_iter().collect());
    assert!(unity.render(&doc, &warm.next_snapshot).audio == reference.audio);
    // Mute mid-loop: ramps to silence in ~10 ms (80 frames at 8 kHz) and stays there next loop.
    let mut e = eng.clone();
    let mut lr = e.begin_loop(&doc, &warm.next_snapshot);
    lr.render_block(&mut e, 8_000);
    lr.set_gain_targets(&mut e, &[("pad".to_string(), 0.0)].into_iter().collect());
    let r = lr.finish(&mut e);
    assert!(r.audio[2 * 8_000..2 * 8_100].iter().any(|x| x.abs() > 1e-3), "a ramp, not a click");
    assert!(r.audio[2 * 8_100..].iter().all(|x| x.abs() < 1e-9), "silent after the ramp");
    let next = e.render(&doc, &r.next_snapshot);
    assert!(next.audio.iter().all(|x| x.abs() < 1e-9), "the mute persists across the loop seam");
}

/// Live mode renders into a ~170 ms ring, so the per-loop work done at the loop top (the
/// controller's pick + `begin_loop`) and the worst block must fit well inside it. Release-only timing
/// (`cargo test --release -p engines_core --lib loop_top_work`); prints the numbers.
#[test]
#[cfg_attr(debug_assertions, ignore)]
fn loop_top_work_fits_the_live_ring() {
    for (name, doc) in block_fixtures().into_iter().chain([("song_glass_garden.json5".to_string(), {
        let src = std::fs::read_to_string(crate::test_support::fixtures_dir().join("song_glass_garden.json5")).unwrap();
        let mut d = LoopDoc::from_json_str(&src).unwrap();
        d.song = None;
        d
    })]) {
        let mut eng = Engine::new(48_000.0);
        let mut ctl = crate::controller::Controller::new(doc.clone(), crate::controller::DitherConfig::default());
        ctl.set_control(crate::contract::SessionControl::from_flags("drift", true, &[], &[], 0.05).unwrap());
        let mut snap = PhaseSnapshot::empty();
        let (mut worst_top, mut worst_block, mut sum_block, mut blocks) = (0.0f64, 0.0f64, 0.0f64, 0usize);
        for i in 0..4u64 {
            let t0 = std::time::Instant::now();
            let p = ctl.next(1000 + i);
            let mut lr = eng.begin_loop(&p.doc, &snap);
            worst_top = worst_top.max(t0.elapsed().as_secs_f64());
            while !lr.is_done() {
                let t = std::time::Instant::now();
                lr.render_block(&mut eng, 1024);
                let dt = t.elapsed().as_secs_f64();
                worst_block = worst_block.max(dt);
                sum_block += dt;
                blocks += 1;
            }
            let r = lr.finish(&mut eng);
            ctl.rebase(p.doc);
            snap = r.next_snapshot;
        }
        let mean = sum_block / blocks as f64;
        eprintln!("{name}: loop-top {:.1} ms · 1024-frame blocks mean {:.2} ms, worst {:.2} ms (real time 21.3 ms)", worst_top * 1e3, mean * 1e3, worst_block * 1e3);
        // The ring (~170 ms) absorbs a spike; the average must keep up with real time with margin.
        assert!(worst_top < 0.100, "{name}: loop-top work {:.1} ms would eat the live ring", worst_top * 1e3);
        assert!(mean < 0.5 * 0.0213, "{name}: blocks average {:.2} ms — too close to real time", mean * 1e3);
        assert!(worst_block + worst_top < 0.150, "{name}: a spike of {:.1} ms would underrun a 170 ms ring", (worst_block + worst_top) * 1e3);
    }
}

// ── Binaural instruments (ADR 0047) ──────────────────────────────────────

/// One held note (`offset` semitones from A4) into a `binaural` instrument `bin`: a plain sine pair
/// (filter open, flat envelope) with `params` merged over it, at master `stereo_width` `width`.
/// `tweak` edits the JSON before it's parsed (extra instruments, routes, buses). 8 s at 60 bpm.
fn bin_doc(params: serde_json::Value, offset: i32, width: f32, tweak: impl FnOnce(&mut serde_json::Value)) -> LoopDoc {
    let mut p = serde_json::json!({ "wave": "sine", "level_db": -6.02, "cutoff_hz": 8000, "sustain": 1.0, "release_s": 0.2 });
    for (k, v) in params.as_object().unwrap() {
        p[k] = v.clone();
    }
    let mut doc = serde_json::json!({
        "schema_version": "0.9", "loop_id": 1,
        "transport": { "bpm": 60, "master_steps": 8 },
        "harmony": { "root": "A", "mode": "major" },
        "global": { "master_gain_db": 0, "stereo_width": width },
        "instruments": { "bin": { "type": "binaural", "note_engine": { "polyphony": 1 }, "params": p } },
        "modulators": { "seq": { "type": "step_seq", "output": "note", "length": 1, "steps": [ { "step": 0, "voicing": { "v": { "offset": offset } } } ] } },
        "matrix": { "m": { "source": "seq", "dest": "instruments.bin.notes", "kind": "note", "voices": 1 } }
    });
    tweak(&mut doc);
    LoopDoc::from_value(doc).unwrap()
}

/// Upward zero crossings per second of channel `ch` over frames `a..b`.
fn channel_hz(audio: &[f32], ch: usize, sr: f32, a: usize, b: usize) -> f32 {
    let n = (a + 1..b).filter(|&f| audio[2 * (f - 1) + ch] < 0.0 && audio[2 * f + ch] >= 0.0).count();
    n as f32 * sr / (b - a) as f32
}

/// A subtractive pad `pad` (A minor triad, widened by the master) sending to a reverb bus `rv` —
/// the stereo-width-and-stems company a binaural pair keeps in a real piece.
fn add_pad_and_bus(doc: &mut serde_json::Value) {
    doc["instruments"]["pad"] = serde_json::json!({ "note_engine": { "polyphony": 3 }, "sends_db": { "rv": -7.96 },
        "params": { "wave": "saw_soft", "level_db": -13.98, "cutoff_hz": 800, "sustain": 0.9, "release_s": 0.8 } });
    doc["modulators"]["chord"] = serde_json::json!({ "type": "step_seq", "output": "note", "length": 1,
        "steps": [ { "step": 0, "voicing": { "a": { "offset": 0 }, "b": { "offset": 3 }, "c": { "offset": 7 } } } ] });
    doc["matrix"]["mp"] = serde_json::json!({ "source": "chord", "dest": "instruments.pad.notes", "kind": "note", "voices": 3 });
    doc["buses"] = serde_json::json!({ "rv": { "return_gain_db": -7.96, "effects": { "verb": { "type": "reverb",
        "params": { "room": 0.6, "damping": 0.4, "shimmer": 0.0, "width": 0.8, "predelay_ms": 10.0 } } } } });
}

#[test]
fn binaural_sides_sit_at_root_minus_and_plus_half_spread() {
    let sr = 8_000.0f32;
    // The ADR's example: root A2 = 110 Hz, spread 2 Hz ⇒ left 109 Hz, right 111 Hz.
    let r = Engine::new(sr as f64).render(&bin_doc(serde_json::json!({ "spread_hz": 2.0 }), -24, 0.5, |_| {}), &PhaseSnapshot::empty());
    let (l, rr) = (channel_hz(&r.audio, 0, sr, 8_000, 64_000), channel_hz(&r.audio, 1, sr, 8_000, 64_000));
    assert!((l - 109.0).abs() < 0.3, "left {l} Hz");
    assert!((rr - 111.0).abs() < 0.3, "right {rr} Hz");
    // Hard L/R: each side holds only its own tone (a 20 Hz spread for clean FFT bins: 100 | 120).
    // ≥ 30 dB apart — what's left is the unwindowed FFT's leakage; any bleed would be ~0 dB.
    let r = Engine::new(sr as f64).render(&bin_doc(serde_json::json!({ "spread_hz": 20.0 }), -24, 0.5, |_| {}), &PhaseSnapshot::empty());
    let pl = channel_band_powers(&r.audio, 0, sr, &[100.0, 120.0], 3.0);
    let pr = channel_band_powers(&r.audio, 1, sr, &[100.0, 120.0], 3.0);
    assert!(pl[0] > 1e3 * pl[1], "left must hold 100 Hz only: {pl:?}");
    assert!(pr[1] > 1e3 * pr[0], "right must hold 120 Hz only: {pr:?}");
}

#[test]
fn binaural_bypasses_the_master_stereo_width() {
    // ADR 0047: the pair's dry is outside the mid/side stage — mono, neutral or wide, the same bits.
    let render = |w: f32| Engine::new(8_000.0).render(&bin_doc(serde_json::json!({ "spread_hz": 4.0 }), -24, w, |_| {}), &PhaseSnapshot::empty()).audio;
    let neutral = render(0.5);
    assert_eq!(render(0.0), neutral, "stereo_width 0 must not fold the pair to mono");
    assert_eq!(render(1.0), neutral, "stereo_width 1 must not widen it");
    // …while master_gain_db still scales it.
    let half = Engine::new(8_000.0)
        .render(&bin_doc(serde_json::json!({ "spread_hz": 4.0 }), -24, 0.5, |d| d["global"]["master_gain_db"] = serde_json::json!(crate::params::amp_to_db(0.5))), &PhaseSnapshot::empty())
        .audio;
    assert!(half.iter().zip(&neutral).all(|(h, n)| (h - 0.5 * n).abs() < 1e-5));
}

#[test]
fn root_hz_pins_the_root_and_the_note_only_gates() {
    let sr = 8_000.0f32;
    let pinned = |offset: i32| Engine::new(sr as f64).render(&bin_doc(serde_json::json!({ "root_hz": 110.0, "spread_hz": 2.0 }), offset, 0.5, |_| {}), &PhaseSnapshot::empty()).audio;
    let a = pinned(0);
    assert_eq!(a, pinned(7), "with root_hz set the note's pitch is ignored");
    assert!((channel_hz(&a, 0, sr, 8_000, 64_000) - 109.0).abs() < 0.3);
    assert!((channel_hz(&a, 1, sr, 8_000, 64_000) - 111.0).abs() < 0.3);
    // Without root_hz the root is the note: A2 → A3 doubles it.
    let follow = |offset: i32| Engine::new(sr as f64).render(&bin_doc(serde_json::json!({ "spread_hz": 4.0 }), offset, 0.5, |_| {}), &PhaseSnapshot::empty()).audio;
    assert!((channel_hz(&follow(-24), 0, sr, 8_000, 64_000) - 108.0).abs() < 0.3);
    assert!((channel_hz(&follow(-12), 0, sr, 8_000, 64_000) - 218.0).abs() < 0.3);
}

#[test]
fn an_idle_route_reads_the_authored_hz_exactly_and_an_unset_root_ignores_its_route() {
    // An Hz param under a zero-depth route renders bit-identically to the unrouted one (no
    // normalize round trip: 109.5 stays 109.5); a route into an absent root_hz does nothing.
    let lfo = |dest: &'static str, depth: f32| {
        move |d: &mut serde_json::Value| {
            d["modulators"]["wob"] = serde_json::json!({ "type": "lfo", "output": "control", "wave": "sine", "sync": "free", "rate_hz": 0.5, "polarity": "bipolar" });
            // `depth` is a fraction of the param's range, written in its Hz.
            d["matrix"]["mw"] = serde_json::json!({ "source": "wob", "dest": dest, "kind": "mod", "depth_hz": depth * crate::loop_doc::depth_span_of(dest) });
        }
    };
    let render = |d: &LoopDoc| Engine::new(8_000.0).render(d, &PhaseSnapshot::empty()).audio;
    let pinned = serde_json::json!({ "root_hz": 109.5, "spread_hz": 2.5 });
    let plain = render(&bin_doc(pinned.clone(), -24, 0.5, |_| {}));
    assert_eq!(render(&bin_doc(pinned.clone(), -24, 0.5, lfo("instruments.bin.params.root_hz", 0.0))), plain);
    assert_eq!(render(&bin_doc(pinned.clone(), -24, 0.5, lfo("instruments.bin.params.spread_hz", 0.0))), plain);
    // a real depth does move it
    assert_ne!(render(&bin_doc(pinned, -24, 0.5, lfo("instruments.bin.params.spread_hz", 0.2))), plain);
    let following = serde_json::json!({ "spread_hz": 2.0 });
    assert_eq!(
        render(&bin_doc(following.clone(), -24, 0.5, lfo("instruments.bin.params.root_hz", 0.3))),
        render(&bin_doc(following, -24, 0.5, |_| {})),
        "a route into an unset root_hz must not move the note-following root"
    );
}

#[test]
fn the_binaural_octave_pedal_beats_at_the_same_spread() {
    // shift +12, full wet: 110 → 220 Hz, still ± 10 Hz ⇒ left 210, right 230.
    let sr = 8_000.0f32;
    let r = Engine::new(sr as f64).render(&bin_doc(serde_json::json!({ "root_hz": 110.0, "spread_hz": 20.0, "shift_st": 12, "shift_mix": 1.0 }), 0, 0.5, |_| {}), &PhaseSnapshot::empty());
    assert!((channel_hz(&r.audio, 0, sr, 8_000, 64_000) - 210.0).abs() < 0.5);
    assert!((channel_hz(&r.audio, 1, sr, 8_000, 64_000) - 230.0).abs() < 0.5);
}

#[test]
fn binaural_voice_state_round_trips_and_resumes() {
    let d = binaural_fixture();
    let mut e = Engine::new(16_000.0);
    let snap = e.render(&d, &PhaseSnapshot::empty()).next_snapshot;
    let j = serde_json::to_string(&snap).unwrap();
    assert!(j.contains(r#""kind":"binaural""#), "the right side's phases are carried: {j}");
    let back: PhaseSnapshot = serde_json::from_str(&j).unwrap();
    assert_eq!(back, snap);
    assert_eq!(e.clone().render(&d, &back).audio, e.render(&d, &snap).audio, "the next loop resumes bit-identically");
}

#[test]
fn a_binaural_source_stem_is_gain_only_and_the_sources_still_sum_to_the_master() {
    // ADR 0030 + 0047: width 0 folds the pad's stem to mono but leaves the pair's stem hard L/R.
    let sr = 8_000.0f32;
    let d = bin_doc(serde_json::json!({ "spread_hz": 20.0 }), -24, 0.0, add_pad_and_bus);
    let off = Engine::new(sr as f64).render(&d, &PhaseSnapshot::empty());
    let mut eng = Engine::new(sr as f64);
    eng.set_source_capture(true);
    let r = eng.render(&d, &PhaseSnapshot::empty());
    assert_eq!(r.audio, off.audio, "the tap never touches the master");
    let src = r.sources.as_ref().unwrap();
    let bin = &src.inst["bin"];
    let pl = channel_band_powers(bin, 0, sr, &[100.0, 120.0], 3.0);
    assert!(pl[0] > 1e3 * pl[1], "the pair's left stem holds only its own 100 Hz: {pl:?}");
    let mut worst = 0.0f32;
    for i in 0..r.audio.len() {
        let sum: f32 = src.inst.values().chain(src.bus.values()).map(|b| b[i]).sum();
        worst = worst.max((sum - r.audio[i]).abs());
    }
    assert!(worst < 1e-5, "Σ sources must reconstruct the master: worst diff {worst}");
}

#[test]
fn a_binaural_out_channel_stem_reconstructs_the_master() {
    // ADR 0027 + 0047: routed to pair 3/4, the pair's dry joins its stem after the width stage.
    let sr = 8_000.0f32;
    let d = bin_doc(serde_json::json!({ "spread_hz": 20.0 }), -24, 0.0, |doc| {
        add_pad_and_bus(doc);
        doc["instruments"]["bin"]["out_channel"] = serde_json::json!(3);
    });
    let off = Engine::new(sr as f64).render(&d, &PhaseSnapshot::empty());
    let mut eng = Engine::new(sr as f64);
    eng.set_stem_capture(true);
    let r = eng.render(&d, &PhaseSnapshot::empty());
    assert_eq!(r.audio, off.audio, "the tap never touches the master");
    let sm = r.stems.as_ref().unwrap();
    let stem = &sm.stems[&3];
    let pl = channel_band_powers(stem, 0, sr, &[100.0, 120.0], 3.0);
    assert!(pl[0] > 1e3 * pl[1], "the routed pair stays hard L/R in its stem: {pl:?}");
    let mut worst = 0.0f32;
    for i in 0..r.audio.len() {
        worst = worst.max((sm.rest[i] + stem[i] - r.audio[i]).abs());
    }
    assert!(worst < 1e-5, "rest + stem must reconstruct the master: worst diff {worst}");
}

#[test]
fn retyping_a_voice_to_binaural_mid_loop_splits_it_hard_left_and_right() {
    let sub = bin_doc(serde_json::json!({ "spread_hz": 20.0 }), -24, 0.5, |d| {
        d["instruments"]["bin"].as_object_mut().unwrap().remove("type");
    });
    let bin = bin_doc(serde_json::json!({ "spread_hz": 20.0 }), -24, 0.5, |_| {});
    let (r, kind) = edit_mid_loop(&sub, &bin, 8_000);
    assert_eq!(kind, EditKind::Structural, "a synth-type change re-strikes");
    assert!(r.audio.iter().all(|x| x.is_finite()));
    let after = &r.audio[2 * 16_000..];
    assert!(after.chunks(2).any(|f| (f[0] - f[1]).abs() > 1e-3), "after the edit L and R differ");
    assert!((channel_hz(&r.audio, 0, 8_000.0, 16_000, 64_000) - 100.0).abs() < 0.5);
}

