# ADR 0034 — Pieces as projects, render bookkeeping, `sme-live` v0, workflow skills

Status: accepted (built 2026-10-09)
Relates to: ADR 0031 (re-scope — the composition workflow), ADR 0033 (control in the doc, strict
checking, `sme-check`), ADR 0029/0030 (songs, stems), ADR 0024 (live-edit doc swaps). Followed by
P4 (mid-loop apply, ADR 0035) and P5 (the `sme-live` protocol, ADR 0036).

## Context

ADR 0031 set the workflow: idea → interview → a plain loop doc → live editing (the user in VS Code,
Claude through its edits) → song → renders. Until now a "piece" was wherever its doc happened to sit
(`lab/tests/fixtures/`, `sandbox/`, `renders/`), renders weren't tied to the doc they came from (the
published `idle-night-v2` render didn't record its flags, and the doc was edited afterwards), the only
live player watched an objectives doc rather than the loop doc, and nothing guarded a typo'd save.

## Decision

**D1 — A piece is a self-contained, gitignored project.** `pieces/<slug>/` holds `<slug>.json5` (the
one doc; the song is added to it in place), `brief.md` (intent, levers, voice sheet, song plan,
renders, changelog), `renders/`, and `.live/` (written only by the tools). `sme_core::piece`
(feature `pieces`, desktop file I/O like `flac`) owns the layout. A doc path `pieces/<slug>/<slug>.json5`
is recognised as a piece by every tool.

**D2 — Revisions are content-addressed history snapshots.** `.live/history/r<NNNNN>-<hash8>.json5` +
`index.jsonl` (`{rev, hash, file, at, source}`). Snapshotting text identical to the latest snapshot
returns that revision; anything else is the next number (history is a timeline — returning to old
text is a new revision). All revisions are kept. `sme-live` snapshots every accepted save; `sme-song`
snapshots what it renders — so both agree on numbering and every render names the exact doc.

**D3 — Renders land in the piece with a sidecar.** `sme-song --piece <slug>` (or a piece doc path)
with no `--out`/`--stems DIR` writes `renders/<local-date>_r<rev>.flac` (s24; never overwriting —
`_2`, `_3`…), a bare `--stems` writes `<date>_r<rev>_stems/`. Each gets a `.json` sidecar: piece, rev,
doc hash, snapshot path, kind, seed, control, max_change, sample rate, format, duration, frames, peak,
clip count, integrated LUFS (BS.1770 on the master — now also on every `done:` line), argv, engine
version, `published: false`. A dry run predicts the revision and writes nothing.

**D4 — `sme-clean` is conservative.** Dry run unless `--yes`; keeps the newest N (default 3) and any
sidecar marked `published`/`keep`; never touches a render with an Ableton `.asd` sibling or anything
without an `sme-song` sidecar (hand-made / legacy renders are "left alone").

**D5 — `sme-live` v0** (`crates/sme_live`): plays a piece and applies every accepted save of its doc.
- File watch by polling: the player's main loop polls the doc on every pass (it sleeps 20 ms, 5 ms
  with `--fast`; each poll is one `metadata()` call, so it's cheap — the file is read only once a
  change has settled); a save is applied once the file has been quiet for 300 ms and skipped when
  the content didn't change; `.live/pause` holds saves back so a multi-step edit lands as one
  revision.
- Each save is strictly checked (ADR 0033): errors ⇒ **refused**, the last good revision keeps
  playing, `status.json` lists the errors; accepted ⇒ snapshot + `Session::apply_doc_rev`.
- `LoopRecord.doc_rev` (new, serde-skipped when 0) carries the shell's revision through the worker,
  so the status reports which revision is actually playing and from which loop (`applied_rev`,
  `applied_at_loop`). `SessionConfig::doc_rev` sets the starting one.
- `.live/status.json` (atomic writes, ≤ 4 Hz): piece, pid, state (`playing`/`paused`/`stopped`/
  `finished`), rev, applied_rev, rejected, warnings, loop, chord, tension, peak, limiter GR, NaNs muted,
  underruns, updated_at (readers detect a dead player by staleness). SIGINT/SIGTERM ⇒ `stopped`.
- Saving a different `optimize` (`hold` ⇄ `drift`) switches the mode at the same loop top — the
  interim fine-tuning toggle (a CLI `--optimize` only sets the start; the P5 `control` command and the
  P7 panel toggle drive the same thing). `status.optimize` reports the mode playing.
- (v0) Edits applied at the **next loop top** (the whole-loop session) with a 4 s ring — superseded by
  ADR 0035: live mode applies them mid-loop with a ~170 ms ring. Originally
  (`--ring-secs`) — worst case ≈ loop length + ring. Mid-loop apply is P4.
- A doc's `song` isn't played live (it would ignore edits, ADR 0029) — the top-level matrix is; the
  status says so. Block audition is P5.
- `--headless [--fast]` drains without a device (tests, monitoring).

**D6 — A live-only protection limiter** on the device output (new `playback::OutputPost` hook,
`start_output_stream_with`): stereo-linked, 1.5 ms lookahead, −1 dBFS ceiling, 100 ms release, NaN/Inf
muted and counted. Correct by construction — each output frame's gain is a box average of sliding
minima that all include that frame's own target, so the ceiling holds without clipping; below the
ceiling it is a bit-exact 1.5 ms delay. Renders and stems never pass through it (ADR 0021/0031).

**D7 — Claude's edits are checked as they land.** A project PostToolUse hook
(`.claude/settings.json` → `.claude/hooks/check-piece-doc.sh`) runs `sme-check` after any
Edit/Write of a piece's doc: errors come back to Claude (exit 2) — the same refusal sme-live applies —
as does an unreadable doc; warnings come back as context, other files are ignored. It's never silent
about a skipped check: if `sme-check` isn't built and can't be built (or can't run), a context note
says the edit went unchecked. The hook needs `jq` (without it, a piece-doc edit is reported unchecked).

**D8 — Skills v1.** `piece-new` (question-card interview until the roster is locked → layout with the
root defaults copied in → check + proof render → ask, then start sme-live), `piece-edit` (levers →
edits as one revision → confirm `applied_rev` → changelog), `piece-song` (interview → matrix-only
feasibility → song in the same doc → verification → confirmed render into `renders/`), `house-style`
(list / add a reference snapshot + traits note). `ambient-loop-doc` stays the engine reference and
house-style recipe they build on.

## Consequences

- A render is reproducible from the piece alone: sidecar → snapshot + seed + control.
- `make song PIECE=<slug>` / `make live PIECE=<slug>` / `make prune PIECE=<slug>` are the everyday
  commands; `make check` covers the tracked docs.
- The device path of `sme-live` is the same cpal shell `sme-play` uses plus the limiter; its tests run
  headless (the limiter is unit-tested on buffers). The first real-speaker run is the user's.
- Latency is still loop-bound in v0; P4's block rendering removes that without changing the files,
  status fields or skills.
