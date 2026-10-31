#!/usr/bin/env bash
# PostToolUse hook (ADR 0034): after Claude edits a piece's doc (pieces/<slug>/<slug>.json5), check it
# strictly with engines-check — the same rules the engine and engines-live load it by.
#   errors   → exit 2 with the report on stderr (Claude sees it and must fix the doc; engines-live keeps
#              playing the last good revision meanwhile)
#   unreadable doc (engines-check exit 2) → exit 2 with its message on stderr
#   warnings → the report goes back to Claude as context (the doc still loads)
#   engines-check unavailable (not built and the build fails, or it can't run) → a note goes back to
#              Claude as context saying the edit went UNCHECKED (never silent)
#   clean / not a piece doc → silent
# Requires `jq` (brew install jq) to read the hook input; without it a piece-doc edit is reported on
# stderr (exit 2) as unchecked. ENGINES_CHECK=<path> overrides the engines-check binary (no build is tried).
set -u
input=$(cat)

if command -v jq >/dev/null 2>&1; then
  file=$(printf '%s' "$input" | jq -r '.tool_input.file_path // .tool_response.filePath // empty')
else  # a crude read, only to tell whether the unchecked edit was a piece doc
  file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
fi
[ -n "$file" ] || exit 0
case "$file" in
  */pieces/*/*.json5|pieces/*/*.json5) ;;
  *) exit 0 ;;
esac
dir=$(basename "$(dirname "$file")")
[ "$(basename "$file" .json5)" = "$dir" ] || exit 0   # only the piece's own doc
[ -f "$file" ] || exit 0
if ! command -v jq >/dev/null 2>&1; then
  echo "check-piece-doc: jq not found (brew install jq) — $file was NOT checked by engines-check; run \`make check DOC=$file\`" >&2
  exit 2
fi

# The edit went unchecked: tell Claude (as context — the doc itself may be fine) instead of staying silent.
unchecked() {
  jq -n --arg ctx "engines-check unavailable — $file was NOT checked: $1
Check it by hand (make check DOC=$file) before relying on it; a live player still refuses a doc with errors." \
    '{hookSpecificOutput: {hookEventName: "PostToolUse", additionalContext: $ctx}}'
  exit 0
}

root=${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}
if [ -n "${ENGINES_CHECK:-}" ]; then
  check=$ENGINES_CHECK
  [ -x "$check" ] || unchecked "ENGINES_CHECK=$check is not an executable"
else
  check="$root/target/release/engines-check"
  [ -x "$check" ] || check="$root/target/debug/engines-check"
  if [ ! -x "$check" ]; then
    if ! build=$(cd "$root" && cargo build -q --release -p engines_authoring --bin engines-check 2>&1); then
      unchecked "it isn't built and \`cargo build --release -p engines_authoring --bin engines-check\` failed:
$(printf '%s\n' "$build" | tail -n 15)"
    fi
    check="$root/target/release/engines-check"
    [ -x "$check" ] || unchecked "the build succeeded but $check is missing"
  fi
fi

report=$("$check" "$file" 2>&1)
status=$?
case "$status" in
  0) ;;
  1)
    printf 'engines-check refused %s — fix these before it can play (engines-live keeps the last good revision):\n%s\n' "$file" "$report" >&2
    exit 2 ;;
  2)
    printf 'engines-check could not read %s:\n%s\n' "$file" "$report" >&2
    exit 2 ;;
  *)
    unchecked "$check exited $status:
$(printf '%s\n' "$report" | tail -n 15)" ;;
esac
if printf '%s' "$report" | grep -q '^  warning'; then
  jq -n --arg ctx "engines-check: $report" '{hookSpecificOutput: {hookEventName: "PostToolUse", additionalContext: $ctx}}'
fi
exit 0
