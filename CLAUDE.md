# tui-do — working notes for Claude

A local-first terminal client for Vikunja. Rust workspace, ratatui UI, SQLite store.
Full phase plan in `PLAN.md`.

## The rules that matter

**1. The render loop never awaits I/O.**
`tui-do-ui` is pure and synchronous. `update(&mut Model, Msg) -> Vec<Effect>` describes side
effects as values; the effect runtime in `crates/tui-do` executes them and sends results back
as `Msg`. `tui-do-ui` has no `reqwest`, no `rusqlite`, no `tokio` dependency, and must never
gain one. Be precise about what that buys: all three arrive *transitively* through
`tui-do-core`, and `tui_do_core::Store` is a public re-export that `update` could call
and block the render thread with. `use rusqlite::…` will not compile here; `Store::open`
would. What actually holds the rule is that `update` is synchronous and nothing in the
crate calls a store — asserted by `a_pure_ui_names_no_io`, which greps the crate's own
source for `Store`, `.await`, `spawn_blocking` and the three crate names.

*Why:* the project tui-do replaces awaited network calls while holding a lock on its
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

**5. Writes are optimistic, and merged before they are sent.**
`update` mutates the local store immediately and queues an outbox entry. On rejection the
sync engine emits `Msg::SyncFailed`, which rolls back and toasts. Undo/redo rides on this
mechanism rather than a parallel one.

A queued `UpdateTask` is *not* sent as it was queued. The push reads the server's current
copy and replays the user's field-level change onto it (`Task::merge_onto`), because
tui-do is multi-instance — see below.

**A failure blocks one task, not the queue.** Ordering is a contract *within* a task —
two edits to one task must arrive in order — and not between tasks, which have no causal
relationship. The drain used to stop dead at the first failure, so one unreachable task
held back every other change the user had made. It now blocks that task's subject and
carries on.

**One dependency crosses subjects, and blocking by subject cannot see it.** An
`AttachLabel` built on a `CreateLabel` has the *task* as its subject and carries the
provisional label id by value. The rejection path was widened for that with
`sync::references`; the *blocking* path was not, so a create that took a 500 or a timeout
still let its attach go out naming an id no server had issued — answered `404`/`403`,
which is a 4xx, which discarded the attach **and every other entry for that task**,
including unrelated edits, while the create went on to succeed and leave a label attached
to nothing. Both paths ask `references` now, and an entry still inside its backoff blocks
what depends on it exactly as a freshly deferred one does — otherwise the same failure
simply arrives on the next pass, five seconds later.

**A failed entry waits before it is retried.** `attempts` was recorded from the first
commit and never read by anything; `store::outbox::backoff` now schedules
`next_attempt_at`, exponential from 5s to a 15-minute ceiling, and the server's
`Retry-After` wins when it sent one. There is deliberately **no** attempt cap and no
dead-letter: a permanent failure is a 4xx, which `is_permanent` already rolls back, so
what is left retrying is a 5xx or a transport error — things that may genuinely recover,
and discarding a user's edit because a server was down for a day is not tui-do's decision
to make. `Store::queue_health` reports how many are failing and why, and the status line
says `3 queued (1 failing)`.

**A sync the user asked for ignores that schedule.** `r` and `R` pass
`Backoff::Ignore` and retry everything queued; startup, the timer, and the push that
follows a write pass `Backoff::Respect` and do not. The backoff is right about a server
that is down and wrong about a user who has just fixed their network, and a keystroke is
the only way that news can reach the queue — found driving F7 on 2026-08-30, where a
create came due fourteen seconds after the startup pass had looked at it and nothing
retried for five minutes, with no key that would. In the runtime the trigger is tracked
beside the pass rather than folded into it (`Trigger::Asked`), including through the
coalescing an in-flight pass does: a `fetch_max` over one combined code would let a
scheduled full pass outrank an asked-for delta and silently drop the force.

