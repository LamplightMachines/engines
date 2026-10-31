# Contributing to Engines

Thanks for looking. Engines is a composing instrument for long-form generative tracks: a deterministic Rust
engine, a live player, authoring tools and a VS Code extension. Bug reports, fixes and new sounds are all
welcome.

## Before you start

- **Sign the CLA.** Your first pull request gets a CLA check with a link. It keeps your copyright and grants
  the project a licence to use your work under the GPL and other terms ([`LICENSING.md`](LICENSING.md),
  [`CLA.md`](CLA.md)). A PR can't merge without it.
- **Open an issue first** for anything bigger than a fix, so we can agree on the shape before you write it.
  A change to the loop-doc contract, to determinism or to the architecture also needs a decision record in
  [`doc/decisions/`](doc/decisions/).
- **Read [`CLAUDE.md`](CLAUDE.md)**, whether or not you use Claude: it's the short version of what this
  project is, what's in and out of scope, and the commitments the code keeps.

## Build and test

You need `cargo` (Rust), `uv` (Python 3.12 for the lab) and `npm` (the extension). On macOS:
`brew install rustup-init uv node`. On Linux, cpal and midir also need the ALSA headers:
`sudo apt-get install libasound2-dev`.

```sh
make test-ci   # what CI runs: the lean core build, the Rust suites, the lab, the extension's unit tests
make audit     # the licence gate (needs `cargo install cargo-deny --locked`)
make help      # every target
```

### The Mac half

Each OS's math library rounds a little differently, so the **golden render hashes** that pin determinism
(`crates/engines_core/tests/golden.rs`) are blessed and checked on macOS only. Off macOS those tests show as
ignored, and CI still checks what holds everywhere: the same cases, the same sample counts, and that cases
with equal hashes still hash alike.

`make test-mac` runs the Mac half: the full Rust suite with the goldens, the VS Code end-to-end suite, and the
Swift shell when Xcode is there. It then posts a `mac-suite` status on the commit. On a pull request, CI marks
`mac-suite` "not needed" when the change touches nothing it covers, and otherwise waits for a maintainer to run
it on a Mac. You don't need a Mac to contribute.

A test that only holds on macOS gets
`#[cfg_attr(not(target_os = "macos"), ignore = "<why>; run make test-mac")]`.

## The commitments

These are in [`CLAUDE.md`](CLAUDE.md) with their reasons. In short:

- **Renders are bit-reproducible.** The same doc, seed and control timeline give the same bytes. A change
  that moves render output on purpose re-blesses the goldens on a Mac (`ENGINES_BLESS=1 cargo test`), and
  its commit says which cases moved and why (`golden-change`).
- **No master limiter in renders.** The master out is `mix · master_gain_db`; headroom is the doc's job.
  The live player has a protection limiter, and renders never do.
- **A portable core.** `crates/engines_core` reads no files, clock or audio device outside its feature-gated
  shell modules. It builds for iOS unchanged.
- **Every param is native in the doc** (Hz, seconds, dB), and the engine reads it as a normalized `0..1`
  through the param's curve (ADR 0005, amended by ADR 0054).
- **No third-party GPL code.** `make audit` checks it; a new dependency with a licence `deny.toml` doesn't
  allow is a conversation, not an exception to add.

## Pull requests

- One PR per issue, with `Closes #N`, from the template.
- Tests come with the code. For a bug, start with a test that fails before the fix.
- Keep commits scoped, with messages that say what changed and why.
- An audible change gets the `needs-listen` label and a Listening section: the doc, the seed and what to
  listen for.

## Decision records

[`doc/decisions/`](doc/decisions/) holds the design decisions, one per record, with the alternatives they
rejected. Read the ones near your change before proposing a different approach. Engines was developed in a
private repository first, and record numbers are shared with it, so the gaps in the sequence are records that
weren't published. Because the sequence is shared, a maintainer gives a new record its number: write it as
`doc/decisions/NNNN-<slug>.md` and the number gets filled in during review.

## Conduct

This project follows the Contributor Covenant: see [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
