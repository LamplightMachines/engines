//! C ABI (CE-R3.6) — the seam the Swift macOS/iOS shell links against.
//!
//! A [`EnginesSession`] wraps the live [`crate::session::Session`]: the shell creates one
//! from a loop-doc JSON string, pulls interleaved-stereo audio on its `AVAudioSourceNode`
//! render block ([`engines_session_pull_audio`], RT-safe — pops only), drains per-loop records,
//! MIDI and telemetry as JSON off the audio thread, and stops/frees it at the end.
//!
//! **Safety contract:** every entry point is panic-guarded (`catch_unwind`) — no Rust
//! unwinding crosses the boundary; failures return null/0 or an out-param error string.
//! No Rust `String`/`Vec` is handed across raw; the caller passes buffers the core fills,
//! and owns the `EnginesSession` handle until [`engines_session_free`].

use std::ffi::{c_char, CStr, CString};
use std::os::raw::c_double;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use crate::controller::DitherConfig;
use crate::loop_doc::LoopDoc;
use crate::midi::TimedMidi;
use crate::session::{Session, SessionConfig};
use crate::telemetry::LoopRecord;

/// Opaque handle to a live streaming session. Created by [`engines_session_new`], destroyed by
/// [`engines_session_free`].
pub struct EnginesSession {
    inner: Session,
    /// Loop records popped from the session but not yet serialized into a caller buffer.
    pending_records: Vec<LoopRecord>,
    /// MIDI events popped from the session but not yet serialized into a caller buffer (ADR 0026).
    pending_midi: Vec<TimedMidi>,
}

/// Serialize as many whole items from the front of `pending` as fit into `out` as newline-delimited
/// JSON; returns bytes written. Delivered (and unserializable / oversized) items are removed; the
/// rest stay for the next call.
fn drain_json_lines<T: serde::Serialize>(pending: &mut Vec<T>, out: &mut [u8]) -> usize {
    let mut written = 0usize;
    let mut drained = 0usize;
    for item in pending.iter() {
        let line = match serde_json::to_string(item) {
            Ok(l) => l,
            Err(_) => {
                drained += 1; // unserializable: drop rather than wedge the queue
                continue;
            }
        };
        let need = line.len() + 1; // + '\n'
        if written + need > out.len() {
            if written == 0 {
                drained += 1; // single item larger than the whole buffer: drop, don't stall
                continue;
            }
            break;
        }
        out[written..written + line.len()].copy_from_slice(line.as_bytes());
        out[written + line.len()] = b'\n';
        written += need;
        drained += 1;
    }
    pending.drain(0..drained.min(pending.len()));
    written
}

/// The per-loop drift step for a session: a positive `arg` (the host's override) wins, else the
/// doc's `max_change`, else the built-in default — the precedence the CLIs use (ADR 0025).
fn resolve_max_change(arg: f32, doc: &LoopDoc) -> f32 {
    if arg.is_finite() && arg > 0.0 {
        arg
    } else {
        doc.max_change.unwrap_or(crate::controller::DEFAULT_MAX_CHANGE)
    }
}

/// Write `msg` into `*err` as a freshly-allocated C string (freed with [`engines_string_free`]).
unsafe fn set_err(err: *mut *mut c_char, msg: String) {
    if !err.is_null() {
        *err = CString::new(msg).unwrap_or_default().into_raw();
    }
}

