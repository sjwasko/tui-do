#!/usr/bin/env bash
# Run the criax-api live tests against the dev Vikunja using a scoped API token.
#
# The token is never passed on a command line (where `ps` would show it) and never
# written to disk by this script -- the same property `deploy/seed-from-prod.sh`
# preserves. It is taken, in order, from:
#
#   1. $CRIAX_TEST_TOKEN, if already exported;
#   2. the first line of the file given as $1, if one is given;
#   3. an interactive prompt, if this is running on a terminal.
#
# Create the token in the web UI under Settings -> API tokens. The round-trip test
# creates and deletes tasks, projects, labels and comments, so it needs read and write
# on all four -- tick everything, and delete the token when the run is done.
#
# Usage:
#   ./run-live-tests-token.sh                  # prompts for the token
#   ./run-live-tests-token.sh ~/.criax-token   # reads it from a file
#   CRIAX_TEST_TOKEN=tk_... ./run-live-tests-token.sh

set -euo pipefail

DEV_URL="https://sw-surface.tail9803a5.ts.net:8443"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Belt and braces on top of the guard inside the tests themselves.
case "$DEV_URL" in
  *sw-hp2*) echo "refusing to run against prod" >&2; exit 1 ;;
esac

if [ -z "${CRIAX_TEST_TOKEN:-}" ]; then
  if [ $# -ge 1 ]; then
    [ -r "$1" ] || { echo "cannot read token file: $1" >&2; exit 1; }
    CRIAX_TEST_TOKEN="$(head -n1 "$1" | tr -d '[:space:]')"
  elif [ -t 0 ]; then
    read -rsp "Vikunja API token for ${DEV_URL}: " CRIAX_TEST_TOKEN
    echo
  else
    echo "No token. Export CRIAX_TEST_TOKEN, pass a file as \$1, or run this on a terminal." >&2
    exit 1
  fi
fi
[ -n "$CRIAX_TEST_TOKEN" ] || { echo "token is empty" >&2; exit 1; }
export CRIAX_TEST_TOKEN

export CRIAX_TEST_URL="$DEV_URL"

# --test-threads=1 because every test shares one dev instance: the full-fetch test walks
# 78 pages while the round-trip test is creating and deleting things.
cd "$REPO"
exec cargo test -p criax-api --test live -- --nocapture --test-threads=1
