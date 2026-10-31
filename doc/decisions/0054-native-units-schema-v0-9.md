# ADR 0054 — Native units in the loop doc (schema v0.9)

Status: accepted — 2026-10-26 (D1–D5 decided with the user on 2026-10-24 and confirmed in question cards
while planning; accepted when the plan's PR merged).
Relates to:
- **ADR 0005: the param-range seam.** Its rule ("normalized `0..1` inside, native units at the doc and CLI
  boundaries") now covers every param, and the seam gains curves.
- **ADR 0047: binaural.** Its uniform-fraction depth rule is **reversed** (D3).
- **ADR 0010/0011, 0025: safe bounds and drift caps.** Bands are written in the param's unit; the caps stay
  fractions, with a drift scale for gains.
- **ADR 0021, 0031: no master limiter.** Why gains above 0 dB get a hard ceiling.
- **ADR 0023: JSON5.** `-Infinity` is valid JSON5; the parse path must keep it.
- **ADR 0050: the capability catalog.** Its unit maps come from the curves, and its authored unit flips.
- **CE-R7.3: determinism.** Still holds; its wording is amended (P10) and the goldens re-bless once.
- **Study:** [`doc/native_units_impact_study.md`](../native_units_impact_study.md). **Plan:**
  [`doc/plans/53-native-units.md`](../plans/53-native-units.md).

## Context

The loop doc stores time, frequency and gain as `0..1`: `cutoff: 0.3` is 318 Hz, `attack: 0.35` is 1.05 s,
`level: 0.5` is −6 dB. Reading or writing a doc means a lookup, for Claude and for the user. Design-rationale §2.6
justified normalized authoring by the learned sensitivity map, which ADR 0031 removed. What's left, one drift step
that works for every param, is internal, and the seam can keep it.

The study (commit a549827) found the change feasible: the seam exists and five params already use it. The work is
curves in the seam, ~30 raw reads that skip it, dB summing for gain modulation, `-Infinity` through the parse and
write paths, and migrating every doc on disk.

## Decision

### D1–D5 (the epic's decisions, confirmed)

| # | Decision |
|---|---|
| D1 | **Every gain is in dB:** `level_db`, `osc_a_level_db`, `osc_b_level_db`, `sends_db`, `return_gain_db`, `master_gain_db`. **`drive` becomes `drive_db`**, a 0 to +20 dB pre-gain into the soft clipper (confirmed 2026-10-26). `sustain` stays `0..1`. |
| D2 | **Every native key carries a unit suffix** (table below). A stale v0.8 key fails strict check instead of mis-rendering. |
| D3 | **Matrix depth is in the dest's own unit** (P4). This reverses ADR 0047's uniform-fraction rule; gain routes sound different, by design. |
| D4 | **Silence is `-Infinity`.** Gains may go above 0 dB, up to the P1 ceiling. |
| D5 | **A clean break** (confirmed 2026-10-26): the engine and every tool read only v0.9; only `sme-migrate` knows v0.8. Provenance (P10) lands first, and each piece's `.live/history` migrates with it. |

### The rename table (P6)

| v0.8 | v0.9 | Unit | Curve (normalized ↔ native) | Range |
|---|---|---|---|---|
| `cutoff` | `cutoff_hz` | Hz | log | 80–8000 (capped at Nyquist, as today) |
| `band_lo`, `band_hi` | `band_lo_hz`, `band_hi_hz` | Hz | log | 80–4000 (20–4000 since ADR 0046's 2026-10-30 amendment) |
| `voices_detune` | `voices_detune_cents` | cents | linear | 0–100 |
| `osc_b_detune` | `osc_b_detune_cents` | cents | linear | 0–50 |
| `attack` | `attack_s` | s | linear | 0–3 |
| `decay` | `decay_s` | s | linear | 0–4 (P7) |
| `release` | `release_s` | s | linear | 0–4 |
| `t60_scale` (`t60_mode: "fixed"`) | `t60_s` | s | log | 0.05–8 |
| `t60_scale` (`t60_mode: "note"`) | `ring_ratio` | × | log | 0.5–2 |
| `level`, `osc_a_level`, `osc_b_level` | `level_db`, `osc_a_level_db`, `osc_b_level_db` | dB | dB | `-Infinity`, or −60 to +6 |
| `sends.<bus>` | `sends_db.<bus>` | dB | dB | `-Infinity`, or −60 to +6 |
| `buses.<id>.return_gain` | `return_gain_db` | dB | dB | `-Infinity`, or −60 to +6 |
| `global.master_gain` | `master_gain_db` | dB | dB | `-Infinity`, or −60 to +6 |
| `drive` | `drive_db` | dB | drive (today's `1 + 9x` pre-gain) | 0–20 |
| `shift` | `shift_st` | st | linear | −24 to +24 |
| bus `predelay` | `predelay_ms` | ms | linear | 0–100 |
| `global.reverb_mix` | **removed** | | | |

Unchanged: `osc_b_ratio`, `root_hz`, `spread_hz` (already native and suffixed), and every unitless fraction:
`sustain`, `resonance`, `shift_mix`, `stereo_width`, `room`, `damping`, `shimmer`, `width`, `fm_index`,
`fm_feedback`, `brightness`, `strike_pos`, `stretch`, `swing`, `prob`, `tension_target`, step `value`. The
categorical and integer levers don't change. Matrix `dest` paths, `safe_bounds` keys, `constraints` paths and
objectives files follow the new names. Free-form `midi` params stay `0..1` and no longer collide with registered
leaves. Registry lookups are scoped by synth type anyway.

For frequency and time, each curve **is today's map**: the normalized space doesn't move, so drift steps and the DSP
stay where they are. Gains are the exception: their normalized space becomes dB over the span (P2).

### P1: headroom (confirmed 2026-10-26)

- **Hard ceiling +6 dB on every gain class**, `master_gain_db` included. Strict check refuses more.
- **Tier-2 safe ceiling 0 dB on every gain**, so drift never pushes a gain past unity. The safe floors keep their
  meaning: `level_db` −26 dB (was `0.05`), `master_gain_db` −8 dB (was `0.4`). `cutoff_hz`'s floor becomes 101 Hz
  (was `0.05`). The schema's `x-safe-bounds-defaults` carries the native values.
- **Why so little room:** rendered over 4 loops at 48 kHz (2026-10-26, measured after a 2 s skip), the auditioned
  fixtures and house references peak between −0.1 and −10.6 dBFS, with a median crest factor of 10.8 dB (modal
  marimba: 21.9 dB). The master has no room, so a boost above 0 dB is a per-voice trim, and +6 dB is a console
  fader's top. `playback_fixtures_do_not_hard_clip` stays the guard.
- **Found while measuring:** `loop_shimmer_showcase` peaks at −2.9 dBFS over one loop, which is what the test
  renders, but at **+7.9 dBFS** by loop 4. Two non-auditioned fixtures (`loop_contract_ambient`,
  `loop_contract_v0`) peak at +10.8 and +12.9 dBFS. That's a gap in the existing guard, outside this ADR.

### P2: gain drift (confirmed 2026-10-26)

Gains are normalized as **dB over −60 to +6 dB** (66 dB). Each curve gets a **drift scale**: 1.0 for every curve
except dB, which gets **0.3**. The controller multiplies `max_change` and `max_drift` by it, so for a gain 0.05 is
about 1 dB a loop and 0.15 is about ±3 dB a session, close to today's feel at `level 0.5`. `sme-defaults.json5` and
every doc keep their cap numbers. A `-Infinity` gain is off: the controller never moves it.

### P3: non-finite values

- **A doc value type that keeps non-finite floats** replaces `serde_json::Value` in `params` and in the check and
  schema walker. It deserializes through serde from the `json5` parser, which already yields `-inf` to a visitor.
- **Strict check** allows `-Infinity` only on gain values, including the `min` of a gain's `safe_bounds` band and a
  gain's constraint floor. `+Infinity` and `NaN` stay refused everywhere.
- **Writers:** every writer of a doc or a doc snapshot (live history, LoopRecords, `--loop-log`, render sidecars,
  `--emit-template`) writes `-Infinity`. Python's `json` reads it. `status.json` and the `sme-live` protocol stay
  strict JSON, because TypeScript's `JSON.parse` doesn't accept `-Infinity`. They carry no doc param values, and
  any dB reading in them keeps today's finite floor.

### P4: native depth

| Dest's unit | Depth key | Example |
|---|---|---|
| log Hz (`cutoff_hz`, `band_*_hz`), and the log times `t60_s` and `ring_ratio` | `depth_oct` | house style `0.06` into cutoff becomes `0.40` oct; +1 oct doubles a ring |
| st (`shift_st`) | `depth_st` | |
| cents (detunes) | `depth_cents` | |
| s (`attack_s`, `decay_s`, `release_s`) | `depth_s` | |
| ms (`predelay_ms`) | `depth_ms` | |
| dB (gains, `drive_db`) | `depth_db` | a "−6 dB in block 3" trim |
| linear Hz (`root_hz`, `spread_hz`) | `depth_hz` | |
| × (`osc_b_ratio`) | `depth_x` | |
| `0..1` dests, `global.stereo_width`, `midi` params | `depth` | |

- **Octaves** for log-frequency dests: a filter sweep reads in octaves, and semitones stay for pitch (`shift_st`).
- A route has **exactly one** depth key, and it must match its dest's unit. A mismatch is an error that names the
  right key. Song `matrices` overrides use the same keys.
- **Inside, depth stays a normalized distance**: `depth_oct / span_oct`, `depth_db / 66`, and so on. For every
  curve but dB, a native depth is an exact rescale of today's depth. Only gain routes change sound (D3). The
  summing stays `add`.
- **Depth drift:** a depth is controllable when `|depth|` is within its curve's span (6.64 oct for cutoff, 66 dB
  for a gain). Its step is `max_change × span × drift scale`, as for the param itself. This replaces the `0..=1`
  gate (`controller/candidates.rs:92`). `safe_bounds` on a depth are keyed by the depth key
  (`matrix.<id>.depth_oct`) and written in its unit.
- `depth_slew_ms` and song-seam depth glides work on the normalized distance, unchanged.

### P5: silence and modulation

A route into a `-Infinity` base is inaudible (−∞ + any dB is −∞). The lint "muted (level 0) but a route adds to
it" becomes a warning: "the base is `-Infinity`, so this route is inaudible; give it a finite floor (say −60 dB)
and a `depth_db`". The lints for a missing `return_gain` and for binaural sends get the −∞ meaning. The
`ambient-loop-doc` skill shows a swell as a finite floor plus a dB depth.

### P7: params with two meanings

- **`decay_s` is real seconds for both envelopes.** It spans 0–4 s, so the ADSR's ceiling rises from 3 s to 4 s,
  matching the `event` bell's. The migrator converts each by its own v0.8 map (ADSR `3·x`, bell `4·x`). The v0.8
  hover that showed a bell's decay on the 3 s scale is fixed in the bit-identical phase.
- **`t60_scale` splits** into `t60_s` and `ring_ratio`. Strict check requires the one that matches `t60_mode` and
  refuses the other.
- **`resonance` stays `0..1`.** Its hover shows both readings (SVF Q, or the pitched-noise peak in dB).

### P8: architecture

**The doc is normalized in memory; native values exist only on parse and on emit** (the study's option A).
Deserializing converts native to normalized once, through each param's curve. Everything that writes a doc
converts back. The engine, the controller and modulation never see a native value, so the ~30 raw reads become
correct by construction and drift no longer round-trips through f32 log/exp. The LSP keeps showing the text as
written, through the CST.

### P9: the migrator

`sme-migrate` lives in `sme_authoring` and rewrites text through the CST, keeping comments:
- **Frozen v0.8 maps:** its own copies, not the engine's, so it keeps working after the engine moves on.
- **Precision per unit:** Hz to 2 decimals below 1 kHz and 1 decimal above, s to 3, ms to 1, cents to 1, dB to 2,
  oct to 3.
- **What it rewrites:** keys, `dest` paths, band keys, `safe_bounds`, constraints, song overrides and depth
  conversion. It is mode-aware for `t60_scale` and envelope-aware for `decay`. `level 0` becomes `-Infinity`, and
  a gain below −60 dB is raised to −60 dB with a note.
- **Comments that quote old values** get a `// TODO(v0.9)` marker.
- **Interface:** `sme-migrate <doc|slug>... [--dry-run] [--json] [--piece]`, with an atomic write. Piece mode also
  migrates `.live/history`, keeps v0.8 backups, and never touches renders, `.als` files or `.asd` siblings. An
  `sme-lsp` quick fix runs it.

### P10: provenance before the break

- **Crate versions track the schema:** `0.8.0` now, `0.9.0` at the break.
- **The build id:** `sme_core` records a build id (`0.8.0+g<sha>`, plus `.dirty` for a dirty tree, or `unknown`
  without git) at build time. Every render sidecar, `midi.json`, the catalog and the preview sidecar record it as
  `engine_build`.
- **The tag:** the last v0.8 commit on `main` is tagged `schema-0.8-final` just before the break merges, with the
  owner's OK to push the tag.
- **CE-R7.3** is amended: a render is re-creatable from its doc snapshot **with the engine build that rendered it**
  (the sidecar's `engine_build`).

### P11: proving the migration

- **A permanent parity test:** for every curve, the migrator's frozen v0.8 map → native → the engine's v0.9 curve
  inverse returns the v0.8 normalized value within 1e-5 (≈ 0.1 cent, < 1 ms, 0.005 dB).
- **A one-time audio null test:** every migrated fixture and house reference renders against its v0.8 render from
  the `schema-0.8-final` build, both with gain routes disabled, at ≤ −100 dBFS (the `sme-als null` method).
  Results go in the break's PR.
- **Gain routes are auditioned** (`needs-listen`).
- **The goldens re-bless once**, with the reason in the commit: the golden cases and the stems golden table.

### P12: latent bugs fixed in the bit-identical phase

- Objectives and control-JSON `guardrails.ceilings`/`floors` are normalized through the seam, as the CLI flags
  already are (`controller.rs:596-600` vs `contract.rs:268-275`).
- The `event` bell's decay hover (P7).

### Amendment (2026-10-27): migrating depths

Settled with the owner in question cards while building `sme-migrate`:

- **`t60_s` takes `depth_oct`**, not `depth_s` (the table above is amended). Its curve is log, so a depth in
  seconds wouldn't be a rescale; in octaves it is, like every other log curve.
- **A gain depth keeps the source's +1 swing.** v0.8 summed a gain route in amplitude and v0.9 sums it in dB, so
  no conversion is exact. The migrator writes `depth_db = 20·log10((a + d) / a)` at the route's base `a` (the
  authored gain, or the engine's default for it), so the source at +1 lands on the amplitude it did: exact at both
  ends for a unipolar source, while a bipolar source's −1 side becomes the mirror image in dB. The house style's
  `−0.28` at level 0.55 becomes −6.18 dB. Its notes name every gain route it converted.
- **Silence and the floor.** A route that ducked past silence ducks to −60 dB. A gain authored (or defaulting) to
  0 that a positive route lifts starts from the −60 dB floor (a `-Infinity` base is silent under any route): the
  migrator writes `-60` and a depth from there. A route down from silence stays inaudible, at depth 0.
- **`drive` routes** take the same +1 rule in dB of pre-gain. Whatever curve v0.9 gives `drive_db`, its base
  converts exactly.
- **Without buses**, a v0.8 doc got an `rv` reverb from `global.reverb_mix`; the migrator writes that bus out
  (return at `reverb_mix`, every non-midi instrument without sends at 0 dB) and drops `reverb_mix`. A route into
  `global.reverb_mix` never moved anything and is removed, with its bands and song overrides.
- **History.** Piece mode migrates `.live/history` snapshots in place under their names; `index.jsonl` keeps the
  v0.8 hashes it recorded, which name the backups in `.v0.8-backup/`, the text that actually played.
- **Strict JSON stays JSON.** A doc whose keys are quoted gets quoted keys and no trailing commas in what's
  inserted (the lab reads some fixtures with Python's `json`, which accepts `-Infinity`).

### Amendment (2026-10-27): building the break

Settled with the owner in question cards while building schema v0.9, or forced by what the code showed:

- **P8 is reversed: the doc stays native in memory.** Since the bit-identical phase every engine read
  goes through the seam's curves (`Instrument::norm`/`native`, `BusEffect::norm`, pinned by a test that bans raw
  reads), so a native in-memory doc is correct by construction too — and needs no conversion layer for parse,
  internal edits or writers. The cost is that a drift step round-trips through its curve each loop (deterministic,
  ~1e-7 relative). A route's depth is held in its dest's unit and turned into a normalized distance where it's used
  (`MatrixEntry::depth_norm`).
- **`drive_db` is a dB curve over 0..+20** (normalized dB/20, pre-gain 10^(dB/20)), like the gains. Its base values
  convert exactly (v0.8's ×(1+9x) is the same pre-gain); a drive route sums in dB, so the migrator's +1 rule for
  drive depths holds, and its drift scale is the dB curve's 0.3.
- **A route into an omitted param starts from the param's default**, as the param plays unrouted. v0.8 started it
  from normalized 0 (a route into an unwritten `cutoff` swept from 80 Hz). No doc in the repo, the pieces or their
  history does that, so nothing changes sound. One defaults table (`params::DEFAULTS`) now feeds the render, the
  synth configs, a route's base and the capability catalog, whose `default_when_routed` goes (catalog v2).
- **The null test runs on docs migrated at full precision** (`sme-migrate --full-precision`, a hidden flag): the
  shipped docs are rounded per P9, which alone leaves a residual near −70 dBFS (a level written −6.02 for −6.0206),
  so the −100 dBFS bar proves the curves and the parse path, and the PR reports the rounded docs' residual beside it.
  Result: every fixture, house reference, sandbox and bench doc nulls at ≤ −100 dBFS except the template's two
  copies, at −89.8 dBFS, which is −99.8 dB under their +11.8 dBFS peak: f32 rounding of one cutoff route's depth
  (`depth_oct` ÷ the curve's span) through a modulated filter, −113 dBFS without that route.
- **Negative depths drift too (P4).** Depth drift's gate is the route's normalized distance, `|depth| ≤ span` in
  the dest's unit, so a negative depth drifts by ±`max_drift` like a positive one. The house docs get no freeze
  bands for it; the listening pass says whether one is wanted.
- **A bus return under a route reaches the +6 dB ceiling** like every gain: the return's amplitude isn't capped at
  unity any more (v0.8's 0..1 range was the cap).
- **Hovers and inlay hints stop echoing units** (a v0.9 value is written in its unit); U6 re-aims them at
  what's useful.
- **A depth band on a route that only a song layer defines** is renamed by the migrator too (its dest is the
  layer's); a part's `patch` paths are migrated (`instruments.$target.sends.$id` → `sends_db`).
- **The check:** another `schema_version` is refused whole, at that key, naming `sme-migrate`; a v0.8 key names
  its v0.9 one ("schema v0.9 calls it `cutoff_hz`"); a modal ring key must match `t60_mode` (P7); a band's min may
  be `-Infinity` only on a gain; a route into a `-Infinity` base warns that it's inaudible (P5).

### P13: coordination

- **The capability catalog (merged):** its unit maps are derived from the curves in the bit-identical phase. Its authored
  unit flips at the break, and it becomes catalog v2. `sme-listen`'s `catalog.py` and the `listen` skill's mapping
  guide follow.
- **Portable math (`libm`):** it lands after the break, so its `libm` swap covers the new curve code once. Each
  re-blesses the goldens for its own stated reason.

## Consequences

- A doc reads in Hz, seconds and dB. Claude's gain staging is "−6 dB", and a song trim is a real offset.
- Old docs fail loudly with a message that names `sme-migrate`; nothing mis-renders silently.
- Gain modulation sounds different (perceptually even swells). Pieces that modulate gain change on migration.
- Renders made before the break can only be re-created with the build that made them. The tag and `engine_build`
  make that possible.
- The engine core stays portable. The build id is a build-time value from `build.rs`, with no runtime file or
  clock access.
- The amended notes in ADR 0005 and 0047 and the SRD scope banner land with the docs phase.
