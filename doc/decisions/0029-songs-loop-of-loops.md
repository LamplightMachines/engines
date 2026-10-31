# ADR 0029 — Songs: a "loop of loops" for finite, hours-long tracks

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** a layer overrides a depth with its dest's depth key, so D1's `{depth: 0.3}` into a cutoff is `{depth_oct: 1.99}`. D2's per-block trim is a real offset now: the `unity` route at `depth_db: -6` plays that block 6 dB down. In Known gaps, Idle's `master_gain` 0.36 and 0.22 are `master_gain_db` −8.87 and −13.15 dB, and `max_drift: 0.10` stays a fraction. `global.reverb_mix` is gone.

Status: accepted (built 2026-10-04)
**Amended** 2026-10-27 by ADR 0055: D2 — a block may also carry `control` (the Orrery off or retuned, hold or
drift, per-param drift overrides), for that block only.
Relates to: ADR 0010/0011 (safe bounds, objectives doc), 0012 (pursue gate), 0015 (Orrery home),
0018 (the Arranger), 0021 (no master limiter), 0023 (JSON5), 0024 (LiveEdit doc swaps), 0025
(`max_drift` anchors), 0028 (phrase pools); `design-rationale.md` "synths run continuously".

## Context

The system targets hours-long sleep/focus sessions, but a loop doc described one looped control
timeline that ran open-endedly (`sme-play --loops -1`). There was no notion of form: no sections,
no ending. The goal is **finite 5–6 h tracks** built as a sequence of configurations:

- a loop doc may contain a **loop of loops**: the song moves through a sequence of **matrix
  configurations**, each lasting a set number of inner loops;
- the song **ends** when the sequence completes;
- the **Orrery** and **drift/pursue** reset at the start of each block.

Nothing could produce such a file, either: `wav::write_wav_f32` wrote the whole buffer with u32
RIFF sizes (overflowing at ≈ 3.1 h of 48 kHz stereo f32), and `sme-run`'s `run_collect` buffers all
audio and samples in RAM (a 6 h f32 render is ≈ 8.3 GB).

## Decisions

**D1 — `song` is an optional top-level loop-doc field, additive at schema v0.8** (the
0025–0028 precedent; no version bump). Shape:

```json5
song: {
  matrices: { <layer>: { <route id>: <partial matrix entry> | null, … }, … },
  sequence: [ { matrix?: <layer>, loops: N, seed?: S, label?: "…" }, … ],
  transition_s: 8,   // seam glide (0 = hard cut); ≤ the shortest block
  tail_loops: 1,     // release/ring-out loops after the last block
}
```

A block's matrix is the top-level `matrix` with its layer merged over it by route id, **RFC 7386
JSON merge patch** semantics: a field overrides (`{depth: 0.3}`), `null` removes a route, a full
entry adds one. Shared routes live once in the base matrix; layers say only what differs. A block
without `matrix` plays the base matrix as-is.

**D2 — Blocks are matrix-only**, enforced by `#[serde(deny_unknown_fields)]` on `SongSpec`/
`SongBlock` (and the schema): there is nowhere to put a per-block instrument / modulator / harmony /
transport / global change. Consequence: the instrument and modulator sets never change, so **one
engine runs the whole song** — voices, filters, reverb tails and free-running phases cross every
seam (the "synths run continuously" commitment holds at block scale too). A per-block *offset* of a
base value is still expressible: route a one-step control sequence at value 1.0 (`unity`) at
`depth = offset` in that block's layer (the demo song trims the crank this way).

**D3 — The song lives above the engine and controller.** `song.rs` is pure (no I/O):
`materialize` (the same serde round-trip `with_param` uses, `song` stripped), `validate` (every
error names its block/layer/route — unknown layers, zero-length blocks, seams longer than the
shortest block, incomplete new routes, typo'd patch fields, dangling sources/dests; each
materialized block must pass `LoopDoc::validate`), `lint`, `SongClock`, `plan`, `seam_between`, and
`SongDriver`. `Session::start` takes the song into a `SongDriver`; the controller and engine only
ever see plain per-block docs (`Controller::new`/`swap_doc` also strip `song` defensively — the
operating point is serialized into every Sample and round-tripped by every perturbation). The run
ends at the song's total (`max_loops` can only cut it shorter).

**D4 — Block reset = a fresh controller on the block, except `pair_id` and the session
control.** `Controller::begin_block(doc, seed)` rebuilds via `Controller::new` + `set_control`,
which is proven (by test) equivalent to a session that began on that block: operating point back to
the authored block (drift travel undone, safe-clamped), drift streak cleared, `max_drift` anchors /
candidates / Orrery home recaptured, pursue gate → `Explore` with empty plateau windows, overrides
re-applied, and the **Orrery rebuilt from home** (fresh `Voyager` at the tonic, phrase clock and
Arranger reset — so phrase boundaries align to block starts). Kept: `pair_id` (corpus ids stay
unique and monotonic) and the standing `SessionControl` (incl. live objectives edits). **The RLS
learner persists** — it lives outside the controller, φ is fixed-width (`feature::DIM`), and stats
are keyed by path, so new routes simply start without evidence; later blocks pursue smarter (the
ADR 0024 "learner preserved across swap" precedent).