**A queued mutation's subject is a `Subject`, not a task id.**
`Subject { Task(TaskId), Label(LabelId) }`, stored as `subject_id` plus a
`subject_kind` column (schema v5). Provisional ids count down from `-1` *per kind*, so an
untyped `subject_id` makes provisional task `-1` and provisional label `-1` the same value
in the same column — and nothing about that fails loudly. `retain_tasks`'s delete guard
would spare a task because a *label* was queued; `settle_create`, adopting task `-1`,
would rewrite the payload of a `CreateLabel` whose subject is label `-1`. Seven SQL sites
read that column and all seven filter on the kind; the plan that added it named five, and
the sixth (`upsert_tasks_from_server`'s pending check) and seventh (`retain_projects`'s
task cascade) came out of review.

**A `CreateLabel` that has already failed reads before it writes.** `is_already_done` has
arms for a replayed attach, detach and label rename, and deliberately **no arm for a
create**: a label title is not unique and `PUT /labels` ignores the body's `id`, so a
replay answers `201` and a second label with nothing in the response to tell it from the
first (measured 2026-08-29, above). So a retry — gated on `entry.is_failing()`, because a
first attempt has no earlier attempt of its own to find — asks `GET /labels?s=<title>`
first and adopts an exact match instead of creating. It protects this box's own replay and
not two boxes creating the same title at once, which nothing can without a unique
constraint the server does not have.

**There is no `DeleteLabel`, and a create is not on the undo stack.** `Mutation::inverse`
answers `None` for `CreateLabel` and `u` reaches past it, because undoing a create means
deleting a label — the one label operation `u` cannot honestly reverse, since the label
would come back with a new id detached from every task it was on. If delete is ever built
it needs a confirmation and an honest "this cannot be undone", not a broken undo.

## Wire-format facts the spec does not tell you

The OpenAPI document describes what the server *means*, not what it *emits*. Each of these
cost a live failure to find; all are handled in `tui-do-api`, and new code must route
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

**The spec is authoritative for paths, not for methods or bodies.** Four counts so far,
all decided in the server's favour: `PUT /migration/vikunja-file/migrate` is documented
`post` (server: `405 Allow: OPTIONS, PUT`); `POST /tasks/{taskID}/comments/{commentID}`
documents no request body but requires one; `repeat_mode`'s prose says the third variant is
`3` while the enum in the same document says `2`; and a label update is documented
`put /labels/{id}`, which the server answers `405` — `OPTIONS` replies
`Allow: OPTIONS, DELETE, GET, POST`. Only live integration tests catch this class of
error, and the fourth sat in `Client::update_label` from the first commit because nothing
called it.

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

**Only a full pull may delete.** A pull comes in two reaches (`sync::Reach`). `Full` asks
for the whole listing — 78 pages and 15s against dev — and deletes every local task those
pages did not mention. `Incremental` asks `updated > <watermark>`, which is one page, and
**must not run the retain step**: a filtered listing names what changed, and every task
that did not change is missing from it, so retaining against that list would erase all but
the last few days of the user's tasks. That is the whole reason the enum exists, and it is
asserted by `an_incremental_pull_does_not_delete_what_it_did_not_mention`.

The cost is real and is the user's decision, taken 2026-08-28: `r` is incremental and
cannot see a task deleted in another client, so `R` is bound to a full pull and `r` names
it in its toast. Startup and the timer stay full.

Two state keys, because they stopped being the same fact: `LAST_PULL` is "everything up to
here has been seen" and both reaches advance it; `LAST_RECONCILE` is "and nothing else is
gone", which only `Full` can claim. The watermark is stamped from **before** the requests,
not after — a task edited while the pages were being fetched may or may not have landed in
one of them — and the filter reaches a further two minutes back, because the watermark
comes from this machine's clock and `updated` from the server's, and a few seconds of
disagreement would drop a task in the gap permanently.

