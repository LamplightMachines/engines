---
name: ambient-loop-doc
description: >-
  Author and verify an original generative loop document (loop schema v0.9, annotated JSON5) for
  Engines' Rust engine in the house style: a FUSED HARMONIC-SERIES DRONE on one centre
  frequency as the focal point (a 1:1-PM `dual` "crank" + just-intonation partials + a sub), a
  quiet tempered "halo" pad that the Orrery may re-voice but is TAMED around the centre by a
  `context` harmonic floor, and a small mechanical PULSE from ADR 0028 sequence pools (sub-step
  `t`, gated `dur`, `prob` ghosts) with swing/humanize feel, all through a shimmer-reverb send
  bus — then renders it and PROVES it with `engines-song`'s render checks (harmonicity, pulse
  spectrum, A/B route audibility, headless Orrery voyage). For a LONG TRACK — a sleep / nap / focus
  track, an all-night or multi-hour piece, "a song" — it then adds an ADR 0029 `song` on top: an arc
  of matrix-only blocks (settle → idle → deep → surface), loop math, glided seams, a ring-out tail,
  gain-staged PER BLOCK and proven with `engines-song` (dry-run plan, every block's start, a scaled-down
  drift + Orrery render checked for peak, clips and seam smoothness). Use when asked to create,
  design, compose, or verify a loop doc / drone / ambient patch / engine hum / generative pattern /
  song / long-form or sleep track for this engine (crates/engines_core, loop_contract v0.9). It is also
  the engine reference for the piece workflow: starting a NEW piece in pieces/<slug>/ goes through
  /engines-new (interview → doc → live in VS Code), live changes through /engines-edit, the song through
  /engines-song; standard layers come from the parts library (`engines-part add`).
---

# Authoring a loop document — house style: **centre · halo · pulse**

You are composing a **loop document** the Rust engine renders, then **proving with audio analysis**
that it does what you intended. The engine loops a *control* timeline while synth voices run
continuously; only the control schedule repeats.

**The target is a specific sound.** Read the reference example before anything else — it *is* the
spec, annotated line by line:
`lab/tests/fixtures/loop_engine_drone_a_mixolydian.json5` (+ its companion
`loop_engine_drone_a_mixolydian.objectives.json5` — the older way; a new doc carries its `optimize` /
`movement` / `constraints` itself, ADR 0033). A doc you author should be a sibling of it, not a
generic ambient pad. Then read `skills/shared/engine-facts.md` (the distilled engine contract — param
ranges, the PM/modal math, the Orrery's rules, the gotchas) and `skills/shared/music-theory.md` (the
harmonic-series method: which overtones to voice, the cents table, the "tempered image" home chord,
pulse design). Formal schema: the annotated `doc/loop_contract_v0.9.json5` and
`contracts/loop_contract_v0.9.schema.json`; parse target `crates/engines_core/src/loop_doc.rs`. Never
guess engine behaviour — read the source. **Schema v0.9 (ADR 0054) writes every param in its native
unit under a suffixed key** (`cutoff_hz`, `attack_s`, `level_db` with `-Infinity` = silent,
`voices_detune_cents`, `shift_st`, `predelay_ms`; no suffix = a `0..1` fraction) and a route's depth in
its dest's unit (`depth_oct`, `depth_db`, …). A v0.8 doc is refused — `engines-migrate <doc>` converts it.

**Asked for a long track** (sleep / nap / focus / all-night / "a 5-hour piece" / "a song")? Build and
verify the loop doc exactly as below first, then add a `song` — see **Long tracks: a `song` over the
doc** near the end. Its worked example is `lab/tests/fixtures/song_idle_night.json5` (5:30:11 over the
reference doc); the mechanics are in `skills/shared/engine-facts.md` → **Songs**.

## The architecture (what "like the example" means)

```
   halo      quiet tempered pad ABOVE the series — the ONLY thing the Orrery re-voices (chord role)
   ───────────────────────────────────────────────────────────────────────────────────────────────
   partials  4–6 held voices on JUST harmonics of f0 (cents_offset-tuned)   ┐
   crank     f0: `dual` osc, integer osc_b_ratio phase modulation          │ the CENTRE — context
   sub       f0/2 low-filtered saw (the family's true fundamental), dry     ┘ + orrery_disable
   ───────────────────────────────────────────────────────────────────────────────────────────────
   piston    struck `modal` on f0 / 2f0, playing fit:"literal" SEQUENCE POOLS (oneshot role)
   chug      bar-locked level dips on the crank (control step-seq) + an f0/4 firing tremolo
```

The centre is **one fundamental** heard as one fused, overtone-rich note. Everything else orbits it
without moving it: the halo supplies colour the Orrery can steer (but only within the harmonic floor
the centre lays down), the pulse supplies a mechanical "engine" rhythm whose *pattern* can vary but
whose *pitch* never leaves f0.

## Target spec (what to deliver)

An **annotated JSON5** loop doc (the engine parses JSON5 natively — ADR 0023) exhibiting **all** of:

1. **A centre** — `f0` chosen deliberately (A2 110 Hz, G2 98 Hz, D2 73 Hz are good; state it in
   `meta.center_hz`). Three fused layers, each a **`context` + `orrery_disable`** note modulator:
   - `sub`: a **`saw_soft` at f0/2 under a low filter** (`cutoff_hz` ≈ 320, `level_db` ≈ −3) — a
     growl, not a hum: its odd harmonics turn the whole spectrum into the f0/2 series; `sustain 1`,
     `sends_db` ≤ −20 (keep the low end tight). A plain sine is the gentler alternative;
   - `crank`: `type:"dual"`, `osc_b_ratio` **integer** (1.0 = every harmonic; 2.0 = odd only),
     `fm_index` 0.2–0.3 with a *slow free LFO on `fm_index`* (the "load" — a big, legible mover),
     `fm_feedback` 0.1–0.25, `drive_db` ≈ 10, **`cutoff_hz` ≥ ~1270** (its own filter otherwise chokes
     the PM harmonics — measured);
   - `partials`: a `triangle` (or `sine`) instrument with `polyphony` = number of harmonics, **no**
     `voices_detune_cents`, one held voicing of 4–6 harmonics (typically h3 h5 h6 h7 h9) with the just
     `cents_offset`s from the table in `skills/shared/music-theory.md`; slow attack; a slow free LFO on
     its `cutoff_hz` (the "bloom").
2. **A halo** — one quiet `saw_soft` pad above the series, **`orrery_role: "chord"`**, home voicing =
   the **tempered image** of the series (root/fifth/ninth stacks, e.g. `E A E B` for A — no third, no
   seventh, so nothing beats against the just partials at rest), stable ids `v1..v4` (the Orrery
   rewrites them ⇒ glides), `glide_ms` ~900, `attack_s` ~2, `level_db` at least ~4.5 dB under the
   crank's, sent hard to the shimmer bus (`sends_db` ≈ −1), `shift_st: 12` + a slow *unipolar* LFO on
   `shift_mix` (an up-octave sheen). Voice it in offsets −5..14 — the Orrery centres pad voicings
   around offset 7 ± 6.
