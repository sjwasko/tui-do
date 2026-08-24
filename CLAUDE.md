# criax — working notes for Claude

A local-first terminal client for Vikunja. Rust workspace, ratatui UI, SQLite store.
Full phase plan in `PLAN.md`.

## The rules that matter

**1. The render loop never awaits I/O.**
`criax-tui` is pure and synchronous. `update(&mut Model, Msg) -> Vec<Effect>` describes side
effects as values; the effect runtime in `crates/criax` executes them and sends results back
as `Msg`. `criax-tui` has no `reqwest`, no `rusqlite`, no `tokio` dependency, and must never
gain one — that dependency ban *is* the enforcement mechanism.

*Why:* the project criax replaces awaited network calls while holding a lock on its
application state, freezing the terminal for the duration of every slow request.

**2. No `show_x_modal: bool` fields.**
Screen and modal state is `enum Screen` plus a `Vec<Modal>` stack. Adding a modal means
adding one enum variant.

*Why:* cria's `App` struct has ~100 fields including 22 `show_*_modal` bools paired with
`Option<Modal>` values, so illegal states are representable and every modal needs a branch in
a 790-line function.

**3. No endpoint is called that isn't in `spec/vikunja.json`.**
That file is the OpenAPI document fetched from a live server (`/api/v1/docs.json`). A
conformance test asserts every path template the client builds exists in it. Refresh the spec
with `cargo xtask fetch-spec` when the server is upgraded.

*Why:* cria hardcoded `/tasks/all`, which upstream renamed to `/tasks`, and its response was
a 195-line "Method 1 / Method 2 / Method 3" fallback chain that guessed at endpoints.

**4. Pagination is never assumed.**
Read `x-pagination-total-pages` and `x-pagination-result-count`; take the page cap from
`/api/v1/info`'s `max_items_per_page` (50 on our server) rather than hardcoding it.

*Why:* cria requests `per_page=10000`, is silently capped at 50, and drops tasks past the
first page without telling anyone.

**5. Writes are optimistic.**
`update` mutates the local store immediately and queues an outbox entry. On rejection the
sync engine emits `Msg::SyncFailed`, which rolls back and toasts. Undo/redo rides on this
mechanism rather than a parallel one.

## Wire-format facts the spec does not tell you

The OpenAPI document describes what the server *means*, not what it *emits*. Each of these
cost a live failure to find; all are handled in `criax-api`, and new code must route
through the same helpers rather than rediscover them.

**Dates: "unset" is Go's zero time**, `"0001-01-01T00:00:00Z"`, never `null` — the SQL
columns are `NOT NULL`. Parse naively and every dateless task reads as 2000 years overdue.
Clearing a date means *sending* that value; `null` is rejected. All date fields go through
`models::datetime`.

**Collections: "empty" is `null`**, not `[]` — Go marshals a nil slice that way, and
Vikunja leaves collections nil whenever an endpoint did not populate them. `serde(default)`
does not save you: it covers an absent field, not a present null, so one `"labels": null`
fails the whole page. All `Vec`/`HashMap` fields go through `models::nullable`.

**Path parameters lose to the request body.** Vikunja binds the path first and the JSON
body second, so a body field that shadows a path parameter silently overwrites it. Sending
a new task with `project_id: 0` to `PUT /projects/31/tasks` makes the server look up
project 0 and answer `404 / 3001 "This project does not exist."` — about the project you
just created. Write the path value into the body.

**The spec is authoritative for paths, not for methods or bodies.** Three counts so far,
all decided in the server's favour: `PUT /migration/vikunja-file/migrate` is documented
`post` (server: `405 Allow: OPTIONS, PUT`); `POST /tasks/{taskID}/comments/{commentID}`
documents no request body but requires one; `repeat_mode`'s prose says the third variant is
`3` while the enum in the same document says `2`. Only live integration tests catch this
class of error.

**`/projects` returns pseudo-projects.** Observed on dev: `-1` Favorites, `-2` My Open
Tasks, `-3` Inbox — saved filters and built-ins presented as projects. They reject writes,
so anything offering a project to create in must filter to `id > 0`. It is also why a
project count from the API does not match the count in the web UI's sidebar.

