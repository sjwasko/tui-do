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

## How to drive these by hand

**The code snippets below are for whoever fixes each bug. They are not the test procedure.**
`md/MANUAL-CHECKS-BUGS.md` has the hand-driven version — what to press, what a pass looks
like, and what a fail looks like — in the same shape as `md/MANUAL-CHECKS2.md`.

One correction recorded there and worth repeating here, because it was found by driving it:
**editing a task, `Ctrl-S`, then `q` cannot expose BUG-3.** That path queues an `UpdateTask`,
and replaying an update just re-applies the same change to the same id — idempotent, nothing
to duplicate. BUG-3 needs a **create** in flight when you quit. A clean run of the edit
version proves the flush works, not that the bug is absent.

## Every open finding has a test case

Each entry below now carries a **Suggested test** written to *expose* the bug — deterministic
where the bug allows it, and honest about where it does not. Two rules were applied
throughout, both learned the hard way in this pass:

- **Never test a race by sampling it.** BUG-2 reorders 18.3% of the time, so a test that
  fires it and hopes is flaky, and worse, would pass for the wrong reason after a fix. Where
  timing is the mechanism, the test either controls the interleaving (wiremock delays) or
  asserts the invariant the fix establishes.
- **Check the test fails for the right reason.** BUG-1's proof failed twice in ways that
  looked like the bug and were not — once because an unmocked create 404s and rolls back
  correctly, once because the dangling selection and the fallback happened to land on the
  same row. Both are written up under BUG-1 as a warning.

| bug | its test is | deterministic? |
|---|---|---|
| ~~BUG-1~~ | FIXED — three tests, none ignored | yes |
| ~~BUG-3~~ | FIXED — reproduced by hand, guarded by an abort test | yes |
| BUG-4 | classifier table + one 408 integration test | yes |
| ~~BUG-6~~ | FIXED — two table tests, both directions | yes |
| BUG-7 | test the fix's reporting, not the panic | yes, after the fix |
| BUG-9 | grapheme table against `truncate`/`wrap` | yes |
| BUG-14 | store-level rollback sequence | yes |
| ~~BUG-15~~ | CONFIRMED by hand, FIXED, two non-vacuous tests | yes |

## What is left, at a glance

| | |
|---|---|
| **Decide, then fix** | BUG-4, BUG-7, BUG-9, BUG-16 |
| **Accepted, not fixing** | BUG-2 — window is sub-50 µs and the mutations that reach it commute |
| **Verify first** | BUG-14 |
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
| **fixed** | **1** | **7** | **2** |
| **open** | **0** | **7** | **13** |

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

## ~~BUG-1~~ — FIXED — Critical — the cursor lands on the wrong task after `a`

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

**Reproduced by hand 2026-08-31**: creating `cursor probe` with no due date and no priority
put the highlight on the top row, id 2071.

**Decision, 2026-08-31: the cursor follows the record just created, whatever detail it was
created with.** So the interface is *told* the id rather than guessing at the row — the
alternative, having the reload adopt the newest row, is a heuristic that would pick wrongly
the moment two creates land together or another client's task arrives in the same pull.

**Fixed.** `Store::queue` allocates the provisional id inside its own transaction and
returns the entry, so the id exists at exactly the moment it is assigned. The runtime now
sends `Msg::TaskCreated(id)` from that returned entry **before** `Msg::Reload`, and `update`
sets the selection from it. By the time `TasksLoaded` arrives the selected id names a real
row, so the "no selection, take the first row" fallback never fires.

The smoke harness mirrors that order, because it reimplements the runtime and `CLAUDE.md`
warns that getting the order wrong there makes tests pass against broken code.

All three tests in `crates/tui-do-smoke/tests/add_selection.rs` now pass and **none is
`#[ignore]`d**. Verified non-vacuous: removing just the `Msg::TaskCreated` half reproduces
the reported symptom exactly — *"the cursor landed on 'overdue, sorts first'"*.

