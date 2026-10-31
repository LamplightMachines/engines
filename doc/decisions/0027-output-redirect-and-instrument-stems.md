# ADR 0027 — Output redirect + per-instrument `out_channel` stems

- **Status:** accepted (built: core stem tap, session composition, sme-play + TUI redirect)
- **Date:** 2026-07-04
- **Context:** with MIDI-out (ADR 0026) driving a DAW and the DAW's audio returning for scoring,
  the studio workflow wants the DAW to be the **mixing console**: the engine's audio should go
  *into* the DAW (over a loopback like BlackHole), the DAW alone drives the real interface, and
  each engine instrument should be mixable on its own DAW track. The previous **audio mirror**
  (`--mirror-device`, an ADR 0026 amendment) double-played — the engine on the speakers *and* a
  copy into the DAW — which is the wrong topology once the DAW monitors: you hear both paths.
- **Code anchors:** `crates/sme_core/src/{loop_doc.rs, engine.rs, session.rs, bin/sme-play.rs}`,
  `crates/sme_tui/src/{engine_link.rs, app.rs}`, `contracts/loop_contract_v0.8.schema.json`.

## Decision

**1. Redirect, not mirror.** `--output-device <N>` (sme-play + TUI; the TUI Setup screen's
always-present **"Output"** selector, `"(default)"` = play locally) makes the named device the
engine's **only** output — nothing plays locally; the device's consumer (the DAW) is the monitor
and the sole program driving the real interface. The mirror (second stream + SPSC tap ring) is
**removed**: redirect is strictly simpler (one stream, one clock — no cross-device drift ring).
Guardrails: the redirect target must differ from `--return-device` (sharing one loopback feeds
the engine's own audio back into scoring), and the DAW-return **monitor mix is disabled** when
redirected — the return carries the DAW's master, and mixing it back into the very stream the
DAW consumes is a feedback loop. Return *capture* (common-mode ±δ scoring, ADR 0026 D1) is
unaffected.

**2. Per-instrument stems via `out_channel`.** An audio instrument may declare
`"out_channel": N` — **odd, 3..=63, the 1-based first channel of a stereo pair** (`3` ⇒ device
channels 3/4, what a DAW shows as input "3/4"). A redirect target opens at its **full channel
count**; the session then streams composed device frames:

- **pair 1/2 — the rest of the mix**: every un-routed instrument **plus all bus/reverb wet**
  (sends are a shared field: a routed instrument's send still feeds its bus, and the return
  stays on 1/2 — exactly how DAW stems + send returns work);
- **pair (N, N+1) — that instrument's dry**, post-level/pan, with the master's per-sample
  stereo-width + master-gain chain applied (the mid/side matrix is linear, so
  `rest + Σ stems` reconstructs the master exactly, up to float association);
- instruments sharing an `out_channel` are summed; a stem whose pair doesn't fit the opened
  device **folds back into 1/2** (on a plain stereo device the doc plays exactly as before —
  the music is always whole). `type:"midi"` instruments can't take `out_channel` (validated).

**3. The scored mix is untouched (the architectural keystone).** The engine still renders the
full stereo master into `RenderResult.audio` with byte-identical math — scoring, ΔCLAP,
features, captures, the corpus, and CE-R7.3 determinism all read that master exactly as before
(pinned by `stem_capture_splits_without_touching_the_master_mix`). Stems are an *additional*,
observation-only decomposition behind an `Engine::set_stem_capture` tap (off by default, like
the viz tap; zero cost when off): parallel accumulation into `RenderResult.stems`
(`StemMix { rest, stems: BTreeMap<out_channel, buffer> }`). The `−δ` counterfactual clone
(`eng_pre`) has the tap switched off — scoring never pays for stems.

**4. Seam placement.** The doc declares the *musical* intent (`out_channel` — which instrument
is its own stem); the **shell** owns the device (which loopback, how many channels — CLI
flag/Setup selector, never doc content; CE-R1.2). The session bridges: `SessionConfig::
out_channels` (2 = classic stereo, the default everywhere including FFI/Swift) sizes the ring
in device frames and composes `rest`/stems into interleaved N-channel frames at both push sites
(normal loops and the LiveEdit crossfade, which blends per composed frame so stems crossfade on
their own pairs). Doc swaps that add/remove/re-pair stems take effect at the loop boundary; the
device width is fixed at session start (overflow folds, with a startup note in sme-play).

## Consequences

- The Ableton workflow becomes: `--output-device "BlackHole 16ch"` (+ `--midi`
  `--return-device "BlackHole 2ch"` as before) → one Ableton audio track per stem pair
  (Monitor In), one for 1/2; Ableton's master → speakers, and optionally → the 2ch loopback for
  scoring return. No double-monitoring, per-instrument faders/effects in the DAW.
- Recording the engine (the old mirror use-case) still works — record the redirect device's
  channels in the DAW; you now get *stems*, not just a stereo print.
- Playback lead is width-independent (ring sized in frames), but a 16-ch ring holds 8× the
  floats of stereo for the same lead (~67 MB at the default ~21 s @ 48 k) — fine for the
  desktop POC; shrink `ring_capacity` if it ever matters.
- The stereo width control applies per stem (linearity keeps the sum exact), so a stem heard
  solo carries the master's width treatment of *its own* signal — slightly different side
  content than it would contribute inside the summed mix field, which is the correct trade for
  reconstructability.
- Two clocks problem is gone with the mirror (one device, one stream); the return input remains
  a second clock, absorbed as before by `latency_ms` + the capture ring.
- Headless renders (`sme-render`/`sme-run`) and the Swift shell stay stereo-master only; a
  multichannel WAV stem export is a natural future consumer of the same `StemMix`.
  **Delivered by ADR 0030** (`sme-song --stems`) — as a finer, separate per-source tap (every
  instrument + every bus) rather than `StemMix`, which stays the live device-routing split.
