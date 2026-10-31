//! Desktop MIDI-out delivery (feature `midi`, ADR 0026) — the shell side of the seam.
//!
//! The core produces deterministic [`TimedMidi`] events (absolute-sample-stamped, via
//! `Session::poll_midi` / `engines_session_poll_midi_json`); this module *delivers* them: a
//! [`MidiOut`] virtual CoreMIDI source ("Engines Out" — appears directly as a MIDI input in
//! Ableton/Logic, no IAC setup) plus a [`MidiPump`] thread that holds each message until the
//! shell's **frames-played** counter (incremented by the audio callback — the DAC clock)
//! reaches its `abs_frame`, then sends it. Ring lookahead means events arrive well ahead of
//! due time; the ~2 ms pump tick bounds delivery jitter — inaudible for ambient work (the
//! CoreMIDI-timestamped Swift path is the precision upgrade, ADR 0026 "Phasing").
//!
//! Like cpal, this is **bin/shell-side I/O living behind a feature gate** so the
//! default core build stays I/O-free (CE-R1.2). The [`MidiScheduler`] itself is pure (fake
//! clock + capture closure in tests); only [`MidiOut`] touches the OS.
//!
//! Hung-note safety on shutdown: [`MidiScheduler::shutdown`] delivers the *safety-relevant*
//! remainder only — queued Note-Offs and CC — while **discarding queued future Note-Ons**
//! (rendered-ahead notes that never sounded must not fire at exit), then belt-and-braces
//! **CC 123 (All Notes Off)** on every channel it ever sent on.

use crate::midi::{MidiMsg, TimedMidi};
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

/// Encode one message as raw MIDI bytes (status | channel, data1, data2).
pub fn encode(msg: MidiMsg) -> [u8; 3] {
    match msg {
        MidiMsg::NoteOn { ch, note, vel } => [0x90 | (ch & 0x0F), note & 0x7F, vel & 0x7F],
        // explicit Note-Off with the conventional release velocity 64
        MidiMsg::NoteOff { ch, note } => [0x80 | (ch & 0x0F), note & 0x7F, 0x40],
        MidiMsg::Cc { ch, cc, value } => [0xB0 | (ch & 0x0F), cc & 0x7F, value & 0x7F],
    }
}

/// Heap entry ordered by `(abs_frame, seq)` — `seq` preserves emission order within a frame
/// (the engine emits CC before Note diffs; a binary heap alone is not stable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Queued {
    abs_frame: u64,
    seq: u64,
    msg: MidiMsg,
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.abs_frame, self.seq).cmp(&(other.abs_frame, other.seq))
    }
}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Pure playback-clocked scheduler: queue [`TimedMidi`] as they're polled (render runs ahead
/// of playback), release them in `(abs_frame, emission)` order once the clock reaches them.
#[derive(Default)]
pub struct MidiScheduler {
    heap: BinaryHeap<Reverse<Queued>>,
    seq: u64,
    /// Every wire channel a message was actually sent on (for the shutdown CC 123 sweep).
    channels_sent: BTreeSet<u8>,
}

