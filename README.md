# tui-do

A fast, local-first terminal client for [Vikunja](https://vikunja.io).

tui-do keeps your tasks in a local SQLite store and reconciles with the server in the
background. It starts instantly, works with the server unreachable, and never blocks a
frame on a network call.

> **Status: usable, pre-1.0.** Everything documented below works and is used against a real
> Vikunja daily. What it does not yet have is the full release checklist — a soak on each
> supported distribution and a packaged build — and the feature gaps are listed under
> [What is not there yet](#what-is-not-there-yet) rather than left to be discovered.

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

## Installing

tui-do is a single binary. SQLite is compiled in, so there is nothing to install alongside
it; the one external program it ever calls is `xdg-open`, and only when you press `o`.

```sh
git clone <this repository>
cd tui-do
cargo build --release
install -Dm755 target/release/tui-do ~/.local/bin/tui-do
```

Needs Rust 1.85 or newer. There are no published packages yet.

Shell completions, including for the quick-add flags:

```sh
tui-do completions fish > ~/.config/fish/completions/tui-do.fish
tui-do completions zsh  > ~/.zfunc/_tui-do          # with ~/.zfunc on $fpath
tui-do completions bash > ~/.local/share/bash-completion/completions/tui-do
```

## Configuring

tui-do reads `~/.config/tui-do/config.yaml` (or `$XDG_CONFIG_HOME/tui-do/config.yaml`).
`TUI_DO_CONFIG` overrides the path.

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

Create an API token from your Vikunja user settings. The tasks database lives at
`~/.local/share/tui-do/tui-do.db`; deleting it is safe — the next run re-syncs.

Coming from [cria](https://github.com/frigidplatypus/cria)? `tui-do migrate` translates its
config and reports what did not carry across.

## Using it

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

**`o` copies instead of opening where there is nothing to open onto.** Over SSH, `xdg-open`
would launch a browser on the machine at the far end of the connection rather than the one
you are sitting at, so tui-do detects that and puts the URL on your clipboard using OSC 52
instead — and says which it did. A few terminals ship with OSC 52 disabled.

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
tui-do add "Call the VA *urgent !3 +Legal tomorrow"
tui-do add "Renew the passport +[Life Admin] 27aug26"
tui-do add "Water the plants every 3 days"
```

`tui-do add` applies locally and queues for the server exactly as the interface does, so it
works on a plane. `--offline` skips the send entirely; `--create-labels` creates any label
the line names that does not exist yet, which the interface asks about instead.

## What is not there yet

Stated plainly, because the alternative is discovering it:

- **Comments** — deferred. The API client exists; nothing in the interface reaches it.
- **Subtasks and task relations** — deferred. Vikunja's relation kinds are parsed and
  preserved, but not shown or editable.
- **Attachments** — out of scope. Existing attachments survive edits untouched; tui-do
  neither displays nor uploads them.
- **Kanban, table and Gantt views, and saved filters** — planned, not built. The list view
  is the only view.
- **Deleting a label** — deliberately absent. It cannot be undone honestly, because the
  label would come back with a new id detached from every task it was on.

## Platform support

**Linux is the only supported platform.** Developed on Omarchy (Arch), with Ubuntu 26.04
LTS as the second supported target.

- **macOS** — a planned port. It may build today; it is not tested or supported.
- **Windows** — not a target. Use [WSL](https://learn.microsoft.com/windows/wsl/install) and
  run the Linux build. There are no plans to ship a native Windows binary.

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

Every endpoint the client calls is checked against `spec/vikunja.json` — the OpenAPI
document served by a live Vikunja at `/api/v1/docs.json` — by a conformance test, so an
upstream API change surfaces as a failing test rather than a runtime 404.

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

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.

## Copyright

Copyright is held collectively by the project's contributors ("The tui-do Authors");
attribution lives in the git history. Contributions are accepted under the dual
MIT/Apache-2.0 terms above.
