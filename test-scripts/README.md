# test-scripts

Scaffolding for the checks in `md/MANUAL-CHECKS.md` — the ones a green suite has
never caught. These set a machine up for a check, or assert the part of one that
does not need a human; none of them replaces the reading of the screen that the
check is actually about.

Run them from anywhere; each finds its own directory.

| check | scripts |
|---|---|
| **A1** — offline is not a degraded mode | `go-offline.sh`, `restore-config.sh` |
| **A2** — production refuses | `a2-prod-refuses.sh`, `a2-prod*.yaml` |

## A1 — offline

`go-offline.sh` points `server.url` at `https://10.255.255.1:8443`, which is
unroutable rather than refused: a refused connection fails in milliseconds and
proves nothing, while a hanging connect is what froze the predecessor.

It backs the config up to `config.yaml.pre-offline` first and refuses to run
while that backup exists, so a second run cannot bury the real URL.
`restore-config.sh` moves the backup back — it restores rather than writing a
remembered URL, because a hardcoded one here goes stale the moment the dev
instance moves.

Both honour `TUI_DO_CONFIG`, so exporting it runs them against a scratch config
instead of the live one.

The store (`~/.local/share/tui-do/tui-do.db`) does not depend on the server URL,
so the cache stays warm across the swap. That is what makes the check meaningful
rather than a test of an empty list.

Edits made while offline stay in the outbox and drain after `restore-config.sh`,
which is check B4 falling out of A1 for free.

## A2 — production refuses

`a2-prod-refuses.sh` asserts three things and hands you a fourth.

Scripted:

1. `tui-do --config a2-prod.yaml` exits non-zero and names `--i-know-this-is-prod`.
2. `tui-do add` with the same config refuses too — **and the outbox count does not
   move**, so it refused before writing anything locally, not just before
   sending.
3. With the flag, the same command is no longer refused and gets as far as
   opening the interface.

Yours:

4. That it then draws. The command is printed at the end of the run.

### The two config files

`a2-prod.yaml` names the real prod host. Handing it to tui-do is safe *only*
because the guard fires before any socket opens — `main.rs:116` for the
interface, `main.rs:146` for `add` — so a refusal is itself the proof that
nothing reached `prod-box`.

`a2-prod-unroutable.yaml` names `prod-box.invalid`. The guard is a
case-insensitive substring match on `prod-box` anywhere in the URL
(`main.rs:128`), so this trips it exactly as the real host does, while
`.invalid` is reserved by RFC 6761 and never resolves.

Everything that runs *with* the flag uses that second file, for a reason worth
stating plainly: tui-do keeps **one store for every server**, and a pull deletes
every local task the listing did not mention. Starting against real prod would
replace the dev-derived cache with prod's tasks — a write to your local state,
and the end of the warm cache the other checks depend on. Reading prod is
allowed by policy; silently repointing the local store at it is not what A2 is
asking you to verify.

The one combination the script will never run, and neither should you:

```
tui-do add --config a2-prod.yaml --i-know-this-is-prod
```

That is the only path in A2 that would write to production.

### Why step 3 uses `setsid`

crossterm opens `/dev/tty` directly rather than stdout, so redirecting output is
not enough to keep tui-do from taking over your terminal. Under `setsid` there is
no controlling terminal, so it fails at `could not put the terminal into raw
mode` — a failure that can only happen *after* the guard has let it through,
which is what makes it a usable assertion.

### That the assertions have teeth

Checked by pointing the same command at a non-prod config: the output then does
not name the flag, so assertion 1 fails as it should. Worth redoing if the
refusal message is ever reworded — these match on its text.
