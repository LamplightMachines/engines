# ADR 0055 — Per-block control in songs: the Orrery off, drift overrides and movement retunes

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** D1 was written for schema v0.8, a day
> before the break. A block's `drift` keys are v0.9 paths and its `{ min, max }` bands are in the param's unit, so
> D1's example reads `"instruments.pad.params.cutoff_hz": "hold"`, `"instruments.drone.params.drive_db": { min: 8.94,
> max: 13.26 }` and `"instruments.pad.params.release_s": { max_drift: 0.02 }`. `max_drift` and `max_change` stay
> fractions of the param's range. `sme-migrate` converts a block's bands the way it converts `constraints`.

Status: accepted — 2026-10-27 (the spec's recommendations, approved).
**Amends ADR 0029 D2**: a block may now carry the session control as well as the matrix.
Relates to:
- **ADR 0029: songs.** D4 already rebuilds the controller at every block start. The only thing a block couldn't do
  was hand it a different control.
- **ADR 0033: control in the doc.** A block's `control` uses the doc's own control vocabulary (`optimize`,
  `movement`, `max_drift`, `max_change`, `constraints`).
- **ADR 0025 / 0032: drift caps and the reflecting walk.** Drift's algorithm is unchanged. A block only changes
  the bands and caps drift is given.
- **ADR 0014–0019: the Orrery and the Arranger.** Their behaviour with the Orrery switched off is unchanged.
- **ADR 0036: the `sme-live` protocol.** The song structure it reports gains each block's Orrery state.
- **ADR 0054: native units (v0.9).** `sme-migrate` converts a block's drift bands like it converts `constraints`.
- **CE-R7.3: determinism.** A song without `control` renders exactly as before, so no golden hash moves.

## Context

Song blocks were matrix-only (ADR 0029 D2). A block couldn't turn the Orrery off, retune it, or stop drift on one
param. The owner asked for two things in particular: **turn the Orrery off for specific blocks**, and **override
drift for specific instrument params, by key, in a block**.

The controller could already do both. `Controller::begin_block` rebuilds it with the standing `SessionControl` at
every block start. `set_control` handles an Orrery that is off. Drift already has per-param levers: a
`constraints` band keyed by JSONPath replaces the `±max_drift` band, and `min == max` pins a param. What was
missing was a way for a block to say which control to use.

## Decisions

**D1 — A block may carry `control`, additive at schema v0.8.** It is a merge patch over the session's control and
uses the doc's own control keys:

```json5
{ matrix: "deep", loops: 24, label: "deep",
  control: {
    movement: { orrery: false },          // or any movement field: { phrase_loops: 8, tension_target: 0.1 }
    optimize: "hold",                     // hold | drift for this block
    max_drift: 0.05,                      // the block's cumulative drift cap (normalized)
    max_change: 0.02,                     // caps the block's per-loop step (normalized)
    drift: {                              // per param, keyed by the canonical JSONPath
      "instruments.pad.params.cutoff": "hold",                   // pinned at its authored value
      "instruments.drone.params.drive": { min: 0.2, max: 0.4 },  // its own band, native units
      "instruments.pad.params.release": { max_drift: 0.02 },     // its own cap, normalized
    } } }
