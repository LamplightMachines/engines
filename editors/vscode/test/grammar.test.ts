// The JSON5 TextMate grammar, tokenized with VS Code's own engine (vscode-textmate + oniguruma):
// keys vs values, every JSON5 literal, comments, and every tracked loop doc without a stray scope.

import * as assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';
import * as oniguruma from 'vscode-oniguruma';
import * as tm from 'vscode-textmate';

const EXT = path.resolve(__dirname, '../..');
const REPO = path.resolve(EXT, '../..');

async function grammar(): Promise<tm.IGrammar> {
  const wasm = fs.readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm'));
  await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const registry = new tm.Registry({
    onigLib: Promise.resolve({ createOnigScanner: (s) => new oniguruma.OnigScanner(s), createOnigString: (s) => new oniguruma.OnigString(s) }),
    loadGrammar: async () => tm.parseRawGrammar(fs.readFileSync(path.join(EXT, 'syntaxes/json5.tmLanguage.json'), 'utf8'), 'json5.tmLanguage.json'),
  });
  const g = await registry.loadGrammar('source.json5.engines');
  assert.ok(g);
  return g;
}

/** `[text, innermost scope]` for every token of `src`. */
function tokens(g: tm.IGrammar, src: string): [string, string][] {
  let stack = tm.INITIAL;
  const out: [string, string][] = [];
  for (const line of src.split('\n')) {
    const r = g.tokenizeLine(line, stack);
    for (const t of r.tokens) {
      out.push([line.slice(t.startIndex, t.endIndex), t.scopes[t.scopes.length - 1]]);
    }
    stack = r.ruleStack;
  }
  return out;
}

function scopeOf(toks: [string, string][], text: string): string {
  const t = toks.find(([s]) => s === text);
  assert.ok(t, `no token ${JSON.stringify(text)} in ${JSON.stringify(toks)}`);
  return t[1];
}

test('keys, values and JSON5 literals', async () => {
  const g = await grammar();
  const src = `{
  // a comment
  cutoff: 0.3, "q": .5, 'w': +Infinity, hex: 0xFF, n: NaN, neg: -1e-3,
  wave: "saw_soft", single: 'it\\'s', on: true, off: null,
  multi: "line one \\
line two",
  /* block */ list: [ 1, 2, ],
  tri: 2.,
}`;
  const t = tokens(g, src);
  assert.equal(scopeOf(t, 'cutoff'), 'support.type.property-name.json5');
  assert.equal(scopeOf(t, 'q'), 'support.type.property-name.json5');
  assert.equal(scopeOf(t, '0.3'), 'constant.numeric.json5');
  assert.equal(scopeOf(t, '.5'), 'constant.numeric.json5');
  assert.equal(scopeOf(t, '+Infinity'), 'constant.numeric.json5');
  assert.equal(scopeOf(t, '0xFF'), 'constant.numeric.json5');
  assert.equal(scopeOf(t, 'NaN'), 'constant.numeric.json5');
  assert.equal(scopeOf(t, '-1e-3'), 'constant.numeric.json5');
  assert.equal(scopeOf(t, '2.'), 'constant.numeric.json5');
  assert.equal(scopeOf(t, 'saw_soft'), 'string.quoted.double.json5');
  assert.equal(scopeOf(t, "\\'"), 'constant.character.escape.json5');
  assert.equal(scopeOf(t, 'true'), 'constant.language.json5');
  assert.equal(scopeOf(t, 'null'), 'constant.language.json5');
  assert.equal(scopeOf(t, 'line two'), 'string.quoted.double.json5', 'a line continuation keeps the string open');
  assert.equal(scopeOf(t, ' a comment'), 'comment.line.double-slash.json5');
  assert.equal(scopeOf(t, ' block '), 'comment.block.json5');
  assert.equal(scopeOf(t, 'tri'), 'support.type.property-name.json5', 'keys after a multi-line string are still keys');
});

test('every tracked loop doc tokenizes without stray scopes', async () => {
  const g = await grammar();
  // Every tracked JSON5 doc, and the JSON fixtures (JSON is JSON5). The open-set CI job deletes the
  // closed paths but leaves them in the index, so only the ones on disk.
  const docs = execFileSync('git', ['ls-files', '*.json5', 'lab/tests/fixtures/*.json'], { cwd: REPO, encoding: 'utf8' })
    .split('\n')
    .filter(Boolean)
    .map((f) => path.join(REPO, f))
    .filter((f) => fs.existsSync(f));
  assert.ok(docs.length > 20, `${docs.length} docs`);
  for (const doc of docs) {
    const src = fs.readFileSync(doc, 'utf8');
    const t = tokens(g, src);
    const bad = t.filter(([text, scope]) => scope.startsWith('invalid') || (scope === 'source.json5.engines' && text.trim() && !/^[\s,]*$/.test(text)));
    assert.deepEqual(bad.slice(0, 5), [], `${path.relative(REPO, doc)}`);
    // Every identifier before a colon is a key.
    for (const m of src.matchAll(/^\s*([A-Za-z_][\w]*)\s*:/gm)) {
      const s = t.find(([text]) => text === m[1]);
      assert.ok(s, `${m[1]} in ${doc}`);
    }
  }
});
