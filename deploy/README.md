# tui-do dev Vikunja instance

An isolated Vikunja for developing and testing tui-do, so production task data is never
at risk. Runs on `dev-box`.

**Production (`prod-box`) is read-only, always.** It is a source for the seed export and
nothing else. `seed-from-prod.sh` refuses to write there; `tui-do` itself refuses to start
against the prod URL without `--i-know-this-is-prod`.

## Versions

Both images are pinned in `docker-compose.yml`. Keep them in lockstep with whatever
production runs, and re-check after any upgrade there: a behaviour difference between dev
and prod should never turn out to be a version artifact.

## First-time setup

On the dev host. Compose stack and data live under a root of your choosing; the paths
below are the default, and secrets stay in `*.env` at mode 600.

```sh
# 1. Create the directories. This is the one step that needs sudo, so run it
#    yourself before deploying rather than expecting a script to do it.
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

Then open the dev instance's URL, register the first account, and set
`VIKUNJA_SERVICE_ENABLEREGISTRATION=false` in `vikunja.env` followed by
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
| `reset-dev.sh` | Restores that dump, keeping API tokens; refuses to target prod |
| `test-ubuntu.sh` | Local Docker only; does not touch either server |

`vikunja.env` needs no edit: every key in it is Vikunja's own. Nothing else moves — the
bind mounts follow `TUI_DO_DEV_ROOT`, and there are no named volumes to orphan.

## Seeding from production

```sh
./seed-from-prod.sh     # prompts for credentials; nothing is written to disk
./snapshot-dev.sh       # capture the seeded state as the reset baseline
```

**A reset keeps your API tokens.** The baseline is a snapshot of task data; a token is a
credential, and restoring one should not revoke the other. Found the hard way on
2026-08-30: this instance's baseline was dumped nine hours before the token the client
authenticates with was created, so an honest restore would have started answering 401 from
a server that was reachable, healthy, and serving the right data — the worst kind of
failure to diagnose. `reset-dev.sh` now saves `api_tokens`, restores the dump, and puts the
table back exactly as it was.

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

Ubuntu 26.04 LTS is a tier-1 target alongside Omarchy and no host here runs it, so it is
exercised in a container. The two distributions diverge sharply on glibc — a green build on
the Arch workstation does not imply a green build here.

## Layout notes

Step 1 above is the only step requiring elevation, which is why it is not in a script:
it cannot run unattended. Everything after it runs as your own account, which needs to be
able to reach Docker.

Override the data root with `TUI_DO_DEV_ROOT` in `.env` if it should move. Check what the
host already serves on the port you are about to claim before deploying — reusing one that
something else answers on is a confusing way to lose an afternoon.

## Getting the work off this machine

`mirror-to-git.sh` pushes this repository to Forgejo. **Forgejo mirrors it to GitHub
itself** — a push mirror on the repository, every eight hours, exactly as every other
repository on that instance does. There is deliberately no `github` remote in this
checkout: a second path to GitHub would be a second thing to keep in step, and the two
could disagree about what the truth is.

    local  --push-->  Forgejo  --Forgejo's push mirror, 8h-->  GitHub (private)

`origin` is the Forgejo instance, reachable on the tailnet only; GitHub is written by
Forgejo's mirror and by nothing else.

The mirror is configured in the web UI under Settings → Repository → Mirror Settings,
with an 8h interval and "sync when new commits are pushed" on — which is already Forgejo's
default interval, so no server config needs changing.

Forgejo shares a box with production Vikunja but is a different service on a different
port. The read-only rule covers the Vikunja instance and its data, not the host; pushing
git there is not a prod write.

Push-to-create may be disabled, in which case the repository has to exist before the
first push will land.

`tui-do-mirror.timer` runs the script every eight hours (`deploy/systemd/`, symlinked
into `~/.config/systemd/user/`).

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