/// Create and start a streaming session from a loop-doc JSON string.
///
/// `sample_rate` ≤ 0 ⇒ 48000. `max_loops` < 0 ⇒ run until [`engines_session_stop`]. `max_change` ≤ 0
/// (or NaN) ⇒ the doc's own `max_change`, else the built-in 0.05 — a positive value overrides the
/// doc, as a CLI flag does. `control_json` is a `SessionControl` (objectives_v0.1) JSON
/// (null/empty ⇒ the doc's own control settings — `optimize` / `movement` / `constraints`, ADR
/// 0033 — else plain `drift`). On failure returns
/// null and, if `err` is non-null, sets `*err` to an error string (caller frees via
/// [`engines_string_free`]).
///
/// # Safety
/// `loop_doc_json` / `control_json` must be valid NUL-terminated C strings (or null where
/// allowed); `err` must be null or a valid `char**`.
#[no_mangle]
pub unsafe extern "C" fn engines_session_new(
    loop_doc_json: *const c_char,
    sample_rate: c_double,
    max_loops: i64,
    max_change: f32,
    seed: u64,
    control_json: *const c_char,
    err: *mut *mut c_char,
) -> *mut EnginesSession {
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<EnginesSession, String> {
        if loop_doc_json.is_null() {
            return Err("loop_doc_json is null".into());
        }
        let json = unsafe { CStr::from_ptr(loop_doc_json) }.to_str().map_err(|e| e.to_string())?;
        let control = if control_json.is_null() {
            None
        } else {
            let s = unsafe { CStr::from_ptr(control_json) }.to_str().map_err(|e| e.to_string())?;
            if s.trim().is_empty() { None } else { Some(crate::contract::SessionControl::from_json_str(s).map_err(|e| format!("control_json: {e}"))?) }
        };
        let doc = LoopDoc::from_json_str(json).map_err(|e| e.to_string())?;
        let cfg = SessionConfig {
            sample_rate: if sample_rate > 0.0 { sample_rate } else { 48_000.0 },
            max_loops: if max_loops < 0 { None } else { Some(max_loops as usize) },
            ..Default::default()
        };
        let dither = DitherConfig { max_change_per_loop: resolve_max_change(max_change, &doc), seed, ..Default::default() };
        Ok(EnginesSession { inner: Session::start(doc, cfg, dither, control), pending_records: Vec::new(), pending_midi: Vec::new() })
    }));
    match outcome {
        Ok(Ok(s)) => Box::into_raw(Box::new(s)),
        Ok(Err(msg)) => {
            unsafe { set_err(err, msg) };
            ptr::null_mut()
        }
        Err(_) => {
            unsafe { set_err(err, "panic in engines_session_new".into()) };
            ptr::null_mut()
        }
    }
}

/// Pull up to `cap` interleaved-stereo samples into `out`; returns the count written
/// (0 ⇒ ring momentarily empty — see [`engines_session_finished`]). RT-safe: pops only.
///
/// # Safety
/// `out` must point to at least `cap` writable `f32`s; `session` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn engines_session_pull_audio(session: *mut EnginesSession, out: *mut f32, cap: usize) -> usize {
    if session.is_null() || out.is_null() || cap == 0 {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let s = unsafe { &mut *session };
        let buf = unsafe { std::slice::from_raw_parts_mut(out, cap) };
        s.inner.pull_audio(buf)
    }))
    .unwrap_or(0)
}

/// Serialize newly-emitted per-loop records (one JSON object per rendered loop: loop id, the
/// controller's move, Orrery transfer, song block, peak) into `buf` as newline-delimited JSON.
/// Writes as many whole lines as fit; returns bytes written; undelivered records are retained for
/// the next call. Records are small (≈300 B); a few KiB of `cap` is plenty. Not for the audio thread.
///
/// # Safety
/// `buf` must point to at least `cap` writable bytes; `session` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn engines_session_poll_records_json(session: *mut EnginesSession, buf: *mut c_char, cap: usize) -> usize {
    if session.is_null() || buf.is_null() || cap == 0 {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let s = unsafe { &mut *session };
        s.pending_records.extend(s.inner.poll_records());
        let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, cap) };
        drain_json_lines(&mut s.pending_records, out)
    }))
    .unwrap_or(0)
}

