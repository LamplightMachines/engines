//! Shared desktop playback shell (feature `playback`; ADR 0026/0027): output-device
//! resolution + redirect width, the composed-output stream (monitor mix, frames-played
//! clock, underrun counter), the DAW-return capture, and the hung-note-safe MIDI-pump
//! shutdown — the wiring `engines-play` and the TUI's `EngineLink` previously each hand-rolled
//! (~200 duplicated lines that had already drifted).
//!
//! cpal `Stream`s are `!Send`: every handle returned here must stay on the thread that
//! created it (both shells keep them on the main thread).
//!
//! **Reconciled semantics** (where the two copies had diverged, now one truth):
//! - `frames_played` **advances through underruns** — it counts `data.len()/channels` per
//!   output callback (AcqRel), because playback *time* keeps moving when the ring underruns
//!   (engines-play's semantics; the MIDI pump and the return anchor ride DAC time). EngineLink
//!   used to count only popped frames with `Relaxed` — for its viz gate the difference is
//!   at most one underrun buffer, and only *during* an underrun.
//! - **Underruns are counted** ([`OutputStream::underruns`]) — engines-play used to signal a
//!   channel nobody read; the TUI had nothing. Pre-first-audio priming silence (the worker
//!   is still rendering loop 1) is not counted.
//! - **Shell-facing messages are returned as data**, never printed here (a TUI can't print
//!   without corrupting its screen): see [`stem_shortfall_note`]. Stream *error* callbacks
//!   take `quiet_errors` for the same reason.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::loop_doc::LoopDoc;
#[cfg(feature = "midi")]
use crate::midi_out::MidiPump;
use crate::session::Session;

/// A resolved output device + the stream geometry the session must match.
pub struct OpenOutput {
    pub device: cpal::Device,
    /// The device's reported name (empty if unavailable).
    pub name: String,
    /// The device's native rate — the engine renders at this (CE-R2.1: no resampling).
    pub sample_rate: f64,
    /// Interleaved stream width: the device's full channel count when redirected (so
    /// `out_channel` instruments get their own stereo pairs, ADR 0027), else plain stereo.
    pub out_channels: usize,
    /// True when `--output-device` named a device: the engine plays ONLY there — nothing
    /// locally — and the DAW-return monitor mix must stay off (feedback via the DAW's master).
    pub redirected: bool,
    sr: cpal::SampleRate,
}

/// Resolve the output device: `None` ⇒ the system default (plain stereo); `Some(name)` ⇒
/// REDIRECT (ADR 0027) to the device whose name equals or contains `name` (`"default"` ⇒ the
/// default device), opened at its full channel count.
pub fn open_output(output_device: Option<&str>) -> Result<OpenOutput, String> {
    let host = cpal::default_host();
    let redirected = output_device.is_some();
    let device = match output_device {
        None => host.default_output_device().ok_or_else(|| "no default output device".to_string())?,
        Some(name) => {
            let found = if name == "default" {
                host.default_output_device()
            } else {
                // An exact name first (a picker's choice), then the first that contains it.
                let devices: Vec<cpal::Device> = host.output_devices().map(|it| it.collect()).unwrap_or_default();
                let named = |exact: bool| devices.iter().find(|d| d.name().is_ok_and(|n| if exact { n == name } else { n.contains(name) })).cloned();
                named(true).or_else(|| named(false))
            };
            found.ok_or_else(|| format!("output device '{name}' not found"))?
        }
    };
    let cfg = device.default_output_config().map_err(|e| format!("output config: {e}"))?;
    // Redirect targets open at their full width for per-instrument stems; the default device
    // stays plain stereo (stems fold into 1/2 there — the music is always whole).
    let out_channels: usize = if redirected { (cfg.channels() as usize).max(2) } else { 2 };
    Ok(OpenOutput {
        name: device.name().unwrap_or_default(),
        sample_rate: cfg.sample_rate().0 as f64,
        out_channels,
        redirected,
        sr: cfg.sample_rate(),
        device,
    })
}

