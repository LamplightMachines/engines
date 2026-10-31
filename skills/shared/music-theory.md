# Music theory for loop docs

Everything here is in **semitone `offset`s from `root`** (the engine's note units; `root` sits at
octave 4; `midi = 60 + root_semitone + offset`, so root `A` offset −24 = A2 = 110 Hz). The house
style is a **harmonic-series drone**: one centre frequency, its own overtones as the "chord", a
tempered halo above, a mechanical pulse below. The chord-progression recipe is kept at the end as
a variant.

## The harmonic series in doc units (just intonation)
Harmonic `h` of a centre at offset `c` sits at `offset: c + semis(h)` with `cents_offset: cents(h)`:

| h | ratio | semis above f0 | cents vs 12-TET | what it is |
|---|---|---|---|---|
| 1 | 1 | 0 | 0 | the centre |
| 2 | 2 | 12 | 0 | octave |
| 3 | 3 | 19 | +2 | perfect fifth |
| 4 | 4 | 24 | 0 | two octaves |
| 5 | 5 | 28 | −14 | the just major third |
| 6 | 6 | 31 | +2 | fifth |
| 7 | 7 | 34 | −31 | the natural ("barbershop") seventh |
| 8 | 8 | 36 | 0 | three octaves |
| 9 | 9 | 38 | +4 | the ninth (major second) |
| 10 | 10 | 40 | −14 | third |
| 11 | 11 | 42 | −49 | a quarter-tone ♯11 — skip it, or embrace it deliberately |
| 12 | 12 | 43 | +2 | fifth |
| 13 | 13 | 44 | +41 | skip |
| 14 | 14 | 46 | −31 | seventh |
| 15 | 15 | 47 | −12 | the just major seventh |
| 16 | 16 | 48 | 0 | four octaves |

Worked example, centre A2 (root `A`, `c = −24`): h3 → `{ offset: -5, cents_offset: 2 }` (E4 330 Hz),
h5 → `{ offset: 4, cents_offset: -14 }` (C♯5 550 Hz), h6 → `{ 7, +2 }`, h7 → `{ 10, -31 }` (770 Hz),
h9 → `{ 14, +4 }` (990 Hz). Rendered, these land within ±1 ¢ of `k·110` — `engines-song verify` checks.

## Which harmonics to voice (and which not to)
- The **crank** (`dual`, integer-ratio PM) already supplies h1 and, through its sidebands, h2 h3 h4…
  at falling amplitude; the **partials** layer is for *upper* colour the PM leaves thin: **h3 h5 h6 h7
  h9** is the house voicing (fifth, just third, fifth, natural seventh, ninth — rich but fused). Add
  h10/h12 for more shimmer; keep 4–6 voices (one `polyphony` slot each), triangle or sine waves (their
  own overtones are still multiples of f0, so the series stays pure).
- Don't double the crank's h2/h4 in the partials (mud) and don't use `voices_detune_cents` on them — exact
  fusion is the point; beating belongs to the halo.
- The **sub** at f0/2 makes the centre feel like the *second* harmonic of something deeper — the
  whole spectrum becomes the series of f0/2. Voice it as a `saw_soft` under a low filter (`cutoff_hz` ≈ 320):
  its odd harmonics (3·, 5·, 7·f0/2) fill the family between the centre's partials and it growls
  instead of humming (a sine is the gentler choice). An f0/4 firing tremolo on the crank keeps its
  sidebands in that same family.

## The mode the series implies
h1–h9 with h7 = root, M2, M3, P5, m7 ⇒ **mixolydian** (A B C♯ D E F♯ G for A). That is the natural
key for a harmonic-series drone; the Orrery's scale, the `lib:` pools' `fit:"chord"` snapping and any
halo colour all read from `harmony.mode`, so set it to what the *partials* say. Omit h7 and the series
reads as major (**ionian**) — or **lydian** if you voice h11 as a ♯11. Never aeolian/dorian over a just
h5: the minor third fights the major third you are sounding.

## The halo's home = the "tempered image" of the series
The halo (`orrery_role: "chord"`) is tempered (the Orrery rewrites it with `cents_offset: 0`), so its
**home voicing must be tones that sit within ~4 ¢ of the just series** — the octaves, fifths and ninth
(h2 h3 h4 h6 h8 h9 h12): for A that is **E A E B** (`v1: -5, v2: 0, v3: 7, v4: 14`) — an open
fifths/ninth stack, **no third, no seventh**. At rest nothing beats; when the Orrery visits a chord
containing C♯ or G, the tempered tone beats against the just h5 (−14 ¢ ⇒ ~4 Hz at C♯5: a pleasant
throb) or h7 (−31 ¢ ⇒ ~14 Hz at G5: rough) — brief colour, resolved by gravity. Voice the home in
offsets −5..14: the Orrery places pad voices around offset 7 ± 6.

