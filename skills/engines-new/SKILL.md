---
name: engines-new
description: >-
  Start a new long-form generative piece (ambient, drone, sleep or focus music) from a short idea:
  interview the user until the voices are settled, then create pieces/<slug>/ with a brief and one
  annotated JSON5 loop doc built from the Engines parts library, check it, prove a short render, and
  hand it over to play and edit live in VS Code. Use when the user wants to start, begin or sketch a
  new piece, track, drone or song idea, or says "new piece" — also from an inspiration snippet or a
  MIDI file, when the Engines Studio skills are installed.
argument-hint: a short idea, e.g. "rain on glass, a sleep drone in D"
---

# New piece: idea → interview → `pieces/<slug>/` → live

<!-- engines:bin -->The `engines-*` commands are on PATH, or in `target/release/` of an Engines checkout.<!-- /engines:bin -->

## The tools

Run them in a terminal **from the workspace folder** (the one that holds `pieces/`): `engines-check`,
`engines-part`, `engines-song`, `engines-live` and `engines-clean`. The line above says where they
are: call them by name, with that folder in front when they aren't on PATH.
- **A command is missing** (not found by name, nor in that folder): stop and tell the user. Never
  build, download or search for the tools or their source. In VS Code the Engines extension provides
  them: reinstalling or reloading it brings them back.
- **An untrusted folder:** VS Code opens a new folder in Restricted Mode, and the Engines extension
  doesn't start until the folder is trusted, so there are no tools and no player. Ask the user to
  trust it (the Restricted Mode banner, or **Workspaces: Manage Workspace Trust**), then to ask again.

## Asking the user

Ask with your question tool if your agent has one; otherwise ask in chat, a few numbered questions at
a time. Put the option you recommend first and mark it **(Recommended)**, and always allow a free
answer. Don't ask what the idea already answers: infer it, then confirm it in the lock.

## The reference (in this folder)

- `reference/engine-facts.md`: params and their units, synth types, modulators, the matrix, buses,
  the Orrery, gain staging, songs. **Read it before writing the doc.**
- `reference/music-theory.md`: scales and their colours, the harmonic series, voicings.
- `reference/brief-template.md`: the piece's `brief.md`.
- `reference/skeleton.json5`: the starting doc (it passes `engines-check` as it is).

## Scope: the piece, not the engine

Work with what the build does: the doc, `brief.md`, the commands above and the player. Don't look for
the engine's code or change it. When a request needs something the build doesn't do, stop and report
it in a few lines: what was asked, what the build does instead, and the closest doc-only alternative.
Log it in `brief.md` → **Engine notes** (`<date> — <what's missing>`). If the `dev-issue` skill is
available, offer to file it as an Engine request issue with that skill.

## 1. Interview

Rounds, merged or skipped as the idea allows:

1. **Intent:** use (sleep, focus, background, meditation), rough length (it shapes the song later),
   mood, and the starting point: from scratch, or from an existing piece in `pieces/`. Offer the
   other starting points only when their skill is available:
   - a **house-style reference**, if the `house-style` skill is available (it lists them);
   - an **inspiration snippet** (a recording), if the `engines-listen` skill is available;
   - a **MIDI file's** key, chords and motifs, if the `engines-midi` skill is available.
2. **Key and time:** mood → mode, with a one-line colour each (aeolian dark and still · dorian
   wistful-warm · mixolydian centred, the overtone series' own mode · lydian bright, floating ·
   ionian plain · phrygian tense). The root, minding the register: a drone centre around A1–E2
   (55–82 Hz) sits well. Tempo and loop length (`master_steps × 60 / bpm`, usually 10–20 s).
3. **The levers:** density (sparse ↔ busy), brightness (dark ↔ airy), structure (predictable ↔
   messy: loop-synced patterns, or free-running odd-length sequences and slow incommensurate LFOs),
   and how much the Orrery (the generative harmony) may move: off, tamed or roaming.
4. **Voices** (several may be picked). These come ready-made from the parts library
   (`engines-part list`):
   - `drone`: a root drone.
   - `bass`: a soft low voice on the Orrery's bass role.
   - `halo`: a chord pad the Orrery re-voices.
   - `partials`: just-intonation harmonics over the root.
   - `pulse`: a plucked phrase.
   - `lead`: a soft sustained melody voice, its cells taking turns.
   - `binaural`: a hard left/right beat pair on the root, dry (headphones; sleep and focus).
   - `breath`: a slow LFO on another voice's cutoff.
   - `shimmer`: a shimmer reverb bus and a swell on another voice.

   Anything else (bells, noise, a `dual` FM "crank", a bass line) is hand-written from engine-facts.
   Ask per voice only what changes it: synth type (`subtractive`, `dual` FM/PM, `modal` resonators,
   `binaural` beat pair, `midi` to a DAW), register, how many voices, what drives it (a held step
   sequence, a euclid pattern, a sequence pool, a pitch walk), its modulators, its Orrery role
   (`context`, `chord`, `bass`, `arp`, `oneshot` or none) and its reverb send. A `binaural` voice also
   gets its beat (`spread_hz`: delta < 4 · theta 4–8 · alpha 8–13 · beta 13–30 · gamma ≥ 30) and its
   carrier (follow the note, or `root_hz` in real Hz).
5. **Lock:** show the voice sheet (a table: id · role · synth · register · driven by · routes and
   sends · Orrery · part or hand-written), the levers and the slug (kebab-case, e.g.
   `rain-on-glass`). Then ask: **Lock this (Recommended)** / Change something. Loop until it's
   locked. **Create nothing before the lock.**

