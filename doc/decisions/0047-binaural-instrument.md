# ADR 0047 — Binaural instrument: a hard-L/R beat pair

> **Amended by [ADR 0054](0054-native-units-schema-v0-9.md) (2026-10-27, schema v0.9):** D3's uniform-fraction depth rule is **reversed** (ADR 0054 D3). A route's depth is in its dest's unit, `depth_hz` for `root_hz` and `spread_hz`, so `loop_binaural_basic.json`'s `depth: 0.05` into `spread_hz` (2 Hz at full swing) is now `depth_hz: 2`. Its gains are in dB: the `binaural` part's `level: 0.25` is `level_db: -12.04`. `voices_detune` and `shift` are `voices_detune_cents` and `shift_st`.

Status: accepted — 2026-10-22 (asked for by the user: a binaural-beating bass drone)
Relates to:
- ADR 0005: the native-units seam. `root_hz`/`spread_hz` are native, and stricter than `shift` (read
  exactly as authored).
- ADR 0009: synth types (a new one, built from the subtractive source).
- ADR 0027 / 0030: stems (the pair's dry skips the width stage there too).
- CE-R7.3: determinism. Docs without a binaural instrument take the old paths verbatim, so no golden
  hash moves.

## Context

A binaural beat is two pure tones a few Hz apart, one per ear. The brain hears a beat at the
difference, though neither ear gets one. The user wants this as an instrument type for long
sleep/focus tracks:
- two basic subtractive synths, one panned hard left and one hard right;
- the author gives a root frequency R and a spread S. The left plays R − S/2 and the right R + S/2
  (110 / 2 ⇒ 109 | 111);
- every usual subtractive param applies to both.

Before this ADR the engine couldn't do it:
- a voice's source was mono, and each voice got a small fixed hash pan (±0.2);
- pitch always came from a note (key + offset + cents);
- no param was in real Hz;
- the master `stereo_width` mid/side stage bleeds L into R, and at 0 folds the pair into one channel,
  which turns a binaural beat into an ordinary acoustic one.

## Decision

**D1 — `type: "binaural"`: two subtractive voices, hard L / hard R.**
- `SynthKind::Binaural(BinauralConfig)`. It's the only type whose source is stereo
  (`SynthKind::hard_lr`).
- Per voice, the subtractive oscillator runs once per side, at `binaural_pair(centre, spread)` =
  (centre − spread/2, floored at 0 Hz; centre + spread/2).
- Drive, the per-voice low-pass, the ADSR and `level` apply identically to both sides. The right
  side's low-pass lives in the per-voice filter map's second slot, which only noise used; a binaural
  voice is never noise.
- **The octave pedal** (`shift`/`shift_mix`) runs per side too, and its oscillator gets the **same**
  ±spread/2 Hz, so the whole instrument beats at one rate. Scaling the offset by the pedal's ratio
  would have given a second beat at 2× or ½× the rate.
- The left side keeps the voice's own `phase_rad`/`phase_rad_shift`. The right side's two phases
  ride in `SynthVoiceState::Binaural`, carried across the seam in the snapshot. Both sides start at
  phase 0 on articulation.
- The pair is hard-panned: left → L only, right → R only. There is no voice pan. With one voice, each
  channel carries what a centred mono voice would.
- `filtered_noise` on a binaural instrument is a **validate() error**: noise has no pitch to beat.

**D2 — The root: `root_hz` when set, else the note.**
- **`root_hz` set:** it pins the carrier. The routed note only **gates** the voice (envelope, song
  blocks, mute); its pitch, cents and glide are ignored. `voices_detune` still spreads unison voices
  around it, giving a chorus of pairs that all beat at `spread_hz`.
- **`root_hz` absent:** the root is the voice's note pitch, exactly as subtractive computes it. It
  follows the key, the glide and the Orrery (if the note isn't `context`).
- sme-check warns when a pinned root with polyphony > 1 and no `voices_detune` would stack identical
  pairs.
- Caveat: with `root_hz` pinned, the Orrery still hears the routed note's pitch (it reads note
  modulators, not instruments). The `binaural` part leaves the root to the note for that reason.

**D3 — Real Hz, read exactly.**
- `root_hz` (20..1000) and `spread_hz` (0..40) are authored, checked, hovered and rendered as Hz,
  fractions allowed.
- Unrouted, the engine reads the authored f32 directly, with no normalize → denormalize round trip,
  so 109.5 sounds at exactly 109.5.
- **Routed** (or glided at a song seam), the value is `authored + (effective − base) · span`. An idle
  route (depth 0, or a smoother at rest) adds exactly 0, so the authored value is untouched.
- A route's `depth` keeps the **uniform rule**: a fraction of the native range, as for `shift` and
  `osc_b_ratio` (the user chose one rule over an Hz-only exception). The LSP hover says what a depth
  is in Hz at full swing.
- A route into an **unset** `root_hz` is ignored: the root keeps following the note, and sme-check
  warns that the route does nothing. Modulating around 20 Hz is never what was meant.
- Display: `Unit::HzNative` (`fmt_hz`: `109.5Hz`, `110Hz`) and `units::beat_band`. The band names are
  delta < 4, theta 4–8, alpha 8–13, beta 13–30, gamma ≥ 30.

**D4 — Never drifted: `ParamKind::Fixed`.**
- The beat rate is the point of the instrument, so drift must not wander it.
- `ParamKind` documents how the controller may perturb a param. `Fixed` means "never, only the author
  and routes move it", and `root_hz`/`spread_hz` are its only rows (pinned by a test).
- `dither_candidates` skips `Fixed` params, and the depth of any route into one. Otherwise drift would
  still move the beat, through the depth.
- Docs without these params get the same candidate list as before, so the drift RNG makes the same
  draws and the `session/drift*` goldens hold.
- The pair's other params (level, cutoff, …) drift like any subtractive voice's. Categorical drift can
  flip `wave` from sine to a richer shape, whose harmonics beat at k · spread. Use `dither_disable`
  for a bed that must stay pure.

**D5 — Outside the master width; stems agree.**
- The pair's dry sums into its own accumulator. That accumulator joins the master **after** the
  mid/side stage and before `master_gain`, so `stereo_width` 0, 0.5 or 1 renders the pair bit-for-bit
  the same, and `master_gain` still scales it.
- **Stems:**
  - A per-source stem (ADR 0030) of a binaural instrument gets the gain only.
  - An `out_channel` stem (ADR 0027) gets the pair's dry added after its width stage.
  - Write W for the linear mid/side map and g for the gain. The master is
    W(Σ other dry + Σ wet)·g + Σ pair·g, and each stem is W(its others)·g + its pairs·g. So
    `rest + Σ stems` and `Σ sources` still rebuild the master up to float association.
- **Determinism guard:** when the doc has no binaural instrument, every path is the old one verbatim.
  The bypass sum is added only when one exists, because `x + 0.0` would turn a −0.0 into +0.0 and move
  the golden bits.

**D6 — Sends are allowed, and warned about.**
- A send works as on any instrument. But the bus blends the two sides, so the wet part beats in the
  room, not between the ears.
- sme-check warns on each **authored** send. A legacy doc's backfilled reverb send isn't the
  author's, so it isn't warned.
- The `binaural` part ships dry, so isolation is the default.

**D7 — Tooling.**
- `param_applies("binaural", …)` = the subtractive non-noise set + `root_hz` + `spread_hz`.
  Completion, route dests and the no-effect warnings follow from it.
- sme-check also warns when `spread_hz` is absent or 0 and no route moves it (no beat).
- sme-lsp:
  - hover on `root_hz` shows its note and both sides (`A2 -8¢ · 109.5Hz — L 108.5Hz | R 110.5Hz`);
  - hover on `spread_hz` shows its band (`4Hz beat · theta (4–8 Hz)`);
  - hover on a route `depth` into either shows Hz;
  - inlays name the note after `root_hz` and the band after `spread_hz`.
- Parts library: `binaural`. It's a held root two octaves down (Orrery `context`, never moved), a
  sine pair at `spread_hz: 4` (theta), low cutoff, no send, and its level frozen when the doc freezes
  levels.

## Consequences

- Listening needs headphones, as all binaural beats do. In the room the pair is a gentle stereo drone.
- A live session keeps the separation: the live-only protection limiter is channel-linked (the same
  gain on L and R), not mid/side.
- In Ableton (ADR 0044) the instrument's own stereo pair carries it. Keep that track's width at 100 %
  and don't sum it to mono on the way to the master.
- Each binaural voice costs one more oscillator (two with the pedal) and one more SVF. Docs without
  the type pay nothing.
- Snapshots stay backward-compatible: `SynthVoiceState::None` is still skipped. A snapshot holding a
  Binaural state can't be read by an older build, which is acceptable since snapshots are
  forward-only.

Tests:
- `synth::tests`: the pair and its 0 Hz floor, `binaural_root` exactness, each side's frequency, the
  pedal's shared spread.
- `engine::tests`:
  - L 109 / R 111 for 110 / 2, and each side holds only its own tone;
  - bit-identical at `stereo_width` 0, 0.5 and 1, with `master_gain` still applied;
  - `root_hz` makes the note gate-only, and without it the root follows the note;
  - an idle route reads the authored Hz exactly, and a route into an unset root does nothing;
  - the pedal at L 210 / R 230;
  - snapshot round trip and resume;
  - per-source and `out_channel` stems stay hard L/R and rebuild the master;
  - a mid-loop retype to binaural.
- `check::tests`: the param table, ranges, the noise error, every advisory.
- `controller::tests`: the Hz params and their route depths are never candidates.
- sme-lsp: hover, inlay and completion. Parts: the new part merges into every house piece and renders
  without clipping.
- Fixture `loop_binaural_basic.json`, in the block-split, playback-headroom and seam-determinism
  suites. New goldens `engine/binaural x2` and `session/stems/binaural x1`. The seven existing
  hashes are unchanged.