**A test bug found on the way, worth recording.** The first run after the fix still failed,
and not because of the product: `mount_create` echoed a hardcoded title, and since the push
replaces the local row with the server's answer, the mock was renaming the task under the
cursor. The selection was right and the assertion was reading the wrong thing. A fixture
that echoes something other than what was sent is not a harmless simplification.

---

## Important

### BUG-2 — ACCEPTED, NOT FIXED — two rapid edits to one task can be applied out of order

`crates/tui-do/src/runtime/mod.rs:253` and `crates/tui-do-core/src/store/mod.rs:179`.

Each `Effect::Apply` is its own `tokio::spawn` around a `spawn_blocking` write that then
races for a `std::sync::Mutex`, which offers no fairness guarantee. Two edits to one task can
therefore take their `outbox.id`s in the wrong order, and the push replays the older `after`
last — reverting the newer edit on the server. This breaks the within-subject ordering
contract the engine assumes: `CLAUDE.md` states ordering is a contract *within* a task even
though it is not one between tasks.

**Measured 2026-08-31, because "there is a race" is not the same as "a user can hit it".**
Two `tokio::spawn`ed `store.queue()` calls, exactly as `perform` issues them, 300 trials per
gap:

| gap between the two spawns | reordered |
|---|---|
| **0 µs** — back to back, which is what the code does | **55/300 (18.3%)** |
| 50 µs | 0/300 |
| 100 µs, 250 µs, 500 µs, 1 ms, 2 ms, 5 ms, 10 ms, 50 ms | 0/300 at every step |

**The window is under 50 microseconds**, so two separately-timed keypresses can never reach
it — the fastest human gap is ~50 ms, a thousand times wider.

**What makes it reachable anyway is the event loop's shape.** `runtime/mod.rs:157-167`
drains every queued message, accumulates all their effects into one `Vec`, and only then
runs `for effect in effects { perform(...) }`. Two `Effect::Apply`s in one drain are spawned
back to back — the 0 µs row. Two key events land in one drain whenever they arrive within a
poll interval while the loop is busy (`INPUT_POLL` is 100 ms), or on key auto-repeat, or on
a paste.

**So: a true bug, narrow but not theoretical**, and 18.3% is far too high to dismiss. One
mitigation worth knowing: it is not entirely silent. The out-of-order second merge sees a
server value that differs from its own `before`, which is a real collision, so
`SyncEvent::Overwrote` toasts *"saved over a change made elsewhere"* — misleading, since
there was no elsewhere, but visible.

**How to test it.** Do **not** test the race; an 18%-failure test is flaky and would pass for
the wrong reason after a fix.

1. **Fix structurally, then assert the invariant.** A single serialized writer — one task
   consuming mutations FIFO — makes ordering not a race at all. The test then issues ~100
   `Effect::Apply`s as fast as possible and asserts `store.pending()` returns them in issue
   order. Deterministic, and it fails on any regression back to per-effect spawning.
2. **A consequence test, worth having regardless.** Queue two edits to one task deliberately
   out of order, push against a mock server, assert the server ends up holding the *older*
   value. Deterministic today, and it pins why ordering is load-bearing.
3. **Keep the sweep as an `#[ignore]`d probe** — it is the evidence for the table above and
   is how the window gets re-checked on other hardware.

**Decision, 2026-08-31: accepted as-is. Not fixed.** The reasoning is not "the window is
small so it probably will not happen" — it is sharper than that, and it came out of checking
whether a *single* keystroke can produce two `Apply`s.

It can. `apply_edit` (`update.rs:1720-1745`) emits `UpdateTask` **plus** one `AttachLabel`
per added label **plus** one `DetachLabel` per removed one, all for the same task subject,
all landing in one `Vec<Effect>` and performed back to back. **So the 0 µs path is not rare
at all — every multi-label edit takes it, and reorders 18.3% of the time.**

It is harmless there, because those mutations **commute**:

- `UpdateTask` writes the task's own fields, and labels are *not* replaced from a task body
  (measured, recorded above in this file).
