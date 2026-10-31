//! The single seam between the TUI and the engine. Owns a live [`Session`] (which runs the
//! drift / Orrery controller + render worker on its own thread) plus the cpal streams that
//! carry it — built on **`engines_core::playback`**, the same shared shell wiring `engines-play`
//! uses (device resolution, output stream + DAC clock, DAW-return monitor, MIDI shutdown).
//! The TUI only ever *reads* from here via [`poll`](EngineLink::poll); it makes no
//! `engines_core` changes.

use engines_core::contract::SessionControl;
use engines_core::controller::DitherConfig;
use engines_core::loop_doc::LoopDoc;
use engines_core::midi_out::MidiPump;
use engines_core::playback;
use engines_core::session::{Session, SessionConfig, VizFrame};
use engines_core::telemetry::{LoopRecord, Telemetry};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct EngineLink {
    session: Session,
    /// Kept alive for the duration of the run; dropping it stops the audio device. The cpal
    /// `Stream` is `!Send`, so `EngineLink` (and the whole TUI) stays on the main thread.
    _output: playback::OutputStream,
    pub sample_rate: f64,
    /// Shell-facing startup notes (e.g. the ADR 0027 stem fold-back warning) — a TUI can't
    /// print, so the app routes these into its Log view.
    pub startup_notes: Vec<String>,
    /// The control the session started from — used to seed the watched objectives doc.
    control: SessionControl,
    /// Device frames the DAC has consumed since session start — the play cursor (ADR 0024)
    /// and the MIDI-pump clock (ADR 0026). Shares the origin of `VizFrame::audio_pos`, so
    /// the instrument screen reveals a hop only once `audio_pos <= frames_played`. Advances
    /// through underrun-silence fills (playback time does) — see `playback`'s module header.
    frames_played: Arc<AtomicU64>,
    /// MIDI-out delivery (ADR 0026): virtual port + playback-clocked pump, fed from
    /// [`poll`](EngineLink::poll). `None` when `--midi` is off.
    midi_pump: Option<MidiPump>,
    /// DAW-audio-return capture (ADR 0026 phase D): the loopback input stream feeding the
    /// output's live monitor mix — held only to keep it alive. `None` without a return device.
    _return_capture: Option<playback::ReturnCapture>,
}

impl EngineLink {
    /// Open the output device, start the session at the device's native rate, and begin
    /// streaming it — the shared `engines_core::playback` wiring.
    /// `midi_port` = `Some(name)` additionally creates a virtual MIDI source and delivers
    /// `type:"midi"` instruments' events clocked to playback (ADR 0026).
    /// `return_device` = `Some(name)` opens that loopback input as the DAW-audio-return (phase D),
    /// monitor-mixed into playback (default output only). Requires an `external_return` bus in
    /// the doc; `"default"` = the default input, else a substring match.
    /// `output_device` = `Some(name)` REDIRECTS the engine to that device instead of the system
    /// default (ADR 0027): nothing plays locally — the device's consumer (a DAW on a loopback)
    /// is the monitor. Opened at its full channel count so `out_channel` instruments get their
    /// own stereo pairs (1/2 = rest of mix). Must differ from `return_device`, and the DAW-return
    /// monitor mix is disabled (the DAW is the monitor — mixing its master back in would loop).
    /// `dither_midi` = midi drift (ADR 0026): drift also moves `type:"midi"` params.
    #[allow(clippy::too_many_arguments)] // mirrors the session-config surface; a struct would just move the noise
    pub fn start(
        doc: LoopDoc,
        optimize: &str,
        orrery: bool,
        max_change: Option<f32>,
        seed: u64,
        midi_port: Option<&str>,
        return_device: Option<&str>,
        output_device: Option<&str>,
        dither_midi: bool,
    ) -> Result<Self, String> {
        if output_device.is_some() && output_device == return_device {
            return Err("output and return must be different devices — sharing one loopback would feed the engine's own audio back into itself".into());
        }
        // Runtime `--max-change` beats the doc's `max_change`, then the built-in default (ADR 0025).
        let max_change = max_change.or(doc.max_change).unwrap_or(engines_core::controller::DEFAULT_MAX_CHANGE);

        let out = playback::open_output(output_device)?;
        let sample_rate = out.sample_rate;
        let mut startup_notes = Vec::new();
        if let Some(n) = playback::stem_shortfall_note(&doc, &out) {
            startup_notes.push(format!("note: {n}"));
        }
        if out.redirected {
            startup_notes.push(format!(
                "output → \"{}\" ({} ch; the engine plays ONLY there — its consumer is the monitor)",
                out.name, out.out_channels
            ));
        }

        // The doc's own control settings (ADR 0033) with the TUI's choices layered on top.
        // Guardrails (ceilings/floors) aren't exposed in the TUI yet — pass empties.
        let control = SessionControl::with_flags(SessionControl::from_doc(&doc), optimize, orrery, &[], &[], max_change)?;
        let materialized = control.clone().unwrap_or_default();

        let mut session = Session::start(
            doc,
            SessionConfig { sample_rate, max_loops: None, out_channels: out.out_channels, ..Default::default() },
            DitherConfig { max_change_per_loop: max_change, seed, include_midi: dither_midi, ..Default::default() },
            control,
        );
        let cons = session
            .take_consumer()
            .ok_or_else(|| "session had no audio consumer".to_string())?;

        // The DAC clock — created first so the MIDI pump can read it (see
        // `playback::start_output_stream`).
        let frames_played = Arc::new(AtomicU64::new(0));

        // ── DAW-audio-return monitor (ADR 0026 phase D) ──────────────────────────
        // Built BEFORE the output stream so its `monitor` ring can move into the output
        // callback; the capture is kept alive for the run. `quiet_errors`: a TUI can't print
        // from stream callbacks.
        let mut monitor: Option<(rtrb::Consumer<f32>, f32)> = None;
        let mut return_capture: Option<playback::ReturnCapture> = None;
        if let Some(name) = return_device {
            let rings = playback::start_return_capture(name, &out, &session, true)?;
            let gain = rings.capture.return_gain;
            // Redirected output (ADR 0027): the redirect target's consumer (the DAW) is the
            // monitor — monitor-mixing its own master back into the stream we feed it would
            // loop, so the return is ignored (same as engines-play).
            if out.redirected {
                startup_notes.push(format!(
                    "return ← \"{name}\" ignored: monitor mix is off while redirected (the DAW is the monitor)"
                ));
            } else {
                startup_notes.push(format!(
                    "return ← \"{name}\" (monitor gain {gain}, latency {} ms; route the DAW's master ONLY here)",
                    rings.capture.latency_ms
                ));
                monitor = Some((rings.monitor, gain));
            }
            return_capture = Some(rings.capture);
        }

        let output = playback::start_output_stream(&out, cons, monitor, &frames_played, true)?;

        // MIDI-out (ADR 0026): the pump shares the play cursor as its clock.
        let midi_pump = match midi_port {
            Some(name) => Some(MidiPump::spawn(name, frames_played.clone()).map_err(|e| format!("midi port: {e}"))?),
            None => None,
        };

        Ok(Self {
            session,
            _output: output,
            sample_rate,
            startup_notes,
            control: materialized,
            frames_played,
            midi_pump,
            _return_capture: return_capture,
        })
    }