3. **A pulse** — a struck **`modal`** instrument (`material:"resonators"`, `num_modes` 4–6,
   `t60_mode:"note"`) on f0 and 2f0, driven by a **phrase arm** (`phrase: { pool, select:"seeded" }`,
   explicit `step_duration`, host step = one beat) with **`orrery_role: "oneshot"`** and 2–4 doc-level
   **`fit:"literal"`** pools tagged `calm`/`sparse`/`busy`: an 8th-note idle with sub-step `t: x.5`
   ghosts at `prob` ≈ 0.5, a four-stroke accent/rest cycle, a triplet over-run (`t` = ⅓, ⅔); plus
   optionally one `lib:` entry (`lib:pedal_pulse`) to show the library. `dur` on every pulse note
   (gated re-strikes). Underneath: a **bar-locked control step-seq** at `step_duration:"1/64"` with
   unequal dips (e.g. `0, .55, .2, .7`) routed into the crank's `level_db` at a negative `depth_db`
   (the chug: −6.18 dB ⇒ dips to ≈ −4.3 dB), and a free LFO at **f0/4 Hz** on the crank's `level_db`
   (`depth_db` ≈ 1: a firing tremolo whose sidebands stay in the harmonic family).
4. **Feel (ADR 0028 D7)** — `transport.swing` 0.05–0.12 (a slightly uneven idle), `timing_jitter_ms`
   2–4, `humanize_ms` 3–6 on the pulse modulator. Sub-step `t` onsets never swing; control seqs
   never jitter — only strikes.
5. **Breathing** — ≥ 3 slow free LFOs (0.02–0.05 Hz): partials `cutoff_hz` (`depth_oct` ≈ 0.8) + the
   bus `shimmer`, crank `fm_index`, halo `shift_mix`. All with `depth_slew_ms` 1500–2000.
6. **One shimmer-reverb send bus** (`room` ~0.8, `damping` ~0.3, `shimmer` 0.3–0.4, `predelay_ms` ~18,
   `return_gain_db` ≈ −7); a doc without buses plays dry and an instrument without `sends_db` is dry,
   so give every instrument one.
   **Keep `shimmer` + every LFO depth on it ≤ 0.6:** above ~0.65 the shimmer loop self-oscillates and
   locks into a roar at +6..+9 dBFS that no gain fixes (0.6–0.65 rings very long).
7. **Gain-staged for the controller** — no master limiter (ADR 0021). Author
   `safe_bounds: { "global.master_gain_db": { min, max } }` around the authored value (a zero-width band
   freezes it; an explicit band side is a hard clamp, a default range only bounds drift). Hours of drift creep loudness up (~5–6 dB by the second hour), so also cap
   every level-ish lever at its authored value (voice `level_db`s, `drive_db`, the bus `return_gain_db`)
   and set `max_drift: 0.10` (≈ ±2 dB on a gain). The reference doc's `safe_bounds` block is the template. Trim until the 3-loop
   render peaks **0.50–0.55 FS**, then confirm a **2-h** drift + Orrery `engines-song` reports **0 clipped**.
   Write both numbers into the header comment.
8. **Tamed Orrery** — the three context layers form the harmonic floor; the halo's home is the
   tempered image; the pulse is `oneshot` + `literal` (pattern may change, pitch/octave never); the
   doc carries its own **control settings** (ADR 0033 — start from the repo-root `engines-defaults.json5`):
   `optimize: "drift"` (the gain staging is proven under drift; audition `--optimize hold` for the
   authored timbre) and `movement: { orrery:true, phrase_loops 4–8, tension_target 0.10–0.15,
   tension_max 0.35–0.40, voice_changes_per_loop {0.25, 1.25}, home:"global", arrange:{
   return_hazard_scale 1.5 } }`. `engines-play`/`engines-song` apply them; `--optimize`/`--orrery` layer over.
9. **Verified** (below) — and the header comment states the measured numbers: peak, % of energy on the
   series, the pulse's dominant envelope line, the A/B balance, the 20-loop Orrery voyage.
10. **Annotated** — a header block explaining the centre, the layers, the taming, the feel, and the
    render/listen commands; inline comments on every non-obvious number. Comments are the point of
    JSON5 here.

## Workflow

1. **Study** the reference example + both `reference/*.md`.
2. **Design on paper**: f0 and its `offset` (`midi = 60 + root_semitone + offset`; A2 with root A is
   −24), the harmonics to voice (offset + cents from the table), the mode the series implies
   (h7 present ⇒ **mixolydian**), the halo's tempered-image home, the 2–4 pulse pools, the LFO rates.
3. **Write the JSON5**. A new piece is `pieces/<slug>/<slug>.json5` — the `/engines-new` skill runs the
   kickoff interview and lays the project out (brief, defaults, the parts, the hand-off to VS Code);
   this skill is the engine reference it builds with. Other scratch goes to `sandbox/<name>.json5`
   (gitignored). A doc meant to ship as an example goes to `lab/tests/fixtures/loop_<name>.json5`
   **and** is registered in `playback_fixtures_do_not_hard_clip` (`crates/engines_core/src/engine/tests.rs`)
   and linked from the README "Authoring a loop document" section. Start the layers that
   have a part from the library (below) and re-voice them to this recipe.
4. **Check it strictly**: `make check DOC=<doc.json5> -- --parts` (or `./target/release/engines-check
   <doc.json5> --parts`) — exactly what the engine will refuse (unknown keys/params with did-you-mean,
   enums, ranges, dangling routes, bad root/mode) plus warnings (params a synth type ignores, silent
   voices/buses, unused bands) and the polymeter ("realigns every ≈ N loops"). **0 errors** to proceed;
   read every warning.
5. **Render and measure**: `cargo build --release --bin engines-song` then
   `./target/release/engines-song verify <doc.json5> --ab` — 2 loops rendered in memory (no file),
   measured, and every route A/B'd. Its report must show **no `WARN: engine lint`** (an unknown pool
   id / `sync` / instrument `type` is a hard error; a multi-loop `sync:"loop"` grid is a lint).
6. **Read the numbers** against the spec (Verification, below).
7. **Balance-iterate**: the A/B must show the **centre leading** (sub / crank / partials the three
   largest diff-RMS; the saw sub is typically the largest), the halo clearly secondary (≈ 25–40 %,
   ≤ 0.6× the crank), pulse ≈ 7–15 %, chug ≈ 12–20 %; peak 0.50–0.55 FS; centre ≥ 35 % and centre +
   family ≥ 90 % of 40–1500 Hz energy, ≤ 2 % elsewhere. Do not ship a route the A/B shows as silent.