- `AttachLabel` and `DetachLabel` go to their own endpoints.
- A label can never be in both the attach and the detach set, because the two are computed
  as a diff of one list.

The harmful pairing is **two `UpdateTask`s for one task**, and no single action emits two.
That requires two distinct user actions, which are at least ~50 ms apart — three orders of
magnitude outside the measured window.

**The residual**, stated so nobody thinks it is zero: key auto-repeat on `d` or `u` delivers
~25–35 ms apart, which is still far outside the window, but a loop stalled on a slow draw
could in principle batch two of them. If that ever happened the user would see the
`Overwrote` toast rather than nothing. Revisit if a "saved over a change made elsewhere"
report ever arrives that nobody can explain.

### ~~BUG-3~~ — FIXED — quitting mid-request can duplicate a task or label

`crates/tui-do/src/runtime/mod.rs:507` with `crates/tui-do-core/src/sync/mod.rs:646`.

`flush_on_exit` aborts the in-flight pass unconditionally. A `CreateTask`/`CreateLabel`
cancelled mid-request leaves its outbox entry intact with `attempts == 0`, so the immediate
`syncer.push()` re-sends it — and because the read-before-retry reconcile is gated on
`is_failing()`, which is false at zero attempts, it is skipped. Result: a duplicate on the
server. The same hole exists on SIGKILL.

**Confirmed by reading the chain, 2026-08-31.** `flush_on_exit` (`runtime/mod.rs:507`) calls
`handle.abort()` unconditionally before its own push. An aborted request never reaches
`defer()`, so `attempts` stays `0`; `is_failing()` is exactly `attempts > 0`
(`outbox.rs:454`); the read-before-retry reconcile is gated on it and is therefore skipped;
and the push that follows immediately re-sends the create.

**Its window is a full network round trip** — milliseconds to seconds — not BUG-2's 50 µs,
and the trigger is pressing `q` while a sync is in flight, which is ordinary. **This is the
most reachable of the open findings.**

**Suggested test — deterministic, no race.** In `crates/tui-do-smoke`, using wiremock's
controllable delay:

```rust
// PUT /projects/1/tasks answers 201, but only after 2s.
Mock::given(method("PUT")).and(path("/api/v1/projects/1/tasks"))
    .respond_with(ResponseTemplate::new(201)
        .set_delay(Duration::from_secs(2))
        .set_body_json(json!({"id": 77, "project_id": 1, "title": "t"})))
    .expect(1)                     // <-- the assertion: exactly one create reaches the server
    .mount(&server).await;

// queue a CreateTask, start a push, abort it mid-request the way quitting does,
// then run the push flush_on_exit performs.
let handle = tokio::spawn({ let s = sync.clone(); async move { s.push().await } });
tokio::time::sleep(Duration::from_millis(100)).await;   // request is in flight
handle.abort();
let _ = sync.push().await;                              // what flush_on_exit does next
```

The delay makes the interleaving deterministic rather than sampled. `.expect(1)` fails on
drop if the server saw two creates, which is the bug. The same shape with
`PUT /labels` plus a `GET /labels?s=` mock proves the reconcile was skipped.

**REPRODUCED BY HAND, 2026-08-31**, by pausing the dev container and quitting with a create
in flight: two tasks, ids **3889** and **3891**, both `Bug #3 - pause container test`.

**And it was worse than this entry said.** The report described the `is_failing()`-gated
reconcile being skipped. That is true for a label — but `Mutation::CreateTask` has **no
replay guard at all**:

```rust
Mutation::CreateTask { task } => Sent::Created(Box::new(
    self.client.create_task(task.project_id, task).await?,
)),
```

`CreateLabel` reads before it writes; `CreateTask` does not, and cannot easily — a label
title is at least a plausible key, and task titles are not unique in any useful sense
("Call the VA" twice is ordinary). So for a task, *any* replay duplicates, whatever
`attempts` says.

**Fixed by never producing the replay.** `flush_on_exit` aborted the running pass
unconditionally and then pushed, which is the two-overlapping-passes hazard that
`spawn_sync`'s own comment warns about — *"an entry sent twice is a task created twice"* —
reached by a path that bypasses the `RUNNING` guard.

