# ADR 0024 — Live-edit authoring mode, instrument visualization, and screen-side constraint editing

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** the doc is written in native units, so the TUI shows each value as the doc writes it (`800Hz`, `-6.0dB`, `-∞dB`) and a drift step in its unit (`+12.5Hz`, `-0.40dB`), rather than converting a stored `0..1`. The constraint editor stores a band as typed: the addendum's `800 2000` is a `cutoff_hz` band in Hz, with no fold into `0..1`. §3's units registry now lives in the core (`sme_core::params::units`).

- **Status:** accepted
- **Date:** 2026-06-29
- **Context:** the POC roadmap (the runtime-control / Orrery epoch, frozen at Phase 6d) is **deliberately
  paused** to turn the Ratatui TUI (`crates/sme_tui`) into a human-authoring tool. The motivation is the
  project's own legibility evidence: the engine is only weakly CLAP-legible (mostly via loudness; fine
  timbral params sit at the noise floor — PLAN.md sweep findings), so "make decent music *by default*"
  is a human-in-the-loop problem before it is a CLAP-pursuit problem. The near-term driver is a YouTube
  live-stream of focus/sleep music (a marketing channel for the eventual mobile app) that the author
  configures by hand. This ADR records the three features built for that and the one architectural rule
  it amends.
- **Amends:** [ADR 0007](0007-sme-tui-live-explorer.md)'s "the TUI is a separate crate with **zero
  `sme_core` changes**." The core now grows **I/O-free observation/control seams** (the same pattern
  `per_inst_rms` established in [ADR 0022](0022-pursue-gate-empirical-direction-and-audibility-telemetry.md)).
  The load-bearing rule is unchanged: **the core stays I/O-free and mobile-portable** (ADR 0003) — all
  file/device I/O, the FFT-for-display, and the crossfade policy live in the TUI. These seams also serve
  the eventual mobile app (it will want visualizations + human control).
- **Code anchors:** `crates/sme_core/src/{contract.rs, controller.rs, session.rs, engine.rs}`,
  `crates/sme_tui/src/{app.rs, ui.rs, engine_link.rs, units.rs, viz.rs}`.

## 1. Live-edit authoring mode (`OptimizeMode::LiveEdit`)

A fourth optimize-axis mode. `LiveEdit` **freezes** the controller's autonomous layers (no
categorical A/B, no ±δ pick — like `Hold`) **and** turns scoring off: the session skips `score_and_emit`
/ the corpus entirely (`Controller::scores()`), so no CLAP calls run. The learner is preserved across the
switch, so the user can flip back to drift/pursue in-session (the existing `o` cycle / `l` toggle).
*(Amended 2026-06-30 — see the addendum: the **Tier-3 constraint attractor now runs in LiveEdit too**,
since a constraint is an explicit user guardrail; only pure `Hold` leaves it frozen.)*

