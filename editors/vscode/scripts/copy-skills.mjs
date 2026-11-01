// Copies the portable skills (the repo's skills/engines-new, -edit and -song) into this
// extension's skills/ for packaging: the VSIX ships them and `contributes.chatSkills` declares
// them. The copy is gitignored; the repo's skills/ is the one source.
import { cpSync, existsSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(dirname(fileURLToPath(import.meta.url)));
const source = join(here, '..', '..', 'skills');
const target = join(here, 'skills');
const skills = ['engines-new', 'engines-edit', 'engines-song'];

rmSync(target, { recursive: true, force: true });
for (const name of skills) {
  const from = join(source, name);
  if (!existsSync(join(from, 'SKILL.md'))) {
    console.error(`copy-skills: ${from}/SKILL.md is missing`);
    process.exit(1);
  }
  cpSync(from, join(target, name), { recursive: true, dereference: true });
}
console.log(`copy-skills: ${skills.join(', ')} → skills/`);