8. **Prove the taming**: `engines-song voyage <doc> --movement default` — the Orrery run headless
   for 20 loops with the doc's `movement` replaced by the default (a chord change every loop), because
   even then the voyage should stay at tension mean ≲ 0.1 / max ≲ 0.45 (**TAMED**) with borrowed
   chords resolving home the next phrase. If it roams, the floor is too thin (add a context pitch) or
   the home voicing is not the series' image.
9. **Prove the headroom under the controller**: `verify`'s raw render never sees the controller's
   safe-clamp or drift. Run `engines-song --doc <doc> --out /tmp/s.wav --duration 2h --orrery` (drift +
   Orrery through the live Session path, ≈ 20 min at ×6 realtime). Its `done:` line must say
   **0 clipped**, with the peak ≲ −0.5 dBFS. Expect ~5 dB over the 3-loop render's peak by the second
   hour. **Don't stop at 30 min**: Idle was clean for 30 min and clipped in hour two. If it clips, check
   the level caps and `max_drift`, lower `master_gain_db` (and its band), and re-run.
10. **Write the measured numbers into the doc header** (and re-run `engines-check`).

## Parts — the standard layers in one step (ADR 0039)
`engines-part add <doc|slug> <part> [--id <id>] [--target <instrument>] [--bus <bus>] [--dry-run]`
(`make part PIECE=<slug> PART=<name>`) merges a whole layer: instrument, modulators, pools, routes,
sends, a frozen level when the doc has `safe_bounds`, pitches as scale degrees of the doc's mode,
comments, unique ids, checked, one write. Author the send bus first (parts send into the doc's first
bus). How the seven map onto this recipe:

| part | gives | for the house style |
|---|---|---|
| `drone` | a low `saw_soft` on the root two octaves down, `context` + `orrery_disable` | the **sub**: move it to f0/2 (`offset`), `level_db` ≈ −3, `cutoff_hz` ≈ 320 |
| `binaural` | a `type:"binaural"` sine pair on the root two octaves down, hard L/R at ±2 Hz (`spread_hz: 4`, theta), dry, `context` + `orrery_disable` | not in the house recipe — a **binaural bed** for sleep/focus tracks (ADR 0047): set `spread_hz` to the band, `root_hz` to pin a carrier in Hz; keep it dry (a send blends L/R); headphones |
| `partials` | three triangle voices on h3 h4 h6, just-tuned, `context` | re-voice to h3 h5 h6 h7 h9 with the cents table; add the bloom LFO |
| `halo` | a slow detuned pad on 5↓·1·3·5, `orrery_role: "chord"`, wet | re-voice to the series' tempered image (no third, no seventh); `shift_st: 12` + the sheen LFO |
| `pulse` | a 3-mode modal pluck + a loop-locked `fit: "chord"` phrase cell | the **piston**: `resonators`, f0/2f0, `fit: "literal"` pools (calm/sparse/busy) |
| `breath` | a ~22 s free sine LFO on a target's `cutoff_hz` (`depth_oct` 0.66) | the slow breathing LFOs (re-rate to 0.02–0.05 Hz, one per lever) |
| `shimmer` | a shimmer-reverb bus + a ~43 s swell of its shimmer + a send from the target | an extra shimmer bus; the recipe's own bus is the doc's `rv` |

The **crank** (a `dual` PM voice), the chug and the firing tremolo have no part — write them by hand
from the skeleton below.

## The gotchas that will bite you (read these — they cost real iterations)
- **Nothing sounds until the `matrix` wires it.** A modulator reaches audio only via a `mod` entry
  (→ an instrument param / `global.*` / a bus param, its depth under the one key the dest's unit
  takes: `depth_oct` into `cutoff_hz`, `depth_db` into a gain, plain `depth` into a `0..1` param — a
  wrong key is a check error that names the right one) or a `note` entry (→ `instruments.<inst>.notes`).
- **Envelope is note-relative**: each articulation starts its ADSR at its own onset. A **held drone**
  needs `sustain > 0`; a pluck is `attack_s: 0`, short `decay_s`, `sustain: 0`. The `modal` type
  ignores the ADSR and filter entirely (intrinsic decay; `level_db` still applies).
- **Pitch**: note `offset` is semitones from `root` at octave 4 (root `A`, offset 0 = A4 440 Hz;
  −24 = A2 110 Hz; −36 = A1 55 Hz). Per-voice `cents_offset` is how you reach just intonation.
- **`cutoff_hz` is real Hz, 80..8000 on a log curve** — drift and routes move it in octaves
  (`depth_oct`; 6.64 oct span the range, 0.8 oct is a gentle bloom). A layer whose content sits
  *above* its own cutoff is filtered away — the crank's PM harmonics need `cutoff_hz` ≥ ~1270, the
  partials (330–1000 Hz) ≈ 1160, a halo in offsets −5..14 ≈ 1050.
- **`dual` PM**: sidebands at `f0 ± k·ratio·f0` — an **integer** `osc_b_ratio` keeps every sideband on
  the harmonic series (1.0 = all harmonics, 2.0 = odd only); a fractional ratio is inharmonic/bell-like.
  `fm_index` 1.0 = 8 rad (far too much); 0.2–0.3 is a warm growl. `fm_feedback` brightens osc B.
