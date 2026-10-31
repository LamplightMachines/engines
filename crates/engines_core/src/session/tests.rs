use super::*;

use crate::test_support::fixture;

// Low SR keeps the (debug-build) render fast; the streaming/draining behavior is SR-agnostic.
const TEST_SR: f64 = 8_000.0;

/// A doc whose `loop_id` is this makes the render worker panic at startup (test builds only).
pub(super) const PANIC_LOOP_ID: u64 = u64::MAX - 7;

/// Drain a bounded session like an audio device until it finishes (wall-clock guarded), returning
/// the interleaved audio pulled.
fn drain_bounded(s: &mut Session) -> Vec<f32> {
    let mut audio = Vec::new();
    let mut buf = [0.0f32; 8192];
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let n = s.pull_audio(&mut buf);
        if n > 0 {
            audio.extend_from_slice(&buf[..n]);
        } else if s.finished() {
            break;
        } else {
            assert!(Instant::now() < deadline, "timed out draining session");
            thread::sleep(Duration::from_millis(1));
        }
    }
    audio
}

#[test]
fn session_streams_audio_and_records_then_finishes() {
    let d = fixture();
    let total = d.total_frames(TEST_SR);
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(2), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 11, cat_change_prob: 0.0, ..Default::default() },
        None,
    );
    let audio = drain_bounded(&mut s);
    assert_eq!(audio.len(), total * 2 * 2, "drained 2 loops of stereo audio");
    let recs = s.poll_records();
    assert_eq!(recs.len(), 2, "one record per loop");
    assert_eq!((recs[0].start_frame, recs[1].start_frame), (0, total as u64), "records tile the stream");
    assert_eq!(recs[1].loop_id, recs[0].loop_id + 1);
    let peak = audio.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert_eq!(recs.iter().map(|r| r.peak).fold(0.0f32, f32::max), peak, "record peaks match the audio");
    assert!(recs.iter().all(|r| r.optimize == "drift" && !r.path.is_empty()), "drift moved a param each loop");
}

#[test]
fn include_midi_drift_moves_midi_params_too() {
    // ADR 0026: with `include_midi` on, drift picks midi-targeted paths as well as the rest.
    let mut d = fixture();
    {
        let ext = d.instruments.get_mut("poly").unwrap();
        ext.kind = Some("midi".into());
        ext.routing = Some(crate::loop_doc::MidiRouting {
            channel: 1,
            cc: std::collections::BTreeMap::from([("cutoff".to_string(), 74u8)]),
        });
    }
    // Pin the other instruments so the seeded drift walk reliably hits midi-targeted picks
    // within a short bounded run (globals/depths/bus params stay in as non-midi controls).
    for (name, inst) in d.instruments.iter_mut() {
        if name != "poly" {
            inst.dither_disable = true;
        }
    }
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(12), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 5, cat_change_prob: 0.0, include_midi: true, ..Default::default() },
        None, // drift
    );
    drain_bounded(&mut s);
    let recs = s.poll_records();
    let targets_midi = |p: &str| p.starts_with("instruments.poly.params.") || p.ends_with(".depth");
    assert!(recs.iter().any(|r| r.delta != 0.0 && targets_midi(&r.path)), "seeded drift over 12 loops moves a midi-targeted path");
    assert!(recs.iter().any(|r| r.delta != 0.0 && !targets_midi(&r.path)), "non-midi controls still drift");
}

// WS1.4: telemetry reports the live operating point and the optimize label.
#[test]
fn telemetry_reports_operating_point_and_mode() {
    let mut s = Session::start(
        fixture(),
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(3), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 5, cat_change_prob: 0.0, ..Default::default() },
        SessionControl::from_flags("hold", false, &[], &[], 0.05).unwrap(),
    );
    drain_bounded(&mut s);
    let t = s.poll_telemetry().expect("telemetry populated after rendering");
    assert_eq!(t.optimize, "hold");
    assert_eq!(t.operating_point["schema_version"], "0.9", "operating point is the doc mirror");
}

// WS1.1: a runtime apply_control lands at a loop boundary and its revision is recorded per loop.
// Starts as plain drift (no control), then a constraint edit.
#[test]
fn apply_control_takes_effect_and_records_revision() {
    let mut s = Session::start(
        fixture(),
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(8), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() },
        None,
    );
    let mut control = SessionControl { revision: 42, ..Default::default() };
    control.constraints.insert(
        "global.master_gain_db".into(),
        crate::loop_doc::ConstraintBand { min: Some(0.5), max: Some(0.6) },
    );
    s.apply_control(control); // taken by the worker at the next loop boundary
    drain_bounded(&mut s);
    let recs = s.poll_records();
    assert!(recs.iter().any(|r| r.revision == 42), "the applied control's revision (42) is recorded once it lands");
}

/// Route the fixture's steady "poly" pad to an `out_channel` (ADR 0027 session tests).
fn doc_with_out_channel(oc: u16) -> LoopDoc {
    let mut v = serde_json::to_value(fixture()).unwrap();
    v["instruments"]["poly"]["out_channel"] = serde_json::json!(oc);
    LoopDoc::from_value(v).unwrap()
}

#[test]
fn multichannel_session_streams_composed_stem_frames() {
    // ADR 0027: with `out_channels: 4` and an instrument routed to pair 3/4, the ring must
    // carry 4-wide device frames with energy on BOTH the main pair (rest of the mix) and the
    // stem pair. An `out_channel` beyond the device width folds back into 1/2.
    let run = |oc: u16| -> (f64, f64) {
        let mut s = Session::start(
            doc_with_out_channel(oc),
            SessionConfig { sample_rate: TEST_SR, max_loops: Some(2), out_channels: 4, ..Default::default() },
            DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() },
            None,
        );
        assert_eq!(s.channels(), 4, "session frames are device-wide");
        let audio = drain_bounded(&mut s);
        assert_eq!(audio.len() % 4, 0, "whole 4-channel frames");
        let (mut main_e, mut stem_e) = (0.0f64, 0.0f64);
        for f in audio.chunks_exact(4) {
            main_e += (f[0] as f64).powi(2) + (f[1] as f64).powi(2);
            stem_e += (f[2] as f64).powi(2) + (f[3] as f64).powi(2);
        }
        (main_e, stem_e)
    };
    // Pair 3/4 fits a 4-channel stream: the routed instrument sounds there, the rest on 1/2.
    let (main_e, stem_e) = run(3);
    assert!(main_e > 1e-3, "rest of the mix sounds on 1/2: {main_e}");
    assert!(stem_e > 1e-3, "routed instrument sounds on its 3/4 stem: {stem_e}");
    // Pair 11/12 does NOT fit: the stem folds back into 1/2 (the music stays whole).
    let (main_fold, stem_fold) = run(11);
    assert!(stem_fold < 1e-12, "an unfittable stem leaves its pair silent: {stem_fold}");
    assert!(main_fold > main_e * 0.5, "the folded stem sounds on 1/2 instead: {main_fold}");
}

#[test]
fn stereo_session_is_unchanged_by_out_channel_routes() {
    // Default `out_channels: 2` must stream the master mix verbatim — same frame count and
    // nonzero audio — regardless of `out_channel` routes in the doc (they only take effect
    // on a multichannel device).
    let mut s = Session::start(
        doc_with_out_channel(3),
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(1), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() },
        None,
    );
    assert_eq!(s.channels(), 2);
    let audio = drain_bounded(&mut s);
    assert!(audio.iter().any(|&x| x != 0.0), "stereo playback unaffected");
}

#[test]
fn live_edit_renders_audio_but_never_drifts() {
    // LiveEdit (ADR 0024): audio still flows, but the controller never moves a param itself.
    let ctrl = SessionControl::from_flags("live_edit", false, &[], &[], 0.05).unwrap();
    assert!(ctrl.is_some(), "live_edit must produce a control");
    let mut s = Session::start(
        fixture(),
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(2), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() },
        ctrl,
    );
    let audio = drain_bounded(&mut s);
    assert!(audio.iter().any(|&x| x != 0.0), "LiveEdit must still produce audible audio");
    let recs = s.poll_records();
    assert_eq!(recs.len(), 2);
    assert!(recs.iter().all(|r| r.path.is_empty() && r.delta == 0.0 && r.optimize == "live_edit"), "LiveEdit never drifts: {recs:?}");
}

#[test]
fn live_edit_apply_doc_structural_swap_keeps_audio_finite() {
    // A structural live-edit (an instrument removed) takes the standby-engine crossfade path; it
    // must not panic and audio must stay finite/non-silent across the transition.
    let d = fixture();
    let mut d2 = d.clone();
    let victim = d2.instruments.keys().next().cloned().unwrap();
    d2.instruments.remove(&victim);
    let ctrl = SessionControl::from_flags("live_edit", false, &[], &[], 0.05).unwrap();
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(4), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 8, cat_change_prob: 0.0, ..Default::default() },
        ctrl,
    );
    s.apply_doc(d2);
    let audio = drain_bounded(&mut s);
    assert!(audio.iter().all(|x| x.is_finite()), "structural crossfade produced non-finite audio");
    assert!(audio.iter().any(|&x| x != 0.0));
}

