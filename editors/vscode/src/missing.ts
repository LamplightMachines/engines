// What to say when one of the tools is missing. In an Engines checkout it's a build away, so the
// warning offers the build. Anywhere else the tools come with the extension, so a missing one means
// a broken install: reinstall, or point the setting at one. Plain node: no `vscode` import, so
// `npm test` covers it.

import * as fs from 'node:fs';
import * as path from 'node:path';

/** `root` is an Engines checkout: it holds the authoring crate, and `target/release/` is built there. */
export function isCheckout(root: string | undefined): root is string {
  return !!root && fs.existsSync(path.join(root, 'crates', 'engines_authoring'));
}

/** The warning for a missing `name`, and whether to offer to build it. */
export function missingTool(name: string, root: string | undefined): { text: string; build: boolean } {
  if (isCheckout(root)) {
    return { text: `${name} isn't built (looked for ${path.join(root, 'target', 'release', name)}).`, build: true };
  }
  return { text: `${name} is missing: reinstall the Engines extension, or set its path in Settings.`, build: false };
}