- **Just vs tempered beating**: a tempered halo tone against a just partial beats at their frequency
  difference — h5 (−14¢ ⇒ ~4 Hz at C#5) throbs pleasantly at low level, h7 (−31¢ ⇒ ~14 Hz at G5) is
  rough. Keep the halo's *home* third-less and seventh-less; let the Orrery visit those tones briefly.
- **The halo's ids**: the Orrery rewrites the `chord` role as a single step-0 voicing with the plan's
  ids and **`cents_offset: 0`** — author the chord role tempered, with no cents; keep the just tuning on
  the context layers, which are never rewritten.
- **`oneshot` vs `arp` for the pulse**: both get energy-weighted pool selection + refit, but only `arp`
  tracks get the Arranger's octave lifts and euclid-density writes. A pulse that must stay on f0 is
  `oneshot` + `fit:"literal"`. A `lib:` entry is `fit:"chord"`: under a plain render it snaps to the
  authored `harmony.mode` scale, under the Orrery it follows the chord.
- **Phrase arm requirements**: explicit `step_duration`; pools tile every `length` host steps; `dur`
  present = gated re-strike, absent = HOLD (glide on re-pitch); every gate must end inside its tile
  (`t + dur ≤ length` — a hard error otherwise); unknown pool ids are hard errors;
  `sync:"loop"` tiles re-anchor each loop (mechanical — right for an engine), `sync:"free"` precesses.
- **Only an instrument literally named `event`** uses the bell envelope; every other name uses ADSR.
- **`shift_mix` modulation needs a base `shift_st`** (`+12` reads far better than −12).
- **Sends are explicit** — a doc without `buses` plays dry (v0.9 removed `global.reverb_mix` and the
  reverb it implied), and an instrument with no `sends_db` is fully dry. `buses.<bus>.return_gain_db`
  is the wet level (it defaults to `-Infinity`, silent — engines-check warns); `predelay_ms` is milliseconds.
- **Output runs hot, there is no master limiter (ADR 0021)** — the doc is the gain stage. Sums peak
  ~3.5× (+11 dB); `master_gain_db` ≈ −13 lands ≈ 0.55 FS for this architecture — below the −8 dB
  default floor, which is fine (an authored value below a *default* range is kept; ADR 0032 amendment),
  but freeze it with a band so drift can't creep it up:
  `safe_bounds: { "global.master_gain_db": { min: -13.15, max: -13.15 } }` (a zero-width band; drift
  reflects off band edges, ADR 0032). An explicit band side is a hard clamp — a `min` above the
  authored value raises it. Gains top out at +6 dB, and their default safe ceiling is 0 dB. To mute a
  voice, `level_db: -Infinity` (silent, never drifted) or `enabled: false`; a route into a `-Infinity`
  gain stays inaudible too (routes sum in dB; engines-check warns). So a **swell out of near-silence is a
  finite floor plus a dB depth**: `level_db: -40` + a unipolar LFO at `depth_db: 34` rises to −6 dB.
  The clip-guard test renders raw *and* from the controller's operating
  point, and fails if the controller plays the doc louder than it renders.
- **Drift creeps loudness up over hours** — and not only through levels. The categorical layer (15 % of
  loops) swaps waves (`saw_soft → square` on a sub is a big jump), and `return_gain_db`, cutoffs and mod
  depths (trims and the chug included) wander. On a gain, or a depth into one, the caps are scaled by
  0.3: `max_change` 0.05 ≈ 1 dB a loop, `max_drift` 0.10 ≈ ±2 dB. Pin the level-ish levers via
  `safe_bounds` caps and set `max_drift: 0.10`, then verify
  over **2 h**, not 30 min. `dither_disable: true` on an instrument exempts it from all drift (numeric
  and categorical) if a layer must never change.
- **Energy bands lie for a centre-drone**: the sub band (20–60 Hz) dominates `verify`'s energy
  split — expected. And a low modal pulse barely registers in its `>1 kHz crest`; prove the pulse
  with `verify`'s pulse section (the crank band's envelope spectrum) instead — the dominant line must
  be the 8th/beat.
- **Control precedence** (ADR 0033): the doc's `optimize`/`movement`/`constraints` apply by default;
  `--optimize`/`--orrery` layer over them; an `--objectives <path>` **replaces** them — and `engines-play`/
  the TUI write a fresh template to that path at start-up (copy a shipped objectives doc over it
  *after* launch), while `engines-song --objectives` loads it.
- **Captures alias the rhythm** (4/loop at 1/8, 3/8, 5/8, 7/8): prove rhythmic modulation in the audio.
- **Determinism**: same `loop_id` + snapshot ⇒ bit-identical audio — the A/B diff is exact.

