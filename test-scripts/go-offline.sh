#!/usr/bin/env bash
#
# MANUAL-CHECKS A1 — point tui-do at an unroutable address so the connect hangs.
#
# Rewrites server.url in the live config and keeps a backup beside it. The store
# lives at ~/.local/share/tui-do/tui-do.db and does not depend on the URL, so the
# cache stays warm — which is the whole point of the check.
#
# Undo with restore-config.sh in this directory. Run that before this one again:
# the backup is only taken when there isn't one, so a second run cannot bury the
# real URL.

set -euo pipefail

# Unroutable, not refused. A refused connection fails in milliseconds and proves
# nothing; a hanging connect is what froze the predecessor.
readonly OFFLINE_URL="https://10.255.255.1:8443"

config="${TUI_DO_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/tui-do/config.yaml}"
backup="$config.pre-offline"

[[ -f $config ]] || { echo "no config at $config" >&2; exit 1; }

if [[ -e $backup ]]; then
	echo "$backup already exists — restore-config.sh has not been run." >&2
	echo "Refusing, so the real URL in that backup is not overwritten." >&2
	exit 1
fi

before=$(awk '/^server:/{s=1;next} /^[^[:space:]#]/{s=0} s&&/^[[:space:]]*url:/{print $2;exit}' "$config")
[[ -n $before ]] || { echo "no server.url found in $config" >&2; exit 1; }

cp -p "$config" "$backup"

# Only the url: inside the server: block — awk tracks the block rather than
# trusting that no other key is spelled the same.
awk -v new="$OFFLINE_URL" '
	/^server:/            { in_server = 1; print; next }
	/^[^[:space:]#]/      { in_server = 0 }
	in_server && /^[[:space:]]*url:/ && !done {
		sub(/url:.*/, "url: " new); done = 1; print; next
	}
	{ print }
' "$config" >"$config.tmp"

after=$(awk '/^server:/{s=1;next} /^[^[:space:]#]/{s=0} s&&/^[[:space:]]*url:/{print $2;exit}' "$config.tmp")
if [[ $after != "$OFFLINE_URL" ]]; then
	rm -f "$config.tmp" "$backup"
	echo "rewrite failed — config left untouched" >&2
	exit 1
fi

mv "$config.tmp" "$config"

echo "config:  $config"
echo "backup:  $backup"
echo "was:     $before"
echo "now:     $after"
echo
echo "tui-do will now hang on connect. Edits you make go to the outbox and will"
echo "drain against the real server once restore-config.sh puts the URL back."
