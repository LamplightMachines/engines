# ADR 0041 — The Orrery's pad register follows the chord track's authored octave

Status: accepted (2026-10-10, branch `feat/sme-live-redesign`)
Amends: ADR 0018 (the Arranger: "registers were hardcoded … `pad_center 7`"). Relates to: ADR 0014
(Orrery roles), `doc/decisions/orrery-generative-harmony-design.md`.

## Context

The Orrery voices every `chord`-role track as a close stack around a fixed register centre,
`HarmonicPreference::pad_center = 7` (G4 above a C root, C5 above an F root), ± 6 semitones
(`theory::voicelead::target_offsets`). The authored voicing only seeded the voice ids; its register
was ignored. The engine-facts note said "author the home there".

`weighted-blanket` (2026-10-10) authored its fog pad as a warm open add9, C3 F3 A3 G4 C5 (offsets
−17…7 from F4, mean −5.6). With `orrery_role: "chord"` the fog jumped up to F4–F5. The Arranger also
dropped it an octave whenever its notional arp descended (`arp_pos < pad_center + 5`), even though
the piece has no arp, so the pad hopped between F3–F4 and F4–F5. The user wanted the chord role kept
and the fog back in its own range.

Options put to the user: (a) an opt-in per-modulator mode that keeps the authored register *and*
spread (new schema field); (b) register only, close voicing, as an engine default (no schema change);
(c) a doc-only `shift: −12` stopgap. The user chose (b).

## Decision

**D1 — The pad register centre follows the authored octave.** When a voyage starts (and on every
live edit and control change that refreshes the preference), `pad_center` is
`7 + 12·round((mean − 7) / 12)`, where `mean` is the mean of the primary chord track's **authored**
voicing offsets (from the `HomeSnapshot`, so it is the doc's home, never a voyage write). Ties go
toward the authored side. No authored offsets (a missing or generator-driven chord track) ⇒ 7.
`orrery::pad_center_for`, `orrery::PAD_CENTER`.

The voicing stays a close stack (the user's choice). Only its octave moves. For the fog the centre
is −5 (C4): Fmaj7 becomes F3 A3 C4 E4 F4, inside the authored C3–C5 range.

**D2 — Whole octaves, not the exact mean.** Moving the default centre by whole octaves, rather than
centring on the authored mean, keeps every pad already authored in the default octave
**bit-identical**. The house pieces' halo (offsets −5 0 7 14, mean 4) keeps centre 7. Centring on 4
would have moved the V chord an octave down and broken re-creating the published `idle-night-v2`
from its doc snapshot (CE-R7.3). All golden render hashes are unchanged.

**D3 — The Arranger's pad drop is unchanged.** It already reads the real `pad_center`, so a low pad
sits below every arp position (`arp_pos ≥ 0 ≥ pad_center + 5` when the centre is −5) and is never
dropped. Suppressing the drop when no `arp` track is bound would also fix arp-less pads in the
default octave. But it changes the house pieces' output (their halo does drop today), so it is left
for a decision of its own.

## Consequences

- A chord track's register is now authored, at octave granularity: write the home voicing in the
  octave you want the Orrery to keep. A pad authored in the default octave behaves exactly as before.
- Moving a chord track's authored voicing by an octave in a live edit moves the voyage's register
  from the next phrase (`follow_edit` refreshes the preference).
- Tests: `orrery::gravity::tests::the_pad_register_follows_the_authored_octave` (the rule) and
  `controller::tests::the_pad_register_follows_the_chord_tracks_authored_octave` (a voyage on a pad
  authored an octave low stays in that octave for 40 phrases, borrowed chords included).
