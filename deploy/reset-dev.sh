#!/usr/bin/env bash
# Restore the dev database to the seeded baseline. Destructive by design -- that is
# the point: destructive test runs against dev should be cheap to undo.
#
# Runs from anywhere; the work happens on the dev host over SSH.
set -euo pipefail
HOST="${TUI_DO_DEV_HOST:-dev-box.example.net}"
ROOT="${TUI_DO_DEV_ROOT:-/opt/appdata/tui-do-dev}"

case "$HOST" in
  *prod-box*) echo "refusing to reset production" >&2; exit 1 ;;
esac

run() {
  if [ -f /opt/stacks/tui-do-dev/docker-compose.yml ] && command -v docker >/dev/null 2>&1; then
    bash -c "$1"
  else
    ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" "$1"
  fi
}

# Two different failures, because they call for opposite actions and this script once
# reported them as one. On 2026-08-30 the dev host was still carrying a directory name this
# script no longer looks for, so $ROOT did not exist, and the message sent a reader towards
# snapshot-dev.sh, which would have captured a dirty test database as the permanent
# baseline. A missing directory is a misconfiguration; a missing dump inside a directory
# that exists is a step not yet run.
run "test -d $ROOT" || {
  cat >&2 <<MSG
$ROOT does not exist on $HOST.

That is a configuration problem, not a missing baseline -- do NOT run snapshot-dev.sh to
"fix" it: it captures whatever is in the database right now and makes that the state
every future reset restores to.

Look for where the deployment actually lives (ls /opt/stacks /opt/appdata on $HOST) and
either point TUI_DO_DEV_ROOT at it or move it to the documented name. deploy/README.md
carries the rename commands.
MSG
  exit 1; }

run "test -f $ROOT/seed.sql" || {
  echo "no baseline at $ROOT/seed.sql on $HOST -- run snapshot-dev.sh first, but only" >&2
  echo "while the database holds a state worth returning to: it captures what is there" >&2
  echo "now, debris and all." >&2
  exit 1; }

# API tokens are carried across the restore, because a baseline is a snapshot of task
# data and a token is a credential -- restoring one should not revoke the other. The
# baseline here was dumped nine hours before the token this workstation authenticates
# with was created, so the first honest reset would have answered every later request
# 401 from a server that was otherwise reachable and healthy: the worst kind of failure
# to diagnose, because nothing looks broken.
#
# Saved, restored, and then the table is *replaced* rather than merged -- the token set
# after a reset is exactly the token set before it. Merging would need conflict handling
# for the rows the seed also carries, and swallowing duplicate-key errors to get it is
# how a script starts hiding real ones.
run "docker stop tui-do-vikunja >/dev/null
     docker exec tui-do-vikunja-db pg_dump -U vikunja -d vikunja --data-only --column-inserts \
       --table=api_tokens > /tmp/tui-do-tokens.sql
     docker exec -i tui-do-vikunja-db psql -U vikunja -d vikunja -q >/dev/null < $ROOT/seed.sql
     {
       echo 'TRUNCATE public.api_tokens;'
       grep '^INSERT INTO public.api_tokens' /tmp/tui-do-tokens.sql
       echo \"SELECT setval('api_tokens_id_seq', COALESCE((SELECT max(id) FROM api_tokens), 1));\"
     } | docker exec -i tui-do-vikunja-db psql -U vikunja -d vikunja -q -v ON_ERROR_STOP=1 >/dev/null
     rm -f /tmp/tui-do-tokens.sql
     docker start tui-do-vikunja >/dev/null"
printf 'dev database restored to baseline\n'