The user hand-edits the loop-doc file; the TUI **watches it read-only** (`App::watch_loop_doc`, mirroring
the objectives watcher's mtime + content echo-guard) and applies the parsed doc at the next loop boundary
via a new `Session::apply_doc` → `pending_doc` channel (the exact shape of `apply_control`/`pending_control`).
Because the TUI never *writes* the loop doc, the author's comments/annotations there are always safe — this
is why we **sidestepped** comment-preserving write-back (jsonc-parser) entirely: rich annotations live in
the loop doc, and the objectives doc stays TUI-owned via the existing template renderer.

### The swap is a hybrid (the key decision)

Verified first: `Engine::render` already reconciles a carried `PhaseSnapshot` by id — new ids
`unwrap_or_default()` (fresh), stale ids are silently dropped (the next snapshot is rebuilt only from
`doc.*`; buses `retain`, filters pruned by `present_keys`), and a changed synth `type` reshapes
`SynthVoiceState` deterministically. **No panic, no index error.** So the swap needs no reconciliation of
its own:

- **Same-id edits** (leaf values, step/voicing content, euclid pulses, LFO rates) → `Controller::swap_doc`
  replaces `base` in place. The snapshot keys still match, DSP tails carry, and the edit is **seamless**.
- **Structural edits** (add/remove/retype an instrument or modulator, or a loop-length change —
  `doc_swap_is_structural`) → a **standby-engine crossfade** (`apply_doc_swap` in `session.rs`): build a
  fresh `Engine`/`Controller` from the new doc, render **both** engines from the *same* pre-swap snapshot
  (so unchanged voices are bit-identical and don't blur), equal-power crossfade the first `XFADE_SECS`
  (0.4 s, wall-clock so it's loop-length-agnostic) old→new, push the blended loop, adopt the new engine.
  Engine/Controller construction is I/O-free and cheap, and the 2× render for one loop is absorbed by the
  ~20 s render-ahead ring, so no dedicated standby thread is needed (a later option if a heavy doc ever
  makes construction costly).

The live-edit crossfade is an interactive transient, not a replayable contract (live-edit is non-learning,
so there is no corpus determinism to honor). Each engine stays bit-reproducible per doc.

## 2. Instrument live-stats screen + the viz seam

A new `Screen::Instrument` (enter with `i`, or **Enter on an `instruments.*` row of the Params tab** —
which selects that instrument and pre-selects the param): a 25 Hz spectrogram of the **selected instrument's dry**
audio **and** the **master mix** (the user's stated goal is to connect what they hear to *when* a voice is
playing), a selectable param list with the effective (post-modulation) value at the current frame, and an
oscilloscope of the selected param's effective value over time.

The seam is a new **I/O-free observation tap** on `Engine`, gated by `set_viz(bool)` and proven
**byte-identical when off** (the CE-R7.3 keystone test `viz_tap_is_byte_identical_when_off_vs_on`). When on,
`render` fills `RenderResult.viz_{master_hops, inst_dry_hops, eff_hops}` — raw mono audio per 25 Hz / 40 ms
hop (1920 samples @48k) + a normalized effective-param snapshot — tapped at the existing per-instrument dry
sum site (next to `per_inst_rms`). The session ships these to the TUI over a dedicated bounded
**`VizFrame`** SPSC channel (`Session::poll_viz`, lossy `try_send` so the worker is never gated by the UI);
`set_viz_instrument` selects which instrument's dry hops flow (bandwidth). The FFT (a rolling
log-frequency spectrogram, `viz.rs`) runs **in the TUI**, reusing the same `realfft` crate the core's
feature extractor uses. The displayed column follows the **play** cursor (newest at the right) rather than
the render cursor.

## 3. Units registry + screen-side constraint editing

A TUI-side **units/display registry** (`units.rs`) maps each JSONPath leaf to its real-world unit
(Hz/seconds/ms/semitones/ratio/normalized) and formats a normalized value as native units. It mirrors the
engine's own maps via **now-public** engine constants (`cutoff_to_hz`/`band_to_hz`/`*_MAX_S`) — one source
of truth, no drift.

Constraint editing reuses the **existing** two-way objectives sync end-to-end (`parse_constraint` →
`commit_edit` → `apply_edit` → template write-back, echo-guarded). New on the instrument screen: a `c` key
seeds the editor for the selected param; a tier detail line shows Tier-1 native range / Tier-2 safe range /
Tier-3 constraint band + the real-unit span; and `commit_edit` now **validates** the Tier-3 band against
the loaded doc's Tier-2 safe range (`validate_band`: each bound ∈ safe range, `min ≤ max`) — an invalid
band keeps the editor open with the error and is never written. The modal editor capture moved up to
`on_key` so it works on the Live *and* Instrument screens.

## Consequences

- The core's public surface gains observation/control seams (`OptimizeMode::LiveEdit`,
  `Controller::swap_doc`/`control_clone`/`scores`, `doc_swap_is_structural`, `Session::apply_doc`/
  `poll_viz`/`set_viz`/`set_viz_instrument`, `RenderResult.viz_*`, pub unit constants). All I/O-free.
- Determinism preserved: existing modes render byte-identically (the freeze/scores gates and the viz tap
  are no-ops when unused); proven by `viz_tap_is_byte_identical_when_off_vs_on` +
  `render_reconciles_structural_doc_change_across_snapshot` and the unchanged determinism/replay suite.
- **Known limitation:** the scoring-thread topology is fixed at `Session::start`. A session that *starts*
  in LiveEdit and later switches to **pursue** degrades pursue to drift until restarted (drift is
  unaffected). Surfaced in the UI; making the topology dynamic is out of scope.
- **Deferred (not built):** helper-thread standby construction; learner-recording from human edits (the
  legibility data says the payoff is low — revisit as a human-preference dataset, not ΔCLAP);
  jsonc-parser in-place objectives editing (sidestepped via §1).

## Tests

`+7` Rust (`sme_core`): viz byte-identity, structural reconciliation, `swap_doc`, LiveEdit no-perturbation,
LiveEdit-no-samples, structural crossfade keeps audio finite, viz stream delivers frames. `+11` TUI
(`sme_tui`): live-edit toggle, loop-doc watcher, units registry (×3), spectrogram ring (×2), constraint
validation + commit rejection, Params-row Enter targets the instrument screen (×2). **253 `sme_core` +
29 `sme_tui` green, no warnings.**

## Addendum (2026-06-30) — instrument-screen fixes + play-cursor alignment

A bug-fix pass on the instrument screen, with one mechanism change worth recording:

- **Play-cursor alignment is now actually wired (§2 stated the intent; the drain ignored it).** `VizFrame`
  gains a public `audio_pos: u64` — the hop's absolute audio-frame offset, stamped by the render worker
  (`render_next`/`apply_doc_swap` track a worker-local frame counter; off the audio/determinism path, so
  `viz_tap_is_byte_identical_when_off_vs_on` still holds). The TUI counts **played** frames in the cpal
  callback (`EngineLink::frames_played`, a wait-free atomic — the only new I/O-side state) and **buffers**
  incoming hops, revealing each only once `audio_pos ≤ frames_played`. Result: the spectrogram + scope
  scroll smoothly at 25 Hz **in sync with what's audible**, replacing the old "drain-everything-each-tick"
  burst. The instrument-screen tick drops to 40 ms; other screens stay at 100 ms. *Known transient:* on
  entering the screen (and on instrument-switch) the spectrogram fills over ~the render-ahead while it
  aligns — the audio playing then predates the tap; surfaced with an "aligning to playback…" hint.
- **Oscilloscope plots any param** (not just modulation destinations): a param absent from the per-hop
  effective snapshot falls back to its authored base → a flat line, via `App::scope_value`.
- **Param list scrolls** with the cursor (`inst_params_state: TableState` + `render_stateful_widget`, like
  the Params tab) instead of letting the highlight run off-screen.
- **Tier detail reads in real units**: `selected_param_detail` routes the native/safe/constraint bounds
  through `param_norm` → `units::fmt_native` (Hz/s/ms/st/×) rather than printing raw canonical 0..1 values.
- **Oscilloscope shows the *newest* samples** (a later follow-up): ratatui's `Sparkline` renders
  `data[0..width]` (the oldest), so feeding the whole `SCOPE_CAP` ring showed ~10 s-stale bars while the
  label showed the live value — the chart appeared to rise while the value fell. `scope_bars` windows to
  the newest `width`, newest at the right edge, matching the label + spectrogram.

## Addendum (2026-06-30b) — author-workflow fixes (objectives opener, LiveEdit base, LiveEdit constraints)

- **Objectives file opens (`0` key).** `open file.json5` fails on macOS (no app associated with `.json5`),
  silently. The opener now uses `open -t` (default text editor), which works for any extension.
- **LiveEdit base reflects in the Params tab.** The tab's `base`/`effective` came from `analytics`, fed by
  `Sample`s — and LiveEdit emits none. `App::param_base_value` falls back to the live-watched loop doc, so a
  hand-edited base shows immediately (`draw_params` resolves it before the field destructure).
- **Constraints enforce in LiveEdit (the §1 amendment).** The Tier-3 attractor's gate changed from
  `!freezes()` to `optimize() != Hold`, so a min/max constraint drives the param into its band even while
  authoring — a constraint is an explicit guardrail, distinct from the frozen autonomous drift/categorical
  layers. Pure `Hold` still freezes everything.
- **Constraint editor takes native units.** The editor now reads the same units the screen shows: new
  inverse maps `hz_to_cutoff`/`hz_to_band` (co-located with the forward maps in `engine.rs`, one source of
  truth) + `units::native_to_canonical` fold a typed Hz/s/ms/st/× value into the param's canonical storage
  space before validate/store; `validate_band` errors render in native units too. So a cutoff band is typed
  `800 2000` (Hz), not `0.3 0.6`. Seam params (predelay ms, shift st, ratio ×) were already native.

**257 `sme_core` + 38 `sme_tui` green, no new warnings.**

## Addendum (2026-10-09) — bus retype / effect-chain edits

§1's "buses `retain`" reconciliation had a latent LiveEdit-only gap. The engine builds a bus once and
keeps it (tails cross the seam), and the `retain` key was only "id still in the doc". So a **same-id**
retype to `type: "external_return"` (ADR 0026 phase D: the core never renders one) left the built
internal `Bus` alive. It kept taking sends (`sends_of` filters on the built set) and kept returning
reverb wet into the master. A same-id **effect-chain** edit (an effect added, removed, renamed or
retyped) was never picked up either, because `Bus::from_doc` only ran on first sight and params can't
turn the built nodes into a different chain. `doc_swap_is_structural` only compared bus id sets, so
both edits took the in-place path on the same engine.

- **Engine (correctness, any caller).** `retain` keeps only buses still present **and** internal. A built
  bus whose doc chain no longer matches (`effects::Bus::built_from`, compared by `(effect id, effect
  type)` in chain order) is rebuilt fresh: the stale chain stops and its tail is dropped. Param and
  `return_gain` edits keep re-applying in place every loop, so the tail still carries for those.
- **Swap (glitch-free, the live path).** A bus `type` or effect-chain change is now **structural**
  (`loop_doc::Bus::same_shape`). An in-place rebuild or drop would hard-cut that bus's wet at the loop
  boundary, so the session crossfades a standby engine instead, as it already does for an instrument
  retype. Bus param edits stay seamless. Songs are unaffected: blocks are matrix-only (ADR 0029), so the
  bus set never changes across a seam.
- **Determinism (CE-R7.3)** holds: rebuild and drop are pure functions of the doc. No renders change for
  docs that don't edit a bus's shape between loops.
- **Still open (session level, pre-existing):** the session reads the `external_return` config
  (`return_gain`, `latency_ms`) once at `Session::start`. A LiveEdit swap that adds, removes or retypes the
  return bus updates the core render but not the shell's capture/monitor mix until restart.

Tests: `retyping_a_bus_to_external_return_drops_its_wet` and `editing_a_bus_effect_chain_rebuilds_the_bus`
(engine). Each compares the carried engine's loop against a twin engine warmed on the same loop with no
buses (so voices and filters match), and it's bit-identical. Each test fails when its half of the fix is
reverted. Also `bus_retype_or_effect_chain_edit_is_a_structural_swap` (controller).
