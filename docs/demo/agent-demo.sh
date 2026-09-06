#!/usr/bin/env bash
# A *simulated* agent session, for the demo recording only.
#
# There is no model behind this. The prose is fixed, the pauses are `sleep`, and the only
# thing that actually happens is the `tui-do add` near the end -- which is real, and really
# writes to the server. It exists so a recording can show what filing a task from an agent
# looks like without depending on a live model, a network round trip, or an API key, and
# without the output changing between takes.
#
# Deliberately not dressed up as any particular product: a recording that wore someone
# else's branding would imply an integration that does not exist.
#
# Install on the recording host as `agent` so the tape can type one short word:
#   install -Dm755 docs/demo/agent-demo.sh ~/.local/bin/agent
set -uo pipefail

r=$'\e[0m'; b=$'\e[1m'
dim=$'\e[38;2;83;104;91m'
jade=$'\e[38;2;45;213;183m'
amber=$'\e[38;2;229;199;54m'
fg=$'\e[38;2;193;196;151m'

say() { printf '%s\n' "$1"; sleep "${2:-0.35}"; }

printf '\n'
say "  ${jade}${b}▍agent${r}${dim}  tui-do${r}" 0.5
say "  ${dim}context: Agents project${r}" 0.7
printf '\n'
say "${amber}❯${r} ${fg}review the sync work and file${r}" 0.15
say "  ${fg}anything that is missing${r}" 0.9
printf '\n'
say "  ${dim}reading tasks in Agents${r}" 0.8
say "  ${dim}9 open, none covering retry${r}" 0.4
say "  ${dim}backoff${r}" 0.9
printf '\n'
say "  ${jade}⏺${r} ${fg}tui-do add${r}" 0.25
say "    ${dim}'Summarise the sync engine${r}" 0.15
say "    ${dim}backoff policy +Agents${r}" 0.15
say "    ${dim}*research !3 tomorrow'${r}" 0.8

# The one real thing in here.
out=$(tui-do add 'Summarise the sync engine backoff policy +Agents *research !3 tomorrow' 2>&1)
printf '\n'
# Folded on word boundaries at the pane width: the reply is wider than the pane the agent
# runs in, and letting the terminal hard-wrap it split a word across two lines.
width=$(( $(tput cols 2>/dev/null || echo 51) - 4 ))
while IFS= read -r line; do
  printf '    %s%s%s\n' "$fg" "$line" "$r"
  sleep 0.3
done < <(printf '%s\n' "$out" | fold -s -w "$width")

printf '\n'
say "  ${jade}✓${r} ${fg}filed 1 task in Agents${r}" 0.6
printf '\n'
