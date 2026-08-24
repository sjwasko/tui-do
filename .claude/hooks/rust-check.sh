#!/usr/bin/env bash
# PostToolUse hook: format the edited Rust file, then lint the workspace.
#
# Reads the Claude Code hook payload on stdin, formats the touched file with
# rustfmt, and runs clippy across the workspace. Exits 2 with the diagnostics on
# stderr when clippy fails, so the failure is reported back rather than silently
# accumulating -- CI runs clippy with -D warnings and should never be the first
# place a lint shows up.

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

file=$(jq -r '.tool_response.filePath // .tool_input.file_path // empty' 2>/dev/null)
[ -n "$file" ] || exit 0

# Only Rust sources inside this repo.
case "$file" in
  "$REPO"/*.rs) ;;
  *) exit 0 ;;
esac
[ -f "$file" ] || exit 0

rustfmt --edition 2021 "$file" 2>/dev/null

cd "$REPO" || exit 0
if ! out=$(cargo clippy --workspace --all-targets --message-format short 2>&1); then
  printf 'clippy failed:\n' >&2
  printf '%s\n' "$out" | grep -E '^[^ ].*(error|warning)' | head -30 >&2
  exit 2
fi
exit 0
