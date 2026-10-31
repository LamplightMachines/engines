// The mac-suite gate's rules (CONTRIBUTING.md): `node --test .github/scripts/mac-suite-gate.test.js`.
const test = require('node:test');
const assert = require('node:assert/strict');
const { isCovered, coveredPaths, isMacResult, decide, run } = require('./mac-suite-gate.js');

const PASS = { state: 'success', context: 'mac-suite', description: 'passed on macOS 15.1 arm64' };
const FAIL = { state: 'failure', context: 'mac-suite', description: 'cargo test failed' };
const RUNNING = { state: 'pending', context: 'mac-suite', description: 'make test-mac is running' };
const NOT_NEEDED = { state: 'success', context: 'mac-suite', description: 'not needed: nothing the Mac suite covers changed' };
const CARRIED = { state: 'success', context: 'mac-suite', description: 'carried from aaaaaaa: passed on macOS 15.1 arm64; nothing covered changed since' };
const GATE_PENDING = { state: 'pending', context: 'mac-suite', description: 'run make test-mac on the Mac: 1 file(s) it covers changed' };

const sha = (n) => String(n).repeat(40);
const HEAD = sha(9);

// A stub of the REST calls the gate makes. `commits` lists the PR's SHAs oldest first (the head
// last); `statuses` maps a SHA to its latest mac-suite status; `compare` maps `base...head` to
// { status, files }.
function stubGithub({ prFiles, commits = [sha(1), HEAD], statuses = {}, compare = {} }) {
  const calls = { compared: [], posted: [] };
  const listFiles = async () => prFiles.map((f) => (typeof f === 'string' ? { filename: f } : f));
  const listCommits = async () => commits.map((s) => ({ sha: s }));
  const github = {
    paginate: async (fn) => fn(),
    rest: {
      pulls: { listFiles, listCommits },
      repos: {
        getCombinedStatusForRef: async ({ ref }) => ({
          data: { statuses: [{ context: 'ci/other', state: 'success' }, ...(statuses[ref] ? [statuses[ref]] : [])] },
        }),
        compareCommitsWithBasehead: async ({ basehead }) => {
          calls.compared.push(basehead);
          const c = compare[basehead];
          if (!c) throw new Error(`no stubbed compare for ${basehead}`);
          return { data: { status: c.status, files: c.files.map((f) => (typeof f === 'string' ? { filename: f } : f)) } };
        },
        createCommitStatus: async (s) => calls.posted.push(s),
      },
    },
  };
  return { github, calls };
}

const pr = (over = {}) => ({ number: 7, commits: 2, head: { sha: HEAD }, ...over });
const repo = { owner: 'o', repo: 'r' };
const gate = (opts, prOver) => decide({ github: stubGithub(opts).github, repo, pr: pr(prOver) });

test('the covered list: the Mac suite\'s inputs, nothing else', () => {
  for (const f of ['crates/engines_core/src/lib.rs', 'Cargo.toml', 'Cargo.lock', 'lab/tests/fixtures/x.json', 'contracts/a.json',
    'editors/vscode/src/x.ts', 'Makefile', '.github/scripts/mac-suite.sh']) assert.ok(isCovered(f), f);
  for (const f of ['doc/process.md', 'README.md', '.github/scripts/mac-suite-gate.js', '.github/workflows/ci.yml',
    'lab/tests/test_x.py', 'cratesx/a', 'sub/Makefile']) assert.ok(!isCovered(f), f);
});

test('a rename counts by its old name too', () => {
  assert.deepEqual(coveredPaths([{ filename: 'doc/old.rs', previous_filename: 'crates/engines_core/src/old.rs' }]), ['crates/engines_core/src/old.rs']);
});

test('only make test-mac\'s pass or failure is a Mac result', () => {
  assert.ok(isMacResult(PASS));
  assert.ok(isMacResult(FAIL));
  for (const s of [RUNNING, NOT_NEEDED, CARRIED, GATE_PENDING]) assert.ok(!isMacResult(s), s.description);
});