`Sync` now carries a `pushing` flag held by an RAII guard, so an **aborted** push still
reports that it has stopped — `Drop` is the one thing that still runs when a task is
aborted, and a plain store-false at the end of `push_with` would never execute. At quit:

- mid-**pull** — aborted exactly as before; pages are applied, the retain has not run, the
  store is a little stale and the next pull fixes it;
- mid-**push** — given the same five-second grace the flush itself gets. If it finishes, its
  entries are settled and there is nothing left to duplicate. If it does not, tui-do says
  *"Still sending; leaving the rest queued for next time."* and walks away **without
  pushing**. A change that arrives late beats a task that exists twice.

**Fix confirmed by hand on 2026-08-31**, by re-driving the same paused-container procedure
that produced 3889 and 3891: one task, not two.

Guarded by `an_aborted_push_reports_that_it_is_no_longer_pushing` in
`crates/tui-do-core/tests/sync.rs`, verified non-vacuous: emptying the `Drop` body alone
fails it with *"the flag survived an abort"*.

**Still open, and narrower:** SIGKILL. Nothing runs on SIGKILL, so a create killed mid-flight
can still be re-sent on the next launch. Closing that needs the outbox to record "this entry
was handed to the server" durably — a schema change — and is worth doing only if it is ever
seen.

### BUG-4 — a 408 or 425 discards the user's edit

`crates/tui-do-core/src/sync/mod.rs:901`. `is_permanent` treats every 4xx except 401/403/429
as the server's final answer, so a proxy's **408 Request Timeout** (or 425 Too Early) rolls
back the edit and toasts "the server refused" when nothing was decided. 401 and 429 are
handled correctly.

**Suggested test — two levels, both deterministic.** A unit test on the classifier, which is
the cheap half and pins the decision once it is made:

```rust
for status in [408, 425, 429, 500, 503] {
    assert!(!is_permanent(&ApiError::from_status(status, &body, None)),
            "{status} is not the server's final answer");
}
for status in [400, 403, 404, 422] {
    assert!(is_permanent(&ApiError::from_status(status, &body, None)));
}
```

Then one integration test for the consequence: mock the update endpoint to answer `408`,
push, and assert the outbox entry is **still queued with `attempts == 1`** rather than rolled
back — that is, `store.pending()` is non-empty and the local row still shows the edit.

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

### ~~BUG-6~~ — FIXED — the production guard is a substring match

`crates/tui-do/src/main.rs:270-279`. `guard_production` matches the literal string
`"sw-hp2"`. A config naming production **by IP address** passes straight through with no
`--i-know-this-is-prod` required. Proved with the documentation-range address `192.0.2.55`;
the real production host was never contacted. The guard exists precisely so that "yes, I
meant it" is possible to say — and a stale config pointed at prod by IP would write to it.

**Fixed 2026-08-31, and it was wrong in *both* directions.** Matching the URL text rather
than its host missed `https://100.76.214.113:8443`, which is production, and would have
refused `https://dev.example.com/?note=sw-hp2`, which is not — and a guard that cries wolf
trains people to pass `--i-know-this-is-prod` by reflex, at which point it protects nothing.

`guard_production` now parses the URL with `url::Url` and tests the **host**: a name matches
when it is a production host or when its first label is one, so `sw-hp2` and
`sw-hp2.tail9803a5.ts.net` are caught while `sw-hp2-notreally.example.com` is not. Port,
path and query never take part, a trailing dot is stripped, and matching is
case-insensitive. Prod's addresses — tailnet and LAN — are listed alongside its name,
because a hostname is not the only way to name a machine. An unparseable URL falls back to
the old substring test rather than being waved through.

Two table tests pin both directions, verified non-vacuous: restoring the old one-line guard
fails them with *"`https://100.76.214.113:8443` was not recognised as production"* and
*"`https://dev.example.com/?note=sw-hp2` was wrongly treated as production"*. Confirmed
against the release binary, which now refuses.