#[test]
fn every_records_peak_is_the_master_actually_streamed_even_across_a_crossfade() {
    // A structural swap pushes an old→new crossfade; its record must report the blend's peak (which
    // can exceed both renders), not the new render's.
    let d = fixture();
    let mut structural = d.clone();
    let victim = structural.instruments.keys().next().cloned().unwrap();
    structural.instruments.remove(&victim);
    // …and much quieter, so the crossfade's start (still mostly the old render) holds the loop's peak.
    for inst in structural.instruments.values_mut() {
        if let Some(l) = inst.params.get_mut("level_db") {
            *l = (-26.0).into();
        }
    }
    let ctrl = SessionControl::from_flags("live_edit", false, &[], &[], 0.05).unwrap();
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(4), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 8, cat_change_prob: 0.0, ..Default::default() },
        ctrl,
    );
    s.apply_doc_rev(structural, 2);
    let audio = drain_bounded(&mut s);
    let recs = s.poll_records();
    let swap = recs.iter().find(|r| r.doc_rev == 2).expect("the swap happened");
    let after = recs.iter().rev().find(|r| r.doc_rev == 2).unwrap();
    assert!(swap.peak > 1.5 * after.peak, "the crossfade loop is louder than the quiet doc alone: {recs:?}");
    for r in &recs {
        let span = &audio[2 * r.start_frame as usize..2 * (r.start_frame as usize + r.num_frames)];
        let streamed = span.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert_eq!(r.peak, streamed, "loop {} (doc_rev {})", r.loop_id, r.doc_rev);
    }
}

#[test]
fn records_carry_the_shells_doc_revision_across_swaps() {
    // engines-live (ADR 0034) numbers doc revisions; each loop record says which one it played —
    // through an in-place swap and a structural (crossfade) swap alike.
    let d = fixture();
    let mut same_ids = d.clone();
    same_ids.global.master_gain_db -= 0.92;
    let mut structural = d.clone();
    let victim = structural.instruments.keys().next().cloned().unwrap();
    structural.instruments.remove(&victim);
    let ctrl = SessionControl::from_flags("live_edit", false, &[], &[], 0.05).unwrap();
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(6), doc_rev: 7, ring_capacity: 4096, ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 8, cat_change_prob: 0.0, ..Default::default() },
        ctrl,
    );
    let mut revs = Vec::new();
    let mut buf = vec![0.0f32; 4096];
    let mut sent = 0;
    while !s.finished() {
        if s.pull_audio(&mut buf) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        for r in s.poll_records() {
            revs.push(r.doc_rev);
            // Queue the next edit once the previous one is playing.
            match (sent, r.doc_rev) {
                (0, 7) => {
                    s.apply_doc_rev(same_ids.clone(), 8);
                    sent = 1;
                }
                (1, 8) => {
                    s.apply_doc_rev(structural.clone(), 9);
                    sent = 2;
                }
                _ => {}
            }
        }
    }
    revs.extend(s.poll_records().into_iter().map(|r| r.doc_rev));
    assert_eq!(revs.first(), Some(&7), "{revs:?}");
    assert!(revs.windows(2).all(|w| w[0] <= w[1]), "revisions only move forward: {revs:?}");
    assert!(revs.contains(&8) && revs.last() == Some(&9), "{revs:?}");
}

#[test]
fn viz_stream_delivers_frames_when_enabled() {
    // The instrument-screen viz tap (ADR 0024) flows 25 Hz frames through the Session channel.
    let d = fixture();
    let inst0 = d.instruments.keys().next().cloned();
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(4), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 4, cat_change_prob: 0.0, ..Default::default() },
        None,
    );
    s.set_viz(true);
    s.set_viz_instrument(inst0);
    let _ = drain_bounded(&mut s);
    let frames = s.poll_viz();
    assert!(!frames.is_empty(), "viz produced no frames");
    assert!(frames.iter().all(|f| !f.master.is_empty()), "every viz frame carries master audio");
    // `audio_pos` (ADR 0024) must tile the played stream contiguously: each hop starts exactly
    // where the previous one ended (its `master.len()` frames later), within and across loops, so
    // the TUI can reveal a hop the instant playback reaches its position.
    for w in frames.windows(2) {
        assert_eq!(
            w[1].audio_pos,
            w[0].audio_pos + w[0].master.len() as u64,
            "viz hops are contiguous in absolute audio frames (play-cursor alignment)",
        );
    }
}

// ── MIDI-out stream (ADR 0026) ───────────────────────────────────────────

/// A doc with one `type:"midi"` instrument (channel 3 ⇒ wire 2, cutoff → CC 74) holding a
/// two-voice chord from step 0 — the session-level midi fixture.
fn midi_fixture() -> LoopDoc {
    LoopDoc::from_json_str(
        r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":240,"master_steps":4,"step_unit":"beat"},
        "harmony":{"root":"D","mode":"dorian"},
        "global":{"master_gain_db":0,"stereo_width":0.0},
        "instruments":{"ext":{"enabled":true,"pitched":true,"type":"midi",
            "note_engine":{"polyphony":4,"glide_ms":0},
            "params":{"cutoff":0.5},
            "routing":{"channel":3,"cc":{"cutoff":74}}}},
        "modulators":{"chords":{"type":"step_seq","output":"note","sync":"loop","length":16,
            "steps":[{"step":0,"voicing":{"v1":{"offset":0,"velocity":96},"v2":{"offset":7}}}]}},
        "matrix":{"m_n":{"source":"chords","dest":"instruments.ext.notes","kind":"note","voices":4,"priority":1}},
        "buses": { "rv": { "return_gain_db": -Infinity, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 } } } } }
    }"#,
    )
    .unwrap()
}

#[test]
fn session_streams_timed_midi_and_flushes_on_exit() {
    let d = midi_fixture();
    let total = d.total_frames(TEST_SR) as u64;
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(2), ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 11, cat_change_prob: 0.0, ..Default::default() },
        None,
    );
    drain_bounded(&mut s);
    let evs = s.poll_midi();
    // Loop 1 (abs 0): CC sync burst + the two Note-Ons. Loop 2: steady drone ⇒ nothing.
    // Exit flush (abs 2·total): Note-Offs for the held chord + CC 123 on the used channel.
    use crate::midi::MidiMsg::*;
    let expect: Vec<(u64, crate::midi::MidiMsg)> = vec![
        (0, Cc { ch: 2, cc: 74, value: 64 }),
        (0, NoteOn { ch: 2, note: 62, vel: 96 }),
        (0, NoteOn { ch: 2, note: 69, vel: 100 }),
        (2 * total, NoteOff { ch: 2, note: 62 }),
        (2 * total, NoteOff { ch: 2, note: 69 }),
        (2 * total, Cc { ch: 2, cc: 123, value: 0 }),
    ];
    let got: Vec<(u64, crate::midi::MidiMsg)> = evs.iter().map(|e| (e.abs_frame, e.msg)).collect();
    assert_eq!(got, expect, "loop emission + session-end safety flush");
}

/// The fixture with an `external_return` bus added (gain 0.8, zero latency).
fn return_fixture() -> LoopDoc {
    let mut d = fixture();
    d.buses.insert(
        "daw".into(),
        crate::loop_doc::Bus {
            id: "daw".into(),
            kind: Some("external_return".into()),
            return_gain_db: crate::params::amp_to_db(0.8),
            latency_ms: 0.0,
            effects: BTreeMap::new(),
        },
    );
    d
}

#[test]
fn session_reports_the_external_return_config_for_the_shell_monitor() {
    // ADR 0026 phase D: no DAW audio enters the core render — the session only surfaces the bus
    // config so the shell can monitor-mix the loopback input.
    let s = Session::start(
        return_fixture(),
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(1), ..Default::default() },
        DitherConfig::default(),
        None,
    );
    let (gain, latency) = s.external_return().unwrap();
    assert!((gain - 0.8).abs() < 1e-6 && latency == 0.0, "{gain}");
}

#[test]
fn session_stop_flushes_all_notes_off() {
    let d = midi_fixture();
    let mut s = Session::start(
        d,
        SessionConfig { sample_rate: TEST_SR, max_loops: None, ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() },
        None,
    );
    // Let at least one loop stream, then stop mid-flight (the worker may be blocked on a
    // full ring — the stop-aware push bails and must still run the exit flush).
    let mut buf = [0.0f32; 4096];
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pulled = 0usize;
    while pulled == 0 {
        pulled = s.pull_audio(&mut buf);
        assert!(Instant::now() < deadline, "no audio before stop");
        thread::sleep(Duration::from_millis(1));
    }
    s.stop();
    while !s.worker_finished() {
        assert!(Instant::now() < deadline, "worker didn't exit after stop");
        thread::sleep(Duration::from_millis(1));
    }
    let evs = s.poll_midi();
    use crate::midi::MidiMsg::*;
    let offs: Vec<_> = evs.iter().filter(|e| matches!(e.msg, NoteOff { .. })).collect();
    assert_eq!(offs.len(), 2, "both held notes flushed on stop: {evs:?}");
    assert!(
        evs.iter().any(|e| matches!(e.msg, Cc { ch: 2, cc: 123, value: 0 })),
        "All-Notes-Off (CC 123) sent on the used channel: {evs:?}"
    );
    assert!(
        matches!(evs.last().map(|e| e.msg), Some(Cc { cc: 123, .. })),
        "flush comes last: {evs:?}"
    );
}

