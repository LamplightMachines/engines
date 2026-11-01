---
name: engines-song
description: >-
  Turn a piece in pieces/<slug>/ into a finite long-form song (a sequence of blocks that change the
  matrix and, optionally, the control, then a ring-out tail) after an interview — length, use, arc,
  which voices play in which block, Orrery and drift per block, seams, tail, master or stems — then add
  the `song` to the SAME doc, verify it with engines-song's checks (the plan, every block's start, the
  whole form scaled down with drift and the Orrery), let the user audition blocks live and, once they
  confirm, render the FLAC or stems into the piece's renders/. Use when the user wants a piece as a
  song, a 1-hour nap or all-night sleep track, an arc, blocks, sections, or the final render.
argument-hint: the length and use, e.g. "a 1-hour nap"
---

# Piece → song

<!-- engines:bin -->The `engines-*` commands are on PATH, or in `target/release/` of an Engines checkout.<!-- /engines:bin -->

The song mechanics are in this folder's `reference/engine-facts.md` → **Songs** (the shape, seams,
the tail, the constant-offset idiom, gain bookkeeping, `engines-song`'s flags): read it before
writing. This skill is the interview, the design and the checks around it. (`engines-song` the
command renders and checks; `/engines-song` is this skill.)

## The tools

Run `engines-check`, `engines-song`, `engines-live` and `engines-clean` in a terminal **from the
workspace folder** (the one that holds `pieces/`). The line above says where they are: call them by
name, with that folder in front when they aren't on PATH.
- **A command is missing:** stop and tell the user. Never build, download or search for the tools.
  In VS Code the Engines extension provides them: reinstalling or reloading it brings them back.
- **An untrusted folder:** the Engines extension doesn't start in VS Code's Restricted Mode, so there
  are no tools and no player. Ask the user to trust the folder (**Workspaces: Manage Workspace
  Trust**), then to ask again.

## Asking the user

Ask with your question tool if your agent has one; otherwise ask in chat, a few numbered questions at
a time. Put the option you recommend first and mark it **(Recommended)**, and always allow a free
answer.

## Scope: the piece, not the engine

Work with what the build does: the doc, `brief.md`, the commands above and the player. Don't look for
the engine's code or change it. When a request needs something the build doesn't do, stop and report
it in a few lines (what was asked, what the build does instead, the closest doc-only alternative) and
log it in `brief.md` → **Engine notes**. If the `dev-issue` skill is available, offer to file it as an
Engine request issue with that skill.

## 1. Read

The doc, `brief.md` (voice sheet, levers, changelog) and `.live/status.json`: if `editor.unsaved` is
true, ask the user to save or undo before you write the doc. Note the loop length
(`master_steps × 60 / bpm`) and every route id in `matrix`: blocks can only re-wire those.

## 2. Interview

1. **Length and use:** 1 h nap · 3 h · 5–6 h all-night · custom; sleep, focus or background.
2. **Arc:** if an `inspiration/*/feel.md` carries an arc sketch, offer it first, marked "(from the
   snippet)". Otherwise e.g. settle → idle → deep → surface (sleep), arrive → work → work → release
   (focus), or the user's own. Rough proportions per block.
