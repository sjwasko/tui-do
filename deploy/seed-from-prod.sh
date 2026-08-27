#!/usr/bin/env bash
# Seed the tui-do dev Vikunja from a production export.
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
#        PUT  /migration/vikunja-file/migrate  -> import into dev

set -euo pipefail

PROD_URL="${TUI_DO_PROD_URL:-https://sw-hp2.tail9803a5.ts.net:8443}"
DEV_URL="${TUI_DO_DEV_URL:-https://sw-surface.tail9803a5.ts.net:8443}"
POLL_SECONDS="${TUI_DO_POLL_SECONDS:-10}"
POLL_TRIES="${TUI_DO_POLL_TRIES:-60}"

die() { printf 'error: %s\n' "$1" >&2; exit 1; }

[ "$PROD_URL" != "$DEV_URL" ] || die "PROD_URL and DEV_URL are identical -- refusing to run"
case "$DEV_URL" in
  *sw-hp2*) die "DEV_URL points at production (sw-hp2) -- refusing to import into prod" ;;
esac

printf 'Source (read-only): %s\n' "$PROD_URL"
printf 'Target (written)  : %s\n\n' "$DEV_URL"

read -rp  'Production username: ' PROD_USER
read -rsp 'Production password: ' PROD_PASS; echo
read -rp  'Dev username: ' DEV_USER
read -rsp 'Dev password: ' DEV_PASS; echo
echo

# --- authenticate against prod -------------------------------------------------
login=$(curl -sS -X POST "$PROD_URL/api/v1/login" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg u "$PROD_USER" --arg p "$PROD_PASS" '{username:$u,password:$p}')")
prod_jwt=$(printf '%s' "$login" | jq -r '.token // empty')
[ -n "$prod_jwt" ] || die "login to production failed: $(printf '%s' "$login" | jq -r '.message // .' )"
printf 'authenticated to production as %s\n' "$PROD_USER"

# Authenticate to dev too. A JWT rather than an API token, because the migration
# routes are not necessarily within an API token's permission scopes, and a JWT
# unambiguously carries the user's full rights.
dev_login=$(curl -sS -X POST "$DEV_URL/api/v1/login" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg u "$DEV_USER" --arg p "$DEV_PASS" '{username:$u,password:$p}')")
dev_jwt=$(printf '%s' "$dev_login" | jq -r '.token // empty')
[ -n "$dev_jwt" ] || die "login to dev failed: $(printf '%s' "$dev_login" | jq -r '.message // .')"
printf 'authenticated to dev as %s\n' "$DEV_USER"

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
prod container logs:  ssh sw-hp2.tail9803a5.ts.net 'docker logs --tail 50 vikunja'"
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
# PUT, not POST. The OpenAPI document declares this operation as `post`, but the
# server answers 405 with `Allow: OPTIONS, PUT`. The spec is wrong here -- verified
# against the live v2.5.0 instance -- so the server wins.
# Capture the HTTP status separately. Polling a submission that was itself rejected
# is how a 405 turned into ten minutes of cheerful dots on the first attempt.
http_code=$(curl -sS -o "$tmp/import.json" -w '%{http_code}' \
  -X PUT "$DEV_URL/api/v1/migration/vikunja-file/migrate" \
  -H "Authorization: Bearer $dev_jwt" \
  -F "import=@$tmp/export.zip")

if [ "$http_code" != "200" ]; then
  printf 'response body: %s\n' "$(head -c 400 "$tmp/import.json")" >&2
  die "import was rejected with HTTP $http_code -- not polling for a migration that never started"
fi
printf 'import accepted: %s\n' "$(jq -r '.message // "ok"' "$tmp/import.json")"
printf 'polling dev for completion'

for _ in $(seq 1 "$POLL_TRIES"); do
  printf '.'
  sleep "$POLL_SECONDS"
  done_at=$(curl -sS "$DEV_URL/api/v1/migration/vikunja-file/status" \
    -H "Authorization: Bearer $dev_jwt" | jq -r '.finished_at // empty')
  case "$done_at" in
    ''|null|0001-01-01*) ;;
    *) printf ' done (%s)\n' "$done_at"; break ;;
  esac
done

# Verify something actually arrived, rather than trusting the status field.
projects=$(curl -sS "$DEV_URL/api/v1/projects" -H "Authorization: Bearer $dev_jwt" | jq 'length')
tasks=$(curl -sS "$DEV_URL/api/v1/tasks?per_page=1" -H "Authorization: Bearer $dev_jwt" -D "$tmp/h" -o /dev/null \
  && grep -i '^x-pagination-result-count' "$tmp/h" | tr -d '\r' | awk '{print $2}')
printf '\ndev now has %s projects; first page reports %s task(s)\n' "${projects:-?}" "${tasks:-?}"
[ "${projects:-0}" -gt 1 ] || printf 'WARNING: only %s project(s) -- the import may not have taken\n' "${projects:-0}" >&2

printf '\nSeeded. Capture this state as the reset baseline:\n  ./snapshot-dev.sh\n'
