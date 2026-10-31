//! `engines-play` — desktop live audition (feature `playback`). Streams a loop doc (or a whole song)
//! to the default output device via **cpal** (CoreAudio on macOS) while the Rust core runs the
//! controller's drift / Orrery movement.
//!
//! Unlike the Swift `engines-shell` (the iOS-aligned path, needs Xcode), this builds with the
//! Command Line Tools alone — a dev convenience to *hear it now*. The audio device runs at
//! its native sample rate; the engine renders to match.
//!
//! Usage: `engines-play --doc <loop_doc.json> [OPTIONS]`  (build with `--features playback`).
//! Run `engines-play --help` for the full option list (run length, drift, controller mode).

use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use engines_core::controller::DitherConfig;
use engines_core::loop_doc::LoopDoc;
use engines_core::midi_out::MidiPump;
use engines_core::playback;
use engines_core::session::{Session, SessionConfig};

/// Live audition of a loop doc or song (cpal → CoreAudio): runs the controller's drift / Orrery
/// movement and streams the result at the device's native rate. Ctrl-C to stop.
#[derive(Parser, Debug, PartialEq)]
// `args_override_self`: a repeated single-value flag keeps its LAST value instead of erroring,
// so `make play -- --doc <doc>` overrides the Makefile's default. Repeatable
// `Vec` flags (--ceiling/--floor) still accumulate.
#[command(name = "engines-play", version, args_override_self = true, after_help = "\
NOTES:
    --output-device (ADR 0027) REDIRECTS all audio: nothing plays locally; the named
    device's consumer (a DAW on a loopback like \"BlackHole 16ch\") is the monitor and the
    only program driving the real interface. The stream opens at the device's full channel
    count: pair 1/2 carries the mix; each doc instrument with \"out_channel\": N (odd, 3..)
    gets its own stereo pair (N, N+1) — 1/2 then carries the rest + reverb/bus wet, and
    stems that don't fit fold back into 1/2. Must differ from --return-device (else the
    engine's own audio feeds back into itself); the DAW-return monitor mix is disabled
    when redirected (the DAW is the monitor).

    --return-device (ADR 0026 phase D; needs an `external_return` bus in the doc) captures
    the DAW's output from a loopback input and monitor-mixes it live at the bus's
    return_gain_db (unless redirected, above). Route the DAW's master ONLY to the loopback
    (else you hear it twice).

EXAMPLES:
    # Let it slowly evolve (drift), Ctrl-C to stop:
    cargo run --release --features playback,midi --bin engines-play -- \\
        --doc lab/tests/fixtures/loop_contract_v0.9.json

    # Frozen timbre, generative harmony:
    cargo run --release --features playback,midi --bin engines-play -- \\
        --doc lab/tests/fixtures/loop_drone_d_lydian.json5 --optimize hold --orrery")]