3. **Voice × block grid:** per block, which layers sound (several of the voice sheet's ids) and any
   level trims ("the pulse 6 dB down in *deep*"). Show the grid as a table and confirm it.
4. **Orrery per block** (only when the piece's Orrery is on): a fresh voyage, a recap of an earlier
   block (the same explicit `seed` on both), retuned (`control.movement`: slower `phrase_loops`,
   lower `tension_target`), or off (`control: { movement: { orrery: false } }`, the home chord for the
   whole block — a deep sleep block's usual pick).
5. **Drift per block:** as the doc says; calmer (`control.max_drift`); held (`optimize: "hold"`); or
   params pinned or banded by key (`control.drift: { "instruments.pad.params.cutoff_hz": "hold" }`, or
   `{ min, max }` in native units).
6. **Seams and tail:** glide length (8–15 s typical, at most the shortest block); tail loops for the
   reverb to ring out (1–3).
7. **Deliverable:** a master FLAC (24-bit) · stems (−18 LUFS per instrument and bus) · both.

## 3. Design: what a block can change

A block can add, remove or patch routes (depths, sources, destinations) and carry a `control` (the
Orrery off or retuned, hold or drift, drift caps, per-param drift overrides): nothing else. No param
value, LFO rate, harmony, tempo, instrument or pool change. For each request the grid can't express,
propose the base-doc addition that makes it a matrix change (a `unity` source plus a trim route
present only in that block; a second LFO routed only in *deep*; a route a block switches on) and
confirm it before adding it. Say plainly what can't be done.

- **One idea per seam.** Every seam is an event, and in a sleep track an event is a disturbance.
  Count ideas, not routes: a removal and its level trim are one move. A seam carrying two ideas wants
  a block between them.
- **Loop math in the comments:** `loops = round(minutes × 60 / loop_s)`, each block's start–end time
  as a comment, the total `(Σ loops + tail_loops) × loop_s` in the header.
- **Seams that glide:** reuse one route id per purpose across layers (`m_trim` in every block that
  needs it) so neighbouring blocks glide its depth. A held voice that loses its note route plays out
  on its own `release_s`: give held voices 2–3 s, never ~0 (a click).
- **Gain-staged per block** (no limiter in renders, and every block is a fresh controller start):
  offsets go through the constant-offset idiom (engine-facts), every removal of a route into a gain is
  booked (`−depth_db × mean(source)` dB), a trim that must stay exact gets a zero-width `safe_bounds`
  band on its depth key. Targets: every block's start ≤ 0.6 FS, never louder under the controller
  than raw, neighbouring blocks within ~1–2 dB for sleep.

## 4. Write

Into `pieces/<slug>/<slug>.json5` itself: the additions from step 3, then `song`: `matrices` (layers
holding only what differs), `sequence` with `label`s, `seed`s for recaps and a start-time comment per
block, `transition_s`, `tail_loops`, and a header table (block · loops · start · what changes · gain
rationale). Batch it as one revision: create `pieces/<slug>/.live/pause`, write, run
`engines-check pieces/<slug>/<slug>.json5` (after every edit: only this repository's Claude Code runs
it for you, from a hook), then delete `.live/pause`. The doc keeps playing as a plain loop (its
top-level matrix) until someone auditions the song.

## 5. Verify

Every check renders in memory and writes no file. Exit code: 0 a pass (with or without warnings), 1 a
FAIL verdict, 2 the check couldn't run.
1. `engines-check pieces/<slug>/<slug>.json5` → 0 errors.
2. `engines-song --piece <slug> --dry-run`: no `warning: song:` lint; the starts and lengths match the
   comments; the routes column shows only the intended changes; recaps share a seed.
3. `engines-song verify pieces/<slug>/<slug>.json5`: its block table plays every block raw and from
   the controller's start. Fix every FAIL (louder under the controller ⇒ a value under its
   `safe_bounds` floor; over 0.7 FS ⇒ lower `master_gain_db` inside its band or trim the hot block),
   a WARN over 0.6 FS, and every level step you didn't intend. To prove a block's new route reaches
   the audio: `engines-song verify pieces/<slug>/<slug>.json5 --block K --ab` (a trim must not read
   0 %).
4. `engines-song seams pieces/<slug>/<slug>.json5`: the whole form scaled down (every block, the
   longest 12 loops, seams and tail kept), rendered with drift and the Orrery. It must say **CLEAN**:
   0 clipped, each seam smooth (a LOUDNESS or TIMBRE lurch ⇒ lengthen `transition_s` or trim the
   step; a CLICK ⇒ more `release_s`), the tail reaching −60 dBFS in time (else raise `tail_loops`).
5. With the Orrery on: `engines-song voyage pieces/<slug>/<slug>.json5 --full` runs the song through
   the Orrery at its real length (nothing is rendered, so it takes a moment): one line per block at
   the end (each block restarts the voyage from home), recaps named. Scaled down (without `--full`),
   a block holds only a few phrases, and a verdict on a few moves is noise.
6. Update `brief.md` → **Song plan** (the block table) and the changelog.

## 6. Audition (the user listens; you can't)

Ask which blocks to hear (the seams in doubt first, e.g. "idle → deep", and the tail), then point the
way:
- **VS Code:** the CodeLens above each block in `song.sequence`: **▶ loop block K** loops that block
  as a plain loop; **play from here** plays the song from that block to its end, seams and tail
  included. The panel's Song section does the same and highlights the playing block.
- **A terminal**, only when nothing else plays the piece: `engines-live <slug> --loop-block K` or
  `engines-live <slug> --play-from K`.

While a block loops, saves re-materialize it (tune it live: a trim, a route's depth). While the song
plays, saves wait for the return to the loop. Take the user's notes back into the doc as in step 4,
and re-run step 5's checks for anything that changed a block.

## 7. Render (ask first)

Quote the dry run's length, size estimate and rough time (≈ 6–7× realtime), then ask: **Render now
(Recommended)** / Later. Run it in the background:
- the master: `engines-song --piece <slug> --orrery`;
- stems: `engines-song --piece <slug> --stems --orrery`.

A song renders **one file per block**: `renders/<date>_r<rev>/<slug>-mix-<NN>-<label>.flac` (stems:
`renders/<date>_r<rev>_stems/<slug>-<stem>-<NN>-<label>.flac`), the tail last, with a
`<date>_r<rev>.json` sidecar (revision, seed, control, peak, LUFS) beside the folder. The cuts are
sample-exact: laid end to end on one track, the files are the song bit for bit. `--whole` renders one
continuous file instead. After the full render, `engines-song seams pieces/<slug>/<slug>.json5 --full`
re-checks the seams at the real length (in memory, as long as the render itself).

Report the `done:` line (peak, LUFS, **0 clipped**) and add the render to `brief.md` → Renders. If
the `engines-ableton` skill is available and the user rendered stems, end your report with a question
of its own, not a passing mention: **Build the Live set with `/engines-ableton` (Recommended)** / Not
now. It lays the stems into the piece's Ableton Live set sample-exactly. `engines-clean <slug>` lists old renders that could go
(`--yes` deletes them; published and Ableton-linked renders are kept). When the user publishes a
render, set `"published": true` in its sidecar so `engines-clean` keeps it.