#[test]
fn a_live_mode_stop_mid_loop_flushes_the_notes_already_sent() {
    // Live mode sends a loop's MIDI block by block: a stop mid-loop must Note-Off what this loop
    // turned on (the loop-start snapshot holds nothing yet), then CC 123.
    let mut s = Session::start(
        midi_fixture(),
        SessionConfig { sample_rate: TEST_SR, max_loops: None, live_block: Some(256), ring_capacity: 4096, ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() },
        None,
    );
    let mut buf = [0.0f32; 512];
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pulled = 0usize;
    while pulled < 2 * 1_000 {
        pulled += s.pull_audio(&mut buf);
        assert!(Instant::now() < deadline, "no audio before stop");
        thread::sleep(Duration::from_millis(1));
    }
    s.stop();
    while !s.worker_finished() {
        assert!(Instant::now() < deadline, "worker didn't exit after stop");
        thread::sleep(Duration::from_millis(1));
    }
    let evs = s.poll_midi();
    use crate::midi::MidiMsg::*;
    let ons = evs.iter().filter(|e| matches!(e.msg, NoteOn { .. })).count();
    let offs = evs.iter().filter(|e| matches!(e.msg, NoteOff { .. })).count();
    assert!(ons > 0, "the loop's notes went out before the stop: {evs:?}");
    assert!(offs >= ons, "every note sent is turned off: {evs:?}");
    assert!(matches!(evs.last().map(|e| e.msg), Some(Cc { cc: 123, .. })), "flush comes last: {evs:?}");
}

// ── ADR 0048: the MIDI mirror + the note log ─────────────────────────────────────────────────

/// `midi_fixture`'s chord on an audio instrument (`pad`, subtractive) — for the mirror.
fn mirror_fixture() -> LoopDoc {
    LoopDoc::from_json_str(
        r#"{
        "schema_version":"0.9","loop_id":1,
        "transport":{"bpm":240,"master_steps":4,"step_unit":"beat"},
        "harmony":{"root":"D","mode":"dorian"},
        "global":{"master_gain_db":0,"stereo_width":0.0},
        "instruments":{"pad":{"enabled":true,"pitched":true,
            "note_engine":{"polyphony":4,"glide_ms":0},
            "params":{"wave":"sine","level_db":-10.46,"sustain":1.0,"release_s":0.8}, "sends_db": { "rv": 0 }}},
        "modulators":{"chords":{"type":"step_seq","output":"note","sync":"loop","length":16,
            "steps":[{"step":0,"voicing":{"v1":{"offset":0,"velocity":96},"v2":{"offset":7}}}]}},
        "matrix":{"m_n":{"source":"chords","dest":"instruments.pad.notes","kind":"note","voices":4,"priority":1}},
        "buses": { "rv": { "return_gain_db": -Infinity, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5, "shimmer": 0, "width": 0.5, "predelay_ms": 0 } } } } }
    }"#,
    )
    .unwrap()
}

fn mirror_opts(note_log: bool) -> SessionOptions {
    SessionOptions { note_tap: ["pad".to_string()].into_iter().collect(), note_log, ..Default::default() }
}

fn taps_got(evs: &[TimedTap]) -> Vec<(u64, crate::midi::TapMsg)> {
    evs.iter().map(|e| (e.abs_frame, e.msg)).collect()
}

#[test]
fn a_session_streams_the_mirror_and_flushes_it_on_exit() {
    let d = mirror_fixture();
    let total = d.total_frames(TEST_SR) as u64;
    let cfg = SessionConfig { sample_rate: TEST_SR, max_loops: Some(2), ..Default::default() };
    let mut s = Session::start_with(d, cfg, DitherConfig { seed: 11, ..Default::default() }, None, mirror_opts(false)).unwrap();
    drain_bounded(&mut s);
    use crate::midi::TapMsg::*;
    let evs = s.poll_note_tap();
    assert!(evs.iter().all(|e| e.inst == "pad"));
    assert_eq!(
        taps_got(&evs),
        vec![
            (0, NoteOn { note: 62, vel: 96 }),
            (0, NoteOn { note: 69, vel: 100 }),
            (2 * total, NoteOff { note: 62 }),
            (2 * total, NoteOff { note: 69 }),
        ]
    );
    assert!(s.poll_midi().is_empty(), "the translator stream stays empty");
    // Without the option nothing is mirrored.
    let mut s = Session::start(mirror_fixture(), cfg, DitherConfig { seed: 11, ..Default::default() }, None);
    drain_bounded(&mut s);
    assert!(s.poll_note_tap().is_empty());
}

#[test]
fn the_note_log_rides_the_records_only_when_asked() {
    let cfg = SessionConfig { sample_rate: TEST_SR, max_loops: Some(2), ..Default::default() };
    let mut s = Session::start_with(mirror_fixture(), cfg, DitherConfig::default(), None, mirror_opts(true)).unwrap();
    drain_bounded(&mut s);
    let recs = s.poll_records();
    assert_eq!(recs.len(), 2);
    for r in &recs {
        let log = r.note_log.as_ref().expect("a note log per loop");
        assert_eq!(log.feeds["pad"], vec!["chords".to_string()]);
        assert_eq!(log.tiles["chords"].iter().map(|t| t.frame).collect::<Vec<_>>(), vec![0], "one pass per loop, at its top");
    }
    let mut s = Session::start_with(mirror_fixture(), cfg, DitherConfig::default(), None, mirror_opts(false)).unwrap();
    drain_bounded(&mut s);
    for r in s.poll_records() {
        assert!(r.note_log.is_none());
        assert!(!serde_json::to_string(&r).unwrap().contains("note_log"), "a record without a log serializes as before");
    }
}

#[test]
fn live_mode_mirrors_the_same_notes_as_the_whole_loop_worker() {
    let whole = SessionConfig { sample_rate: TEST_SR, max_loops: Some(3), ..Default::default() };
    let live = SessionConfig { live_block: Some(256), ring_capacity: 4096, ..whole };
    let run = |cfg| {
        let mut s = Session::start_with(phrase_doc(), cfg, DitherConfig { seed: 5, ..Default::default() }, None, all_mirrored(&phrase_doc())).unwrap();
        drain_bounded(&mut s);
        taps_got(&s.poll_note_tap())
    };
    let (a, b) = (run(whole), run(live));
    assert!(!a.is_empty());
    assert_eq!(a, b);
}