**The same hole existed in a second place and is closed too.** `FORBIDDEN_HOSTS` in
`crates/tui-do-api/tests/live.rs` guards the live test suite the same way and checked only
the name. That one is arguably worse — a test suite pointed at production writes with nobody
watching.

**Suggested test — a pure table test, no network.** `guard_production` takes a URL and
returns a decision, so it is directly testable:

```rust
// Each of these is production and must be refused without --i-know-this-is-prod.
for url in [
    "https://sw-hp2.tail9803a5.ts.net:8443",
    "https://SW-HP2.tail9803a5.ts.net:8443",   // case
    "https://sw-hp2.tail9803a5.ts.net:8443/",  // trailing slash
    "https://sw-hp2.tail9803a5.ts.net",        // no port
    "https://100.x.y.z:8443",                  // the tailnet IP -- FAILS TODAY
] {
    assert!(guard_production(url).is_err(), "{url} was not recognised as production");
}
// And the dev server must still start without the flag.
assert!(guard_production("https://sw-surface.tail9803a5.ts.net:8443").is_ok());
```

The IP row is the one that fails now. Note the fix needs a decision first — whether "what
counts as production" is a hostname list, a resolved address, or an explicit config flag —
because a substring match cannot be made correct.

### BUG-7 — a panicking effect leaves the screen garbled instead of exiting

`crates/tui-do/src/runtime/terminal.rs:36-40`. The global panic hook restores the terminal
on any thread's panic, but tokio catches a spawned effect's panic at the task boundary and
the process keeps running — raw mode off, alternate screen gone, application still drawing
into a terminal that no longer expects it.

**Confirmed by experiment, 2026-08-31.** A throwaway reproducing the exact structure — a
global hook whose `restore()` is one-shot via `RAW.swap(false, ..)`, and a `tokio::spawn`
whose `JoinHandle` is dropped, as `perform` does:

```
1. spawning an effect that panics, exactly as perform() does
  [hook] restore() ran -- raw mode off, alt screen exited
2. process alive? true | hook ran? true | terminal already restored? true
3. the event loop would now keep drawing into a restored terminal.
4. Drop did nothing -- the one-shot was spent by the panic hook
```

So all four halves hold, including the one nobody had noticed: **the eventual clean exit's
`Drop` is a no-op**, because the panic hook already spent the one-shot.

**Reachability is the open question, and it is lower than the mechanism suggests.** Every
effect currently returns a `Result`, and the workspace denies `unwrap`/`panic` in production
code, so a panic has to come from inside a dependency or from arithmetic overflow in a debug
build. This is a "when something else goes wrong, it goes wrong badly" robustness problem
rather than something reachable from input.

**Suggested test — test the fix, not the panic.** The fix is for `perform` to stop discarding
`JoinHandle`s: join them, or wrap each effect so a panic becomes a message. Then:

```rust
// A test-only effect that panics on purpose.
perform(Effect::__PanicForTest, &store, None, &tx);
let msg = rx.recv().await.expect("a panicking effect must report something");
assert!(matches!(msg, Msg::EffectFailed(_)));
```

Plus a unit test that `restore()` is idempotent *and* that the guard can still restore after
the hook has run — the second half is what is broken today.

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

**Suggested test — pure, deterministic, no dependency needed to write it.** Only to fix it:

```rust
// A ZWJ family and a regional-indicator flag are each one grapheme, several chars.
let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";   // one glyph
let flag = "\u{1F1EC}\u{1F1E7}";                                // one glyph
for text in [format!("{family} tail"), format!("{flag} tail")] {
    for width in 1..=6 {
        let cut = truncate(&text, width);
        assert!(display_width(&cut) <= width);
        // The real assertion: never end part-way through a sequence.
        assert!(!cut.ends_with('\u{200D}'), "cut on a zero-width joiner: {cut:?}");
        assert!(!ends_with_lone_regional_indicator(&cut), "split a flag: {cut:?}");
    }
}
```

