#!/usr/bin/env bash
# Capture the current dev database as the reset-to baseline.
# Run this immediately after seed-from-prod.sh.
set -euo pipefail
ROOT="${CRIAX_DEV_ROOT:-/opt/appdata/criax-dev}"
docker exec criax-vikunja-db pg_dump -U "${CRIAX_DB_USER:-vikunja}" \
  --clean --if-exists "${CRIAX_DB_NAME:-vikunja}" > "$ROOT/seed.sql"
printf 'baseline written: %s (%s)\n' "$ROOT/seed.sql" "$(du -h "$ROOT/seed.sql" | cut -f1)"
