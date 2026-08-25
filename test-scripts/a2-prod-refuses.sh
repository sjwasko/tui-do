#!/usr/bin/env bash
#
# MANUAL-CHECKS A2 — production refuses.
#
# Automates everything that can be asserted without a terminal:
#
#   1. `criax --config <prod>`      refuses, naming --i-know-this-is-prod
#   2. `criax add --config <prod>`  refuses the same way, and queues nothing
#   3. with the flag, it gets past the guard and reaches the interface
#
# Only "and then it draws" is left for your eyes; step 4 prints that command
# rather than running it. See the README for why it points at prod-box.invalid.
#
# Nothing here touches the real prod server: the guard runs before any socket
# is opened, so a refusal is proof no request was made.

set -uo pipefail

cd "$(dirname "$0")"

readonly PROD_CONFIG="a2-prod.yaml"
readonly UNROUTABLE_CONFIG="a2-prod-unroutable.yaml"
readonly FLAG="--i-know-this-is-prod"

# A task text that is obviously a probe, so it is recognisable if it ever does
# escape — which is the thing this check exists to prevent.
readonly PROBE_TEXT="A2 probe — this must never reach a server"

bin=${CRIAX_BIN:-}
if [[ -z $bin ]]; then
	for candidate in ../target/debug/criax ../target/release/criax; do
		[[ -x $candidate ]] && { bin=$candidate; break; }
	done
fi
[[ -z $bin ]] && bin=$(command -v criax || true)
[[ -n $bin ]] || { echo "no criax binary — build it or set CRIAX_BIN" >&2; exit 1; }
# absolute, so the command printed in step 3 works from any directory
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")

store="$HOME/.local/share/criax/criax.db"
failures=0

pass() { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; failures=$((failures + 1)); }

outbox_count() {
	[[ -f $store ]] || { echo 0; return; }
	sqlite3 "$store" 'select count(*) from outbox;' 2>/dev/null || echo unknown
}

echo "binary:  $bin"
echo "config:  $PROD_CONFIG ($(awk '/url:/{print $2; exit}' "$PROD_CONFIG"))"
echo

# --- 1. the interface refuses ------------------------------------------------
echo "1. criax --config $PROD_CONFIG"
# timeout, in case the guard is broken and this tries to open the interface.
out=$(timeout 15 "$bin" --config "$PROD_CONFIG" 2>&1 </dev/null)
status=$?

[[ $status -ne 0 ]] && pass "exits non-zero ($status)" || fail "exited 0 — it did not refuse"
[[ $status -eq 124 ]] && fail "timed out — it appears to have started"
grep -qF -- "$FLAG" <<<"$out" && pass "names $FLAG" || fail "message does not name $FLAG"
grep -qi "production server" <<<"$out" && pass "says why" || fail "message does not explain"
echo "  ---"
sed 's/^/  | /' <<<"$out"
echo

# --- 2. `add` refuses, and queues nothing ------------------------------------
echo "2. criax add --config $PROD_CONFIG"
before=$(outbox_count)
out=$(timeout 15 "$bin" add --config "$PROD_CONFIG" "$PROBE_TEXT" 2>&1 </dev/null)
status=$?
after=$(outbox_count)

[[ $status -ne 0 ]] && pass "exits non-zero ($status)" || fail "exited 0 — it did not refuse"
grep -qF -- "$FLAG" <<<"$out" && pass "names $FLAG" || fail "message does not name $FLAG"

if [[ $before == unknown || $after == unknown ]]; then
	echo "  ....  outbox not readable (sqlite3 missing?) — skipped"
elif [[ $before == "$after" ]]; then
	pass "queued nothing locally (outbox still $after)"
else
	fail "outbox went $before -> $after — it wrote before refusing"
fi
echo "  ---"
sed 's/^/  | /' <<<"$out"
echo

# --- 3. the flag gets past the guard -----------------------------------------
# Run under setsid, so there is no controlling terminal: criax then gets as far
# as opening the interface and fails on raw mode instead. That failure is the
# proof — it only happens after guard_production has let it through. Without
# setsid this would take over your terminal, because crossterm opens /dev/tty
# directly rather than stdout.
echo "3. criax --config $UNROUTABLE_CONFIG $FLAG"
if command -v setsid >/dev/null; then
	out=$(setsid timeout 15 "$bin" --config "$UNROUTABLE_CONFIG" "$FLAG" 2>&1 </dev/null)
	status=$?

	if grep -qi "production server" <<<"$out"; then
		fail "still refused — the flag did not get past the guard"
	else
		pass "not refused — the guard let it through"
	fi

	if [[ $status -eq 124 ]]; then
		fail "timed out"
	elif grep -qiE "raw mode|terminal" <<<"$out"; then
		pass "reached the interface (stopped only for want of a terminal)"
	else
		fail "did not reach the interface — stopped earlier, for another reason"
	fi
	echo "  ---"
	sed 's/^/  | /' <<<"$out"
else
	echo "  ....  setsid missing — skipped, run step 4 by hand"
fi
echo

# --- 4. the half that needs your eyes ----------------------------------------
cat <<EOF
4. That it actually draws — run this yourself, it opens the interface:

     $bin --config $(pwd)/$UNROUTABLE_CONFIG $FLAG

   Expect: it starts, draws the cached list, and reports that it cannot reach
   the server. That is the guard letting it through, which is the whole point.

   Not scripted, and deliberately pointed at prod-box.invalid rather than the
   real host: starting against prod runs a pull, and a pull deletes every local
   task the listing did not mention. It would overwrite your dev cache with
   prod's tasks.

   Never run: criax add --config $PROD_CONFIG $FLAG
   That is the one combination in A2 that would write to production.

EOF

if [[ $failures -eq 0 ]]; then
	printf '\033[32mA2: the scripted half passed.\033[0m Step 4 is yours.\n'
else
	printf '\033[31mA2: %d assertion(s) failed.\033[0m\n' "$failures"
	exit 1
fi