## Skeleton (JSON5 — a renderable starting point; the fixture is the annotated full version)
```json5
{
  schema_version: "0.9", loop_id: 2803,
  meta: { created_by: "ambient-loop-doc", center_hz: 110, note: "…" },
  transport: { bpm: 66, master_steps: 16, step_unit: "beat", swing: 0.08, timing_jitter_ms: 3 },
  harmony: { root: "A", mode: "mixolydian" },        // h7 in the series ⇒ mixolydian
  global: { master_gain_db: -13.15, stereo_width: 0.55 }, // measured peaks (raw + 2-h engines-song) go in the header comment
  safe_bounds: {                                      // gain-staged for hours under the controller
    "global.master_gain_db": { min: -13.15, max: -13.15 },   // under the −8 dB default floor: author the band (frozen)
    "instruments.sub.params.level_db": { min: -2.85, max: -2.85 }, "instruments.crank.params.level_db": { min: -5.19, max: -5.19 },
    "instruments.crank.params.drive_db": { min: 10.24, max: 10.24 }, "instruments.partials.params.level_db": { min: -10.46, max: -10.46 },
    "instruments.halo.params.level_db": { min: -17.08, max: -17.08 }, "instruments.piston.params.level_db": { min: -10.46, max: -10.46 },
    "buses.rv.return_gain_db": { min: -6.94, max: -6.94 }, // every level-ish lever frozen at its authored value (min == max)
  },
  max_drift: 0.10,
  instruments: {
    sub:      { enabled: true, pitched: true, note_engine: { polyphony: 1, glide_ms: 0 },
                params: { wave: "saw_soft", level_db: -2.85, cutoff_hz: 318.49, attack_s: 1.05, decay_s: 0.6, sustain: 1.0, release_s: 2.4 }, sends_db: { rv: -26.02 } },
    crank:    { enabled: true, pitched: true, type: "dual", note_engine: { polyphony: 1, glide_ms: 0 },
                params: { osc_a_wave: "sine", osc_b_wave: "sine", osc_a_level_db: -1.94, osc_b_level_db: -Infinity, osc_b_ratio: 1.0,
                          fm_index: 0.24, fm_feedback: 0.2, drive_db: 10.24, level_db: -5.19, cutoff_hz: 1267.9, resonance: 0.1,
                          attack_s: 0.9, decay_s: 0.6, sustain: 1.0, release_s: 2 }, sends_db: { rv: -16.48 } },
    partials: { enabled: true, pitched: true, note_engine: { polyphony: 5, glide_ms: 0 },
                params: { wave: "triangle", voices_detune_cents: 0, level_db: -10.46, cutoff_hz: 1156.4, attack_s: 1.8, decay_s: 0.9, sustain: 0.85, release_s: 2.8 }, sends_db: { rv: -6.02 } },
    halo:     { enabled: true, pitched: true, note_engine: { polyphony: 4, glide_ms: 900 },
                params: { wave: "saw_soft", voices_detune_cents: 4, level_db: -17.08, cutoff_hz: 1054.6, resonance: 0.15,
                          attack_s: 2.1, decay_s: 0.9, sustain: 0.75, release_s: 3.2, shift_st: 12, shift_mix: 0.0 }, sends_db: { rv: -0.92 } },
    piston:   { enabled: true, pitched: true, type: "modal", note_engine: { polyphony: 3, glide_ms: 0 },
                params: { material: "resonators", num_modes: 5, t60_mode: "note", ring_ratio: 0.895, brightness: 0.45, strike_pos: 0.22, stretch: 0.0, level_db: -10.46 }, sends_db: { rv: -12.04 } },
  },
  modulators: {
    lfo_breath: { type: "lfo", output: "control", wave: "sine", sync: "free", rate_hz: 0.045, polarity: "bipolar" },
    lfo_load:   { type: "lfo", output: "control", wave: "sine", sync: "free", rate_hz: 0.031, polarity: "bipolar" },
    lfo_sheen:  { type: "lfo", output: "control", wave: "sine", sync: "free", rate_hz: 0.021, polarity: "unipolar" },
    lfo_fire:   { type: "lfo", output: "control", wave: "sine", sync: "free", rate_hz: 27.5, polarity: "bipolar" }, // f0/4
    chug: { type: "step_seq", output: "control", sync: "loop", length: 4, step_duration: "1/64", entry_value: 0.0,
            steps: [ { step: 0, value: 0.0 }, { step: 1, value: 0.55 }, { step: 2, value: 0.2 }, { step: 3, value: 0.7 } ] },
    drone_sub:      { type: "step_seq", output: "note", sync: "loop", length: 16, orrery_role: "context", orrery_disable: true,
                      steps: [ { step: 0, voicing: { s: { offset: -36 } } } ] },
    drone_crank:    { type: "step_seq", output: "note", sync: "loop", length: 16, orrery_role: "context", orrery_disable: true,
                      steps: [ { step: 0, voicing: { c: { offset: -24 } } } ] },
    drone_partials: { type: "step_seq", output: "note", sync: "loop", length: 16, orrery_role: "context", orrery_disable: true,
                      steps: [ { step: 0, voicing: { h3: { offset: -5, cents_offset: 2 }, h5: { offset: 4, cents_offset: -14 },
                                 h6: { offset: 7, cents_offset: 2 }, h7: { offset: 10, cents_offset: -31 }, h9: { offset: 14, cents_offset: 4 } } } ] },
    halo_chords: { type: "step_seq", output: "note", sync: "loop", length: 16, orrery_role: "chord",
                   steps: [ { step: 0, voicing: { v1: { offset: -5 }, v2: { offset: 0 }, v3: { offset: 7 }, v4: { offset: 14 } } } ] },
    piston_seq: { type: "step_seq", output: "note", sync: "loop", length: 16, step_duration: "1/16", orrery_role: "oneshot", humanize_ms: 4,
                  phrase: { pool: ["idle", "fourstroke", "overrun", "lib:pedal_pulse"], select: "seeded" } },
  },
  sequence_pools: {   // t/dur in host steps (= beats); fractional t = sub-step
    idle:       { fit: "literal", tags: ["calm", "sparse"], length: 4,   // the beat on f0, the "and" an octave up as a 55% ghost
                  notes: [ { t: 0.0, offset: -24, dur: 0.35 }, { t: 0.5, offset: -12, dur: 0.2, prob: 0.55 },
                           { t: 1.0, offset: -24, dur: 0.35 }, { t: 1.5, offset: -12, dur: 0.2, prob: 0.55 },
                           { t: 2.0, offset: -24, dur: 0.35 }, { t: 2.5, offset: -12, dur: 0.2, prob: 0.55 },
                           { t: 3.0, offset: -24, dur: 0.35 }, { t: 3.5, offset: -12, dur: 0.2, prob: 0.55 } ] },
    fourstroke: { fit: "literal", tags: ["calm"], length: 2, notes: [ { t: 0.0, offset: -24, dur: 0.4 }, { t: 0.5, offset: -12, dur: 0.2, prob: 0.8 }, { t: 1.0, offset: -12, dur: 0.25 } ] },
    overrun:    { fit: "literal", tags: ["busy"], length: 2,            // triplet ratchet — thirds of a beat
                  notes: [ { t: 0.0, offset: -24, dur: 0.25 }, { t: 0.333, offset: -12, dur: 0.2 }, { t: 0.667, offset: -12, dur: 0.2 },
                           { t: 1.0, offset: -24, dur: 0.25 }, { t: 1.333, offset: -12, dur: 0.2 }, { t: 1.667, offset: -12, dur: 0.2, prob: 0.7 } ] },
  },
  matrix: {
    m_breath_partials: { source: "lfo_breath", dest: "instruments.partials.params.cutoff_hz", kind: "mod", depth_oct: 0.797, mode: "add", depth_slew_ms: 1500 },
    m_breath_shimmer:  { source: "lfo_breath", dest: "buses.rv.effects.verb.params.shimmer", kind: "mod", depth: 0.22, mode: "add", depth_slew_ms: 1500 },
    m_load_fm:  { source: "lfo_load",  dest: "instruments.crank.params.fm_index", kind: "mod", depth: 0.07, mode: "add", depth_slew_ms: 2000 },
    m_sheen:    { source: "lfo_sheen", dest: "instruments.halo.params.shift_mix", kind: "mod", depth: 0.2, mode: "add", depth_slew_ms: 1500 },
    m_fire:     { source: "lfo_fire",  dest: "instruments.crank.params.level_db", kind: "mod", depth_db: 1.04, mode: "add" },   // ±1 dB, summed in dB
    m_chug:     { source: "chug",      dest: "instruments.crank.params.level_db", kind: "mod", depth_db: -6.18, mode: "add" },  // dips 0..−4.3 dB
    m_sub: { source: "drone_sub", dest: "instruments.sub.notes", kind: "note", voices: 1, priority: 1 },
    m_crank: { source: "drone_crank", dest: "instruments.crank.notes", kind: "note", voices: 1, priority: 1 },
    m_partials: { source: "drone_partials", dest: "instruments.partials.notes", kind: "note", voices: 5, priority: 1 },
    m_halo: { source: "halo_chords", dest: "instruments.halo.notes", kind: "note", voices: 4, priority: 1 },
    m_piston: { source: "piston_seq", dest: "instruments.piston.notes", kind: "note", voices: 3, priority: 2 },
  },
  buses: { rv: { return_gain_db: -6.94, effects: { verb: { type: "reverb", params: { room: 0.8, damping: 0.3, shimmer: 0.35, width: 0.8, predelay_ms: 18 } } } } },
}
```

## Verification (always do this; it is the deliverable's proof)
Run from the repo root; it needs only the release `engines-song` (`cargo build --release --bin
engines-song`). The checks render in memory, write no files (`--out <file>` keeps the render), read
JSON or JSON5 docs, and print the report as text (`--json`: one object). Exit code: **0** a pass
(with or without warnings), **1** a FAIL verdict, **2** the check couldn't run (a bad flag, a refused doc).
```sh
cargo build --release --bin engines-song
./target/release/engines-song verify <doc.json5> --ab                 # 2 raw loops: level, bands, centre, every route's share
./target/release/engines-song voyage <doc.json5> --movement default   # the Orrery headless, 20 loops, at its worst case
./target/release/engines-song --doc <doc.json5> --out /tmp/s.wav --duration 2h --orrery   # peaks under the controller (~20 min)
```
- **verify** — the doc as written (no controller), 2 loops (`--loops N`): basics (finiteness,
  **peak**, RMS), the spectral band split, spectral-centroid modulation split into a **slow** trend
  (free LFOs) and a **rhythmic** residual, onsets/crest, per-beat energy. For this style expect: peak
  0.50–0.55, a dominant sub band, slow trend swing of a few hundred Hz, and ignore the >1 kHz crest
  (the pulse is low).