**From an inspiration snippet:** if the `engines-listen` skill is available, follow it for the
measuring and mapping before round 2: the snippet's values go first in each question, marked "(from
the snippet)", and its parts replace step 2's layers. The lock stays the gate. Without that skill,
say that starting from a recording needs Engines Studio, and start from scratch.

**From a MIDI file:** if the `engines-midi` skill is available, follow it: it reads the file, asks
about the split, the key and what to take, and lays the material onto library voices. The lock stays
the gate. Without it, say that a MIDI file needs Engines Studio.

## 2. Build

```
pieces/<slug>/<slug>.json5   the one loop doc (the song is added to it later, in place)
pieces/<slug>/brief.md       from reference/brief-template.md
```

`pieces/<slug>/` is self-contained: never put piece files anywhere else.

1. **The skeleton:** copy `reference/skeleton.json5` to `pieces/<slug>/<slug>.json5`. Fill the header
   comment (the idea, the voice sheet, the loop math, the levers) and set `transport`, `harmony` and
   `movement` from the interview (Orrery off ⇒ `orrery: false`; tamed ⇒ `phrase_loops` 4–8,
   `tension_target` 0.10–0.15). Bind Orrery voices with `orrery_role` on their note modulators. Keep
   the `rv` bus: parts send into the doc's first bus. If the piece freezes its levels (balanced once,
   then held), add an empty `safe_bounds: {}` now, and each part brings its level frozen too.
2. **The parts,** one command each, in roster order. `breath` and `shimmer` act on another voice, so
   they come after it:
   ```sh
   engines-part add <slug> drone --id root
   engines-part add <slug> halo --id halo
   engines-part add <slug> breath --target halo
   engines-part add <slug> shimmer --target halo
   ```
   Each add merges every section the part needs (instrument, modulators, pools, routes, sends),
   resolves its pitches in the doc's mode, keeps the comments, checks the result and writes the doc
   once. It refuses a change that would add errors, and a doc that already has errors (fix those
   first). `--dry-run` shows the diff first; `engines-part show <part>` prints the part.
3. **Hand-written voices and tuning:** parts are quiet, key-agnostic starting points, so tune
   register, level, voicing, rates and Orrery role to the interview. Every param is in its unit under
   a suffixed key (`cutoff_hz`, `attack_s`, `level_db`, `sends_db`; silence is `-Infinity`), and a
   route's depth is in its destination's unit (`depth_oct`, `depth_db`, …). Free-form notes go in
   `meta` or `x_*` keys. Give every hand-written instrument a `sends_db` so it isn't dry.
4. **Gain staging** for what the controller plays (engine-facts → `global`): `master_gain_db` about
   −6 dB to start, voice levels balanced by role, levels frozen with `min == max` bands once they're
   right. There is no limiter in renders: headroom is the doc's job.

**Run `engines-check` after every edit of the doc.** Only this repository's Claude Code runs it for
you, from a hook.

## 3. Verify, before the user hears it

1. `engines-check pieces/<slug>/<slug>.json5 --parts` must say **0 errors**. Read every warning and
   fix it or say why it's fine. Its structure line ("repeats every loop" / "realigns every ≈ N
   loops") must match the structure lever: adjust sequence lengths or LFO rates if not.
2. `engines-song verify pieces/<slug>/<slug>.json5 --ab` renders 2 loops in memory (no file) and
   reports. It must show `all_finite=true`, a peak of about 0.4–0.6 FS, every route **audible** (a
   route at 0 % is wired but silent: a FAIL, exit 1) and no `WARN: engine lint`. When the doc has a
   centre (`meta.center_hz`, or a `context` voice's lowest pitch), it adds a harmonicity and pulse
   section: a CHECK there matters only for a centre drone. Adjust `global.master_gain_db` or voice
   levels and verify again if the peak is off.
3. With the Orrery on: `engines-song voyage pieces/<slug>/<slug>.json5 --loops <20 × phrase_loops>`
   runs it headless (nothing is rendered) and says TAMED or ROAMING. The Orrery moves once per phrase,
   so give it about 20 moves: a verdict on a handful is noise. A piece meant to be tamed must read
   TAMED; add `--movement default` for the worst case (a chord change every loop, so the default 20
   loops are enough).
4. Keep test renders short (a few minutes of audio at most). The long proof under the controller
   belongs to the song (`/engines-song`).

## 4. Hand it over

Add the first changelog line to `brief.md`. The player uses the speakers, so the user starts it:
- **VS Code with the Engines extension (Recommended):** open `pieces/<slug>/<slug>.json5` and press
  ▶ (the status bar or the editor title). The live panel opens.
- **A terminal:** `engines-live <slug>` plays it from the workspace folder, once the user says go.
  With no audio device (a test, a remote machine), `engines-live <slug> --headless` stands in.

Before starting a player, read `pieces/<slug>/.live/status.json`: a live `pid`, a `state` of
`playing` or `paused` and a fresh `updated_at` mean one is already playing. Never start a second.

Then tell the user, in a few lines:
- **Edit, then save (⌘S / Ctrl+S) to hear it.** Each accepted save is applied mid-loop about 0.2 s
  later. A save with errors is refused and the last good version keeps playing. Unsaved edits are
  marked `● unsaved`.
- **The panel:** transport, hold ⇄ drift (hold plays exactly what's written), mute and solo with
  meters, the Orrery's chord on a keyboard, revisions with revert.
- **In the editor:** completion and hovers that know the doc's scale, **Engines: Add a Part…**, solo
  and mute above each instrument.
- **Changes by request:** they can just ask ("darker", "thin out the pulse"): that's
  `/engines-edit`. The song comes later, with `/engines-song`.