impl MidiScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a batch (any order; the heap orders delivery).
    pub fn push(&mut self, events: impl IntoIterator<Item = TimedMidi>) {
        for e in events {
            self.seq += 1;
            self.heap.push(Reverse(Queued { abs_frame: e.abs_frame, seq: self.seq, msg: e.msg }));
        }
    }

    /// Send every event due at `now` (frames played since session start) via `send`.
    pub fn pump(&mut self, now: u64, send: &mut impl FnMut(&[u8])) {
        while let Some(Reverse(q)) = self.heap.peek().copied() {
            if q.abs_frame > now {
                break;
            }
            self.heap.pop();
            self.deliver(q.msg, send);
        }
    }

    /// Shutdown drain: deliver remaining **Note-Offs and CC only** (a queued future Note-On
    /// never sounded — firing it at exit would *start* a note), then CC 123 (All Notes Off)
    /// on every channel this scheduler ever sent on.
    pub fn shutdown(&mut self, send: &mut impl FnMut(&[u8])) {
        self.flush(send);
    }

    /// [`shutdown`](Self::shutdown)'s drain, leaving the scheduler usable: what a shell does when a
    /// session stops but the port stays open for the next one (ADR 0048) — its queued Offs and CC
    /// now, its rendered-ahead Note-Ons never, CC 123 on every channel used, and a clean slate.
    pub fn flush(&mut self, send: &mut impl FnMut(&[u8])) {
        while let Some(Reverse(q)) = self.heap.pop() {
            match q.msg {
                MidiMsg::NoteOn { .. } => {}
                m => self.deliver(m, send),
            }
        }
        for ch in std::mem::take(&mut self.channels_sent) {
            send(&encode(MidiMsg::Cc { ch, cc: 123, value: 0 }));
        }
    }

    fn deliver(&mut self, msg: MidiMsg, send: &mut impl FnMut(&[u8])) {
        let ch = match msg {
            MidiMsg::NoteOn { ch, .. } | MidiMsg::NoteOff { ch, .. } | MidiMsg::Cc { ch, .. } => ch,
        };
        self.channels_sent.insert(ch);
        send(&encode(msg));
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

/// A virtual MIDI **source** the OS exposes to other apps (CoreMIDI on macOS via midir).
/// Ableton: enable the port's Track input and arm — no IAC bus needed.
pub struct MidiOut {
    conn: midir::MidiOutputConnection,
}

impl MidiOut {
    pub fn create_virtual(port_name: &str) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use midir::os::unix::VirtualOutput;
            let out = midir::MidiOutput::new("Engines").map_err(|e| e.to_string())?;
            let conn = out.create_virtual(port_name).map_err(|e| e.to_string())?;
            Ok(Self { conn })
        }
        #[cfg(not(unix))]
        {
            let _ = port_name;
            Err("virtual MIDI ports are not supported on this platform".into())
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.conn.send(bytes);
    }
}

/// Pump-tick period: bounds delivery jitter (≈2–4 ms worst case) without busy-spinning.
const PUMP_TICK: Duration = Duration::from_millis(2);

/// What the pump's owner sends it.
enum Cmd {
    Batch(Vec<TimedMidi>),
    Flush,
}

/// Where a pump's bytes go (a port, or a test's capture).
pub type MidiSink = Box<dyn FnMut(&[u8]) + Send + 'static>;
/// A pump's clock: frames played, or `None` while there is no clock (everything waits).
pub type MidiClock = Box<dyn Fn() -> Option<u64> + Send + 'static>;