/// Serialize newly-emitted MIDI-out events (ADR 0026) into `buf` as newline-delimited JSON —
/// one `{"abs_frame":N,"msg":{"type":"note_on",…}}` object per line, in emission order. Writes
/// as many whole lines as fit; returns bytes written; undelivered events are retained for the
/// next call. `abs_frame` is the absolute audio-frame position (session-start origin — the
/// same clock a frames-played counter in the audio callback measures): the shell holds each
/// message until playback reaches it, then sends it to the OS MIDI port. A session-end flush
/// appends Note-Offs + CC 123 per used channel. Events are small (≈80 B); a few KiB of `cap`
/// is plenty. Not for the audio thread.
///
/// # Safety
/// `buf` must point to at least `cap` writable bytes; `session` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn engines_session_poll_midi_json(session: *mut EnginesSession, buf: *mut c_char, cap: usize) -> usize {
    if session.is_null() || buf.is_null() || cap == 0 {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let s = unsafe { &mut *session };
        s.pending_midi.extend(s.inner.poll_midi());
        let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, cap) };
        drain_json_lines(&mut s.pending_midi, out)
    }))
    .unwrap_or(0)
}

/// Apply a runtime control edit (Phase 1, CE-R8.2): parse a `SessionControl` (objectives_v0.1) JSON
/// and hand it to the live session; it takes effect at the **next loop boundary** with re-base.
/// Returns `true` on success; on parse failure returns `false` and, if `err` is non-null, sets `*err`
/// (caller frees via [`engines_string_free`]).
///
/// # Safety
/// `control_json` must be a valid NUL-terminated C string; `session` a live handle; `err` null or a
/// valid `char**`.
#[no_mangle]
pub unsafe extern "C" fn engines_session_apply_control(session: *mut EnginesSession, control_json: *const c_char, err: *mut *mut c_char) -> bool {
    if session.is_null() || control_json.is_null() {
        return false;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let s = unsafe { &mut *session };
        let json = unsafe { CStr::from_ptr(control_json) }.to_str().map_err(|e| e.to_string())?;
        let control = crate::contract::SessionControl::from_json_str(json).map_err(|e| format!("control_json: {e}"))?;
        s.inner.apply_control(control);
        Ok(())
    }));
    match outcome {
        Ok(Ok(())) => true,
        Ok(Err(msg)) => {
            unsafe { set_err(err, msg) };
            false
        }
        Err(_) => {
            unsafe { set_err(err, "panic in engines_session_apply_control".into()) };
            false
        }
    }
}

/// Serialize the latest read-only telemetry snapshot (operating point, constraint status, Orrery
/// state, track audibility, song position) into `buf` as a single JSON object; returns bytes written (0 if
/// no snapshot yet or it doesn't fit). Not for the audio thread; `cap` should comfortably exceed the
/// operating point (≥ 64 KiB recommended).
///
/// # Safety
/// `buf` must point to at least `cap` writable bytes; `session` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn engines_session_poll_telemetry_json(session: *mut EnginesSession, buf: *mut c_char, cap: usize) -> usize {
    if session.is_null() || buf.is_null() || cap == 0 {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let s = unsafe { &*session };
        let Some(t) = s.inner.poll_telemetry() else { return 0 };
        let json = match serde_json::to_string(&t) {
            Ok(j) => j,
            Err(_) => return 0,
        };
        if json.len() > cap {
            return 0;
        }
        let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, cap) };
        out[..json.len()].copy_from_slice(json.as_bytes());
        json.len()
    }))
    .unwrap_or(0)
}

/// True once the worker has finished **and** the audio ring is fully drained — safe to stop. A
/// worker that panicked also reports finished (the panic message goes to stderr), so a host never
/// waits on a dead session.
///
/// # Safety
/// `session` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn engines_session_finished(session: *const EnginesSession) -> bool {
    if session.is_null() {
        return true;
    }
    catch_unwind(AssertUnwindSafe(|| unsafe { (*session).inner.finished() })).unwrap_or(true)
}

/// True once the worker has stopped producing audio (a bounded run ended, or `stop`) — the signal
/// to play out the tail and stop the device.
///
/// # Safety
/// `session` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn engines_session_worker_finished(session: *const EnginesSession) -> bool {
    if session.is_null() {
        return true;
    }
    catch_unwind(AssertUnwindSafe(|| unsafe { (*session).inner.worker_finished() })).unwrap_or(true)
}

/// Signal the worker to stop at the next loop boundary.
///
/// # Safety
/// `session` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn engines_session_stop(session: *mut EnginesSession) {
    if session.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe { (*session).inner.stop() }));
}

