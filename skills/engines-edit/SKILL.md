---
name: engines-edit
description: >-
  Change a piece in pieces/<slug>/ while it plays live (the Engines VS Code extension's player, or
  engines-live in a terminal): check the editor has no unsaved edits, translate the request into the
  doc's levers (density, brightness, structure, mood), make the edits so they land as ONE revision
  (engines-part add for a new layer; .live/pause around several edits), check them strictly, confirm
  in .live/status.json that the new revision is playing (or why it was refused), and log the change in
  brief.md. Use when the user asks to change, tweak, adjust, add or remove something in a piece —
  "make it darker", "thin out the pulse", "more movement", "add a bell layer", "slow the LFOs" — or
  to fix a refused save.
argument-hint: the change, e.g. "darker, and thin out the pulse"
---

# Edit a live piece

<!-- engines:bin -->The `engines-*` commands are on PATH, or in `target/release/` of an Engines checkout.<!-- /engines:bin -->

The user edits the same file in their editor. **Re-read the doc right before editing**, change only
what the request needs, keep their comments and formatting, and never undo their edits. The engine
reference is in this folder: `reference/engine-facts.md` (params, units, what each one does per synth
type) and `reference/music-theory.md`.

## The tools

Run `engines-check`, `engines-part`, `engines-song` and `engines-live` in a terminal **from the
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

Work with what the build does: the doc, `brief.md`, the parts library (`engines-part`),
`engines-check`, `engines-song`'s checks and the player (`.live/status.json`, `.live/pause`). Don't
look for the engine's code or change it. When a request needs something the build doesn't do (a
param or key the checker rejects, an Orrery or player behaviour that isn't there, a bug), **stop and
report it** in a few lines: what was asked, what the build does instead, and the closest doc-only
alternative if there is one (ask before using it). Log it in `brief.md` → **Engine notes** (`<date> —
<what's missing or broken>`). Don't design or prototype an engine change. If the `dev-issue` skill is
available, offer to file it as an Engine request issue with that skill.

## 1. Read the state

- `pieces/<slug>/.live/status.json`:
  - It's live if `state` is `playing` or `paused` and `updated_at` is within a few seconds. Otherwise
    nothing is playing: offer to start it (it uses the speakers, so ask): ▶ in VS Code, or
    `engines-live <slug>` in a terminal. The edit can still be made while nothing plays.
  - **`editor.unsaved: true`** means the user's editor holds edits they haven't saved. Writing the
    file now would collide with them. Ask them to save (⌘S / Ctrl+S) or undo first, and wait until
    `status.json` says `unsaved: false`. With no `editor` key, no editor is attached.
  - `rev` / `applied_rev`, `rejected` (a refused save's errors), `warnings`, `chord`, `peak_dbfs`,
    `limiter_gr_db`, `optimize` (`hold` / `drift`).
  - `routing` (the player feeds an Ableton Live set, one pair per source): after an edit that adds a
    layer, its id shows in `routing.unrouted` until the set is rebuilt. Mention it, and if the
    `engines-ableton` skill is available, offer it for the new layer's own track.
- The doc, and `brief.md` (voice sheet, levers, changelog) for intent. A doc refused as `schema 0.8`
  predates native units: open it in VS Code and run the **Migrate to schema v0.9** code action on it
  (or ask the user to). Write v0.9 keys only (`cutoff_hz`, `level_db`, `depth_oct`, …).
- If the piece isn't obvious, list `pieces/` and ask.

## 2. Translate the request

Map words to levers. When one word could mean several moves, ask **one** question with the options as
musical outcomes (recommended first), e.g. "darker" → lower the halo cutoff (Recommended) · move to
aeolian · less shimmer · drop the bells.

| lever | the usual moves |
|---|---|
| density | euclid `pulses`, step count / `prob` ghosts, `polyphony` / voices, pool entries, removing or adding a route, a new layer from the parts library |
| brightness | `cutoff_hz` / `resonance`, `fm_index`, `shift_st` / `shift_mix`, wave (`sine` → `triangle` → `saw_soft`), modal `brightness`, `band_hi_hz`, reverb `shimmer` / `damping` |
| structure | `sync: "free"` + odd `length`s (5/7/11/13/23) for polymeter, LFO rates (incommensurate ⇒ long repeats), `prob`, `movement.phrase_loops` / `tension_target` |
| mood | `harmony.mode` / `root`, the home voicing, `movement.orrery` on/off, `tension_target` |

**"Make X sound more like the snippet's Y"** (the piece has an `inspiration/` folder): if the
`engines-listen` skill is available, follow it (its rounds, the user's A/B verdict, the accepted diff
landing as one revision as in step 3). Without it, say that matching a recording needs Engines Studio.

A **new layer** that matches a part (`engines-part list`: `binaural`, `drone`, `bass`, `partials`,
`halo`, `pulse`, `lead`, `breath`, `shimmer`) comes from the library rather than being hand-written
across five sections: `engines-part add <slug> <part> --id <id> [--target <instrument>] [--dry-run]`.
Tune it afterwards (parts are quiet and key-agnostic). It refuses a doc that already has errors (a
refused save on disk): fix those first.

Structure edits: after checking, quote the new polymeter line ("realigns every ≈ N loops (≈ m:ss)") so
the user sees what changed. A root or mode change, or adding or removing a chord or bass modulator,
starts the Orrery's voyage again from home; any other edit leaves it wandering. Say so when it
matters.

## 3. Edit as one revision

Every write of the doc is a new revision that the player applies. One small change = one write.
`engines-part add` is one write, however many sections it touches. For anything with several steps:
1. create the empty file `pieces/<slug>/.live/pause` (the player holds the file's saves back;
   `status.json` says `paused`);
2. make the edits (intermediate states may not check: finish the batch);
3. `engines-check pieces/<slug>/<slug>.json5 --parts` → **0 errors** (run it after every edit: only
   this repository's Claude Code runs it for you, from a hook);
4. delete `pieces/<slug>/.live/pause`: the whole batch lands as one revision.

Never leave `.live/pause` behind. (It holds back the file, not the user's own saves from VS Code,
which reach the player directly.) The editor reloads the open doc after your write, so the user sees
the change. The history records where each revision came from (`.live/history/index.jsonl`
`source`): your writes are `disk`, the user's saves `editor`, a revert `revert`.

## 4. Confirm it's playing

Read `status.json` every ~0.25 s, for up to ~3 s, until `applied_rev` equals the new `rev`. The
player reads a write once the file has been still for 300 ms, then applies it mid-loop about 0.2 s
later. Report "r12 playing from loop 340" plus anything notable (the chord, a peak or limiter reading
if levels moved). If `rejected` appears instead, read its errors, fix the doc, and confirm again. You
can't hear it: if the change should be audible, say so. When audibility is in doubt, offer
`engines-song verify pieces/<slug>/<slug>.json5 --ab` (every route's share of the mix, rendered in
memory; 0 % = wired but silent).

**Fine-tuning a sound?** Drift keeps moving timbre levers around their written values. Suggest
**hold** (the panel's hold ⇄ drift toggle, or `optimize: "hold"` in the doc) while the user dials
something in, so they hear exactly what's written; back to drift when it's right.

## 5. Log it

Append to `brief.md` → Changelog: `<date> — r<N> — <what changed, in musical terms, and why>`. Update
the voice sheet or levers table when the roster or a lever changed.
