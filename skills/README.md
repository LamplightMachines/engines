# The portable Engines skills

`/engines-new`, `/engines-edit` and `/engines-song`: the piece workflow as agent skills
that run anywhere the bundled `engines-*` commands do — Copilot or Claude Code, in VS Code with only
the Engines extension installed, or in this repository.

| folder | what it does |
|---|---|
| `engines-new/` | idea → interview → `pieces/<slug>/` (a brief + one loop doc from the parts library) → checked, verified, handed over to play live |
| `engines-edit/` | a change to a playing piece, landed as one revision and confirmed playing |
| `engines-song/` | a piece → a finite song: interview, blocks, the checks (`engines-song verify`, `seams`, `voyage`), audition, the render |
| `shared/` | the engine reference every skill carries: `engine-facts.md`, `music-theory.md` |

## One copy

These folders are the only copy:
- **Claude Code** reads them through committed relative links, `.claude/skills/engines-new` →
  `../../skills/engines-new` (and the other two), so a fresh worktree needs no setup step.
- **The VS Code extension** packages copies of them.

Each skill folder must stand alone, because an agent reads a copy of the folder. So `shared/` is the
canonical engine reference and **`make skills`** copies it into each skill's `reference/`. The
copies are committed; edit `shared/`, run `make skills`, and commit both. A test fails when a copy
drifts (`crates/engines_authoring/tests/skills.rs`), and when:
- a skill calls `make`, `cargo`, `uv`, Python or `engines-render`, or names an agent's question tool;
- a folder's name differs from its `name` field (VS Code's `chatSkills` requires them to match);
- `engines-new`'s skeleton doc stops checking clean, or its control settings differ from
  `engines-defaults.json5`.

## Writing them

- **Commands:** only `engines-check`, `engines-part`, `engines-song` (and its checks),
  `engines-live` with its `.live/` files, and `engines-clean`, called by bare name. One marked line
  near the top of each `SKILL.md` says where they are:
  `<!-- engines:bin -->…<!-- /engines:bin -->`. The extension rewrites it to its own `bin/` in the
  copy it ships.
- **Questions:** "ask with your question tool if your agent has one", the recommended option first
  and marked (Recommended), a free answer always allowed. No tool is named.
- **Hand-offs** to the closed Engines Studio skills (`engines-listen`, `engines-midi`,
  `engines-ableton`) and to `house-style` and `dev-issue` are conditional: "if the `engines-listen`
  skill is available …". The open skills are complete without them.
- **Checking:** outside this repository's Claude Code there's no hook, so each skill says to run
  `engines-check` after every edit.
