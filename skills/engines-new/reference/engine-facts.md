# Engine facts (the authoring contract)

Distilled from the engine's source (the file names in headings are where each fact came from, in an
Engines checkout). These are the behaviors that actually govern how a loop doc sounds. When in doubt,
check (`engines-check`) or try it (`engines-song verify`, a short render in memory), never guess. In
piece work (the `/engines-new`, `/engines-edit` and `/engines-song` skills) don't open the source:
what the build can't do is reported, not engineered. The formal definition is the engine's v0.9 loop
contract, which its strict checker embeds: run `engines-check <doc> --parts` on every draft (unknown
keys, bad enums/ranges and dangling routes are errors), and `engines-check --catalog` for every
param's range, default, unit and depth key.

## Document shape (v0.9)
- **Native units** (ADR 0054): every param is written in its unit under a suffixed key — `_hz`, `_s`,
  `_ms`, `_db`, `_cents`, `_st`; no suffix = a unitless `0..1` fraction (or `osc_b_ratio`, a ratio).
  Silence is **`-Infinity`**, on gains only (`+Infinity`/`NaN` are refused). The doc is normalized in
  memory, so drift and modulation still reason in `0..1` inside. A v0.8 doc is refused (`schema 0.8`):
  the editor offers **Migrate to schema v0.9** on it, which converts the doc and keeps its comments. A
  stale v0.8 key (`cutoff`, `level`, …) is an unknown-key error.
- Parsed as **JSON5** (ADR 0023): `//` / `/* */` comments and trailing commas are fine, so an annotated
  doc renders as-is.
- Top level: `schema_version` ("0.9"), `loop_id` (u64; also the RNG seed for `prob`/noise), optional
  `meta` (free object), `transport`, `harmony`, `global`, `instruments`, `modulators`, `matrix`,
  optional `buses` (the send-bus / effects framework — shimmer reverb; see below), and the controller
  knobs `safe_bounds` (per-path Tier-2 ranges in native units — see gain staging under `global`) and
  `max_drift` (how far drift may wander from the authored value, a fraction of the param's range;
  default 0.15 — on a gain ×0.3, ≈ ±3 dB), and an optional `song` (a sequence of blocks that change the
  matrix and, optionally, the control — see **Songs** below).
- `instruments`, `modulators`, `matrix` are **id-keyed objects** (the key is the id). `steps` and
  `pitch_walk.pool` are arrays. Per-step `voicing` is an **id-keyed object** of voices.
- Iteration is by **sorted key** (deterministic).

