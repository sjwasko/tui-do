# bugs.md

Findings from a full-codebase review on **2026-08-31**, run by seven parallel reviewers
over all ~28,000 lines of production Rust, plus a live smoke test of the built binary.

Each finding names a file and line, a concrete failure scenario, and — where one was run —
the experiment that confirmed or refuted it.

**Nine were fixed on 2026-08-31** and are struck through below with their commit. They were
the ones needing no design decision: each matched a pattern already in the codebase. What
remains is deliberately the harder list — every open item is a real decision or needs
verification first.

After the nine fixes: **687 tests pass** (plus one `#[ignore]`d, which is BUG-1's proof), `cargo clippy --workspace --all-targets -- -D warnings`
exits 0, the release build succeeds, and **no invocation of the binary panicked** under any
malformed input the smoke test could construct.

## What is left, at a glance

| | |
|---|---|
| **Decide, then fix** | BUG-1 (Critical), BUG-2, BUG-3, BUG-4, BUG-6, BUG-7, BUG-9 |
| **Verify first** | BUG-15 (archived tasks — highest value), BUG-14 |
| **Structural** | `push_with`, `runtime::add`, `apply_edit` |
| **Minor** | 13 remaining, none urgent |

Every open item needs either a decision or a measurement. Nothing is left that is merely
mechanical — that was the point of the 2026-08-31 pass.

## Summary

| area | Critical | Important | Minor |
|---|---|---|---|
| UI state machine | **1** | 1 → 0 | 2 |
| Sync engine | 0 | 4 | 7 → 5 |
| Outbox and schema | 0 | 2 → 1 | 0 |
| Rendering | 0 | 3 → 2 | 2 |
| Runtime and CLI | 0 | 3 → 2 | 3 |
| API client | 0 | 1 → 0 | 1 |
| **found** | **1** | **14** | **15** |
| **fixed** | 0 | **5** | **2** |
| **open** | **1** | **9** | **13** |

Plus two of the four structural items, leaving three.

Two things came back clean that were expected to be the weak points, and both were checked
properly rather than assumed:

- **All seven `subject_id` SQL sites correctly filter on `subject_kind`** — verified
  crate-wide, not just in `outbox.rs`. There is no eighth unfiltered site. This is the
  invariant `CLAUDE.md` says has already been got wrong twice.
- **`Reach::Incremental` never runs the retain step.** The delete-safety separation holds on
  every path, `is_archived=true` is still passed, and all three `is_already_done` arms plus
  the deliberately-absent create arm are correct.

---

## BUG-1 — Critical — the cursor lands on the wrong task after `a`

**Where:** `crates/tui-do-ui/src/update.rs:2212-2219` (the `CreateTask` arm of
`apply_locally`), with `update.rs:65-68` (the selection fallback) and
`update.rs:1026-1038` (`build_task`).

**What happens.** `build_task` ends with `..Task::default()` and never sets an id, so the
optimistic row is built with `TaskId(0)`. `apply_locally` inserts that row and sets
`model.list.selected = Some(task.id)` — that is, `Some(TaskId(0))`. The store then assigns a
*negative* provisional id, and the `Msg::Reload` that follows loads the list back with that
real id. `TaskId(0)` is now in no row, `selected_index()` returns `None`, and
`update.rs:66` silently falls back to `model.data.tasks.first()`.

The comment above the line says the selection "follows it there, which is what tracking the
selection by id is for". The mechanism is right; the id being tracked is the wrong one.

**Failure scenario.** Press `a`, type a task with no due date, press Enter. The cursor is now
on whatever task sorts first — not the one you just made. The next single keystroke acts on
that task: **`x` deletes it, `d` marks it done, `p` re-prioritises it.**

**Confirmed by experiment**, and the first two attempts at that experiment were wrong in
ways worth recording:

1. With no mock mounted, the create 404s, `is_permanent` correctly rolls it back, and the
   test fails for the *right* reason — a false positive.
2. With a create mounted but the new task sorting *first*, both tests pass — because the
   "no selection, take the first row" fallback lands on the same row as the correct answer.
   The bug is invisible.
3. With a create mounted **and** the new task not sorting first, it fails:
   `the cursor landed on "overdue, sorts first"`.

`crates/tui-do-smoke/tests/add_selection.rs` holds all three. The first two pass and are
kept as regression tests; the third is `#[ignore]`d with a pointer here and is run with
`cargo test -p tui-do-smoke -- --ignored`. **Delete the `#[ignore]` when this is fixed.**

**Why no existing test caught it.** The one `tui-do-ui` test covering this state seeds the
*real* provisional id by hand instead of driving `add_task` through the reload path. The id
the interface selects and the id the store assigns are agreed nowhere except in the smoke
crate, which is exactly what that crate exists for.

**Note on the fix.** There is no obviously correct one-liner. The UI cannot know the
provisional id — it comes from the store's own counter — so either `Effect::Apply` must
answer with the assigned id, or the reload must be told to adopt the newest row. That is a
design decision, which is why this is filed rather than patched.

---

## Important

### BUG-2 — two rapid edits to one task can be applied out of order

`crates/tui-do/src/runtime/mod.rs:253` and `crates/tui-do-core/src/store/mod.rs:179`.

Each `Effect::Apply` is its own `tokio::spawn` around a `spawn_blocking` write, so two edits
to the same task can take their `outbox.id`s in the wrong order. The push then replays the
older `after` last and **silently reverts the newer edit on the server**. This breaks the
within-subject ordering contract the engine explicitly assumes — `CLAUDE.md` states that
ordering is a contract *within* a task even though it is not one between tasks.

### BUG-3 — quitting mid-request can duplicate a task or label

`crates/tui-do/src/runtime/mod.rs:507` with `crates/tui-do-core/src/sync/mod.rs:646`.

`flush_on_exit` aborts the in-flight pass unconditionally. A `CreateTask`/`CreateLabel`
cancelled mid-request leaves its outbox entry intact with `attempts == 0`, so the immediate
`syncer.push()` re-sends it — and because the read-before-retry reconcile is gated on
`is_failing()`, which is false at zero attempts, it is skipped. Result: a duplicate on the
server. The same hole exists on SIGKILL.

### BUG-4 — a 408 or 425 discards the user's edit

`crates/tui-do-core/src/sync/mod.rs:901`. `is_permanent` treats every 4xx except 401/403/429
as the server's final answer, so a proxy's **408 Request Timeout** (or 425 Too Early) rolls
back the edit and toasts "the server refused" when nothing was decided. 401 and 429 are
handled correctly.

### ~~BUG-5~~ — FIXED — a server can silence retries indefinitely

`crates/tui-do-core/src/store/outbox.rs:855-882` with
`crates/tui-do-api/src/client.rs:1179-1209`. `Store::defer` honours a `Retry-After` or
`x-ratelimit-reset` **verbatim with no ceiling**, unlike the computed exponential backoff,
which is capped at 15 minutes. A misbehaving server or proxy sending an oversized value
pushes `next_attempt_at` arbitrarily far out. Not unrecoverable — `r`/`R` force a retry via
`Backoff::Ignore` — but automatic retry stops silently.

**Fixed.** `Store::defer` now clamps the server's value with `.min(BACKOFF_CEILING)`, the
same 15-minute ceiling the computed backoff already used. Guarded by
`a_wild_retry_after_cannot_silence_an_entry`, verified non-vacuous — with the clamp removed
it reports `PT2591999.99S`, the full 30 days, and fails.

### BUG-6 — the production guard is a substring match

`crates/tui-do/src/main.rs:270-279`. `guard_production` matches the literal string
`"prod-box"`. A config naming production **by IP address** passes straight through with no
`--i-know-this-is-prod` required. Proved with the documentation-range address `192.0.2.55`;
the real production host was never contacted. The guard exists precisely so that "yes, I
meant it" is possible to say — and a stale config pointed at prod by IP would write to it.

### BUG-7 — a panicking effect leaves the screen garbled instead of exiting

`crates/tui-do/src/runtime/terminal.rs:36-40`. The global panic hook restores the terminal
on any thread's panic, but tokio catches a spawned effect's panic at the task boundary and
the process keeps running — raw mode off, alternate screen gone, application still drawing
into a terminal that no longer expects it.

### ~~BUG-8~~ — FIXED — `tui-do add --offline` swallows a real configuration error

`crates/tui-do/src/runtime/mod.rs:743, 852-855`. `--offline` discards the `build_sync`
diagnostic, so an unreadable `token_file`, or one pointing at a directory, prints nothing —
while the same config without `--offline` prints a clear error. The "the next run will send
it" reassurance is then false, because the next run cannot authenticate either.

**Fixed.** `--offline` now reports the diagnostic instead of discarding it:
`Queued, but not syncing later either: {problem}`. `--offline` means "do not send it now",
not "do not tell me sending is broken".

### BUG-9 — `truncate` and `wrap` split graphemes

`crates/tui-do-ui/src/rows.rs:472-494, 540-556`. Both operate per-`char`, not per-grapheme
(there is no `unicode-segmentation` dependency), so a flag or ZWJ emoji sequence can be cut
mid-glyph and the cell corrupted. No panic — `unicode-width` keeps the arithmetic sound —
but the display is wrong.

### ~~BUG-10~~ — FIXED — a percent-width column can collapse to width 1

`crates/tui-do-ui/src/rows.rs:81-88`. `measure()`'s percentage path has no floor. A config
producing a wrap-enabled column of width 1, combined with unspaced CJK text, makes `wrap()`'s
long-word-break loop emit blank rows and drop the title entirely, leaving only `…`.

**Fixed.** The percentage path now floors at `natural_min(spec.column)`, which the
non-percent path already used as its default — so the two agree rather than leaving the
percentage to round itself away to nothing.

### BUG-11 — the edit form can focus a field that is off-screen

`crates/tui-do-ui/src/view.rs:1211-1289`. `edit_body` has no scroll or windowing logic,
unlike every other multi-row modal. On a short terminal, `Tab` moves focus to a field that
has been clipped away, and the user types into it with no visual feedback at all — caret
placement is silently skipped when `y >= area.bottom()`.

### ~~BUG-12~~ — FIXED — the local-day rule is broken in the backdate check

`crates/tui-do-ui/src/update.rs:1694`. `apply_edit`'s day-changed check calls `.date_naive()`
directly on the UTC due-date instant instead of converting into `model.now.timezone()` first.
`rows::relative_date` does convert, which is the correct pattern. At positive UTC offsets a
genuine local-day backdate can look unchanged in UTC, suppressing the "that date has passed"
warning. This is a narrower instance of the exact bug `CLAUDE.md` says the predecessor had.

**Fixed.** The `day` closure now converts with `.with_timezone(&model.now.timezone())`
before `.date_naive()`, matching `rows::relative_date`.

### ~~BUG-13~~ — PARTLY FIXED — a dead error classifier disagrees with the live one

`crates/tui-do-api/src/error.rs:193`. `ApiError::is_retryable()` is dead code whose doc
comment claims the sync engine uses it. The engine actually reimplements the classification
as `is_permanent()` (`sync/mod.rs:901`), and the two **disagree on 401** — and on 429 and
`Deserialize`. Harmless today because nothing calls it; a landmine for whoever refactors
next and reasonably assumes the shared-looking helper is the shared one.

**Partly fixed, deliberately.** The doc comment was the landmine — it claimed the sync
engine used this — so it now states plainly that it does not, names `sync::is_permanent` as
the real decision, spells out that the two disagree on 401 and `Deserialize`, and points at
BUG-4. **Collapsing them into one is not done**, because which classification is correct
*is* BUG-4, and that is an open decision.

### BUG-14 — a rolled-back `DeleteTask` can resurrect a phantom label row

`crates/tui-do-core/src/sync/mod.rs:938-953` with `outbox.rs:401-402`. `references()`
deliberately excludes `DeleteTask` from a failed `CreateLabel`'s blast radius — correct for
the wire, since a delete sends no body — but a queued `DeleteTask.before.labels` can still
carry the provisional label id. If that delete is later undone or rejected, its rollback
calls `upsert_task(before)` and recreates a `labels` row at a stale negative id that the
`CreateLabel` rollback already deleted. The result never settles and never syncs. The code's
own comment flags this as a known gap; the reviewer confirmed a concrete reachable sequence
and that no test covers it.

### BUG-15 — unverified: are tasks in archived projects deleted on every full pull?

`crates/tui-do-core/src/sync/mod.rs:831-866`. **This is the one finding nobody could
settle, and it is the highest-value thing to check next**, because it is the same shape as a
bug that already destroyed data once.

The archived-projects fix covers `GET /projects` only. Nobody has measured whether an
unfiltered `GET /tasks` includes tasks belonging to an *archived* project. If it does not,
`retain_tasks` deletes them locally on every `Reach::Full` pull — silently, repeatedly.
`an_archived_project_and_its_tasks_survive_a_pull` mocks the server's answer rather than
observing it, so it proves the handling and not the premise.

**The experiment**, against dev only:

```sh
# 1. baseline: confirm the project's tasks appear in the unfiltered listing
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/projects/10/tasks?per_page=50"
# walk every page of $BASE/tasks and record the ids

# 2. archive the project
curl -s -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"id":10,"title":"Trust","is_archived":true}' "$BASE/projects/10"

# 3. walk every page of $BASE/tasks again and look for those same ids
# 4. un-archive, restoring the dev state
```

I ran step 1 on 2026-08-31 — dev has 3,878 tasks over 78 pages, and project 10 ("Trust", 4
tasks: 354, 355, 356, 357) is the smallest useful subject. **Steps 2 onward were blocked by
a permission classifier** and were not run, so the question is still open.

---

## Minor

- `crates/tui-do-api/src/models/task.rs:296` — `Task::repeats()` may over-report for
  `RepeatMode::FromCurrentDate` with `repeat_after == 0`. Cosmetic badge only, unverified.
- `crates/tui-do-ui/src/theme.rs:248` — `Theme::label` paints a background, the one exception
  to the theme's documented "foregrounds and selections only" rule. Probably deliberate,
  worth confirming and commenting.
- `draw_sidebar` rebuilds the whole project tree every frame before windowing, where the task
  list windows first. Asymmetric; low impact at realistic project counts.
- `crates/tui-do-ui/src/update.rs` — `rows::fit` is computed on every action regardless of
  relevance. Bounded, so wasted work rather than a scaling problem.
- A rejected write drops undo/redo history for *every* prior entry on that task, not only the
  failed one. Looks like a documented deliberate trade rather than an oversight.
- The 120-second clock-skew margin on the incremental watermark is a guess, where
  `GET /info`'s `Date` header would give the real offset for free. Bounded by the timer's
  periodic Full pass.
- ~~`LAST_RECONCILE` is written and never read.~~ **Fixed** — kept (the distinction it
  carries is real and documented in `CLAUDE.md`) but its doc comment now says plainly that
  nothing in production reads it and what it is for.
- ~~`Store::pending`'s comment claims an SQL backoff filter that is not in the query.~~
  **Fixed** — and the comment was worse than stale: implementing what it described would
  **break** `r`/`R`, which pass `Backoff::Ignore` precisely to retry entries still inside
  their backoff. The comment now says the query is deliberately unfiltered and why.
- `RUNNING` is never cleared on panic or abort.
- A two-atomic seam in the sync trigger can drop an `Asked` force.
- A 4xx from `labels_named` rejects the create it was there to protect.
- The concurrent projects fan-out feeds a cascading retain.
- `crates/tui-do-api/src/secret.rs:118` carries an `#[allow(dead_code)]` in production code.
- `enum Screen` is vestigial: one variant, no use outside `model.rs`. Scope actually lives in
  `query.scope`. Either delete it or give it the job its name implies.
- "Which modals hold a label" is an unnamed concept re-derived across five exhaustive
  `match modal` sites in `update.rs`.

---

## Structure: where the code is hard to troubleshoot

The question asked was whether tui-do has drifted back toward what it was built to replace —
the predecessor's 790-line `run_ui` with a 46-branch `if app.show_X` chain over a ~100-field
god struct.

**It has not.** `Model` is 21 fields grouped into six sub-structs with three independently
meaningful bools and no bool/`Option` pairings. `Modal` is an 11-variant enum behind a
`ModalView` trait dispatched through a 13-line `as_view_mut`. `ConfirmLabelsState` uses an
explicit `enum Waiting` rather than a bool, citing rule 2 by name. **The state shape is
better than the rule requires.**

Twelve production functions exceed 100 lines. Length alone is not the finding — a flat
`match` over 30 delegating arms is long and trivial to follow, and splitting it would hurt.
Four are genuine problems:

### 1. `sync::push_with` — `sync/mod.rs:380`, 134 lines — the worst debugging surface

Two jobs interleaved: **eligibility selection** (four independent exclusion criteria, plus
the side effect that a not-due entry inserts itself into two of those same sets while being
skipped) and **outcome settlement** (three arms, one with a cross-subject cascade discard).

This matters more than its size. The selection predicate is the single most-corrected piece
of logic in the project — `CLAUDE.md` documents it being wrong **twice**, and both were found
live rather than by a test, because the predicate has no name, no signature and no test of
its own. It exists only as a `continue`-chain inside a loop that also performs I/O. You
cannot unit-test "is this entry eligible" today without a store and a server.

**Suggested split** — the candidate loop needs no `&self` and touches no I/O:

```rust
enum Eligible { Send(OutboxEntry), Blocks(Subject), None }
fn next_eligible(
    pending: &[OutboxEntry],
    attempted: &HashSet<i64>,
    blocked: &HashSet<Subject>,
    blocked_labels: &HashSet<LabelId>,
    backoff: Backoff,
    now: DateTime<Utc>,
) -> Eligible
```

`push_with` then reads: fetch pending, ask `next_eligible`, record what it blocks, deliver,
settle — and the predicate becomes testable against a hand-built `Vec<OutboxEntry>`, which is
exactly the shape of both historical bugs.

### 2. `runtime::add` — `runtime/mod.rs:726`, 148 lines

Five jobs, of which roughly 100 lines are error-prose construction and a six-branch console
report. Linear, so defensible in kind, but "the CLI printed the wrong thing" and "the CLI
queued the wrong thing" are debugged in one body with no seam. Extract
`resolve_or_explain(...) -> Result<Built>` and `report(...)`; `add` drops to ~50 lines.

### 3. `update::apply_edit` — `update.rs:1598`, 142 lines

Parse, validate, diff labels, compute a backdate warning, queue up to three mutations,
compose the toast. It is a pipeline and the `notes` accumulator is a deliberate design, but
it is the only function in the UI crate doing parsing *and* validation *and* mutation-building
in one body — and its second `resolve_labels` call needs a nine-line comment explaining why
it is not a duplicate. Extract `draft_into_task(draft, model) -> (Task, Vec<String>)`.

### ~~4.~~ DONE — `update::on_sync` (`update.rs:164`) and `update::update` (`update.rs:32`)

Both were clean flat dispatches spoiled by one obese arm each: `SyncEvent::Rejected` was ~60
lines doing four jobs, and `Msg::LabelsLoaded` was ~78 of `update`'s 127.

**Done.** Hoisted verbatim — no logic changed, every comment carried across — into
`on_rejected` and `absorb_labels`. Both callers are now one line, and the measurements moved:

| | before | after |
|---|---|---|
| `fn update` | 127 | **70** |
| `fn on_sync` | 134 | **73** |

Neither exceeds 100 lines any more, so the count of over-100-line functions is 12 → 10.

### Explicitly fine, do not "fix"

**`fn act` at 397 lines is not the pattern rule 2 warns about.** It is 38 exhaustive,
mutually independent arms with compiler-enforced coverage and 56 lines of comment — the
structural opposite of an unchecked `if app.show_X` chain. Splitting it would add indirection
and buy no seam. The same verdict applies to `apply_locally`, `perform`, `transmit`, `cell`,
`upsert_task` and `into_migration`: long because the domain genuinely has that many cases.

`update.rs` at 2,543 lines is navigationally large but internally well decomposed — 64
functions averaging ~40 lines.

---

## Already known, recorded elsewhere

`md/MANUAL-CHECKS2.md` has a **"What is known to be wrong, and is not being fixed yet"**
section listing seven deliberate issues found on 2026-08-29 and judged at the time — the
`Esc`-then-`l` duplicate-create window, ASCII-only case folding, a retry adopting another
box's label, `C-n` being unable to type a space, and others. Those are not repeated here.
Note that **BUG-14 above is the confirmed, reachable form of one of them.**

## A correction to this document's own method

The first pass at measuring function length over-counted `client.rs:947 fn resolve` as 198
lines; it is 41 and does one job. A reviewer caught it independently. The other twelve
measurements were correct. Anything in this file derived from a script rather than from
reading is worth re-checking before acting on it.
