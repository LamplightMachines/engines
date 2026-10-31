# ADR 0053 — sme-midi: a MIDI file as music-theory inspiration for a piece

> **Published trimmed.** This record also decides parts of tools that stay closed (a companion's MIDI file reader, `engines-midi`).
> Those sections are left out here; the decisions below are the ones the open code implements, word for word.
> It was written before the rename to Engines: `sme` is `engines` throughout (`sme-part` is `engines-part`, `sme_core::theory` is `engines_core::theory`).

Status: accepted — 2026-10-25, amended the same day (the top line).
Relates to:
- **ADR 0039: parts.** The tool's output joins a doc as parts, through `engines-part add --file`.
- **ADR 0028: pools, phrases and `fit`.** Motifs become pool cells for the phrase arm.
- **ADR 0014–0019: the Orrery.** It re-voices the chord role, which decides when a progression is heard.
- **CE-R7.3: determinism.** Nothing here changes engine output, and no golden hash moves.

## Context

A closed tool reads a MIDI file as music-theory inspiration and lays its material onto library voices. What it
needed from the open side is what's below: parts that carry key-exact offsets with a guard at merge, voice-leading
in the core's theory module, and two library parts (`bass`, `lead`) for the lines it writes.

## Decision (the open part)

**D9 — Overlay onto library voices.** `sme-midi parts` writes one complete part per layer by putting the file's
material in place of a library part's own sequence, so the sound stays house style:
- **Chords → `halo`:** voice-led on `v1..v4`, the Orrery's pad ids.
- **The bass → a new `bass` library part:** `orrery_role: "bass"`, id `b`.
- **Motifs → `pulse`'s pool.**
- **Comments:** the library part's comments are kept, through the CST.
- **Merging:** the parts merge with `sme-part add --file`.

**D10 — Key-exact offsets.**
- **Why not `$deg`:** parts carry raw semitone offsets from the doc's `harmony.root`. A `$deg` placeholder would
  quietly change a chord's quality in another mode.
- **The guard:** `parts` refuses a doc whose pitch set isn't the chosen key's. It records `part.key` in the part
  header, which `sme-part add` checks again at merge.
- **No re-centre tool:** there is no tool to re-centre a doc on another tonic of the same notes. The user doesn't
  need one.

From the amendment:

- **D9, amended — a `lead` part.** A soft sustained voice plays the top line: a triangle, legato, its brightness
  breathing (the engine has no pitch vibrato). Its phrase arm takes turns (`select: "cycle"`). `pulse` keeps the
  other line. The library grows to 9 parts.

Left out: D1–D8 (the reader itself: voices, tempo, the analysis, literalness, the parser) and the rest of the
amendment.