## transport / harmony / global
- `transport`: `bpm`, `master_steps` (steps in the master loop), `step_unit` ("beat"), `swing`,
  `timing_jitter_ms`. **Loop seconds = master_steps × 60/bpm.** **Feel is applied** (ADR 0028 D7,
  deterministic): `swing` (0..1) delays every **odd on-grid step** by `swing·step/3` (1 = triplet feel;
  sub-step `t` onsets never swing); `timing_jitter_ms` + per-modulator `humanize_ms` shift every strike by
  a seeded ±ms (a gated note's off rides its onset). Control step-seqs don't swing/jitter — only strikes.
- `harmony`: `root` (note name `C`,`C#`/`Db`,…,`B`) and `mode` (label only; affects scale-aware
  `pitch_walk`, not chord offsets). **All note `offset`s are semitones from `root` at octave 4.**
  `pitch_to_hz`: midi = 60 + root_semitone + offset; root `D`, offset 0 → D4 ≈ 293.66 Hz.
- `global`: `master_gain_db` (dB, default 0 = unity; −60..+6, `-Infinity` silent), `stereo_width`
  (`0..1`, default 0.5; `width*2` = side gain). v0.9 removed `reverb_mix` and the reverb it implied:
  **a doc without `buses` plays dry** — author a bus (`buses.rv`, below) and give each instrument a
  `sends_db`; the bus's `return_gain_db` is the wet level. **Summed output is hot (~3.5× peak) and there is
  no master limiter in renders — by design** (ADR 0021/0031; renders feed stems and a DAW's mastering), so
  the doc must self-gain-stage — **for what the controller plays, not just a raw render**:
  - Every live path (`engines-live`, `engines-song`) starts from `Controller::new`, which
    keeps the controller inside each param's **Tier-2 safe range** (ADR 0010). Defaults (the engine's
    `params.rs`, mirrored in the schema's `x-safe-bounds-defaults`):
    `master_gain_db` **[−8, 0] dB**, `level_db` [−26, 0] dB, the other gains (osc levels, sends,
    `return_gain_db`) [−60, 0] dB, `cutoff_hz` [101, 8000], `fm_feedback` [0, 0.95]; strict check
    refuses any gain above +6 dB. A **default** range
    only bounds what drift does: an authored value outside it is kept and the range widens to it
    (ADR 0032 amendment) — a `master_gain_db` of −13.15 plays at −13.15, a `level_db: -Infinity` is a
    silent mute (and isn't drifted). Each side of an explicit `safe_bounds` band, though, is a **hard clamp**: the
    controller raises/lowers an authored value into it. (Before 2026-10-09 the defaults clamped too —
    the old "Idle" authored master gain −8.9 dB, played at −8 dB and clipped.)
  - Drift still creeps upward over hours, so **author a band** on gain levers:
    `safe_bounds: { "global.master_gain_db": { min: -13.15, max: -13.15 } }` (native units; replaces the
    default range for that path). Drift **reflects** off band edges (ADR 0032), so a plain `max` means
    "never louder" but the lever still wanders below it; a **zero-width band (`min == max`) freezes**
    it. That's what you want for gain: the controller plays it exactly as written and drift never moves
    it. Don't freeze timbre levers you want to move.
  - **Leave drift headroom — for hours, not minutes.** Drift's walk creeps loudness up. Its categorical
    layer swaps waves on 15 % of loops, and a sub that goes `saw_soft → square` is much hotter. `level_db`s,
    `return_gain_db`, cutoffs and matrix depths also wander ±`max_drift` (a gain, or a depth into one,
    at 0.3× the caps — ADR 0054 P2: `max_change` 0.05 ≈ 1 dB a loop, `max_drift` 0.10 ≈ ±2 dB). Measured on Idle over 2 h of drift +
    Orrery: a master-only band reached **+6.3 dB** over the first loop and clipped. The first 30 min were
    clean, so a short check proves nothing. Freezing every level-ish lever at its authored value (voice
    `level_db`s, `drive_db`, the bus `return_gain_db` — `min == max`) plus `max_drift: 0.10` held it to +4.6 dB.
    (A plain `max` at the authored value is the "breathe" variant: never louder, but levels wander up to
    `max_drift` below — slow volume variation over hours. Ask which the piece wants.)
  - So for a long-session doc: author that recipe (copy the reference doc's `safe_bounds`/`max_drift`
    block), target **≈ 0.50–0.55 FS** on the 3-loop render, then prove it under the controller with
    `engines-song --doc <doc> --out <wav> --duration 2h --orrery` (≈ 20 min at ×6 realtime). Its summary line
    prints `peak … dBFS · N clipped`, and that must say 0 clipped. The house-style Idle lands 0.54 FS raw
    at `master_gain_db` −13.15. An unbroken session longer than you verified can still creep further. `engines-song`
    songs reset the controller at every block, which bounds the walk.

## Instruments (`instruments.<name>`)
`enabled`, `pitched`, `note_engine { polyphony, glide_ms }`, a heterogeneous `params` map, and an
optional `sends_db` map (`{ <bus_id>: dB }`, −60..+6, `-Infinity` = no send) routing the instrument's
signal into send buses (see **Send buses** below; no entry for a bus = no send, no map = dry).
- **The name `event` is special**: it uses a bell envelope `exp(-3t/decay_s)` (`decay_s` = its ring,
  0..4 s). Every other name uses the **full ADSR**.
- `params` (each numeric in the unit its key names — no suffix = `0..1`; modulatable + a dither candidate):
  | param | meaning |
  |---|---|
  | `wave`/`source` | `sine`, `triangle`, `saw_soft` (richest pad/pluck), `sine_bell`, `filtered_noise` (→ name `event` aside, source picks the osc) |
  | `level_db` | per-instrument gain in dB (−60..+6; `-Infinity` silent; default −6.02) |
  | `cutoff_hz` | LP brightness in Hz, 80..8000 on a log curve (capped at 0.45·sr; default ≈ 2010). Routes and drift move it in octaves (`depth_oct`; 6.64 oct span the range) |
  | `resonance` | filter Q, `0..1 → Q 0.707..6`. Subtle, broadband-quiet lever |
  | `drive_db` | soft-clip pre-gain, 0..+20 dB (×1..×10; 0 bypasses the clipper) — adds harmonics → helps a part cut |
  | `attack_s` `decay_s` `release_s` | ADSR times in seconds: 0..3 / 0..4 / 0..4 |
  | `sustain` | ADSR hold **level** `0..1` (the audible steady state) |
  | `band_lo_hz`/`band_hi_hz` | bandpass edges for `filtered_noise`, 20..4000 Hz (log; the floor was 80 Hz before ADR 0046's amendment). Routes and drift move them in octaves (`depth_oct`; 7.64 oct span the range) |
  | (pitched noise) | `filtered_noise` + an explicit **`pitched: true`** (ADR 0046): the band_lo_hz..band_hi_hz noise goes on through a band-pass **at each voice's note** (glide too), 24 dB/oct either side, level-matched across notes; `resonance` = the peak at the note, 0 none … 1 +24 dB narrow (a breathy whistle) at the same loudness. A note outside the band is mostly cut (that noise isn't there): for a low pitched thump (a kick, a heartbeat, down to B0) put `band_lo_hz` an octave or more under the note. A narrow peak rings up slowly on a low note (resonance 0.95 at D2 ≈ 0.12 s), so a very short envelope wants a lower `resonance` (≈ 0.3) to get the hit through. Left out / `false` = plain band noise, bit-identical to before |
  | `voices_detune_cents` | unison detune: the sounding voices spread evenly across ±this many cents (0..100) |
  | `shift_st` | octave-pedal interval, **−24..+24 semitones** |
  | `shift_mix` | dry↔shifted crossfade `0..1` (0 = off; needs a nonzero `shift_st` to be audible) |

## The amplitude envelope (critical)
`engine.rs`: `t = elapsed_decay_s + frame/sr`. A fresh articulation now stamps
`elapsed_decay_s = -(frame/sr)` so **the envelope is note-relative** — every hit starts its ADSR at its
own onset, anywhere in the loop, and carries across the seam.
- **Pluck**: `attack_s: 0`, short `decay_s` (0.25–0.45), `sustain: 0`, short `release_s` → percussive
  on every euclid/step hit. This is the right way to make a rhythm.
- **Held drone/pad/bass**: `sustain > 0`. Articulate once (at step 0) and **glide** (reuse the voice id
  with a new `offset`) to change pitch continuously across chords; `glide_ms` sets the portamento.
- **`event` bells**: `decay_s` sets the ring length; they re-trigger fresh per hit now too.
- Historical note: before the note-relative fix, mid-loop `sustain:0` notes and `event` bells were
  **silent** (born past their decay). If you see that, you're on a stale binary — rebuild.

## Modulators (`modulators.<id>`)
Two `type`s, two `output` domains, two `sync` modes. Phase persists across loops via the snapshot.
- **`lfo`** (`output:"control"`): `wave`, `rate_hz`, `polarity` (`bipolar` −1..1 / `unipolar` 0..1).
  For ambient "breathing" use slow rates (0.02–0.08 Hz) and `sync:"free"` (don't reset each loop).
- **`step_seq`**: `sync` (`"loop"` restarts each bar / `"free"` carries phase = drifts), `length`,
  optional `step_duration` ("1/N" = fraction of the **master loop**, default `1/length`), `humanize_ms`.
  - `output:"control"` → a `control` curve from `steps[].value` (with `entry_value` for seam continuity
    and `interp:"smooth"` for ramps, else stepped/held). Route it to any modulatable param; the
    route's depth scales it in the dest's unit.
  - `output:"note"` → note events from **either** explicit `steps[].voicing` **or** a `generator`
    `{ "kind":"euclid", "pulses", "rotation" }` (rhythm) + optional `pitch_walk { pool, order }`
    (self-contained pitch: cycles the pool `up`/`down`/`random`). euclid fires `pulses` notes spread
    over `length` steps.

### Sub-step onsets + sequence pools (ADR 0028)
- A `voicing` entry may carry `t` (`0 ≤ t < 1`, in host steps): it fires at `step + t` — `0.5` = the "and",
  `0.333`/`0.667` = triplets. `t: 0` is the default (on the grid).
- **Phrase arm** — the THIRD exclusive note source (`steps ⊕ generator ⊕ phrase`):
  `phrase: { pool: ["<id>", "lib:<name>"], select: "seeded"|"cycle", tags?, active? }` plays entries from the
  doc's top-level `sequence_pools` and/or the built-in library (`crates/engines_core/assets/phrase_library.json5`,
  8 entries: `glass_up`, `and_lift`, `triplet_ripple`, `pedal_pulse`, `sigh`, `alberti_low`, `long_hold`,
  `syncopa`). **Requires an explicit `step_duration`.** An entry is `{ fit, tags, length, notes: [{ t, offset,
  dur?, legato?, prob?, velocity?, cents_offset? }] }` tiled every `length` host steps; `t`/`dur` are in host
  steps. **`dur` present = gated** (note-off at `t+dur`, which must stay `≤ length`; re-strikes each tile);
  **no `dur` = HOLDS** across
  tiles (pitch changes glide — the pad idiom). `select:"seeded"` re-picks per tile from the ever-growing tile
  ordinal, so plain renders vary tile-to-tile yet stay seam-safe and deterministic.
- `fit`: `chord` (default) snaps to the authored `harmony.mode` scale under a plain render and is re-voiced to
  the current chord under the Orrery (strong notes to the *nearest* chord tone — an arpeggio can collapse:
  F A C under Gm7 → F B♭ B♭); **`arp`** = the same, but along the written line (ties follow its direction, no
  repeats where it moves; when no new chord tone fits, a tension over the chord — 9th/11th/13th, never an
  avoid note — so no step is ever re-struck; kept inside the modulator's **note range** if it sets one, else
  inside the entry's written range; never developed — no rotate/fragment) — use it for a chord-following
  arpeggio; `root` = parallel transpose; `literal` = exactly as written (use it for a
  pulse that must stay on one pitch — see `loop_engine_drone_a_mixolydian.json5`).
- Orrery: a phrase-armed **`arp`**-role mod gets energy-weighted selection (`busy` ∝ energy, `calm`/`sparse` ∝
  1−energy), refit, AND the Arranger's octave lifts; an **`oneshot`**-role one gets the selection + refit but is
  never octave-shifted. Unknown pool ids are hard `validate()` errors.

### Voice transitions (per id, in a note seq's `voicing`)
- Same id + same pitch → **sustain**; same id + new `offset` → **glide** (portamento if `glide_ms>0`);
  new id → **articulate** (fresh envelope/filter); `"release": true` → note-off + release tail.
  Voicing entries also take `cents_offset` (detune/beating) and `prob` (0..1 chance; rolled per
  `loop_id`+frame+index — **deterministic within a fixed `loop_id`**, so a plain render thins the same
  way every loop; vary `loop_id` for loop-to-loop variation).
- Generator (euclid) notes are **anonymous** → each is a fresh articulation (re-triggers) → percussive.

## Matrix (`matrix.<id>`) — routing
- `source` (a modulator id) → `dest` (JSONPath) with `kind`:
  - **`mod`**: `dest` = `instruments.<inst>.params.<param>`, `global.<param>`, `buses.<bus>.return_gain_db`
    or `buses.<bus>.effects.<fx>.params.<param>`. The depth (signed) is in the **dest's unit, under exactly
    one key** (ADR 0054 P4; a wrong or second key is an error that names the right one): `depth_oct`
    (octaves at the source's ±1) into `cutoff_hz`, `band_*_hz`, `t60_s`, `ring_ratio`; `depth_db` into a
    gain or `drive_db`; `depth_s` into `attack_s`/`decay_s`/`release_s`; `depth_ms` into `predelay_ms`;
    `depth_st` into `shift_st`; `depth_cents` into a detune; `depth_hz` into the binaural `root_hz`/
    `spread_hz`; `depth_x` into `osc_b_ratio`; plain `depth` into a `0..1` param (`resonance`, the mixes,
    `stereo_width`, reverb params, `midi` params). depth × source value, `mode:"add"` onto the base, then
    clamped to the dest's range. **Gain routes sum in dB**: `depth_db: 3` from a bipolar LFO swings ±3 dB
    at any base, and a route into a `-Infinity` base stays silent (engines-check warns — give it a finite
    floor such as −60 dB). `depth_slew_ms` smooths depth changes
    across loops (great for slow LFOs). Control values are smoothed sample-to-sample → zipper-free, and
    a sharp stepped control = fast attack/quick release on the target param (e.g. a level gate).
  - **`note`**: `dest` = `instruments.<inst>.notes`. `voices` (budget into that instrument's polyphony)
    and `priority` (lower = kept; higher = stolen first when sources share an instrument).
- **Modulatable params** picked up: `level_db,cutoff_hz,resonance,drive_db,attack_s,decay_s,sustain,
  release_s,band_lo_hz,band_hi_hz,shift_st,shift_mix`, the `dual` levers (`osc_a_level_db,osc_b_level_db,
  osc_b_ratio,osc_b_detune_cents,fm_index,fm_feedback` — a slow LFO on `fm_index` is a big, legible timbral
  mover) and the `modal` levers (`t60_s` or `ring_ratio`, `brightness,strike_pos,stretch`), the `binaural`
  pair's `root_hz,spread_hz` (real Hz, `depth_hz` in Hz — `depth_hz: 2` on `spread_hz` = ±2 Hz; never
  drifted, and neither is a depth into them) + `global.{master_gain_db,stereo_width}` + send-bus params
  `buses.<bus>.effects.<fx>.params.{room,damping,shimmer,predelay_ms}` and `buses.<bus>.return_gain_db`.
  Every param is written native, but the whole modulation pipeline runs in normalized `0..1` (converted
  once on parse and on write): a native depth is a normalized distance × the dest's span.

## Send buses & effects (`buses.<id>`)
A **send-bus / effects framework** (the one place fundsp's *graph* composition is allowed; per-voice
synth DSP stays hand-rolled). Each instrument routes a `sends_db` level (dB) into one or more buses; a
bus runs an ordered (sorted-id) `effects` chain and returns its wet signal to the master at `return_gain_db`.
- **`buses.<bus>.effects.<fx>`**: `type` (e.g. `"reverb"`) + a `params` map. The **reverb** effect:
  `room` (decay / size), `damping` (HF absorption → darker tail), `shimmer` (**octave-up pitch-shifted
  feedback** — the rising ethereal sheen; `0` = plain reverb), `width` (reverb-internal stereo,
  **build-time-fixed → never dithered/modulated**), `predelay_ms` (milliseconds, 0..100).
- **`buses.<bus>.return_gain_db`** (dB, −60..+6): wet return level into the master mix. It defaults to
  `-Infinity` (silent) — engines-check warns when a bus has none.
- **No implied reverb**: a doc without `buses` plays dry, and `global.reverb_mix` is refused (ADR 0054).
  `engines-migrate` writes a v0.8 doc's implied reverb out as an explicit `rv` bus (return at the old
  `reverb_mix`, every instrument without sends sending at 0 dB).
- **Ambient use**: a long `room` + low `damping` + a touch of `shimmer` (0.3–0.6) on a high-register
  bell/pad send is the signature shimmer-reverb pad. Slowly modulating `shimmer` or `room` from a free
  LFO is a gorgeous, legible gesture (and a strong steering lever — see Legibility).

## Legibility / register (so parts are audible AND measurable)
The ear (and every audio-embedding model we measured) responds to **gross steady-state spectrum and
level of the dominant sound**, not fine detail. Practical rules:
- **Register-separate** the parts: bass `offset −24..−17` (≈55–110 Hz), pad/chords `≈0..17` (D4–G5),
  pluck/sparkle `+12..+27`. A part whose fundamental is **above its own `cutoff_hz`** is filtered away —
  keep a pluck's `cutoff_hz` above the notes it plays, or raise the notes above the pad.
- `cutoff_hz` (steady-state brightness), `sustain`/`level_db`/`master_gain_db` (steady level), and
  register `shift_st` are the **legible** levers. `resonance`, `drive_db`, `attack_s`/`decay_s`/
  `release_s` are subtle/transient — fine for human listening, weak as steering signals.
- To make `shift_mix` modulation read, set `shift_st: 12` (up-octave accent rings clear) rather than −12
  (a sub-octave the bass masks).

## Polyrhythm mechanics
- A `sync:"free"` step-seq whose total period (`length × step_duration`) does **not divide the bar**
  precesses against it every loop (e.g. `length:23` at `"1/16"` = 1.4375 bars → drifts).
- Euclid `E(pulses,length)` is intrinsically cross-rhythmic (`E(7,16)`, `E(5,16)`, `E(9,16)`); pair a
  free euclid with a **bar-locked** (`sync:"loop"`) gate/wah so a stable pulse underpins the drift.
- Coprime lengths between two free seqs (e.g. 16 vs 17, 7 vs 16) maximize the precession period.
- The control-rate captures alias bar-synced motion (1/8,3/8,5/8,7/8 of the loop) — verify rhythmic
  modulation in the **audio** (scripts), not the capture JSON.

## Repeating bars & grid-locking (drum-machine patterns)
A loop-synced `step_seq` whose pattern is **shorter than the loop cycles repeatedly within it**:
`modulator.rs` advances `step_i` while `frame < total_frames` and wraps the `length`/euclid pattern, so
the pattern repeats `1 / (length × step_duration_frac)` times per loop. This is how you build a classic
**N-step bar that repeats every bar** over a multi-bar loop:
- Loop of `B` bars with `S` steps/bar → `length:S`, `step_duration:"1/(S·B)"`.
- e.g. a **4-bar 4/4 loop**: `transport.master_steps 16 @ 60 bpm` = 16 s loop (bar = 4 s, 16th = 0.25 s);
  a 16-step bar = `length:16, step_duration:"1/64"` (cycles 4× = once per bar).
- `pitch_walk` advances **continuously across the internal cycles** (it only resets at the loop seam),
  so a *fixed* euclid rhythm still **walks melodically** bar-to-bar — grid-locked but not robotic.

**euclid 4/4 placements on a 16-step bar:** `E(2,16)`→steps [0,8] (kick on beats 1&3),
`E(4,16)`→[0,4,8,12] (four-on-the-floor), `E(8,16)`→eighths, `E(16,16)`→sixteenths. To **grid-lock** an
arp/voice *to* the beat (instead of the polyrhythm recipe above) use `sync:"loop"` with a
`step_duration` that divides the loop; chords/bass on explicit steps `0,4,8,12` then land on the bar
downbeats automatically. (This is the deliberate trade: grid-lock for a steady 4/4 vs. coprime/free for
polyrhythm — pick per piece.)

## Determinism
Same `loop_id` + phase snapshot ⇒ **bit-identical** audio. That makes the A/B diff-RMS test exact: a
non-zero difference between full and a routing removed is *exactly* that routing's audio contribution.

## Synth types for the centre (ADR 0009 — `crates/engines_core/src/synth.rs`)
- **`type:"dual"`** — two oscillators; osc B phase-modulates osc A (DX-style) and can also be mixed in.
  Params: `osc_a_wave`/`osc_b_wave` (categorical), `osc_a_level_db` (default −1.94 dB), `osc_b_level_db`
  (default `-Infinity` = modulator only), **`osc_b_ratio`** (0.5..12: osc B = note × ratio),
  `osc_b_detune_cents` (0..50 cents, upward), **`fm_index`** (0..1 → 0..8 rad of PM), `fm_feedback` (0..1 osc-B self-feedback;
  safe bound 0.95). PM sidebands land at `f0 ± k·ratio·f0`: an **integer ratio keeps every sideband
  on the harmonic series** (1.0 = all harmonics, 2.0 = odd harmonics only, 3.0 = 1,2,4,5,7,8…); a
  fractional ratio is inharmonic (bells). `fm_index` 0.2–0.3 (≈ 1.6–2.4 rad) is a warm growl;
  `fm_feedback` brightens osc B (adds a saw-like edge to the modulator). `drive_db` is applied inside
  the 2×-oversampled block (band-limited). Dual **rides the shared LP filter and ADSR** — keep its
  `cutoff_hz` ≥ ~1270 (≈ 1.3 kHz) or the filter eats the very harmonics PM creates (measured on the
  reference doc: ≈ 550 Hz left the growl dull). All six levers are matrix-modulatable; a slow free LFO on
  `fm_index` (depth ~0.07, slew 2000 ms) is one of the biggest legible timbral movers in the engine.
- **`type:"modal"`** — a parallel bank of decaying two-pole resonators struck at articulation. Params:
  `material` (`tuning_fork` 3 partials: 1, 6.27, 17.55 — near-pure; `vibraphone` 5: 1, 3.97, 9.5, 16,
  24.6; **`resonators` 8: exactly 1..8 — harmonic**, amps 1, .6, .42, .3, .22, .16, .12, .09; `marimba`
  3: 1, 3.93, 9.2), `num_modes` 1..8 (truncates the table), `brightness` (tilt across modes),
  `strike_pos` (0..1 mode weighting `|sin(π(k+1)·pos)|`), `stretch` (inharmonicity), and the ring,
  by `t60_mode`: `"fixed"` (the default) takes **`t60_s`** (the fundamental's T60, 0.05..8 s, log;
  default 0.63), `"note"` takes **`ring_ratio`** (0.5..2, log) — the check requires the one that
  matches the mode and refuses the other; both take `depth_oct`. **`"note"` couples the ring to the
  driving note's duration**: `T60 = note_dur · ring_ratio` (1 ⇒ rings for the note; < 1 staccato;
  > 1 blooms) — so a pool's `dur` is audible. Per-material normalisation strikes at ~0.9 FS at
  `level_db: 0`. Modal **ignores the shared filter and ADSR** (`AmpPolicy::Intrinsic`; `level_db` still
  applies), so `cutoff_hz`/`attack_s`… on a modal instrument are inert. Cost = voices × modes × frames.
- **`type:"binaural"`** (ADR 0047) — a binaural-beat drone: two subtractive voices, the left hard LEFT at
  root − `spread_hz`/2 and the right hard RIGHT at root + `spread_hz`/2 (110 / 2 ⇒ 109 | 111). Every
  subtractive param applies to both (wave, filter, drive, ADSR, level, `voices_detune_cents`, the octave pedal —
  whose oscillator gets the **same** ±spread/2 Hz, one beat rate). **`root_hz`** (REAL Hz 20..1000, fractions
  fine) pins the carrier and the note only **gates** (pitch ignored); leave it out and the root is the note
  (follows key/Orrery). **`spread_hz`** (REAL Hz 0..40) is the beat: delta < 4, theta 4–8, alpha 8–13, beta
  13–30, gamma ≥ 30. Both are read exactly as written and **never drifted** (`ParamKind::Fixed`). The dry
  **skips the master `stereo_width`** (and its stems are gain-only), so isolation survives any width — but a
  **send blends L and R** in the bus (engines-check warns; keep it dry). Use **`wave:"sine"`**: a saw's k-th
  harmonic beats at k · spread. Carriers ~100–250 Hz are the classic bass bed; beats need headphones. Noise
  is an error. The `binaural` part is the ready-made bed (held root two octaves down, theta, dry).
- `type:"midi"` produces no audio (ADR 0026) — irrelevant to a rendered doc.

## Orrery facts that shape a doc (the movement axis — `orrery/`, `controller/orrery_axis.rs`)
- **Roles** (`orrery_role` on NOTE modulators): `chord` (re-voiced every phrase — a single step-0
  voicing with the plan's voice ids, **`cents_offset` reset to 0**; several chord mods are octave-
  staggered), `bass` (single voice on the chord root/fifth; **omit the role and the centre never
  moves**), `arp` (euclid `pulses` written by the Arranger's density, `pitch_walk.pool` re-filled,
  sectional **octave lifts**; a phrase-armed arp gets pool selection + refit *and* the lifts),
  `oneshot` (accent placement — skipped for phrase-armed mods, which instead get **pool selection +
  refit but never an octave shift**), `context` (read-only; with `orrery_disable: true` = a fixed
  pitch the voyage fits around). Binding is by role; missing roles are skipped gracefully.
- **The note range** (ADR 0042): `orrery_note_min` / `orrery_note_max` on a note modulator (semitone
  offsets from the root, like `offset`) bound everything the Orrery writes into it — chord voicings, the
  bass, arp pools and Arranger octave lifts, one-shot accents, re-fitted phrases, a track rested at its
  home. Per phrase the whole surface takes the octave shift that leaves the fewest notes outside (no
  shift preferred), so a lift that won't fit is skipped; any note still outside folds by octaves. The
  plain way to keep an `arp` track in a register, e.g. F3..F5 in F = `-12` / `12`. **It takes precedence
  over a pool entry's written line** (ADR 0042 amendment): the phrase refit already works inside it. A
  `fit: "arp"` entry is bounded by the range instead of its written extremes (one key set ⇒ the other side
  open), so widening `orrery_note_max` gives the arp room above the line as written. `chord` moves a note
  past an edge to the nearest fitting tone inside, `root` picks the octave that fits best, `literal` only
  folds. A line written wholly outside the range moves by octaves first; one that overlaps it keeps its
  register.
- **The harmonic floor**: the sounding pitch-classes of every `context`/`orrery_disable` note modulator
  feed `theory::chord_fit` — per fixed pitch, the smallest interval to any chord tone scores: shared
  tone **+1.0**, P4/P5 **+0.6**, 3rd/6th **+0.4**, whole tone **0**, tritone **−0.7**, semitone
  **−1.0** (averaged; weight `w_fit` 0.8 — a soft bias, never a veto). More floor pitches ⇒ a tighter
  orbit.
- **Gravity defaults**: inward pull `γ = 0.55 + 0.25·loops_since_home`, preference weight 0.8,
  repetition penalty 0.6. **`HarmonicPreference` defaults**: `tension_target` 0.32, **`pad_center`
  7** (moved by whole octaves to the octave nearest the chord track's **authored** voicing mean,
  ADR 0041: chord-role voicings are close stacks around that centre ± 6, so the home voicing's
  octave is kept — a C3–C5 pad centres on C4 — but its open spread is not), `adventure` 0.35.
- **Phrase arms under the Orrery** (`select_and_refit_phrases`, per phrase window): candidates =
  the `pool` hard-filtered by the ref's `tags`; weight ×(0.25 + 1.5·energy) for `busy` entries,
  ×(0.25 + 1.5·(1−energy)) for `calm`/`sparse`; seeded draw; 40 % of windows apply a development op
  (rotate the pitch row / fragment; never to a `fit: "arp"` entry); then `refit` per the entry's `fit`
  inside the modulator's note range (`chord` → chord/scale tones, `arp` → chord tones along the line,
  `root` → parallel transpose, `literal` → verbatim). A **narrative drop** materialises an empty
  `realized` tile for one phrase (no new onsets; a dur-less held voice rides through — documented gap).
- **Objectives knobs** (`contracts/objectives_v0.1.schema.json`, runtime, JSON5): `optimize`
  `hold|drift|live_edit`; `movement: { orrery, bind, phrase_loops (default 1 = a chord change EVERY
  loop), tension_target 0.32, tension_max 0.7 (a breach forces a return home), voice_changes_per_loop
  {min,max} (4-loop moving-average governor), home "global"|"per_modulator", arrange: { arp_density
  {min,max} (euclid arps only), return_hazard_scale (×1.5 ⇒ returns fire sooner) } }`. A tamed
  companion: `phrase_loops 4–8, tension_target 0.10–0.15, tension_max 0.35–0.40, voice_changes
  {0.25, 1.25}, return_hazard_scale 1.5`.
- **`engines-song --objectives <path>`** replaces the doc's own control with an objectives file (an
  `optimize: "hold"` there turns drift off). Pieces keep their control in the doc instead (ADR 0033).
- **Measuring a voyage headlessly**: `engines-song voyage <doc>` steps the controller with nothing
  rendered (the Orrery's picks never depend on the audio) and tabulates chord + tension per loop: 20
  loops of a plain doc, a song scaled down like `seams`. `--movement default` replaces the doc's
  movement with the default (the worst case); the default `--movement doc` plays its tamed settings.
  A render's `--loop-log` rows carry the same `harmonic_move: { chord, tension, default_chord }` on
  move loops.

## Songs — the loop of loops (ADR 0029 — `song.rs`, `bin/engines-song.rs`)
- **Shape** (optional top-level `song`; `deny_unknown_fields` on the song and each block):
  `song: { matrices: { <layer>: { <route id>: <partial matrix entry> | null } }, sequence: [ { matrix?, loops,
  seed?, label?, control? } ], transition_s (default 8), tail_loops (default 1) }`. The schema validates it.
- **Merge** = RFC 7386 JSON merge patch, by route id, over the top-level `matrix`: a field overrides
  (`m_sheen: { depth: 0.32 }`), `null` removes the route, an id the base lacks must be a **full** entry
  (`source`, `dest`, `kind` at least) and adds it. A block without `matrix` plays the base as-is. A
  patch may only name matrix-entry fields (`source dest kind`, a depth key, `mode depth_slew_ms voices
  priority`); its depth key must match the dest's unit, as in the matrix (`m_crank_trim: { depth_db: -1.74 }`).
- **Matrix + control only.** Nowhere to put a per-block instrument, modulator, harmony, transport, global,
  bus or `safe_bounds` change — the instrument and modulator sets never change, so **one engine plays the
  whole song**: voices, filters, reverb tails and free LFO phases cross every seam. Anything a block
  routes must already exist in the base doc; an unrouted modulator is inert.
- **Block `control`** (ADR 0055, optional): a patch over the session control for that block only —
  `{ movement?: <partial movement, merged field by field>, optimize?: "hold" | "drift", max_drift?, max_change?,
  drift?: { <param path>: "hold" | { min?, max? } | { max_drift } } }`. `movement: { orrery: false }` turns the
  Orrery off: its bound voices play their **authored** material for the whole block (no chord changes, no
  Arranger), the seams glide in and out, and the next block with it on starts a fresh voyage from home.
  `drift` keys are drift candidates (as `constraints` keys are): `"hold"` pins the param at its authored value
  (after the safe clamp), `{min, max}` is its own band in **native** units, `{max_drift}` its own normalized
  cap around the authored value — each becomes a `constraints` band that replaces a session band on the same
  key. `max_drift` replaces the doc's for the block; `max_change` can only **lower** the session's step. The
  block wins at its start (over CLI flags too); the live panel's hold ⇄ drift / Orrery switch wins until the
  next block. `--loop-block K` plays block K with its control. Errors (located at the key): a `drift` key that
  isn't a candidate (typo, non-numeric, `dither_disable`), a band outside the range or missing the safe range,
  `min > max`, band + `max_drift` together, `optimize: "live_edit"`. Lints: Orrery fields beside
  `orrery: false`; `orrery` changes in a doc whose Orrery is never on; drift overrides in a block that holds;
  an authored value outside its block band (it gets driven in at the block start).
- **Hard errors** (each names the block / layer / route): empty `sequence`; `loops: 0`; an unknown
  layer; `transition_s` < 0 or **longer than the shortest block** (`loops × loop_s`); an incomplete new
  route; an unknown patch field; a dangling source/dest; any materialized block failing
  `LoopDoc::validate`.
- **Lints** (`warning: song: …` on stderr): a layer never used by the sequence; a `null` removing a route
  the base doesn't have (no-op); `tail_loops 0` (stops mid-sound); `transition_s` > 60 s; a held
  (`sustain` > 0.05) subtractive/`dual` instrument losing its note route at a seam — **including the
  last block → the tail** — with `release_s` < 0.05 (it stops dead: a click, measured 3.4× the in-block
  transient maximum); a total over 24 h.
- **Timing**: `loop_s = master_steps·60/bpm` (frames = `round(loop_s·sr)`); block *k* starts at
  `Σ_{j<k} loops_j · loop_s`; total = `(Σ loops + tail_loops) · loop_s`. 16 beats @ 66 bpm = 14.545 s
  ⇒ 1 h ≈ 247.5 loops, 5.5 h ≈ 1361.
- **Block reset** (`Controller::begin_block`) ≡ a fresh controller on the block: the operating point
  returns to the authored block (drift travel undone, safe-clamped through the doc's `safe_bounds`),
  the drift streak and `max_drift` anchors reset, and the **Orrery restarts from home** (new voyage;
  phrase clock + Arranger aligned to the block start). Kept: the session control, with the block's own
  `control` applied over it (ADR 0055).
- **Seeds**: block 0 runs on the session seed unchanged (a one-block song ≡ a plain session,
  bit-identical); block *k* > 0 on `hash(session, k)`; an explicit block `seed` on `hash(session, seed)`
  — blocks sharing an explicit seed **replay the same voyage** (a recap; give *both* blocks the seed —
  block 0 without one can't be recapped). `--seed` still varies the whole song. Engine rolls (`prob`,
  seeded pools) stay keyed on `loop_id`, which counts on globally across blocks.
- **The seam glides** over `transition_s` (one raised-cosine gain, carried on the `PhaseSnapshot` —
  deterministic — renders stay bit-reproducible): each modulatable param starts where the old block left
  it (drift included) and eases to the new authored value; a **kept** route whose depth changed glides
  from the old depth; a new or re-targeted route fades in from 0; a removed route fades out; a voice
  whose `(instrument, source)` note route vanished gets a note-off at the seam's first frame (its own
  release plays it out). **These step instead** (known gap): categoricals (`wave`, `enabled` — so a wave
  that drift flipped snaps back at the reset), `voices_detune_cents`, `glide_ms`, bus `width`.
- **Renders are per block** (ADR 0043): `engines-song` writes a song's mix and every stem as one file per
  block plus the tail, `<song>-<part>-<NN>-<label>`. NN is the plan's block index; `<song>` is the
  piece slug or the doc's file name. For a song, `--out` names a directory: `x.wav` ⇒ `x/`. The cuts
  are sample-exact and the s16 dither carries across, so the files laid end to end are the `--whole`
  render, bit for bit. Each stem gets one gain over the whole song; a silent block still gets its
  file. The summary's peak and clip counts cover every file. `--whole` gives one file.
- **The tail** drops every note route (voices release) while mod routes keep running (the LFOs still
  breathe the reverb); no drift moves. `engines-song` warns when the last 0.5 s is above
  −60 dBFS ("the ending is cut off"). House style (room 0.8, shimmer 0.35, releases ≤ 3.2 s) falls to
  −60 dBFS **3–8 s** into the tail — one 14.5 s loop is enough.
- **Drift vs block routes**: the controller dithers every `mod` depth within its dest's span
  (|`depth_oct`| ≤ 6.64 into a cutoff, |`depth_db`| ≤ 66 into a gain; ADR 0054 P4), **negative depths
  included** — a trim or the chug wanders ±`max_drift` like anything else (a gain depth at 0.3× the
  caps: `max_drift: 0.10` ≈ ±2 dB). (Before v0.9 only depths in `0..1` drifted, so trims were exact.)
  Freeze a depth that must stay put — a trim, or a route a layer adds that raises something level-ish —
  with a zero-width band keyed by its depth key: `safe_bounds: { "matrix.<id>.depth_db": { min: <d>, max: <d> } }` (doc-level, shared by
  every block; a band naming a route absent from a block is simply unused).
- **The constant-offset idiom**: `unity: { type: "step_seq", output: "control", sync: "loop", length: 1,
  entry_value: 1.0, steps: [ { step: 0, value: 1.0 } ] }` routed `kind: "mod"` at a depth `d` in the
  dest's unit adds exactly `d` to the dest (`depth_db: -1.74` = a 1.74 dB trim; then the dest's range
  clamp). The only way a block can offset a base value. Mean-level
  bookkeeping when a layer **removes** a route into a gain: the dest's mean (in dB — gain routes sum in
  dB) moves by `−depth_db × mean(source)` — a control step_seq's mean is the average of its
  (equal-length) step values; a bipolar LFO's is 0 (only the movement goes); a unipolar LFO's 0.5.
  Idle's chug: `(0 + .55 + .2 + .7)/4 = 0.3625 × −6.18 dB ≈ −2.2 dB` (≈ −2 dB of loudness) ⇒ every
  chug-less block trims the crank by about that (the reference song: −1.74 dB, its v0.8 trim migrated).