fn phrase_doc() -> LoopDoc {
    let p = crate::test_support::fixtures_dir().join("loop_phrase_pools.json");
    LoopDoc::from_json_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn all_mirrored(d: &LoopDoc) -> SessionOptions {
    SessionOptions { note_tap: d.instruments.keys().cloned().collect(), ..Default::default() }
}

#[test]
fn a_live_mode_stop_mid_loop_flushes_the_mirrored_notes() {
    let cfg = SessionConfig { sample_rate: TEST_SR, max_loops: None, live_block: Some(256), ring_capacity: 4096, ..Default::default() };
    let mut s = Session::start_with(mirror_fixture(), cfg, DitherConfig::default(), None, mirror_opts(false)).unwrap();
    let mut buf = [0.0f32; 512];
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pulled = 0usize;
    while pulled < 2 * 1_000 {
        pulled += s.pull_audio(&mut buf);
        assert!(Instant::now() < deadline, "no audio before stop");
        thread::sleep(Duration::from_millis(1));
    }
    s.stop();
    while !s.worker_finished() {
        assert!(Instant::now() < deadline, "worker didn't exit after stop");
        thread::sleep(Duration::from_millis(1));
    }
    use crate::midi::TapMsg::*;
    let evs = s.poll_note_tap();
    let ons = evs.iter().filter(|e| matches!(e.msg, NoteOn { .. })).count();
    let offs = evs.iter().filter(|e| matches!(e.msg, NoteOff { .. })).count();
    assert_eq!((ons, offs), (2, 2), "both notes sent, both turned off: {evs:?}");
}

#[test]
fn a_structural_swap_keeps_the_mirror_armed() {
    // A structural edit (an instrument added) builds a fresh engine; it must mirror too: the new
    // doc's pad chord (G4 + D5) goes out.
    let mut b = mirror_fixture();
    let pad = b.instruments["pad"].clone();
    b.instruments.insert("pad2".into(), pad);
    let chords = b.modulators.get_mut("chords").unwrap();
    for v in chords.steps[0].voicing.values_mut() {
        v.offset += 5.0;
    }
    let cfg = SessionConfig { sample_rate: TEST_SR, max_loops: Some(3), ..Default::default() };
    let mut s = Session::start_with(mirror_fixture(), cfg, DitherConfig::default(), None, mirror_opts(false)).unwrap();
    s.apply_doc(b);
    drain_bounded(&mut s);
    use crate::midi::TapMsg::*;
    let evs = s.poll_note_tap();
    assert!(evs.iter().any(|e| e.msg == NoteOn { note: 67, vel: 96 }), "the new chord is mirrored after the swap: {evs:?}");
    assert!(evs.iter().all(|e| e.inst == "pad"), "only the mirrored instrument");
}

// ── ADR 0029: songs ──────────────────────────────────────────────────────────────────────────

/// The small two-voice song doc at 1 s loops (4 beats @ 240 bpm): blocks of 2, 1, 2 loops — block 1
/// drops the drone's note route and deepens the LFO — plus a 2-loop tail. 7 loops total.
fn small_song(transition_s: f32, tail_loops: u32) -> LoopDoc {
    let mut v = crate::test_support::song_doc(serde_json::json!({
        "matrices": { "thin": { "m_drone": null, "m_wob": { "depth_oct": 2.658 } } },
        "sequence": [ { "loops": 2 }, { "matrix": "thin", "loops": 1 }, { "loops": 2, "label": "home" } ],
        "transition_s": transition_s,
        "tail_loops": tail_loops
    }));
    v["transport"]["bpm"] = serde_json::json!(240);
    LoopDoc::from_json_str(&v.to_string()).unwrap()
}

/// The Orrery demo doc (shortened to 4 s loops) as a 3-block song.
fn orrery_song() -> LoopDoc {
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(crate::test_support::fixtures_dir().join("loop_orrery_dorian.json")).unwrap()).unwrap();
    v["transport"]["bpm"] = serde_json::json!(240);
    v["song"] = serde_json::json!({
        "matrices": { "sparse": { "m_spark": null, "m_rhy": null, "m_breath_c": { "depth_oct": 1.329 } } },
        "sequence": [ { "loops": 2 }, { "matrix": "sparse", "loops": 2 }, { "loops": 2, "seed": 9 } ],
        "transition_s": 2,
        "tail_loops": 1
    });
    LoopDoc::from_json_str(&v.to_string()).unwrap()
}

fn start_song(doc: LoopDoc, max_loops: Option<usize>, control: Option<SessionControl>) -> Session {
    Session::start(
        doc,
        SessionConfig { sample_rate: TEST_SR, max_loops, ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 21, cat_change_prob: 0.0, ..Default::default() },
        control,
    )
}

#[test]
fn a_song_plays_every_block_and_its_tail_then_ends() {
    let doc = small_song(0.5, 2);
    let loop_frames = doc.total_frames(TEST_SR);
    let mut s = start_song(doc, None, None);
    let audio = drain_bounded(&mut s);
    assert!(s.worker_finished());
    assert_eq!(audio.len(), 7 * loop_frames * 2, "2 + 1 + 2 block loops + 2 tail loops, then it ends");
    assert_eq!(s.frames_produced(), 7 * loop_frames as u64);
    let t = s.poll_telemetry().unwrap().song.expect("song telemetry");
    assert_eq!((t.loop_in_song, t.total_loops, t.blocks, t.tail, t.label.as_str()), (6, 7, 3, true, "tail"));
    assert!((t.total_s - 7.0).abs() < 1e-9);
    let recs = s.poll_records();
    assert_eq!(recs.len(), 7, "one record per loop, the tail included");
    let blocks: Vec<u32> = recs.iter().map(|r| r.song.unwrap().block).collect();
    assert_eq!(blocks, vec![0, 0, 1, 2, 2, 2, 2], "the tail reports the last block");
    // Seam loops (each block start after the first, and the tail's start, which glides the dropped
    // note routes out) are flagged; 0.5 s < one loop ⇒ just one loop each.
    let seams: Vec<bool> = recs.iter().map(|r| r.song.unwrap().seam).collect();
    assert_eq!(seams, vec![false, false, true, true, false, true, false]);
    // How far into its block each loop is (ADR 0040: the panel's structure-bar playhead).
    let pos: Vec<(u32, u32, bool)> = recs.iter().map(|r| r.song.map(|t| (t.loop_in_block, t.block_loops, t.tail)).unwrap()).collect();
    assert_eq!(pos, vec![(0, 2, false), (1, 2, false), (0, 1, false), (0, 2, false), (1, 2, false), (0, 2, true), (1, 2, true)]);
    assert!(recs[5..].iter().all(|r| r.path.is_empty() && r.delta == 0.0), "the tail never moves");
    assert!(recs.windows(2).all(|w| w[1].loop_id == w[0].loop_id + 1));
    // The tail really decays: the last 0.25 s is near-silent (0.3 release ⇒ 1.2 s tail).
    let tail = &audio[audio.len() - (loop_frames / 2)..];
    assert!(tail.iter().all(|x| x.abs() < 1e-3), "tail rang out");
}

#[test]
fn max_loops_can_only_cut_a_song_short() {
    let doc = small_song(0.5, 2);
    let lf = doc.total_frames(TEST_SR);
    let mut s = start_song(doc.clone(), Some(3), None);
    assert_eq!(drain_bounded(&mut s).len(), 3 * lf * 2);
    let mut s = start_song(doc, Some(100), None);
    assert_eq!(drain_bounded(&mut s).len(), 7 * lf * 2, "a longer limit doesn't extend the song");
}

#[test]
fn a_one_block_song_is_bit_identical_to_a_plain_session() {
    // Block 0 keeps the session seed and the song adds nothing until a seam or the tail.
    let one = small_song(0.0, 0);
    let mut v = serde_json::to_value(&one).unwrap();
    v["song"] = serde_json::json!({ "sequence": [ { "loops": 3 } ], "transition_s": 0, "tail_loops": 0 });
    let song: LoopDoc = LoopDoc::from_json_str(&v.to_string()).unwrap();
    let mut plain = song.clone();
    plain.song = None;
    let run = |d: LoopDoc, max: Option<usize>| {
        let mut s = start_song(d, max, None);
        let audio = drain_bounded(&mut s);
        let moves: Vec<(String, f32)> = s.poll_records().into_iter().map(|r| (r.path, r.delta)).collect();
        (audio, moves)
    };
    let (a, moves_a) = run(song, None);
    let (b, moves_b) = run(plain, Some(3));
    assert_eq!(a, b, "bit-identical audio");
    assert_eq!(moves_a, moves_b, "identical drift trajectory");
}

#[test]
fn a_one_block_song_with_control_equals_a_session_with_that_control() {
    // ADR 0055: the block's control is applied before anything plays, so a one-block song carrying
    // one renders bit-identically to a plain session started with the merged control.
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(crate::test_support::fixtures_dir().join("loop_orrery_dorian.json")).unwrap()).unwrap();
    v["transport"]["bpm"] = serde_json::json!(240);
    v["movement"] = serde_json::json!({ "orrery": true, "phrase_loops": 1 });
    let cutoff = "instruments.mono.params.cutoff_hz";
    let mut song = v.clone();
    song["song"] = serde_json::json!({
        "sequence": [ { "loops": 6, "control": { "movement": { "phrase_loops": 2, "tension_target": 0.2 }, "max_drift": 0.05, "drift": { cutoff: "hold" } } } ],
        "transition_s": 0, "tail_loops": 0
    });
    let song = LoopDoc::from_json_str(&song.to_string()).unwrap();
    let held = song.get_param(cutoff).expect("the fixture's pad has a cutoff");
    let mut plain = v;
    plain["movement"] = serde_json::json!({ "orrery": true, "phrase_loops": 2, "tension_target": 0.2 });
    plain["max_drift"] = serde_json::json!(0.05);
    plain["constraints"] = serde_json::json!({ cutoff: { "min": held, "max": held } });
    let plain = LoopDoc::from_json_str(&plain.to_string()).unwrap();
    let run = |d: LoopDoc| {
        let mut s = start_song(d, Some(6), None);
        let audio = drain_bounded(&mut s);
        let recs = s.poll_records();
        (audio, recs.into_iter().map(|r| (r.path, r.delta, r.harmonic_move.map(|m| m.chord))).collect::<Vec<_>>())
    };
    let (a, ma) = run(song);
    let (b, mb) = run(plain);
    assert!(ma.iter().any(|m| m.2.is_some()), "the Orrery moved");
    assert!(ma.iter().all(|m| m.0 != cutoff || m.1 == 0.0), "the held cutoff never steps");
    assert_eq!(ma, mb, "same moves");
    assert!(a == b, "bit-identical audio");
}

