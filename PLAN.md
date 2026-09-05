# tui-do — Foundation Plan

## Context

`tui-do` is a fork-in-spirit of [cria](https://github.com/frigidplatypus/cria), a Rust/ratatui TUI client
for [go-vikunja](https://github.com/go-vikunja). A patched copy of cria lives at `/home/swasko/code/cria`
(the local fix: `/tasks/all` → `/tasks`). Upstream has been unresponsive for months to both issues and
direct messages, so tui-do is a real fork, not a contribution track.

An audit of that codebase (14.4k LOC src, 5.4k LOC tests, 63 files) found the *domain* work is valuable
but the *architecture* is not salvageable in place:

- `src/tui/app/state.rs:22` — `App` is a God object with ~100 fields, including **22 `show_*_modal` bools**
  paired with `Option<Modal>` values. No state machine; illegal states are fully representable.
- `src/ui_loop.rs:16` — `run_ui` is a single ~790-line function with a 46-branch `if app.show_X` chain,
  **15** pointless `lock().await` → `drop()` → `lock().await` sequences, and `.await`ed network calls made
  *while holding the `App` lock* (so the TUI freezes during any slow request). A blocking 250 ms
  `event_handler.next()?` stalls the tokio runtime each iteration.
- Two competing API clients (`src/vikunja/client.rs` is entirely dead, every method tagged
  `#[allow(dead_code)] // Alternative client implementation`) and two competing task models
  (`VikunjaTask` at `vikunja_client/tasks.rs:15` vs `vikunja::models::Task`).
- **A live data-loss bug.** `get_all_tasks_comprehensive` (`vikunja_client/tasks.rs:616`) is 195 lines of
  "Method 1 / Method 2 / Method 3" endpoint guessing. The server reports `max_items_per_page: 50`; cria
  requests `per_page=10000` and is silently capped. Its fallback chain isn't compensating for a broken
  endpoint — it's papering over unhandled pagination, and almost certainly dropping tasks past the first 50.
- 67 `unwrap()`, 198 `.clone()`, no domain error type. CI runs `build` + `test` only — no clippy, no fmt.
- A 206 KB compiled `libterminal_capabilities.rlib` is committed to git; several source files are empty.

Since the target UX is *Vikunja-inspired* — i.e. the UI layer is being redesigned regardless — the worst
code is exactly the code we were going to replace. What remains worth keeping is everything that **isn't**
the UI.

**Outcome:** a new `tui-do` repo at `/home/swasko/code/tui-do` with a local-first architecture where the
render loop never touches I/O, built on current dependencies against a spec pulled from a real server,
developed against an isolated dev instance so production task data is never at risk.

## Locked decisions

| | |
|---|---|
| **Strategy** | Fresh repo, aggressive harvest. `../cria` stays checked out as reference only — never a dependency. |
| **Stack** | Rust 1.97 + ratatui 0.29 + crossterm 0.29 (cria is on ratatui 0.26 / crossterm 0.27, ~2 years stale) |
| **Data layer** | Local-first: SQLite is the UI's source of truth; a background sync engine reconciles with the server |
| **Compatibility** | Clean break. New config at `~/.config/tui-do/config.yaml`; `tui-do migrate` imports a cria config |
| **License** | `MIT OR Apache-2.0` (Rust ecosystem standard; Apache adds the patent grant MIT lacks) |
| **Platforms** | **Linux only through GA.** macOS is a post-GA port. Windows: use WSL — a documented answer, not a target. |
| **Dev target** | Isolated Vikunja on `dev-box`, seeded from a prod export. Prod is **read-only, always.** |

## Environment (probed live 2026-08-24)

- **Prod Vikunja:** go-vikunja **v2.5.0**, `https://prod-box.example.net:8443` → `127.0.0.1:3456`,
  Postgres, behind Tailscale Serve. `max_items_per_page: 50`. `available_migrators` includes
  **`vikunja-file`**, so the export/import seeding path is enabled.
- **Live spec beats the bundled one decisively.** `GET /api/v1/docs.json` returns 200 unauthenticated:

  | | live (v2.5.0) | cria's bundled `docs.json` |
  |---|---|---|
  | paths | **126** | 104 |
  | definitions | **98** | 80 |

  `/tasks/all` **removed**, `/tasks` **added** — the local patch, provable from the spec. 23 endpoints cria
  has never seen, including `/user/token/refresh`, `/user/logout`, `/tasks/{taskID}/duplicate`,
  `/projects/{project}/tasks/by-index/{index}`, and the `/migration/csv/*` family.
- **dev-box:** Surface Laptop 4, i7-1185G7, Arch (kernel 6.19.8), **Docker 29.7.2 installed**, ~11 GB RAM
  free, **302 GB free on `/`**, reachable over the tailnet. SSH is its only remaining service.
- **Homelab conventions to follow:** compose stack at `/opt/stacks/docker-compose.yml`, data on local disk at
  `/opt/appdata/<service>` (never SMB), secrets at `/opt/stacks/<service>.env` mode 600 and gitignored,
  HTTP services bind `127.0.0.1` and are published via `tailscale serve`.

## Architecture

**The one rule that fixes cria's central defect: the render loop never awaits I/O.**

Elm-style, single-threaded UI with an async effect runtime alongside it:

```
crossterm events ─┐
                  ├─► Msg ──► update(&mut Model, Msg) -> Vec<Effect> ──► view(&Model, Frame)
sync engine ──────┘                    │                                   (pure, sync, fast)
                                       ▼
                            Effect ──► async worker (tokio)
                                       │  API calls, SQLite writes, file I/O
                                       └──► Msg back over mpsc
```

- `Model` owns no `Arc<Mutex<_>>`. No locks in the UI path at all.
- Screen/modal state is an **enum + stack**, not 22 booleans:
  ```rust
  enum Screen { TaskList, Detail(TaskId), Kanban(ProjectId) }
  struct Model { screen: Screen, modals: Vec<Modal>, /* ... */ }
  enum Modal { QuickAdd(QuickAddState), Edit(EditState), Picker(PickerState), Confirm(ConfirmState), .. }
  ```
  Adding a modal means adding one enum variant, not five fields and a branch in a 790-line function.
- Every modal implements a shared `trait ModalView { fn handle_key(..) -> ModalOutcome; fn render(..); }`
  — cria hand-rolls 13 independent `draw_*_modal` functions in one 1,155-line file with no shared abstraction.
- Writes are **optimistic**: `update` mutates the local store immediately and emits an `Effect::Push`.
  On failure the sync engine emits `Msg::SyncFailed`, which rolls back and raises a toast. This is what
  makes undo/redo trivially correct, versus cria's ad-hoc undo stack.

### Workspace layout

```
tui-do/
├── crates/
│   ├── tui-do-api/     Typed go-vikunja client. Models, endpoints, auth, thiserror errors. No TUI, no SQLite.
│   ├── tui-do-core/    Domain models, SQLite store, sync engine + outbox, quick-add parser, config.
│   ├── tui-do-ui/     Model/Msg/update/view, widgets, keymap, theme. Pure + sync. No network.
│   └── tui-do/         Binary: clap CLI, wiring, terminal setup/teardown, panic hook.
├── spec/vikunja.json  Live API spec, refreshed from the dev server
├── deploy/            docker-compose + seed/reset scripts for the dev instance
└── xtask/             Model codegen from spec, spec-conformance check
```

The crate boundaries are the enforcement mechanism: `tui-do-ui` cannot depend on `reqwest`, so the freeze
bug is structurally impossible.

### Platform policy

**Linux end-to-end through GA.** No cross-platform work competes with shipping. macOS is a deliberate
post-GA port, and tui-do's answer for Windows is **"use WSL"** — stated plainly in the README, not carried
as a build target.

Tier-1 test targets, both real:

| Target | Where | Note |
|---|---|---|
| **Omarchy Quattro** | this workstation (**Omarchy 4.0.0-1**) + `tiny-box` | Primary dev and daily-driver target. `tiny-box` is a second Omarchy 4 box — but the KB flags it asleep on a default power policy and unreachable until ~2026-08-30. |
| **Ubuntu 26.04 LTS** | `ubuntu:26.04` container on `dev-box` | No homelab host runs 26.04 (prod-box/spare-box/arm-host-1 are all 24.04.4); the image tag exists and is published. Scripted as `deploy/test-ubuntu.sh` — build + test in-container against the dev Vikunja. Doubles as the glibc-version floor check, since Arch and Ubuntu LTS differ sharply there. |

Two habits kept from day one because they're good practice on Linux regardless — not as Windows tax:

- All paths through `dirs`/`directories`. No hardcoded `~`, no cwd-relative writes — cria writes attachments
  into a cwd-relative `downloads/` directory from inside its event loop, which is a bug on any OS.
- `rusqlite` with the `bundled` feature — no system SQLite dependency, which is what makes a single static
  binary work across Arch and Ubuntu LTS in the first place.

Platform-varying behavior (URL opening, markdown rendering, clipboard, `$EDITOR`) sits behind small traits
with a Linux implementation only. That's the seam the macOS port slots into later; the plan explicitly
accepts that some rework will be needed then, rather than paying for it now.

**One cheap exception:** a non-gating `cargo check` on `macos-latest` in CI. It costs one matrix cell and
nothing in attention, but keeps the post-GA macOS port from becoming an excavation. Drop it if it's noise.

## Phases

### Phase 0 — Guardrails first

Do this before writing feature code; it's what keeps quality from drifting.

- `cargo new` workspace; `git init`; add `LICENSE-MIT` + `LICENSE-APACHE`, set
  `license = "MIT OR Apache-2.0"` in the workspace manifest, and credit cria in the README.
- Run **`/init`** to author `CLAUDE.md` capturing the architecture rules above.
- Run **`update-config`** to install a PostToolUse hook: `cargo fmt` + `cargo clippy --all-targets -- -D warnings`
  on every `.rs` edit.
- Run **`fewer-permission-prompts`** to allowlist `cargo build/test/clippy/fmt`.
- Workspace lints in root `Cargo.toml`:
  ```toml
  [workspace.lints.clippy]
  unwrap_used = "deny"        # cria has 67
  expect_used = "warn"
  panic = "deny"
  redundant_clone = "warn"    # cria has 198 .clone()
  ```
  (`unwrap` stays allowed in `#[cfg(test)]`.)
- CI: `fmt --check`, `clippy -D warnings`, `test`, `cargo-deny` on `ubuntu-latest` (gating), plus the
  `ubuntu:26.04` container job and a non-gating `cargo check` on macOS. Cria has none of this.
- `.gitignore` excluding `target/`, `*.rlib`, `*.env`.

### Phase 0.5 — Dev Vikunja on dev-box

Stand this up **before** Phase 1, so every subsequent phase is verified against a server we can freely break.

1. **Deploy.** `deploy/docker-compose.yml` in the tui-do repo: `vikunja` + `postgres:16`, pinned to the
   **same v2.5.0** as prod. Data at `/opt/appdata/tui-do-vikunja-db`, secrets in
   `/opt/stacks/tui-do-vikunja.env` (mode 600, gitignored). Bind `127.0.0.1:3456`.
2. **Publish.** `tailscale serve` on `:8443` → `127.0.0.1:3456`, giving
   `https://dev-box.example.net:8443` — same port as prod so only the hostname differs.
3. **Seed, prod-read-only.** `POST /user/export/request` on prod → poll → `POST /user/export/download` →
   import into dev with the `vikunja-file` migrator. Scripted as `deploy/seed-from-prod.sh`. The script
   takes prod strictly as a **source**: no writes, no DB access, no `pg_dump`, hard-refuses any prod URL
   as a destination.
4. **Golden snapshot.** Immediately after seeding, `pg_dump` the *dev* DB to
   `/opt/appdata/tui-do-vikunja-seed.sql`. `deploy/reset-dev.sh` restores it in seconds, so destructive
   test runs are cheap and repeatable.
5. **Prod-write guard, belt and braces.** `tui-do` refuses to run against a URL matching
   `TUI_DO_PROD_DENY` (defaulting to the prod-box host) unless `--i-know-this-is-prod` is passed, and
   integration tests hard-fail unless `TUI_DO_TEST_URL` points at the dev host. The dev URL is the
   committed default in every config example.
6. **KB update — ask first.** `common-hosts.md` has an explicit agent write policy: *an agent may edit it,
   but must ask Steve every time, showing the intended diff and what was verified.* So this is a
   **proposal step, not an automatic one**: draft the diff replacing dev-box's `DECOMMISSION PENDING`
   note with its new dev-host role, adding the two service rows and a `Recent changes` entry, then present
   it for approval before touching the file.

### Phase 1 — `tui-do-api`

- **Fetch the live spec** from the dev instance into `spec/vikunja.json` and treat it as ground truth.
  Never guess an endpoint again.
- `xtask generate-models` reads the spec's `definitions` → serde structs in `tui-do-api/src/models/`.
  Generate the ~30 types we need, not all 98. Commit the output (readable, greppable, diffable).
- Hand-write endpoint methods (~25) — codegen for Swagger 2.0 request shapes in Rust costs more than it saves.
- **Spec-conformance test**: assert every URL path template the client constructs exists in
  `spec/vikunja.json`. Refresh the spec, run tests, and an upstream API change becomes a failing test
  instead of a runtime 404 — exactly the `/tasks/all` breakage, caught automatically.
- **Pagination as a first-class concern.** Read `x-pagination-total-pages` / `x-pagination-result-count`,
  respect the server's `max_items_per_page` from `/info` rather than assuming, and expose a paged stream.
  This is the fix for cria's silent truncation, and it gets a regression test with >50 tasks.
- Auth: support both schemes the spec declares — static API token *and* `POST /login` → JWT, with
  `/user/token/refresh` and `/user/logout` (all three invisible to cria).
- `thiserror` error enum distinguishing transport / 4xx / 5xx / deserialization. `anyhow` only in the binary.
- Prefer the modern view-based endpoints (`/projects/{id}/views/{view}/tasks`) — this is how the Vikunja
  web frontend actually loads tasks, and it's the foundation for Kanban later.

### Phase 2 — `tui-do-core`

- **SQLite store** via `rusqlite` (feature `bundled`); DB calls run under `spawn_blocking`. Schema:
  `tasks`, `projects`, `labels`, `saved_filters`, `sync_state`, `outbox`.
- **Sync engine**: periodic pull + an outbox queue for mutations, with retry and rollback. Emits
  `Msg::SyncStarted` / `SyncFinished` / `SyncFailed`.
- **Quick-add parser** — port the behavior from `cria/src/vikunja_parser.rs` (615 LOC): `*label`,
  `@assignee`, `+project`, `!1`–`!5`, `due …`, `start …`, `every N days`, `at 5pm`, plus the
  quoted/bracketed variants and the `today|tomorrow|eow|eom|next monday` keyword set. Written against
  Vikunja's documented quick-add syntax, with cria's tests brought across as the conformance set.
- **Config** — reimplement cria's `config.rs` schema (it's the best-written file in the repo): XDG
  resolution, `api_key_file` indirection with tilde expansion, quick actions, column layouts. Add
  `tui-do migrate` to import `~/.config/cria/config.yaml`.

### Phase 3 — `tui-do-ui` first light

Read-only milestone: launch, load from SQLite, browse. Should be *instant* — no network on the critical path.

- Event loop, `Model`/`Msg`/`update`/`view`, modal stack, `ModalView` trait.
- Task list with cria's configurable column layouts (`COLUMN_LAYOUTS.md` documents the schema).
- Declarative keymap table, rendered directly into the help modal so they can never drift apart.
- Theme module with Vikunja's palette; priority/due-date/label colorization.

### Phase 4 — Mutations

Toggle done, edit, quick-add, the Space-prefixed quick actions (`QUICK_ACTIONS.md`), undo/redo on the
optimistic-write mechanism, project/label/filter pickers with fuzzy match.

**The label lifecycle was a gap in this phase, and was closed out of sequence on 2026-08-29.** Nothing
above says *create a label*, so nothing did: the pickers could only choose from labels the server already
had, and four places in the interface apologised for it in words ("tui-do cannot create labels yet"). It
was found after Phase 4 was signed off and built before Phase 5, at the user's direction — designed in
`md/2026-08-28-label-creation-design.md` and shipped in eleven tasks. What landed: a queued mutation's
subject became a typed `Subject` with a `subject_kind` column (schema v5); `Mutation::CreateLabel` and
`UpdateLabel`, with `Label::merge_onto` and a read-before-retry for a create whose response may have been
lost; `Ctrl-N` in the `l` form to create, `Ctrl-E` there and in the `g l` picker to rename or recolour, a
`y`/`n` confirmation for an unknown `*label` in quick-add and the edit form, and `--create-labels` for
`tui-do add`, which has no interface to confirm in. **Deliberately no delete** — see decision 1 of the
design. The rules it left behind are under rule 5 of `CLAUDE.md`; the checks a human drives are section F
of `md/MANUAL-CHECKS2.md`.

### Phase 5 — Rich features

- **Task descriptions, rendered.** A description is Markdown, plain text, or the HTML the web
  editor stores, and the API does not say which. `comrak` renders the first two to HTML, the
  third is HTML already, and `html2text` turns both into wrapped lines annotated with styles
  from `theme.rs`. In process, so `update` gains no `Effect` and the model gains no cache.
  **`glow` was specified here and was measured and rejected on 2026-08-30** — it flattens an
  HTML description to a single run, pads every line to a fixed width with styled spaces,
  emits OSC 8 that ratatui cannot represent, and paints in its own palette. None of that is
  particular to `glow`, so `markdown_renderer: glow | builtin | auto` and `doctor`'s
  markdown line are dropped with it: there is no external binary left to choose between.
  Design and measurements in `md/2026-08-30-markdown-descriptions-design.md`.
- Task detail pane — fields and the rendered description, done. It may later carry a count of comments and
  attachments; `comment_count` is already in the store and neither needs a download path.
- **URL extraction and opening — done 2026-08-31.** `o` opens the link in the selected task, and asks which
  when there is more than one. Extraction reads the title as well as the description, because 245 of the
  576 linked tasks carry their URL in the title; it scans for the scheme rather than parsing three grammars,
  which works because Markdown closes a link with `)` and HTML with `"`. Validated over the whole store: 576
  tasks, 676 links, no mis-extraction.
  **On a box with no display it copies instead of opening, and says so.** `xdg-open` over SSH either fails or
  opens a browser at the far end of the connection, which is not where the person is — and a fleet over
  Tailscale makes that the ordinary case. The copy goes out as OSC 52, so it lands in the clipboard of the
  machine the *user* is at rather than the one tui-do runs on. Which of the two `o` does is
  `Model::url_action`, set once by the runtime beside the theme, so `update` reads a field instead of
  sniffing the environment and rule 1 holds.
- **Comments are deferred, decided 2026-08-31.** Deferred, not dropped: revisit if there is real interest
  after launch. No task on this instance has ever had one — 0 of 3,878, measured against `comment_count`,
  which the store already keeps — and the interface work is a thread view plus multi-line prose entry, which
  drags in the `$EDITOR` question that `C-n`'s "cannot type a space" already foreshadows. That is real work
  for a feature with no demonstrated demand. The client half exists (`task_comments`, `update_comment`,
  `delete_comment`) and has never been tested against a real server.
  **Measure before building:** `Task` serialises `comments` and the store has no column for them, which is
  the exact shape of the reminders bug — a field the store cannot hold goes back to the server as `[]`.
  `merge_onto` looks like it protects them, but that is inference, not measurement, and this file has
  already deleted one general rule that was "both unfounded and false".
- **Subtasks and relations are deferred, decided 2026-08-31.** Deferred, not dropped, on the same terms.
  A relation is any typed link between two tasks — subtask/parent, blocking/blocked, precedes/follows,
  duplicates, copied-from, or loosely related — and cria has them half-built and disabled, which is the
  warning: do it properly or not at all.
  Deferring is cheap and was reasoned about rather than assumed. The store is a *cache*, not the system of
  record, so the worst case of any later migration is deleting the file and re-syncing; migrations are
  additive and each runs inside its own transaction with `user_version` stamped in the same transaction, so
  there is no half-migrated state to land in. Crucially the urgency that forced reminders to schema v3 does
  **not** apply: `related_tasks` is measured as *not* replaced from an update body, pinned by
  `a_task_update_leaves_relations_alone`, so nothing is silently lost while this waits.
  What it will cost when it is built is not the `CREATE TABLE`. It is the retain and delete-cascade rules —
  the one place this project has actually lost data — the outbox `Subject` model, which schema v5 already
  showed is easy to get wrong, and the unsolved display question of what a subtask looks like in a flat,
  sorted, filtered list.
- **Attachments are out of scope, decided 2026-08-30.** Not deferred with a plan to return: dropped. Reading
  them needs a download path and a place to put files; writing them needs multipart upload, which nothing in
  `tui-do-api` does; and showing them in a terminal needs image-protocol negotiation per terminal. That is
  the largest item in this phase and the one a task client is least often reached for. `Task::attachments`
  still deserialises, and a task carrying attachments survives an update untouched — measured 2026-08-30,
  recorded in `CLAUDE.md` — so nothing here loses data. The detail pane may say how many there are.

### Phase 6 — Vikunja-parity views

Kanban buckets, saved filters via the views API, table/Gantt. This is where "Vikunja as inspiration" pays off.

### GA bar (Linux)

tui-do is GA when, on **both** Omarchy Quattro and Ubuntu 26.04 LTS:

1. Phases 1–5 are complete and every gate below is green.
2. A single static binary installs and runs with no system SQLite and no runtime surprises.
3. It survives a full day as the daily driver against the dev instance, then against prod, without a freeze,
   a panic, or a lost write.
4. Offline start, offline edit, and reconnect-and-sync all work.
5. README documents the WSL answer for Windows and states macOS as post-GA.

**Post-GA, in order:** macOS port (fill in the platform traits, fix whatever the `cargo check` cell has been
quietly warning about, test on `mac-laptop`), and standalone mode with a first-run setup (below). The two are
independent and the order between them is not decided. Then reassess whether anything else is worth
supporting.

## Post-GA — standalone mode and first-run setup

**Decided 2026-09-01: build it, after launch.** tui-do runs local-only against its own SQLite store and
never speaks to a Vikunja server, and a first-run setup asks which of the two the user wants. Not a
separate build or a feature flag — one binary, one question at first run.

**They are one feature, not two.** The setup script's first question *is* the mode: answer "no server" and
the URL, the token and the sync interval all disappear. Built separately, the wizard is a form that can only
be filled in one way and standalone mode is reachable only by hand-editing YAML. The README already carries
an honest note that there is no first-run wizard; this closes that too.

### What already holds — surveyed 2026-09-01, not built

Most of this is done, as a consequence of rule 1 rather than by intent:

- **The interface never speaks to the server.** `update` is synchronous and reads the store through
  effects, so everything on screen is a function of local SQLite. Nothing in the render path has a server
  to lose.
- **"No server" is already a designed state, not an error path.** `runtime::build_sync` returns
  `Option<Arc<Sync>>` and answers `None` with *"No API token configured — showing the local cache only."*
  Every `spawn_sync` site already handles that `None`. `run()`'s own comment: *"an offline start that
  renders the cached list is the whole thesis of the rewrite."*
- **`sync.enabled: false` already skips the timer** — `spawn_sync_timer` returns before spawning.
- **Writes are already local-first.** The mutation lands in the store immediately; the outbox entry is a
  separate concern. Provisional ids are already negative and allocated locally, and never need a server to
  answer. Undo is a stack of `Mutation`s in the model, independent of anything draining.

### The blocker: no project can be created

`Mutation` has seven variants — `CreateTask`, `UpdateTask`, `DeleteTask`, `AttachLabel`, `DetachLabel`,
`CreateLabel`, `UpdateLabel`. **Nothing for projects.** They only ever arrive from a pull, so a standalone
install starts empty and stays empty. Verified against a never-synced store on 2026-09-01:

```
Error: no project to add to — tui-do knows of no projects at all;
run tui-do once to sync, or check the server is reachable
```

`Store::upsert_projects` is already generic rather than `_from_server`-shaped, so the store layer is ready.
The gap is a mutation, the interface to drive it, and a third `Subject` kind. **That last part is the
dangerous one:** `subject_kind` filtering has already been got wrong twice across seven SQL sites, and
schema v5's own history is the warning.

### Decide this before writing any code: does the outbox stay armed?

**What happens when a local-only user later gets a server.** Not hypothetical — it is the likely path for
exactly the users standalone mode widens to: try it local, like it, stand up Vikunja six months later.

- **Keep queuing.** Every local write already produces an outbox entry with a provisional negative id. If
  those accumulate, "connect a server" is letting the drain run, and `settle_create` already knows how to
  swap a provisional id for a server-assigned one across every table referencing it. **The first-connect
  pass must reconcile, not pull-then-retain** — the retain step would delete every local task the empty
  server never mentioned, which is BUG-15's shape exactly and the one place this project has lost data.
- **Or do not queue at all**, and accept that connecting later leaves the local history local, or needs an
  export/import.

The first is more work and much better. It is listed first here because it decides whether standalone
writes go through the outbox *at all*, and retrofitting "those should have been queued" onto six months of
someone's data is not a migration worth writing.

### Smaller, and consequent

- **The outbox would otherwise grow forever with nothing to drain it.** `queue_health` would count up and
  the status line would say `N queued` permanently. There is deliberately no attempt cap and no
  dead-letter, and that stays right — so standalone mode either does not queue or settles entries locally.
  Falls out of the fork above.
- **`server` becomes `Option<ServerConfig>`.** It is a required, non-defaulted field today, so a stub
  config works only by writing a dummy URL — a lie in the file rather than an honest mode. Making it
  optional later is backward-compatible for existing configs, so **there is no deadline here and no reason
  to rush the config shape before GA.**
- **The sync surface goes honestly absent** rather than permanently failing: no `⟳` indicator saying "not
  synced yet" forever, and `r`/`R` are not keys that do nothing.

### Not checked

The survey read the main paths only. `saved_filters`, `project_views`, quick-add's `+project` resolution
and `CreateLabel`'s `GET /labels?s=` reconciliation were **not** examined for server assumptions. On this
project's record, driving it will find something reading it did not.

## Post-GA — the CLI as the agent surface

**Proposed 2026-09-05.** Add the verbs the CLI does not have — `tui-do ls --filter 'done = false' --json`
and `tui-do done <id>` — so that an agent on this fleet drives *tui-do* rather than driving Vikunja.

**The argument is that every caller should take the same path.** Today `add` is the only mutation the CLI
offers, so anything else an agent wants to do it must do against the Vikunja API directly — and that route
misses everything this project spent Phases 2 and 4 building. Going through the CLI instead puts an agent
through the **same optimistic write, the same outbox, the same `Task::merge_onto` conflict replay** the TUI
uses, and gives it the local store's instant reads rather than a round trip and 78 pages. An agent that
writes directly is a second client with none of that, against a server with no conditional write to ask
for — which is exactly the concurrency the "tui-do is multi-instance" note in `CLAUDE.md` is about.

**It is small, which is the other half of the case.** The query and mutation logic already exists in
`tui-do-core`; it simply has no CLI surface. `ls` is a `TaskQuery` and a serialiser, and `done` is the
`UpdateTask` the `d` key already queues. Neither needs a new idea, and neither touches `tui-do-ui`.

**The quick-add parser is the underrated piece.** An agent emitting

```sh
tui-do add "Chase the Telnyx DID *urgent !4 +Infra tomorrow"
```

instead of constructing Vikunja JSON is a real reduction in agent-side complexity — the label, the
priority, the project and the date are all resolved by a parser that already exists, is already tested, and
already handles the local-timezone rule that cria got wrong. Every one of those tokens is documented
quick-add syntax today. The agent does not need to know a project id, a label id, or that "unset" on the
wire is Go's zero time.

**So the conclusion is to widen the CLI rather than to route agents around it.** The alternative — agents
talking to Vikunja directly — is more work at every call site and gives up the outbox, the merge and the
cache to get there.

**Open before it is built**, none of them blocking:

- **What `--json` promises.** A stable shape is an API, and it is the first thing here that another program
  would depend on. Worth deciding whether it is versioned or explicitly unstable.
- **Whether `ls` pulls first.** Instant reads are the point, so probably not — but then a stale store
  answers, and an agent has no way to ask for freshness without a flag that costs 78 pages.
- **Which other verbs follow.** `done` is the obvious one; `rm`, `label` and `move` are the same shape and
  the same argument. Adding them piecemeal is fine; adding them all before deciding the `--json` contract is
  not.

## Post-GA — a Kanban board as the run surface for a multi-agent process

**Proposed 2026-09-05, and the continuation of the section above rather than a separate idea.** Take a
complex multi-agent process, write it into a board as tasks, give different agents different tasks, and
watch the cards move through the columns as the run happens. The board stops being a to-do list and becomes
the run's live status display.

**The observability half is free, and that is what makes this cheap.** Nothing has to be built to *watch* —
Vikunja's web UI already renders the board, on any screen, for anyone who wants to look, with no agent
dashboard to write and nothing to keep running. tui-do only needs the **write** side: a verb that moves a
task from one bucket to the next. Everything a human wants — where the run is, what is stuck, who holds
what — is then a page that already exists.

**The server supports it in one call**, checked against `spec/vikunja.json` rather than assumed:

```
POST /projects/{project}/views/{view}/buckets/{bucket}/tasks    # move a task into a bucket
GET|PUT /projects/{id}/views/{view}/buckets                     # list, create
```

**And more of the groundwork is in place than the Phase 6 entry suggests.** The `project_views` table
already stores every view with its kind and position; `ViewKind` already has the bucketed-board variant;
and `Task` already carries the bucket it sits in when fetched through a Kanban view. What is missing is
narrow: **`tui-do-api` has no bucket calls at all** — the word appears only in doc comments and a field —
so this is a client method, a `Mutation`, and a CLI verb, not a new subsystem.

**It should go through the outbox like every other write**, for the reason the section above gives: several
agents moving cards on one board at once is precisely the concurrent-writer case, and a move that bypassed
the queue would be a second client with no replay and no rollback.

**Open before it is built:**

- **A bucket is per view, not per project.** So a move has to name a view, and a project with a List view
  and a Kanban view has a task in a bucket in only one of them. The CLI shape has to make that unmissable
  or someone will move a card on a board nobody is looking at.
- **Agent identity.** "Assign different agents different tasks" wants each agent to be somebody —
  quick-add's `@user` already exists, but it resolves to Vikunja users, so either each agent gets an
  account or assignment is carried by a label instead. The first is more honest and costs more.
- **What a stuck run looks like.** A card that stops moving is the failure mode this design is meant to
  make visible, and nothing yet distinguishes "still working" from "the agent died holding it". A
  last-touched time or a heartbeat label is the smallest thing that would.
- **Whether tui-do needs to render the board at all.** Phase 6 plans one, but for this use the web UI is
  the display and tui-do is the actuator. Worth knowing that the write side alone delivers most of the
  value, and can ship first.

## Harvest manifest

**Port (rewrite, keep the behavior):**

| From `../cria` | Into | Note |
|---|---|---|
| `src/vikunja_parser.rs` (615) | `tui-do-core::quickadd` | Self-contained, no TUI coupling |
| `src/config.rs` (543) | `tui-do-core::config` | Best-written file in the repo |
| `COLUMN_LAYOUTS.md`, `QUICK_ACTIONS.md` | schema + docs | Config schemas worth preserving |
| `tests/` (5.4k LOC, 37 files) | spec + tests | `tests/app.rs` alone is 31 real behavioral tests |
| `src/url_utils.rs`, `src/color_helper.rs`, `src/terminal_capabilities.rs` | `tui-do-ui` | Small, focused, useful |

**Reference only:** `src/tui/ui/*` — read to learn *what* each screen shows, then rewrite.

**Discard entirely:** `src/ui_loop.rs`, `src/tui/app/state.rs`, `src/vikunja/client.rs` (dead),
`docs.json` (stale — 22 paths and 18 definitions behind), the `VikunjaTask`/`Task` dual model,
`src/tui/handlers.rs` + `src/tui/shortcuts.rs` (empty), `libterminal_capabilities.rlib`.

## Note on cria's licensing

`LICENSE` was **never committed** in cria's entire git history (`git log --all --diff-filter=A` finds
nothing), `Cargo.toml` has no `license` field, and `README.md:94` points at a file that doesn't exist. With
upstream unresponsive for months, no grant is going to materialize.

