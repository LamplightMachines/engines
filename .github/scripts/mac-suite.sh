#!/usr/bin/env bash
# The Mac half of the test split (CONTRIBUTING.md): everything GitHub's Linux CI can't run, with the result
# posted as the `mac-suite` commit status on HEAD, so a PR shows it ran on that exact commit.
#   make test-mac                  run, then post the status (needs `gh auth login`)
#   NO_STATUS=1 make test-mac      run only
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

if [[ "$(uname -s)" != Darwin ]]; then
  echo "make test-mac runs on macOS, where the goldens are blessed; on Linux run make test-ci" >&2
  exit 2
fi

sha=$(git rev-parse HEAD)
post=1
[[ -n "${NO_STATUS:-}" ]] && post=0
if [[ $post == 1 ]]; then
  if ! command -v gh >/dev/null; then
    echo "gh isn't installed: brew install gh, or run with NO_STATUS=1" >&2
    exit 2
  fi
  # Untracked files count too: cargo picks up a new test file, and a test can read a fixture
  # that was never added. Gitignored files (pieces/, target/, renders) don't show here.
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "uncommitted or untracked files: the status would describe a commit that wasn't tested. Commit them, or run with NO_STATUS=1" >&2
    exit 2
  fi
  if ! gh api --silent "repos/{owner}/{repo}/commits/$sha" 2>/dev/null; then
    echo "HEAD ($sha) isn't on GitHub yet: push it, or run with NO_STATUS=1" >&2
    exit 2
  fi
fi

status() { # <state> <description>
  [[ $post == 1 ]] || return 0
  gh api --silent -X POST "repos/{owner}/{repo}/statuses/$sha" \
    -f state="$1" -f context=mac-suite -f description="$2" || echo "couldn't post the mac-suite status" >&2
}
fail() {
  status failure "$1"
  echo "mac-suite: FAILED: $1" >&2
  exit 1
}

status pending "make test-mac is running"
echo "==> cargo test (the full Rust suite, Mac-pinned tests included)"
cargo test || fail "cargo test failed"
echo "==> VS Code: unit + end-to-end"
make vscode-test || fail "the VS Code tests failed"
if xcodebuild -version >/dev/null 2>&1; then
  echo "==> Swift shell"
  make shell || fail "the Swift shell didn't build"
else
  echo "==> Swift shell skipped: no full Xcode"
fi
status success "passed on macOS $(sw_vers -productVersion) $(uname -m)"
echo "mac-suite: passed for $sha"
