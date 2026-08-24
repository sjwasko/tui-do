#!/usr/bin/env bash
# Deploy (or redeploy) the criax dev Vikunja to sw-surface, from the workstation.
#
# Idempotent: re-running syncs the compose file and restarts. Secrets are generated
# once on first run and preserved afterwards -- rerunning will not rotate the JWT
# secret out from under existing sessions or orphan the database password.
#
# Prerequisite (needs a sudo password, so run it on the host yourself):
#   sudo mkdir -p /opt/stacks/criax-dev /opt/appdata/criax-dev/{db,files}
#   sudo chown -R "$USER:$USER" /opt/stacks/criax-dev /opt/appdata/criax-dev

set -euo pipefail

HOST="${CRIAX_DEV_HOST:-sw-surface.tail9803a5.ts.net}"
STACK=/opt/stacks/criax-dev
DATA=/opt/appdata/criax-dev
PORT=8443
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '\n== %s\n' "$1"; }
ssh_() { ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" "$@"; }

say "checking prerequisites on $HOST"
ssh_ "test -d $STACK -a -w $STACK" || {
  cat >&2 <<EOF
error: $STACK does not exist or is not writable.

Run this on $HOST first (it needs a sudo password, which this script cannot supply):

  sudo mkdir -p $STACK $DATA/{db,files}
  sudo chown -R "\$USER:\$USER" $STACK $DATA
EOF
  exit 1
}
ssh_ "test -d $DATA/db -a -d $DATA/files -a -w $DATA" ||
  { echo "error: $DATA/{db,files} missing or not writable" >&2; exit 1; }

say "syncing compose file"
scp -q "$HERE/docker-compose.yml" "$HOST:$STACK/docker-compose.yml"

say "ensuring secrets exist (generated once, then preserved)"
ssh_ "bash -s" <<EOF
set -euo pipefail
cd $STACK
umask 077
if [ ! -f .env ]; then
  printf 'CRIAX_DEV_ROOT=%s\nCRIAX_DB_USER=vikunja\nCRIAX_DB_NAME=vikunja\nCRIAX_DB_PASSWORD=%s\n' \
    "$DATA" "\$(openssl rand -hex 24)" > .env
  echo "  generated .env"
else
  echo "  .env already present, kept"
fi
if [ ! -f vikunja.env ]; then
  db_pass=\$(grep '^CRIAX_DB_PASSWORD=' .env | cut -d= -f2-)
  cat > vikunja.env <<INNER
VIKUNJA_DATABASE_TYPE=postgres
VIKUNJA_DATABASE_HOST=vikunja-db
VIKUNJA_DATABASE_DATABASE=vikunja
VIKUNJA_DATABASE_USER=vikunja
VIKUNJA_DATABASE_PASSWORD=\$db_pass
VIKUNJA_SERVICE_PUBLICURL=https://$HOST:$PORT/
VIKUNJA_SERVICE_JWTSECRET=\$(openssl rand -hex 32)
VIKUNJA_SERVICE_ENABLEREGISTRATION=true
VIKUNJA_SERVICE_TIMEZONE=America/New_York
VIKUNJA_LOG_LEVEL=INFO
INNER
  echo "  generated vikunja.env"
else
  echo "  vikunja.env already present, kept"
fi
chmod 600 .env vikunja.env
EOF

say "starting containers"
ssh_ "cd $STACK && docker compose up -d"

say "waiting for the API to answer"
ok=""
for _ in $(seq 1 30); do
  sleep 4
  if ssh_ "curl -sf -o /dev/null http://127.0.0.1:3456/api/v1/info" 2>/dev/null; then ok=1; break; fi
  printf '.'
done
[ -n "$ok" ] || { echo; echo "error: API did not come up; check 'docker compose logs' on $HOST" >&2; exit 1; }
echo " up"

say "publishing on the tailnet"
# Reclaim :8443 and drop two stale mappings whose backends no longer exist
# (:443 -> 3030 was Forgejo, :8444 -> 8089 unknown; verified dead 2026-08-24).
ssh_ "tailscale serve --bg --https $PORT http://127.0.0.1:3456" >/dev/null
ssh_ "tailscale serve --https 443 off"  2>/dev/null || true
ssh_ "tailscale serve --https 8444 off" 2>/dev/null || true
ssh_ "tailscale serve status"

say "verifying from this workstation"
version=$(curl -sS "https://$HOST:$PORT/api/v1/info" | jq -r '.version')
printf 'dev Vikunja reachable at https://%s:%s  (version %s)\n' "$HOST" "$PORT" "$version"

cat <<EOF

Next:
  1. Open https://$HOST:$PORT and register the first account.
  2. Close registration:
       ssh $HOST "sed -i s/ENABLEREGISTRATION=true/ENABLEREGISTRATION=false/ $STACK/vikunja.env && cd $STACK && docker compose up -d"
  3. Create an API token in Settings -> API Tokens, then seed:
       ./seed-from-prod.sh && ./snapshot-dev.sh
EOF
