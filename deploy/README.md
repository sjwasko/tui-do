# tui-do dev Vikunja instance

An isolated Vikunja for developing and testing tui-do, so production task data is never
at risk. Runs on `sw-surface`.

**Production (`sw-hp2`) is read-only, always.** It is a source for the seed export and
nothing else. `seed-from-prod.sh` refuses to write there; `tui-do` itself refuses to start
against the prod URL without `--i-know-this-is-prod`.

## Versions

Pinned to match production exactly, verified 2026-08-24 on `sw-hp2`:

| | version |
|---|---|
| Vikunja | `vikunja/vikunja:2.5.0` |
| Postgres | `postgres:18.4` |

Keep these in lockstep with prod. A behaviour difference between dev and prod should
never turn out to be a version artifact.

## First-time setup

On `sw-surface`, following the homelab convention: compose stacks under `/opt/stacks`,
data on local disk under `/opt/appdata`, secrets in `*.env` at mode 600.

```sh
# 1. Create the directories. This is the one step that needs sudo -- sw-surface
#    requires a password for it, so run this yourself before deploying.
sudo mkdir -p /opt/stacks/tui-do-dev /opt/appdata/tui-do-dev/{db,files}
sudo chown -R "$USER:$USER" /opt/stacks/tui-do-dev /opt/appdata/tui-do-dev

# 2. Copy this directory to /opt/stacks/tui-do-dev, then fill in the two env files
cp .env.example .env
cp vikunja.env.example vikunja.env
chmod 600 .env vikunja.env
openssl rand -hex 32          # -> VIKUNJA_SERVICE_JWTSECRET
openssl rand -hex 24          # -> TUI_DO_DB_PASSWORD (both files must agree)

# 3. Start it
docker compose up -d

# 4. Publish it on the tailnet
tailscale serve --bg --https 8443 http://127.0.0.1:3456
```

Then open `https://sw-surface.tail9803a5.ts.net:8443`, register the first account, and
set `VIKUNJA_SERVICE_ENABLEREGISTRATION=false` in `vikunja.env` followed by
`docker compose up -d` to close registration.

## Where each script runs

All of these live in the tui-do repo at `deploy/` and are run **from your workstation** —
only the compose file and the two secret files are copied to the dev host. The ones that
need Docker reach the host over SSH themselves; they also detect when they are already
running on it, so either location works.

| Script | What it touches |
|---|---|
| `up.sh` | Deploys/redeploys the stack; safe to re-run |
| `seed-from-prod.sh` | Reads prod over HTTPS, writes dev |
| `snapshot-dev.sh` | `pg_dump` on the dev host; the dump stays there |
| `reset-dev.sh` | Restores that dump; refuses to target prod |
| `test-ubuntu.sh` | Local Docker only; does not touch either server |

## Seeding from production

```sh
./seed-from-prod.sh     # prompts for credentials; nothing is written to disk
./snapshot-dev.sh       # capture the seeded state as the reset baseline
```

`seed-from-prod.sh` uses Vikunja's own export/import path — `POST /user/export/request`
on prod, then `POST /migration/vikunja-file/migrate` on dev. Both export endpoints
require your **account password**, not an API token, so the script prompts for it and
keeps it only in memory. Run it yourself; do not pass credentials on the command line,
where they would land in shell history and `ps` output.

## Resetting between test runs

```sh
./reset-dev.sh          # restore the database to the seeded baseline
```

Destructive by design. Integration tests that mutate data should assume they can trash
the instance and reset in seconds.

## Testing against Ubuntu

```sh
./test-ubuntu.sh        # build + test in ubuntu:26.04
```

Ubuntu 26.04 LTS is a tier-1 target alongside Omarchy, and no homelab host runs it
(sw-hp2, sw-hp1 and sw-pi are all 24.04.4), so it is exercised in a container. The two
distributions diverge sharply on glibc — a green build on the Arch workstation does not
imply a green build here.

## Layout notes

`sw-surface` had never adopted the homelab's `/opt/stacks` + `/opt/appdata` convention —
neither directory existed before this stack — so step 1 above creates them. That is also
the only step requiring elevation: `sudo` on `sw-surface` prompts for a password, so it
cannot run unattended. Everything after it runs as `swasko` (who is in the `docker`
group, and is Tailscale's `OperatorUser`, so `tailscale serve` needs no sudo either).

Override the data root with `TUI_DO_DEV_ROOT` in `.env` if it should move.

## Tailnet mappings

Before this stack, `sw-surface` carried three stale `tailscale serve` mappings left over
from decommissioned services — `:443`→`3030` (Forgejo), `:8443`→`8787`, and
`:8444`→`8089` — none of which had anything listening behind them. Deploying reclaims
`:8443` for dev Vikunja and removes the other two, leaving one mapping that reflects
reality.

## Getting the work off this machine

`mirror-to-git.sh` pushes this repository to Forgejo (`origin`) and then to a private
GitHub repository (`github`).

| | |
|---|---|
| **Forgejo** | `https://sw-hp2.tail9803a5.ts.net:9443` — v15.0.7, tailnet only, proxying `127.0.0.1:3030` |
| **git over ssh** | `ssh://git@sw-hp2.tail9803a5.ts.net:2222/swasko/tui-do.git` |
| **GitHub mirror** | `git@github.com:sjwasko/tui-do.git` — private |

Forgejo shares the `sw-hp2` box with production Vikunja but is a different service on a
different port. The read-only rule covers the Vikunja instance and its data, not the
host; pushing git there is not a prod write.

Push-to-create is disabled on this instance, so a new repository has to be made in the
web UI (or through the API with a token) before the first push will land. `tui-do-mirror.timer` runs it every eight hours;
`deploy/systemd/` holds both units, symlinked into `~/.config/systemd/user/`.

    systemctl --user status tui-do-mirror.timer     # when it next runs
    journalctl --user -u tui-do-mirror -n 30        # what it did last time
    deploy/mirror-to-git.sh                         # run it now

It pushes commits and never makes them. A dirty working tree is work in progress, and
committing that on a timer would put broken states into the history *and* disguise the
fact that nothing was actually backed up. Uncommitted paths are reported, and the run
exits 2 — "a human should look at this" — which the unit treats as success so the timer
keeps its schedule.

The one failure it shouts about is the mirror not running. Forgejo being unreachable is
survivable; both copies being stale is how a project ends up existing on one disk.
