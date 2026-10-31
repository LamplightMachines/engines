# Orrery — a generative harmonic-morphing engine for ambient drone

- **Status:** proposed design (direction chosen 2026-06-18; an implementation plan + ADR will
  follow). This is the creative architecture, not a build plan.
- **Supersedes the open questions in** `harmonic-melodic-control-options.md` with locked answers.
- **Related:** ADR 0005 (param-range seam + CLAP legibility), `design-rationale.md`,
  `.claude/skills/ambient-loop-doc/reference/music-theory.md` (the theory this design promotes
  into code). Engine anchors: `crates/sme_core/src/{controller,feature,learner,loop_doc,modulator,voice,engine}.rs`.

## The metaphor (and why it's the architecture, not decoration)

An **orrery** is a clockwork model of a solar system — planets orbiting a sun in smooth,
deterministic, never-quite-repeating motion. That *is* tonal ambient music:

- **The tonic is the star.** Home. The `harmony.root` / `harmony.mode` center.
- **Chords are bodies in orbit.** A chord's **distance from the star = its harmonic tension**
  (how far from rest it feels).
- **Stable inner orbits** = consonant, diatonic, restful chords. **Eccentric outer orbits** =
  borrowed / secondary / colored chords — further out, more tension.
- **Gravity** = the restoring pull toward the tonic. Wander outward and the pull strengthens, so
  every departure *eventually resolves home* — but gradually, never with a jerk (locked answer #1).
- **An orbital transfer** = a discrete harmonic move (option **B**).
- **The glide along the transfer arc** = continuous **voice-leading morph** (option **A**) — bodies
  don't teleport between orbits; they trace a smooth path.
- **Solar wind** = the CLAP objective ("warmer", "more spacious"), a gentle steady pressure that
  biases the whole voyage toward the user's perceptual goal without overpowering the orbits.
- **Comet tails** = melodic strands drawn from the body's current chord/scale, trailing the motion.

The magic we're after — *smooth but interesting* — falls straight out: **voice-leading + gravity
give smoothness; borrowed orbits + seeded generation + a developing motif give interest.** Every
session is a new voyage (new seed, new objective), but any single loop is bit-reproducible.

## Locked design decisions

1. **Borrowed chords are allowed and they resolve home** — implemented as the gravity field, not as
   a whitelist. Outer orbits exist; the restoring force guarantees return.
2. **Both surfaces** — harmony (chord bodies) *and* melody (comet tails) are generated and steered.
3. **Two-timescale scoring** (my call): continuous texture/morph params scored per loop (today's
   machinery); discrete harmonic moves scored over a **phrase window** (multiple loops), because
   harmony only reveals itself over time.
4. **One-shot from musical intuition** — no legibility pre-gate; the theory makes it sound good
   first, and CLAP biases on top. (We can still run the sweep rig later to *confirm* which biases
   bite.)
5. **B + hybrid** — a discrete **Harmonic Voyager** (B, a music-theory-primed contextual bandit)
   coupled to a continuous **Voice-Leading Morph** (A), both living inside a generative **Orrery
   field** (C-style grammar with a gravity potential). Three ideas, one system.
6. **Mostly generated from theory in code** — a dedicated, self-contained `theory` crate/module
   holds the music knowledge; the field is generated from it, not hand-authored per loop.

## Architecture at a glance

```
                        ┌──────────────────────────────────────────┐
   user objective  ───► │  SOLAR WIND  (CLAP objective bias)        │
   ("warmer"…)          └──────────────────────────────────────────┘
                                   │ biases both layers
        ┌──────────────────────────┴───────────────────────────────┐
        ▼ slow loop (per phrase, ~4–16 loops)        fast loop (per loop) ▼
 ┌─────────────────────────────┐              ┌──────────────────────────────┐
 │  HARMONIC VOYAGER  (B)      │   chord +    │  VOICE-LEADING MORPH  (A)     │
 │  discrete orbital transfer  │── voicing ──►│  glide voices to targets;     │
 │  = contextual bandit over   │   targets    │  steer openness, register,    │
 │  music-theory moves,        │              │  extension, morph-rate,       │
 │  gravity-defaulted          │◄── ΔCLAP ────│  melody range/density/conson. │
 └──────────────┬──────────────┘  (phrase)    └───────────────┬───────────────┘
                │ moves constrained & scored by               │ rewrites doc params
                ▼                                              ▼
 ┌──────────────────────────────────────────────────────────────────────────┐
 │  THE ORRERY FIELD  (generated from the THEORY core)                        │
 │  nodes = reachable chords (diatonic+borrowed+secondary), radius = tension, │
 │  edges = voice-leading smoothness × functional pull, gravity → tonic       │
 └──────────────────────────────────────────────────────────────────────────┘
                                   │ all built from
                                   ▼
 ┌──────────────────────────────────────────────────────────────────────────┐
 │  THEORY CORE  (pure, deterministic, isolated module — the music knowledge) │
 │  pitch · scales/modes · chords · function · tension · voice-leading ·      │
 │  borrowed/secondary/resolution · melodic walk · motif development          │
 └──────────────────────────────────────────────────────────────────────────┘
                                   │ realized as
                                   ▼
        existing engine: note modulators (voicings / pitch_walk) + glide + synths
```

## Part 1 — The Theory Core (the music knowledge in code)

A pure, I/O-free, deterministic module (`crates/sme_core/src/theory/` or its own crate
`sme_theory`), **kept separate** from DSP/controller code as requested. It owns *all* music
knowledge so the rest of the system stays generic. Sketch:

```rust
// pitch — pitch-class & interval math (mod 12)
pub struct Pc(pub u8);                 // 0..11
pub fn interval(a: Pc, b: Pc) -> u8;   // directed mod-12

// scale — modes as degree sets; the working scale
pub enum Mode { Ionian, Dorian, Phrygian, Lydian, Mixolydian, Aeolian, Locrian,
                HarmonicMinor, MelodicMinor, WholeTone /* extensible */ }
impl Mode { pub fn degrees(self) -> &'static [u8]; } // dorian => [0,2,3,5,7,9,10]
pub struct Scale { pub root: Pc, pub mode: Mode }
impl Scale {
    pub fn pitches(&self) -> Vec<Pc>;
    pub fn contains(&self, pc: Pc) -> bool;
    pub fn quantize(&self, semis: f32) -> f32;       // snap to nearest in-scale tone
    pub fn degree_semitone(&self, degree: i8) -> i32; // mode-aware, octave-wrapping
}

// chord — built on a scale degree, with quality/extensions/inversion/source
pub enum Quality { Min, Maj, Dim, Aug, Sus2, Sus4, Quartal }
pub enum Source  { Diatonic, Borrowed(Mode), SecondaryDom(i8), Neapolitan, Chromatic }
pub struct Chord { pub degree: i8, pub quality: Quality, pub adds: Extensions,
                   pub inversion: u8, pub source: Source }
impl Chord {
    pub fn tones(&self, s: &Scale) -> Vec<i32>;       // semitone offsets from root
    pub fn pitch_classes(&self, s: &Scale) -> Vec<Pc>;
}

// function — Roman-numeral function + where a chord "wants" to resolve
pub enum Function { Tonic, Subdominant, Dominant, Color }
pub fn function_of(c: &Chord, s: &Scale) -> Function;
pub fn resolution_targets(c: &Chord, s: &Scale) -> Vec<Chord>; // gravity gradient home

// tension — the orbital radius (0=rest .. 1=far)
pub fn tension(c: &Chord, s: &Scale, prev: Option<&Voicing>) -> f32;

// voice-leading — minimal-movement morph between sounding voicing and a target chord
pub struct Voicing { pub voices: Vec<(VoiceId, i32)> } // stable ids → semitone offsets
pub fn lead(from: &Voicing, to: &Chord, s: &Scale, opts: &LeadOpts) -> Voicing;

// melody — scale/chord-aware contour walk + motif development
pub struct MelodyParams { pub range: f32, pub density: f32, pub step_leap_bias: f32,
                          pub consonance: f32, pub register: f32, pub direction: f32 }
pub struct Motif { /* interval contour + rhythm cell */ }
pub fn next_phrase(s: &Scale, c: &Chord, motif: &Motif, p: &MelodyParams, seed: u64)
    -> (Vec<NoteEvent>, Motif);
```

Design notes that make it musical, not mechanical:

- **`tension`** blends (a) functional distance from tonic, (b) interval roughness of the chord
  (tritones/semitones add tension), (c) out-of-scale "borrowed-ness", and (d) voice-leading
  distance from the current voicing. This single scalar *is* the orbital radius — and the objective
  can bias a **preferred tension band** ("calmer" → inner orbits, "restless" → wider orbits).
- **`resolution_targets`** encodes real tendencies: secondary dominant `V/x → x`; Neapolitan
  `bII → V → i`; modal-interchange `bVI/bVII → i`. This gives gravity a *direction*, not just a
  magnitude — borrowed chords don't just decay home, they resolve *correctly*.
- **`lead`** is minimal-total-movement voice assignment (tiny voice counts → exact solve), with
  common-tone retention and a **pedal option** (hold a top voice across several chords — the floaty
  ambient anchor from music-theory.md). Stable `VoiceId`s mean the engine's existing **glide** does
  the morph for free.
- **`next_phrase`** is a seeded contour-biased walk over scale tones, gravitating to **chord tones
  on structural beats** and allowing **passing/neighbor tones** on weak beats; it carries a
  **motif** that is re-rooted diatonically as harmony changes and occasionally developed
  (fragment / invert / augment) — the recognizable thread that ties a long session together.

## Part 2 — The Orrery field & gravity

The field is **generated** from the theory core for the current `Scale`:

- **Nodes** = the reachable chord vocabulary: all diatonic chords, plus a curated outer shell of
  borrowed (modal-interchange) chords and secondary dominants. Each node carries its `tension`
  (radius), `function`, and `resolution_targets`.
- **Edges** = legal transitions, weighted by `voice_leading_smoothness × functional_pull ×
  resolution_tendency`. Smooth, musically-plausible moves are cheap; jarring leaps are expensive.
- **Gravity** = a potential with the tonic at the bottom. The transfer score is roughly:

  ```
  score(edge) =  μ · musical_prior(edge)              // theory: smooth + plausible
               − γ(t) · Δtension(edge)                // restoring force, grows with time-out
               − λ · repetition_penalty(edge)         // keep it from looping
               + w · objective_bias(edge, CLAP)       // the solar wind
  ```

  `γ(t)` **grows with accumulated tension and loops-since-home**, so excursions are allowed but the
  pull home strengthens the longer/further you roam → **guaranteed gentle resolution** (answer #1).

The Voyager's state: `current chord`, `current voicing`, `accumulated tension`, `loops_since_home`,
`motif`. A transfer is chosen by **seeded softmax** over edge scores (temperature = an
"adventurousness" knob), so the path is generative and non-repeating, yet reproducible per
`(session_seed, loop_id)`.

## Part 3 — The two-timescale controller (B + A)

### Slow loop — the Harmonic Voyager (discrete, option B)

Every **phrase** (when the current chord has "settled," ~4–16 loops), pick the next orbital
transfer. This is a **music-theory-primed contextual bandit**:

- **Candidates** = the field's legal out-edges from the current node.
- **Prior** = the gravity-weighted `musical_prior` (so even untrained, it sounds intentional).
- **Learned bias** = a per-CLAP-term linear value over harmonic features
  `φ_harm = [function one-hot, tension bucket, register, loops-since-home] ++ [centroid, rms,
  flatness]` (reusing the RLS structure from `learner.rs`, with **move-type / target-function
  one-hots** in place of param-role one-hots — the option-B insight made concrete).
- **The clever counterfactual:** the antithetic baseline for a discrete move is the
  **gravity-default move** (what theory alone would have chosen). We score *chosen vs. default* over
  the phrase window, so `ΔCLAP` measures **"did steering beat the musical default?"** The baseline
  is always musical → exploration is always safe, and the learner only ever nudges *away from
  default when CLAP rewards it*. This is the multi-loop, structurally-honest analogue of today's
  ±δ pair.

### Fast loop — the Voice-Leading Morph (continuous, option A)

Between harmonic states, the existing continuous controller steers the **character** of the morph
and texture, per loop, with today's antithetic ±δ + RLS, on the ADR-0005 seam + new `ROLES`:

- `morph_rate` (→ `glide_ms`), `voicing_openness`, `register` (diatonic, the in-key cousin of
  `shift`), `extension_density`, and the melodic knobs (`mel_range`, `mel_density`,
  `mel_consonance`, `mel_register`), plus all existing timbre params.

So: **discrete decisions, slowly; smooth morphs and texture, continuously** — the hybrid.

## Part 4 — The melodic layer (comet tails)

Driven by the arp/pluck note modulators. Each phrase, `theory::next_phrase` regenerates the
melodic content from the **current chord + scale**, so melody tracks the harmony automatically. The
controller's continuous melodic knobs steer ambitus, density, step-vs-leap, chord-tone adherence
(consonance↔tension), and register. The **motif** is the unifying thread: a short cell re-rooted to
each chord and occasionally developed — giving a sense of *the same music evolving*, not random
notes. Implementation reuses `pitch_walk` (scale/chord-locked pool) and/or generated explicit
`step_seq`s, seeded by `hash_site`.

## Part 5 — Determinism & "new music every time"

These aren't in tension once you split the seeds:

- **Per-loop determinism (CE-R7.3) is preserved:** every stochastic choice (transfers, softmax,
  melodic walk, prob rolls) is seeded by `hash_site(session_seed, loop_id, …)`. Same inputs ⇒
  bit-identical audio — the antithetic/counterfactual/offline-rescore machinery still works.
- **New every session:** the **`session_seed`** (injected by the shell — the core stays wall-clock-
  free) parametrizes the entire voyage; a different seed traces a different path through the field.
  Combined with the chosen **objective** (different solar wind) and the **emergent learner state**,
  every session is a genuinely new piece, while remaining fully reproducible given its seed +
  objective.

## Part 6 — Mapping onto the existing engine (reuse, don't rebuild)

- **Harmony → note-modulator voicings.** Each phrase the Voyager rewrites the chord `step_seq`'s
  id-keyed `voicing`; `theory::lead` assigns target `offset`s to **stable voice ids**, so
  `voice.rs`'s existing **glide/sustain** performs the morph automatically. The morph is *mostly
  free*.
- **Bass** = one mono voice gliding the chord roots (reuse `glide_ms`).
- **Melody → arp modulator** `pitch_walk.pool` / generated `step_seq` (scale/chord-aware, seeded).
- **Continuous params** ride the ADR-0005 seam + `ROLES`; the existing `controller`/`learner`/
  antithetic/corpus/sweep code is **unchanged**.
- **Discrete moves** = the new slow-loop layer (Voyager bandit) + small corpus additions (chosen
  move, gravity-default baseline, phrase-window scores).
- `harmony.mode` finally becomes **active** (it's parsed-but-unused today).

## Part 7 — A worked voyage (D dorian, to make it concrete)

```
loop 0    HOME      Dm9      v1 0  v2 7  v3 10  v4 14   tension .05   (rest)
phrase →  the bright dorian IV (signature color), small voice-led glide
loop 6              Gmaj7    v1 -1 v2 5  v3  7  v4 14   tension .28   ▲ depart
phrase →  borrow bVII for lift (modal interchange), gravity still gentle
loop 12             Cmaj9    v1 -2 v2 4  v3  9  v4 14   tension .41   ▲ outer orbit
phrase →  γ(t) now strong; resolution_targets pull toward the dominant region
loop 18             Am7      v1  0 v2 4  v3  7  v4 12   tension .33   ▼ turning home
phrase →  cadence-ish settle
loop 24   HOME      Dm9      v1  0 v2 7  v3 10  v4 14   tension .05   ▼ arrived
```

Throughout: **E5 (offset 14) is held as a pedal on v4** (common-tone anchor → seamless), every
inter-chord motion is the **smallest available glide**, a 3-note **motif** re-voices to each chord,
and a "warmer" objective has quietly biased openness down and register toward the low-mid the
solar-wind term rewards. Re-run with a new `session_seed` and the same home → a *different* but
equally coherent voyage.

## Part 8 — What the later plan will pin down

- Exact `tension` weighting and the borrowed/secondary **vocabulary breadth**.
- Phrase-boundary policy (fixed K vs. "settled" detection) and the phrase scoring window.
- `γ(t)` gravity curve + softmax temperature (the smoothness↔adventure dial).
- Motif-development rules (how often / how to fragment/invert).
- Bandit feature set + whether the objective also biases the **tension band** directly.
- Corpus schema additions for discrete moves; feature-schema-version bump.
- The §7 sweep rig run to *confirm* which harmonic/melodic biases CLAP actually hears.

## Why this is the right shape

It is **mostly generated from theory** (answer #6), uses **B plus two more ideas** woven into one
system (answer #5), lets the music **wander and return** (answer #1), moves **both** harmony and
melody (answer #2), scores **at the timescale each layer lives on** (answer #3), and **reuses the
proven engine/controller stack** wherever possible while adding exactly one new layer. The theory
guarantees beauty; gravity guarantees coherence; voice-leading guarantees smoothness; the seed
guarantees novelty; CLAP guarantees it drifts toward what the listener asked for. That's the
magic — a clockwork that never plays the same piece twice.
```