- **verify**'s centre section — the house-style proof, run when the doc has a centre (`--f0`, else
  `meta.center_hz`, else the lowest `context` pitch). **Harmonicity**: % of 40–1500 Hz energy within
  ±1.5 % of `k·f0` (target ≥ 35 %), + the rest of the `f0/4` family (the sub-octave floor + firing
  sidebands; centre + family ≥ 90 %), + "other" (target ≤ 2 %); the top spectral peaks labelled by
  harmonic number and cents (your just partials should land within ±2 ¢ of the table). **Pulse**:
  the envelope spectrum of the crank band — the dominant line must be the 8th (or the beat);
  transient onsets per loop (8ths = 32 at 16 beats). **PASS** or **CHECK**: a CHECK is a WARN, not a
  FAIL (only a centre drone wants PASS), and the house style needs PASS.
- **verify --ab** — removes each `matrix` route in turn and prints its **diff-RMS** contribution (one
  render per route). The engine is deterministic, so a non-zero diff = that route reaches the audio;
  a route at 0 % is a **FAIL**. The centre layers must be the three largest; halo ≈ 25–40 %; pulse ≈ 7–15 %.
- **engines-song** (the render) — the peak **as played**: the controller's safe-clamped start, then
  2 h of drift + Orrery. The `done:` line prints `peak … dBFS · N clipped`. It must say 0 clipped. A
  raw render alone can't show drift's ~5 dB of creep by the second hour (or a value an explicit band
  clamped).
- **voyage** — the Orrery's picks with nothing rendered (they never depend on the audio): chord +
  tension per loop, distinct chords, tension min/mean/max, longest excursion, **TAMED** or
  **ROAMING** (a verdict, not a FAIL: a piece may want to roam). Run it at `--movement default` (the
  worst case); the doc's own tamed `movement` (`--movement doc`, the default) only makes it calmer.
- To isolate one part's exact contribution (reverb included): render with and without its route and
  subtract — what `verify --ab` does for every route.

Read the outputs against the spec; if a requirement isn't visible in the numbers, fix the doc and
re-render. Keep `skills/shared/engine-facts.md` open while authoring — it is the distilled engine contract.

## Long tracks: a `song` over the doc (ADR 0029)
A **song** gives the open-ended loop *form* and an *end*: an ordered `sequence` of **blocks**, each a
**matrix configuration** held for `loops` inner loops, then `tail_loops` of release. Blocks re-wire the
matrix **only** (layers merge over `matrix` by route id: a field overrides, `null` removes, a full entry
adds), so one engine plays straight through and every seam glides; at each block start the controller
and the Orrery reset. Read `lab/tests/fixtures/song_idle_night.json5` and engine-facts → **Songs** first.

### Song spec (what to deliver — on top of items 1–10)
1. **A finished base doc.** The song inherits the house-style doc whole (sound, `safe_bounds`,
   `max_drift`, its `optimize`/`movement`), so build and verify it as a plain loop doc first. Its top-level
   `matrix` is the **home block** — the full idle, what `verify` (and its A/B) and a plain listen play.
   Add one `unity` source (below); anything else a block routes must already exist in the base doc.
2. **An arc of 3–6 blocks, one idea each**, designed by *subtraction* from the home matrix — the house
   style's energy ladder:

   | block | routes vs the base | what the ear gets |
   |---|---|---|
   | **idle** | the base matrix | the full engine: pulse + chug + firing tremolo under the halo |
   | **settle** | − pulse, − chug, + crank trim | drone + halo: the engine idling down |
   | **deep** | − pulse, − chug, − firing, + trim; sheen/shimmer depth up, room + | no rhythm at all — space instead of motion |
   | **surface** | the base again, usually with an explicit `seed` | the engine waking: a fresh (or recapped) voyage |

   **All-night sleep** (5–6 h): settle → idle → deep → surface, deep the longest (≈ 40 %), settle and
   surface ≈ 15 % each (the reference song: 48 m / 1 h 42 / 2 h 11 / 48 m). **Nap** (20–90 min): the
   same shape, scaled. **Focus** (1–3 h): idle-led — idle → settle (a 5–10 min breath) → idle′ (a recap
   `seed`) → …. End on **surface** only when the brief wants a wake-up; a track that should end asleep
   ends on **deep** and its tail rings out of stillness. **One idea per seam** (`--dry-run`'s route
   column shows exactly what changes): every seam is an event, and in a sleep track an event is a
   disturbance. Count ideas, not routes — a removal and its level trim are one move, and so are the
   depth nudges serving the block's idea: idle → **deep** changes 6–7 routes (pulse, chug, firing out;
   + trim; sheen/shimmer up; + room) for *one* idea, motion out / space in, and glides smooth in both
   `song_idle_night` and the skeleton's nap. A seam carrying two ideas wants a block between them;
   `engines-song seams` judges (a LOUDNESS/TIMBRE lurch = the seam carries too much).
3. **Loop math in the comments.** `loop_s = master_steps·60/bpm` (Idle: 16 @ 66 = 14.545 s);
   `loops = round(minutes·60 / loop_s)`; each block's start–end time as a comment; the total
   `(Σ loops + tail_loops) · loop_s` in the header. `engines-song --dry-run` must print the same table.
4. **Seams that glide.** `transition_s` 8–15 s: at most the shortest block (a hard error otherwise),
   never over 60 s (lint: it blurs whole sections). Reuse **one route id per purpose** across layers
   (`m_crank_trim` in every chug-less layer) so neighbouring blocks *glide its depth* rather than fade
   one route out and another in. A held voice that loses its note route plays out on its own
   `release_s` (the house-style layers have 2–3.2 s; ~0 lints and clicks).
5. **A tail that rings out.** `tail_loops × loop_s` ≥ the measured ring-out to −60 dBFS + ~3 s. The house
   style rings out in 3–8 s, so **1 loop** (14.5 s) is enough; take 2 for room ≥ 0.9, pushed shimmer or
   long releases. `engines-song seams` measures it and prints the `tail_loops` that would do.
