#!/usr/bin/env bash
# Seed the criax dev Vikunja from a production export.
#
# PRODUCTION IS READ ONLY. This script only ever issues export requests against
# prod; it refuses to run if the two URLs are the same, and it never writes there.
#
# Vikunja's export endpoints require your ACCOUNT PASSWORD, not an API token. The
# script prompts for it, keeps it in a shell variable for the duration, and never
# writes it to disk or to the command line (so it cannot leak via `ps` or history).
#
# Flow:  POST /user/export/request  ->  wait for the export to be built
#     -> POST /user/export/download ->  POST /migration/vikunja-file/migrate on dev

set -euo pipefail

PROD_URL="${CRIAX_PROD_URL:-https://sw-hp2.tail9803a5.ts.net:8443}"
DEV_URL="${CRIAX_DEV_URL:-https://sw-surface.tail9803a5.ts.net:8443}"

die() { printf 'error: %s\n' "$1" >&2; exit 1; }

[ "$PROD_URL" != "$DEV_URL" ] || die "PROD_URL and DEV_URL are identical -- refusing to run"
case "$DEV_URL" in
  *sw-hp2*) die "DEV_URL points at production (sw-hp2) -- refusing to import into prod" ;;
esac

printf 'Source (read-only): %s\n' "$PROD_URL"
printf 'Target (written)  : %s\n\n' "$DEV_URL"

read -rp  'Production username: ' PROD_USER
read -rsp 'Production password: ' PROD_PASS; echo
read -rsp 'Dev API token (Settings -> API Tokens on the dev instance): ' DEV_TOKEN; echo

# --- authenticate against prod -------------------------------------------------
prod_jwt=$(curl -sS -X POST "$PROD_URL/api/v1/login" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg u "$PROD_USER" --arg p "$PROD_PASS" '{username:$u,password:$p}')" \
  | jq -r '.token // empty')
[ -n "$prod_jwt" ] || die "login to production failed"
printf 'authenticated to production\n'

# --- request the export --------------------------------------------------------
curl -sS -X POST "$PROD_URL/api/v1/user/export/request" \
  -H "Authorization: Bearer $prod_jwt" -H 'Content-Type: application/json' \
  -d "$(jq -n --arg p "$PROD_PASS" '{password:$p}')" >/dev/null
printf 'export requested; waiting for it to be built'

# Vikunja builds the export asynchronously. Poll /user until it reports a file.
for _ in $(seq 1 60); do
  sleep 5; printf '.'
  ready=$(curl -sS "$PROD_URL/api/v1/user" -H "Authorization: Bearer $prod_jwt" \
    | jq -r '.export_file_id // 0')
  [ "$ready" = "0" ] || { printf ' ready\n'; break; }
done
[ "${ready:-0}" != "0" ] || die "export was not ready after 5 minutes"

# --- download it ---------------------------------------------------------------
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -sS -X POST "$PROD_URL/api/v1/user/export/download" \
  -H "Authorization: Bearer $prod_jwt" -H 'Content-Type: application/json' \
  -d "$(jq -n --arg p "$PROD_PASS" '{password:$p}')" \
  -o "$tmp/export.zip"
[ -s "$tmp/export.zip" ] || die "downloaded export is empty"
printf 'downloaded %s\n' "$(du -h "$tmp/export.zip" | cut -f1)"

# --- import into dev -----------------------------------------------------------
curl -sS -X POST "$DEV_URL/api/v1/migration/vikunja-file/migrate" \
  -H "Authorization: Bearer $DEV_TOKEN" \
  -F "import=@$tmp/export.zip" >/dev/null
printf 'import submitted; polling status'
for _ in $(seq 1 60); do
  sleep 5; printf '.'
  st=$(curl -sS "$DEV_URL/api/v1/migration/vikunja-file/status" \
    -H "Authorization: Bearer $DEV_TOKEN" | jq -r '.finished_at // empty')
  [ -z "$st" ] || { printf ' done\n'; break; }
done

printf '\nSeeded. Snapshot this state as the reset baseline:\n  deploy/snapshot-dev.sh\n'