#[test]
fn an_orrery_off_block_plays_home_and_the_next_block_voyages_again() {
    let mut v = serde_json::to_value(orrery_song()).unwrap();
    v["song"]["sequence"][1]["control"] = serde_json::json!({ "movement": { "orrery": false } });
    let doc = LoopDoc::from_value(v).unwrap();
    let control = SessionControl::from_flags("drift", true, &[], &[], 0.05).unwrap();
    let mut s = start_song(doc.clone(), None, control.clone());
    let audio = drain_bounded(&mut s);
    assert!(audio.iter().all(|x| x.is_finite()));
    let recs = s.poll_records();
    let block = |r: &LoopRecord| r.song.unwrap().block;
    let in_block = |b: u32| recs.iter().filter(move |r| block(r) == b && !r.song.unwrap().tail);
    assert!(in_block(0).all(|r| r.orrery.is_some()), "block 0 voyages");
    assert!(in_block(1).all(|r| r.orrery.is_none() && r.harmonic_move.is_none()), "block 1 holds the home harmony");
    assert!(in_block(2).all(|r| r.orrery.is_some()), "block 2 brings the voyage back");
    assert!(in_block(2).next().unwrap().orrery.as_ref().unwrap().loops_since_home <= 1, "a fresh voyage from home at the block start");
    // The seams in and out glide: no louder than the same song without the control (the fixture
    // itself isn't gain-staged for songs), and no sample step bigger than block 0's own.
    let peak = |a: &[f32]| a.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let mut base = start_song(orrery_song(), None, SessionControl::from_flags("drift", true, &[], &[], 0.05).unwrap());
    let base = drain_bounded(&mut base);
    assert!(peak(&audio) <= peak(&base) + 1e-3, "the Orrery-off block is no louder: {} vs {}", peak(&audio), peak(&base));
    let lf = doc.total_frames(TEST_SR);
    let max_step = |loops: std::ops::Range<usize>| audio[loops.start * lf * 2..loops.end * lf * 2].chunks_exact(2).map(|f| f[0]).collect::<Vec<_>>().windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    let (inside, seams) = (max_step(0..2), max_step(2..3).max(max_step(4..5)));
    assert!(seams <= inside * 1.5 + 0.01, "a seam clicks: step {seams} vs {inside} inside block 0");
    // Deterministic, like any song.
    let mut again = start_song(doc.clone(), None, control);
    assert!(drain_bounded(&mut again) == audio, "bit-reproducible");
    // Auditioning the block from its start (`--play-from`) applies its control too.
    let mut from = Session::start(
        doc,
        SessionConfig { sample_rate: TEST_SR, song_start_block: 1, ..Default::default() },
        DitherConfig { max_change_per_loop: 0.05, seed: 21, cat_change_prob: 0.0, ..Default::default() },
        SessionControl::from_flags("drift", true, &[], &[], 0.05).unwrap(),
    );
    drain_bounded(&mut from);
    let recs = from.poll_records();
    assert!(recs.iter().filter(|r| r.song.unwrap().block == 1).all(|r| r.orrery.is_none()), "the audition starts with the Orrery off");
    assert!(recs.iter().any(|r| r.song.unwrap().block == 2 && r.orrery.is_some()));
}

#[test]
fn songs_are_deterministic_in_drift_and_with_the_orrery() {
    let drift = || {
        let mut s = start_song(small_song(0.5, 1), None, None);
        drain_bounded(&mut s)
    };
    assert_eq!(drift(), drift(), "drift song is bit-reproducible");

    let drift_orrery = || {
        let control = SessionControl::from_flags("drift", true, &[], &[], 0.05).unwrap();
        let mut s = start_song(orrery_song(), None, control);
        let audio = drain_bounded(&mut s);
        let moves: Vec<String> = s.poll_records().into_iter().filter_map(|r| r.harmonic_move.map(|m| m.chord)).collect();
        (audio, moves)
    };
    let (a, ca) = drift_orrery();
    let (b, cb) = drift_orrery();
    assert!(!ca.is_empty(), "the Orrery moved");
    assert_eq!(ca, cb, "same voyages");
    assert_eq!(a, b, "drift + Orrery song is bit-reproducible (CE-R7.3)");
}

#[test]
fn live_edit_swaps_are_ignored_during_a_song() {
    let doc = small_song(0.5, 1);
    let baseline = {
        let mut s = start_song(doc.clone(), None, None);
        drain_bounded(&mut s)
    };
    let mut s = start_song(doc.clone(), None, None);
    let mut other = doc.clone();
    other.song = None;
    other.global.master_gain_db = -14.0;
    s.apply_doc(other);
    assert_eq!(drain_bounded(&mut s), baseline, "the song owns the doc");
}

#[test]
fn song_telemetry_tracks_blocks_and_seams() {
    // Poll telemetry as the worker advances and record what each loop reported; the last-written
    // value must match the final loop.
    let mut s = start_song(small_song(0.5, 1), None, None);
    let mut seen: std::collections::BTreeMap<usize, crate::telemetry::SongTelemetry> = Default::default();
    let mut buf = [0.0f32; 512];
    let deadline = Instant::now() + Duration::from_secs(120);
    while !s.finished() {
        if s.pull_audio(&mut buf) == 0 {
            thread::sleep(Duration::from_micros(200));
        }
        if let Some(t) = s.poll_telemetry().and_then(|t| t.song) {
            seen.insert(t.loop_in_song, t);
        }
        assert!(Instant::now() < deadline);
    }
    let last = &seen[&5];
    assert_eq!((last.tail, last.block, last.loop_in_block), (true, 2, 0));
    if let Some(t) = seen.get(&2) {
        assert_eq!((t.block, t.label.as_str(), t.seam, t.loop_in_block, t.block_loops), (1, "thin", true, 0, 1));
    }
    if let Some(t) = seen.get(&3) {
        assert_eq!((t.block, t.label.as_str(), t.seam), (2, "home", true));
    }
}

// ── ADR 0030: per-source stem frames ───────────────────────────────────────────────────────────

fn stem_cfg(max_loops: usize, source_stems: bool) -> SessionConfig {
    SessionConfig { sample_rate: TEST_SR, max_loops: Some(max_loops), source_stems, ..Default::default() }
}

fn stem_dither() -> DitherConfig {
    DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() }
}

#[test]
fn source_stem_session_streams_the_master_then_one_pair_per_source() {
    let doc = fixture();
    let layout = crate::engine::source_layout(&doc);
    assert!(layout.len() >= 3, "fixture has several instruments + a bus: {layout:?}");
    // `out_channels` is overridden by `source_stems`.
    let cfg = SessionConfig { out_channels: 4, ..stem_cfg(2, true) };
    let mut s = Session::start(doc.clone(), cfg, stem_dither(), None);
    let w = 2 + 2 * layout.len();
    assert_eq!(s.channels() as usize, w);
    assert_eq!(s.source_layout(), Some(layout.as_slice()));
    let frames = drain_bounded(&mut s);
    assert_eq!(frames.len() % w, 0, "whole {w}-wide frames");

    let mut plain = Session::start(doc, stem_cfg(2, false), stem_dither(), None);
    assert_eq!(plain.source_layout(), None);
    let stereo = drain_bounded(&mut plain);
    assert_eq!(stereo.len(), frames.len() / w * 2);

    let mut worst = 0.0f32;
    let mut pair_energy = vec![0.0f64; layout.len()];
    for (f, fr) in frames.chunks_exact(w).enumerate() {
        assert_eq!([fr[0], fr[1]], [stereo[2 * f], stereo[2 * f + 1]], "frame {f}: the master pair is the stereo session, bit for bit");
        for ch in 0..2 {
            let sum: f32 = (0..layout.len()).map(|i| fr[2 + 2 * i + ch]).sum();
            worst = worst.max((sum - fr[ch]).abs());
        }
        for (i, e) in pair_energy.iter_mut().enumerate() {
            *e += (fr[2 + 2 * i] as f64).powi(2) + (fr[3 + 2 * i] as f64).powi(2);
        }
    }
    assert!(worst < 1e-5, "the source pairs rebuild the master: worst {worst}");
    assert!(pair_energy.iter().filter(|&&e| e > 1e-6).count() >= 2, "several sources carry signal: {pair_energy:?}");
}

#[test]
fn source_stem_song_keeps_its_layout_and_the_tail_rings_on_the_bus() {
    let mut v = serde_json::to_value(small_song(0.5, 2)).unwrap();
    // `small_song` is structural (silent: no polyphony, and dry) — give it voices and a reverb bus
    // they send into, so there's something to ring out.
    v["instruments"]["pad"]["note_engine"]["polyphony"] = serde_json::json!(2);
    v["instruments"]["drone"]["note_engine"]["polyphony"] = serde_json::json!(1);
    v["instruments"]["pad"]["sends_db"] = serde_json::json!({ "rv": 0 });
    v["instruments"]["drone"]["sends_db"] = serde_json::json!({ "rv": 0 });
    v["buses"] = serde_json::json!({ "rv": { "return_gain_db": -6.02, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5, "damping": 0.5 } } } } });
    let doc = LoopDoc::from_value(v).unwrap();
    let loop_frames = doc.total_frames(TEST_SR);
    let mut s = Session::start(doc, SessionConfig { sample_rate: TEST_SR, source_stems: true, ..Default::default() }, stem_dither(), None);
    let keys: Vec<String> = s.source_layout().unwrap().iter().map(|k| k.to_string()).collect();
    assert_eq!(keys, ["inst:drone", "inst:pad", "bus:rv"]);
    let frames = drain_bounded(&mut s);
    assert_eq!(frames.len(), 7 * loop_frames * 8, "every block + the tail, 8-wide");
    let tail_bus: f64 = frames.chunks_exact(8).skip(5 * loop_frames).map(|f| (f[6] as f64).powi(2) + (f[7] as f64).powi(2)).sum();
    assert!(tail_bus > 1e-6, "the reverb rings out on its own pair through the tail: {tail_bus}");
}