test('a head that already has a mac-suite status is left as is', async () => {
  assert.equal(await gate({ prFiles: ['Makefile'], statuses: { [HEAD]: RUNNING } }), null);
  assert.equal(await gate({ prFiles: ['doc/x.md'], statuses: { [HEAD]: FAIL } }), null);
});

test('nothing covered in the PR: not needed, without walking the commits', async () => {
  const { github, calls } = stubGithub({ prFiles: ['doc/process.md', '.github/workflows/ci.yml'] });
  assert.deepEqual(await decide({ github, repo, pr: pr() }), { state: 'success', description: 'not needed: nothing the Mac suite covers changed' });
  assert.deepEqual(calls.compared, []);
});

test('carried: an earlier pass, and only uncovered files changed since', async () => {
  const verdict = await gate({
    prFiles: ['Makefile', 'doc/process.md'],
    statuses: { [sha(1)]: PASS },
    compare: { [`${sha(1)}...${HEAD}`]: { status: 'ahead', files: ['doc/process.md', '.github/scripts/claude-session.js'] } },
  });
  assert.deepEqual(verdict, { state: 'success', description: 'carried from 1111111: passed on macOS 15.1 arm64; nothing covered changed since' });
});

test('carried from the newest pass, past the gate\'s own statuses and an aborted run', async () => {
  const { github, calls } = stubGithub({
    prFiles: ['crates/a.rs'],
    commits: [sha(1), sha(2), sha(3), sha(4), sha(5), HEAD],
    statuses: { [sha(1)]: FAIL, [sha(2)]: PASS, [sha(3)]: CARRIED, [sha(4)]: RUNNING, [sha(5)]: GATE_PENDING },
    compare: { [`${sha(2)}...${HEAD}`]: { status: 'ahead', files: ['README.md'] } },
  });
  const verdict = await decide({ github, repo, pr: pr({ commits: 6 }) });
  assert.equal(verdict.state, 'success');
  assert.match(verdict.description, /^carried from 2222222: passed on macOS/);
  assert.deepEqual(calls.compared, [`${sha(2)}...${HEAD}`]);
});

test('not carried: a covered file changed since the pass', async () => {
  const verdict = await gate({
    prFiles: ['Makefile'],
    statuses: { [sha(1)]: PASS },
    compare: { [`${sha(1)}...${HEAD}`]: { status: 'ahead', files: ['doc/x.md', 'crates/engines_core/src/engine.rs'] } },
  });
  assert.deepEqual(verdict, { state: 'pending', description: 'run make test-mac on the Mac: 1 file(s) it covers changed' });
});

test('not carried: a covered file renamed away since the pass', async () => {
  const verdict = await gate({
    prFiles: ['Makefile'],
    statuses: { [sha(1)]: PASS },
    compare: { [`${sha(1)}...${HEAD}`]: { status: 'ahead', files: [{ filename: 'attic/x.rs', previous_filename: 'crates/x.rs' }] } },
  });
  assert.equal(verdict.state, 'pending');
});

test('not carried: force-pushed, so the passed commit is no longer an ancestor of the head', async () => {
  for (const status of ['diverged', 'behind', 'identical']) {
    const verdict = await gate({
      prFiles: ['Makefile'],
      statuses: { [sha(1)]: PASS },
      compare: { [`${sha(1)}...${HEAD}`]: { status, files: [] } },
    });
    assert.equal(verdict.state, 'pending', status);
  }
});

test('not carried: a force-push dropped the passed commit from the PR', async () => {
  // The old commit still has its pass, but the PR's commits are all new: nothing to carry from.
  const { github, calls } = stubGithub({ prFiles: ['Makefile'], commits: [sha(3), HEAD], statuses: { [sha(1)]: PASS } });
  assert.equal((await decide({ github, repo, pr: pr() })).state, 'pending');
  assert.deepEqual(calls.compared, []);
});

test('not carried: no earlier commit passed make test-mac', async () => {
  for (const statuses of [{}, { [sha(1)]: NOT_NEEDED }, { [sha(1)]: RUNNING }, { [sha(1)]: CARRIED }]) {
    const { github, calls } = stubGithub({ prFiles: ['Makefile'], statuses });
    assert.equal((await decide({ github, repo, pr: pr() })).state, 'pending');
    assert.deepEqual(calls.compared, []);
  }
});

