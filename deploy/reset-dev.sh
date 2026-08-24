#!/usr/bin/env bash
# Restore the dev database to the seeded baseline. Destructive by design -- that is
# the point: destructive test runs against dev should be cheap to undo.
#
# Runs from anywhere; the work happens on the dev host over SSH.
set -euo pipefail
HOST="${CRIAX_DEV_HOST:-sw-surface.tail9803a5.ts.net}"
ROOT="${CRIAX_DEV_ROOT:-/opt/appdata/criax-dev}"

case "$HOST" in
  *sw-hp2*) echo "refusing to reset production" >&2; exit 1 ;;
esac

run() {
  if [ -f /opt/stacks/criax-dev/docker-compose.yml ] && command -v docker >/dev/null 2>&1; then
    bash -c "$1"
  else
    ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" "$1"
  fi
}

run "test -f $ROOT/seed.sql" || {
  echo "no baseline at $ROOT/seed.sql on $HOST -- run snapshot-dev.sh first" >&2; exit 1; }

run "docker stop criax-vikunja >/dev/null
     docker exec -i criax-vikunja-db psql -U vikunja -d vikunja -q >/dev/null < $ROOT/seed.sql
     docker start criax-vikunja >/dev/null"
printf 'dev database restored to baseline\n'