#[test]
fn source_stem_session_survives_a_structural_live_edit() {
    let doc = fixture();
    let n = crate::engine::source_layout(&doc).len();
    let mut s = Session::start(doc.clone(), stem_cfg(4, true), stem_dither(), None);
    // Add an instrument (structural ⇒ standby engine + crossfade). It isn't in the start-time
    // layout, so its dry reaches the master pair only; the width never changes.
    let mut v = serde_json::to_value(&doc).unwrap();
    v["instruments"]["extra"] = serde_json::json!({ "params": { "wave": "sine", "level_db": -10.46, "sustain": 1.0 } });
    s.apply_doc(LoopDoc::from_value(v).unwrap());
    let frames = drain_bounded(&mut s);
    assert_eq!(s.channels() as usize, 2 + 2 * n);
    assert_eq!(frames.len() % (2 + 2 * n), 0);
    assert!(frames.iter().all(|x| x.is_finite()));
}

// ── ADR 0044: routed device frames ─────────────────────────────────────────────────────────────

/// Pull `n` interleaved samples from a running session (wall-clock guarded).
fn pull_n(s: &mut Session, n: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; n];
    let mut got = 0;
    let deadline = Instant::now() + Duration::from_secs(120);
    while got < n {
        let k = s.pull_audio(&mut out[got..]);
        got += k;
        if k == 0 {
            assert!(!s.finished(), "session ended after {got} of {n} samples");
            assert!(Instant::now() < deadline, "timed out pulling");
            thread::sleep(Duration::from_millis(1));
        }
    }
    out
}

/// Energy of stereo pair `pair` across `w`-wide frames.
fn pair_energy(frames: &[f32], w: usize, pair: usize) -> f64 {
    frames.chunks_exact(w).map(|f| (f[2 * pair] as f64).powi(2) + (f[2 * pair + 1] as f64).powi(2)).sum()
}

#[test]
fn a_sequential_routing_fills_pairs_from_1_2_and_validates() {
    let keys = [SourceKey::Inst("a".into()), SourceKey::Inst("b".into()), SourceKey::Bus("rv".into())];
    let r = SourceRouting::sequential(&keys, 8);
    assert_eq!(r.slots, [Some(keys[0].clone()), Some(keys[1].clone()), Some(keys[2].clone()), None]);
    assert_eq!(r.residual, 3, "the first empty pair is the overflow");
    assert!(r.validate().is_ok());
    let full = SourceRouting::sequential(&keys, 4);
    assert_eq!((full.slots.len(), full.residual), (2, 1), "a full device shares its last pair");

    let bad = |r: SourceRouting| r.validate().unwrap_err();
    assert!(bad(SourceRouting { width: 5, ..r.clone() }).contains("even"));
    assert!(bad(SourceRouting { slots: r.slots[..2].to_vec(), ..r.clone() }).contains("slots"));
    assert!(bad(SourceRouting { residual: 4, ..r.clone() }).contains("residual"));
    let twice = SourceRouting { slots: vec![Some(keys[0].clone()), Some(keys[0].clone()), None, None], ..r.clone() };
    assert!(bad(twice).contains("two pairs"));
    assert!(Session::start_routed(fixture(), stem_cfg(1, false), stem_dither(), None, SourceRouting { residual: 9, ..r }).is_err());
}

#[test]
fn a_routed_session_puts_each_source_on_its_pair_and_the_pairs_sum_to_the_master() {
    let doc = fixture();
    let keys = crate::engine::source_layout(&doc);
    let n = keys.len();
    let w = 2 * (n + 1); // every source + an overflow pair
    let routing = SourceRouting::sequential(&keys, w);
    let mut routed = Session::start_routed(doc.clone(), stem_cfg(2, false), stem_dither(), None, routing.clone()).unwrap();
    assert_eq!((routed.channels() as usize, routed.routing(), routed.source_layout()), (w, Some(&routing), None));
    let frames = drain_bounded(&mut routed);

    // The ADR 0030 stem layout of the same run: master on 1/2, then the sources in the same order.
    let mut stems = Session::start(doc, stem_cfg(2, true), stem_dither(), None);
    let sw = 2 + 2 * n;
    let reference = drain_bounded(&mut stems);
    assert_eq!(frames.len() / w, reference.len() / sw, "same frame count");

    let mut worst = 0.0f32;
    for (fr, rf) in frames.chunks_exact(w).zip(reference.chunks_exact(sw)) {
        assert_eq!(&fr[..2 * n], &rf[2..], "every slotted pair is its stem, bit for bit");
        assert_eq!([fr[2 * n], fr[2 * n + 1]], [0.0, 0.0], "an idle overflow pair is exactly silent");
        for ch in 0..2 {
            let sum: f32 = (0..=n).map(|p| fr[2 * p + ch]).sum();
            worst = worst.max((sum - rf[ch]).abs());
        }
    }
    assert!(worst < 1e-5, "the pairs rebuild the master: worst {worst}");
}

#[test]
fn live_mode_routes_exactly_like_the_whole_loop_worker() {
    let doc = fixture();
    let routing = SourceRouting::sequential(&crate::engine::source_layout(&doc), 16);
    let mut whole = Session::start_routed(doc.clone(), stem_cfg(3, false), stem_dither(), None, routing.clone()).unwrap();
    let mut live = Session::start_routed(doc, live_cfg(3), stem_dither(), None, routing).unwrap();
    let (a, b) = (drain_bounded(&mut whole), drain_bounded(&mut live));
    assert_eq!(a.len(), b.len());
    assert!(a == b, "block-wise routing in the live worker changed the frames");
}

#[test]
fn a_source_past_the_device_rides_the_shared_last_pair() {
    let doc = fixture();
    let keys = crate::engine::source_layout(&doc);
    assert!(keys.len() >= 3, "needs more sources than the 2-pair device: {keys:?}");
    let routing = SourceRouting::sequential(&keys, 4); // keys[0] on 1/2, keys[1] + the rest on 3/4
    assert_eq!(routing.residual, 1);
    let mut routed = Session::start_routed(doc.clone(), stem_cfg(2, false), stem_dither(), None, routing).unwrap();
    let frames = drain_bounded(&mut routed);
    let mut stems = Session::start(doc, stem_cfg(2, true), stem_dither(), None);
    let reference = drain_bounded(&mut stems);
    let sw = 2 + 2 * keys.len();
    let mut worst = 0.0f32;
    for (fr, rf) in frames.chunks_exact(4).zip(reference.chunks_exact(sw)) {
        assert_eq!(&fr[..2], &rf[2..4], "the first source keeps its own pair, bit for bit");
        for ch in 0..2 {
            worst = worst.max((fr[ch] + fr[2 + ch] - rf[ch]).abs());
        }
    }
    assert!(worst < 1e-5, "the shared pair carries everything else: worst {worst}");
    let rest: f64 = reference.chunks_exact(sw).map(|f| (2..keys.len()).map(|i| (f[2 + 2 * i] as f64).powi(2)).sum::<f64>()).sum();
    assert!(rest > 1e-6, "the unslotted sources actually sound (so the residual is exercised): {rest}");
}

#[test]
fn a_live_structural_add_plays_on_the_overflow_pair() {
    let doc = held(0);
    let routing = SourceRouting::sequential(&crate::engine::source_layout(&doc), 4); // pad on 1/2, overflow 3/4
    assert_eq!(routing.residual, 1);
    let mut s = Session::start_routed(doc.clone(), live_cfg(6), DitherConfig::default(), None, routing).unwrap();
    let before = pull_n(&mut s, 4 * 8_000); // 1 s of 4-wide frames, before the edit
    assert!(pair_energy(&before, 4, 0) > 1e-3, "the pad sounds on its pair");
    assert_eq!(pair_energy(&before, 4, 1), 0.0, "nothing unslotted yet: the overflow is exactly silent");

    let mut v = serde_json::to_value(&doc).unwrap();
    v["instruments"]["extra"] = serde_json::json!({ "note_engine": { "polyphony": 1 }, "params": { "wave": "sine", "level_db": -7.96, "sustain": 1.0, "release_s": 0.2 } });
    v["matrix"]["m2"] = serde_json::json!({ "source": "seq", "dest": "instruments.extra.notes", "kind": "note", "voices": 1 });
    s.apply_doc(LoopDoc::from_value(v).unwrap());
    let after = drain_bounded(&mut s);
    assert_eq!(after.len() % 4, 0);
    assert!(pair_energy(&after, 4, 1) > 1e-3, "the added instrument plays on the overflow pair");
    assert!(pair_energy(&after, 4, 0) > 1e-3, "the pad keeps its pair");
}

