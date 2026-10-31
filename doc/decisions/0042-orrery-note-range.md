# ADR 0042 — A note range the Orrery must stay inside (`orrery_note_min` / `orrery_note_max`)

Status: accepted (2026-10-10, branch `feat/sme-live-redesign`; **amended 2026-10-11** — the range
takes precedence over a pool entry's written line, see the amendment at the end)
Relates to: ADR 0014 (roles), 0018 (the Arranger's octave moves), 0019 (drops), 0028 (phrase arms
and their refit), 0041 (the pad register follows the authored octave).

## Context

`weighted-blanket`'s `horizon` is a chord-following arpeggio that must stay between F3 and F5. The
`arp` role brings the Arranger's sectional octave lifts, which would push it outside that range. The
first build avoided them by giving horizon the `oneshot` role, which is never octave-shifted. The
user rejected that: it made the author know the Orrery's internals and misuse a role. What an author
means is a range: *"the Orrery may do what it likes with this part, inside these notes."*

## Decision

**D1 — Two optional keys on a note modulator:** `orrery_note_min`, `orrery_note_max`. They are
semitone offsets from the harmony root, the same unit as a voicing `offset` (the user's choice over
note names). Either may be absent (no floor or no ceiling); `min > max` is an error, and so is a value beyond
±127 semitones (`ORRERY_NOTE_LIMIT`, in the schema too: the octave fold jumps by whole octaves, but a
far-off bound is a typo, not a range). They are
ignored outside Orrery mode, and the authored content plays as written.

**D2 — Enforced on everything the Orrery writes into that modulator, after all of it**
(`orrery::enforce_ranges`, at the end of each phrase step and after a voyage starts). That covers:
- chord voicings and the bass (`write_phrase`);
- arp `pitch_walk` pools and the Arranger's octave lifts (`apply_octaves`);
- one-shot accents (`write_oneshots`);
- re-fitted phrases (`realized`);
- a track rested at its authored home.

`orrery_disable` / `context` modulators aren't written, so they're untouched.

**D3 — Skip, then fold (the user's choice).** The modulator's surface (step voicings, pool,
`realized` notes) first takes the whole-octave shift that leaves the fewest notes outside the range.
Ties go to no shift, then the smallest shift. An Arranger lift or pad placement that won't fit is
therefore undone with its shape kept, and one that fits stays. Any note still outside then folds by
octaves to its nearest instance inside. A range narrower than an octave keeps a missing pitch class
at its nearest instance.

## Consequences

- Horizon is now `orrery_role: "arp"` with `orrery_note_min: -12, orrery_note_max: 12`, plus
  `fit: "arp"` on its pool entry (ADR 0028 amendment) so its contour survives re-fitting.
- As an `arp` track it can be thinned by a narrative drop (ADR 0019) for a phrase at a tension peak.
  A range doesn't change that.
- `sme-lsp` shows each bound as a note (inlay and hover). The bounds are limits, not played pitches,
  so they never trip the out-of-scale lint.
- Docs without the keys are bit-identical; goldens unchanged.
- Tests: `orrery::tests::enforce_ranges_skips_a_lift_that_wont_fit_and_folds_strays`,
  `orrery::tests::fold_into_handles_open_and_narrow_ranges`,
  `loop_doc::tests::the_orrery_note_range_is_bounded_and_ordered`,
  `controller::tests::every_orrery_track_stays_inside_its_note_range` (chord, bass, arp and one-shot
  tracks of the roles fixture, 80 loops), `lsp::inlay::tests::the_orrery_note_range_reads_as_notes`.

## Amendment 2026-10-11 — the range takes precedence over the line as written

Branch `feat/arp-fit-note-range`.

**Context.** D2 bounds a re-fitted phrase only *after* the fit. But `fit: "arp"` (ADR 0028
amendment) already kept every note inside the entry's **written** range, so the written line's
extremes were the real limit. Widening `orrery_note_max` did nothing. `weighted-blanket`'s
`horizon` (F3..F5 written) needed room above F5 under triads with few tones there (C, Am). The user:
*"the Orrery puts a modulator's note min/max range at precedence over the line as written for a
pool sequence."*

**D4 — The fit sees the range** (`phrase_fit::refit(…, NoteRange)`, passed by
`select_and_refit_phrases`). Decided with the user:
- **`arp`**: the modulator's range replaces the written range when either key is set. **With
  only one key set, the other side is open** (no floor, or no ceiling). With neither, the written
  range is used as before.
- **`chord`**: a note that snaps past an edge moves to the nearest fitting tone inside the range: a
  chord tone for a strong note, a non-avoid scale tone for a weak one.
- **`root`**: the centred root, moved by the whole octaves that leave the fewest notes outside
  (`phrase_fit::octave_fit`: D3's rule — no move preferred, then the smallest). Any stray then
  folds by octaves. Besides the neighbouring octaves it tries the ones that align the line's lowest
  note with the floor and its highest with the ceiling. (Copilot review on PR 22: a ±1-octave search
  folded `[0, 7, 12]` into `100..=127` as `[103, 110, 103]` instead of `[103, 110, 115]`.)
- **A line wholly outside the range** (an `arp` or `chord` entry with no note inside it) first
  moves by that octave fit, so it keeps its contour instead of piling onto the edge. A line that
  overlaps the range keeps its written register. Moving it then would break the rules above: the
  author's register stands, and an open side is no bound, not room to relocate the line (a ceiling
  under the written top must not drop the whole arp an octave).
- **`literal`**: as written. D2's `enforce_ranges` still folds it, as it does every surface.

D2/D3 are unchanged and still run after everything. An Arranger lift that won't fit the range is
skipped, as before.

**D5 — An arp never re-strikes or drops a step** (the brief's other engine note: "`fit: "arp"`
drops steps"). The code had two causes:
1. When no new chord tone fit the range, `refit_arp` re-struck the previous pitch. The F3..F5 line
   peaked E5 E5 under C and Am, and with a long release that sounds like a missing step. Now that
   step takes a **tension over the chord** (`phrase_fit::tension_pcs`). That is a scale tone a 9th,
   11th or 13th above the chord root (intervals 1 2 3 5 6 8 9; not a 7th, which would recolour the
   chord). It is never a chord tone and never an avoid note (a semitone above a chord tone: F over
   Am, the 11th over a major triad). The nearest tension the way the line moves comes first, else
   the chord or tension tone nearest the written note. So under C the written-range peak is D5 (the
   9th) and under Am it is D5 (the 11th).
2. `select_and_refit_phrases` developed a seeded 40 % of phrase windows. A third of those
   **fragment** the tile, so its back half (an up-down arp's whole descent) went silent, and a third
   **rotate** the pitch row (the contour scrambles). A `fit: "arp"` entry is now never developed.

**Consequences.** Docs without the keys play every `chord`/`root`/`literal` entry exactly as
before. An `arp` entry's output changes only where it used to re-strike a step, and it is no longer
developed. No golden fixture plays a `fit: "arp"` entry or a note range, so goldens are unchanged.
Tests: `phrase_fit::tests::arp_fit_takes_the_modulator_range_over_the_written_one`,
`…::arp_fit_never_restrikes_a_step_it_takes_a_tension_instead`,
`…::chord_and_root_fits_honour_the_range`,
`orrery::tests::an_arp_entry_is_never_developed_and_climbs_to_the_modulator_range`.