**D5 — Seeds.** Block 0 runs on the session seed **unchanged** (so a one-block song is
bit-identical to a plain session — a strong regression anchor, pinned by test); block k > 0 on
`hash64(session, k, SONG_SEED_SALT, 0)`; an explicit block `seed` on `hash64(session, seed,
SONG_EXPLICIT_SEED_SALT, 0)` — blocks sharing an explicit seed replay the same voyage (a recap),
while `--seed` still varies the whole song. Note the block seed re-keys *every* `DitherConfig.seed`
site: drift picks, directed explore, categorical moves and all Orrery draws. Engine rolls stay keyed
on `loop_id`, which keeps counting globally across blocks.

**D6 — The seam glides, carried on the `PhaseSnapshot`.** At a block start the driver builds
`seam_between(outgoing operating point, new block's operating point, transition_frames)` and puts it
on the snapshot (`PhaseSnapshot.seam`, serde-skipped when `None` ⇒ every non-song snapshot and
render is byte-identical). The next render(s) consume it; everything decays by one raised-cosine
gain `g` (1 → 0 over `transition_s`):
- `param_offsets` (`old − new`, normalized) enter the existing modulation path as a synthetic
  contribution `off · g`, so a param starts where the old block left it (drifted value) and eases to
  the new authored value. Contributions land **only on the `CONTROL_HOP` grid** (the engine sets
  `target = base + Σcontrib` at those exact frames; off-grid events would wipe the routes' sums).
  Unmodulated params get a slot appended to `index_of` before `InstBase`/bus/CC routing reads it,
  and seam-touched slots start **at** their frame-0 value (no 64-sample blip from the new base).
- `depth_from`: a kept route whose depth changed glides from its old depth; a new (or re-targeted)
  route fades in from 0 (overriding the per-loop `depth_slew_ms`).
- `fading_routes`: removed (or re-targeted) mod routes keep rendering at `depth · g` until zero.
- `release_orphans`: at the seam's first frame every carried voice whose `(instrument, source)` note
  route no longer exists gets a note-off (its own release envelope + filter play it out; MIDI
  instruments get a Note-Off). Previously such voices sustained forever — voices are carried from
  the snapshot regardless of routing.
- A glide longer than one loop carries via `elapsed_frames`; validation keeps seams from overlapping.

Why the snapshot: `(doc, snapshot)` must fully determine a loop (CE-R7.3), and the −δ counterfactual
renders from a clone of the same snapshot — so the seam is **common-mode to the antithetic pair**
and ΔCLAP stays valid. It is also recorded in every corpus `Sample.phase_snapshot`. The depth glide
is a one-off scalar slew like `depth_slew_ms`, not depth-as-a-modulation-destination (out of scope,
SRD §12).

**D7 — Learning rules.** A seam loop still emits its antithetic pair, flagged `meta.song.seam`, but
never reaches `learner.observe` or the pursue gate's `observe_outcome` (the glide moves the sound
for reasons other than the perturbation). The **tail** (`Controller::begin_tail`: note routes
dropped, the engine releases the orphans, `next` returns the unperturbed operating point,
`scores()` false) emits nothing. `SampleMeta.song = {block, seam}` (absent outside songs); the lab
harness skips seam pairs on re-render.

**D8 — LiveEdit is disabled during a song.** The song owns the matrix, and a structural swap's
crossfade loop would skip the song hook; the session drops pending doc swaps with a one-shot warning
(and the TUI says so).

**D9 — Output: `sme-song` + a streaming RF64 writer.** `wav::WavWriter` streams, checkpoints its
header every ~10 s (a killed render is still a valid file), and upgrades in place to **RF64**
(EBU Tech 3306: the 28-byte `JUNK` placeholder becomes `ds64`) only when the file outgrows 4 GiB;
`f32` verbatim (no clamp, ADR 0021) / `s24` / `s16` with hashed (deterministic) TPDF dither.
`write_wav_f32` stays byte-identical but refuses past the RIFF limit. `sme-song` (default `cli`
feature, no cpal) runs a `Session` headlessly — the exact worker path live playback uses — and
drains the ring into the writer as fast as the worker renders (≈ 6–7× realtime for the demo with
drift + Orrery, so 5.5 h ≈ 50 min); `--dry-run` prints the block plan, `--emit-blocks` writes each
materialized block for per-block verification; the summary reports peak/clip count and warns if the
tail didn't ring out. `sme-run` refuses song docs; `sme-render`/`sme-sweep` note they play the
top-level matrix. `sme-play` now plays out the ~20 s ring at the end of a bounded run (it used to
sleep 1.5 s and drop the stream — cutting off every ending) via `Session::frames_produced()`.