#[test]
fn a_retiring_bus_tail_rings_out_on_the_overflow_pair_in_live_mode() {
    let doc = orrery_doc();
    let keys = crate::engine::source_layout(&doc);
    let bus = keys.iter().position(|k| *k == SourceKey::Bus("rv".into())).expect("the Orrery doc has a reverb bus");
    let w = 2 * (keys.len() + 1);
    let overflow = keys.len();
    let mut s = Session::start_routed(doc.clone(), live_cfg(4), DitherConfig::default(), None, SourceRouting::sequential(&keys, w)).unwrap();
    let before = pull_n(&mut s, w * 8_000 * 3); // 3 s in: the reverb has a tail to ring out
    assert!(pair_energy(&before, w, bus) > 1e-6, "the reverb sounds on its pair");
    assert_eq!(pair_energy(&before, w, overflow), 0.0);

    // Rename the bus's effect: a chain edit no param can reach, so the bus is rebuilt fresh under
    // the same id (its pair) and the old one retires — its tail reaches the master but no source
    // buffer, so it must land on the overflow pair. (Dropping the bus outright wouldn't do: a doc
    // with no buses gets a default `rv` backfilled, a mere value edit.)
    let mut v = serde_json::to_value(&doc).unwrap();
    let fx = v["buses"]["rv"]["effects"]["verb"].take();
    v["buses"]["rv"]["effects"] = serde_json::json!({ "verb2": fx });
    s.apply_doc(LoopDoc::from_value(v).unwrap());
    let after = drain_bounded(&mut s);
    assert!(pair_energy(&after, w, overflow) > 1e-6, "the retiring tail rings out on the overflow pair");
    assert!(after.iter().all(|x| x.is_finite()));
}

// ── ADR 0033: a doc carries its own control settings ───────────────────────────────────────────

/// The Orrery demo doc at 4 s loops (no song).
fn orrery_doc() -> LoopDoc {
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(crate::test_support::fixtures_dir().join("loop_orrery_dorian.json")).unwrap()).unwrap();
    v["transport"]["bpm"] = serde_json::json!(240);
    LoopDoc::from_value(v).unwrap()
}

fn doc_with_movement(mut d: LoopDoc) -> LoopDoc {
    d.optimize = Some(crate::contract::OptimizeMode::Hold);
    d.movement = Some(crate::contract::Movement { orrery: true, phrase_loops: 1, tension_target: 0.2, ..Default::default() });
    d
}

#[test]
fn a_doc_with_movement_renders_like_the_same_explicit_control() {
    let cfg = SessionConfig { sample_rate: TEST_SR, max_loops: Some(4), ..Default::default() };
    let dither = DitherConfig { seed: 9, ..Default::default() };
    let doc = doc_with_movement(orrery_doc());
    let run = |d: LoopDoc, c: Option<SessionControl>| {
        let mut s = Session::start(d, cfg, dither, c);
        let audio = drain_bounded(&mut s);
        (audio, s.poll_records())
    };
    let (from_doc, recs) = run(doc.clone(), None);
    let explicit = SessionControl::from_doc(&doc).expect("the doc authors control");
    let mut plain = doc.clone();
    plain.optimize = None;
    plain.movement = None;
    let (from_control, _) = run(plain, Some(explicit));
    assert_eq!(from_doc, from_control, "doc-authored control ≡ the same control passed explicitly");
    assert!(recs.iter().all(|r| r.optimize == "hold"), "the doc's optimize applied");
    assert!(recs.iter().any(|r| r.harmonic_move.is_some()), "the doc's movement turned the Orrery on");
}

#[test]
fn an_explicit_control_overrides_the_doc() {
    let doc = doc_with_movement(orrery_doc());
    let mut s = Session::start(
        doc,
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(3), ..Default::default() },
        DitherConfig::default(),
        SessionControl::from_flags("drift", false, &[], &[], 0.05).unwrap().or(Some(SessionControl::default())),
    );
    drain_bounded(&mut s);
    let recs = s.poll_records();
    assert!(recs.iter().all(|r| r.optimize == "drift" && r.harmonic_move.is_none()), "{recs:?}");
}

#[test]
fn a_live_edit_to_the_docs_movement_reaches_the_session() {
    // Live edit (the player owns the mode): editing the doc's `movement` mid-session turns the
    // Orrery on — the session control picks up the doc's settings, keeping the live-edit mode.
    let doc = orrery_doc();
    let mut s = Session::start(
        doc.clone(),
        SessionConfig { sample_rate: TEST_SR, max_loops: Some(8), ..Default::default() },
        DitherConfig::default(),
        SessionControl::from_flags("live_edit", false, &[], &[], 0.05).unwrap(),
    );
    s.apply_doc(doc_with_movement(doc));
    drain_bounded(&mut s);
    let recs = s.poll_records();
    assert!(recs.iter().all(|r| r.optimize == "live_edit"), "the player's mode is kept: {recs:?}");
    assert!(recs.iter().any(|r| r.harmonic_move.is_some()), "the doc's movement turned the Orrery on");
    // The edit may land before or after the first loop renders; either way the revision bumped once.
    assert_eq!(recs.iter().map(|r| r.revision).max(), Some(1), "the control revision moved with the edit");
}

// ── Live mode (ADR 0035) ───────────────────────────────────────────────────

fn live_cfg(max_loops: usize) -> SessionConfig {
    SessionConfig { sample_rate: TEST_SR, max_loops: Some(max_loops), live_block: Some(256), ring_capacity: 4096, ..Default::default() }
}

#[test]
fn live_mode_without_edits_streams_the_same_audio_as_the_whole_loop_worker() {
    let dither = DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() };
    let mut whole = Session::start(fixture(), SessionConfig { sample_rate: TEST_SR, max_loops: Some(3), ..Default::default() }, dither, None);
    let mut live = Session::start(fixture(), live_cfg(3), dither, None);
    let (a, b) = (drain_bounded(&mut whole), drain_bounded(&mut live));
    assert_eq!(a.len(), b.len());
    assert!(a == b, "block rendering in the live worker changed the audio");
    assert_eq!(live.poll_records().len(), 3);
    assert!(!live.poll_blocks().is_empty(), "block telemetry flows");
}

#[test]
fn live_block_telemetry_meters_each_channel() {
    // ADR 0040: per-instrument and master peaks per channel. A voice's pan (a hash of its id,
    // within ±0.2) makes left and right differ; `peak` stays their max.
    let mut s = Session::start(held(0), live_cfg(2), DitherConfig::default(), None);
    drain_bounded(&mut s);
    let blocks = s.poll_blocks();
    let loud: Vec<_> = blocks.iter().filter(|b| b.peak > 0.01).collect();
    assert!(!loud.is_empty(), "the held sine sounds");
    for b in &loud {
        assert_eq!(b.peak, b.peak_lr[0].max(b.peak_lr[1]));
        assert_eq!(b.meters.len(), 1);
        assert!(b.song.is_none(), "a song-less session has no song position");
    }
    let [l, r] = loud.last().unwrap().meters[0].1;
    assert!(l > 0.01 && r > 0.01 && (l - r).abs() > 1e-4, "a panned voice meters differently per channel: {l} vs {r}");
}

#[test]
fn live_block_telemetry_carries_the_song_position() {
    let mut s = Session::start(small_song(0.5, 2), live_cfg(100), DitherConfig { max_change_per_loop: 0.05, seed: 21, cat_change_prob: 0.0, ..Default::default() }, None);
    drain_bounded(&mut s);
    let seen: std::collections::BTreeSet<(u64, u32, u32, u32, bool)> =
        s.poll_blocks().iter().map(|b| { let t = b.song.expect("song position on every block"); (b.loop_id, t.block, t.loop_in_block, t.block_loops, t.tail) }).collect();
    let base = seen.iter().next().unwrap().0;
    let per_loop: Vec<(u32, u32, u32, bool)> = seen.iter().map(|&(id, b, i, n, t)| { assert!(id >= base); (b, i, n, t) }).collect();
    assert_eq!(per_loop, vec![(0, 0, 2, false), (0, 1, 2, false), (1, 0, 1, false), (2, 0, 2, false), (2, 1, 2, false), (2, 0, 2, true), (2, 1, 2, true)]);
}