6. **Gain-staged PER BLOCK** — no limiter, and every block is a fresh controller start:
   - **Offsets go through the constant-offset idiom** — `unity` (a one-step control step_seq at 1.0)
     routed at the offset in the dest's unit (`depth_db: -1.74` trims a gain by 1.74 dB). It is the
     only way a block can shift a base value.
   - **Book-keep every removal.** Removing a route into a gain moves that gain's mean by
     `−depth_db × mean(source)` dB (gain routes sum in dB): the chug (mean 0.3625 × −6.18 dB ≈ −2.2 dB,
     ≈ −2 dB of loudness) ⇒ trim the crank by about that in every chug-less block (the reference song:
     **−1.74 dB**); the f0/4 firing tremolo is a bipolar LFO (mean 0) ⇒ no trim; a unipolar LFO,
     `depth_db × 0.5`. Removing the pulse drops transients, not the mean.
   - **Every depth drifts, trims included**: the controller dithers any depth within its dest's span
     (±66 dB for a gain, ADR 0054 P4), a gain depth at 0.3× the caps (`max_drift: 0.10` ≈ ±2 dB). A
     trim that must stay exact for hours, and any route a layer adds that raises something level-ish (a
     swell, a longer room), gets a zero-width band keyed by its depth key:
     `safe_bounds: { "matrix.m_crank_trim.depth_db": { min: -1.74, max: -1.74 } }`.
   - **Targets**: every block's controller start **≤ 0.6 FS** (0.7 is the clip guard's hard ceiling;
     full-length renders peaked **+3.4 dB** (1-h nap) and **+3.9 dB** (5.5-h reference) over their block
     starts — drift, Orrery voicings, busy pools) and **never louder than its raw render** (the
     `master_gain_db` band and its −8 dB default floor apply per block); neighbouring blocks' RMS within
     **~1–2 dB** for sleep (both songs: 0.1 dB spread). A deliberately quieter block is fine — let the
     seam glide it.
7. **Verified** (below), with the plan total, the per-block start peaks + RMS spread and the scaled
   render's peak / clips / seams / ring-out in the header — and, to ship, the full render's `done:` line.

### Song workflow
1. **Finish the base doc** (Workflow 1–10); then **design the arc on paper**: blocks and their one idea,
   minutes → loops, `transition_s`, `tail_loops`, which removals need a trim (and how much).
2. **Write it** into the same JSON5 (scratch: `sandbox/song_<name>.json5`): the `unity` modulator, the
   `song` (`matrices` holding only what differs; `sequence` with labels and start-time comments), and a
   header table (block · length · idea) with the gain-staging rationale.
3. **Check + plan**: `engines-check` (it covers `song`), then `engines-song --doc <song> --dry-run`
   — no error, **no `warning: song:` lint**, starts/lengths match your comments, the routes column
   shows only the intended changes, and recap blocks show the same seed.
4. **Per block**: `engines-song verify <song>` renders every block raw and from the controller's
   start, and prints peak / RMS / Δ vs the previous block (its block table). Fix every FAIL (louder under the controller
   ⇒ author or repair the band; > 0.7 FS ⇒ lower `master_gain_db` inside its band or trim the hot block),
   a > 0.6 FS WARN unless the full render proves it, and every level step you didn't intend. Prove a layer's new route reaches the audio with
   `engines-song verify <song> --block NN --ab` (a trim must not read 0 %).
5. **The whole form, scaled down**: `engines-song seams <song>` shrinks every block (the longest → 12
   loops, none under 4; seam and tail kept — **not** `--max-loops`, which cuts the song short), renders
   it as it will play (drift + Orrery) in memory, and reports peak, clips, per-block level + creep,
   seams and the tail. `engines-song voyage <song>` runs the same scaled form through the Orrery
   (per-block voyages, recaps).
6. **Iterate** until `seams` says **CLEAN**: a LOUDNESS lurch ⇒ trim the step or lengthen the
   seam; a TIMBRE lurch only under drift ⇒ a wave that drift flipped snapping back at the block reset
   (`dither_disable: true` on that instrument if it matters); a CLICK ⇒ `release_s`; a cut-off ending ⇒
   `tail_loops`; clips ⇒ gain.
7. **To ship**: the full render, `make song SONG=<doc> SONG_OUT=renders/<name>.wav -- --orrery --format
   s24` (×6–7 realtime: 10 min for 1 h, ≈ 50 min for 5.5 h) — its `done:` line must say **0 clipped**
   (the doc's own `optimize`/`movement` apply — next gotcha); then `engines-song seams <song> --full`
   (the whole song at its real length, in memory — as long as the render itself). Then the fixture,
   its clip guard and the README (*Shipping an example*, below).

### Song gotchas
- **`verify`'s raw render plays the top-level matrix** (the home block): its measures and `--ab` are
  that block's — `--block K` measures block K (the block table still covers every block's start).
  `--max-loops` truncates a song. Live, `engines-live` plays the doc as a plain loop until a block is
  auditioned (VS Code's CodeLens / panel, or `make live PIECE=<slug> -- --loop-block K` /
  `--play-from K`): a looped block re-materializes on every save, while a playing song records saves
  and applies them on the return to the loop.
- **Only routes change**: not a pool's tags, an LFO rate, a param or the `movement`. A "calmer" block
  removes or trims routes; it can't retune the Orrery.
- **Block 0 runs on the session seed**: a recap needs the *same explicit `seed`* on both blocks.
- **The doc's control drives the render**: `engines-song` applies the doc's `optimize`/`movement`;
  `--optimize`/`--orrery` layer over them; `--objectives <file>` replaces them wholesale (an
  `optimize:"hold"` there turns drift off — the gain staging was proven for drift).
  Prove the voyage at default movement (the worst case: the nap's 58- and 115-loop blocks held mean
  ≤ 0.12, max 0.40, one 3-loop excursion); the companion cuts it to a chord change per 4 loops.
- **Every block restarts the voyage at home**: a short block is mostly tonic — right for sleep; keep a
  focus track's blocks long if they should develop.
- **Seams step whatever isn't a modulation slot**: `wave`/`enabled`, `voices_detune_cents`,
  `glide_ms`, bus `width` — and a wave that drift flipped snaps back at the block reset.
- **Long blocks drift further**: a block start resets the walk, but a 2-h block walks for 2 h, and a
  scaled render can't show it (−3.2 → −0.4 dBFS for the reference song, −5.3 → −3.0 for the nap). The
  ≤ 0.6 FS block-start target and the level caps are what hold it.