## Pedal point: how the floor tames the voyage
With the centre layers as `context`, the Orrery scores every candidate chord by `chord_fit` against the
sounding pitch-classes (for the A voicing above: **A E C♯ G B**): shared tone +1.0, fourth/fifth +0.6,
thirds/sixths +0.4, whole tone 0, tritone −0.7, **semitone −1.0**. Chords built from those tones (A7,
A9, F♯m7, C♯m7, Bm7…) win; F (semitone under E) or B♭ lose. Measured on the reference doc at default
movement: tension mean 0.05 / max 0.40 over 20 loops, one borrowed Fmaj7 that resolved next phrase.
More floor pitches = a tighter orbit; a floor of only root + fifth lets the third and seventh roam.

## Register map (house style)
| layer | where | why |
|---|---|---|
| sub | f0/2 (55 Hz for A2) | a low-filtered saw: the family's true fundamental, growling under the centre; nearly dry |
| crank | f0 (73–110 Hz) | the note; PM harmonics reach ~1 kHz |
| partials | h3–h9 (330–1000 Hz) | the overtone body |
| halo | offsets −5..14 (330–1000 Hz) | shares the partials' band but is quiet, tempered, wet |
| pulse | f0 and 2f0 (modal strikes) | a thump inside the drone, not above it |
Everything above 2 kHz is reverb shimmer. A centre-drone is *dark* by design; brightness lives in
the crank's `fm_index` and the partials' `cutoff_hz`, both breathing under slow LFOs.

## Designing the pulse (the "engine")
Mechanical means **grid-locked** (`sync:"loop"`), not polyrhythmic. Build it from sequence pools on a
one-beat host step (`step_duration:"1/16"` over 16 beats), all `fit:"literal"` on f0 / 2f0:
- **idle** (`calm`,`sparse`, length 4): the beat on f0 (`dur` .35) + the "and" (`t: x.5`) an octave up
  as a ghost (`dur` .2, `prob` ≈ .55) — an 8th-note tick with a breathing off-beat;
- **fourstroke** (`calm`, length 2): power on the beat, exhaust on the "and", intake on beat 2,
  compression = rest — an accent/rest cycle the ear reads as a cycle;
- **overrun** (`busy`, length 2): triplets (`t` = 0, ⅓, ⅔) — the entry the Arranger reaches for at high
  energy (`busy` ∝ energy, `calm`/`sparse` ∝ 1 − energy);
- one `lib:` entry (`lib:pedal_pulse`, `fit:"chord"`) if you want a mid-register tick that harmonises.
Under it, the **chug**: a bar-locked control step-seq at `"1/64"` (16ths at 16 beats) routed into
the crank's `level_db` at a negative `depth_db` (−6.18) with **unequal** dips (`0, .55, .2, .7` ⇒ 0 to
−4.3 dB) — equal dips read as a tremolo, unequal
ones as a two-cylinder lope; period = one beat, dominant envelope line = the 8th. Tempo 60–72 bpm puts
the 8th at 2–2.4 Hz, the natural idling rate. A touch of `swing` (0.05–0.12) makes the idle uneven; keep
the pulse's `humanize_ms` small (3–6 ms) — an engine wobbles, it doesn't play loose.

---

## Variant: chord-progression ambient (the older recipe)
Use when the brief wants a *moving* harmonic bed rather than a centred drone. Everything below still
holds for that style.

## Modes (semitones from root)
| mode | degrees | flavor |
|---|---|---|
| ionian (major) | 0 2 4 5 7 9 11 | bright, resolved |
| **dorian** | 0 2 3 5 7 9 10 | minor + bright 6th — the classic modal/ambient color |
| phrygian | 0 1 3 5 7 8 10 | dark, Spanish/tense |
| lydian | 0 2 4 6 7 9 11 | dreamy, floating (#4) |
| mixolydian | 0 2 4 5 7 9 10 | major + b7, hopeful-mellow |
| **aeolian (minor)** | 0 2 3 5 7 8 10 | natural minor, melancholic |
| locrian | 0 1 3 5 6 8 10 | unstable (rare) |

Ambient favors **dorian / aeolian / lydian**. The `mode` field only labels the scale for
`pitch_walk`; chord/voicing offsets are whatever you write — keep them in-scale for consonance.

## Triads/7ths as offset stacks (root-position; transpose by adding the chord root offset)
- minor triad `0 3 7`; minor7 `0 3 7 10`; min9 `0 3 7 10 14`
- major triad `0 4 7`; major7 `0 4 7 11`; maj9 `0 4 7 11 14`
- dom7 `0 4 7 10`; sus2 `0 2 7`; sus4 `0 5 7`; add9 `0 4 7 14`
- **quartal/open** (very ambient, ambiguous): `0 5 10` or `0 7 14` (stack 4ths/5ths — no 3rd)

Build a chord on scale degree *d* by adding *d*'s semitone to each stack tone (and keep results in the
mode). Drop or omit the 3rd for the floaty "no-major/minor" ambient sound.