/// The shell-facing note when the doc routes `out_channel` stems past the opened stream's
/// width (they fold back into 1/2 — the music stays whole). `None` when everything fits.
/// Returned as data: engines-play eprintlns it, the TUI logs it.
pub fn stem_shortfall_note(doc: &LoopDoc, out: &OpenOutput) -> Option<String> {
    let needed = doc
        .instruments
        .values()
        .filter(|i| !i.is_midi())
        .filter_map(|i| i.out_channel)
        .map(|oc| oc as usize + 1)
        .max()?;
    (needed > out.out_channels).then(|| {
        format!(
            "doc routes instruments up to device channel {needed} but the output stream has {} — those stems fold into channels 1/2{}",
            out.out_channels,
            if out.redirected { "" } else { " (use --output-device with a multichannel loopback, e.g. \"BlackHole 16ch\")" }
        )
    })
}

/// The live output stream + its underrun counter. Dropping it stops the device.
pub struct OutputStream {
    /// Kept alive for the duration of the run (`!Send` — stay on the creating thread).
    pub stream: cpal::Stream,
    /// Output callbacks that found the ring (momentarily) empty and filled silence.
    pub underruns: Arc<AtomicU64>,
    /// Interleaved samples actually popped from the session ring — unlike `frames_played` (DAC
    /// time, which also advances through priming silence and underruns), this only moves when
    /// rendered audio is consumed. Divide by the stream width for device frames; compare against
    /// `Session::frames_produced` to know the rendered audio has all been handed to the device.
    pub ring_samples_consumed: Arc<AtomicU64>,
}

/// Pop up to `data.len()` samples from the ring into `data` (RT-safe: pops only) and fill the
/// remainder with silence; returns how many samples came from the ring.
pub(crate) fn pop_into(cons: &mut rtrb::Consumer<f32>, data: &mut [f32]) -> usize {
    let mut i = 0;
    while i < data.len() {
        match cons.pop() {
            Ok(s) => {
                data[i] = s;
                i += 1;
            }
            Err(_) => break,
        }
    }
    for s in data[i..].iter_mut() {
        *s = 0.0;
    }
    i
}

/// Build + start the output stream: pop composed device frames from the session's ring,
/// fill silence on underrun (counted), optionally monitor-mix the DAW return
/// (`monitor = (ring, return_gain)` — pass `None` when redirected, ADR 0027), and advance
/// `frames_played` — the caller-owned DAC clock (create it first: the return capture
/// anchors on it and the MIDI pump reads it). Advances through underrun-silence fills too,
/// because playback time does. `quiet_errors` swallows cpal error prints (TUI).
pub fn start_output_stream(
    out: &OpenOutput,
    cons: rtrb::Consumer<f32>,
    monitor: Option<(rtrb::Consumer<f32>, f32)>,
    frames_played: &Arc<AtomicU64>,
    quiet_errors: bool,
) -> Result<OutputStream, String> {
    start_output_stream_with(out, cons, monitor, frames_played, quiet_errors, None)
}

/// A device-output post-processor: called in the audio callback with each interleaved buffer
/// (and the channel count) after the ring pop and the monitor mix. **Live-only** (e.g.
/// `engines-live`'s protection limiter) — renders and stems never pass through it. Must be RT-safe:
/// no allocation, locks or I/O.
pub type OutputPost = Box<dyn FnMut(&mut [f32], usize) + Send + 'static>;