**D9a — FLAC output (amendment, 2026-10-08).** An `--out` ending in `.flac` streams lossless FLAC
through `flac::FlacWriter` (feature `flac`, on by default; the lean core build doesn't carry it, and
a `--features cli` build of `sme-song` still renders WAV but refuses a FLAC request).
The encoder is the pure-Rust `flacenc` crate (default settings ≈ `flac -5`) driven one fixed
4096-frame block per call, so memory stays flat over 6 h and no RF64 analogue is needed (36-bit
sample count). FLAC is integer-only: `.flac` defaults to `s24`, takes `s16`, and **refuses `f32`**
(an explicit f32 would silently lose the overs ADR 0021 keeps). Both writers quantize through the
one `wav::quantize` (incl. the hashed s16 TPDF dither), so a `.flac` decodes bit-for-bit to the
same-format `.wav`. The 42-byte STREAMINFO is rewritten at each ~10 s checkpoint (sample count +
frame sizes, MD5 unset since frames follow) and at finalize (with the stream MD5 `flac -t` checks).
Measured on a 10 min drift + Orrery render: 51 % of the s24 PCM size, no realtime cost (encoding is
far faster than the worker); ffmpeg's decode, the s24 WAV and the STREAMINFO MD5 all agree.
Rejected: shelling out to `flac`/`ffmpeg` (an external runtime dependency) and a hand-rolled
fixed-predictor encoder (worse compression, more code to own, for a dev-side output path).

## Alternatives rejected

- **A separate song document wrapping loop docs** — the user asked for the loop doc itself to hold
  the form, and matrix-only blocks don't need whole docs per block.
- **The LiveEdit structural path (fresh engine + 0.4 s crossfade)** — loses reverb tails, filter and
  voice state at every seam, and has no param glide; the matrix key-set change would always trip it.
- **The seam as an `Engine` field** — hidden state outside the replayable snapshot.
- **Synthetic glide routes injected into the ±δ docs** — they'd become dither candidates and pollute
  the corpus operating point.
- **Resetting the learner per block** — blocks would each re-explore from zero; the learned
  param→term structure is block-independent.

## Known gaps

- Params the engine doesn't read through a modulation slot **step** at a seam: categoricals
  (`wave`, `enabled`), `voices_detune`, `glide_ms`, `reverb_mix`, bus `width`.
- The pre-existing 64-sample restart-from-base ramp at **every** loop start for modulated dests
  (not introduced here; a separate fix — it changes bytes).
- No per-block transport or harmony (matrix-only by decision). ~~A block can't retune the Orrery's movement
  settings.~~ **Since ADR 0055** a block's `control` can turn the Orrery off, retune it, hold, and override drift
  per param.
- `sme-render`/`sme-sweep` ignore the song (use `sme-song --emit-blocks` to audition a block);
  `sme-run` refuses it. The TUI's param rows come from the top-level doc, so routes that exist only
  in a layer have no rows.
- Instruments with ~zero `release` that lose their route at a seam stop dead (linted, not fixed).
- **Gain staging must survive hours of drift, not just a block start.** The first full render of the
  demo clipped (2580 samples in the pulse blocks): over hundreds of loops drift + Orrery transients
  reached ~3 dB above the start-of-block peak. The demo now caps every level-ish lever at its
  authored value with `safe_bounds` (which pins it, so drift never makes it hotter) and keeps ≥ 3 dB of
  start-of-block headroom (pinned by `song_fixture_blocks_do_not_hard_clip`); the full 5:30:11 render
  is then clean (peak −0.4 dBFS). A song-level "loudness never drifts up" option may be worth adding.
- ~~The house-style "Idle" doc authors `master_gain` 0.36 — below the Tier-2 safe floor 0.4 — so
  under any controller it is clamped **up** (+0.9 dB) and can clip under drift.~~ **Fixed
  (2026-10-05):** Idle now authors 0.22 at the top of a `safe_bounds` band plus the song's level
  caps and `max_drift: 0.10`. A continuous session never gets the per-block reset, and over 2 h of
  drift + Orrery a master-only band crept +6.3 dB and clipped; with the full recipe it was +4.6 dB,
  peak −0.5 dBFS. Six other playback fixtures had the same sub-floor gain (clamped up to 1.12–1.41
  FS); each now authors a band at its own value. `playback_fixtures_do_not_hard_clip` also renders the controller's operating point
  and fails if it plays a doc louder than the doc renders raw.
- A lever capped at its authored value (the song's level caps) is **pinned**, not "free to get
  quieter": drift's ±δ pair is symmetric, so it needs room on both sides of the base value.
