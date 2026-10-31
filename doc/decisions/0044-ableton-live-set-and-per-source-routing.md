# ADR 0044 — Ableton Live sets for pieces + per-source live routing (`sme-als`, `--route-sources`)

> **Published trimmed.** This record also decides parts of tools that stay closed (a companion's Ableton Live set builder, `engines-als`).
> Those sections are left out here; the decisions below are the ones the open code implements, word for word.
> It was written before the rename to Engines: `sme` is `engines` throughout (`sme-live` is `engines-live`, `sme_core` is `engines_core`; the routing map moved to the open `engines_core::routing`).

Status: accepted — 2026-10-21.
Relates to: ADR 0027 (output redirect + `out_channel` stems — amended here: a routed session replaces
the doc-driven split for live work), 0030 (the per-source tap and stem names), 0035 (live mode, retiring
buses), 0036 (protocol — additive fields); CE-R7.3 determinism (untouched: offline renders never route).

## Context

The user finishes pieces in a DAW and wants it in the loop while composing: **one DAW track per audio
source**, fed by the engine, with the DAW's master chain applied. The live path is a multichannel loopback
(e.g. BlackHole 64ch: 32 stereo pairs) into the DAW's audio inputs; ADR 0027 already redirects the engine
there. What was missing: a layout that gives **every** source its own pair (ADR 0027 lumps un-routed
instruments and all bus wet on 1/2, and needs a doc annotation per instrument), and an assignment both
sides agree on.

## Decision (the open part)

**D1 — Routed device frames (`sme_core`).** `Session::start_routed(doc, cfg, dither, control,
SourceRouting)` streams `routing.width`-wide frames: stereo pair *k* carries `slots[k]` — an
instrument's dry or a bus's wet, the ADR 0030 tap (post level/pan/mute, through the master's
width × gain stage) — and one **overflow** pair (`residual`) additionally carries
**master − Σ slotted pairs**: a source a live edit added, a source past the device width, a retiring
bus's tail (ADR 0035; `SourceStems::uncaptured` flags it). So the pairs always sum to the master
(float association only), however the doc changes mid-session. The residual is gated: with every
source slotted and no retiring tail, the overflow pair is **exactly** silent and each slotted pair is
its stem **bit for bit**. Width is fixed per session. The live worker resolves slot buffers once per
block. `SessionConfig` stays `Copy`; `start` is unchanged; offline paths never route, so the goldens
are untouched. `SourceRouting::sequential` (sources from 1/2, overflow on the next free pair, sharing
the last pair when full) and `validate` live beside it.

**D2 — The routing map (`sme_authoring::als::routing`).** `pieces/<slug>/ableton/<slug>.routing.json`
(`schema: "sme-routing/1"`) records which pair each source plays on — `slots: [{channel (1-based odd:
3 ⇒ 3/4, the ADR 0027 convention), source ("inst:x" / "bus:x"), name (the stem name), track_id,
stale}]`, `overflow {channel, track_id, shared_with}`, `unrouted`, `retired`, `template`. Assignments
are **sticky**: a source keeps its pair across rebuilds (its Live track listens there), a source that
leaves the doc is marked `stale` with its pair reserved (its track is never deleted), a new source
takes the lowest free pair (then the lowest stale one, retiring it), and the overflow keeps its pair.
`Policy::Grow` (`sme-als`) assigns new sources; `Policy::Frozen` (`sme-live`) reports them as
`unrouted`. Sources are listed in the doc's authored order (`doc_sources`, via the CST's
`member_order`), instruments then buses, MIDI instruments skipped. Phase 2 adds `mode` (`live` /
`render`, what the set was last built as), `render {dir, rev, seed, mix}` (the render laid in),
`faders {source: dB}` (the base each fader was last set to — see D8) and `mix_track_id` (the
reference track). All optional: a Phase 1 map reads as live mode with untouched faders.

**D3 — `sme-live --route-sources[=MAP]`.** Needs a named `--output-device` (or `--headless`). Each
`play` re-reads the map and starts a routed session from it (frozen — the open Live set's tracks
listen on those pairs); without a map the doc's sources play in order from 1/2. A map wider than the
device is refused (an `error`, nothing plays). `ready.routing` / `status.json routing` report the
pairs, the overflow, the `unrouted` sources (recomputed per applied edit — they play on the overflow
until `make als` gives them a track) and `stale`; a changed map on disk raises one `routing` event
("reopen the Live set, then restart") — routing changes apply on restart, never mid-session (Live
must reopen a rebuilt set anyway). Every device stream now checks the session's frame width against
the device's (a mismatch would scramble channels); headless pacing uses the session width.
`make live PIECE=x DEVICE="BlackHole 64ch" ROUTE=1`; VS Code `sme.live.outputDevice` +
`sme.live.routeSources`.

**D4 — The protection limiter detects the downstream sum.** On more than two channels the live-only
limiter's per-frame peak also takes |Σ even channels| and |Σ odd channels| — what Live's Main will
sum — with the same linked gain, so the pairs still add up to the master. Stereo is unchanged (bit for
bit). This also covers ADR 0027's multichannel redirect.

**D7 — Choosing the output in the panel (Phase 1b).** The device is picked in VS Code, not typed into
settings: the live panel's device line (and **SME: Choose the Output Device…**) lists
`sme-live --list-devices --json` (the system output first — it follows macOS's choice each time it's
opened, e.g. after plugging headphones in — then every device with its channels and rate), then, for a
multichannel device, "each source on its own pair" or "the mix". The choice is written to
`sme.live.outputDevice` / `sme.live.routeSources` (where they're already set, else the user's settings —
a device belongs to the machine), and any change of those settings switches a running player **in
place** with the protocol's `output` command: `sme-live` opens the new device, checks the routing map
fits it, then swaps the stream and restarts the session like ⟲ — mutes, solos, hold/drift and the song
mode carry over, the drift and Orrery start afresh, a ~0.2 s gap. A refused switch (no such device, a
map wider than it) keeps the old output, and the extension puts the settings back. Crash restarts and
the next start use the settings. `open_output` now prefers an exact device name over a substring.
For a piece with no Live set yet (no `ableton/<slug>.als`), "each source on its own pair" builds it
first: "Build Live Set…" in live mode, sized for the picked device. It uses the set's recorded
template, else the user's `sme-template`, and only asks when there's neither. The new set opens in Live
by itself (`--open new`), and the switch then reads the new routing map. If the build is cancelled or fails, the switch still happens and the
sources play in doc order. (Fixed 2026-10-21: the option only switched the output, and its "(make als)"
hint read like an action.)

Left out: D5 (the Live set builder), D6 (reference sets), D8 (render mode), D9 (the null test) and D10
(track colours in Live).

## Consequences

- A doc's `out_channel` annotations are ignored under routing (ADR 0027's split stays for `engines-play`
  and the TUI).
- The routing map keeps the fields it doesn't know, so the free tools never erase what a companion
  records in it.
