# utils

Small standalone helpers. They are not part of the engine workspace or `make test`.

## `record-system-audio`: record what the Mac is playing

```sh
utils/record-system-audio [out.wav]
```

This records everything macOS is playing (Spotify, a browser, any app) to a WAV. It shows the elapsed time and peak level, and stops when you press **Enter** (or Ctrl-C). Without an argument it writes `./system-audio-<YYYYmmdd-HHMMSS>.wav`, a 32-bit float stereo file at the output's rate (about 23 MB per minute; `*.wav` is gitignored).

- **How it works:** it uses a Core Audio process tap (macOS 14.2+), a private mixdown of every app's output. You keep hearing the audio, and there's no BlackHole or Multi-Output routing to set up. Rust source: `system-audio-recorder/`. The wrapper builds it on first use and whenever the source changes.
- **Permission (one time):** macOS must allow the app you launch it from to record system audio. Without permission the tap delivers silence; the meter then reads `silent` and prints a hint after 3 s.
  - **VS Code's terminal:** VS Code declares the capability, so macOS asks on first run.
  - **Terminal.app** doesn't declare it, so it can't prompt. Add it by hand under System Settings › Privacy & Security › Screen & System Audio Recording › **System Audio Recording Only** (`+`). The same applies to any other terminal that doesn't prompt.
  - **A Claude Code session in the desktop app:** commands run under the bundled *Claude Code* app (`com.anthropic.claude-code`), not under *Claude*, and it doesn't declare the capability either. So enabling "Claude" isn't enough: add `~/Library/Application Support/Claude/claude-code/<version>/<hash>/claude.app` with `+` (⌘⇧G to type the path). Verified this way: a 3.00 s 440 Hz test tone came back at its exact level. The path changes with each Claude Code update.
- **Volume:** the tap records what apps send to the output, before the Mac's volume slider. So the level doesn't depend on your volume setting, though an app's own volume (Spotify's slider) does change it.
