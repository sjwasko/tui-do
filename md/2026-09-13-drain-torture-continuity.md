# Continuity prompt — the drain torture test

**Paste the block below into a fresh session before executing
`md/drain-torture-test-plan.md`.** It exists because the plan assumes a dozen facts that
took a session to establish, and a session that re-derives them will get some of them
wrong — two were got wrong in the session that wrote this.

This is a **handoff for a task that is mid-flight**, which `CLAUDE.md`'s convention allows.
It is not a session continuity note of the kind deleted on 2026-08-30, and it should be
deleted once the test has been driven and its results are recorded in the plan.

---

## The prompt

> We are about to run a deliberate load test against the **dev** Vikunja
> (`sw-surface.tail9803a5.ts.net:8443`). Read `md/drain-torture-test-plan.md` and execute
> it. Read `CLAUDE.md` and `md/TODO.md` first — in particular item 10, which is what this
> test exists to measure.
>
> **The short version of why.** A bad Microsoft To Do import in May 2026 left 1,767 tasks
> sitting in a project called "Archive" with their completion flag lost — they are
> `done = 0` but semantically finished. Marking them done by holding the `d` key is a real
> thing the owner wants to do, and it happens to be the largest load tui-do's outbox has
> ever seen by roughly 350x. Rather than script around it, we are driving it and measuring
> everything we can.
>
> **Facts already established — do not re-derive these:**
>
> - Dev is **already seeded** with the prod data. `GET /projects/28/tasks` with
>   `filter=done = false` answers **1,767**, and `done = true` answers 64. Identical to
>   prod. Nothing needs importing.
> - Dev is `v2.5.0`, `max_items_per_page` 50, 3,879 tasks total, 33 projects.
> - The client box is **`x1-omarchy`** — that is where a human can physically hold a key
>   down. It is a *different machine* from `sw-x1`, despite the name; `sw-x1` is the
>   development workstation and where this repository lives.
> - `x1-omarchy`'s config points at dev but at the **demo account**, which holds synthetic
>   screenshot data, not the seeded pile. It must be repointed at the main dev account for
>   this test. `sw-x1:~/.config/tui-do/token` is that account's token.
> - `x1-omarchy` runs **tui-do 1.0.0**, three versions behind, installed as a real binary
>   rather than a symlink. Decide deliberately whether to test that or build current.
> - Each `d` queues one `UpdateTask`. Each `UpdateTask` is **two** HTTP requests, not one —
>   `client.task(id)` then `client.update_task` (`sync/mod.rs:713`), because the push
>   replays the edit onto the server's current copy. So 1,767 presses is **3,534 requests**.
> - The drain is a **serial loop that re-reads the entire pending list every iteration**
>   (`sync/mod.rs:459`). Whether that is O(n²) in practice at this depth is one of the
>   things being measured.
>
> **Guardrails:** dev only, never prod. `deploy/snapshot-dev.sh` before, so
> `deploy/reset-dev.sh` can put it back. Nothing in this test writes to `sw-hp2`.
