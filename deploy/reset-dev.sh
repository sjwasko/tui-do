#!/usr/bin/env bash
# Restore the dev database to the seeded baseline. Destructive by design -- that is
# the point: destructive test runs against dev should be cheap to undo.
set -euo pipefail
ROOT="${CRIAX_DEV_ROOT:-/opt/appdata/criax-dev}"
[ -f "$ROOT/seed.sql" ] || { echo "no baseline at $ROOT/seed.sql -- run snapshot-dev.sh first" >&2; exit 1; }
docker stop criax-vikunja >/dev/null
docker exec -i criax-vikunja-db psql -U "${CRIAX_DB_USER:-vikunja}" \
  -d "${CRIAX_DB_NAME:-vikunja}" -q < "$ROOT/seed.sql"
docker start criax-vikunja >/dev/null
printf 'dev database restored to baseline\n'
