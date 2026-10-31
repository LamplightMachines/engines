# Dev convenience targets — Rust core + cpal shell + Python lab.
# Requires: cargo (Rust) and uv on PATH  (brew install rustup-init uv).
.PHONY: help build test test-rust test-lab test-ci test-mac audit play tui render song check lsp part migrate live vscode vscode-test prune templates skills clean

# The default doc for play/render: the neutral test drone, gain-staged to be heard. The contract
# fixtures are correctness rigs that run hot (+13 dBFS) — play one with FIXTURE=<doc>.
FIXTURE = lab/tests/fixtures/loop_drone_d_lydian.json5
# Anything after the target (play / tui / song) is forwarded to the underlying binary, so you can
# override the doc:
#   make play -- --doc lab/tests/fixtures/loop_shimmer_showcase.json
# The forwarded flags land AFTER the target's defaults (`--doc $(FIXTURE)` for play,
# `--doc $(SONG) --out $(SONG_OUT)` for song), and a forwarded flag wins because engines-play and
# engines-song are built with clap's `args_override_self` (a repeated single-value flag keeps its last
# value; repeatable ones like --ceiling still accumulate). `tui` passes no defaults of its own. The
# swallow rule at the bottom of this file stops Make from treating those forwarded tokens as
# targets. As an alternative, override the variable directly:  make play FIXTURE=<doc>
# (for song prefer SONG_OUT=<wav> over a forwarded --out: only SONG_OUT's directory gets mkdir'd).
PLAY_ARGS = $(filter-out play,$(MAKECMDGOALS))
TUI_ARGS = $(filter-out tui,$(MAKECMDGOALS))
SONG_ARGS = $(filter-out song,$(MAKECMDGOALS))
CHECK_ARGS = $(filter-out check,$(MAKECMDGOALS))
LIVE_ARGS = $(filter-out live,$(MAKECMDGOALS))
PRUNE_ARGS = $(filter-out prune,$(MAKECMDGOALS))
PART_ARGS = $(filter-out part,$(MAKECMDGOALS))
# `make song` (ADR 0029): the song doc + output WAV (renders/ is git-ignored). Override with
#   make song SONG=<doc.json5> SONG_OUT=renders/night.wav -- --orrery --format s24
# A SONG_OUT ending in .flac writes lossless 24-bit FLAC instead (e.g. SONG_OUT=renders/night.flac).
# STEMS=<dir> (ADR 0030) writes loudness-normalized per-instrument + per-bus stems there INSTEAD of
# SONG_OUT (FLAC by default):  make song SONG=<doc.json5> STEMS=renders/garden -- --orrery --stem-lufs -14
# (through make, write `--stem-lufs -14` with a space: make reads any `a=b` word as a variable).
# `make check` (ADR 0033): strictly check DOC (default: the template and every fixture loop doc);
# forward `-- --parts` (per-modulator cycles) or `-- --json`.
DOC ?= doc/loop_contract_v0.9.json5 $(filter-out %objectives.json5,$(wildcard lab/tests/fixtures/*.json lab/tests/fixtures/*.json5))
SONG ?= lab/tests/fixtures/song_lydian_tide.json5
# PIECE=<slug> (ADR 0034): `make song PIECE=<slug>` renders pieces/<slug>/<slug>.json5 into its renders/
# (<date>_r<rev>.flac + a .json sidecar; a song ⇒ a <date>_r<rev>/ dir of per-block files, WHOLE=1 ⇒ one
# file; STEMS=1 ⇒ a <date>_r<rev>_stems/ dir); `make live PIECE=<slug>`
# plays it live, applying every save; `make prune PIECE=<slug>` lists old renders (`-- --yes` deletes).
PIECE ?=
# `make live PIECE=<slug> DEVICE="BlackHole 64ch" ROUTE=1`: play into a multichannel loopback, every
# audio source on its own stereo pair (a DAW listens there, one track per pair).
DEVICE ?=
ROUTE ?=
SONG_OUT ?= renders/song.wav
STEMS ?=
WHOLE ?=

help:  ## List available targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk -F':.*?## ' '{printf "  %-12s %s\n", $$1, $$2}'

build:  ## Build the Rust workspace (release)
	cargo build --release

test: test-rust test-lab  ## Run the Rust + Python lab suites

test-rust:  ## Rust unit/integration tests (engine, controller, session, songs, FFI, golden renders)
	cargo test

test-lab:  ## Python lab tests (drive the built CLI binaries over the file boundary)
	uv run --python 3.12 --with numpy --with pytest pytest -q

# The test split (CONTRIBUTING.md): GitHub CI runs test-ci on Linux; test-mac runs what only the Mac can
# (the goldens are blessed there) and reports it as the `mac-suite` commit status. On the Mac,
# `make test` is still the everyday Rust + lab run.
test-ci:  ## What GitHub CI runs on Linux (CONTRIBUTING.md): the lean core build, the Rust suites (Mac-pinned tests show as ignored), the lab, the VS Code unit tests, the rules of the .github/ scripts (the licence check, the mac-suite gate)
	cargo build -p engines_core --no-default-features
	cargo test
	cargo build --release -p engines_live --bin engines-live
	$(MAKE) test-lab
	cd editors/vscode && npm ci && npm test
	node --test .github/scripts/*.test.js

audit:  ## The licence gate (LICENSING.md, deny.toml): no GPL, AGPL or LGPL-only third-party crate anywhere in the workspace (the owner's crates exempt by name), the extensions' runtime npm packages on the same list, a licence field in every manifest, each extension's licence file. Needs cargo-deny (`cargo install cargo-deny --locked`); CI runs it
	cargo deny check licenses
	node .github/scripts/licences.js

test-mac:  ## The Mac part (CONTRIBUTING.md): the full Rust suite incl. the Mac-pinned goldens, the VS Code end-to-end suite; then posts `mac-suite` on HEAD (needs gh; NO_STATUS=1 to skip)
	.github/scripts/mac-suite.sh

play:  ## Hear it live (cpal -> CoreAudio); `make play -- --doc <doc>` or FIXTURE=<doc>; add `--midi` to send type:"midi" instruments' Note/CC to a virtual port (ADR 0026); `--output-device <dev>` redirects all audio to a loopback (ADR 0027)
	cargo run --release --features playback,midi --bin engines-play -- --doc $(FIXTURE) $(PLAY_ARGS)

tui:  ## Interactive live TUI (retired from the composing workflow, kept working); forward args like `make tui -- --doc <doc> --optimize drift`. Release build — debug render is slower than real time and underruns.
	cargo run --release -p engines_tui -- $(TUI_ARGS)

render:  ## Headless render the fixture to /tmp/out.wav (4 loops @ 48k)
	cargo run --release --bin engines-render -- $(FIXTURE) /tmp/out.wav 4 48000

song:  ## Render a SONG (ADR 0029) headlessly to $(SONG_OUT) (RF64 past 4 GiB; .flac ⇒ FLAC), or STEMS=<dir> for normalized per-source stems (ADR 0030) — a song is written one file per block (ADR 0043; $(SONG_OUT) minus its extension becomes the folder), WHOLE=1 for one continuous file; SONG=<doc>, or PIECE=<slug> to render into the piece's renders/ (STEMS=1 for stems); forward flags like `make song -- --orrery --format s24` (`-- --dry-run` prints the plan)
ifneq ($(PIECE),)
	cargo run --release --bin engines-song -- --piece $(PIECE) $(if $(STEMS),--stems) $(if $(WHOLE),--whole) $(SONG_ARGS)
else
	@$(if $(STEMS),:,mkdir -p $(dir $(SONG_OUT)))
	cargo run --release --bin engines-song -- --doc $(SONG) $(if $(STEMS),--stems $(STEMS),--out $(SONG_OUT)) $(if $(WHOLE),--whole) $(SONG_ARGS)
endif

check:  ## Strictly check loop docs like the engine loads them (unknown keys/params w/ did-you-mean, enums, ranges, dangling routes) + the polymeter; DOC=<doc.json5>; `make check -- --parts` / `-- --json`
	cargo run -q --release -p engines_authoring --bin engines-check -- $(DOC) $(CHECK_ARGS)

lsp:  ## Build the loop-doc language server (engines-lsp, ADR 0037) at target/release/engines-lsp — diagnostics, completion (scale-aware offsets), hover, inlay hints, CodeLens, code actions, rename; the VS Code extension (P7) starts it over stdio
	cargo build --release -p engines_authoring --bin engines-lsp

part:  ## Add a part from the library to a piece (engines-part, ADR 0039): `make part PIECE=<slug> PART=<name>` (instrument + its modulators, routes, sends — comments kept, ids made unique, checked); `make part` lists them; forward flags like `-- --target pad --dry-run`
ifneq ($(PART),)
	@$(if $(PIECE),:,echo "make part needs PIECE=<slug> (and PART=<name>)" && exit 2)
	cargo run -q --release -p engines_authoring --bin engines-part -- add $(PIECE) $(PART) $(PART_ARGS)
else
	cargo run -q --release -p engines_authoring --bin engines-part -- list
endif

vscode:  ## Build + install the VS Code extension (ADR 0038): engines-lsp + engines-live (release), npm ci, package editors/vscode/engines.vsix, code --install-extension (reload VS Code after). Needs network for npm
	cargo build --release -p engines_authoring --bin engines-lsp -p engines_live --bin engines-live
	cd editors/vscode && npm ci && npm run package
	code --install-extension editors/vscode/engines.vsix --force

vscode-test:  ## The extension's tests: unit (node:test — state, keyboard, panel model, grammar, the process manager against the real engines-live) + end to end in an isolated instance of the installed VS Code (a window opens for ~10 s)
	cargo build --release -p engines_authoring --bin engines-lsp -p engines_live --bin engines-live
	cd editors/vscode && npm ci && npm run package && npm test && npm run test:e2e

live:  ## Play a piece live (ADR 0034; engines-live): every save of pieces/$(PIECE)/$(PIECE).json5 is checked, snapshotted into .live/history and applied at the next loop; .live/status.json reports; live-only limiter. `make live PIECE=<slug>`; into a DAW: `DEVICE="BlackHole 64ch" ROUTE=1` (one pair per source); forward flags like `-- --optimize hold`
	@$(if $(PIECE),:,echo "make live needs PIECE=<slug>" && exit 2)
	cargo run -q --release -p engines_live --bin engines-live -- $(PIECE) $(if $(DEVICE),--output-device "$(DEVICE)") $(if $(ROUTE),--route-sources) $(LIVE_ARGS)

migrate:  ## Migrate a schema v0.8 loop doc to v0.9 (engines-migrate, ADR 0054): real units with suffixed keys (cutoff_hz, attack_s, level_db, -Infinity), route depths in their dest's unit, comments kept, one atomic write. `make migrate DOC=<doc>` (a loop doc, a part or an objectives file); `make migrate PIECE=<slug>` does the whole piece (doc, .live/history, objectives; v0.8 kept in pieces/<slug>/.v0.8-backup/, renders and the Live set untouched); `DRY=1` prints the diff
	@$(if $(DOC)$(PIECE),:,echo "make migrate needs DOC=<doc> or PIECE=<slug>" && exit 2)
	cargo run -q --release -p engines_authoring --bin engines-migrate -- $(if $(PIECE),--piece $(PIECE),"$(DOC)") $(if $(DRY),--dry-run) $(MIGRATE_ARGS)

prune:  ## List old renders of a piece that could go (dry run): `make prune PIECE=<slug>`; `-- --yes` deletes. Keeps the newest 3 (`-- --keep N`), published/keep-marked and protected renders; never touches renders engines-song didn't make
	@$(if $(PIECE),:,echo "make prune needs PIECE=<slug>" && exit 2)
	cargo run -q --release -p engines_authoring --bin engines-clean -- $(PIECE) $(PRUNE_ARGS)

templates:  ## Write commented JSON5 starter templates into the cwd (loop_doc + objectives; ADR 0023)
	cargo run --release --bin engines-render -- --emit-template loop loop_doc.template.json5
	cargo run --release --bin engines-render -- --emit-template objectives objectives.template.json5

skills:  ## Copy the shared engine reference (skills/shared/) into each portable skill's reference/: each skill folder must stand alone. The copies are committed; a test fails when one drifts
	@for s in skills/engines-*/; do mkdir -p "$$s/reference" && cp skills/shared/*.md "$$s/reference/"; done
	@echo "skills/shared/ copied into $$(ls -d skills/engines-*/ | tr '\n' ' ')"

clean:  ## Remove build artifacts (Rust, Python)
	cargo clean
	rm -rf .venv .build .pytest_cache

# Forwarded-arg support for `play`: when `play` is a goal, turn the trailing tokens
# (e.g. `--doc <path>`) into no-op targets so Make doesn't fail with "No rule to make target".
# Scoped to `play` so it never shadows other targets (e.g. `make test foo` still errors on foo).
ifeq ($(filter play,$(MAKECMDGOALS)),play)
ifneq ($(PLAY_ARGS),)
$(PLAY_ARGS):
	@:
endif
endif

# Same forwarded-arg support for `tui` (e.g. `make tui -- --doc <path> --orrery`).
ifeq ($(filter tui,$(MAKECMDGOALS)),tui)
ifneq ($(TUI_ARGS),)
$(TUI_ARGS):
	@:
endif
endif

# Same forwarded-arg support for `live`, `prune` and `part`.
ifeq ($(filter live,$(MAKECMDGOALS)),live)
ifneq ($(LIVE_ARGS),)
$(LIVE_ARGS):
	@:
endif
endif
ifeq ($(filter prune,$(MAKECMDGOALS)),prune)
ifneq ($(PRUNE_ARGS),)
$(PRUNE_ARGS):
	@:
endif
endif
ifeq ($(filter part,$(MAKECMDGOALS)),part)
ifneq ($(PART_ARGS),)
$(PART_ARGS):
	@:
endif
endif


# Same forwarded-arg support for `check` (e.g. `make check -- --parts`).
ifeq ($(filter check,$(MAKECMDGOALS)),check)
ifneq ($(CHECK_ARGS),)
$(CHECK_ARGS):
	@:
endif
endif

# Same forwarded-arg support for `song` (e.g. `make song -- --orrery --format s24`).
ifeq ($(filter song,$(MAKECMDGOALS)),song)
ifneq ($(SONG_ARGS),)
$(SONG_ARGS):
	@:
endif
endif