### Song skeleton (merged into a finished house-style doc)
```json5
  modulators: {
    // … the base doc's modulators, plus the per-block OFFSET source (inert until a layer routes it):
    unity: { type: "step_seq", output: "control", sync: "loop", length: 1, entry_value: 1.0, steps: [ { step: 0, value: 1.0 } ] },
  },
  safe_bounds: {
    // … the base doc's band + level caps, plus the depths a layer adds that must not drift (every depth
    // is a drift candidate; the band is keyed by the route's depth key):
    "matrix.m_crank_trim.depth_db": { min: -1.74, max: -1.74 },
    "matrix.m_room.depth": { max: 0.08 },
  },
  song: {
    matrices: {
      settle: {                                       // drone + halo: the engine idling down
        m_piston: null, m_chug: null,                 // null removes a route
        // the chug's dips averaged ≈ −2 dB on crank.level_db (0.3625 × −6.18 dB in dB terms): give it back
        m_crank_trim: { source: "unity", dest: "instruments.crank.params.level_db", kind: "mod", depth_db: -1.74 },
      },
      deep: {                                         // nothing rhythmic; space instead of motion
        // 7 route changes vs idle but ONE idea (motion out, space in) — see "One idea per seam"
        m_piston: null, m_chug: null, m_fire: null,   // the firing tremolo is bipolar (mean 0): no trim
        m_crank_trim: { source: "unity", dest: "instruments.crank.params.level_db", kind: "mod", depth_db: -1.74 }, // same id ⇒ glides
        m_sheen: { depth: 0.32 }, m_breath_shimmer: { depth: 0.30 },                // a field overrides
        m_room: { source: "unity", dest: "buses.rv.effects.verb.params.room", kind: "mod", depth: 0.08 },  // a full entry adds
      },
    },
    sequence: [                                       // loop = 16 beats @ 66 bpm = 14.545 s; loops = round(min·60 / 14.545)
      { matrix: "settle", loops: 33, label: "settle" },   // 0:00:00 – 0:08:00
      { loops: 58, label: "idle", seed: 11 },              // 0:08:00 – 0:22:04  (no layer ⇒ the base matrix)
      { matrix: "deep", loops: 115, label: "deep" },      // 0:22:04 – 0:49:56  (the longest)
      { loops: 41, label: "surface", seed: 11 },           // 0:49:56 – 0:59:53  (idle's seed ⇒ a recap)
    ],
    transition_s: 12,                                 // ≤ the shortest block
    tail_loops: 1,                                    // 14.5 s ≥ the ring-out + margin ⇒ 1:00:07 in all
  },
```

### Song verification
```sh
cargo build --release --bin engines-song
./target/release/engines-song --doc <song.json5> --dry-run           # the plan + any song lints
./target/release/engines-song verify <song.json5>                    # + every block as the controller starts it
./target/release/engines-song verify <song.json5> --block 2 --ab     # one block's routes (a trim must not read 0 %)
./target/release/engines-song seams <song.json5>                     # the whole form scaled down, drift + Orrery
./target/release/engines-song voyage <song.json5>                    # the same scaled form through the Orrery
```
- **verify**'s block table — per block, as the controller starts it: raw vs controller peak, RMS (past
  the fade-in), a coarse band split, Δ RMS vs the previous block. FAIL = louder under the controller
  (a value under an explicit `safe_bounds` floor), or over `--ceiling` (0.7); WARN = over `--target`
  (0.6), a step over 3 dB, or any engine lint.
- **seams** — CLEAN needs 0 clipped (and a scaled peak ≲ −3 dBFS), plus each seam's 1-s
  loudness step ≤ max(1.5 dB, the in-block p99.9), centroid step ≤ max(150 Hz, 1.5× p99.9), and its
  transient energy above 4 kHz ≤ 1.5× the in-block maximum; the tail must reach −60 dBFS in time.
  Glided seams measure ≤ 1.1 dB / ≤ 125 Hz / ≤ 0.4×. A 0-s seam with a 3.4 dB level jolt reads 4.2 dB
  and 1285 Hz; a zero-release drone cut, 3.4× (a CLICK). Per-block creep > 2 dB warns. `--longest N`
  / `--min N` reshape the copy, `--full` plays the song at length, `--out <file>` keeps the render.
- **voyage** on a song — the whole voyage, then one line per block (tension, verdict — "indicative"
  under 8 moves — opening chords). Blocks sharing an explicit `seed` are named as a recap (WARN if
  they open differently); a shared opening without one is called a coincidence.
- Measured on a 1-h nap test song (the skeleton above over Idle): block starts 0.45–0.48 FS, RMS spread
  0.1 dB; the 6:33 scaled render −5.3 dBFS, 0 clipped, seams ≤ 1.06 dB / ≤ 124 Hz / ≤ 0.39×, ring-out
  3.3 s, recap detected; the full 1:00:07 render (10 min) −3.0 dBFS, 0 clipped, seams ≤ 1.75 dB (in-block
  p99.9 1.81) / ≤ 147 Hz / ≤ 0.23×, ring-out 3.6 s — CLEAN.

## Shipping an example (vs scratch)
- A **piece** (the normal case) lives in `pieces/<slug>/` (gitignored; ADR 0034) — see `/engines-new`. `sandbox/` is gitignored scratch. Tracked examples live in `lab/tests/fixtures/` as `.json` or
  `.json5`; add the file name to `playback_fixtures_do_not_hard_clip` (`crates/engines_core/src/engine/
  tests.rs`). It renders each fixture raw **and** from `Controller::new(doc, ..).operating_point()`, and
  asserts both are finite and ≤ 1.0 FS, that the controller's start is **not louder** than the raw render
  (which catches a sub-floor `master_gain_db`), and that it stays under a per-fixture ceiling (0.7 FS for
  the house-style reference). Link it from README "Authoring a loop document". The TUI
  Setup picker lists both extensions; `engines-render`/`engines-play`/`engines-song` take either.
- A **song** ships as `lab/tests/fixtures/song_<name>.json5`. Its per-block clip guard is
  `song_fixture_blocks_do_not_hard_clip` (same file), which today names only `song_idle_night.json5`
  and asserts a 5–6 h total — turn it into a list (with each song's length range) when adding one. It
  renders every block from `Controller::new(block).operating_point()` and requires ≤ 0.7 FS and no
  `song:` lints — but aim ≤ 0.6: full-length renders peaked +3.4 dB (1-h nap) and +3.9 dB (the 5.5-h
  reference, 0.61 → 0.955 FS) over their block starts. Link it from README "Authoring a song".

## Variant: chord-progression ambient (the older recipe)
If the brief explicitly asks for a *moving* harmonic bed rather than a centred drone — a slow
i–bVII–bIII–v progression, a gliding four-voice pad, a deep gliding bass, a free-running euclid pluck
precessing against a bar-locked wah — see the "Variant" half of `skills/shared/music-theory.md` and the
fixtures `lab/tests/fixtures/loop_ambient_showcase.json`, `loop_shimmer_showcase.json` and
`loop_orrery_dorian.json` (a calm D-dorian bed whose chords, bass and arp the Orrery rewrites live).
The same gotchas, verification and gain-staging rules apply; the Orrery then drives `chord` + `bass` +
`arp` roles with no context floor (it roams by design).
