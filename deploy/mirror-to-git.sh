#!/usr/bin/env bash
#
# Get this repository's committed work off this machine.
#
#   local  ->  origin (Forgejo, prod-box)  ->  github (private mirror)
#
# Forgejo is where the work lives; GitHub is a copy that exists so one disk
# failure is not the end of the project. Run by tui-do-mirror.timer every eight
# hours, or by hand at any time.
#
# It pushes commits. It never *makes* them: a dirty working tree is
# work in progress, and committing that on a timer would put broken states into
# the history and hide the fact that nothing was backed up. Uncommitted work is
# reported and left alone.
#
# Usage: mirror-to-git.sh [repo-path]        (default: the repo this lives in)

set -euo pipefail

readonly UPSTREAM=origin   # Forgejo
readonly MIRROR=github     # private copy

repo="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$repo"

say() { echo "[mirror] $*"; }

git rev-parse --git-dir >/dev/null 2>&1 || { say "$repo is not a git repository"; exit 1; }

# A passphrase-free key does the pushing, so never sit at a prompt: under a
# systemd timer there is nobody to answer it and the unit would hang, not fail.
export GIT_SSH_COMMAND="${GIT_SSH_COMMAND:-ssh -o BatchMode=yes -o ConnectTimeout=15}"
export GIT_TERMINAL_PROMPT=0

needs_attention=0

dirty=$(git status --porcelain | wc -l)
if [[ $dirty -gt 0 ]]; then
	say "$dirty uncommitted path(s) — not backed up until you commit them"
	needs_attention=1
fi

branch=$(git rev-parse --abbrev-ref HEAD)

push_to() { # remote
	local remote=$1
	if ! git remote get-url "$remote" >/dev/null 2>&1; then
		say "no '$remote' remote configured — skipping"
		return 1
	fi
	# Deliberately not --exit-code: that reports 2 for a repository that is
	# reachable but holds no refs, which is precisely a freshly created one
	# waiting for its first push. Plain ls-remote answers the question actually
	# being asked -- can this remote be talked to -- and an empty repo answers yes.
	if ! git ls-remote "$remote" >/dev/null 2>&1; then
		say "'$remote' is unreachable or has no repository yet — skipping"
		return 1
	fi
	# --all keeps every branch, not just the one checked out; tags go separately
	# because git will not send them with --all.
	if git push "$remote" --all && git push "$remote" --tags; then
		say "$remote up to date"
		return 0
	fi
	say "push to $remote was rejected — someone else has advanced it; resolve by hand"
	return 2
}

say "$repo on $branch"

set +e
push_to "$UPSTREAM"; upstream_status=$?
push_to "$MIRROR";   mirror_status=$?
set -e

[[ $upstream_status -eq 2 || $mirror_status -eq 2 ]] && needs_attention=1

# The mirror is the whole point: if the copy off this machine did not happen,
# say so loudly enough that the timer's status shows it.
if [[ $mirror_status -ne 0 ]]; then
	say "MIRROR DID NOT RUN — this repository has no fresh copy off this machine"
	exit 2
fi

[[ $needs_attention -eq 1 ]] && exit 2
say "done"