```

- **`movement`** patches the session's movement field by field (RFC 7386, like a matrix layer). `orrery: false`
  turns the Orrery off for the block.
- **`optimize`** is `hold` or `drift`. `live_edit` is refused, because a block isn't hand-edited.
- **`max_drift`** replaces the doc's `max_drift` in the block. It is written into the block's doc, which is where
  the controller reads it.
- **`max_change`** caps the per-loop step. It can only make drift gentler: the session's step (`--max-change`, else
  the doc's) stays the ceiling. A block that wants more movement raises the doc's `max_change` and caps the other
  blocks.
- **`drift`** entries become `constraints` bands, keyed by path. A block's band replaces a session band on the
  same key:
  - **`"hold"`** is `min == max` at the param's authored value, after the Tier-2 safe clamp. A band of zero width
    pins the param.
  - **`{min, max}`** is a band in native units, used as given.
  - **`{max_drift: d}`** is the authored value ±`d` in normalized space, converted to native units.

**D2 — The control is block-scoped.** A block without `control` gets the session control back. Nothing carries
over to the next block.

**D3 — Precedence: the block wins at its start; the live panel's switches win until the next block.** The
standing control is what the session started with: the doc's control with the CLI flags layered on, or an
`--objectives` file. A control edit (the player's, a watched objectives file) replaces it. At a block start the
block's patch is applied over the standing control, so the block beats a CLI flag, and the block keeps its patch
through any whole-control edit.

The live panel's hold ⇄ drift and Orrery switches are different: the player names the switch a command flipped
(`Session::apply_live_control`, a `LiveToggle`), and that switch beats the block's patch until the next block
starts. The switches the command didn't name keep the block's value.

This refines the spec's decision 4 ("the toggle wins until the next block start"). The player sends whole
controls, so taken literally, a hold ⇄ drift toggle in an Orrery-off block would also turn the Orrery back on.
Because the player's control includes the panel's settings, a panel setting carries into later blocks, and a block
that sets the same field beats it from that block's start.

**D4 — Orrery off in a block is the same as switching it off.** The block's fresh controller never starts a
voyage. The Orrery-bound voices play their authored home material: no chord changes, no Arranger moves. The seam
in and out is ADR 0029's glide. A later block with the Orrery on starts a fresh voyage from home, and the phrase
clock restarts, so phrase boundaries line up with that block's start. This was already the behaviour for every
block start.

**D5 — A single-block audition plays the block's control.** `materialize` writes a block's control into the
block doc's own control settings (ADR 0033: `optimize`, `movement`, `constraints`, `max_drift`, `max_change`).
So `sme-live --loop-block K`, `sme-song --emit-blocks` and anything else that plays one materialized block hear
it, through `SessionControl::from_doc`. In that case the CLI flags layer over the block, as they do over any doc.
In a song, the controller strips those fields (ADR 0033) and takes the block's control from the song driver (D3).

**D6 — Checked where it's authored.** The schema gives `control` its keys. `song::validate` names the block and
the key at fault for these errors:
- a `drift` key that isn't a drift candidate: a typo, a non-numeric leaf, or an instrument with `dither_disable`;
- a band outside the param's range or its Tier-2 `safe_bounds`;
- `min > max`;
- a non-positive `max_drift` or `max_change`;
- `optimize: "live_edit"`.

It warns about:
- Orrery-only fields next to `orrery: false`, because they do nothing;
- `orrery: false` (or a retune) in a doc whose Orrery is never on;
- a drift override in a block that holds.

**D7 — The block's control is visible.** `sme-song --dry-run` prints each block's control under its row. The
loop log already records what each loop played (`optimize`, and the Orrery state, absent while it's off). The
`sme-live` song structure (ADR 0036) carries each block's `orrery` (true, false, or absent when the block leaves
the session's), and each `loop` event says whether the Orrery ran. So the panel's Orrery switch shows what plays,
and its song rows tag the blocks that switch the Orrery.

## Consequences

- A song with no `control` anywhere renders bit-identically, so the golden hashes don't move. A one-block song
  with `control` equals a plain session started with that control (pinned by test).
- ADR 0029 D2 now reads "matrix and control". Instruments, modulators, harmony, transport and globals are still
  fixed for the whole song, and one engine still runs it.
- Out of scope, as the spec says: a per-block key or mode (Phase B, its own issue), per-block tempo (dropped for
  now), and wildcard drift keys. Use `optimize: "hold"`, or `dither_disable` on the instrument, to hold everything.

## Alternatives rejected

- **Flat block fields** (`orrery: false`, `drift: {…}` directly on the block). That would be a second vocabulary
  next to the doc's control keys. One `control` object mirrors the doc.
- **A new per-param `max_drift` table inside the controller.** A native `constraints` band around the authored
  value does the same job with the machinery drift already has (D1).
- **The block always wins over a live toggle.** Auditioning a block in VS Code would then ignore the panel's
  hold ⇄ drift and Orrery buttons.
