# The cross-agent checklist

A fixed script per skill, re-run whenever a skill changes. This file sits outside the skill folders,
so no agent reads it as instructions.

- **Now: Claude Code in VS Code.** The profile is clean, with only the Engines extension
  and the Claude Code extension (`anthropic.claude-code`, signed in to your Claude plan) installed. Then it
  runs once more with Engines Studio, for the hand-offs.
- **Later: Copilot**, on Copilot's default model and on one Claude model from Copilot's menu, with
  only the Engines extension installed. The items are the same; the Copilot notes below say what
  differs.

The VSIX: `spikes/vsix-spike/build.sh` packages `editors/vscode` with the `engines-*` tools in
its `bin/`. Install it into a fresh profile as that folder's README says (**Reproduce**). For the Studio
items, `npm ci && npm run package` in `editors/studio` and install `engines-studio.vsix` into the same profile.
`spikes/vsix-spike/smoke.sh` checks the same VSIX without an agent first. A fresh folder gets the skills'
copy for Claude Code (a stand-in extension triggers it) and its piece from the bundled tools, and the
extension plays and applies a save there with nothing from a checkout.

For Claude Code, install it into the same profile:

```bash
code --user-data-dir <profile>/ud --extensions-dir <profile>/ext --install-extension anthropic.claude-code
```

Before a run:
- Open an empty folder and trust it. Engines then copies its three skills into the folder's
  `.claude/skills/` (Studio's three too, when it's installed), each with a `.engines-copy` stamp.
- Open Claude Code's panel. If `/engines-new` isn't listed, run `/reload-skills` and note it: Claude Code
  doesn't watch a `.claude/skills/` made after its session started.
- Allow the `engines-*` commands when asked.
- Note the model, the VS Code, Engines and Claude Code versions, and the date.
- On Copilot: check the quota first. A full run is a few dozen requests per skill per model.

For each item: **pass**, **fail** (what happened instead), or **n/a**. On Copilot, note the requests used.

## /engines-new

Prompt: `/engines-new rain on glass, a sleep drone in D`

| # | must happen | note |
|---|---|---|
| N1 | the agent lists `/engines-new`, `/engines-edit`, `/engines-song` (Claude Code: from `.claude/skills/`; Copilot: in the slash menu) | `/reload-skills` needed? |
| N2 | the questions come as the agent's question tool (Claude Code's question cards, Copilot's forms) or numbered questions, the recommended option first and marked (Recommended), a free answer allowed | which form |
| N3 | it doesn't ask what the idea already says (sleep, D) | |
| N4 | the voice sheet and the slug are shown for a lock, and nothing is created before the lock | |
| N5 | without Studio: no snippet, MIDI or house-style starting point is offered | |
| N6 | `pieces/<slug>/<slug>.json5` starts from the skeleton; `brief.md` from the template | |
| N7 | layers come from `engines-part add` (one command each) | |
| N8 | every command runs from the extension's `bin/` (the path in the skill), with nothing on PATH | the path used |
| N9 | `engines-check` runs after the edits and reaches 0 errors | |
| N10 | does the agent fix `engines-lsp`'s diagnostics after its own edits, unprompted? (Claude Code reads them through its VS Code connection) | yes / no |
| N11 | `engines-song verify <doc> --ab` runs; peak and audibility are read back correctly | |
| N12 | the hand-off says to press ▶ and save to hear edits; the user presses ▶ and it plays | |

## /engines-edit

With the piece playing (▶). Prompt: `/engines-edit make it darker, and thin out the pulse`

| # | must happen | note |
|---|---|---|
| E1 | it reads `.live/status.json` first (and asks to save if the editor has unsaved edits) | Claude Code saves the buffer before its agent reads a file (`claudeCode.autosave`, on by default), so there's nothing left to ask about: the ask is n/a unless that's off |
| E2 | an ambiguous word gets one question with musical outcomes | |
| E3 | the change lands as one revision (`.live/pause` around several edits), checked to 0 errors | |
| E4 | it confirms `applied_rev == rev` and reports the revision playing | |
| E5 | the change is audible to the user | |
| E6 | `brief.md`'s changelog gets a line | |

## /engines-song

Prompt: `/engines-song a 10-minute nap`

| # | must happen | note |
|---|---|---|
| S1 | the interview covers length, arc, the voice × block grid, seams and tail, the deliverable | |
| S2 | the `song` is written into the same doc, with loop math in the comments, as one revision | |
| S3 | `engines-song --piece <slug> --dry-run` shows the plan without song lints | |
| S4 | `engines-song verify <doc>` (block table), `seams` (CLEAN) and `voyage` run and are read back | |
| S5 | it points the user to ▶ loop block / play from here to audition | |
| S6 | it asks before rendering, renders with `engines-song --piece <slug> --orrery`, and reports 0 clipped | the peak, LUFS and clipped count it reports (from `engines-song`'s last line, `done: …`) |

## With Engines Studio installed

| # | must happen | note |
|---|---|---|
| H1 | given a MIDI file in the idea (`/engines-new a sleep drone from <path>.mid`), `/engines-new` hands off to `/engines-midi` (it needn't ask where to start when the idea says) | |
| H2 | given a recording in the idea, `/engines-new` hands off to `/engines-listen` | n/a without a recording |
| H3 | after a stems render, `/engines-song` ends by asking whether to build the Live set with `/engines-ableton` (a question, not a passing mention) | |

## Also note (not pass/fail)

- On Copilot: whether VS Code's Claude agent host lists the extension's skills, and whether Copilot
  lists each skill twice when the Claude Code extension is installed too (`chatSkills` plus the folder's
  copy).
- Anything the agent improvised that the skill should have said.