The same table against `wrap`. Fixing it means taking `unicode-segmentation` and iterating
graphemes rather than `char`s — which is the decision, since it is a new dependency.

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

**Suggested test — a store-level sequence test, fully deterministic.** No server needed for
the assertion that matters:

```rust
// 1. create a label -> provisional id -1, and attach it to a task
let create = store.queue(Mutation::CreateLabel { label: provisional }).await?;
store.queue(Mutation::AttachLabel { task: TaskId(1), label: provisional.clone() }).await?;
// 2. delete that task -- `before` carries the provisional label
let delete = store.queue(Mutation::DeleteTask { before: task_carrying_label }).await?;
// 3. the create is rejected: its rollback deletes label -1
store.roll_back(create.id).await?;
// 4. the delete is rejected too, or undone: its rollback re-writes `before`
store.roll_back(delete.id).await?;

// The assertion: no label row with a negative id may survive a rollback of its create.
let orphans = store.labels_with_negative_ids().await?;
assert!(orphans.is_empty(), "a provisional label was resurrected: {orphans:?}");
```

If `labels_with_negative_ids` does not exist, the same check is one `SELECT id FROM labels
WHERE id < 0` in a test helper. The row it finds will never settle and never sync, which is
what makes it a phantom rather than merely stale.

### ~~BUG-17~~ — FIXED — archiving the project you are viewing wedges the sidebar

`crates/tui-do-ui/src/update.rs:827` (movement) and `Msg::ProjectsLoaded` (the cause), with
`crates/tui-do-ui/src/sidebar.rs:98` (the filter).

**Found in use on 2026-08-31**, on the first day of driving it: five tasks added to
`Retirement` in the web UI, `R` in tui-do, navigate into the project, archive it in the web
UI, `R` again. The project left the sidebar — and then:

- the breadcrumb still read `Retirement › List`, naming a project that appeared nowhere;
- its five tasks were still listed, with no way back to them after navigating away;
- **the sidebar's arrow keys stopped working entirely**, with no way out but `g p` or a
  restart. Confirmed live: `g p` restored navigation.

**Two faults, one cause.** `sidebar::rows` filters archived projects out of the tree, but
nothing re-checked `sidebar.selected` after a pull. Movement then did:

```rust
let Some(current) = targets.iter().position(|t| *t == model.sidebar.selected) else {
    return Vec::new();          // <- every arrow key, forever
};
```

A selection naming a row that is no longer drawn returned without moving.

**Note this only became visible today.** Before the BUG-15 fix those five tasks were deleted
on the pull, so the stranded scope had nothing in it and the wedge was harder to notice.
Fixing the data loss surfaced the navigation bug behind it.

**Fixed in two places.** `settle_sidebar_selection` runs on every `ProjectsLoaded`: if the
selected project is no longer in the tree it falls back to All Tasks and toasts *"Retirement
was archived or removed elsewhere — showing all tasks"* — it happened on another machine and
nothing on screen would otherwise explain it. And movement now starts from the top rather
than returning, so the keyboard stays alive even if the model and the tree ever disagree
again.

Two tests, both verified non-vacuous: `a_project_archived_elsewhere_does_not_strand_the_sidebar`
drives the reported sequence, and `the_sidebar_arrows_never_dead_end` forces the wedged state
directly.

### BUG-16 — an error you must act on is truncated with no way to read the rest

`crates/tui-do-ui/src/view.rs:498` — `let text = rows::truncate(&toast.text, area.width);`

The status line is one row and a toast is cut to fit it. There is no wrapping, no scrollback
and no second surface, so a message longer than the terminal is simply unreadable.

**Found in use on 2026-08-31**, setting up a new machine. The toast was:

```
⚠ offline — not authorized: missing, malf…
```

The full text is Vikunja's own: *"not authorized: missing, malformed, expired or otherwise
invalid token provided"* — about a third of it visible. The cause turned out to be a token
copied from the production instance instead of dev, which is well-formed and simply unknown
to the other server. Nothing in the visible third could have led anybody there.