**Reminders travel in the task body too, and cost a real bug.** `POST /tasks/{id}`
replaces a task's reminders from the request body exactly as it replaces assignees —
the spec does not say so, and marks only `attachments` and `labels` read-only.
`Task` serialises every field, so a task read back out of a store with nowhere to keep
reminders went to the server carrying `"reminders": []`, and the server deleted them:
renaming a task destroyed its reminders, silently, every time. Measured on dev
2026-08-29 (one reminder in, zero out). The store keeps them now — `task_reminders`,
schema v3 — so the body carries them back, and
`a_task_update_replaces_reminders_from_the_body` asserts both halves: carrying them
preserves them, and sending `[]` still clears them.

**There is no general rule here, and the one this used to state was wrong.** It read
"any `Vec` on `Task` that the spec does not mark read-only is replaced from the body",
which is both unfounded and false. Unfounded: the OpenAPI document sets `readOnly` on
*none* of them — what `attachments` and `labels` have is a sentence of English prose,
one of which contains a typo (`This property is read-onlym`). And false: measured on dev
2026-08-30, a `POST /tasks/{id}` carrying `related_tasks: {}` and `attachments: []` left
a relation and an attachment **untouched**, where the same shape deletes reminders.

| collection | replaced from the body? |
|---|---|
| `reminders` | **yes** — measured, cost a live bug |
| `assignees` | **yes** — measured |
| `related_tasks` | no — measured 2026-08-30 |
| `attachments` | no — measured 2026-08-30 |
| `labels` | no; attached and detached through their own endpoints |

So each collection is its own question and the answer only comes from asking the server.
`a_task_update_leaves_relations_alone` in `tests/live.rs` pins the relations half; the
attachment half was measured by hand, because uploading one needs a multipart request the
client has no method for.

**A write's response is not evidence about them either.** That same update answered with
`related_tasks` empty while the relation was still on the server — the request had sent
none and the response echoed none. Nothing may read a write's answer and conclude a task
has no relations, which is the same shape as `sync::with_labels`'s caution one paragraph
up, arrived at from the other direction.

**`GET /projects` omits archived projects unless asked.** `is_archived=true` reads like
a filter and is the opposite — the spec words it "if true, *also* returns all archived
projects". The sync engine feeds that listing to `retain_projects`, which deletes every
project the listing did not name **and cascades to its tasks**, so the missing parameter
deleted the user's archived projects and everything in them on *every* pull. Not
contained by `Reach`, either: both reaches call `pull_lists` unconditionally, which is
why the delete-safety reasoning about `Full` versus `Incremental` did not cover it.
`an_archived_project_and_its_tasks_survive_a_pull` mocks the two shapes the way the
server answers them.

**Writing a label: three findings, measured on dev 2026-08-29.** The label lifecycle is
not the task lifecycle with a different noun.

- **The update verb is `POST /labels/{id}`**, not the `put` the spec documents. See above.
- **A partial body clears what it omits**, exactly as a task body does: `POST /labels/12`
  carrying only `title` cleared `hex_color` to `""`. A rename sends the whole label.
- **The body's `id` beats the path, and silently writes to a different label.**
  `POST /labels/12` carrying `"id": 13` updated label **13**, left 12 untouched, and
  answered with 13. This is the same path-versus-body binding order as
  `PUT /projects/31/tasks`, but the failure is worse: the task case 404s about a project
  that does not exist, and this one succeeds against the wrong row. `update_label` takes
  the whole label and derives the path from it so the two cannot disagree; anything that
  ever splits them must write the path id into the body.

**A label title is not unique, so a replayed create is undetectable.** Creating
`tui-do probe alpha` twice answered `201` twice with two different ids. There is nothing
in the response to tell a duplicate from a first creation, so `is_already_done` cannot
grow an arm for it — a `CreateLabel` whose response was lost must reconcile against
`GET /labels?s=<title>` before it retries, or the user gets two labels. Creation ignores
the body's `id` entirely: `0` and `-7` both came back with a server-assigned id, so
carrying a provisional id on the wire is harmless.