This is handled, not blocking: the two files we carry across are independently derivable — the parser
implements *Vikunja's own documented* quick-add syntax, and the config is a YAML schema documented in
cria's own markdown. Both get written from the spec and the tests rather than copy-pasted, which is what
the plan already calls for. tui-do ships `MIT OR Apache-2.0` with a README credit to cria as inspiration.

## Verification

- **Per-commit (hook + CI):** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
  on Linux (gating), plus the `ubuntu:26.04` container job.
- **Dev instance:** Phase 0.5 is done when `curl https://dev-box.example.net:8443/api/v1/info`
  returns v2.5.0 and the dev instance shows your imported projects and tasks.
- **API layer:** the spec-conformance test, plus integration tests against the **dev** instance gated on
  `TUI_DO_TEST_URL`. Phase 1 is done when we round-trip create → read → update → delete against dev, and the
  >50-task pagination regression test passes.
- **Sync engine:** unit tests over the outbox with a mocked API (`wiremock`) — verify optimistic write,
  server rejection, rollback, retry. Then live against dev with `reset-dev.sh` between runs.
- **TUI:** `update` is pure and sync, so drive it with `Msg` sequences and assert on `Model` — no terminal
  needed. Golden-file rendering tests via ratatui's `TestBackend`.
- **End-to-end:** use the **`/run`** skill to launch the TUI against **dev** at each milestone, on this
  Omarchy workstation. First light (Phase 3) must show real imported tasks. From Phase 3 onward, every
  milestone also gets a build-and-test pass in the `ubuntu:26.04` container before it counts as done.
- **Milestone gates:** `/code-review` after each phase; `/code-review ultra` before Phase 3 and Phase 6;
  `/security-review` after Phase 1 (token handling) and Phase 5 (URL opening).
- **Non-negotiable manual checks:**
  1. With the server unreachable, `tui-do` starts instantly, renders cached tasks, accepts edits into the
     outbox, and never freezes. That's the whole thesis of the rewrite.
  2. Pointing `tui-do` at the prod URL without `--i-know-this-is-prod` refuses to start.

## Skills to load

**Now:** `/init`, `update-config`, `fewer-permission-prompts`
**Per task:** `/code-review`, `/simplify`, `/security-review`, `/run`, `diagnose-crash`
**Optional:** `claude-in-chrome` — to study the Vikunja web UI as design reference
**To author:** a project skill wrapping `spec/vikunja.json`, and one for launching/screenshotting the TUI
