# engines-live protocol v1 (JSON lines over stdio)

`engines-live --stdio <slug> [--headless] [--loop-block K | --play-from K]` (ADR 0036) plays a piece and talks to its host — the VS Code extension
(P7) or a test — over **stdin/stdout, one JSON object per line** (UTF-8, `\n`-terminated). Commands
go in on stdin; events come out on stdout. Diagnostics/logs go to stderr only. `.live/status.json`
and `.live/history/` are written as in file mode, so Claude and other readers see the same state.

Every object has a `"type"`. Unknown commands get an `error` event; unknown fields are ignored.
Commands may carry an `"id"` (any JSON value); **every event answering a command echoes it** — the
ones it emits at once (`accepted` / `rejected` / `unchanged`, `gains`, `control`, `song`, `playing`,
`stopped`, `limiter`, `ready` for `status`, `error`) and the later ones it causes: the `applied` of
the revision a `doc` queued, a `preview`'s result, and the final `stopped` after `quit`. A `doc`
accepted while nothing is playing (stopped, finished) or while a song plays has no `applied` of its
own — the next `play` / `song stop` starts on it. Events with no command behind them (`tick`,
`loop`, `external_change`, a disk save's `accepted` / `unchanged` / `rejected`) carry no id.
Commands are lines of UTF-8: a line that isn't is an `error` event (the session goes on).

## Commands (host → engines-live)

| type | fields | effect |
|---|---|---|
| `doc` | `text` (string, the whole doc), `version`? (host's buffer version), `source`? (a label: `"editor"` (the default), `"revert"`, …; lowercase letters, digits, `-` / `_`, ≤ 32 chars — anything else is `"editor"`) | The `source` is recorded in the history with the revision (a save picked up from the file is `"disk"`; ADR 0040). Check strictly; refused ⇒ `rejected`; accepted ⇒ a history revision, applied **mid-loop** ⇒ `applied` (or `unchanged` when identical to the playing revision). A revision that can't be written to the history (a full disk) is `rejected` (code `history`) — nothing plays unrecorded. The editor sends the doc as it saves it (or, with `engines.live.applyOn: "edit"`, its buffer as you type, debounced ~300 ms; ADR 0038). The file's own save of a text that already arrived as a `doc` isn't heard a second time. |
| `play` | — | Start playing (a fresh session from the current accepted doc) if stopped or finished ⇒ `playing`. While already playing it's a no-op — no event (`restart` starts over). |
| `stop` | — | Stop playing (the session ends; the player stays open). |
| `restart` | — | Stop + play (from loop 0, fresh controller). |
| `mute` | `instrument`, `on` (bool) | Mute / unmute one instrument (a ~10 ms ramp, per block). Audio only: a mirrored instrument's MIDI goes on (ADR 0048). |
| `solo` | `instrument`, `on` (bool) | Solo set: when non-empty, every instrument outside it is silent. Audio only, like `mute`. |
| `clear_solo` | — | Empty the solo set. |
| `control` | `optimize`? (`"hold"` / `"drift"`), `orrery`? (bool) | Switch the timbre axis / the Orrery. `hold` also re-applies the doc, so it plays **exactly as written** (drift's wandering is dropped) — the fine-tuning toggle. Takes effect at the next loop top (the doc reset immediately); while stopped, the next `play` starts under it. While a song plays, the switch this command names beats the block's own `control` until the next block starts; the next block applies its control again (ADR 0055 D3). ⇒ `control` event. |
| `song` | `action`: `"loop_block"` + `block` (index), or `"play_from"` + `block`, or `"stop"` | `loop_block`: play block *k*'s materialized doc as a plain loop (doc edits re-materialize it — live editing works). `play_from`: play the song from block *k*'s start (live edits are ignored while a song plays, ADR 0029). `stop`: back to the doc's top-level matrix. ⇒ `song` event. |
| `preview` | `seconds`? (number, clamped to 1..600 — 1..300 with `stems`; default 10), `stems`? (bool) | Render that long from the current doc offline into `.live/preview.wav` ⇒ `preview` event with the path. The file is replaced only once the new render is whole. With `stems` (ADR 0040), the same render also writes one −18 LUFS 24-bit WAV per audio instrument + bus (ADR 0030) into `.live/preview_stems/` with a `stems.json` — each file renamed over its old one when whole; only stems the previous manifest listed are ever removed; refused up front (an `error`) when the disk can't hold the float temps + stems. One at a time: a request while one renders is an `error`. |
| `limiter` | `on` (bool) | Toggle the live protection limiter (device mode). |
| `status` | — | Re-send `ready` (state snapshot). |
| `output` | `device` (a device name, or `null` / `""` for the system output), `route_sources` (bool: each audio source on its own pair, per the piece's routing map — needs a named device) | Play on another device without quitting (ADR 0044 D7): the device is opened, a routing it can't carry is refused, and only then does the player switch — a running session starts over like `restart` (mutes, solos, hold/drift and the song mode carry over; the drift and Orrery start afresh) and a fresh `ready` reports the new `device`, `sample_rate` and `routing`. Any failure (no such device, a routing map wider than it, a headless player) is an `error` (`output: …`) and the old output keeps playing. The device list comes from `engines-live --list-devices --json` (`[{name, channels, sample_rate, default}]`). |
| `editor` | `unsaved` (bool) | The host's buffer of the doc holds unsaved edits (or no longer does). No event; `status.json` gets `editor: {unsaved, at}` at once, so Claude can ask for a save before writing the file (the two edits would collide). |
| `quit` | — | Stop and exit (status `stopped`). EOF on stdin does the same. |

## Events (engines-live → host)

| type | fields |
|---|---|
| `ready` | `protocol: 1`, `capabilities` (what a companion player adds: `["midi_mirror"]` from Engines Studio's `engines-live-studio`, `[]` from the free `engines-live`; absent from an older player — read it as `[]`), `piece`, `rev`, `sample_rate`, `loop_s`, `master_steps` (the loop's steps), `device` (name or `null` headless), `limiter` (the protection limiter's state, bool), `playing`, `optimize`, `orrery`, `instruments` (ids, **in the doc's order**), `muted`, `soloed`, `note` (the doc's `meta.note`, or `null`), `song` (`null` or `{blocks: [{label, loops, orrery?}], mode, block, from, current, tail_loops}` — `from`: the block a song plays from (song mode, else `null`); `current`: the block playing now (song or block mode, else `null`); a block's `orrery` is what its `control` says about the Orrery (ADR 0055), absent when it leaves the session's), `key` (`{name, root, scale, degrees, names}`: the key's name, its root pitch class, the scale in semitones above the root with degree labels, and all 12 pitch classes spelled in the key — `names[pc]`; ADR 0038), `routing` (`--route-sources`, ADR 0044; else `null`: `{channels, map, pairs: [{pair, source, name}], overflow, shared_with?, unrouted, stale, note?}` — `pair`/`overflow` like `"3/4"`, `source` `inst:<id>` / `bus:<id>`, `map` the routing map's path or absent before `engines-als` has built one (the doc's sources then play in order from 1/2), `unrouted` the playing doc's sources with no pair of their own (they ride the overflow), `stale` once the map changed on disk — the same object is `status.json`'s `routing`. The MIDI mirror (ADR 0048): a pair whose source sends its notes as MIDI has `midi: {port, channel}` (`"Engines Out"` / `"Engines Out 2"`, channel = the pair's number `1..=16` on its port), a pair whose source can't has `no_midi` (why: a bus, a `midi` instrument, a binaural pair pinned to `root_hz`, past pair 32 — the panel offers its MIDI switch only without it), and `routing.midi` is `{ports, unavailable: [{source, reason}], delivery}` when the map switches MIDI on for any source — `delivery` `"virtual"` (the OS's MIDI ports), `"off"` (headless: nothing is sent) |
| `rejected` | `version`?, `errors`: [`{severity, path, code, message}`], `source` (`"command"` / `"disk"`) |
| `unchanged` | `version`?, `rev`, `source` (`"command"` / `"disk"`) — identical to the playing revision (a refusal, if any, is cleared) |
| `accepted` | `version`?, `rev`, `warnings` — checked + queued (an `applied` follows) |
| `applied` | `rev`, `loop`, `at_s` (seconds into the loop), `abs_frame`, `kind` (`"value"` / `"structural"`), `xfade_s` |
| `tick` | ≤ 20 Hz while playing: `loop`, `pos_s`, `loop_s`, `steps` (`{modulator: step}` for loop-synced step sequences), `meters` (`{instrument: dBFS}` since the last tick, post mute/solo), `master_db`, `gr_db` (the limiter's deepest gain reduction since the last tick, dB ≤ 0; `null` headless or with the limiter off), `meters_lr` (`{instrument: [left, right]}` dBFS — `meters` is their max), `master_lr` (`[left, right]` dBFS — `master_db` is their max), `song` (the latest block's song position, as in `loop`, or `null`) |
| `loop` | once per loop (several may arrive together; each carries its own loop's state): `loop`, `rev`, `optimize`, `orrery` (whether the Orrery ran this loop — a song block's `control` can switch it, ADR 0055), `chord`?, `tension`?, `voicing`? (`{pad, bass, tones}` — the Orrery's pad voicing, bass and chord tones in semitones from the root; `null` with the Orrery off; ADR 0038), `peak_db`, `song` (`null` in loop mode, or `{block, seam, loop_in_block, block_loops, tail}`: `loop_in_block` counts from 0 — in block mode from the loop the block started, wrapping every `block_loops` (the block repeats); `tail` (with `block_loops` = `tail_loops`) during a song's release tail) |
| `gains` | after `mute` / `solo` / `clear_solo`: `muted`, `soloed` |
| `limiter` | after `limiter`: `on` |
| `finished` | the session ran out (a song's end, `--max-loops`): `mode` — `play` / `song stop` / `restart` start again |
| `control` | `optimize`, `orrery` (the mode now requested; while a song plays, the switch the command didn't name keeps what the block plays) |
| `song` | `mode` (`"loop"` / `"block"` / `"song"`), `block`? — after a `song` command, and after an edit that leaves block mode (the block it looped no longer exists) |
| `preview` | `path`, `seconds`, `peak_db`, `lufs`?, `stems` (`null`, or `{dir, target_lufs, files: [{id, kind, file, gain_db, normalized, limited_by, raw_lufs, out_lufs, out_peak_dbfs}], skipped: [{id, kind, reason}]}`) |
| `underrun` | device mode: `count` (the process's total so far — it doesn't reset on `play` / `restart`; a finished session's ring running dry isn't one) |
| `external_change` | `rev` — the doc file changed on disk to a text that didn't come via `doc` (Claude's edit, another editor) and was accepted (a `rejected` with `source: "disk"` instead when it's refused, an `unchanged` with `source: "disk"` when it's the revision already playing — a refused save put back) |
| `stopped` | after `stop` / `quit`, and after an `error` saying the render worker died (a bug — it no longer hangs as "playing" in silence; `play` / `restart` start again) |
| `playing` | a session started (`play` when not playing, `restart`, a song action): `mode`, `block` |
| `routing` | `stale: true`, `message` — the routing map changed on disk (`make als` rebuilt the Live set): reopen the set in Live, then `restart` to play the new routing (ADR 0044; once per session). `stale: false`, `routing` — a (re)start re-read the map and what plays changed (the `ready.routing` object, e.g. a source's MIDI switched on, ADR 0048) |
| `error` | `message`, `id`? |

## Notes

- Revisions are shared with file mode and `engines-song` (ADR 0034): content-addressed, so the same text
  from the editor and from disk is one revision.
- The host should treat `applied` (not `accepted`) as "you can hear it now" — it fires when the
  worker crossfades into the revision (~a ring, ≈ 170 ms, after acceptance).
- Mid-loop apply: value edits crossfade 30 ms, structural ones 250 ms (ADR 0035).
- Additions keep `protocol: 1` (hosts ignore unknown fields): `ready.key` and `loop.voicing` were
  added for the VS Code extension's keyboard (ADR 0038); the `editor` command for Claude's skills
  (P9); `unchanged.source`, with a disk save's `unchanged`. ADR 0040 (the panel
  redesign) added `ready.master_steps` / `limiter` / `note` / `song.{from, current, tail_loops}`,
  `instruments` in document order, `tick.meters_lr` / `master_lr` / `song`, the song position fields
  of `loop.song` (block mode included), the history `source` of a `doc`, and `preview.stems`.
  ADR 0044 (per-source routing) added `ready.routing` and the `routing` event; D7 the `output` command.
  ADR 0048 (the MIDI mirror) added `routing.pairs[].midi` / `no_midi` and `routing.midi`. Since the open/closed split they come only
  from a player whose `ready.capabilities` has `midi_mirror` (Engines Studio's): the free player's routing
  carries none of them, and a host offers MIDI switches only with the capability. The mirror's notes go
  out only in a routed session (`--route-sources`), timed against the session's own audio; a stop,
  `restart`, song action or `output` switch ends every mirrored note (and sends CC 123 on each
  channel used) before the next session starts. The ports stay open for the player's life.