**Why it matters more than it looks.** Toasts carry the failures a user has to act on —
a rejected write, a credential that does not work, a queue that is stuck. Those are exactly
the messages that are long, and exactly the ones where the tail carries the diagnosis.

**Suggested fix.** Any of: wrap a toast onto a second row when it does not fit and the
terminal has room; keep the last error somewhere `?` or a key can show in full; or shorten
what tui-do prepends so more of the server's own words survive. The first is probably
right — the status line already knows the width, and an error is worth a row.

**Worth considering alongside it:** a `401` toast could name the server it was talking to.
*"not authorized against sw-surface.tail9803a5.ts.net"* would have made the wrong-instance
token obvious immediately, in fewer characters than the message it replaces.

**Suggested test.** Drive it: make the toast longer than the terminal is wide.

1. Set `server.token_file` to a file containing a token from a *different* Vikunja instance,
   or any 43-character string beginning `tk_`.
2. Start `tui-do` in an 80-column terminal and press `R`.

**Pass:** the whole message is readable, whether by wrapping or by a key that shows it.
**Fail:** it ends in `…` and the rest is unreachable.

### ~~BUG-15~~ — CONFIRMED AND FIXED — tasks in archived projects were deleted on every full pull

`crates/tui-do-core/src/sync/mod.rs:831-866`. **This is the one finding nobody could
settle, and it is the highest-value thing to check next**, because it is the same shape as a
bug that already destroyed data once.

The archived-projects fix covers `GET /projects` only. Nobody has measured whether an
unfiltered `GET /tasks` includes tasks belonging to an *archived* project. If it does not,
`retain_tasks` deletes them locally on every `Reach::Full` pull — silently, repeatedly.
`an_archived_project_and_its_tasks_survive_a_pull` mocks the server's answer rather than
observing it, so it proves the handling and not the premise.

**MEASURED 2026-08-31. The answer was yes, and it was real data loss.**

Driven by hand: a project archived in the web UI lost all four of its tasks from the local
store on the next `R`. The server still had them.

| | |
|---|---|
| server, `GET /projects/31/tasks` | **4 tasks** — 3880, 3881, 3882, 3883 |
| local store, project 31 | **0 tasks** |
| unfiltered `GET /tasks` | 3,879 tasks over 78 pages, **none of the four** |

So the listing that feeds `keep` is *incomplete* with respect to archived projects, and
`retain_tasks` read absence as deletion. Every full pull did it again, silently.

**This is the second half of a bug whose first half was already fixed.** Passing
`is_archived=true` to `GET /projects` made the archived *projects* survive a pull. Their
tasks went on being deleted underneath them, because the tasks listing has the same gap and
nobody checked it. The note in this file said the fix "covers `/projects` only" — it did,
and that was the bug.

**Fixed** by teaching both of `retain_tasks`'s delete paths that a task in an archived
project is never deleted here, whatever the listing said — the same reasoning that stops
`Reach::Incremental` retaining at all: a listing that *cannot* name a task is not evidence
the task is gone.

Two tests, both verified non-vacuous against the unguarded query:
`an_archived_projects_tasks_are_not_deleted_by_a_listing_that_omits_them` and
`the_same_holds_when_the_retain_names_projects`. The pre-existing
`an_archived_project_and_its_tasks_survive_a_pull` did not catch this because it **mocks**
the server's answer rather than observing it — it proved the handling and not the premise.

**Two things this does not do**, both smaller and both open:

- **It does not bring back what is already gone.** Nothing fetches an archived project's
  tasks — the global listing omits them and the pull does not ask per project — so a store
  that has lost them stays that way, and a fresh install never sees them at all. They are
  safe on the server. Fetching them needs a per-project pull for archived projects.
- **Editing a task in an archived project fails with `412`**, which is how this was noticed:
  *"update_task rejected: 412 This project is archived."* The server is right to refuse, and
  `is_permanent` correctly rolls the edit back — but the toast reads as a sync failure rather
  than "this project is archived", and the user loses what they typed with no warning that
  it could never have been saved.

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
