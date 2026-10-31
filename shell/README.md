# engines-shell — macOS live shell over the Rust core

A thin Swift CLI (CE-R12) that links the `engines_core` C ABI (`../crates/engines_core`) and streams a
loop doc (or song) live via **AVAudioEngine** while the Rust core runs the controller's drift /
Orrery movement, optionally logging one JSON record per loop. This is the macOS-first form of an
eventual iOS app shell — the Rust core recompiles unchanged for `aarch64-apple-ios`; only this
shell changes.

## ⚠️ Requires full Xcode (not just Command Line Tools)

SwiftPM macOS/AVFoundation targets need the macOS **platform SDK**, which only ships with
**Xcode.app**. With Command Line Tools alone, `swift build` fails at
`xcrun --sdk macosx --show-sdk-platform-path`. Install Xcode, then:

```sh
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

## Build & run

```sh
# 1) build the Rust core (produces ../target/release/libsme_core.dylib + the C header).
cargo build --release            # (run from the repo root)

# 2) build + run the shell (from this shell/ directory)
swift run engines-shell -- \
  --doc ../lab/tests/fixtures/loop_contract_v0.9.json \
  --log /tmp/loops.jsonl
```

`Package.swift` resolves `../target/release` absolutely and embeds it as an rpath, so the
executable finds `libsme_core.dylib` at runtime.

### Flags
| flag | meaning | default |
|---|---|---|
| `--doc PATH` | loop document JSON/JSON5 (**required**) | — |
| `--log PATH` | per-loop records (newline-delimited JSON) | none |
| `--loops N` | number of loops; `<0` = run until Ctrl-C | `-1` |
| `--sr HZ` | sample rate | `48000` |
| `--max-change F` | max drift step per loop (δ bound); overrides the doc's `max_change` | the doc's, else `0.05` |
| `--seed N` | drift / Orrery seed | `0x5EED` |
| `--no-audio` | render without live playback | (plays) |

## Layout
```
Package.swift                       SwiftPM manifest (resolves libDir, links libsme_core)
Sources/CEngines/include/engines_core.h     → symlink to the crate's cbindgen header
Sources/CEngines/include/module.modulemap
Sources/engines-shell/main.swift        CLI + AVAudioEngine source node + record drain
```

> **Status: compile-maintained only** (decision 2026-07-07, see `doc/decisions/README.md`
> "Known gaps") — kept building against the current C ABI, but **feature catch-up is
> deferred to the iOS phase**: no MIDI-out, no DAW-return, no `out_channel` stems, no live
> control (the ABI itself doesn't expose those session options yet). Not compiled in CI —
> this dev environment has CLT only, no Xcode, so the Swift build can't be verified here;
> the Rust C ABI it links is fully tested (`cargo test`, see `crates/engines_core/src/ffi.rs`).
> Build it on a machine with Xcode.
