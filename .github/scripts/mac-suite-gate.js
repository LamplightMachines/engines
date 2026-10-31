// The `mac-suite gate` job (CONTRIBUTING.md): what CI posts as the `mac-suite` commit status on
// a PR's new head, before or instead of a `make test-mac` run on the Mac.
//
// In order:
//   1. the head already has a `mac-suite` status → leave it (make test-mac may have posted it);
//   2. the PR's diff against base touches nothing the Mac suite covers → success, "not needed";
//   3. the newest `make test-mac` result in the PR is a pass, the head is strictly ahead of that
//      commit, and nothing covered changed between them → success, carried from that commit;
//   4. otherwise → pending: run make test-mac.
// A carry is made only when it's provably safe: a failure as the newest result, a force-push or
// rebase (the commit isn't an ancestor any more), a truncated compare or a PR too long to list all
// fall back to pending.

// What the Mac suite covers: Rust (the goldens), the fixtures the goldens render, the schema, the
// VS Code extension (its end-to-end suite), the Swift shell, and the two files that define the suite
// itself (the Makefile targets and mac-suite.sh).
const COVERED = [/^crates\//, /^Cargo\.(toml|lock)$/, /^lab\/tests\/fixtures\//, /^contracts\//, /^editors\/vscode\//, /^shell\//, /^Makefile$/, /^\.github\/scripts\/mac-suite\.sh$/];
const CONTEXT = 'mac-suite';
// mac-suite.sh's success description; its failures are any `failure` (the gate never posts one).
const PASSED = /^passed on macOS/;
// pulls.listCommits lists at most 250 commits, oldest first; compareCommits at most 300 files.
const MAX_COMMITS = 250;
const MAX_FILES = 300;
// A commit status description holds at most 140 characters.
const MAX_DESCRIPTION = 140;

const isCovered = (path) => COVERED.some((re) => re.test(path));

/** The paths a changed file touches that the Mac suite covers: a rename counts by both names. */
const coveredPaths = (files) =>
  files.flatMap((f) => [f.filename, f.previous_filename].filter(Boolean)).filter(isCovered);

/** A `make test-mac` result, as opposed to the gate's own statuses or a run still in progress. */
const isMacResult = (s) => s.state === 'failure' || (s.state === 'success' && PASSED.test(s.description ?? ''));

/**
 * Where a pass can be carried from: walks `commits` (oldest first, as the API lists them, head last)
 * from newest to oldest, skipping the head, and stops at the first one whose latest `mac-suite`
 * status is a make test-mac result. Returns `{ sha, description }` when that result is a pass, else
 * null with the reason.
 */
async function carrySource({ commits, latestStatus }) {
  for (const sha of commits.slice(0, -1).reverse()) {
    const status = await latestStatus(sha);
    if (!status || !isMacResult(status)) continue;
    return status.state === 'success'
      ? { sha, description: status.description }
      : { sha: null, reason: `the newest make test-mac result (${sha.slice(0, 7)}) is a failure` };
  }
  return { sha: null, reason: 'no earlier commit passed make test-mac' };
}

/** Whether a compare of the pass's commit to the head allows the carry; the reason when it doesn't. */
function carryBlocker(compare) {
  if (compare.status !== 'ahead') return `the head isn't ahead of it (${compare.status}: a force-push or rebase)`;
  const files = compare.files ?? [];
  if (files.length >= MAX_FILES) return `the compare lists ${files.length} files and may be truncated`;
  const hits = coveredPaths(files);
  if (hits.length) return `${hits.length} file(s) it covers changed since`;
  return null;
}

/** Decides the status for the PR's head: `{ state, description }`, or null to leave it as is. */
async function decide({ github, repo, pr, log = () => {} }) {
  const sha = pr.head.sha;
  const latestStatus = async (ref) => {
    const { data } = await github.rest.repos.getCombinedStatusForRef({ ...repo, ref });
    return data.statuses.find((s) => s.context === CONTEXT) ?? null;
  };

  const existing = await latestStatus(sha);
  if (existing) {
    log(`mac-suite is already ${existing.state} on ${sha}; left as is`);
    return null;
  }

  const files = await github.paginate(github.rest.pulls.listFiles, { ...repo, pull_number: pr.number, per_page: 100 });
  const hits = coveredPaths(files);
  if (!hits.length) return { state: 'success', description: 'not needed: nothing the Mac suite covers changed' };
  const pending = { state: 'pending', description: `run make test-mac on the Mac: ${hits.length} file(s) it covers changed` };

  if (pr.commits >= MAX_COMMITS) {
    log(`not carried: the PR has ${pr.commits} commits, more than listCommits shows`);
    return pending;
  }
  const listed = await github.paginate(github.rest.pulls.listCommits, { ...repo, pull_number: pr.number, per_page: 100 });
  const commits = listed.map((c) => c.sha);
  if (commits[commits.length - 1] !== sha) {
    log(`not carried: the PR's commits don't end at the head ${sha}`);
    return pending;
  }

  const source = await carrySource({ commits, latestStatus });
  if (!source.sha) {
    log(`not carried: ${source.reason}`);
    return pending;
  }
  const { data: compare } = await github.rest.repos.compareCommitsWithBasehead({ ...repo, basehead: `${source.sha}...${sha}` });
  const blocker = carryBlocker(compare);
  if (blocker) {
    log(`not carried from ${source.sha.slice(0, 7)}: ${blocker}`);
    return pending;
  }
  const description = `carried from ${source.sha.slice(0, 7)}: ${source.description}; nothing covered changed since`;
  return { state: 'success', description: description.length > MAX_DESCRIPTION ? `${description.slice(0, MAX_DESCRIPTION - 1)}…` : description };
}

/** The job's step: decides, then posts the status on the head. */
async function run({ github, context, core }) {
  const pr = context.payload.pull_request;
  const verdict = await decide({ github, repo: context.repo, pr, log: (m) => core.info(m) });
  if (!verdict) return;
  await github.rest.repos.createCommitStatus({ ...context.repo, sha: pr.head.sha, context: CONTEXT, ...verdict });
  core.info(`mac-suite: ${verdict.state} (${verdict.description})`);
}

module.exports = { COVERED, isCovered, coveredPaths, isMacResult, carrySource, carryBlocker, decide, run };