/// Stop, join the worker, and free the session. The handle is invalid afterward.
///
/// # Safety
/// `session` must be a handle from [`engines_session_new`] not already freed (or null).
#[no_mangle]
pub unsafe extern "C" fn engines_session_free(session: *mut EnginesSession) {
    if session.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(session));
    }));
}

/// Core version string (static; do not free).
#[no_mangle]
pub extern "C" fn engines_version() -> *const c_char {
    use std::sync::OnceLock;
    static V: OnceLock<CString> = OnceLock::new();
    V.get_or_init(|| CString::new(crate::version()).unwrap_or_default()).as_ptr()
}

/// Free a string previously returned via an out-parameter (e.g. the `err` from
/// [`engines_session_new`]).
///
/// # Safety
/// `s` must be a pointer returned by this library, freed at most once (or null).
#[no_mangle]
pub unsafe extern "C" fn engines_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn fixture_json() -> CString {
        CString::new(crate::test_support::fixture_json()).unwrap()
    }

    #[test]
    fn a_host_without_a_max_change_gets_the_docs() {
        let mut doc = crate::test_support::fixture();
        assert_eq!(resolve_max_change(-1.0, &doc), crate::controller::DEFAULT_MAX_CHANGE, "no doc value ⇒ the default");
        doc.max_change = Some(0.02);
        assert_eq!(resolve_max_change(-1.0, &doc), 0.02, "the doc's own");
        assert_eq!(resolve_max_change(f32::NAN, &doc), 0.02);
        assert_eq!(resolve_max_change(0.0, &doc), 0.02);
        assert_eq!(resolve_max_change(0.08, &doc), 0.08, "an explicit override wins");
    }

    #[test]
    fn ffi_roundtrip_stream_and_records() {
        let json = fixture_json();
        let mut err: *mut c_char = ptr::null_mut();
        // low SR for a fast debug render; 2 bounded loops
        let s = unsafe { engines_session_new(json.as_ptr(), 8_000.0, 2, 0.05, 9, ptr::null(), &mut err) };
        assert!(!s.is_null(), "session created");
        assert!(err.is_null());

        let mut audio = 0usize;
        let mut buf = vec![0.0f32; 4096];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let n = unsafe { engines_session_pull_audio(s, buf.as_mut_ptr(), buf.len()) };
            if n > 0 {
                audio += n;
            } else if unsafe { engines_session_finished(s) } {
                break;
            } else {
                assert!(std::time::Instant::now() < deadline, "drain timeout");
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
        }
        assert!(audio > 0, "streamed audio");
        assert!(unsafe { engines_session_worker_finished(s) }, "audio production finished");

        // one record per loop, as JSON lines
        let mut jbuf = vec![0u8; 64 * 1024];
        let nbytes = unsafe { engines_session_poll_records_json(s, jbuf.as_mut_ptr() as *mut c_char, jbuf.len()) };
        let text = std::str::from_utf8(&jbuf[..nbytes]).unwrap();
        let recs: Vec<LoopRecord> = text.lines().filter(|l| !l.is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(recs.len(), 2, "one record per loop");
        assert_eq!(recs[1].loop_id, recs[0].loop_id + 1);
        assert_eq!(recs[1].start_frame, recs[0].num_frames as u64);

        unsafe { engines_session_free(s) };
    }

    #[test]
    fn ffi_poll_midi_json_streams_timed_events() {
        // A midi-bearing doc (ADR 0026): the poll must deliver newline-delimited TimedMidi
        // JSON — the loop-1 CC burst + Note-Ons, then the session-end flush (Offs + CC 123).
        let json = CString::new(
            r#"{
            "schema_version":"0.9","loop_id":1,
            "transport":{"bpm":240,"master_steps":4,"step_unit":"beat"},
            "harmony":{"root":"D","mode":"dorian"},
            "instruments":{"ext":{"enabled":true,"pitched":true,"type":"midi",
                "note_engine":{"polyphony":4,"glide_ms":0},
                "params":{"cutoff":0.5},
                "routing":{"channel":3,"cc":{"cutoff":74}}}},
            "modulators":{"chords":{"type":"step_seq","output":"note","sync":"loop","length":16,
                "steps":[{"step":0,"voicing":{"v1":{"offset":0,"velocity":96},"v2":{"offset":7}}}]}},
            "matrix":{"m_n":{"source":"chords","dest":"instruments.ext.notes","kind":"note","voices":4,"priority":1}}
        }"#,
        )
        .unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let s = unsafe { engines_session_new(json.as_ptr(), 8_000.0, 2, 0.05, 11, ptr::null(), &mut err) };
        assert!(!s.is_null(), "session created");

        // drain audio to completion (bounded 2 loops)
        let mut buf = vec![0.0f32; 4096];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let n = unsafe { engines_session_pull_audio(s, buf.as_mut_ptr(), buf.len()) };
            if n == 0 {
                if unsafe { engines_session_finished(s) } {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "drain timeout");
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
        }

        let mut mbuf = vec![0u8; 64 * 1024];
        let n = unsafe { engines_session_poll_midi_json(s, mbuf.as_mut_ptr() as *mut c_char, mbuf.len()) };
        assert!(n > 0, "midi events delivered");
        let text = std::str::from_utf8(&mbuf[..n]).unwrap();
        let evs: Vec<crate::midi::TimedMidi> =
            text.lines().filter(|l| !l.is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
        use crate::midi::MidiMsg::*;
        // loop 1: CC burst + 2 Ons at abs 0; loop 2 sustains (nothing); exit flush: 2 Offs + CC123.
        assert_eq!(evs.len(), 6, "{evs:?}");
        assert_eq!(evs[0].msg, Cc { ch: 2, cc: 74, value: 64 });
        assert!(matches!(evs[1].msg, NoteOn { ch: 2, note: 62, vel: 96 }), "{evs:?}");
        assert!(matches!(evs.last().unwrap().msg, Cc { cc: 123, .. }), "flush last: {evs:?}");
        assert!(evs.last().unwrap().abs_frame > 0, "flush stamped at end-of-audio");
        // drained: a second poll returns nothing
        assert_eq!(unsafe { engines_session_poll_midi_json(s, mbuf.as_mut_ptr() as *mut c_char, mbuf.len()) }, 0);
        unsafe { engines_session_free(s) };
    }

    #[test]
    fn ffi_bad_json_sets_error() {
        let json = CString::new("{ not valid").unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let s = unsafe { engines_session_new(json.as_ptr(), 8_000.0, 1, 0.05, 1, ptr::null(), &mut err) };
        assert!(s.is_null());
        assert!(!err.is_null(), "error string set");
        let msg = unsafe { CStr::from_ptr(err) }.to_str().unwrap().to_string();
        assert!(!msg.is_empty());
        unsafe { engines_string_free(err) };
    }

    #[test]
    fn ffi_apply_control_parses_and_applies() {
        let json = fixture_json();
        let mut err: *mut c_char = ptr::null_mut();
        let s = unsafe { engines_session_new(json.as_ptr(), 8_000.0, 4, 0.05, 1, ptr::null(), &mut err) };
        assert!(!s.is_null());
        // a valid objectives_v0.1 control applies (guardrails/movement/etc. all default)
        let ctrl = CString::new(r#"{"revision":7,"optimize":"hold","constraints":{"global.master_gain_db":{"min":-6.02,"max":-4.44}}}"#).unwrap();
        assert!(unsafe { engines_session_apply_control(s, ctrl.as_ptr(), &mut err) }, "valid control applied");
        // malformed JSON → false + error string set
        let bad = CString::new("{ not json").unwrap();
        assert!(!unsafe { engines_session_apply_control(s, bad.as_ptr(), &mut err) });
        assert!(!err.is_null(), "parse error surfaced");
        unsafe { engines_string_free(err) };
        unsafe { engines_session_free(s) };
    }

    #[test]
    fn ffi_version_nonempty() {
        let v = unsafe { CStr::from_ptr(engines_version()) }.to_str().unwrap();
        assert!(!v.is_empty());
    }
}
