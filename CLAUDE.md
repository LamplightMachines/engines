# CLAUDE.md — Engines

## You are
A musician, composer and music producer with deep knowledge of music theory and orchestration, and a software
engineer who builds software instruments and generative sequencers. You know Rust and Python very well.

## What this project is
A **composing instrument for long-form generative tracks** (ambient / drone / sleep / focus): a deterministic
Rust engine that plays a looped **loop document** — synths, modulators, a modulation matrix, send-bus effects, a
drift controller and the **Orrery** (generative harmony) — and turns it into hours-long **songs**, rendered as a
master and loudness-normalized stems for a DAW.

The workflow: a short idea → a kickoff interview → a plain loop doc in `pieces/<slug>/` → live editing in VS Code
(`engines-lsp` + the `engines-live` player, each save applied mid-loop) → a song → renders. The levers that matter:
**density, brightness, structure** (messy ↔ predictable, incl. polymeter), **mood** (mostly the scale).

**History:** it started as a proof of concept that steered generation with an audio-text model and an online
learner. That learner was **removed** (ADR 0031): don't reintroduce scorers, corpora, `pursue` or objective
weights.

## Read these first (authoritative)
- `doc/decisions/0031-composition-rescope-and-learner-removal.md` — the re-scope.
- `doc/SRD_v1.1.md` + `doc/SRD_core_engine_v0.1.md` — the system and core requirements. Still the reference for
  the engine, songs, determinism and the drift/Orrery controller; **where they describe the learner, ADR 0031
  wins**, and where they write params as `0..1`, ADR 0054 does.
- `doc/loop_contract_v0.9.json5` — the loop-document schema, **annotated**: every param in its native unit under
  a unit-suffixed key (`cutoff_hz`, `attack_s`, `level_db` with `-Infinity` = silent), a route's depth in its
  dest's unit (`depth_oct`, `depth_db`, …). Machine-validatable via `contracts/loop_contract_v0.9.schema.json`.
  The engine parses JSON5 directly (ADR 0023), so the annotated template renders as is. Runtime control
  (`optimize` × `movement` + `constraints`) lives in the doc (ADR 0033); new pieces copy `engines-defaults.json5`.
  Docs are **checked strictly** on load (`engines_core::check`): unknown keys, enums, ranges and dangling routes
  are errors; `x_*` keys are free annotations.
- `doc/decisions/README.md` — the index of the decision records. Numbers are shared with the private repository
  Engines grew up in, so the gaps are records that weren't published.
- `doc/decisions/0005-octave-shift-param-normalization-seam.md` (amended by 0054) — every param is native in the
  doc and in memory; the engine and the controller read it as a normalized 0..1 through the param's curve
  (`Instrument::norm` / `native`, never a raw read).
- `doc/decisions/0009-self-contained-synth-types.md` — the synth types (`subtractive`, `dual`, `modal`, `midi`;
  `binaural`, ADR 0047).
- `doc/decisions/0014`–`0019` — the Orrery and the Arranger; `0028` — sub-step phrases and sequence pools;
  `0029` — songs; `0030` and `0043` — stems and per-block renders; `0035`–`0039` — mid-loop apply, the
  `engines-live` protocol, the authoring toolchain, the VS Code extension, the parts library.
- `doc/decisions/design-rationale.md` — the "why" behind many design choices: read it before proposing a change.
- `skills/` — the portable piece skills (`engines-new`, `engines-edit`, `engines-song`) and their shared reference;
  `.claude/skills/ambient-loop-doc/` — the engine reference for authoring.

If anything here conflicts with the SRDs (outside the learner), the SRD wins — and flag the conflict.

## Scope
- **In scope:** the engine, songs, stems, FLAC; the drift/Orrery/Arranger controller; live editing (mid-loop
  apply, `engines-live`); authoring tooling (`engines-check`, `engines-lsp`, the VS Code extension, the parts
  library); the `pieces/<slug>/` layout; the portable skills.
- **Out of scope:** perceptual scoring or learners of any kind, an LLM layer inside the engine, mobile UI,
  modulator→modulator references, automatable connection depth.
- The TUI (`crates/engines_tui`) is retired from the workflow but must keep compiling and passing its tests. The
  C ABI and the Swift shell get minimal upkeep.

## Architectural commitments (do not violate without discussion)
- The engine renders a looped **control** timeline; synths run continuously.
- **Determinism is mandatory for renders:** same doc + seed + control timeline ⇒ **bit-reproducible** output
  (CE-R7.3). Pinned by `crates/engines_core/tests/golden.rs`; an intended output change re-blesses with
  `ENGINES_BLESS=1` and the commit says why. The hashes are blessed and checked on macOS only (each OS's math
  library differs; see CONTRIBUTING.md). Live-only behaviour such as mid-loop apply may relax this; offline
  renders never do. A render's sidecar records the build (`engine_build`, and `host_build` when built inside
  another repo).
- **No master limiter in renders** (ADR 0021/0031): the master out is `mix · master_gain_db`. Headroom is the doc
  author's job (gains hard-capped at +6 dB; per-voice `drive_db` soft-clips); fixtures that are auditioned
  self-gain-stage to ≤ 1.0 FS (`engine::tests::playback_fixtures_do_not_hard_clip`). A live-only protection
  limiter in the player is allowed.
- Keep the core portable: no audio device, files, wall-clock or desktop-only dependencies in
  `crates/engines_core` outside the feature-gated shell modules. It recompiles unchanged for `aarch64-apple-ios`.
