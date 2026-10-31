# Decision records — index

Status view of every ADR and design note in this directory. **This table is the up-to-date
map**; individual ADRs are point-in-time and may be amended or superseded by later ones (each
supersession is also banner-noted in the affected file). Add a row when adding a record.

**Published from a private repository.** Engines was developed in a private repository first, and these records
keep the numbers they had there, so the gaps are records that weren't published: the learner era, the closed
tools and the process. A reference to an unpublished number points at one of them. The records were written
before the rename to Engines, so `sme-*` / `sme_*` names are today's `engines-*` / `engines_*`.

Statuses: **accepted** (in force) · **superseded** (kept for the reasoning record only) ·
**proposed/roadmap** (direction, not yet fully built).

| # | Title | Status |
|---|-------|--------|
| [0003](0003-rust-core-swift-shell.md) | Rust core + Swift shell | accepted |
| [0005](0005-octave-shift-param-normalization-seam.md) | Octave-pedal `shift` + param-range normalization seam | accepted (amended by 0054: every param native, the seam gains curves) |
| [0009](0009-self-contained-synth-types.md) | Self-contained synth types (`dual`, `modal`) | accepted (amended 2026-07: modal loudness norm + `t60_mode:"note"`) |
| [0010](0010-runtime-control-plane-and-orrery-roadmap.md) | Runtime control plane + Orrery v2 roadmap | roadmap (phases delivered by 0011–0019; objective/pursue parts removed by 0031) |
| [0011](0011-two-axis-control-and-objectives-doc.md) | Two-axis control (`optimize`×`movement`) + objectives doc | accepted (amended by 0031: no `objective`/`pursue`/`clap_avoid`) |
| [0014](0014-orrery-decoupled-and-enriched.md) | Orrery decoupled + enriched | accepted |
| [0015](0015-orrery-dynamics-and-home.md) | Orrery dynamics + home | accepted |
| [0016](0016-leader-led-arrangement-chains.md) | Leader-led arrangement chains + organic returns | accepted |
| [0017](0017-axis-seam-and-generative-arrangement.md) | Axis seam + generative arrangement layer | accepted |
| [0018](0018-the-arranger.md) | The Arranger (energy arc: register/density/form) | accepted |
| [0019](0019-narrative-orchestration-and-multitrack.md) | Narrative orchestration (the drop) + multi-track roles | accepted |
| [0021](0021-phase-6d-cleanup-and-solidify.md) | Phase 6d cleanup & solidify + pursue directed exploration | accepted (v0.8 "freeze" later relaxed additively by 0026/0027) |
| [0023](0023-json5-input-and-commented-templates.md) | JSON5 input + commented templates | accepted |
| [0024](0024-live-edit-authoring-and-instrument-viz.md) | Live-edit authoring + instrument viz | accepted |
| [0025](0025-session-cumulative-max-drift.md) | Session-cumulative `max_drift` cap | accepted |
| [0026](0026-midi-out-instrument-type.md) | `midi` instrument type (MIDI-out + DAW return) | accepted (audio-mirror amendment superseded by 0027; the DAW return is monitor-only since 0031) |
| [0027](0027-output-redirect-and-instrument-stems.md) | Output redirect + per-instrument `out_channel` stems | accepted |
| [0028](0028-sub-step-phrases-and-sequence-pools.md) | Sub-step timing (`t`), sequence pools + the phrase arm, swing/humanize | accepted (amended 2026-07-09: plain-render chord-fit snaps to the authored scale; 2026-10-10: `fit: "arp"` keeps an arpeggio's contour on each chord) |
| [0029](0029-songs-loop-of-loops.md) | Songs: a "loop of loops" — matrix-only blocks, per-block controller/Orrery reset, glided seams, `sme-song` + RF64 | accepted (amended 2026-10-08: D9a FLAC output) |
| [0030](0030-loudness-normalized-stem-export.md) | Loudness-normalized stem export: per-instrument + per-bus split, BS.1770 −18 LUFS per stem, one render + float-temp rescale, `sme-song --stems` | accepted |
| [0031](0031-composition-rescope-and-learner-removal.md) | Re-scope to a composing instrument; remove the CLAP learner (scorers, ±δ corpus, RLS, pursue, sweep, harness) — `LoopRecord` stream, golden render hashes | accepted |
| [0032](0032-reflecting-drift.md) | Drift reflects off band edges (no more edge pinning); freezing a lever = a zero-width `safe_bounds` band; amended: a default safe range bounds drift, not the author (`level: 0` is a mute) | accepted |
| [0033](0033-control-in-the-doc-and-strict-checking.md) | Control settings in the loop doc (`optimize`/`movement`/`constraints`, additive at v0.8) + root defaults file; strict checking (`check.rs`, `sme-check`); polymeter analysis | accepted |
| [0034](0034-pieces-layout-and-live-player-v0.md) | Pieces as gitignored projects (`pieces/<slug>/`), content-addressed revisions, render sidecars + `sme-clean`, `sme-live` v0 (watch → check → apply at the loop top, `status.json`, live-only limiter), the check-on-edit hook, skills v1 | accepted |
| [0035](0035-block-rendering-and-mid-loop-apply.md) | Block rendering (`begin_loop` / `render_block` / `finish`, bit-identical), `LoopRender::retarget` for mid-loop edits, retiring buses, live gains, session live mode (fork + equal-gain crossfade, `Applied` / block telemetry); amended 2026-10-10: the Orrery's voyage survives live edits unless the key or the chord/bass carriers change | accepted |
| [0036](0036-sme-live-protocol.md) | `sme-live --stdio` JSON-lines protocol (`contracts/sme_live_protocol_v1.md`): doc / mute / solo / control (hold = as written) / song audition (`loop_block`, `play_from` via `SongDriver::start_at`) / preview; events incl. applied, rejected, tick (steps + meters) | accepted |
| [0037](0037-authoring-toolchain-and-lsp.md) | Authoring toolchain: a lossless, recovering JSON5 CST (`json5`-parity tested; spans, JSONPath ↔ node, comment-preserving edits), theory for the editor (key-aware spelling, degree labels, `chord::identify`, roman numerals; `Chord::label` fixed), units into the core, and `sme-lsp` (diagnostics, completion incl. scale-aware offsets, hover, inlays, CodeLens → `sme-live` commands, code actions, rename) | accepted |
| [0038](0038-vscode-extension.md) | The VS Code extension (`editors/vscode/`): the `sme-loop` language (JSON5 grammar), the `sme-lsp` client (live CodeLens titles, `sme/stepRanges`), one `sme-live --stdio` player (hears each save — amended 2026-10-10, `sme.live.applyOn`; crash restart, retry on save), the live panel (transport, hold ⇄ drift, mute/solo + meters, Orrery chord on a keyboard, song blocks, revisions), status bar, step highlighting; protocol adds `ready.key` + `loop.voicing` | accepted |
| [0039](0039-parts-library.md) | The parts library (`crates/sme_authoring/assets/parts/`): multi-section fragments with placeholders (`$id` made unique, `$bus`, `$target`, `"$deg(N, oct)"`) merged into a doc's text (append-only, comments kept, sections created in place, `safe_bounds` only where the doc freezes, checked); six parts (pulse, drone, partials, halo, breath, shimmer); `sme-part`, `sme-lsp` `source.addPart` actions, the extension's Add a Part | accepted |
| [0040](0040-sme-live-panel-redesign.md) | The SME Live panel redesign (Ableton-style: transport, hold ⇄ drift, mixer strips with stereo meters, song structure + blocks, revisions, collapsible sections, jump-to-source; preview mix or mix + stems) | accepted |
| [0041](0041-pad-register-follows-the-authored-octave.md) | The Orrery's pad register follows the chord track's authored octave (`pad_center` = 7 moved by whole octaves to the authored voicing's mean; default-octave pads bit-identical) | accepted |
| [0042](0042-orrery-note-range.md) | A note range the Orrery must stay inside: `orrery_note_min` / `orrery_note_max` on a note modulator (semitone offsets); whole-surface octave moves that won't fit are skipped, stray notes fold by octaves | accepted |
| [0043](0043-song-renders-per-block.md) | A song renders one file per block (mix and every stem), cut sample-exactly so the files laid end to end are the whole render; `--whole` for one file | accepted |
| [0044](0044-ableton-live-set-and-per-source-routing.md) | Per-source live routing: routed device frames with an overflow residual, the sticky routing map, `sme-live --route-sources`, the sum-aware limiter, the output picker. *Published trimmed: the Live set builder is left out* | accepted |
| [0046](0046-pitched-filtered-noise.md) | Pitched filtered noise: with an explicit `pitched: true`, a noise voice's source band goes on through a band at its note (24 dB/oct either side), `resonance` = a peak at the note at the same loudness, even across notes; unpitched noise bit-identical | accepted |
| [0047](0047-binaural-instrument.md) | Binaural instrument: `type: "binaural"` — two subtractive voices hard L/R at root ∓ `spread_hz`/2 (root = `root_hz` in real Hz, else the note); dry outside the master `stereo_width` (stems agree); `root_hz`/`spread_hz` read exactly, matrix-modulatable, never drifted (`ParamKind::Fixed`); the `binaural` part | accepted (amended by 0054: depth in Hz, not a fraction) |
| [0048](0048-midi-mirror-tracks-for-live.md) | An audio instrument's notes as MIDI (the note tap, keyed by source, audio untouched); phrase-pass marks + a per-loop note log; `sme-song` writes `midi.json` with stems. *Published trimmed: sending MIDI and Live tracks are left out* | accepted |
| [0050](0050-sme-listen.md) | The capability catalog (`sme-check --catalog`) and `sme-part add --file`: the engine helpers an inspiration tool asked for. *Published trimmed: the tool itself is left out* | accepted |
| [0053](0053-sme-midi.md) | Key-exact parts (raw semitone offsets from the root, `part.key` checked at merge), voice-leading in `theory`, and the `bass` and `lead` library parts. *Published trimmed: the MIDI file reader is left out* | accepted |
| [0054](0054-native-units-schema-v0-9.md) | Native units in the loop doc (schema v0.9): time in s/ms, frequency in Hz/cents, every gain in dB (`drive_db` 0–20) with `-Infinity` for silence; unit-suffixed keys; matrix depth in the dest's own unit (reverses ADR 0047's fraction rule); +6 dB hard and 0 dB safe ceilings on gains, a 0.3 drift scale for dB; native in memory, read through the seam's curves (P8 reversed in a later amendment); a clean break, v0.8 known only to `sme-migrate`; provenance (`engine_build`, the `schema-0.8-final` tag) first; goldens re-blessed once | accepted |
| [0055](0055-per-block-control-in-songs.md) | Per-block control in songs: a block's optional `control` patches the session control for that block only — `movement` field by field (`orrery: false` turns the Orrery off), `optimize` hold/drift, `max_drift`, `max_change` (caps only), and per-param `drift` overrides by JSONPath ("hold", a native band, or a normalized `max_drift`) that become `constraints` bands; the block wins at its start, the live panel's switches until the next block; a single-block audition plays it; amends ADR 0029 D2 | accepted |

**Known gaps (deliberate deferrals):** the C ABI (`ffi.rs` / `include/sme_core.h`) does **not**
expose ADR 0026/0027's session options (`out_channels`, `include_midi`) and the Swift shell (`shell/`)
is **compile-maintained only** — no MIDI-out, no DAW-return monitor, no stems, no live control. Both
are parked (minimal upkeep since ADR 0031); extending the ABI is out of scope for desktop work.

## Design notes (not numbered ADRs)

| Doc | What it is | Status |
|-----|-----------|--------|
| [design-rationale.md](design-rationale.md) | The "why" ledger behind the SRD/contract — read before proposing design changes | living |
| [categorical-dither.md](categorical-dither.md) | Categorical A/B dither extension (CE-R8) | implemented 2026-06-21 |
| [orrery-generative-harmony-design.md](orrery-generative-harmony-design.md) | Orrery harmonic-morphing design | direction chosen 2026-06-18; delivered via 0014–0019 |