    /// Capture (input) device names for the Setup screen's return-device selector.
    pub fn list_input_devices() -> Vec<String> {
        playback::list_input_devices()
    }

    /// Output device names for the Setup screen's Output (redirect) selector.
    pub fn list_output_devices() -> Vec<String> {
        playback::list_output_devices()
    }

    /// Device frames played since session start — the viz play cursor (advances through
    /// underrun-silence fills; see `playback`).
    pub fn frames_played(&self) -> u64 {
        self.frames_played.load(Ordering::Acquire)
    }

    /// Drain the per-loop records emitted since the last poll (worker thread → TUI), one per
    /// rendered loop, in order. Never blocks. Also forwards any newly-emitted MIDI events to the
    /// pump (ADR 0026) — the TUI tick is the poll cadence; the pump's own thread releases them
    /// at their playback time.
    pub fn poll(&mut self) -> Vec<LoopRecord> {
        if let Some(p) = &self.midi_pump {
            p.push(self.session.poll_midi());
        }
        self.session.poll_records()
    }

    /// True once the worker stopped producing (e.g. a bounded run finished).
    pub fn worker_finished(&self) -> bool {
        self.session.worker_finished()
    }

    /// Apply a runtime control edit (Phase 1) — takes effect at the next loop boundary, re-based.
    pub fn apply_control(&self, control: SessionControl) {
        self.session.apply_control(control);
    }

    /// Apply a hand-edited loop doc (LiveEdit, ADR 0024) at the next loop boundary — seamless for
    /// same-id edits, standby-engine crossfade for structural ones.
    pub fn apply_doc(&self, doc: LoopDoc) {
        self.session.apply_doc(doc);
    }

    /// Enable/disable the live-viz tap (instrument screen, ADR 0024).
    pub fn set_viz(&self, on: bool) {
        self.session.set_viz(on);
    }

    /// Select which instrument's dry hops the viz tap captures (`None` ⇒ master + effective only).
    pub fn set_viz_instrument(&self, id: Option<String>) {
        self.session.set_viz_instrument(id);
    }

    /// Drain the live-viz frame stream (instrument screen, ADR 0024).
    pub fn poll_viz(&mut self) -> Vec<VizFrame> {
        self.session.poll_viz()
    }

    /// Latest read-only telemetry (operating point, drifted param, constraints, Orrery, song).
    pub fn poll_telemetry(&self) -> Option<Telemetry> {
        self.session.poll_telemetry()
    }

    /// The control the session started from (for seeding the watched objectives doc).
    pub fn control(&self) -> &SessionControl {
        &self.control
    }
}

// `Session`'s own `Drop` stops the worker and joins it; the streams stop when their handles
// drop. With MIDI on we additionally run the shared hung-note-safe shutdown FIRST
// (`playback::stop_midi_pump`: session exit flush → pump → pump's own Offs+CC123 drain), so
// quitting the TUI never leaves hung notes.
impl Drop for EngineLink {
    fn drop(&mut self) {
        if let Some(pump) = self.midi_pump.take() {
            playback::stop_midi_pump(&mut self.session, pump, std::time::Duration::from_millis(500));
        }
    }
}
