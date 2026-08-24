#!/usr/bin/env bash
# Run the criax-api live tests against the dev Vikunja using a username and password.
#
# Use this when the API token path fails: some Vikunja routes accept only a JWT, and a
# scoped token cannot reach them. This path exercises POST /login and the refresh-cookie
# handling, which the token path never touches.
#
# The password is never passed on a command line and never written to disk. It is taken,
# in order, from $CRIAX_TEST_PASSWORD, then an interactive prompt.
#
# Usage:
#   ./run-live-tests-password.sh admin
#   CRIAX_TEST_USERNAME=admin CRIAX_TEST_PASSWORD=... ./run-live-tests-password.sh

set -euo pipefail

DEV_URL="https://sw-surface.tail9803a5.ts.net:8443"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

case "$DEV_URL" in
  *sw-hp2*) echo "refusing to run against prod" >&2; exit 1 ;;
esac

CRIAX_TEST_USERNAME="${1:-${CRIAX_TEST_USERNAME:-}}"
if [ -z "$CRIAX_TEST_USERNAME" ]; then
  if [ -t 0 ]; then
    read -rp "Vikunja username on ${DEV_URL}: " CRIAX_TEST_USERNAME
  else
    echo "No username. Pass it as \$1 or export CRIAX_TEST_USERNAME." >&2
    exit 1
  fi
fi

if [ -z "${CRIAX_TEST_PASSWORD:-}" ]; then
  if [ -t 0 ]; then
    read -rsp "Password for ${CRIAX_TEST_USERNAME}: " CRIAX_TEST_PASSWORD
    echo
  else
    echo "No password. Export CRIAX_TEST_PASSWORD or run this on a terminal." >&2
    exit 1
  fi
fi
[ -n "$CRIAX_TEST_PASSWORD" ] || { echo "password is empty" >&2; exit 1; }

export CRIAX_TEST_USERNAME CRIAX_TEST_PASSWORD
export CRIAX_TEST_URL="$DEV_URL"

cd "$REPO"
exec cargo test -p criax-api --test live -- --nocapture --test-threads=1
