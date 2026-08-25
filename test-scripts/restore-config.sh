#!/usr/bin/env bash
#
# MANUAL-CHECKS A1 — put the real server URL back after go-offline.sh.
#
# Restores from the backup that go-offline.sh took, rather than writing a
# remembered URL back: a hardcoded value here would go stale the moment the
# dev instance moves.

set -euo pipefail

config="${CRIAX_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/criax/config.yaml}"
backup="$config.pre-offline"

if [[ ! -e $backup ]]; then
	echo "no backup at $backup — nothing to restore." >&2
	echo "Either go-offline.sh was never run, or this already ran." >&2
	current=$(awk '/^server:/{s=1;next} /^[^[:space:]#]/{s=0} s&&/^[[:space:]]*url:/{print $2;exit}' "$config" 2>/dev/null || true)
	[[ -n ${current:-} ]] && echo "server.url is currently $current" >&2
	exit 1
fi

before=$(awk '/^server:/{s=1;next} /^[^[:space:]#]/{s=0} s&&/^[[:space:]]*url:/{print $2;exit}' "$config" 2>/dev/null || true)

mv "$backup" "$config"

after=$(awk '/^server:/{s=1;next} /^[^[:space:]#]/{s=0} s&&/^[[:space:]]*url:/{print $2;exit}' "$config")

echo "config:  $config"
echo "was:     ${before:-<unreadable>}"
echo "now:     $after"
echo
echo "Anything queued while offline goes out on the next criax run or sync."
