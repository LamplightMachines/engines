# Engines — a composing instrument for long-form generative tracks

A deterministic Rust engine that plays a looped **loop document** — synths, modulators, a
modulation matrix, a shimmer send bus, a drift controller and the **Orrery** (generative harmony)
— and renders hours-long **songs** as a master plus loudness-normalized stems for a DAW. Built for
ambient / drone / sleep / focus tracks. You compose in VS Code, hearing each save mid-loop, with
Claude doing the interviews, the edits you ask for and the song arrangement.

Engines is free software under the GPL-3.0-or-later ([`LICENSING.md`](LICENSING.md)). To build it, test it
or contribute, see [`CONTRIBUTING.md`](CONTRIBUTING.md). The design decisions are in
[`doc/decisions/`](doc/decisions/); [`0031`](doc/decisions/0031-composition-rescope-and-learner-removal.md)
sets the direction (the project began as a learning proof of concept steered by an audio-text model; that
learner was removed).

*A demo (a render and a short video) comes with the first release.*

## Quick start

```sh
make test      # the Rust suite (engine, controller, songs, golden renders) + the Python lab
make vscode    # build engines-lsp + engines-live and install the VS Code extension (needs npm); reload VS Code
```

Then, in Claude Code in this repo:
1. **"New piece: a slow dorian drone for focus, with a soft pulse."** Claude interviews you with
   question cards, then writes `pieces/<slug>/<slug>.json5` and checks and proves it.
2. **Open the doc in VS Code and press ▶** (status bar or editor title). Edit, then **save (⌘S)**:
   the player applies each save mid-loop, about 0.2 s later.
3. **Ask Claude for changes** ("darker", "thin the pulse", "add a bell layer"). They play the same
   way.
4. **"Make it a 1-hour nap."** Claude arranges it as a song, you audition blocks in the editor, and
   it renders the song into `pieces/<slug>/renders/` as a FLAC per block (or stems, also per block).

Without VS Code, `make live PIECE=<slug>` plays a piece from the terminal and applies every save.

## Composing a piece (the workflow, ADR 0031/0034)

1. **Kick off** — the `/engines-new` skill interviews you until the voice roster is exact (layers,
   synth types, registers, what drives each, their Orrery roles), then writes
   `pieces/<slug>/<slug>.json5` (a plain annotated doc, starting from its skeleton, with the
   standard layers added from the parts library) and `brief.md`. It checks the doc, proves a short
   render and hands it to you.
2. **Edit live in VS Code** — piece docs open as the `engines-loop` language. ▶ starts `engines-live`, and
   each **save** is checked, snapshotted into `.live/history/` and applied mid-loop. A save with
   errors is refused, the panel says why, and the last good revision keeps playing. Unsaved edits are
   marked `● unsaved`. (Set `engines.live.applyOn: "edit"` to hear the buffer as you type instead.) The
   live panel has:
   - transport, and **hold ⇄ drift**: hold plays exactly what's written, for fine-tuning;
   - mute/solo with meters;
   - the Orrery's chord on a keyboard spelled in the doc's key;
   - song blocks to loop or play from;
   - revisions with diff and revert.

   A protection limiter guards the speakers (live only, never in renders).