/// [`start_output_stream`] with an optional [`OutputPost`] stage on the device buffer.
pub fn start_output_stream_with(
    out: &OpenOutput,
    mut cons: rtrb::Consumer<f32>,
    monitor: Option<(rtrb::Consumer<f32>, f32)>,
    frames_played: &Arc<AtomicU64>,
    quiet_errors: bool,
    mut post: Option<OutputPost>,
) -> Result<OutputStream, String> {
    let stream_cfg = cpal::StreamConfig {
        channels: out.out_channels as u16,
        sample_rate: out.sr,
        buffer_size: cpal::BufferSize::Default,
    };
    let frames_played_cb = frames_played.clone();
    let underruns = Arc::new(AtomicU64::new(0));
    let underruns_cb = underruns.clone();
    let ring_samples_consumed = Arc::new(AtomicU64::new(0));
    let consumed_cb = ring_samples_consumed.clone();
    let out_channels = out.out_channels;
    let mut mon = monitor;
    let mut started = false; // audio has reached the ring — before that, silence is priming, not an underrun
    let stream = out
        .device
        .build_output_stream(
            &stream_cfg,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // RT-safe: pop only. On underrun, the remainder is filled with silence.
                let i = pop_into(&mut cons, data);
                consumed_cb.fetch_add(i as u64, Ordering::AcqRel);
                started |= i > 0;
                if i < data.len() && started {
                    underruns_cb.fetch_add(1, Ordering::Relaxed); // ring momentarily empty → silence
                }
                // Monitor-mix the DAW return live (ADR 0026 phase D): whatever the loopback
                // carries *now* sounds now; scoring alignment is handled separately
                // (latency_ms). Only reached on the plain-stereo default output (`mon` is
                // None when redirected, ADR 0027), so per-sample iteration interleaves the
                // stereo return correctly.
                if let Some((m, gain)) = mon.as_mut() {
                    for s in data.iter_mut() {
                        *s += *gain * m.pop().unwrap_or(0.0);
                    }
                }
                if let Some(p) = post.as_mut() {
                    p(data, out_channels);
                }
                // The DAC consumed this whole buffer (underrun silence included) — playback
                // time advanced by it, so the MIDI/return clock does too.
                frames_played_cb.fetch_add((data.len() / out_channels) as u64, Ordering::AcqRel);
            },
            move |err| {
                if !quiet_errors {
                    eprintln!("cpal stream error: {err}");
                }
            },
            None,
        )
        .map_err(|e| format!("build output stream: {e}"))?;
    stream.play().map_err(|e| format!("stream play: {e}"))?;
    Ok(OutputStream { stream, underruns, ring_samples_consumed })
}

/// DAW-audio-return capture (ADR 0026 phase D): the loopback input stream that feeds the live
/// monitor mix. Dropping it stops the input device.
pub struct ReturnCapture {
    /// Kept alive for the duration of the run (`!Send`).
    _stream: cpal::Stream,
    /// The bus's authored round-trip latency (ms) — for shell status messages.
    pub latency_ms: f32,
    /// The bus's return (`return_gain_db` as an amplitude) — the monitor-mix gain (for shell status messages).
    pub return_gain: f32,
}

/// The capture plus its live monitor ring. The shell decides the monitor's fate: feed it to
/// [`start_output_stream`] on a default (local) output, or **drop it** when redirected — the
/// DAW is the monitor there, and mixing its own master back into the stream it consumes
/// would feed back (ADR 0027).
pub struct ReturnRings {
    pub capture: ReturnCapture,
    pub monitor: rtrb::Consumer<f32>,
}

