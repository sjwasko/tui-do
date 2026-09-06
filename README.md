# tui-do

A fast, local-first terminal client for [Vikunja](https://vikunja.io).

tui-do keeps your tasks in a local SQLite store and reconciles with the server in the
background. It starts instantly, works with the server unreachable, and never blocks a frame
on a network call.

> **Status: 1.0 release candidate.** `v1.0.0-rc.1` is published and is what the commands
> below install. Everything documented here works and is used daily against a real Vikunja;
> [what is not there yet](#what-is-not-there-yet) is listed rather than left to be
> discovered.

## What it does

- **Reads instantly and works offline.** Every read is answered by the local store, so the
  interface does not wait on the network, and neither startup nor editing needs the server
  to be reachable.
- **Writes optimistically.** A change lands locally and is queued; the sync engine sends it
  when it can. A rejection rolls the change back and says so. A failure blocks that one
  task, not the queue.
- **Quick-add syntax** for creating tasks in one line, from the interface or the shell.
- **Task descriptions rendered** — Markdown, plain text, or the HTML Vikunja's web editor
  stores, in your own colours, in process, with no external tools.
- **Labels, priorities, due dates, projects**, with fuzzy pickers for each. Labels can be
  created, renamed and recoloured from inside tui-do.
- **Undo and redo**, riding on the same optimistic-write mechanism rather than a parallel
  one.
- **Configurable column layouts** and quick actions.

## Install

One statically linked binary. It needs no Rust, no C toolchain and no system SQLite — only a
Linux kernel — and the same file runs on Arch, Ubuntu, Debian, Fedora and older LTS releases
alike. [Building from source](#building-from-source) is only needed to modify tui-do.

```sh
tag=v1.0.0-rc.1
arch=$(uname -m)          # x86_64 or aarch64
base=https://github.com/sjwasko/tui-do/releases/download/$tag

curl -fLO "$base/tui-do-$tag-$arch-unknown-linux-musl.tar.gz"
curl -fLO "$base/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS

tar -xzf "tui-do-$tag-$arch-unknown-linux-musl.tar.gz"
install -Dm755 "tui-do-$tag-$arch-unknown-linux-musl/tui-do" ~/.local/bin/tui-do
```

Make sure `~/.local/bin` is on your `PATH`. The only external program tui-do ever calls is
`xdg-open`, when you press `o`.

The `sha256sum --check` line is worth keeping: it catches a truncated download, which
otherwise shows up as a confusing crash rather than as the incomplete file it is.

## Set up

**There is no first-run wizard, and no config is written for you.** Without one tui-do stops
with `could not read …/config.yaml: No such file or directory`.

Create an API token in Vikunja under **Settings → API tokens**, then:

```sh
mkdir -p ~/.config/tui-do
printf '%s' 'YOUR_TOKEN_HERE' > ~/.config/tui-do/token
chmod 600 ~/.config/tui-do/token
$EDITOR ~/.config/tui-do/config.yaml
```

A URL and somewhere to find the token is the whole minimum:

```yaml
server:
  url: https://vikunja.example.com
  token_file: ~/.config/tui-do/token
```

Then run `tui-do`. Everything else has a default — see the
[configuration reference](#configuration-reference) for the rest.

Only the first line of the token file is used, and it is trimmed, so a trailing newline is
harmless. `TUI_DO_API_TOKEN` works instead of the file if you would rather not have one, and
`TUI_DO_CONFIG` overrides where the config is read from. tui-do warns if either file is
readable by other accounts.

Your tasks live at `~/.local/share/tui-do/tui-do.db`. Deleting it is safe — the next run
re-syncs.

Coming from [cria](https://github.com/frigidplatypus/cria)? `tui-do migrate` translates its
config and reports what did not carry across.

## Keys

`?` shows this table inside the application, and `:` runs any command by name if you would
rather not remember a key.

### Navigation

| | |
|---|---|
| `j` / `k`, `↓` / `↑` | Move down / up |
| `C-d` / `C-u`, `PgDn` / `PgUp` | Page down / up |
| `g g` / `G`, `Home` / `End` | Jump to top / bottom |
| `Tab` / `S-Tab` | Focus the next / previous pane |
| `g p` / `g l` | Go to a project / label |
| `/` | Search the current list |
| `Enter` | Open the selected task |
| `Esc` | Back to the list, then out of a search |

### Tasks

| | |
|---|---|
| `a` | Add a task, in quick-add syntax |
| `e` | Edit the selected task |
| `d` | Mark done, or not done |
| `p` / `D` | Set priority / due date |
| `l` | Add or remove labels |
| `m` | Move to another project |
| `o` | Open a link in the selected task |
| `x` | Delete the task |
| `u` / `C-r` | Undo / redo |
| `Space` | Configured quick actions |

### View and application

| | |
|---|---|
| `z s` / `z p` | Show or hide the sidebar / preview pane |
| `t` | Show or hide completed tasks |
| `L` / `H` | Next / previous column layout |
| `r` | Sync — push, then fetch what changed |
| `R` | Sync everything, so deletions made elsewhere are noticed |
| `:` / `?` | Run a command by name / show the keys |
| `q`, `C-c` | Quit |

**`o` copies rather than opens when there is nothing to open onto.** Over SSH, `xdg-open`
would launch a browser on the machine at the far end of the connection instead of the one you
are sitting at — so with `SSH_CONNECTION` set, or with neither `DISPLAY` nor
`WAYLAND_DISPLAY`, `o` puts the URL on your clipboard over OSC 52, which lands in the
terminal *you* are typing in. That is working as intended, not a missing `xdg-open`. The
toast names what it copied, because a few terminals ship with OSC 52 disabled and tui-do has
no way to tell.

## Quick-add syntax

Tokens may appear anywhere in the line and are taken out of the title.

| | |
|---|---|
| `+project` | File it in a project, by title or id: `+Legal`, `+#12` |
| `*label` | Attach a label: `*urgent` |
| `@user` | Assign someone: `@admin` |
| `!1` … `!5` | Priority, 1 lowest to 5 highest |
| a date | `tomorrow`, `next friday`, `27/08/26`, `27aug26`, `2026-08-27` |
| `due <date>` / `start <date>` | The same, said explicitly |
| `every <n> <unit>` | Repeat: `every 2 weeks`, `every month` |

Wrap a name containing spaces in brackets or quotes — brackets are usually easier from a
shell, which strips quotes before tui-do sees them:

```sh
tui-do add "Renew the domain *urgent !3 +Admin tomorrow"
tui-do add "File the quarterly return +[Life Admin] 27aug26"
tui-do add "Water the plants every 3 days"
```

## From the shell

`tui-do add` applies locally and queues for the server exactly as the interface does, so it
works on a plane. Two flags matter:

- **`--create-labels`** creates any label the line names that does not exist yet. The
  interface asks instead of assuming.
- **`--offline`** skips the send entirely — **but it needs a store that already knows your
  projects**, because a task has to be filed somewhere. On a machine that has never synced
  you get *"no project to add to"*. Without the flag `tui-do add` fetches the project list
  before it gives up, so run it once without `--offline` on a new machine (or start
  `tui-do`, which syncs at launch). After that it works on a plane as advertised.

Shell completions, including for the quick-add flags:

```sh
tui-do completions fish > ~/.config/fish/completions/tui-do.fish
tui-do completions zsh  > ~/.zfunc/_tui-do          # with ~/.zfunc on $fpath
tui-do completions bash > ~/.local/share/bash-completion/completions/tui-do
```

## For agents

`tui-do add` is a good target for automation. One line resolves a label, a priority, a
project and a due date:

```sh
tui-do add "Chase the upstream ticket *urgent !4 +Infra tomorrow"
```

Constructing the equivalent Vikunja request means knowing a project id, a label id, and that
"unset" on the wire is Go's zero time rather than `null`.

It is also the right surface rather than merely a convenient one. The write lands in the
local store first, so the command returns whether or not the server is reachable, and an
unreachable server queues the task instead of losing it. It takes **the same path the
interface does** — the same outbox, the same backoff, and the same three-way merge that
replays your change onto the server's current copy. Something calling the Vikunja API
directly is a second writer with none of that.

**`skills/tui-do/SKILL.md`** documents this for agents and is installable as a skill by tools
that support them. It is plain Markdown, so anything else can simply read it.

One flag deserves care when nobody is watching: **`--create-labels` should usually stay off.**
Labels are one global pool shared by every project, so a typo in a generated line becomes a
permanent entry that pollutes completion everywhere. Pass it only where the label vocabulary
is yours rather than a model's.

### This is the part I most want to hear about

**Today `add` is the whole agent surface.** Reading, completing and moving tasks from the
command line are planned and not in this release, and which verbs get built first is being
decided by what people ask for. If you are driving tui-do from an agent,
[open an issue](https://github.com/sjwasko/tui-do/issues) and say what you were trying to
automate — that is worth more to me than a feature request in the abstract.

**Kanban is the open design question.** The list view is the only view today, and boards are
planned. What an agent should *say* to move a task across a board — name a bucket, set a
status, ask for "the next column" — is genuinely undecided, and it is the kind of thing that
is easy to get wrong in a way you only notice after people depend on it. If you work in
Vikunja's board views, I would like your opinion before it is built rather than after.

**Contributors and testers are welcome, and specifically wanted here.** This surface is small
enough to be a reasonable first contribution and is not yet load-bearing for anyone, which
makes it the safest place in the project to try something. Testing counts as much as code:
running it against your own Vikunja, on your own hardware, and reporting what broke is the
work that most needs doing.

## Configuration reference

tui-do reads `~/.config/tui-do/config.yaml`, or `$XDG_CONFIG_HOME/tui-do/config.yaml`.
`TUI_DO_CONFIG` overrides the path. Only `server.url` and a credential are required; every
other key below shows its default.

```yaml
server:
  url: https://vikunja.example.com
  # Either point at a file holding the token, or set TUI_DO_API_TOKEN.
  # tui-do warns if the file is readable by other accounts.
  token_file: ~/.config/tui-do/token

sync:
  interval_seconds: 300   # the local store answers every read; this sets staleness
  enabled: true

view:
  default_filter: done = false
  active_layout: default

# Reached with Space, then the key.
quick_actions:
- key: 'u'
  action: priority
  target: 5
- key: 'w'
  action: project
  target: Work
```

An unknown key is an error rather than a shrug, so a typo is caught at the one moment you can
still connect it to what you just edited.

## What is not there yet

Stated plainly, because the alternative is discovering it:

- **Comments** — deferred. The API client exists; nothing in the interface reaches it.
- **Subtasks and task relations** — deferred. Vikunja's relation kinds are parsed and
  preserved, but not shown or editable.
- **Attachments** — out of scope. Existing attachments survive edits untouched; tui-do
  neither displays nor uploads them.
- **Kanban, table and Gantt views, and saved filters** — planned, not built. The list view is
  the only view.
- **Deleting a label** — deliberately absent. It cannot be undone honestly, because the label
  would come back with a new id detached from every task it was on.

## Platform support

**Linux is the only supported platform.** Developed on Omarchy (Arch), with Ubuntu 26.04 LTS
as the second supported target.

- **macOS** — a planned port. It may build today; it is not tested or supported.
- **Windows** — not a target. Use [WSL](https://learn.microsoft.com/windows/wsl/install) and
  run the Linux build. There are no plans to ship a native Windows binary.

## Building from source

Only needed to modify tui-do, or to run on an architecture no release covers. **SQLite is
compiled from source**, so a C toolchain is required even though the resulting binary needs
none. Rust **1.85 or newer**.

On a fresh Debian or Ubuntu:

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    curl ca-certificates build-essential pkg-config git

# Take Rust from rustup rather than the distribution, which may package
# a version older than 1.85.
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
. "$HOME/.cargo/env"
```

On Arch: `sudo pacman -S --needed rust base-devel git`.

No OpenSSL development headers are needed — TLS is rustls, and `ca-certificates` is what it
reads the system trust store from.

Then:

```sh
git clone https://github.com/sjwasko/tui-do.git
cd tui-do
cargo build --release
install -Dm755 target/release/tui-do ~/.local/bin/tui-do
```

The first build fetches and compiles the whole dependency tree and takes a few minutes.

### Running the tests

```sh
cargo test --workspace
```

**Read the total, not the last line.** `cargo test` prints one `test result:` line per test
binary — eighteen of them — and the last few are doc-test targets that legitimately contain
no tests, so the output ends with `0 passed` however well the run went. Summing them is what
tells you:

```sh
cargo test --workspace 2>&1 | grep -E '^test result' | \
  awk '{p+=$4; f+=$6} END {print "passed="p"  failed="f}'
```

Anything but `failed=0` is a real problem. `cargo clippy --workspace --all-targets` should
also be silent.

## Design

**The render loop never awaits I/O.** The UI is an Elm-style `Model` / `Msg` / `update` /
`view` loop that is pure and synchronous. Side effects are described as values and executed
by a separate async runtime that sends results back as messages.

This is enforced by the crate graph rather than by discipline:

| Crate | Responsibility | Notably cannot |
|---|---|---|
| `tui-do-api` | Typed go-vikunja REST client | — |
| `tui-do-core` | Domain model, SQLite store, sync engine, quick-add parser, config | render |
| `tui-do-ui` | `Model`, `Msg`, `update`, `view`, widgets, keymap | perform I/O — no `reqwest`, no `rusqlite`, no `tokio` |
| `tui-do` | CLI, wiring, effect runtime, terminal lifecycle | — |

Writes are optimistic: the local store updates immediately and the change is queued in an
outbox. If the server rejects it, the change is rolled back and reported. A queued update is
not sent as it was queued — the push reads the server's current copy and replays the user's
field-level change onto it, because tui-do is built to run on several machines at once.

Every endpoint the client calls is checked against `spec/vikunja.json` — the OpenAPI document
served by a live Vikunja at `/api/v1/docs.json` — by a conformance test, so an upstream API
change surfaces as a failing test rather than a runtime 404.

## Relationship to cria

tui-do began as a fork-in-spirit of [cria](https://github.com/frigidplatypus/cria) by
frigidplatypus, which established the idea of a keyboard-driven Vikunja TUI along with its
quick-add syntax and column-layout configuration. tui-do is an independent implementation
with a different architecture and does not share cria's code.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.

Copyright is held collectively by the project's contributors ("The tui-do Authors");
attribution lives in the git history.