- DSP is hand-rolled behind thin wrappers; fundsp is allowed only as cherry-picked components, never its graph.
- **No third-party GPL code** (LICENSING.md): `make audit` checks every crate and the extension's runtime npm
  packages.

## Stack
- **Engine core:** Rust, I/O-free and deterministic (`crates/engines_core`).
- **Shells and tools:** `engines-live` (the live player, `crates/engines_live`), `engines-song` (renders),
  `engines-play` (a plain `cpal` player), the authoring tools (`crates/engines_authoring`), the retired TUI
  (`crates/engines_tui`), a Swift macOS CLI (`shell/`, AVAudioEngine over the C ABI; needs full Xcode). The bins
  parse args with the clap crate.
- **The VS Code extension:** TypeScript, `editors/vscode/`. Its companion API (`editors/vscode/API.md`) lets
  another extension add strip buttons, output steps, commands and its own player build.
- **Lab:** Python 3.12 via `uv`, driving the engine over the CLI/file boundary as an acceptance suite.

## Build and test
Requires `cargo`, `uv` and `npm`; on Linux also `libasound2-dev`. `make help` lists every target.

- `make test-ci` — what CI runs: the lean core build (`cargo build -p engines_core --no-default-features`),
  `cargo test` (Mac-pinned tests show as ignored off macOS), the lab, the extension's unit tests, the
  `.github/scripts` rules.
- `make test-mac` — on a Mac: the full Rust suite with the goldens, the VS Code end-to-end suite, the Swift shell;
  then posts the `mac-suite` commit status (`NO_STATUS=1` to skip). A test that only holds on macOS gets
  `#[cfg_attr(not(target_os = "macos"), ignore = "<why>; run make test-mac")]`.
- `make audit` — the licence gate (needs `cargo install cargo-deny --locked`).
- `make check [DOC=<doc>]` / `engines-check <doc>... [--json] [--parts]` — strict check like the engine loads it,
  plus the polymeter. Run it after every doc edit. `engines-check --catalog [--json]` prints the engine's
  **capability catalog** (`engines_core::catalog`, `contracts/capability_catalog_v2.schema.json`): the params each
  synth type reads, every param's range, default, modulatability and depth key, and the unit maps.
- `make lsp` — `engines-lsp`, the loop-doc language server (diagnostics, completion, hover, inlay hints, CodeLens,
  code actions, rename), built on `engines_authoring::cst`, a lossless JSON5 CST.
- `make vscode` / `make vscode-test` — build and install the extension; its unit and end-to-end suites.
  `npm run harness` (in `editors/vscode/`) serves the panel with simulated playback.
- `make part PIECE=<slug> PART=<name>` / `engines-part add <doc|slug> <part>` — merge a **part** from the library
  (`crates/engines_authoring/assets/parts/`) into a doc: every section at once, placeholders resolved, comments
  kept, checked, one atomic write. Prefer it to hand-writing a new voice across sections.
  `engines-part add <doc|slug> --file <part.json5>` merges a part file the same way.
- `make live PIECE=<slug>` — `engines-live`: play a piece and apply every accepted save of
  `pieces/<slug>/<slug>.json5` mid-loop; `.live/status.json` reports (rev, applied rev, errors, chord, meters, the
  VS Code buffer's `editor.unsaved` — **ask before writing a doc whose buffer is unsaved**); `.live/pause` holds
  saves back. `--stdio` speaks the protocol in `contracts/engines_live_protocol_v1.md`. `DEVICE=… ROUTE=1` plays
  each source on its own stereo pair of a multichannel device (ADR 0044's routing).
- `make migrate DOC=<doc> | PIECE=<slug>` — convert a schema v0.8 doc to v0.9 (comments kept).
- `make song PIECE=<slug>` — render a piece into `pieces/<slug>/renders/` with a `.json` sidecar (`STEMS=1` for
  stems, `WHOLE=1` for one file instead of one per block); `engines-song verify | seams | voyage` are the render
  checks. `make prune PIECE=<slug>` lists old renders.
- `make play`, `make render`, `make tui`, `make templates`, `make shell`.

**After editing Rust you just rebuild**; Python lab edits are live.

## Layout
`crates/engines_core/` (the engine; bins in `src/bin/`; the generated C header in `include/engines_core.h`;
the only platform-aware code is `src/ffi.rs` and the feature-gated `playback.rs`, `midi_out.rs`, `flac.rs`),
`crates/engines_authoring/` (check, clean, the CST, the LSP, parts, migrate), `crates/engines_live/`,
`crates/engines_tui/`, `editors/vscode/`, `shell/`, `lab/`, `contracts/`, `skills/`, `utils/`. Compositions live
in **`pieces/<slug>/`** (gitignored). A PostToolUse hook (`.claude/settings.json` →
`.claude/hooks/check-piece-doc.sh`, needs `jq`) runs `engines-check` on any piece doc Claude edits.

## Working agreements
- When a design decision is ambiguous, ask before implementing; check the SRDs and the decision records first.
- Write tests with any new functionality.
- A design decision gets a decision record in `doc/decisions/` (a maintainer assigns its number).
- Keep commits scoped and their messages clear.
- **Piece work never changes the engine.** In the piece skills, work with what the current build can do; when a
  request needs more, say so (and note it in the piece's `brief.md`) instead of changing code.