/// A held sine (one step at the top of a 2 s loop), as in the engine tests.
fn held(offset: i32) -> LoopDoc {
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

#[test]
fn a_live_edit_lands_mid_loop_with_an_applied_event() {
    let ctrl = SessionControl::from_flags("hold", false, &[], &[], 0.05).unwrap();
    let mut s = Session::start(held(0), SessionConfig { doc_rev: 1, ..live_cfg(2) }, DitherConfig::default(), ctrl);
    let mut audio = Vec::new();
    let mut buf = [0.0f32; 512];
    let mut sent = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !s.finished() {
        let n = s.pull_audio(&mut buf);
        audio.extend_from_slice(&buf[..n]);
        // Halfway through the first loop (2 s = 16 000 frames at 8 kHz), edit the offset.
        if !sent && audio.len() >= 2 * 8_000 {
            s.apply_doc_rev(held(12), 2);
            sent = true;
        }
        if n == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    let applied = s.poll_applied();
    assert_eq!(applied.len(), 1, "{applied:?}");
    let a = &applied[0];
    assert_eq!((a.doc_rev, a.kind), (2, crate::engine::EditKind::Value));
    assert!(a.pos > 0 && a.pos < 16_000, "applied mid-loop, not at a loop boundary: {a:?}");
    // The ring is ~4096 frames, so the edit lands within a ring + a block of where it was made.
    assert!(a.abs_frame >= 8_000 && a.abs_frame < 8_000 + 2_048 + 256 + 512, "edit latency: applied at {}", a.abs_frame);
    let zc = |from: usize, to: usize| (from + 1..to).filter(|&f| audio[2 * (f - 1)] < 0.0 && audio[2 * f] >= 0.0).count() as f32 * 8_000.0 / (to - from) as f32;
    let at = a.abs_frame as usize + a.xfade_frames;
    assert!((zc(1_000, 7_900) - 440.0).abs() < 10.0);
    assert!((zc(at + 100, 15_900) - 880.0).abs() < 15.0, "the new offset sounds after the crossfade");
    let recs = s.poll_records();
    assert_eq!(recs.iter().map(|r| r.doc_rev).collect::<Vec<_>>(), vec![2, 2], "the edited loop and the next play r2");
}

#[test]
fn a_structural_swap_keeps_the_live_gains_in_the_whole_loop_worker() {
    // A muted voice stays muted across a structural edit: the standby engine continues the gains
    // (it used to start at unity — the mute silently undone).
    let ctrl = SessionControl::from_flags("hold", false, &[], &[], 0.05).unwrap();
    let cfg = SessionConfig { sample_rate: TEST_SR, max_loops: Some(5), ring_capacity: 4096, ..Default::default() };
    let mut s = Session::start(held(0), cfg, DitherConfig::default(), ctrl);
    s.set_live_gains([("pad".to_string(), 0.0)].into_iter().collect());
    let mut structural = held(0);
    structural.instruments.insert("other".into(), structural.instruments["pad"].clone());
    let mut buf = [0.0f32; 512];
    let mut pulled = 0usize;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !s.finished() {
        let n = s.pull_audio(&mut buf);
        pulled += n / 2;
        if pulled >= 20_000 && pulled - n / 2 < 20_000 {
            s.apply_doc_rev(structural.clone(), 2); // lands at a later loop top, after the mute
        }
        if n == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    let recs = s.poll_records();
    assert!(recs.iter().any(|r| r.doc_rev == 2), "the structural edit landed: {:?}", recs.iter().map(|r| r.doc_rev).collect::<Vec<_>>());
    // The mute lands at the first loop top after `set_live_gains` and ramps in over ~10 ms, so that
    // loop's peak isn't silent. Loop ids start at the doc's `loop_id`, 1. Usually the mute lands at
    // loop 1, and loop 2 (recs[1]) is the first silent one. If the worker reaches loop 1's top before
    // `set_live_gains`, it lands at loop 2, and loop 3 (recs[2]) is. Never later: loop 2's top waits
    // until loop 1's frames have been pulled, all after the mute. From the first silent loop on, every
    // loop stays silent, the swap included.
    let first_silent = recs.iter().position(|r| r.peak < 1e-4).expect("the mute took effect");
    assert!(first_silent <= 2, "the mute landed late: peaks {:?}", recs.iter().map(|r| r.peak).collect::<Vec<_>>());
    for r in &recs[first_silent..] {
        assert!(r.peak < 1e-4, "loop {} (r{}) sounds at {} — the mute was dropped", r.loop_id, r.doc_rev, r.peak);
    }
}

#[test]
fn live_gains_mute_within_a_block() {
    let ctrl = SessionControl::from_flags("hold", false, &[], &[], 0.05).unwrap();
    let mut s = Session::start(held(0), live_cfg(2), DitherConfig::default(), ctrl);
    let mut audio = Vec::new();
    let mut buf = [0.0f32; 512];
    let mut muted_at = None;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !s.finished() {
        let n = s.pull_audio(&mut buf);
        audio.extend_from_slice(&buf[..n]);
        if muted_at.is_none() && audio.len() >= 2 * 4_000 {
            s.set_live_gains([("pad".to_string(), 0.0)].into_iter().collect());
            muted_at = Some(audio.len() / 2);
        }
        if n == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    let m = muted_at.unwrap();
    assert!(audio[2 * 1_000..2 * 3_900].iter().any(|x| x.abs() > 0.3), "sounding before the mute");
    let silent_from = m + 2_048 + 512 + 256; // ring + a block + the 10 ms ramp, at most
    assert!(audio[2 * silent_from..].iter().all(|x| x.abs() < 1e-6), "muted within a ring + a block");
}

#[test]
fn a_song_can_start_at_a_later_block() {
    // Block audition (ADR 0036): blocks 2/1/2 loops + 1 tail; starting at block 1 plays 1 + 2 + 1.
    let doc = small_song(0.0, 1);
    let mut s = Session::start(doc, SessionConfig { sample_rate: TEST_SR, song_start_block: 1, ..Default::default() }, DitherConfig::default(), None);
    drain_bounded(&mut s);
    let recs = s.poll_records();
    let blocks: Vec<u32> = recs.iter().map(|r| r.song.as_ref().unwrap().block).collect();
    assert_eq!(blocks, vec![1, 2, 2, 2], "block 1, then block 2 (2 loops), then the tail (reports the last block)");
    assert!(recs[1].song.as_ref().unwrap().seam, "block 2 still starts with its seam");
    // Its loops are numbered as the full song numbers them (prob rolls, drift picks key on them).
    let mut full = Session::start(small_song(0.0, 1), SessionConfig { sample_rate: TEST_SR, ..Default::default() }, DitherConfig::default(), None);
    drain_bounded(&mut full);
    let full_ids: Vec<u64> = full.poll_records().iter().map(|r| r.loop_id).collect();
    let ids: Vec<u64> = recs.iter().map(|r| r.loop_id).collect();
    assert_eq!(ids, full_ids[2..], "block 1 starts at song loop 2");
}

#[test]
fn each_record_carries_its_own_loops_orrery_state() {
    // Drained only at the end — every loop's record is read at once — each still names the chord
    // its own loop played (it used to take the latest telemetry snapshot's).
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(crate::test_support::fixtures_dir().join("loop_orrery_dorian.json")).unwrap()).unwrap();
    v["transport"]["bpm"] = serde_json::json!(240);
    let doc = LoopDoc::from_json_str(&v.to_string()).unwrap();
    let control = SessionControl { movement: crate::contract::Movement { orrery: true, phrase_loops: 1, ..Default::default() }, ..Default::default() };
    let mut s = start_song(doc, Some(10), Some(control));
    drain_bounded(&mut s);
    let recs = s.poll_records();
    assert_eq!(recs.len(), 10);
    let chords: Vec<&str> = recs.iter().map(|r| r.orrery.as_ref().expect("Orrery state on every record").chord.as_str()).collect();
    for r in &recs {
        if let Some(m) = &r.harmonic_move {
            assert_eq!(m.chord, r.orrery.as_ref().unwrap().chord, "loop {}: the record's chord is the move it made", r.loop_id);
        }
    }
    let distinct: std::collections::BTreeSet<&str> = chords.iter().copied().collect();
    assert!(distinct.len() > 1, "the voyage moves, and each record shows its own stop: {chords:?}");
    assert!(recs.iter().all(|r| !r.orrery.as_ref().unwrap().tones.is_empty()));
}

#[test]
fn a_render_worker_panic_finishes_the_session_and_says_why() {
    // Every host (engines-song's drain, engines-live, the FFI) waits on `finished()`: a panicking worker
    // must still set it, report the panic, and not make `join` wait out the shutdown grace.
    for live_block in [None, Some(256)] {
        let mut d = fixture();
        d.loop_id = PANIC_LOOP_ID;
        let mut s = Session::start(
            d,
            SessionConfig { sample_rate: TEST_SR, max_loops: Some(2), live_block, ..Default::default() },
            DitherConfig::default(),
            None,
        );
        let audio = drain_bounded(&mut s);
        assert!(audio.is_empty());
        let why = s.worker_failure().expect("the panic is reported");
        assert!(why.contains("test-only render worker panic"), "{why}");
        let t = Instant::now();
        s.join();
        assert!(t.elapsed() < SHUTDOWN_GRACE, "join doesn't wait out the grace on a dead worker");
    }
    // A run that simply ends reports no failure.
    let mut s = Session::start(fixture(), SessionConfig { sample_rate: TEST_SR, max_loops: Some(1), ..Default::default() }, DitherConfig::default(), None);
    drain_bounded(&mut s);
    assert!(s.worker_failure().is_none());
}
