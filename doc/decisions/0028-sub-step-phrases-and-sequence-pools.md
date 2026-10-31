# ADR 0028 — Sub-step melodic timing + sequence pools (the phrase arm)

Status: accepted (built 2026-07; **amended 2026-07-09** — `fit:"chord"` snaps to the authored
scale under plain renders too, see D3; **amended 2026-10-10** — `fit: "arp"`, see the amendment at the end; its range and no-re-strike rules
**amended 2026-10-11** by ADR 0042's amendment)
Relates to: ADR 0005 (param seam), 0009 (synth types), 0014 (Orrery enrichment — explicitly
deferred "generated melody step_seqs"), 0015/0016 (home + leader-led returns), 0017 (axis
seam), 0018 (the Arranger), 0026 (midi type / velocity), and the Orrery design doc's
"comet tails" section (`orrery-generative-harmony-design.md` Part 4).

## Context

Note events were quantized to the modulator step grid: `evaluate_modulator` only ever emitted
frames at `k · step_dur_frames`, although engine *dispatch* has always been sample-accurate
(events carry arbitrary frames through voice/glide/ADSR/MIDI unchanged). Melodically, the
Orrery generated a *bag of pitches*: `theory::melody::next_phrase` produces strong/weak-beat
structure, then flattens it into `pitch_walk.pool`, where a euclid generator re-rhythms it —
the beat information died at that seam. `Motif` was designed as "interval contour + rhythm
cell" but the rhythm cell was never built. Separately, `transport.swing`,
`transport.timing_jitter_ms` and per-modulator `humanize_ms` were parsed-but-never-applied
(the CE-R5.2 debt; ADR 0004 prescribed implementing them deterministically seeded).

This ADR adds **(A)** sub-step note timing as a hand-authorable primitive, **(B)** **sequence
pools** — named, timed phrases (pitch + rhythm + articulation) that hand-authored docs play
directly and the Orrery selects/refits per phrase window — and **(C)** the swing/humanize
implementation. All additive under schema v0.8 (the 0026/0027 precedent).

## Decisions

**D1 — Per-voice sub-step onset `t` on `NoteVoice`** (`0 ≤ t < 1`, in host steps): a voice
fires at `step + t`. Voices sharing an onset group into one event (sorted-id order); groups
split ascending by `t`. Chosen over fractional `Step.step` to keep the step-index lookup and
enable within-step strums. `t: 0` is serde-skipped — legacy docs stay byte-identical.

**D2 — The phrase arm, third exclusive note-source shape** (`steps ⊕ generator ⊕ phrase`):
`modulators.<id>.phrase = { pool, active?, select, tags, realized? }` plays entries from the
doc's top-level `sequence_pools` and/or the **built-in library**
(`crates/sme_core/assets/phrase_library.json5`, `lib:<name>` refs, same `PhraseDef` shape —
pin an entry against engine upgrades by pasting it into the doc). A `PhraseDef` is
`{ fit, tags, length (host steps), notes: [{t, offset, dur?, legato, prob, velocity?,
cents_offset}] }`, tiled every `length` host steps (polymetric under `sync:"free"`). The
phrase arm **requires an explicit `step_duration`** (the `1/length` fallback would re-grid
per selection). Unknown pool ids are hard authoring errors (the unknown-instrument-type
posture); doc pool ids may not squat on the `lib:` prefix.

**D3 — Engine expands, Orrery selects + refits.** `evaluate_modulator` expands the active
phrase (works under plain `sme-render`, no controller). The Orrery, at its existing phrase
clock, materializes `phrase.active` (the choice) + `phrase.realized` (a chord-refit copy that
wins over `active`) — the doc stays the deterministic ground truth, pristine pool entries are
never mutated in place, and the chord state never enters the engine.
**Amended 2026-07-09** (originally: "under a plain render `fit` is inert — entries play
literally"): that made the library's major-triad contours impose out-of-scale pitches on
modal docs (`lib:and_lift`'s offset 16 = F♯ over a D-dorian doc — surfaced immediately in
live use). The engine *does* know the authored harmony, so now: with no `realized` copy
present, a `fit:"chord"` entry snaps every note to the `harmony.mode` scale at expansion
(`theory::phrase_fit::refit_static` — nearest scale tone, avoid rule against the implied
tonic triad; no strong/weak split because there is no chord, only a key). `root` (no chord
to follow) and `literal` (the exact-offsets escape hatch) stay verbatim; an Orrery
`realized` copy is always trusted un-re-fit. Still a pure function of the doc — CE-R7.3
holds.

**D4 — Deterministic voice ids: `dur` decides the articulation family.** Every phrase note
owns id `p{k}` (`k` = its legato-chain head's index; a `legato` note reuses its
predecessor's id → the engine glides). **`dur` present = gated**: paired `release:true` at
`t + dur` (validated `0 < dur` **and `t + dur ≤ length`** — a gate ends inside its own tile, so
the off precedes ANY next-tile strike of the shared `p{k}`: the note's own, a legato chain
head's, or another pool entry's; the engine additionally clamps the off to the tile's last
frame so rounding / late jitter can never push it onto the next tile — review fix 2026-09;
under jitter the bound is `tile_end − ⌈jitter⌉`, since the next tile's strike may itself land
that early, and a late-jittered onset is pulled in to leave its off room — follow-up
2026-09-26; released ids re-articulate fresh). **`dur` absent = the note HOLDS**: the same id
+ pitch next tile sustains (never a re-strike, never an anonymous-voice pileup); a pitch
change (re-selection/refit) glides — the pad/drone idiom for free. This deliberately
diverges from generator notes' anonymous fresh-strike semantics; the trade is that arp-style
notes must carry a `dur` (the library models it).

**D5 — All phrase hash sites are `loop_id`-free**, seeded by the ever-growing **tile
ordinal** (`hash_site(0xAD0028, mod_id, ordinal, salt)`): selection (`seeded` draw /
`cycle`), per-note `prob` (pair-skip: a failed roll skips the strike AND its note-off), and
phrase-note jitter. Rationale: a tile can straddle the loop seam, where `loop_id` changes;
both sides must recompute identical choices. Free-run seam events are **recomputed**, not
carried: an onset with a negative frame was the previous loop's business, one past the loop
end is the next one's — deterministic ids make even a cross-seam note-off recomputable. The
exception is a tile that *starts* on the seam: the previous loop never walked it, so an
early-jittered head clamps to frame 0 (as under loop-sync) instead of vanishing and leaving
an orphan note-off (follow-up 2026-09-26).
Loop-sync tiles re-anchor at step 0 (the ordinal accumulates per STARTED tile, a seam-straddling
one included — review fix 2026-09), clamp early-jittered edge
notes to 0, and carry overflow via `pending_notes` like the walk arms.

**D6 — Cross-seam carry for the walk arms:** `ModulatorPhase.pending_notes` (a `frame`
relative to the next loop + captured voicing) carries any explicit-step/euclid event whose
feel-adjusted or sub-step onset spills past the loop end (free-run non-divisible grids —
previously that hit would have been lost). Plus `phrase_anchor_step` / `phrase_ordinal` for
the tile clock. All three serde-default-skipped → pre-0028 snapshots stay byte-identical
(`modulator_phase_default_serialization_unchanged` pins the four legacy keys).

**D7 — Feel, finally implemented** (deterministic): **swing** delays every **odd absolute
on-grid step** by `swing · step_dur / 3` (swing 1 ⇒ the offbeat sits on the 2/3 triplet
point); fractional-`t` onsets are authored micro-timing and **never swing**. **Jitter**
(`timing_jitter_ms + humanize_ms`) shifts every strike by a seeded ±ms offset
(`hash_site(loop_id, mod, abs_step, "hum")` for the walk arms; ordinal-seeded for phrase
notes); a gated note's off **rides its onset** (sounding length preserved — under loop-sync
that is the *clamped* frame-0 onset when jitter pulled it early). The canonical
fixture's authored `timing_jitter_ms: 8` / arp `humanize_ms: 6` became audible with this —
an intentional one-time render change (kept, per review); the frame-pin test became a
band + determinism assert.

**D8 — Orrery integration** (`select_and_refit_phrases`, called at the phrase boundary
between `vary_arp_pool` and `apply_octaves`): pool hard-filtered by the ref's `tags`,
**energy-weighted** (`busy` entries ∝ Arranger energy, `calm`/`sparse` ∝ 1−energy), seeded
draw; a seeded minority of windows applies a development op (`theory::phrase_fit::
develop_notes` — rotate the pitch row / fragment to a half-tile breath); then
`theory::phrase_fit::refit` per the entry's `fit`: `chord` = strong (whole-step `t`) notes
snap to the nearest chord tone, weak notes to the nearest scale tone with the melody-fit
avoid rule (a semitone above a chord tone resolves up a scale step) — rhythm, gates,
legato, prob, velocity untouched; `root` = parallel transpose (centered); `literal` =
verbatim. `write_oneshots` and `drop_surface` skip/branch on phrase-armed mods (a drop
materializes an **empty `realized`** — legal machine-written silence, no new onsets, tails
ring per D4's gap below). `apply_octaves` shifts the `realized` copy. `HomeSnapshot`
captures the authored `phrase` arm; Orrery-off/return restores it (no pin, no realized).
The axis seam extends: `sequence_pools.*` and `*.phrase.*` are Orrery-owned note content,
never dither candidates (pinned in `categorical.rs`).

## Consequences

- The engine gained no new dispatch machinery — event *production* changed only. All
  determinism guards (two-fresh-engines, seam continuation, MIDI reproducibility, legacy
  snapshot bytes) hold; new snapshot state only serializes for docs that use the features.
- Canonical fixture audio changed once (D7). The fixture/template pair gained an unrouted
  `glints` phrase mod + a `sequence_pools` section (nothing sounds until wired).
- MIDI: phrase velocities reach Note-On (midi instruments only, as before); sub-step onsets
  are sample-exact through the scheduler; note-offs balance across phrase swaps.
- `pitch_walk` remains, still provisional — pools + phrases are the richer replacement path,
  but euclid+pool docs render unchanged.
- The 2026-07-09 amendment changes plain renders **only** for chord-fit entries carrying
  out-of-scale offsets (the library over non-major docs, chiefly); fully diatonic pools —
  the behavior fixture included — are bit-identical, and `literal` docs are untouched.

## Known gaps / deferred

- A **dur-less held phrase voice rings through a narrative drop** (the drop only stops new
  onsets). Acceptable v1; a follow-up could release phrase-held ids in `drop_surface`.
- Likewise a **hold persists across a pool transition**: when the next tile (re-selection or
  an Orrery refit) plays an entry that does not reuse the held `p{k}`, the voice keeps
  sounding (unreferenced ids sustain) until polyphony steals it. That is D4's hold semantics
  as specified — the pad idiom — but pools that mix holds with gated entries accumulate stale
  pitches. Deferred design call: a release-on-switch rule (or an explicit `hold_across` flag).
- **Velocity for internal (non-midi) synths** — deliberately out of scope (it would change
  the rendered-audio contract); revisit as its own decision.
- Phrase selection is seeded/weighted, not bandit-scored — the two-timescale "phrase choice
  as a discrete move with a gravity-default baseline" upgrade mirrors chord transfers and
  can land later without schema changes.
- An instrument authored **without** `note_engine` still gets derived-default
  `polyphony: 0` (all voices truncate — silent). Pre-existing foot-gun, surfaced during
  this work; left untouched here.
- MPE-style `cents_offset` over MIDI stays dropped (ADR 0026's stance).

## Test surface

`modulator.rs`: sub-step grouping/exactness, seam carry + recompute, tile cycling, prob
pair-skip, legato chains, holds, swing/jitter exactness + determinism. `engine/tests.rs`:
bit-identical + JSON-snapshot resume over the phrase fixture, MIDI velocity/balance, hold
sustain (no pileup), snapshot byte-compat, clip guard over the new fixtures.
`phrase.rs`/`loop_doc.rs`: library integrity, validation matrix, lints. `theory/phrase_fit`:
snapping/avoid/transpose/develop. `orrery/mod.rs` + `controller/tests.rs`: energy gating,
octave shift of `realized`, drop silence, home restore, deterministic selection, dither-seam
exclusion. Fixtures: `loop_phrase_pools.json`, `loop_orrery_phrase.json`, and the annotated showcase
`loop_engine_drone_a_mixolydian.json5` (+ `.objectives.json5`: a harmonic-series drone whose
`oneshot`-role phrase arm plays `fit:"literal"` pools — the Orrery picks the pulse pattern, never the pitch); demos:
`sandbox/loop_phrase_demo.json5`, `sandbox/loop_midi_engine_demo.json` (`sparkle_midi`).

## Amendment 2026-10-10 — `fit: "arp"` (contour-preserving chord fit)

`fit: "chord"` moves each strong note to the *nearest* chord tone, with ties going down. For an
arpeggio that collapses neighbours that share a nearest tone. `weighted-blanket`'s `horizon`, an
`arp` track whose pool entry is F3 A3 C4 F4 A4 C5 F5 and back, played F B♭ B♭ F B♭ B♭ F under Gm7
and never sounded G or D. (Its register is held by the modulator's note range, ADR 0042.)

**`fit: "arp"`** (additive; existing docs unchanged), `phrase_fit::refit_arp`. Notes are taken in
time order, and every note, on or off the grid, goes to a chord tone:
- a tie goes the way the written line moves (up while it rises, down while it falls);
- where the written line moves, a note never repeats the previous pitch; it steps on to the next
  chord tone in that direction;
- nothing leaves the entry's written range (a note past an edge folds to the nearest chord tone
  inside it).

  *Amended 2026-10-11* (ADR 0042 amendment, D4–D5): a modulator `orrery_note_min`/`max`, when set,
  replaces the written range (an unset side is open). A step that would re-strike the previous pitch
  takes a tension over the chord (9th/11th/13th, never an avoid note) instead. An `arp` entry is
  never developed (no rotate, no fragment).

Gm7 now plays F3 B♭3 D4 F4 B♭4 D5 F5 B♭4 G4 F4 B♭3 G3. Under a plain render it snaps to the scale
like `"chord"`. Schema enum and `validate_phrase`. Test
`phrase_fit::tests::arp_fit_keeps_the_contour_and_range_on_the_current_chord`.
