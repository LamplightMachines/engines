# ADR 0026 — `midi` instrument type: external-MIDI translation at playback time

- **Status:** accepted (Phases A+B+C+D1 built: core events, session/FFI stream, desktop shell
  delivery, and DAW-audio-return with async-mode scoring; D2 — pursue's lagged pipeline — is
  the staged follow-up, see "Phasing")
- **Date:** 2026-07-01
- **Amended by:** [ADR 0048](0048-midi-mirror-tracks-for-live.md) (2026-10-22) — an *audio* instrument
  can also send its notes as MIDI (the mirror, a shell setting keyed by instrument id, not a channel),
  and `sme-live` delivers it on up to two virtual ports ("SME Out", "SME Out 2"). A live edit that
  removes a `midi` instrument's route now sends its Note-Offs at the edit point, not a loop later.
- **Context:** we want the engine (Orrery harmony + drift/pursue modulation) to drive *external*
  instruments — a hardware synth or a DAW like Ableton — as a generative brain: note sequences out
  as **MIDI Note-On/Off**, control sequences (including free-running LFO modulation) out as
  **MIDI CC**, delivered to a virtual MIDI destination *while the music plays*. Later, the DAW's
  audio returns into the final mix + CLAP scoring (MIDI out / audio stream back). Faster-than-
  real-time rendering of the MIDI stream is explicitly not required (streaming playback is already
  real-time-paced by ring backpressure in `session.rs`).
- **Code anchors:** `crates/sme_core/src/{midi.rs, loop_doc.rs, synth.rs, engine.rs, voice.rs,
  controller.rs, categorical.rs, session.rs, ffi.rs}`, `contracts/loop_contract_v0.8.schema.json`,
  `doc/loop_contract_v0.8.json5`.

## Decision

