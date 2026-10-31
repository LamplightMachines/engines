# ADR 0043 — A song renders one file per block (`sme-song`)

Status: accepted (built 2026-10-20)
Relates to: ADR 0029 (songs, `sme-song`, the streaming writers), 0030 (stems: one gain per stem),
0034 (pieces: `renders/<date>_r<rev>` + sidecar), 0021 (no limiter); CE-R7.3 determinism.

## Context

A finished song goes into Ableton, where it gets arranged, layered and mastered. A 5 h song as one
file per stem is awkward to work with there. The author wants each block of the song (settle, idle,
deep, …) as its own file, for the mix and for every stem, lined up **in sequence on a track**.
Doing that must leave **no audible artifact at a join**: no click, gap, overlap or level jump.

## Decision

**D1 — A song always renders one file per block, plus one for the tail.** Mix and stems alike, with
no flag needed. `--whole` writes one continuous file (or one per stem) instead, for the whole-song
proofs (`song_seams.py` reads one WAV). A song-less doc renders as before.

**D2 — Names.** `<song>-<part>-<NN>-<label>.<ext>`:
- `<song>` is the piece slug, or the doc's file name.
- `<part>` is `mix`, an instrument id, or `bus_<id>` (the ADR 0030 stem names).
- `NN` is the block's index as the `--dry-run` plan, `--loop-block K`, the CodeLens and
  `--emit-blocks` number it, zero-padded. The tail is numbered after the last block.
- `<label>` is the block's label (else its layer name, else `base`); the tail's is `tail`.

All parts are sanitized like stem names. Ids and labels may contain `-` and digits, so two
(stem, block) pairs can spell the same name: `pad` with block 0 `01-deep` and `pad-00` with block 1
`deep` are both `<song>-pad-00-01-deep`. A stems render whose names clash (case-insensitively) is
refused before anything renders, naming both pairs; rename one of them. (A mix can't clash: its
part is fixed and its block numbers are fixed-width.)

**D3 — Where the files go.**
- Stems go in the `--stems DIR` as before.
- For a song, the mix's `--out` names a **directory**. An `.flac`/`.wav` extension on it picks the
  container and is dropped, so `--out renders/night.flac` writes `renders/night/…flac` and existing
  invocations keep working.
- With no `--out` or `--container`, the default is FLAC, as for stems.
- A piece's song lands in `renders/<date>_r<rev>/`, with the sidecar `<date>_r<rev>.json` beside it
  (`kind: "mix"`, `manifest: "blocks.json"`, `files: N`). That is the shape `sme-clean` already prunes
  for stems directories.

**D4 — The cuts are sample-exact, so the joins are seamless by construction.**
- The session renders the song exactly as before: one engine, one continuous stream. Block starts
  fall on loop boundaries (`SongPlan::segments()`; `start_loop × round(loop_s·sr)` frames).
- A writer (`blocks::SplitOut`) switches files at exactly those frames. Every frame lands in exactly
  one file, with no fade, padding or overlap.
- A reverb tail or a seam glide crossing a block start simply continues into the next file.
- The s16 TPDF dither is keyed by stream position. Each file continues the count
  (`WavWriter`/`FlacWriter::continuing_at`), so **the files laid end to end decode bit-for-bit to
  the `--whole` render**. `tests/sme_song_blocks.rs` pins that for s16 WAV, s24 FLAC and the stems.

**D5 — Stems: one gain per stem, measured over the whole song.**
- The float temps and the BS.1770 measurement are unchanged (ADR 0030), so the gain each stem gets is
  the one the `--whole` render would give it.
- The post-pass writes it into every block file of that stem. Level never steps at a join, a quiet
  block stays quiet, and `mix_fader_db` still rebuilds the mix.
- A block where a stem is digitally silent still gets its (silent) file. That keeps a stem's files a
  complete, gap-free sequence: dropped in order on one track, nothing shifts.
- A stem silent for the whole song is still skipped.

**D6 — Manifests.**
- `stems.json` gives each stem a `files` list (file, block, label, `start_frame`, frames, peak,
  clipped, bytes) and the song's `blocks` table, replacing a single `file`.
- A per-block mix directory carries `blocks.json`: every file and its start, plus the master's peak,
  clips and LUFS.
- A re-render into the same directory removes the files the previous manifest recorded that this one
  didn't rewrite (`stems::remove_stale_files`), so a stale `…-05-tail` from a longer earlier render
  can't end up lined up with the new song.

**D7 — A render cut short** (`--max-loops`) writes the blocks it reached. The last of them is short,
and later blocks get no file.

## Consequences

- No engine or render-path change: the goldens and the song-less stems golden are untouched. A song
  render's bytes per frame are the same, only spread over more files.
- Verification renders of a song (`scale_song.py` → `song_seams.py`) need `--whole`; the skills say so.
- In the DAW: keep the project at the render's sample rate, turn warping off for these clips, and turn
  off any automatic clip-edge fades (Ableton's "Create Fades on Clip Edges"). The files carry no fades
  of their own, and a fade added at a join would be the only discontinuity.
