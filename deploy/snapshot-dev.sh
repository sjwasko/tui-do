#!/usr/bin/env bash
# Capture the current dev database as the reset-to baseline.
# Run this immediately after seed-from-prod.sh.
#
# Runs from anywhere: the pg_dump happens on the dev host over SSH, and the dump is
# left there next to the data it belongs to.
set -euo pipefail
HOST="${TUI_DO_DEV_HOST:-sw-surface.tail9803a5.ts.net}"
ROOT="${TUI_DO_DEV_ROOT:-/opt/appdata/tui-do-dev}"

run() {
  if [ -f /opt/stacks/tui-do-dev/docker-compose.yml ] && command -v docker >/dev/null 2>&1; then
    bash -c "$1"                                    # already on the dev host
  else
    ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" "$1"
  fi
}

run "docker exec tui-do-vikunja-db pg_dump -U vikunja --clean --if-exists vikunja > $ROOT/seed.sql"
size=$(run "du -h $ROOT/seed.sql | cut -f1")
printf 'baseline written on %s: %s (%s)\n' "$HOST" "$ROOT/seed.sql" "$size"
