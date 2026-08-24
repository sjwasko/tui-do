#!/usr/bin/env bash
# Seed the criax dev Vikunja from a production export.
#
# PRODUCTION IS READ ONLY. The only thing this asks prod to do is build an export of
# your own account; nothing is modified and nothing is imported there. The script
# refuses to run if the two URLs are the same or if the target looks like prod.
#
# Vikunja's export endpoints require your ACCOUNT PASSWORD, not an API token. The
# script prompts for it, keeps it in a shell variable for the duration, and never
# writes it to disk or passes it on a command line (where it would show up in `ps`).
#
# Flow:  GET  /user/export           -> is one already built?
#        POST /user/export/request   -> if not, ask for one and wait
#        POST /user/export/download  -> fetch the zip
#        POST /migration/vikunja-file/migrate  -> import into dev

set -euo pipefail

PROD_URL="${CRIAX_PROD_URL:-https://prod-box.example.net:8443}"
DEV_URL="${CRIAX_DEV_URL:-https://dev-box.example.net:8443}"
POLL_SECONDS="${CRIAX_POLL_SECONDS:-10}"
POLL_TRIES="${CRIAX_POLL_TRIES:-60}"

die() { printf 'error: %s\n' "$1" >&2; exit 1; }

[ "$PROD_URL" != "$DEV_URL" ] || die "PROD_URL and DEV_URL are identical -- refusing to run"
case "$DEV_URL" in
  *prod-box*) die "DEV_URL points at production (prod-box) -- refusing to import into prod" ;;
esac

printf 'Source (read-only): %s\n' "$PROD_URL"
printf 'Target (written)  : %s\n\n' "$DEV_URL"

read -rp  'Production username: ' PROD_USER
read -rsp 'Production password: ' PROD_PASS; echo
read -rsp 'Dev API token (Settings -> API Tokens on the dev instance): ' DEV_TOKEN; echo
echo

# --- authenticate against prod -------------------------------------------------
login=$(curl -sS -X POST "$PROD_URL/api/v1/login" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg u "$PROD_USER" --arg p "$PROD_PASS" '{username:$u,password:$p}')")
prod_jwt=$(printf '%s' "$login" | jq -r '.token // empty')
[ -n "$prod_jwt" ] || die "login to production failed: $(printf '%s' "$login" | jq -r '.message // .' )"
printf 'authenticated to production as %s\n' "$PROD_USER"

# Export status lives at GET /user/export and returns {id, created, expires, size}.
# `id` is 0 until an export has been built. (Not on GET /user -- user.User has no
# export field at all.)
export_status() {
  curl -sS "$PROD_URL/api/v1/user/export" -H "Authorization: Bearer $prod_jwt"
}

status=$(export_status)
export_id=$(printf '%s' "$status" | jq -r '.id // 0')

if [ "$export_id" != "0" ] && [ "$export_id" != "null" ]; then
  printf 'an export already exists (built %s, %s bytes) -- reusing it\n' \
    "$(printf '%s' "$status" | jq -r '.created // "?"')" \
    "$(printf '%s' "$status" | jq -r '.size // 0')"
else
  curl -sS -X POST "$PROD_URL/api/v1/user/export/request" \
    -H "Authorization: Bearer $prod_jwt" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg p "$PROD_PASS" '{password:$p}')" >/dev/null
  printf 'export requested; Vikunja builds it in the background'

  for _ in $(seq 1 "$POLL_TRIES"); do
    printf '.'
    sleep "$POLL_SECONDS"
    status=$(export_status)
    export_id=$(printf '%s' "$status" | jq -r '.id // 0')
    [ "$export_id" = "0" ] || [ "$export_id" = "null" ] || { printf ' ready\n'; break; }
  done

  if [ "$export_id" = "0" ] || [ "$export_id" = "null" ]; then
    printf '\n'
    printf 'last status from %s/api/v1/user/export was: %s\n' "$PROD_URL" "$status" >&2
    die "export was not ready after $((POLL_SECONDS * POLL_TRIES))s.
Vikunja builds exports in a background worker; if it never completes, check the
prod container logs:  ssh prod-box.example.net 'docker logs --tail 50 vikunja'"
  fi
fi

# --- download it ---------------------------------------------------------------
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -sS -X POST "$PROD_URL/api/v1/user/export/download" \
  -H "Authorization: Bearer $prod_jwt" -H 'Content-Type: application/json' \
  -d "$(jq -n --arg p "$PROD_PASS" '{password:$p}')" \
  -o "$tmp/export.zip"
[ -s "$tmp/export.zip" ] || die "downloaded export is empty"
# A JSON error body would also be non-empty, so check it is actually a zip.
file "$tmp/export.zip" | grep -qi zip \
  || die "download was not a zip: $(head -c 200 "$tmp/export.zip")"
printf 'downloaded %s\n' "$(du -h "$tmp/export.zip" | cut -f1)"

# --- import into dev -----------------------------------------------------------
import=$(curl -sS -X POST "$DEV_URL/api/v1/migration/vikunja-file/migrate" \
  -H "Authorization: Bearer $DEV_TOKEN" \
  -F "import=@$tmp/export.zip")
printf 'import submitted: %s\n' "$(printf '%s' "$import" | jq -r '.message // .' )"
printf 'polling dev for completion'

for _ in $(seq 1 "$POLL_TRIES"); do
  printf '.'
  sleep "$POLL_SECONDS"
  done_at=$(curl -sS "$DEV_URL/api/v1/migration/vikunja-file/status" \
    -H "Authorization: Bearer $DEV_TOKEN" | jq -r '.finished_at // empty')
  case "$done_at" in
    ''|null|0001-01-01*) ;;
    *) printf ' done (%s)\n' "$done_at"; break ;;
  esac
done

printf '\nSeeded. Capture this state as the reset baseline:\n  ./snapshot-dev.sh\n'