/// Open the named loopback input (`"default"` ⇒ the default input, else substring match) as
/// the DAW-audio-return. Requires an `external_return` bus in the doc and the input to run
/// at the output's rate (no resampling).
pub fn start_return_capture(name: &str, out: &OpenOutput, session: &Session, quiet_errors: bool) -> Result<ReturnRings, String> {
    let (return_gain, latency_ms) = session.external_return().ok_or_else(|| {
        "return device selected but the doc has no `external_return` bus (set return_gain_db + latency_ms)".to_string()
    })?;
    let host = cpal::default_host();
    let input = if name == "default" {
        host.default_input_device()
    } else {
        host.input_devices()
            .ok()
            .and_then(|mut it| it.find(|d| d.name().map(|n| n == name || n.contains(name)).unwrap_or(false)))
    };
    let input = input.ok_or_else(|| format!("input device '{name}' not found"))?;
    let in_cfg = input.default_input_config().map_err(|e| format!("input config: {e}"))?;
    if in_cfg.sample_rate() != out.sr {
        return Err(format!(
            "return device runs at {} Hz but the output at {} Hz — match them in Audio MIDI Setup (phase D does not resample)",
            in_cfg.sample_rate().0,
            out.sr.0
        ));
    }
    let cin = in_cfg.channels() as usize;
    let (mut mon_prod, mon_cons) = rtrb::RingBuffer::<f32>::new(1 << 15);
    let in_stream_cfg = cpal::StreamConfig {
        channels: in_cfg.channels(),
        sample_rate: in_cfg.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };
    let stream = input
        .build_input_stream(
            &in_stream_cfg,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Interleave to stereo (mono duplicates; >2ch takes the first pair) into the
                // monitor ring. RT-safe: pushes only; a full ring drops (a momentary artifact).
                for f in 0..data.len() / cin {
                    let l = data[f * cin];
                    let r = if cin > 1 { data[f * cin + 1] } else { l };
                    let _ = mon_prod.push(l);
                    let _ = mon_prod.push(r);
                }
            },
            move |err| {
                if !quiet_errors {
                    eprintln!("cpal input stream error: {err}");
                }
            },
            None,
        )
        .map_err(|e| format!("build input stream: {e}"))?;
    stream.play().map_err(|e| format!("input stream play: {e}"))?;
    Ok(ReturnRings {
        capture: ReturnCapture { _stream: stream, latency_ms, return_gain },
        monitor: mon_cons,
    })
}

#[cfg(feature = "midi")]
/// Hung-note-safe MIDI shutdown (ADR 0026), shared by both shells: stop the session, wait
/// (bounded) for the worker's exit flush (Note-Offs + CC 123, stamped at end-of-audio),
/// hand it to the pump, then stop the pump — whose own shutdown drain (Offs-only + CC 123
/// sweep, never future Note-Ons) is the backstop. Call BEFORE dropping the output stream.
pub fn stop_midi_pump(session: &mut Session, mut pump: MidiPump, worker_wait: Duration) {
    session.stop();
    let deadline = Instant::now() + worker_wait;
    while !session.worker_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    pump.push(session.poll_midi());
    pump.stop();
}

/// Output device names (for the TUI Setup screen's Output selector).
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Ok(n) = d.name() {
                names.push(n);
            }
        }
    }
    names
}

/// An output device as a picker shows it: what [`open_output`] would open it at.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct OutputDeviceInfo {
    pub name: String,
    /// Its default output config's channel count (a redirect opens it at this width).
    pub channels: usize,
    pub sample_rate: f64,
    /// The system's default output.
    pub default: bool,
}

/// Every output device with its width and rate (for the VS Code output picker). A device whose
/// config can't be read is left out — it couldn't be opened either.
pub fn output_device_infos() -> Vec<OutputDeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            let (Ok(name), Ok(cfg)) = (d.name(), d.default_output_config()) else { continue };
            out.push(OutputDeviceInfo { default: default.as_deref() == Some(name.as_str()), channels: cfg.channels() as usize, sample_rate: cfg.sample_rate().0 as f64, name });
        }
    }
    out
}

/// Capture (input) device names (for the TUI Setup screen's Return selector).
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(n) = d.name() {
                names.push(n);
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_into_counts_only_ring_samples_and_silences_the_rest() {
        let (mut prod, mut cons) = rtrb::RingBuffer::<f32>::new(64);
        for k in 0..10 {
            prod.push(k as f32 + 1.0).unwrap();
        }
        let mut data = [9.0f32; 16];
        assert_eq!(pop_into(&mut cons, &mut data), 10);
        assert_eq!(&data[..10], &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert!(data[10..].iter().all(|&s| s == 0.0), "underrun remainder is silence");
        assert_eq!(pop_into(&mut cons, &mut data), 0, "an empty ring consumes nothing (DAC time would still advance)");
    }
}