The rest of the replay table, same measurement session:

| asked | answered |
|---|---|
| delete a label that is already gone | `404`, code `8002`, "This label does not exist." |
| rename a label that is already gone | `404`, code `8002`, the same |
| create a label whose title already exists | `201`, a second label |

**Assignees travel in the task body; labels do not.** `POST /tasks/{id}` replaces the task
from the body, and an empty `assignees` clears them. Labels are the opposite: they are
attached and detached through their own endpoints and the body's `labels` field is ignored.

Verified on dev by assigning a user and re-reading through a list endpoint: **list results
do populate assignees**, so a task fetched from a list can be passed back to `update_task`
safely, and a `null` assignees field means genuinely nobody rather than "not loaded". What
is still unsafe is *constructing* a task from partial data and sending it — anything that
does must fill assignees itself or it will unassign everyone.

**A description is Markdown, plain text, or HTML, and nothing on the wire says which.**
Measured on dev 2026-08-30 across 487 non-empty descriptions: 367 carry no tag at all, 108
are TipTap HTML from the web editor, and the rest are mixed. A task written through the API
keeps what was sent; a task touched in the web editor comes back as HTML. Both persist, so
every reader meets both.

What tells them apart is a **recognised HTML block tag**, not a `<`: real descriptions say
`Use Vec<String> here` and `# - CAM_<CAMERA_MAC>_NAME=fr`, and routing those to an HTML
parser deletes the bracketed word. `markdown::looks_like_html` is the one place that
decision is taken.

Two `comrak` options in `markdown::render` are load-bearing and both fail *silently, by
deleting text*. `render.hardbreaks` keeps a single newline as a line break — CommonMark
joins consecutive lines, which runs an address block together, and roughly 350 of those 367
descriptions carry their structure in single newlines. `render.escape` keeps text that looks
like a tag as text — without it `<String>` is read as raw HTML and dropped, and the line
reads `Use Vec here`. Each has a named regression test.

`html2text`'s CSS parser drops a stylesheet block's final declaration silently if it lacks a
trailing `;` — `add_agent_css` returns `Ok` either way, so a missing semicolon on the last
rule in a block is a no-op and headings simply come out uncoloured. Every rule needs its own
trailing `;`, including the last.

`html5ever` strips `<script>` and `<style>` element bodies on its own; nothing here needs to
repeat that filtering.

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

**Dates the user sees are resolved in the user's zone, not UTC.** `Model.now` is a
`DateTime<FixedOffset>` stamped from `Local::now()` by `runtime::now()`, and it is the
only clock the interface sees. It was `Utc::now()`, which meant `due today` resolved to
23:59 UTC — 18:59 the same evening for a UTC-5 user, so every task turned overdue hours
early — and `relative_date` compared UTC calendar days, so a task due this evening read
as "Tomorrow" and then flipped to "Today" at midnight UTC. This is the bug cria has, and
`quickadd/dates.rs`'s own doc comment claims to have fixed it; the parser always was
correct, and only the wiring was wrong. Anything asking "what day is it" converts into
`now.timezone()` first. Anything asking "has this instant passed" does not need to.

**Opening a link is the runtime's job, and whether to open at all is the environment's.**
`o` reaches `markdown`-adjacent `urls::extract`, which is pure string work over the task's
title *and* description — 245 of the 576 linked tasks on dev carry their URL in the title,
so reading only the description misses nearly half of them. It scans for the scheme and
reads to the first character that cannot be in a URL, which terminates all three shapes at
once: bare text, Markdown's `)`, and HTML's `"`. No second grammar is parsed.

Two things about it are easy to get wrong and are pinned by tests. `)` is **not** a
terminator — it appears inside real addresses (`…/wiki/Rust_(programming_language)`) and a
terminator can only cut a URL short, never restore it — so parens are counted afterwards
instead. And `&amp;` in an `href` is `&`; a query string carrying the entity is not the
address anyone copied.