**An unfiltered `GET /tasks` includes done tasks.** Verified on dev 2026-08-24: of
3,877 tasks the listing returned 1,942 done, and `filter=done = true` returned exactly
those same 1,942 with none missing. What filters completed tasks out is a *project
view* — the default List view carries `done = false` — not the plain collection
endpoint. The sync engine's pull depends on this: it deletes every local task the
listing did not mention, so a server that quietly omitted done tasks would erase the
user's entire completed history in one pass. `tests/live.rs` asserts it differentially,
and reports "inconclusive" rather than passing quietly if dev holds no done tasks.

**Assignees travel in the task body; labels do not.** `POST /tasks/{id}` replaces the task
from the body, and an empty `assignees` clears them. Labels are the opposite: they are
attached and detached through their own endpoints and the body's `labels` field is ignored.

Verified on dev by assigning a user and re-reading through a list endpoint: **list results
do populate assignees**, so a task fetched from a list can be passed back to `update_task`
safely, and a `null` assignees field means genuinely nobody rather than "not loaded". What
is still unsafe is *constructing* a task from partial data and sending it — anything that
does must fill assignees itself or it will unassign everyone.

**A label change that has already happened answers three different ways**, and only one of
them is the one you would guess. Measured on dev 2026-08-24, because the spec describes
none of it:

| asked | answered |
|---|---|
| delete a task that is already gone | `404` |
| attach a label that is already attached | `400`, Vikunja code `8001`, "This label already exists on the task." |
| detach a label that is already detached | **`403 Forbidden`** — no code, no message beyond the word |

That last one matters because a lost response means a retry, and every one of these is a
4xx, which `sync::is_permanent` treats as the server's final answer. Without an arm for
each, a replayed attach rolls the label back off the task and a replayed detach puts it
back on — undoing, in both cases, exactly what the user asked for. `is_already_done` has
all three, each with a test that fails when its arm is removed.

Treating *any* `403` on a detach as "already done" does swallow a genuine permission
failure. That is the deliberate trade: the label is then still on the server's copy, so
the next pull restores it and the user sees the truth. The alternative rolls back a detach
the server has already honoured.

**A task write does echo its labels back**, contrary to what `sync::with_labels` was
written to compensate for: a write carrying one label answered with one label, matching
the task. The helper stays as belt and braces — one measurement of one shape, guarding a
silent failure — but it is no longer the load-bearing step its comment claimed.

## Environment

| | |
|---|---|
| **Dev server** | `https://dev-box.example.net:8443` — use this for everything |
| **Prod server** | `https://prod-box.example.net:8443` — **read-only, always.** Never a write target, never a test target |
| **Vikunja version** | v2.5.0 (dev pinned to match prod) |
| **TLS** | Tailscale Serve certs are publicly trusted; never disable certificate verification |

Reset the dev server to its seeded baseline with `deploy/reset-dev.sh`. Seed it from a prod
export with `deploy/seed-from-prod.sh` (which reads prod and writes only to dev).

`crates/criax` refuses to start against the prod URL without `--i-know-this-is-prod`, and
integration tests refuse to run unless `CRIAX_TEST_URL` points at dev. Do not weaken either
guard to make something pass.

## Platform policy

Linux only through GA — tested on Omarchy (Arch, this workstation) and Ubuntu 26.04 LTS (via
`ubuntu:26.04` container). macOS is a post-GA port; there is a non-gating `cargo check` in CI
purely to limit drift. Windows is answered with "use WSL" and is not a build target.

Still write portably where it is free: paths via `dirs`, never cwd-relative writes,
`rusqlite` with `bundled`. Platform-varying behavior (URL opening, markdown rendering,
clipboard, `$EDITOR`) goes behind a small trait with a Linux impl — that trait is the seam
the macOS port uses later.

## Commands

```sh
cargo build --workspace
cargo clippy --workspace --all-targets    # must be clean; CI runs with -D warnings
cargo fmt --all
cargo test --workspace
cargo xtask fetch-spec                    # refresh spec/vikunja.json from the dev server
deploy/test-ubuntu.sh                     # build + test in the ubuntu:26.04 container
```

Workspace lints deny `unwrap`, `panic`, `todo`, `dbg!` and forbid `unsafe`. Tests may
`allow` them at module level; production code may not.

## Reference material

`../cria` is the predecessor, checked out for reference. Read it to learn *what* a screen
shows or *how* the quick-add syntax behaves — its `tests/` are a useful behavioral spec. Do
not copy its code: it carries no license (no `LICENSE` file was ever committed), and its
architecture is the thing criax exists to replace.

The Vikunja web UI is the design reference for layout and interaction.
