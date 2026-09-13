# What 1,767 writes through the outbox proved — and the three defects they found

**Driven 2026-09-13 against dev, from `x1-omarchy` running v1.0.2.** The full procedure,
the raw numbers and the predictions-as-written live in `md/drain-torture-test-plan.md`,
which is untracked and local. This document is the part worth keeping in the repository:
what the exercise established, what it cost, and what it changes.

## Why it was worth doing

A Microsoft To Do import in May 2026 lost its completion flags, leaving **1,767 tasks in a
project called `Archive`** marked `done = 0` but semantically finished years ago. Marking
them done by holding `d` is a thing the owner actually wanted to do, and it happens to be
**the largest load tui-do's write path has ever seen, by roughly 350x**. The outbox was
designed for a person making a few edits.

Rather than script around it, it was driven and instrumented. Dev already held the same
pile, so nothing needed importing.

## What held

**Rule 1 held, and this is the strongest evidence it has.** 1,767 writes and 1,767
reloads, and the interface stayed responsive throughout — every keystroke instantaneous,
no stall, motion keys and the help modal answering during the drain. The render loop
never blocking had never been tested above a handful.

**Correctness was perfect across both runs.** 2,120 writes total, every request answered
`200`, zero lost, zero duplicated:

| | |
|---|---|
| run 1 | 1,767 distinct tasks; server ends 0 open / 1,831 done |
| run 2 | 360 presses; server ends 1,407 open / 424 done, matching the local store exactly |

**`flush_on_exit` passes.** Quitting with 353 entries queued left them queued rather than
re-pushing, and they drained on the next launch as **353 POSTs for 353 distinct ids, zero
duplicates**. Re-pushing would risk a duplicate task — `CreateTask` has no
read-before-retry — so declining is right.

## What it measured

**The drain is O(n²), and now it is a number rather than a worry.** Fitting per-entry cost
against queue depth:

```
  ms_per_entry = 0.0185 x depth + 133.8
```

- **133.8 ms fixed** — the two HTTP requests each `UpdateTask` costs, ~67 ms apiece.
- **18.5 µs per queued row, per iteration** — `store.pending()` being re-read every time
  round the drain loop.
- At the peak depth of 1,781 that added 33 ms/entry, and integrated it cost **~29 s of the
  268 s run, about 11 %**.

**So the candidate-selection split is worth doing and is not urgent.** It only becomes
dominant around 10,000 queued entries, where the re-read would exceed the network cost.
Confidence is an order of magnitude, not three significant figures: nine fitting windows,
one clear outlier.

**A full pull costs 25.0 s, not the ~15 s the backlog assumed** — 78 pages, 83 requests,
3,879 tasks, measured cold. `md/TODO.md`'s lease cost argument was corrected: four
processes spend 100 s of every 300 s window pulling, not 60.

## The three defects

All three were found by driving load, none by reading the code, and all three are
mechanical rather than decisions awaiting an answer. Full write-ups in `bugs.md`.

**BUG-21 — the queued counter is stale for the whole of a long drain.** `SyncEvent::Pushed`
fires once when a push pass *ends*, and the drain loop emits nothing on success. The count
climbed correctly to 1,781 while the key was held, then sat frozen there for the entire
268-second drain. Cosmetic, but a frozen counter during a long operation reads as a hang,
and the honest response to a hang is to kill the process — which is the one action that
would actually cost work.

**BUG-22 — a reload racing a write re-shows an already-queued task.** `apply_locally`
removes a done task from a filtered list synchronously so the cursor advances; a reload
issued by an earlier task's write can read the store *before* this task's write commits and
put the row back under the cursor, where a held key presses it again. **17 of 1,767 tasks
(0.96 %)** were written twice, clustered in ids 2082–2118 — one window, not a steady rate.
Harmless here only because both writes set `done = true`. **`d` is a toggle**, and the
other resolution of the same race silently un-does the user's work.

**BUG-23 — the UI's own writes starve the push.** The most consequential find. Holding the
key produced a **fifteen-second window with no requests at all**, then four in 350 ms once
it was released: roughly **7 % of the unloaded rate**. Run 1 shows the same shape in
hindsight — its 6.3 entries/s was entirely post-release, and essentially nothing drained
during the 74-second hold.

## What BUG-23 does to BUG-2's acceptance

BUG-2 — two rapid edits to one task applied out of order — is **accepted and not fixed**,
on the grounds that its window is sub-50 µs and, in its own words, *"the whole acceptance
rests on a human being slow."*

The unfairness it exploits is one `Arc<Mutex<Connection>>` with no fairness guarantee. That
same unfairness turns out to have a second and much larger consequence, **which a human
reaches today** with no agent and no MCP server involved: a held key stops the outbox
draining.

**The mutex is now implicated in two defects rather than one.** That is a stronger case for
fixing it structurally than either makes alone, and it should be read before BUG-2 is
dismissed again. It also sharpens `md/TODO.md` item 10(a), which already argues the MCP
crate must write sequentially by construction.

## What is still unexplained

**Why the five-second exit grace was idle.** `flush_on_exit` gives an in-flight push five
seconds and then prints *"Still sending; leaving the rest queued for next time."* — but
**zero requests went out during that window**, so the message described a push that was not
sending. By then the key had been released, so the mutex contention that explains the
fifteen-second gap does not explain this.

The last line in that process's life is a POST at `23:15:38.628919`, with nothing after it.
What the push was blocked on is not established, and the answer is probably *not* in the
logs — because **there is no store-layer tracing at all**. That absence was the plan's
biggest named blind spot before the run, and it is the one that bit.

**This makes the twenty lines of store instrumentation worth taking**: a span around
`Store::write` recording elapsed time, and a counter of drain iterations with the size of
`pending` on each.

## Raw data, deliberately retained

Kept in two places — `x1-omarchy:/home/swasko/drain-test/` and
`sw-x1:/home/swasko/drain-test-archive/` — against the plan's own instruction to delete it,
because BUG-23 is unexplained and these logs are the only evidence of it. Attributes and
what each file proves are tabulated in `md/drain-torture-test-plan.md`.

**One flaw in the instrument, worth knowing before any repeat.** The launcher deletes the
trace log on every start, which is right within a run and wrong across runs: relaunching to
drain the queue **destroyed the run-2 pre-quit trace on the host** — the single most
valuable file of the exercise — and it survived only because it had been copied off the box
during analysis. Stamp the filename with the start time instead.

**And the plan's own tracing directive was wrong.** `TUI_DO_LOG=tui_do=trace` produces a log
with **no requests in it**: the per-request trace is emitted from `tui_do_api`, and
`EnvFilter` matches on module-path segments, so `tui_do_api` is not a child of `tui_do`.
All three crate targets are needed.

## What was not reached

**Item 10(b) did not trigger.** No `SQLITE_BUSY` at any point, from tui-do or from a second
process reading the store once a second throughout both runs. That is **not** evidence the
hazard is absent — the sampler only ever read, and `SQLITE_BUSY_SNAPSHOT` needs two
*writers*. The deferred-transaction change and its deterministic two-connection test are
unaffected and still owed.

## The decision this was evidence for

Prod still holds its own 1,767. The test says doing it there is safe — about five minutes,
nothing lost, and **the counter will lie for all of it**. That is a decision to take
deliberately, not a footnote to this exercise.