**`xdg-open` is wrong on most of the fleet.** Over SSH it either fails or opens a browser
on the machine at the far end of the connection, which is not where the person is sitting,
and tui-do is used across boxes over Tailscale. So `Model::url_action` — set once by
`runtime::url_action()` beside the theme, never sniffed in `tui-do-ui` — says whether `o`
opens or copies, and the copy goes out as **OSC 52** so it lands in the clipboard of the
terminal the *user* is at. `wl-copy` would have put it on the wrong machine's clipboard,
which is the whole reason a local clipboard tool is not used. A terminal that ignores OSC
52 leaves the clipboard untouched and reports nothing, which is why the toast names what
it copied rather than merely saying "copied".

**This is the platform seam the macOS port fills in**, and the reason Phase 5's
`MarkdownRenderer` trait was dropped rather than built: rendering turned out to need no
platform knowledge at all, and URL opening genuinely does.

## tui-do is multi-instance

A developer with a fleet of boxes, tui-do on each, all against one Vikunja. Each box has
its own SQLite store; the server is the only shared state. So the concurrency that matters
is **concurrent writers against one server**, not two processes on one database file —
there is no shared file, and nothing here needs cross-process locking.

**Vikunja offers nothing to build on.** No version field, no ETag, and `updated` is
server-set and explicitly unwritable. There is no conditional write to ask for.

**A partial body clears every field it omits.** Measured on dev 2026-08-29: a
`POST /tasks/{id}` carrying only `id` and `title` cleared `description` to `""`, `priority`
to `0` and `due_date` to the zero time. So "send only what changed" is not available
either — a write must carry the whole task.

Both roads being closed is what forces the third: **read, merge, write.** `Task::merge_onto`
is a three-way merge of `before` (what the user started from), `after` (what they want)
and the server's current copy — their fields win, everything else keeps the server's value.
It does not make concurrent editing safe; it narrows the window from "since this box last
pulled", which is minutes at best and hours after an offline spell, to one request.

On a true collision — both changed the same field — the user's value wins and
`SyncEvent::Overwrote` toasts it. Refusing would lose what they just typed; overwriting in
silence is how a fleet loses work nobody can account for.

`server` is destructured exhaustively in `merge_onto` on purpose: a new field on `Task`
fails to compile there rather than silently keeping the server's value, which is how a
field quietly becomes uneditable.

## Environment

| | |
|---|---|
| **Dev server** | `https://dev-box.example.net:8443` — use this for everything |
| **Prod server** | `https://prod-box.example.net:8443` — **read-only, always.** Never a write target, never a test target |
| **Vikunja version** | v2.5.0 (dev pinned to match prod) |
| **TLS** | Tailscale Serve certs are publicly trusted; never disable certificate verification |

Reset the dev server to its seeded baseline with `deploy/reset-dev.sh`. Seed it from a prod
export with `deploy/seed-from-prod.sh` (which reads prod and writes only to dev).

`crates/tui-do` refuses to start against the prod URL without `--i-know-this-is-prod`, and
integration tests refuse to run unless `TUI_DO_TEST_URL` points at dev. Do not weaken either
guard to make something pass.

## Driving another box by hand

The fleet is hand-driven constantly — the hardware checks, the offline checks, an rc
install — and one thing about it fails every single time.

**Never hand the user multi-line shell content to paste into a remote terminal.** It
arrives mangled. Measured twice within minutes on 2026-09-06 while installing
`v1.0.0-rc.1` on `arm-host-1`: a `cat > config.yaml <<'EOF'` block came through with every
line indented, so the closing `EOF` no longer matched its delimiter, bash sat at the `>`
continuation prompt, and `^C` aborted the command *before it ran* — leaving no file and
an error identical to the one being fixed. The `printf` written to avoid the heredoc then
lost a `\n` and gained a stray `s`, collapsing `url:` and `token_file:` onto one line.