/// The delivery thread: owns the port + scheduler; the session owner feeds it polled
/// [`TimedMidi`] batches (`push`) and the audio callback's frames-played counter is its
/// clock. Dropping it (or [`stop`](MidiPump::stop)) runs the safety shutdown drain.
pub struct MidiPump {
    tx: Option<mpsc::Sender<Cmd>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MidiPump {
    /// Create the virtual port and spawn the pump. `frames_played` is the DAC clock: audio
    /// frames the output callback has consumed since session start (count them even through
    /// underrun silence — the clock advances regardless).
    pub fn spawn(port_name: &str, frames_played: Arc<AtomicU64>) -> Result<Self, String> {
        let mut port = MidiOut::create_virtual(port_name)?;
        Ok(Self::spawn_with(Box::new(move |b| port.send(b)), Box::new(move || Some(frames_played.load(Ordering::Acquire)))))
    }

    /// A pump over any sink and clock (a shell whose clock changes with each session; tests).
    pub fn spawn_with(mut sink: MidiSink, clock: MidiClock) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handle = thread::spawn(move || {
            let mut sched = MidiScheduler::new();
            let take = |sched: &mut MidiScheduler, sink: &mut MidiSink| {
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Batch(batch) => sched.push(batch),
                        Cmd::Flush => sched.flush(&mut |b| sink(b)),
                    }
                }
            };
            loop {
                take(&mut sched, &mut sink);
                if let Some(now) = clock() {
                    sched.pump(now, &mut |b| sink(b));
                }
                if stop_t.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(PUMP_TICK);
            }
            // final drain: late-arriving batches (e.g. the session's exit flush), then the
            // Offs-only shutdown + CC 123 sweep.
            take(&mut sched, &mut sink);
            sched.shutdown(&mut |b| sink(b));
        });
        Self { tx: Some(tx), stop, handle: Some(handle) }
    }

    /// Feed newly-polled events (call with `session.poll_midi()` on the owner's poll cadence).
    pub fn push(&self, events: Vec<TimedMidi>) {
        if events.is_empty() {
            return;
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Batch(events));
        }
    }

    /// Flush without stopping ([`MidiScheduler::flush`]): queued Offs and CC go out now, queued
    /// Note-Ons never, then CC 123 on every channel used. Ordered after every earlier `push`.
    pub fn flush(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Flush);
        }
    }

    /// Stop the pump: runs the shutdown drain (queued Offs + CC 123 per used channel) and joins.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.tx.take(); // unblock any pending recv semantics; signals no more batches
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MidiPump {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(abs_frame: u64, msg: MidiMsg) -> TimedMidi {
        TimedMidi { abs_frame, msg }
    }
    fn on(ch: u8, note: u8) -> MidiMsg {
        MidiMsg::NoteOn { ch, note, vel: 100 }
    }
    fn off(ch: u8, note: u8) -> MidiMsg {
        MidiMsg::NoteOff { ch, note }
    }

    /// Capture-send helpers: run one scheduler op, return the bytes it sent.
    fn pump_capture(s: &mut MidiScheduler, now: u64) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::new();
        s.pump(now, &mut |b: &[u8]| out.push(b.to_vec()));
        out
    }
    fn shutdown_capture(s: &mut MidiScheduler) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::new();
        s.shutdown(&mut |b: &[u8]| out.push(b.to_vec()));
        out
    }

    #[test]
    fn encode_wire_bytes() {
        assert_eq!(encode(MidiMsg::NoteOn { ch: 2, note: 62, vel: 96 }), [0x92, 62, 96]);
        assert_eq!(encode(MidiMsg::NoteOff { ch: 2, note: 62 }), [0x82, 62, 0x40]);
        assert_eq!(encode(MidiMsg::Cc { ch: 15, cc: 74, value: 127 }), [0xBF, 74, 127]);
    }

    #[test]
    fn pump_releases_only_due_events_in_frame_then_emission_order() {
        let mut s = MidiScheduler::new();
        // pushed out of order across batches; same-frame events must keep push order (CC first).
        s.push([ev(100, on(0, 64)), ev(500, off(0, 62))]);
        s.push([ev(100, MidiMsg::Cc { ch: 0, cc: 74, value: 10 })]);
        s.push([ev(0, on(0, 62))]);

        let sent = pump_capture(&mut s, 99);
        assert_eq!(sent, vec![encode(on(0, 62)).to_vec()], "only frame-0 due at t=99");

        let sent = pump_capture(&mut s, 100);
        // frame-100 events: the On (seq 1) precedes the CC (seq 3)? No — heap orders by
        // (frame, seq): On@100 was pushed before Cc@100, so On first. Emission order holds.
        assert_eq!(sent, vec![encode(on(0, 64)).to_vec(), encode(MidiMsg::Cc { ch: 0, cc: 74, value: 10 }).to_vec()]);

        let sent = pump_capture(&mut s, 400);
        assert!(sent.is_empty(), "frame-500 Off not due yet");
        let sent = pump_capture(&mut s, 500);
        assert_eq!(sent, vec![encode(off(0, 62)).to_vec()]);
        assert!(s.is_empty());
    }

    #[test]
    fn shutdown_sends_offs_and_cc_but_never_future_note_ons() {
        let mut s = MidiScheduler::new();
        s.push([ev(0, on(2, 62))]);
        let _ = pump_capture(&mut s, 0);
        s.push([
            ev(10_000, on(2, 64)),                              // future On: must be discarded
            ev(10_000, off(2, 62)),                             // future Off: must be delivered
            ev(10_000, MidiMsg::Cc { ch: 2, cc: 123, value: 0 }), // session flush CC: delivered
        ]);
        let sent = shutdown_capture(&mut s);
        assert_eq!(
            sent,
            vec![
                encode(off(2, 62)).to_vec(),
                encode(MidiMsg::Cc { ch: 2, cc: 123, value: 0 }).to_vec(),
                // belt-and-braces CC 123 sweep over every channel we ever sent on
                encode(MidiMsg::Cc { ch: 2, cc: 123, value: 0 }).to_vec(),
            ],
            "no future Note-On may fire at exit"
        );
    }

    #[test]
    fn a_flush_ends_what_sounds_drops_what_never_did_and_keeps_running() {
        let mut s = MidiScheduler::new();
        s.push([ev(0, on(3, 60)), ev(500, on(3, 62)), ev(500, off(3, 60))]);
        let _ = pump_capture(&mut s, 0);
        let mut out: Vec<Vec<u8>> = Vec::new();
        s.flush(&mut |b: &[u8]| out.push(b.to_vec()));
        assert_eq!(out, vec![encode(off(3, 60)).to_vec(), encode(MidiMsg::Cc { ch: 3, cc: 123, value: 0 }).to_vec()]);
        assert!(s.is_empty());
        // Still usable: a new session's events play, and the next sweep covers only its channels.
        s.push([ev(0, on(5, 64))]);
        assert_eq!(pump_capture(&mut s, 0), vec![encode(on(5, 64)).to_vec()]);
        assert_eq!(shutdown_capture(&mut s), vec![encode(MidiMsg::Cc { ch: 5, cc: 123, value: 0 }).to_vec()]);
    }

    #[test]
    fn a_pump_over_a_sink_follows_its_clock_and_flushes_in_order() {
        use std::sync::Mutex;
        let sent: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let clock: Arc<AtomicU64> = Arc::default();
        let (s2, c2) = (sent.clone(), clock.clone());
        let mut pump = MidiPump::spawn_with(Box::new(move |b| s2.lock().unwrap().push(b.to_vec())), Box::new(move || Some(c2.load(Ordering::Acquire))));
        pump.push(vec![ev(0, on(0, 60)), ev(1_000, on(0, 62)), ev(1_000, off(0, 60))]);
        let wait = |n: usize| {
            let t0 = std::time::Instant::now();
            while sent.lock().unwrap().len() < n && t0.elapsed() < Duration::from_secs(5) {
                thread::sleep(Duration::from_millis(1));
            }
        };
        wait(1);
        assert_eq!(*sent.lock().unwrap(), vec![encode(on(0, 60)).to_vec()], "only frame 0 is due");
        pump.flush();
        wait(3);
        assert_eq!(
            *sent.lock().unwrap(),
            vec![encode(on(0, 60)).to_vec(), encode(off(0, 60)).to_vec(), encode(MidiMsg::Cc { ch: 0, cc: 123, value: 0 }).to_vec()],
            "the flush ends the note, drops the future On"
        );
        pump.stop();
        assert_eq!(sent.lock().unwrap().len(), 3, "nothing left to sweep");
    }

    #[test]
    fn shutdown_cc123_sweep_covers_all_sent_channels_only() {
        let mut s = MidiScheduler::new();
        s.push([ev(0, on(1, 60)), ev(0, on(5, 72))]);
        let _ = pump_capture(&mut s, 0);
        let sent = shutdown_capture(&mut s);
        assert_eq!(
            sent,
            vec![
                encode(MidiMsg::Cc { ch: 1, cc: 123, value: 0 }).to_vec(),
                encode(MidiMsg::Cc { ch: 5, cc: 123, value: 0 }).to_vec(),
            ]
        );
        // an untouched scheduler sends nothing at shutdown
        let mut fresh = MidiScheduler::new();
        assert!(shutdown_capture(&mut fresh).is_empty());
    }
}