Add a fourth instrument **`type: "midi"`** (ADR 0009's seam): a **translator sink** that produces
**no audio**. It plugs into the existing matrix exactly like a synth — note connections land on
`instruments.<i>.notes` (with the shared polyphony/priority/steal semantics), mod connections land
on `instruments.<i>.params.<p>` — but its output is `RenderResult.midi_events` instead of samples.

- **Doc surface.** `routing: { channel: 1..16, cc: { <param> → 0..127 } }` (required for the type;
  channel collisions between midi instruments are linted). `velocity_default: 1..127` (⇒ 100) on
  the instrument; optional `velocity: 1..127` per voicing entry — both **native MIDI units** at the
  doc boundary (ADR 0005 seam rule). A midi instrument's `params` are free-form named 0..1
  value-holders (CC sources / mod destinations); `routing.cc` names must exist in `params`
  (validated at the authoring boundary, `LoopDoc::from_json_str`). **Port/device binding is a
  shell concern** (CLI flag), never doc content — the core stays I/O-free (CE-R1.2).
- **Note translation = voice-set diffing, not action mapping.** The engine's note semantics are
  "absent ⇒ sustain, never a dropout" (design-rationale §2.5) and voice **steals/prunes emit no
  `Action`** — so Note-Offs cannot be derived from events. Instead the engine reconciles the
  instrument's **desired note set** (its held, un-released voices) against the **last-emitted set**
  carried in `PhaseSnapshot.midi_out` (`midi::diff_notes`), at loop start and after each note
  event lands (post-`enforce_polyphony`, post-release-stamp):
  - sustained voice, same pitch (incl. across loops) → **nothing** (the external synth holds);
  - new voice → **Note-On** (velocity: voicing's, else last-emitted, else default);
  - pitch change → **On(new) then Off(old)** — legato overlap so the *external* synth's
    portamento glides (plain MIDI has no per-note bend; a future MPE mode would);
  - vanished voice (release / steal / prune / disable / doc-swap removal) → **Note-Off**;
  - note numbers are **ref-counted** across voices, so unison (cents-detuned doubles → one note#)
    never half-dies; `note# = clamp(round(60 + root_semitone + offset), 0..127)` — the same anchor
    as `pitch_to_hz`; `cents_offset` is **dropped** in plain MIDI.
  - Safety flushes: instrument disabled / removed / channel edited → Offs at loop start (a channel
    edit restarts the CC state too); session stop → session-level Note-Offs + **CC 123** per used
    channel. Release-tail for midi is 0 (the Off goes out at the release frame; the external synth
    owns its own tail).
- **CC translation.** Per `routing.cc` entry, the param's **effective** value (base + Σ matrix
  modulation, incl. free-running LFOs — the same `cur_vals` pipeline the synths read) is sampled
  on the `CONTROL_HOP` grid (64 samples ≈ 1.3 ms), quantized to 7 bits, and emitted **on change
  only** — an empty snapshot naturally produces the initial full sync burst, and objectives-file
  `overrides` flow through (they rewrite the base). Worst case ≈ 750 msg/s per param (virtual
  ports are fine; a min-interval throttle is a documented future knob).
- **Determinism (CE-R7.3).** `midi_events` are a pure function of `(doc, snapshot, loop_id)`
  (emitted state lives in the snapshot; bit-repro + snapshot-resume tested). The session tags each
  played event with `abs_frame = loop_start + frame` on the cumulative sample clock — **only the
  played `+δ` side emits**; `−δ` counterfactual renders discard theirs. Delivery timing is
  shell-side only (poll via `sme_session_poll_midi_json`, schedule against a frames-played
  counter).
- **Learning stays off midi params; movement is opt-in (perturb-only).** A midi param's effect
  exists only in the *external* audio of the played side — the `−δ` antithetic counterfactual can
  never contain it, so ΔCLAP on it is structurally zero under common-mode return scoring.
  `dither_candidates` / `categorical_candidates` (and therefore the sweep rig) skip midi
  instruments **and** matrix depths whose dest resolves into one — by default. The
  **perturb-only opt-in** (`DitherConfig::include_midi`; `--dither-midi` on sme-play/TUI, plus a
  midi-gated "MIDIdith" Setup toggle) adds those numeric paths back to the drift/pursue
  candidates so the external synth's CCs *evolve*, while keeping learning honest: such pairs are
  flagged **`external_only`** in `SampleMeta` and **skipped by the online learner**
  (`path_targets_midi`) — movement without learning, until a per-side pairing scheme (twin-chain
  or temporal ±δ) makes midi-param ΔCLAP real. Categorical dither stays off midi unconditionally.

## Phasing

Built now: **(A+B)** core event generation + session/FFI stream (headless-testable), and
**(C)** shell delivery — cargo feature `midi` (`midir`; `playback` implies it) + `midi_out.rs`:
a virtual CoreMIDI source (default "SME Out") and a **playback-clocked pump** whose clock is a
frames-played atomic incremented by the audio callback (the DAC clock — sme-play counts buffer
lengths so it advances through underrun silence; the TUI reuses its viz play-cursor). ~2 ms tick
bounds delivery jitter. Shutdown is hung-note-safe end to end: the session's exit flush
(Note-Offs + CC 123, stamped at end-of-audio) rides the normal stream, and the pump's own
shutdown drain delivers remaining **Note-Offs/CC only — never rendered-ahead future Note-Ons** —
plus a CC 123 sweep over every channel it sent on. `sme-play --midi [--midi-port NAME]`; TUI
takes the same flags and badges midi translators on the Instrument screen.

**(D1) DAW-audio-return** (built): a doc bus may declare `type: "external_return"`
(`return_gain` + `latency_ms`, at most one, **no effects** — the core render never sees it;
validated). `sme-play --return-device "BlackHole 2ch"` opens the loopback input (must match the
output sample rate; mono duplicates, >2ch takes the first pair), feeds two SPSC rings from the
input callback, and **monitor-mixes** one into the output live at `return_gain` (route the
DAW's master ONLY to the loopback, or you hear it twice). The **TUI** exposes the same path
(`--return-device`, or a **"Return" selector on the Setup screen** shown only when `--midi` is on —
the same `EngineLink` input-stream/ring/anchor wiring as `sme-play`, drained in its poll tick). The other ring drains through
`Session::push_return_audio` (FFI: `sme_session_push_return_audio`) into a capture timeline
anchored on the output's frames-played clock at first capture and contiguous thereafter
(ppm-scale device drift folds into the user-calibrated `latency_ms`). In the **async scoring
modes (drift/hold)** each job waits (`SessionConfig::return_grace`, stop-aware) for its loop's
window at `loop_start + latency`, then mixes the *same* window into **both ±δ sides** before
scoring — common-mode, preserving the antithetic mandate; grace expiry falls back to
internal-only. DSP features/captures stay internal-render-derived (φ must remain reproducible);
only the scored signal carries the return, and those samples are flagged **`return_mixed`**
(SampleMeta + harness `Pair`; the engine-backed `rerender_baseline` skips + counts them —
stored-score consistency and ΔCLAP re-derivation never re-render and still hold). Pursue's
inline path keeps scoring the internal render until the **(D2)** lagged-pipeline follow-up.

**Audio mirror → output redirect (superseded by ADR 0027):** the original amendment here added
`--mirror-device` — a second output stream carrying a copy of the internal render so a DAW could
record the engine alongside the MIDI. Field use showed the copy topology is wrong once the DAW is
the monitor (the engine sounds twice), so ADR 0027 **replaced** it with `--output-device`: the
engine streams *only* to the named device (e.g. "BlackHole 16ch"), the DAW alone drives the real
interface, and per-instrument `out_channel` stems give the DAW a mixable stereo pair per
instrument. The return path's monitor mix is disabled when redirected (mixing the DAW's master
back into the stream the DAW consumes would loop); return *capture* for common-mode scoring is
unchanged.

**Fixed while validating perturb-only (pre-existing CE-R7.3 bug):** `Bus` used a fundsp `Net`
with `Shared` (`Arc<AtomicU32>`) param handles, so **`Engine::clone` aliased the live bus params**
between the worker's engine and each ScoreJob's `eng_pre` — the async −δ counterfactual render
raced the next played render's control-rate writes whenever a matrix route modulated a bus param
(e.g. shimmer), contaminating both the played audio and ΔCLAP by ~1e-4..1e-3 (the order of
LAION's usable signal). The chain is now hand-rolled (`effects.rs`: `FxNode` enum, plainly-owned
params) so `Bus: Clone` is genuinely deep; pinned by `bus_clone_is_deep_and_render_isolated` and
by the perturb-only session test's strict ΔCLAP ≡ 0 assertion on `external_only` pairs.

## Consequences

- A midi instrument is **benign silence** to the audio path: skipped in the synth loop, no filter
  or per-inst-RMS bookkeeping (absent RMS reads "no dry signal" → `audible=false` in telemetry —
  the TUI should badge midi tracks instead of warning "silent"). No default reverb send.
- Voice allocation is shared, so chords/arps/Orrery drive external synths with the same fan-in
  arbitration as internal ones; the same note source can fan out to a synth *and* a midi
  instrument (audio + MIDI in parallel).
- Headless faster-than-real-time modes (`sme-render`/`sme-run`) still *produce* deterministic
  `midi_events` but have no playback clock to deliver against; a MIDI-file export is the natural
  future consumer.
- **Return-scoring pacing (D1):** waiting for playback makes scoring throughput = playback rate.
  Steady state is self-pacing (the full ring paces the render worker to real time anyway), but
  the initial render-ahead transient means `Backpressure::DropScoring` can drop the first
  ~ring-lead/loop-duration loops' scoring (counted in `scoring_dropped`, as ever);
  `BlockForCorpus` (`--out`) paces the worker instead and keeps the corpus complete without
  starving audio (audio for a loop is always pushed before its job).
- A disable→re-enable cycle re-sounds held voices at `velocity_default` (the emitted state was
  flushed); authored velocities re-assert on the next articulation.
- MPE (per-voice channels: true glide + `cents_offset` as bend), MIDI-Clock-master, multi-port
  routing, and 14-bit CC are explicitly deferred (`routing.mode` is the natural extension seam).