So: **move the file, do not retype it.** `scp` from a box that already has a working one
copies something already proven to parse, and it is one short line with nothing in it to
corrupt — that is what worked, for both the token and the config. Where something must be
typed, keep it to one short line, no escape sequences, no leading whitespace, and follow
it with a `cat` so what actually landed is visible before anything depends on it.

A mangled config does at least fail loudly, because `deny_unknown_fields` rejects what it
does not recognise. Say so when handing one over: it means a parse error is the paste, not
the binary.

**Scratch files are deleted by whoever made them**, on this workstation, on a remote host,
and in the working tree. `md/ruflo-audit/`, `md/bloat-detector/` and `md/minify/` are what
happens otherwise — untracked and unignored for days, one of them holding a live dev API
token that a single `git add -A` would have committed. They are ignored now; the habit is
the actual fix.

## Platform policy

Linux only through GA — tested on Omarchy (Arch, this workstation) and Ubuntu 26.04 LTS (via
`ubuntu:26.04` container). macOS is a post-GA port; there is a non-gating `cargo check` in CI
purely to limit drift. Windows is answered with "use WSL" and is not a build target.

Still write portably where it is free: paths via `dirs`, never cwd-relative writes,
`rusqlite` with `bundled`. Platform-varying behavior (URL opening, markdown rendering,
clipboard, `$EDITOR`) goes behind a small trait with a Linux impl — that trait is the seam
the macOS port uses later.

## Where a test that spans layers goes

Rule 1 buys purity at a price: `tui-do-ui` cannot see a store and `tui-do-core` cannot see
a keystroke, so the assertions that matter most have nowhere to live. The UI tests fake the
store's answers and the store tests fake the user, and *between* them is a seam where a
message can be produced with arguments nobody agreed on while every test stays green.

`crates/tui-do-smoke` is that seam's home — `publish = false`, no binary, depends on both.
`Harness` reimplements the effect runtime faithfully but small: same store calls, same
messages back, and a push deferred until the reload it races has landed, because the real
runtime's reload is a local read and its push is a round trip. Getting that order wrong is
not cosmetic — with the push run inline, the label form picked the server's id up through
`absorb_created` and the F3 smoke passed with the engine's adoption event *deleted*.

What it cannot see is the runtime's own wiring, which is why the manual checks stay.

## Commands

```sh
cargo build --workspace
cargo build --workspace --release         # what `tui-do` on PATH actually runs -- see below
cargo clippy --workspace --all-targets    # must be clean; CI runs with -D warnings
cargo fmt --all
cargo test --workspace
cargo xtask fetch-spec                    # refresh spec/vikunja.json from the dev server
deploy/test-ubuntu.sh                     # build + test in the ubuntu:26.04 container
```

**`~/.local/bin/tui-do` is a symlink to `target/release/tui-do`**, so that is the binary a
manual check exercises. A debug build proves the tests pass and changes nothing the user
is looking at: handing over a fix without `--release` means they retest the old code and
report it still broken. Build release before saying a fix is ready to try.

Workspace lints deny `unwrap`, `panic`, `todo`, `dbg!` and forbid `unsafe`. Tests may
`allow` them at module level; production code may not.

## Where tui-do keeps things

The config is at `~/.config/tui-do/config.yaml`, the store at
`~/.local/share/tui-do/tui-do.db`, and `~/.local/bin/tui-do` is a symlink to
`target/release/tui-do`. None of the three is inside the repository, so a change here does
not reach them — which is why a fix has to be built with `--release` before it can be
tried.

`cria` is the *predecessor*, still checked out at `../cria`. Every mention of it is
deliberate.

## Reference material

`../cria` is the predecessor, checked out for reference. Read it to learn *what* a screen
shows or *how* the quick-add syntax behaves — its `tests/` are a useful behavioral spec. Do
not copy its code: it carries no license (no `LICENSE` file was ever committed), and its
architecture is the thing tui-do exists to replace.

The Vikunja web UI is the design reference for layout and interaction.