## Progression templates (slow — one chord every 4–8 beats, gliding)
In **D dorian** (root D; bVII=C, bIII=F, iv=G, v=Am, i=Dm):
- **i – bVII – bIII – v**: Dm9 → Cmaj7 → Fmaj7 → Am7  (modal, gently descending pull — a strong default)
- **i – IV(dorian, major) – i – bVII**: Dm → Gmaj → Dm → Cmaj  (the bright dorian IV is the signature)
- **i – v – bVII – IV**: Dm → Am → Cmaj → Gmaj
In **A aeolian**: i – bVI – bIII – bVII (Am → Fmaj → Cmaj → Gmaj) — the classic minor "axis".
In **F lydian**: I – II(major) – I – II (Fmaj → Gmaj …) — the #4 keeps it suspended/dreamy.

## Voicing & voice-leading (make the pad glide, not leap)
- Keep **4 pad voices** with stable ids `v1..v4` across all chord steps so each **glides** to the
  nearest chord tone (small semitone moves; reuse the id, change `offset`).
- Hold a **common tone** on top as a pedal (e.g. keep `v4` on the same `offset` across 2–3 chords).
- A little `cents_offset` (±3–5) on inner voices = slow beating/chorus; `voices_detune_cents` 4–6 lush.
- Worked example (D dorian, E5=offset 14 held as a pedal):
  - Dm9  `v1 0, v2 7, v3 10, v4 14`
  - Cmaj7 `v1 -2, v2 5, v3 9, v4 14`
  - Fmaj7 `v1 3, v2 7, v3 10, v4 14`
  - Am7  `v1 7, v2 10, v3 14, v4 17`

## Bass (deep, register-separated)
One mono voice (`polyphony:1`, `glide_ms` 400–700) on the chord roots an octave or two below:
roots of the progression at `offset −24..−17` (≈55–110 Hz). Triangle + low `cutoff_hz` (≈ 220) = a warm
sub clearly distinct from the saw pad's mid band. Reuse one voice id so it **glides** the bassline.

## Euclidean rhythms `E(pulses, steps)` (the subtle rhythm + polyrhythm engine)
Evenly distribute `pulses` over `steps` — musical, non-square patterns:
- `E(3,8)` = `x..x..x.` (tresillo)   · `E(5,8)` = `x.xx.xx.` (cinquillo)
- `E(7,16)` = `x..x.x.x.x.x.x...`-ish (a flowing 7-over-16) · `E(5,16)`, `E(9,16)` sparser/busier
- `rotation` shifts the pattern start. Drive a pluck's `generator` with these; pair with `pitch_walk`
  for melody. Keep it **subtle**: 5–9 pulses over 16 at eighth-note `step_duration:"1/32"`.

## Polyrhythm recipes
The cross-rhythm comes from **two periods that don't line up**:
- **Locked vs free**: a bar-locked (`sync:"loop"`) gate/wah pulse + a `sync:"free"` euclid pluck →
  the pluck precesses against the pulse every loop.
- **Coprime free lengths**: pluck `length:16` (`"1/32"`) against a sparse event seq `length:17`
  (`"1/16"`) — they realign only every lcm of periods (musically "never"), so the texture evolves.
- **Polymeter**: same `step_duration`, different `length` (e.g. 3 vs 4 vs 5 against the 16-beat bar)
  gives a rotating accent. A control LFO at a non-bar rate (0.022 Hz over a 17.8 s loop) adds a third,
  slow period on top.
Aim for **2–3 simultaneous periods**: the bar (chords/bass), a locked pulse, and ≥1 free/drifting part.

## Putting it together (ambient + subtly rhythmic)
1. Root/mode → progression → 4-voice gliding pad (`poly`), deep gliding bass (`mono`).
2. A **subtle** euclid pluck (`pluck`, `attack_s: 0`/`sustain: 0`, up an octave so it rings clear), free-
   running for polyrhythm; optional sparse `event` bells (also high, `prob`<1) for twinkle.
3. **Slow** free LFOs on `poly.{cutoff_hz,resonance,shift_mix}` (breathing) + **bar-locked** step-seqs on
   `pluck.{cutoff_hz,resonance,shift_mix}` (rhythmic wah/accents/up-octave stabs).
4. Keep the pluck/bells supporting (≈30–50% of mix energy in the A/B), the pad/bass leading.