test('not carried: the newest make test-mac result is a failure, even with an older pass', async () => {
  const { github, calls } = stubGithub({
    prFiles: ['Makefile'],
    commits: [sha(1), sha(2), HEAD],
    statuses: { [sha(1)]: PASS, [sha(2)]: FAIL },
  });
  assert.equal((await decide({ github, repo, pr: pr({ commits: 3 }) })).state, 'pending');
  assert.deepEqual(calls.compared, []);
});

test('not carried: a compare at the 300-file cap may be truncated', async () => {
  const files = Array.from({ length: 300 }, (_, i) => `doc/f${i}.md`);
  const verdict = await gate({
    prFiles: ['Makefile'],
    statuses: { [sha(1)]: PASS },
    compare: { [`${sha(1)}...${HEAD}`]: { status: 'ahead', files } },
  });
  assert.equal(verdict.state, 'pending');
});

test('not carried: a PR too long for listCommits to show its head', async () => {
  assert.equal((await gate({ prFiles: ['Makefile'], statuses: { [sha(1)]: PASS } }, { commits: 250 })).state, 'pending');
  // The list doesn't end at the head (a push raced the run): pending too.
  assert.equal((await gate({ prFiles: ['Makefile'], commits: [sha(1), sha(2)], statuses: { [sha(1)]: PASS } })).state, 'pending');
});

test('a long carried description fits a status\'s 140 characters', async () => {
  const long = { ...PASS, description: `passed on macOS ${'x'.repeat(130)}` };
  const verdict = await gate({
    prFiles: ['Makefile'],
    statuses: { [sha(1)]: long },
    compare: { [`${sha(1)}...${HEAD}`]: { status: 'ahead', files: [] } },
  });
  assert.equal(verdict.state, 'success');
  assert.equal(verdict.description.length, 140);
});

test('run() posts the verdict on the head as mac-suite, and nothing when the head has one', async () => {
  const context = { repo, payload: { pull_request: pr() } };
  const core = { info: () => {} };
  const fresh = stubGithub({ prFiles: ['doc/x.md'] });
  await run({ github: fresh.github, context, core });
  assert.deepEqual(fresh.calls.posted, [{ ...repo, sha: HEAD, context: 'mac-suite', state: 'success', description: 'not needed: nothing the Mac suite covers changed' }]);
  const done = stubGithub({ prFiles: ['Makefile'], statuses: { [HEAD]: PASS } });
  await run({ github: done.github, context, core });
  assert.deepEqual(done.calls.posted, []);
});

test('a companion repo passes its own covered list, and the rules apply to it', async () => {
  const covered = [/^crates\//, /^open$/, /^editors\/studio\//];
  assert.ok(isCovered('open', covered) && isCovered('editors/studio/src/x.ts', covered));
  assert.ok(!isCovered('editors/vscode/src/x.ts', covered), 'the default list no longer applies');
  const context = { repo, payload: { pull_request: pr() } };
  const core = { info: () => {} };
  const bump = stubGithub({ prFiles: ['open'] });
  await run({ github: bump.github, context, core }, { covered });
  assert.equal(bump.calls.posted[0].state, 'pending', 'a submodule bump needs the Mac suite');
  const ext = stubGithub({ prFiles: ['editors/vscode/src/x.ts'] });
  await run({ github: ext.github, context, core }, { covered });
  assert.equal(ext.calls.posted[0].state, 'success', 'not covered in the companion: not needed');
  const carried = stubGithub({
    prFiles: ['open', 'doc/x.md'],
    commits: [sha(1), HEAD],
    statuses: { [sha(1)]: PASS },
    compare: { [`${sha(1)}...${HEAD}`]: { status: 'ahead', files: ['doc/x.md'] } },
  });
  assert.equal((await decide({ github: carried.github, repo, pr: pr(), covered })).state, 'success', 'carried past an uncovered change');
});