struct Args {
    /// Loop document JSON/JSON5 (schema v0.8), e.g. lab/tests/fixtures/loop_contract_v0.9.json
    #[arg(long)]
    doc: String,
    /// Write the live "objectives doc" here and WATCH it: edit + save while playing to
    /// re-steer (optimize / orrery / constraints / overrides)
    #[arg(long)]
    objectives: Option<String>,
    /// Stop after N control loops; -1 = run forever (Ctrl-C to stop). A song doc (ADR 0029) ends
    /// on its own when its sequence + tail completes; N can only cut it shorter
    #[arg(long, default_value_t = -1, allow_negative_numbers = true)]
    loops: i64,
    /// Max per-loop drift step δ, normalized 0..1. Omitted ⇒ the doc's max_change, then the
    /// built-in default — runtime beats the doc (ADR 0025)
    #[arg(long)]
    max_change: Option<f32>,
    /// RNG seed for the drift / Orrery sequence
    #[arg(long, default_value_t = 0x5EED)]
    seed: u64,
    /// Timbre axis (CE-R8.2): drift (bounded wander, the default) | hold (freeze at authored values)
    #[arg(long, default_value = "")]
    optimize: String,
    /// Enable generative Orrery harmony (movement axis; combines with any --optimize)
    #[arg(long)]
    orrery: bool,
    /// Per-param upper bound in the param's unit as the doc writes it (Hz, s, dB for gains), repeatable:
    /// --ceiling instruments.pad.params.cutoff_hz=2000
    #[arg(long = "ceiling", value_name = "PATH=V")]
    ceilings: Vec<String>,
    /// Per-param lower bound in the param's unit (dB for gains, `-inf` for silence), repeatable:
    /// --floor instruments.pad.params.level_db=-24
    #[arg(long = "floor", value_name = "PATH=V")]
    floors: Vec<String>,
    /// MIDI-out (ADR 0026): deliver `type:"midi"` instruments' Note-On/Off + CC to a
    /// virtual CoreMIDI source, clocked to playback (Note-Offs + CC 123 flush on exit)
    #[arg(long)]
    midi: bool,
    /// Virtual MIDI port name (implies --midi). Default: "Engines Out"
    #[arg(long)]
    midi_port: Option<String>,
    /// DAW-audio-return (ADR 0026 phase D; implies --midi): loopback input to capture,
    /// e.g. "BlackHole 2ch" ("default" = the default input). See NOTES below
    #[arg(long)]
    return_device: Option<String>,
    /// Output redirect (ADR 0027): stream ONLY to this device (e.g. "BlackHole 16ch") —
    /// its consumer is the monitor; `out_channel` instruments get their own stereo pairs.
    /// See NOTES below
    #[arg(long)]
    output_device: Option<String>,
    /// MIDI drift (implies --midi): `type:"midi"` params join the drift candidates so the
    /// external synth's CCs evolve too
    #[arg(long)]
    dither_midi: bool,
}