3. **Ask Claude for changes** (`/engines-edit`). Claude first checks that you have no unsaved edits
   (they'd collide with its write), then makes the change as one revision and confirms it's playing.
   A hook checks every doc edit Claude makes. A new layer comes from the parts library in one step.
4. **Make it a song** (`/engines-song`) — an interview about length, arc and which voices play where.
   Claude adds the `song` to the same doc and verifies seams and gain (`engines-song verify`, `seams`). You audition blocks (the
   CodeLens above each block: **▶ loop block** / **play from here**). Then it renders
   `renders/<date>_r<rev>/`, a FLAC per block (or stems), with a sidecar that records exactly how to
   re-create it.
   `make prune PIECE=<slug>` tidies old renders, never touching published or protected ones.
5. **Finish in your DAW** — the stems line up end to end, block by block, and `make live` can play each
   source on its own stereo pair of a multichannel loopback (`DEVICE="BlackHole 64ch" ROUTE=1`), one DAW
   track per pair.

The Orrery keeps wandering through your edits. Its voyage starts again from home only when you change
the key or restructure the chord/bass modulators.

The three piece skills live in `skills/` and run in any agent with only the bundled `engines-*`
commands: Claude Code here (through links in `.claude/skills/`), or Copilot and Claude in VS Code with
the Engines extension (`skills/README.md`).

## Authoring a loop document

A **loop document** (loop doc) is the JSON5 the engine plays. The engine loops a **control
timeline** — the schedule of note events and modulation — while the synth voices run
*continuously*, so envelope tails, filter state and reverb cross the loop seam (this is what makes
long generative drones sound seamless). It's deterministic: the same doc, seed and control timeline
render bit-identical audio, so a render can always be re-created from its doc snapshot.

**References.**
- The authoritative schema is the annotated **v0.9** contract
  [doc/loop_contract_v0.9.json5](doc/loop_contract_v0.9.json5). It plays as-is, and `make templates`
  copies it out as a starting template. It's machine-validatable as
  [contracts/loop_contract_v0.9.schema.json](contracts/loop_contract_v0.9.schema.json). A v0.8 doc is
  refused with a message naming `engines-migrate`, which converts it (ADR 0054).
- A drone in the house style, annotated:
  [lab/tests/fixtures/loop_drone_d_lydian.json5](lab/tests/fixtures/loop_drone_d_lydian.json5).
- Docs are JSON5 (ADR 0023): `//` comments and trailing commas are fine, and comments are the point.

### Where to start

- **A new piece**: ask Claude (`/engines-new`); it builds the doc with you, from an interview and the
  parts below.
- **A layer at a time**: the **parts library** (ADR 0039) adds a whole idea in one step: its
  instrument, modulators, routes and sends, gain-staged and pitched in the doc's own mode, comments
  kept, a unique id, checked. The parts:
  - **drone** — a low held saw on the root;
  - **bass** — a soft low voice on the root, the Orrery's bass role;
  - **lead** — a soft sustained voice for a melody, its phrase cells taking turns;
  - **binaural** — a held sine pair on the root, hard left and right, beating at 4 Hz (dry; headphones);
  - **partials** — just-tuned harmonics 3, 4 and 6 over it;
  - **halo** — a slow detuned pad the Orrery re-voices;
  - **pulse** — a plucked modal pulse with its phrase cell;
  - **breath** — a slow LFO on an instrument's filter;
  - **shimmer** — a shimmer reverb bus with a slow swell.

  Use `Engines: Add a Part…` in the editor or `make part PIECE=<slug> PART=<name>` (`engines-part add <doc|slug>
  <part> [--id] [--target] [--dry-run]`). Parts send into the doc's first bus, so author the reverb
  bus first. A doc with a `safe_bounds` section gets each part's level frozen too.
- **By hand**: from the minimal loop below, or the template.

### Help while you write

- **Strict checking** (ADR 0033). `make check DOC=your.json5` (`engines-check <doc> [--parts] [--json]`)
  checks the doc exactly as the engine loads it, and the engine refuses a doc with errors:
  - **errors**: an unknown key or param (with a did-you-mean), a bad enum or range, a misspelled
    root/mode, a route to nothing;
  - **warnings**: params a synth type ignores, silent voices, unused bands.

  Keys starting `x_` are free annotations. It also prints the doc's **polymeter**: each modulator's
  cycle and how many loops pass before everything realigns.
- **In VS Code** (`engines-lsp`, ADR 0037; the extension, ADR 0038): the checker's findings on the exact
  token, and completion that knows the schema, the doc's ids and its scale (offsets offered as
  `7 · A4 · 5 · D dorian`). Also:
  - hovers in native units and musical terms (the step's chord, the just harmonic a `cents_offset`
    tunes to);
  - inlay hints (note names, Hz/s/ms, loop length, cycles);
  - quick fixes: did-you-mean, snap to the scale, transpose by scale degrees;
  - rename;
  - solo/mute CodeLens on each instrument, and the playing step highlighted where it's written.

  See [editors/vscode/README.md](editors/vscode/README.md).

### The shape of a doc

| section | what it holds |
|---|---|
| `transport` | `bpm`, `master_steps` (steps per loop), `step_unit: "beat"`, `swing`, `timing_jitter_ms`. Loop seconds = `master_steps × 60 / bpm` |
| `harmony` | `root` (`"D"`, `"F#"`…) and `mode` (`dorian`, `lydian`, …). Note offsets are semitones from the root at octave 4 |
| `global` | `master_gain_db` (dB), `stereo_width` (0..1) |
| `instruments` | id → a synth: `type`, `note_engine { polyphony, glide_ms }`, `params`, `sends_db` |
| `modulators` | id → an `lfo` or a `step_seq` (notes or a control curve) |
| `sequence_pools` | named phrase cells a `phrase` arm plays (ADR 0028) |
| `matrix` | id → a route from a modulator to a param (`mod`) or to an instrument's notes (`note`) |
| `buses` | id → a send bus: an effects chain and its `return_gain_db` |
| `optimize`, `movement`, `constraints`, `max_change`, `max_drift` | how the controller plays it (below) |
| `safe_bounds` | JSONPath → `{ min, max }` hard bands for the controller; `min == max` freezes a param |
| `song` | optional: the doc as a finite song (see **Authoring a song**) |
| `meta`, `x_*` | free annotations |