- **`engines-song`** (headless `Session` = exactly what the live player plays; ≈ 6–7× realtime with drift +
  Orrery): `--dry-run` prints the block table (label, layer, loops, start, length, seed, routes
  `+added −removed ~changed` vs the previous block) + total + file size; `--emit-blocks <dir>` writes
  `block_NN_<label>.json` (materialized, song stripped, `safe_bounds`/`max_drift` kept); `--out <wav>
  [--format f32|s24|s16] [--optimize drift|hold] [--orrery] [--seed N] [--loop-log <jsonl>]`; the
  summary line prints `peak … dBFS · N clipped`. Size: f32 1.38 GB/h, s24 1.04, s16 0.69; **RF64** past
  4 GiB. `--max-loops` cuts a song **short** (later blocks, seams and the tail never play).
  `engines-song --piece <slug>` renders into the piece's `renders/` with a sidecar.
- **What ignores the song**: `engines-song verify`'s raw render plays the top-level matrix (`--block K`
  measures block *K*, and its block table covers every block's start); `engines-live` plays the
  top-level matrix too, until a block is auditioned (`--loop-block K` re-materializes block *K* on
  every save; `--play-from K` plays the song and records saves without applying them until the return
  to the loop — ADR 0036).
- **Loop log**: every song loop's record (`engines-song --loop-log`) carries `song: { block, seam }`; the
  tail's loops report the last block, and its first loop is a seam (the note routes fade out).

## Pieces and `meta`
- A **piece** lives in `pieces/<slug>/` (ADR 0034): `<slug>.json5` (the one doc; the song is added to
  it later), `brief.md`, `renders/` with a `.json` sidecar per render, and `.live/` (the player's
  `status.json`, `pause` and `history/`). `/engines-new` creates it.
- `meta` is a free object — `meta.center_hz` tells `engines-song verify` the centre; `meta.note` is the
  one-line description the player shows.
