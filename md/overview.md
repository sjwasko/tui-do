# criax

A local-first terminal client for [Vikunja](https://vikunja.io), written
in Rust.

It opens instantly against a local SQLite cache, renders your tasks whether or
not the server is reachable, accepts edits offline, and reconciles when the
network comes back. The render loop never waits on I/O — not for the network,
not for the disk. That single constraint is the reason the project exists, and
it shapes every decision below.

---

## What it talks to

**Vikunja** is an open-source, self-hostable task manager: projects, tasks,
labels, assignees, due dates, saved filters, Kanban boards. It has a web UI, a
documented REST API, and no official terminal client. criax speaks that API and
stores everything it learns locally.

criax is built against Vikunja **v2.5.0**, and the OpenAPI document it was built
from — 126 paths — is checked into the repo at `spec/vikunja.json`. A
conformance test asserts that every URL the client builds exists in
that document.

## The project it replaces

criax is the second attempt. The first is **cria**, and the honest summary is
that cria works, taught this project what a Vikunja TUI should *show*, and could
not be built on.

Its central problem was structural: it awaited network calls while holding a
lock on its application state, so every slow request froze the terminal. Around
that sat an application struct with roughly a hundred fields — including
twenty-two `show_*_modal` booleans, each paired with an `Option<Modal>` — which
made illegal states representable and gave every modal a branch in a 790-line
function. It hardcoded an endpoint that upstream had renamed, and answered the
resulting 404 with a 195-line chain of guesses. It asked for 10,000 tasks per
page, was silently capped at 50, and dropped everything past the first page
without telling anyone.

None of that is a criticism of the person who wrote it. Those are the failure
modes of a codebase that grew by accretion without a test suite or a lint gate,
which is most codebases. **criax exists to keep cria's behaviour and discard its
architecture** — its `tests/` directory in particular is a genuinely useful
behavioural spec, and the quick-add syntax and configurable column layouts are
cria's ideas, reimplemented.

A note on provenance, because it matters: cria ships **no license file** and
never has, so nothing has been copied from it. The two ideas carried across are
independently derivable — the parser implements *Vikunja's own documented*
quick-add syntax, and the column layouts are a YAML schema described in cria's
own documentation. Both were written from those descriptions and from tests.
criax ships `MIT OR Apache-2.0`.

## How it was built

**100% vibe coded.** Every line of Rust, every test, every commit message in
this repository was written by Claude in conversation. There is no hand-written
code in it.

That is a statement about method, not about rigour. The workflow that
produced it:

- **Design is agreed in writing before code is written.** Each phase begins with
  a design note in `md/` recording the decisions, the alternatives, and why.
  When the build proves a decision wrong, the note is updated to say so.
- **The human directs and tests.** Scope, architecture calls, and priorities are
  theirs. So is the most valuable input of all: running the thing and reporting
  what feels wrong. Six real defects in this codebase were found that way, none
  of which a passing test suite had noticed.
- **Nothing is trusted because it compiled.** Fixes proposed by review are
  verified by reverting them and confirming the right test goes red. Assumptions
  about the server are settled by asking the server, and the answers are written
  into `CLAUDE.md`.

## Layout

```
                   ┌─────────────────────────────────────┐
                   │              criax                  │   the binary:
                   │   terminal · effect runtime · CLI   │   owns tokio, the
                   └───────┬──────────────────┬──────────┘   terminal, and the
                           │                  │              only awaits
                 Msg ▲     │ Effect           │
                     │     ▼                  ▼
          ┌──────────┴──────────┐   ┌──────────────────────┐
          │      criax-tui      │   │      criax-core      │
          │  Model · Msg ·      │──▶│  store · outbox ·    │
          │  update · view      │   │  sync · quickadd ·   │
          │                     │   │  config              │
          │  pure & synchronous │   └──────────┬───────────┘
          └─────────────────────┘              │
                                               ▼
                                    ┌──────────────────────┐
                                    │      criax-api       │
                                    │  typed client ·      │
                                    │  models · paging     │
                                    └──────────┬───────────┘
                                               │  HTTPS
                                               ▼
                                        Vikunja server
```

The arrows that matter are the ones that are **missing**. `criax-tui` does not
depend on `reqwest`, `rusqlite` or `tokio`, so it *cannot* await anything — the
dependency list is the enforcement mechanism, not a convention anyone has
to remember.

### `criax-api` — the wire

A typed client over Vikunja's REST API. Owns the models, the pagination (page
size read from the server's `/api/v1/info`, never hardcoded), authentication by
scoped token or by password-and-refresh-cookie, and the error taxonomy the sync
engine classifies against.

### `criax-core` — what is true right now

The local SQLite store is the source of truth the interface reads from.
Beside it:

- **`store`** — tasks, projects, labels, users, and their conversions. Unset
  dates become `NULL` on the way in and Go's zero time on the way out, in
  one place.
- **`outbox`** — an *ordered* queue of `Mutation`s. `queue()` applies a change
  locally and records it for the server in a single transaction, so the two can
  never disagree. Every mutation knows its own `inverse`, which is what undo is
  built from.
- **`sync`** — push, then pull. Rejections roll back; a 404 that means "already
  done" is distinguished from one that means "your project is gone"; a pull
  never overwrites a task with unsent local changes.
- **`quickadd`** — `Call the VA *urgent !3 +Legal tomorrow` → a task. Token
  order is irrelevant; tokens are recognised anywhere in the line.
- **`config`** — YAML, XDG paths, token resolution, and a one-way importer for
  cria's config.

### `criax-tui` — everything on screen

`update(&mut Model, Msg) -> Vec<Effect>` is a pure, synchronous function. It
cannot read a clock — the current time arrives inside `Msg::Tick` — and it
cannot touch a store. Screens and modals are an `enum` and a stack, never
booleans. The keymap is a single table read by three consumers, so the bindings,
the help modal and the command palette cannot drift.

### `criax` — the only place that blocks

The effect runtime: it owns the terminal, the tokio runtime, the store handle
and the sync timer. It executes `Effect`s on their own tasks and sends results
back as `Msg`s, so nothing sits between a keystroke and a frame. It also hosts
the CLI — `criax add`, which shares the parser and the outbox with the interface
rather than reimplementing either.

## The loop

```
keypress ─▶ Msg::Key ─▶ update ─┬─▶ Model changed ─▶ view ─▶ frame
                                │                            (immediately)
                                └─▶ Effect::Apply
                                          │
                                          ▼
                                    store.queue()
                                          │
                                          ├─▶ local row + outbox entry
                                          │      (one transaction)
                                          └─▶ push ─▶ server
                                                         │
                              Msg::Sync ◀────────────────┘
```

An edit appears on screen before anything touches the disk. If the server
rejects it, the sync engine rolls the change back, the list reloads, and the
user is told why. If the server cannot be reached, the change waits in the
outbox — through a quit, through a reboot — and goes out when it can.

## Five rules

1. **The render loop never awaits I/O.** Enforced by the dependency ban above.
2. **No `show_x_modal: bool`.** Screen state is an enum plus a stack of modals.
3. **No endpoint is called that isn't in `spec/vikunja.json`**, asserted by
   a test.
4. **Pagination is never assumed.** The page cap comes from the server.
5. **Writes are optimistic**, with rollback on rejection. Undo rides the
   same mechanism.

## What the spec doesn't tell you

Every one of these cost a live failure to find, and each is now handled in
one place:

- **"Unset" is Go's zero time**, `0001-01-01T00:00:00Z`, never `null`. Parse it
  naively and every dateless task reads as two thousand years overdue.
- **"Empty" is `null`, not `[]`.** `serde(default)` does not save you — it
  covers an absent field, not a present null — so one `"labels": null` fails a
  whole page.
- **Path parameters lose to the request body.** Send `project_id: 0` to
  `PUT /projects/31/tasks` and the server answers 404 about the project you
  just named.
- **Labels don't travel in the task body**, but assignees do — and an empty
  `assignees` field clears them.
- **A label change that already happened answers three different ways**: 404 for
  a deleted task, `400` code `8001` for an attach, and — the one nobody
  guesses — **`403 Forbidden`** for a detach. A client that assumes 404 undoes
  a detach the user asked for every time a lost response makes it retry.

## Testing

424 tests, of four kinds:

- **Unit and integration**, including the whole HTTP client against `wiremock`.
- **`update` driven by message sequences** with assertions on `Model` — no
  terminal needed, which is the point of keeping the layer pure.
- **Golden screens** rendered through ratatui's test backend at 45×20, 80×24,
  110×40 and 160×50 — the four bands where the layout changes.
- **Live tests against a dev instance**, gated on an environment variable, with
  an allow-list that refuses to run against production and no variable that can
  countermand it.

And a list of manual checks (`md/MANUAL-CHECKS.md`) for what none of the above
can see. Every entry on it exists because something shipped that a green suite
did not catch.

## Where it is

Phases 0–3 are complete: the API layer, the store, the sync engine, and a
read-only interface that shows 3,877 real tasks instantly from cache. Phase 4 —
mutations — is half built: add, complete, delete, undo and redo work against a
live server, from the interface or from `criax add` in a shell.

Ahead: the rest of the edit keys, markdown descriptions, comments and
attachments, then Vikunja's own views — Kanban, table, saved filters.

Linux through 1.0, tested on Arch and Ubuntu LTS. It has been run on a phone,
over ssh, in Termux. macOS is a port after that; Windows is answered with WSL.