`instruments`, `modulators`, `matrix` and `buses` are objects keyed by id. Nothing sounds until the
**matrix** wires it: a modulator reaches an instrument only through a `mod` route (to a param) or a
`note` route (to that instrument's `.notes`).

### A minimal loop

```json5
{
  schema_version: "0.9",
  loop_id: 1,
  // How the controller plays it (ADR 0033): drift walks timbre levers within bands; no Orrery.
  optimize: "drift",
  movement: { orrery: false },

  transport: { bpm: 56, master_steps: 16, step_unit: "beat" }, // 16 beats @ 56 bpm = a 17.1 s loop
  harmony: { root: "D", mode: "dorian" },                       // offsets: semitones from D4
  global: { master_gain_db: -13.15, stereo_width: 0.6 },              // the doc is the gain stage: ≈ 0.55 FS peak
  safe_bounds: { "global.master_gain_db": { min: -13.15, max: -13.15 } }, // frozen (min == max): drift never turns it up

  instruments: {
    pad: {                                                // the sustained bed
      note_engine: { polyphony: 3, glide_ms: 600 },       // slow portamento between chords
      params: { wave: "saw_soft", voices_detune_cents: 6, level_db: -6.94, cutoff_hz: 460.35, resonance: 0.2,
                attack_s: 1.2, decay_s: 0.6, sustain: 0.85, release_s: 2.2 },
      sends_db: { rv: -4.44 },
    },
    pluck: {                                              // sparse plucks, two octaves up
      note_engine: { polyphony: 3 },
      params: { wave: "triangle", level_db: -10.46, cutoff_hz: 2009.5, attack_s: 0, decay_s: 0.36, sustain: 0.0, release_s: 1.2 },
      sends_db: { rv: -1.94 },
    },
  },

  modulators: {
    breath: { type: "lfo", output: "control", wave: "sine", sync: "free", rate_hz: 0.05, polarity: "bipolar" },
    chords: {
      type: "step_seq", output: "note", sync: "loop", length: 16,
      steps: [
        { step: 0, voicing: { v1: { offset: 0 }, v2: { offset: 7 }, v3: { offset: 10 } } }, // i: D·A·C
        { step: 8, voicing: { v1: { offset: -2 }, v2: { offset: 5 }, v3: { offset: 9 } } }, // ♭VII: C·G·B (same ids ⇒ glide)
      ],
    },
    sparkle: {                                            // 5 hits over 13 beats, free-running: it
      type: "step_seq", output: "note", sync: "free",     // drifts against the 16-beat loop
      length: 13, step_duration: "1/16",
      generator: { kind: "euclid", pulses: 5, rotation: 0 },
      pitch_walk: { pool: [24, 26, 29, 31, 33], order: "random" },
    },
  },

  matrix: {
    m_breath: { source: "breath", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: 0.997, mode: "add", depth_slew_ms: 1200 },
    m_chords: { source: "chords", dest: "instruments.pad.notes", kind: "note", voices: 3 },
    m_sparkle: { source: "sparkle", dest: "instruments.pluck.notes", kind: "note", voices: 3 },
  },

  buses: {
    rv: { return_gain_db: -6.02, effects: { verb: { type: "reverb", params: { room: 0.85, damping: 0.25, shimmer: 0.3, width: 0.8, predelay_ms: 20 } } } },
  },
}
```

`engines-check` passes it clean and reports "realigns every ≈ 91 loops". It renders at ≈ 0.53 FS. The
test suite checks and renders every full doc in this README, so the example stays true.

### Instruments

An instrument's `type` picks its synth (ADR 0009). A param is written in its unit, and its key says
which (ADR 0054, schema v0.9): `_hz`, `_s`, `_ms`, `_db` (`-Infinity` is silent), `_cents`, `_st`;
an unsuffixed number is a plain `0..1`. Any numeric param can be modulated by the matrix and is a drift
candidate, except a binaural instrument's `root_hz`/`spread_hz`, which only you (and routes) move.

| `type` | what it is | its own params |
|---|---|---|
| `subtractive` (the default) | an oscillator → a low-pass filter → an ADSR | `wave` |
| `dual` | osc B phase-modulates osc A (DX-style); integer ratios keep every sideband harmonic | `osc_a_wave`, `osc_b_wave`, `osc_a_level_db`, `osc_b_level_db`, `osc_b_ratio` (0.5–12), `osc_b_detune_cents` (0–50), `fm_index` (0..1 → 0–8 rad), `fm_feedback` |
| `modal` | a bank of struck resonators (ignores the filter and ADSR) | `material` (`resonators` = harmonic, `marimba`, `vibraphone`, `tuning_fork`), `num_modes`, `brightness`, `strike_pos`, `stretch`, `t60_mode` with its ring: `t60_s` (`fixed`, the default: the fundamental's ring time, 0.05–8 s) or `ring_ratio` (`note`: × the note's own length, 0.5–2) |
| `midi` | no audio: notes and CCs to a DAW (ADR 0026) | `routing` |
| `binaural` | two subtractive voices, hard left at root − `spread_hz`/2 and hard right at root + `spread_hz`/2: a beat between the ears on headphones; its dry skips the master `stereo_width` (ADR 0047) | the subtractive params + `root_hz` (real Hz 20–1000; absent ⇒ the note's pitch) and `spread_hz` (real Hz 0–40, the beat), both read exactly as written |

The common params:

| param | range | what it does |
|---|---|---|
| `wave` | `sine`, `triangle`, `saw_soft`, `square`, `pulse`, `sine_bell` | the oscillator; `source: "filtered_noise"` for texture (`band_lo_hz` / `band_hi_hz`, 20 Hz–4 kHz); with `pitched: true` the noise also gets a band on each note, 24 dB/oct either side, `resonance` raising the note; open `band_lo_hz` below a low note so it has noise under it (ADR 0046) |
| `level_db` | −60…+6 dB, `-Infinity` | the instrument's gain (−6 dB ≈ half amplitude) |
| `cutoff_hz` | 80 Hz…8 kHz | low-pass brightness, the main timbral lever (≈ 320 Hz dark, 800 Hz mid, 2 kHz open) |
| `resonance` | 0..1 | filter emphasis at the cutoff (Q 0.707…6) |
| `drive_db` | 0…20 dB | soft-clip pre-gain (×1…×10; 0 bypasses the clipper) |
| `attack_s` `decay_s` `release_s` | 0…3 s / 0…4 s / 0…4 s | envelope times |
| `sustain` | 0..1 | the held level; `0` with a short `decay_s` is a pluck |
| `voices_detune_cents` | 0…100 ¢ | unison detune across the voices |
| `shift_st` | **−24…+24 semitones** | an octave pedal: transposes the instrument's register |
| `shift_mix` | 0..1 | dry ↔ shifted crossfade (0.5 = an octave-doubling shimmer) |

The envelope is **note-relative**: each note starts its ADSR at its own onset, anywhere in the loop.
A held drone needs `sustain` > 0; a pluck is `attack_s: 0`, a short `decay_s`, `sustain: 0`.

### Notes and harmony

A note's `offset` is semitones from `harmony.root` at octave 4 (root `A`: offset 0 = A4 440 Hz, −24 =
A2 110 Hz). A per-voice `cents_offset` reaches just intonation (h5 is −14 ¢, h7 −31 ¢). In VS Code the
inlay hints name each offset's note, hovers give its scale degree and the step's chord, and
completion offers offsets in the doc's scale.

In a step's `voicing`, voices are keyed by id:
- the same id with a new `offset` **glides** (`glide_ms`);
- a new id **articulates** (a fresh envelope);
- `prob` thins a voice;
- `release: true` ends it.

### Modulators

- **`lfo`** (`output: "control"`): `wave`, `rate_hz`, `polarity` (`bipolar` / `unipolar`),
  `sync: "free"`. Slow rates (0.02–0.08 Hz) are the "breathing" gesture.
- **`step_seq`**: `sync` (`loop` restarts each loop; `free` carries on, so a length that doesn't
  divide the loop drifts against it), `length`, `step_duration` (`"1/N"` of the loop), `humanize_ms`.
  - With `output: "control"`, `steps[].value` makes a control curve (`interp: "smooth"` for ramps).
  - With `output: "note"`, use **one** note source:
    - explicit `steps[].voicing`, with a sub-step onset `t` (0.5 = the "and", thirds for triplets);
    - a `generator` (`{ kind: "euclid", pulses, rotation }`) plus a `pitch_walk` (`pool`, `order`);
    - a **`phrase` arm** (`{ pool: [...], select: "seeded" | "cycle" }`) that plays
      **`sequence_pools`**. A pool is `{ fit, tags, length, notes: [{ t, offset, dur?, prob? }] }`:
      - `dur` present makes a gated note; absent, the note holds and glides;
      - `fit: "chord"` snaps to the scale and the Orrery's chord, `"literal"` plays exactly as
        written;
      - `lib:<name>` entries come from the built-in phrase library.
- **`orrery_role`** on a note modulator hands it to the Orrery:
  - `chord` — re-voiced each phrase;
  - `bass` — the chord's root or fifth;
  - `arp` — density and register follow the Arranger;
  - `oneshot` — accents;
  - `context` — a fixed pitch the voyage stays consonant with. With `orrery_disable: true` it never
    moves.

### The matrix

- `kind: "mod"` → `instruments.<id>.params.<param>`, `global.<param>`,
  `buses.<bus>.effects.<fx>.params.<param>` or `buses.<bus>.return_gain_db`. The depth is written in
  the dest's unit, under the one key that fits it: `depth_oct` into a Hz param (log), `depth_db`
  into a gain, `depth_s` / `depth_ms` / `depth_st` / `depth_cents` / `depth_hz` / `depth_x`, or a plain
  `depth` into a `0..1` param. Signed, × the source, it's added to the param's value (a bipolar LFO
  at `depth_oct: 1` swings a cutoff an octave each way); `depth_slew_ms` smooths depth changes. A
  route into a `-Infinity` gain is inaudible: give the gain a finite floor (−60 dB).
- `kind: "note"` → `instruments.<id>.notes`. `voices` is its share of the instrument's polyphony;
  `priority` decides which source gives up a voice first.

### Send buses and effects

Instruments route `sends_db` (dB per bus) into **buses**. A bus runs an effects chain and returns
its wet signal at `return_gain_db` (left out, a return is silent). The reverb's params are `room`,
`damping`, `shimmer` (octave-up pitch-shifted feedback, the rising sheen; 0 = plain), `width` (fixed
at build) and `predelay_ms` (0–100). `room`, `damping`, `shimmer`, `predelay_ms` and
`return_gain_db` can be modulated and drift.

**A doc without `buses` plays dry**, and an instrument is heard wet only through its `sends_db`.
(Schema v0.9 dropped the implicit `rv` reverb a v0.8 doc got from `global.reverb_mix`; `engines-migrate`
writes it out as a real bus.)

### How the controller plays it

The doc carries its own control (ADR 0033). New pieces start from the repo-root
`engines-defaults.json5`:
- **`optimize`**: `"drift"` walks the timbre levers around their written values, at most
  `max_change` per loop and `max_drift` overall, reflecting off band edges (ADR 0032). `"hold"`
  plays exactly what's written (LFOs and sequences still move). The panel's hold ⇄ drift toggle
  switches live.
- **`movement`**: the Orrery. `{ orrery, phrase_loops, tension_target, tension_max, home, arrange }`.
  A tamed voyage uses `phrase_loops` 4–8 and `tension_target` 0.10–0.15.
- **`constraints`** (soft bands the controller steers back into) and **`safe_bounds`** (hard bands)
  map JSONPaths to `{ min, max }` in native units. In either, `min == max` freezes a param.

`--optimize` / `--orrery` on the command line layer over the doc; `--objectives <file>` replaces it.

### Gain staging (there is no master limiter)

Renders feed stems and a DAW, so the master out is `mix · master_gain_db`, unlimited (ADR 0021/0031).
The doc is the gain stage:
- aim for ≈ 0.5–0.55 FS peak on a short render;
- freeze `master_gain_db` and the level-ish levers (voice `level_db`s, `drive_db`, `return_gain_db`)
  with `min == max` bands once they're balanced;
- set `max_drift` ≈ 0.10 for long pieces.

`engines-song verify <doc>` measures a short render in memory: level and peak, the band split, the
centre's harmonicity and pulse, and with `--ab` each route's share of the mix. `engines-song voyage
<doc>` tabulates the Orrery's chords and tension without rendering. Drift creeps loudness up over
hours, so prove a long piece under the controller:
`engines-song --doc <doc> --out <wav> --duration 2h --orrery` must report **0 clipped**. A value
authored below a default range (a `master_gain_db` of −13 dB, under the −8 dB default floor) is kept
as written.

### Structure and polymeter

Loop-synced sequences repeat every loop: predictable. Free-running ones with odd lengths (5, 7, 11,
13, 23 steps) and slow LFOs at unrelated rates drift against the loop and each other: messy, never
quite repeating. `engines-check --parts` (and the inlay hints) say how long until everything realigns.

### The ambient recipe

- **Slow and sparse** — `bpm` 50–70, few events, `prob` < 1, free-running sequences.
- **Soft attacks, long tails** — long `attack_s`/`release_s` and `sustain` on pads. Voices ring
  across the loop seam.
- **Dark, moving filters** — `cutoff_hz` ≈ 300–600 Hz with a very slow LFO on it (a small
  `depth_oct`, ±⅓–⅔ octave, and a long `depth_slew_ms`).
- **Space** — `stereo_width`, a touch of `voices_detune_cents`, and a send-bus reverb with a little
  `shimmer`.
- **Register separation** — pads low/mid, events an octave or two up (`offset` +12…+24, or
  `shift_st`).
- **A centre** — the house style fuses a drone on one fundamental (a `dual` PM voice, just-tuned
  partials, a sub) and lets everything else orbit it. See the `ambient-loop-doc` skill and its
  reference pieces.

## Authoring a song (5–6 h tracks, ADR 0029)

A loop doc becomes a **song** by adding a top-level `song`: an ordered `sequence` of **blocks**,
each a **matrix configuration** played for `loops` inner loops; after the last block a short `tail`
releases every note and lets the reverb ring out, then the track **ends**. Blocks change **the matrix**
and, through an optional `control`, **how the controller moves** (ADR 0055) — instruments, modulators,
harmony, transport and globals hold for the whole song, so one engine plays straight through (voices and
reverb tails cross every seam).

```json5
"song": {
  "matrices": {                                   // named LAYERS, merged over `matrix` by route id
    "settle": { "m_piston": null, "m_chug": null },              // null removes a route
    "deep":   { "m_fire": null, "m_sheen": { "depth": 0.32 } },  // a field overrides
    "bloom":  { "m_new": { "source": "lfo", "dest": "instruments.pad.params.cutoff_hz", "kind": "mod", "depth_oct": 1.33 } }
  },                                                              // a full entry adds a route
  "sequence": [
    { "matrix": "settle", "loops": 200, "label": "settle" },
    { "loops": 420 },                             // no layer ⇒ the top-level matrix as-is
    { "matrix": "deep", "loops": 540,
      "control": {                                // this block only (ADR 0055)
        "movement": { "orrery": false },          // the Orrery rests: the home chord all block
        "drift": { "instruments.pad.params.cutoff_hz": "hold" }  // or { min, max } (native) / { max_drift }
      } },
    { "loops": 200, "seed": 7 }                   // equal explicit seeds replay the same voyage
  ],
  "transition_s": 10,                             // seam glide; 0 = hard cut; ≤ the shortest block
  "tail_loops": 2                                 // release + ring-out after the last block
}
```

- **Length:** loop seconds = `master_steps × 60 / bpm` (16 @ 66 bpm = 14.545 s ⇒ 5 h ≈ 1238 loops).
  `engines-song --dry-run` prints the block table, total time and file size.
- **Each block starts fresh:** drift travel is undone (params back to the authored values) and the
  Orrery begins a new voyage from home (a new seed per block).
- **A block's `control`** patches the piece's control for that block only: `movement` field by field
  (`orrery: false` turns the Orrery off; `phrase_loops`, `tension_target`, … retune it), `optimize`
  (`hold` | `drift`), `max_drift`, `max_change` (it can only lower the step), and `drift` overrides by
  param path — `"hold"` pins it at its authored value, `{ min, max }` gives it a band in native units,
  `{ max_drift }` its own cap. `engines-song --dry-run` prints each block's control; `--loop-block K` plays
  it. In VS Code the panel's hold ⇄ drift and Orrery switches beat a block's control until the next
  block starts.
- **Seams glide** over `transition_s`: params ease from where the last block drifted to, new routes
  fade in, removed routes fade out, notes whose route disappeared release on their own envelope
  (give held voices some `release_s`). Categorical params (`wave`, …) and `voices_detune_cents` step
  instead.
- **A per-block offset** of a base value: add a constant source, e.g.
  `"unity": { "type": "step_seq", "output": "control", "length": 1, "entry_value": 1.0, "steps": [ { "step": 0, "value": 1.0 } ] }`,
  and route it in the block's layer with the offset as its depth, in the dest's unit (`depth_db: -3`
  lowers a level 3 dB for that block).
- **Gain-stage per block** (no limiter, ADR 0021): removing a negative-depth level route *raises*
  the level. `engines-song verify <song doc>` prints every block's start, raw and as the controller
  plays it (peak, RMS, the step from the previous block), and `engines-song seams <song doc>` renders
  the whole form scaled down and judges each seam and the tail. Keep
  `master_gain_db` inside its `safe_bounds` (an explicit band side is a hard clamp; a value below the
  −8 dB *default* floor is kept as authored).
- **Audition live:** the doc keeps playing as a plain loop. In VS Code, the CodeLens above each block
  loops it (**▶ loop block K**) or plays the song from it (**play from here**). A looped block
  re-materializes on every save, so you can tune it live; a playing song records saves and applies
  them on the return to the loop. From a terminal: `make live PIECE=<slug> -- --loop-block 2` (or
  `--play-from 2`).
- **Render:** `make song PIECE=<slug> -- --orrery` writes `pieces/<slug>/renders/<date>_r<rev>/`
  plus a `<date>_r<rev>.json` sidecar (revision, seed, control, peak, LUFS). Outside a piece:
  `make song SONG=<doc> SONG_OUT=renders/night.flac -- --orrery --format s24` (or
  `engines-song --doc … --out …`). A file over 4 GiB is written as RF64. An `--out` ending in `.flac`
  writes lossless 24-bit FLAC (`--format s16` for 16-bit; no f32) — about half the size,
  bit-identical PCM to the s24/s16 WAV. `engines-play`/the TUI play songs live too and stop at the end.
- **One file per block** (ADR 0043): a song renders as `<song>-mix-<NN>-<label>.flac` per block,
  plus `…-tail`. `NN` is the block's number in the `--dry-run` plan; `<song>` is the piece slug or
  the doc's file name. They go into the `--out` directory: `renders/night.flac` ⇒ `renders/night/`.
  `blocks.json` there lists each file's start. The cuts are sample-exact, so the files laid end to
  end on one DAW track **are** the song, bit for bit: no click, gap or level step at a join. In
  Ableton, keep the project at the render's sample rate and turn off warping and clip-edge fades for
  these clips. `--whole` (`make song … WHOLE=1`) writes one continuous file instead.
  Worked examples: `lab/tests/fixtures/song_lydian_tide.json5` (a short test song over the D-lydian
  drone) and `lab/tests/fixtures/song_glass_garden.json5` (1:00:13, D-lydian bells/harp/pad over
  two reverbs).
- **Stems for a DAW** (ADR 0030): `make song PIECE=<slug> STEMS=1 -- --orrery` (or
  `engines-song --doc … --stems <dir>`). It writes one file per audio instrument (its dry signal) and one
  `bus_<id>` file per reverb/send bus (its wet return; an instrument's reverb lives there).
  - **Format:** FLAC by default (`--container wav` for WAV).
  - **Loudness:** each stem is normalized to **−18 LUFS** (BS.1770) with one static gain. Change the
    target with `--stem-lufs -14` (through `make`, with a space). Peaks are capped at −1 dBFS, so
    percussive stems land quieter.
  - **Rebuilding the mix:** `<dir>/stems.json` lists every gain. Set each DAW fader to the stem's
    `mix_fader_db` and the engine's mix is rebuilt exactly.
  - **Disk:** the render first writes float temp files (≈ 1.4 GB per stem-hour, deleted at the end).
    `--dry-run` prints the estimate and the free space; `--stem-temp-dir` moves the temps to another
    volume.
  - **Re-exporting** into the same `<dir>` replaces the previous export's stems, and removes ones it
    no longer writes.
  - **A song's stems are per block too:** `<song>-<stem>-<NN>-<label>.flac`. Each stem's one gain is
    measured over the whole song and applies to every one of its block files. A block where a stem
    is silent still gets its file, so a stem's files dropped in order on one track line up exactly.

## Architecture (ADR 0003)
- **Engine core** — **Rust**, I/O-free and deterministic (`crates/engines_core`): synth + sequencer +
  modulators + matrix + songs + the drift/Orrery controller. No audio device or files in the core;
  recompiles unchanged for iOS. Same doc + seed ⇒ bit-identical render (pinned by
  `crates/engines_core/tests/golden.rs`).
- **Authoring** — `crates/engines_authoring`: `engines-check`, a lossless JSON5 CST, the `engines-lsp` language
  server and `engines-part` with the parts library. `crates/engines_live`: `engines-live`, the live player (mid-loop
  apply, `.live/` status and history, a JSON-lines protocol for the editor). `editors/vscode`: the
  extension.
- **Shells** — `engines-song` for headless renders, a **Rust `cpal`** bin (`engines-play`) for plain live
  audition, an interactive **Ratatui TUI** (`engines-tui` / `make tui`; retired from the composing
  workflow but kept working), and a **Swift** macOS CLI (`shell/`, AVAudioEngine over a C ABI; needs
  Xcode).
- **Lab** — **Python**: drives the engine over the CLI/file boundary (no pybind11) as an acceptance
  suite.

## Dev setup (macOS)
Requires: `cargo` (Rust) + `uv` (`brew install rustup-init uv`); `npm` for the VS Code extension; `jq`
for the hook that checks Claude's piece-doc edits (`.claude/hooks/check-piece-doc.sh`; preinstalled on
recent macOS, else `brew install jq`). The Swift shell additionally needs **full Xcode**.

```sh
make test     # cargo test (engine/controller/session/songs/FFI + golden renders) + the Python lab suite
make vscode   # build + install the VS Code extension (engines-lsp + the live player in the editor; needs npm)
make vscode-test   # the extension's unit + end-to-end suites (an isolated VS Code window opens briefly)
make live PIECE=<slug>   # play a piece from the terminal; every save of its doc is checked + applied (engines-live)
make song PIECE=<slug>   # render a piece into pieces/<slug>/renders/ (+ a .json sidecar; STEMS=1 for stems; a song: a file per block, WHOLE=1 for one)
make check    # strictly check loop docs (unknown keys, enums, ranges, dangling routes) + the polymeter
make part PIECE=<slug> PART=pulse   # add a part (instrument + modulators + routes + sends) to a piece; `make part` lists them
make lsp      # build engines-lsp, the loop-doc language server
make prune PIECE=<slug>  # list old renders that could go (`-- --yes` deletes; keeps published and protected ones)
make templates   # write the annotated loop-doc + objectives templates into the cwd
make play     # hear a doc live: cpal → CoreAudio (`make play -- --doc <doc> --orrery`)
make tui      # interactive TUI (release): live params, Orrery + song state, live-edit
make render   # headless render to a WAV
make help     # list all targets
```

**MIDI out to a DAW (ADR 0026):** give an instrument `"type": "midi"` + a `routing` section
(see the annotated template) and add `--midi` to `make play`/`make tui` — its notes go out as
Note-On/Off and its modulated params as CC on a virtual CoreMIDI source (default **"Engines Out"**,
`--midi-port` renames), clocked to playback. In Ableton: Settings → Link/MIDI, set the port's
**Track** switch On, arm a MIDI track. Note-Offs + CC 123 are flushed on exit — no hung notes.

**…and the DAW's audio back in (phase D):** install a loopback device
([BlackHole 2ch](https://existential.audio/blackhole/)), route the DAW's master **only** to it,
author an `external_return` bus in the doc (`return_gain_db`, `latency_ms` — see the template), and
add `--return-device "BlackHole 2ch"`. The DAW audio is monitor-mixed into the engine's output.

The Swift live shell is in `shell/` (see `shell/README.md`).

## Layout
```
crates/engines_core/   Rust core — src/ (+ bin/ for engines-render/engines-play/engines-song),
                   include/engines_core.h (generated C ABI), src/ffi.rs, tests/golden.rs
crates/engines_authoring/  authoring toolchain: engines-check (strict checking + polymeter), engines-clean, the
                   JSON5 CST (spans, comment-preserving edits), engines-lsp (the language server) and
                   engines-part + assets/parts/ (the parts library)
crates/engines_live/   engines-live: the live player for a piece (watch → check → apply; status.json; limiter)
crates/engines_tui/    interactive Ratatui live explorer (wraps engines_core; retired from the workflow)
editors/vscode/    the VS Code extension (TypeScript): engines-loop language, engines-lsp client, live panel
shell/             Swift macOS shell (AVAudioEngine)
lab/               Python lab (engines_lab/ CLI wrappers) + tests/
contracts/         JSON schemas (loop contract — embedded by the checker —, objectives), the engines-live protocol
engines-defaults.json5 the control settings new pieces start from (copied in at creation)
doc/               SRDs, loop contract, decisions/ (authoritative)
skills/            the portable piece skills (engines-new, engines-edit, engines-song) + their shared reference
.claude/skills/    Claude Code's skills: links to skills/, and ambient-loop-doc (the engine reference)
pieces/            your compositions, one folder each (gitignored)
```

## License

GPL-3.0-or-later, with a contributor licence agreement for outside contributions: see
[`LICENSING.md`](LICENSING.md) and [`CONTRIBUTING.md`](CONTRIBUTING.md).

Engines was developed in a private repository and published here with fresh history. Its decision
records keep their original numbers, so [`doc/decisions/`](doc/decisions/) has gaps where records stayed
private.