fn main() -> ExitCode {
    let mut args = Args::parse();
    // Implications (same semantics as engines-tui): a named port, a DAW return, or midi dither
    // each only matter when MIDI-out delivers — they imply --midi.
    args.midi |= args.midi_port.is_some() || args.return_device.is_some() || args.dither_midi;
    let midi_port = args.midi_port.clone().unwrap_or_else(|| "Engines Out".into());

    let doc = match std::fs::read_to_string(&args.doc).map_err(|e| e.to_string()).and_then(|s| LoopDoc::from_json_str(&s).map_err(|e| e.to_string())) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}: {e}", args.doc);
            return ExitCode::FAILURE;
        }
    };
    for w in doc.lint() {
        eprintln!("warning: {w}");
    }
    let has_midi_inst = doc.instruments.values().any(|i| i.is_midi());
    if has_midi_inst && !args.midi {
        eprintln!("note: doc has `type:\"midi\"` instruments but --midi is off — their events are discarded");
    }
    // Runtime `--max-change` beats the doc's `max_change`, then the built-in default (ADR 0025).
    let max_change = args.max_change.or(doc.max_change).unwrap_or(engines_core::controller::DEFAULT_MAX_CHANGE);

    // ── open the output device; the engine renders at its native rate ──────────
    // Default: the system output, plain stereo. `--output-device` (ADR 0027) REDIRECTS the
    // engine to that device instead (nothing plays locally — its consumer, e.g. a DAW on a
    // loopback, is the monitor), opened at its full channel count so `out_channel` instruments
    // get their own stereo pairs. Shared wiring: `engines_core::playback` (also used by the TUI).
    if let (Some(o), Some(r)) = (&args.output_device, &args.return_device) {
        if o == r {
            eprintln!("error: --output-device must differ from --return-device — sharing one loopback would feed the engine's own audio back into itself");
            return ExitCode::FAILURE;
        }
    }
    let out = match playback::open_output(args.output_device.as_deref()) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (sample_rate, out_channels) = (out.sample_rate, out.out_channels);
    if let Some(n) = playback::stem_shortfall_note(&doc, &out) {
        eprintln!("note: {n}");
    }
    if out.redirected {
        eprintln!("output → \"{}\" ({out_channels} ch; the engine plays ONLY there — its consumer is the monitor)", out.name);
    }

    // The doc's own control settings (ADR 0033), with any CLI flags layered on top.
    let doc_control = engines_core::contract::SessionControl::from_doc(&doc);
    let control = match engines_core::contract::SessionControl::with_flags(doc_control, &args.optimize, args.orrery, &args.ceilings, &args.floors, max_change) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: control: {e}");
            return ExitCode::from(2);
        }
    };
    let initial_control = control.clone().unwrap_or_default();
    // Song form (ADR 0029): announce the plan; the session plays it and ends after the tail.
    let song_total_s = match &doc.song {
        Some(spec) => match engines_core::song::plan(&doc, spec, args.seed) {
            Ok(p) => {
                let labels: Vec<String> = p.blocks.iter().map(|b| format!("{}×{}", b.label, b.loops)).collect();
                eprintln!("song: {} blocks ({}) + {} tail loops · {}", p.blocks.len(), labels.join(" → "), p.tail_loops, engines_core::song::fmt_hms(p.total_s));
                Some(p.total_s)
            }
            Err(e) => {
                eprintln!("error: song: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let mut session = Session::start(
        doc,
        SessionConfig {
            sample_rate,
            max_loops: if args.loops < 0 { None } else { Some(args.loops as usize) },
            out_channels,
            ..Default::default()
        },
        DitherConfig { max_change_per_loop: max_change, seed: args.seed, include_midi: args.dither_midi, ..Default::default() },
        control,
    );
    let cons = session.take_consumer().expect("fresh session has a consumer");

    // MIDI clock (ADR 0026): audio frames the DAC has consumed since session start.
    let frames_played = Arc::new(AtomicU64::new(0));

    // ── DAW-audio-return monitor (ADR 0026 phase D) ──────────────────────────
    // A loopback input (e.g. BlackHole carrying the DAW our MIDI drives) is mixed into the output
    // callback live. The capture is kept alive for the run.
    let mut monitor: Option<(rtrb::Consumer<f32>, f32)> = None;
    let mut _return_capture: Option<playback::ReturnCapture> = None;
    if let Some(name) = &args.return_device {
        let rings = match playback::start_return_capture(name, &out, &session, false) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let gain = rings.capture.return_gain;
        if out.redirected {
            // The redirect target's consumer (the DAW) is the monitor — monitor-mixing its own
            // master back into the stream we feed it would loop.
            eprintln!("return ← \"{name}\" ignored: monitor mix is off while redirected (the DAW is the monitor)");
        } else {
            eprintln!("return ← \"{name}\" (monitor gain {gain}; route the DAW's master ONLY here)");
            monitor = Some((rings.monitor, gain));
        }
        _return_capture = Some(rings.capture);
    }

    let output = match playback::start_output_stream(&out, cons, monitor, &frames_played, false) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stream = output.stream;
    let underruns = output.underruns;
    let ring_consumed = output.ring_samples_consumed;

    // ── MIDI-out delivery (ADR 0026): virtual port + playback-clocked pump ──────
    let mut midi_pump = if args.midi {
        match MidiPump::spawn(&midi_port, frames_played.clone()) {
            Ok(p) => {
                eprintln!("midi → virtual port \"{}\" (enable its Track input in your DAW)", midi_port);
                if !has_midi_inst {
                    eprintln!("note: --midi is on but the doc has no `type:\"midi\"` instruments — nothing will be sent");
                }
                Some(p)
            }
            Err(e) => {
                eprintln!("error: midi port: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    eprintln!(
        "playing live @ {} Hz (δ≤{}); {}",
        sample_rate as u32,
        max_change,
        if args.loops < 0 { "Ctrl-C to stop".to_string() } else { format!("{} loops", args.loops) },
    );

    // ── runtime control (Phase 1): write the objectives doc + watch it for live edits ───────────
    let mut last_obj: Option<(std::time::SystemTime, String)> = None;
    if let Some(path) = &args.objectives {
        let init = engines_core::contract::objectives_template_json5(&initial_control);
        match std::fs::write(path, &init) {
            Ok(()) => {
                eprintln!("objectives doc → {path}  (edit + save to re-steer live; bump \"revision\" to track)");
                last_obj = std::fs::metadata(path).ok().and_then(|m| m.modified().ok()).map(|t| (t, init));
            }
            Err(e) => eprintln!("warning: could not write objectives doc {path}: {e}"),
        }
    }
    let mut tele_tick = 0u32;
    let mut last_song_line = Instant::now();

    // ── main thread: deliver MIDI, watch the objectives doc, watch for completion ─────────────
    let mut n_loops = 0usize;
    loop {
        n_loops += session.poll_records().len();
        if let Some(p) = &midi_pump {
            p.push(session.poll_midi()); // render-ahead events queue; the pump releases them on time
        }

        // Watch the objectives doc for live edits (echo-guarded by content equality) + readout.
        if let Some(path) = &args.objectives {
            let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
            if mtime != last_obj.as_ref().map(|(t, _)| *t) {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if !last_obj.as_ref().is_some_and(|(_, c)| c == &content) {
                        match engines_core::contract::SessionControl::from_json_str(&content) {
                            Ok(ctrl) => {
                                let rev = ctrl.revision;
                                session.apply_control(ctrl);
                                eprintln!("↻ applied objectives edit (revision {rev})");
                            }
                            Err(e) => eprintln!("objectives parse error (ignored): {e}"),
                        }
                    }
                    last_obj = mtime.map(|t| (t, content));
                }
            }
            tele_tick = tele_tick.wrapping_add(1);
            if tele_tick % 20 == 0 {
                if let Some(t) = session.poll_telemetry() {
                    let orr = t.orrery.map(|o| format!(" · orrery {} t={:.2} (home-{})", o.chord, o.tension, o.loops_since_home)).unwrap_or_default();
                    let viol: Vec<String> = t.constraints.iter().filter(|c| !c.in_range)
                        .map(|c| format!("{}={:.2}!", c.path.rsplit('.').next().unwrap_or(&c.path), c.value)).collect();
                    let vstr = if viol.is_empty() { String::new() } else { format!(" · correcting {}", viol.join(",")) };
                    eprintln!("loop {} rev {} [{}]{}{}", t.loop_id, t.revision, t.optimize, orr, vstr);
                }
            }
        }

        // Song progress (ADR 0029): the play position (DAC clock) + the block the worker is
        // rendering (≈ a ring's worth — up to ~20 s — ahead of what you hear).
        if let Some(total_s) = song_total_s {
            if last_song_line.elapsed() >= Duration::from_secs(10) {
                last_song_line = Instant::now();
                if let Some(t) = session.poll_telemetry().and_then(|t| t.song) {
                    let played_s = frames_played.load(Ordering::Relaxed) as f64 / sample_rate;
                    let at = if t.tail { "tail".to_string() } else { format!("block {}/{} {} loop {}/{}", t.block + 1, t.blocks, t.label, t.loop_in_block + 1, t.block_loops) };
                    eprintln!("song ▸ {} / {} · rendering {at}", engines_core::song::fmt_hms(played_s), engines_core::song::fmt_hms(total_s));
                }
            }
        }

        // `worker_finished` = audio production done; let the device play out the buffered tail.
        if session.worker_finished() {
            n_loops += session.poll_records().len();
            // Hand the pump the session's exit flush (Note-Offs + CC 123, stamped at end-of-
            // audio) before the tail plays out, so it delivers on time as the DAC catches up.
            if let Some(p) = &midi_pump {
                p.push(session.poll_midi());
            }
            // Play out what's still buffered (up to a ring's worth ≈ 20 s — a song's ending + tail,
            // ADR 0029) before dropping the stream: wait until the device has popped every
            // produced frame from the ring. NOT the DAC clock (`frames_played`), which also
            // advances through priming silence and underruns and so can run ahead of the audio.
            // Bounded so a stalled device can't hang us; then slack for the device's own latency.
            let produced = session.frames_produced();
            let ring_s = (SessionConfig::default().ring_capacity / 2) as f64 / sample_rate;
            let deadline = Instant::now() + Duration::from_secs_f64(ring_s + 3.0);
            while !played_out(ring_consumed.load(Ordering::Acquire), out_channels, produced) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            std::thread::sleep(Duration::from_millis(300));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Stop the pump before the stream (shared hung-note-safe shutdown, ADR 0026): the
    // session's exit flush rides first; the pump's own drain (Offs + CC 123, never future
    // Note-Ons) is the backstop — Ctrl-C/end can't leave hung notes.
    if let Some(p) = midi_pump.take() {
        playback::stop_midi_pump(&mut session, p, Duration::from_millis(500));
    }
    drop(stream);
    let ur = underruns.load(Ordering::Relaxed);
    let ur_note = if ur > 0 { format!("; {ur} audio underruns") } else { String::new() };
    eprintln!("done ({n_loops} loops){ur_note}");
    ExitCode::SUCCESS
}

/// Has the device popped every frame the session produced? (`consumed_samples` is interleaved
/// samples popped from the ring; `produced_frames` is device frames pushed into it.)
fn played_out(consumed_samples: u64, out_channels: usize, produced_frames: u64) -> bool {
    consumed_samples / out_channels.max(1) as u64 >= produced_frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn played_out_compares_consumed_device_frames() {
        assert!(!played_out(0, 2, 10));
        assert!(!played_out(19, 2, 10), "a half-popped final frame isn't played out");
        assert!(played_out(20, 2, 10));
        assert!(!played_out(20, 4, 10), "width-aware (ADR 0027 multichannel redirect)");
        assert!(played_out(40, 4, 10));
    }

    #[test]
    fn cli_is_wellformed_and_implications_hold() {
        use clap::CommandFactory;
        Args::command().debug_assert();
        let a = Args::parse_from(["engines-play", "--doc", "d.json"]);
        assert_eq!((a.loops, a.seed), (-1, 0x5EED));
        assert!(!a.midi);
        // --loops takes a negative sentinel.
        let a = Args::parse_from(["engines-play", "--doc", "d.json", "--loops", "-1"]);
        assert_eq!(a.loops, -1);
        // The --midi implications main() applies (matches engines-tui's semantics).
        for extra in [["--midi-port", "X"], ["--return-device", "BlackHole 2ch"], ["--dither-midi", ""]] {
            let argv: Vec<&str> = ["engines-play", "--doc", "d.json"].into_iter().chain(extra.into_iter().filter(|s| !s.is_empty())).collect();
            let mut a = Args::parse_from(argv);
            a.midi |= a.midi_port.is_some() || a.return_device.is_some() || a.dither_midi;
            assert!(a.midi, "flag set {extra:?} must imply --midi");
        }
    }

    /// `make play -- --doc X` expands to the Makefile's own `--doc $(FIXTURE)` FOLLOWED by the
    /// forwarded flags — the last occurrence must win, not error.
    #[test]
    fn forwarded_flags_override_makefile_defaults() {
        let a = Args::try_parse_from(["engines-play", "--doc", "default.json", "--seed", "1", "--doc", "mine.json5", "--seed", "2", "--loops", "1"])
            .expect("a repeated --doc/--seed must override, not error");
        assert_eq!((a.doc.as_str(), a.seed, a.loops), ("mine.json5", 2, 1));
        // Repeatable flags still accumulate rather than override.
        let a = Args::parse_from(["engines-play", "--doc", "d", "--ceiling", "a=1", "--ceiling", "b=2"]);
        assert_eq!(a.ceilings, ["a=1", "b=2"]);
    }
}
