// End to end inside VS Code: an isolated instance of the installed VS Code (its own user-data and
// extensions dirs, nothing else loaded) opens a scratch workspace with two pieces, and the suite
// drives the extension — language id, engines-lsp features, the headless player, the panel, CodeLens
// commands, step highlighting, revert. `npm run test:e2e` (or `make vscode-test`).
// VSCODE_PATH overrides the VS Code executable; ENGINES_BIN the dir holding engines-lsp / engines-live.

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runTests } from '@vscode/test-electron';

const REPO = path.resolve(__dirname, '../../../..');
const EXT = path.resolve(__dirname, '../..');
/** Engines Studio, loaded beside Engines when it's compiled: your install's shape. */
const STUDIO = path.join(REPO, 'editors', 'studio');
const withStudio = fs.existsSync(path.join(STUDIO, 'out', 'src', 'extension.js'));

async function main(): Promise<void> {
  const code = process.env.VSCODE_PATH ?? '/Applications/Visual Studio Code.app/Contents/MacOS/Code';
  if (!fs.existsSync(code)) {
    console.error(`No VS Code at ${code} — set VSCODE_PATH.`);
    process.exit(2);
  }
  const bin = process.env.ENGINES_BIN ?? path.join(REPO, 'target/release');
  for (const tool of ['engines-lsp', 'engines-live']) {
    if (!fs.existsSync(path.join(bin, tool))) {
      console.error(`No ${tool} in ${bin} — make vscode-test builds them.`);
      process.exit(2);
    }
  }
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'engines-vscode-e2e-'));
  const ws = path.join(scratch, 'ws');
  fs.mkdirSync(path.join(ws, '.vscode'), { recursive: true });
  fs.writeFileSync(
    path.join(ws, '.vscode/settings.json'),
    JSON.stringify(
      {
        'engines.server.path': path.join(bin, 'engines-lsp'),
        'engines.live.path': path.join(bin, 'engines-live'),
        'engines.live.headless': true,
        'sme.live.debounceMs': 150, // its old key: the extension carries it over
        'files.autoSave': 'off',
        '[engines-loop]': { 'files.autoSave': 'off' },
        'workbench.startupEditor': 'none',
        'security.workspace.trust.enabled': false,
      },
      null,
      2,
    ),
  );
  // The user's settings from the SME days, under their old keys (the rename's carry-over).
  fs.mkdirSync(path.join(scratch, 'user', 'User'), { recursive: true });
  fs.writeFileSync(path.join(scratch, 'user', 'User', 'settings.json'), JSON.stringify({ 'sme.trace.server': 'messages', 'files.associations': { '*.loopdoc': 'sme-loop' } }, null, 2));
  for (const [slug, text] of Object.entries(PIECES)) {
    fs.mkdirSync(path.join(ws, 'pieces', slug), { recursive: true });
    fs.writeFileSync(path.join(ws, 'pieces', slug, `${slug}.json5`), text);
  }
  try {
    await runTests({
      vscodeExecutablePath: code,
      extensionDevelopmentPath: withStudio ? [EXT, STUDIO] : EXT,
      extensionTestsPath: path.join(__dirname, 'suite', 'index.js'),
      launchArgs: [
        ws,
        '--disable-extensions',
        '--user-data-dir',
        path.join(scratch, 'user'),
        '--extensions-dir',
        path.join(scratch, 'ext'),
        '--skip-welcome',
        '--skip-release-notes',
        '--disable-workspace-trust',
        '--disable-telemetry',
      ],
      extensionTestsEnv: { ENGINES_E2E_WS: ws, ENGINES_E2E_STUDIO: withStudio ? '1' : '' },
    });
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
}

const PIECES: Record<string, string> = {
  demo: `{
  // A 1 s loop, a held pad, a two-step sequence and a two-block song.
  schema_version: "0.9",
  transport: { bpm: 240, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: {
    pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 800, sustain: 1.0 } },
  },
  modulators: {
    seq: { type: "step_seq", output: "note", length: 4, steps: [
      { step: 0, voicing: { v1: { offset: 0 } } },
      { step: 2, voicing: { v1: { offset: 7 } } },
    ] },
  },
  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
  song: { matrices: { thin: { m: null } }, sequence: [ { loops: 2, label: "full" }, { matrix: "thin", loops: 2, label: "thin" } ], transition_s: 0.5, tail_loops: 1 },
}
`,
  typo: `{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  instruments: { pad: { note_engine: { polyphony: 1 }, params: { wave: "saw_soft", level_db: -7.96, cutof_hz: 800 } } },
}
`,
};

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
