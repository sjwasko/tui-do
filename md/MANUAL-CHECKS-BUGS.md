# Manual checks for the open findings in `bugs.md`

Written 2026-08-31, replacing the Rust snippets in `bugs.md`, which were the wrong artefact:
they described tests for someone writing code, and these are driven by hand.

Same shape as `md/MANUAL-CHECKS2.md` — each check says what to do, what a **pass** looks
like, and what a **fail** looks like. The fail line is the important half: several of these
bugs are invisible unless you know precisely what to look at.

**Drive all of these against dev.** `deploy/reset-dev.sh` puts it back afterwards.

---

## G1 — BUG-3: quitting while a change is still in flight

**Driven 2026-08-31: it failed, then the fix was made, then it passed.** The failing run
produced tasks 3889 and 3891, both `Bug #3 - pause container test`. Keep this check — it is
now the regression test for that fix, and it is the only one that exercises the quit path
against a server that will not answer.

### G1a — why the obvious version of this test cannot fail

Editing a task, `Ctrl-S`, then `q` **cannot** expose BUG-3, and it is worth saying why so
nobody concludes from a clean run that the bug is not there.

BUG-3's damage is a **duplicate**, and only a *create* can duplicate. Editing an existing
task queues an `UpdateTask`, and re-sending an update simply re-applies the same change to
the same id — it is idempotent, and there is nothing for a replay to duplicate. Driving that
path tests `flush_on_exit`'s happy path, which is worth having, but it is check G3 below and
not this one.

**What you need instead is a task or a label that does not exist on the server yet**, and a
quit while its create is in flight.

### G1b — the check that can fail

The window is one network round trip, so the trick is to make the round trip slow.

1. `deploy/reset-dev.sh`, then start `tui-do`.
2. Make the server slow to answer. Easiest without touching code: on the dev host, pause the
   Vikunja container mid-test — `docker pause tui-do-vikunja` — or pull the tailnet route.
3. In tui-do, press `a`, type `duplicate probe one`, Enter. The task appears immediately;
   its create is now queued and the push is in flight against a server that will not answer.
4. Press `q` **while it is still trying**. You should see `Sending 1 queued change` and
   dots.
5. Unpause the server (`docker unpause tui-do-vikunja`).
6. Start `tui-do` again, press `R` for a full sync.

**Pass:** exactly one task called `duplicate probe one`.

**Fail:** two. The first went out before the quit aborted the pass; the abort left the outbox
entry looking untried, so the flush sent it again.

**Repeat with a label**, which is the case the code has a guard for and the guard is what
BUG-3 says gets skipped: press `l`, `Ctrl-N`, type `duplicateprobe`, and do the same. Two
labels of the same name is the fail.

**Faster variant if pausing the container is awkward:** point tui-do's config at a URL that
routes but never answers (a firewalled port), do the same steps, then point it back. The
create is queued either way.

---

## G2 — BUG-6: the production guard does not recognise prod by IP

**No writes are involved. Nothing is sent. This check is safe.**

1. Copy your config to `/tmp/prod-by-name.yaml` and set `server.url` to the production
   hostname, `https://prod-box.example.net:8443`.
2. `TUI_DO_CONFIG=/tmp/prod-by-name.yaml tui-do`
   **Pass:** it refuses to start and tells you to pass `--i-know-this-is-prod`.
3. Now find prod's tailnet IP — `tailscale status | grep prod-box` — and copy the config to
   `/tmp/prod-by-ip.yaml` with `server.url` set to `https://<that IP>:8443`.
4. `TUI_DO_CONFIG=/tmp/prod-by-ip.yaml tui-do`

**Fixed 2026-08-31.** Both steps must now refuse. Before the fix, step 4 started normally —
same server, same data, no guard — because the check matched the literal text `prod-box` and
an IP does not contain it.

**Fail:** step 4 starts. That means the address is no longer in the guard's list, which is
what happens if prod is ever renumbered.

**Quit immediately with `q`.** Starting is enough to prove it; do not press anything else.

Also worth trying, all of which should refuse: the hostname in capitals, with a trailing
slash, and without the `:8443` port.

---

## G3 — a big description survives a save and a quit

Not a bug — a regression check, and it is the one you invented on 2026-08-31 by pasting
1,907 lines into a task. It is worth keeping because it exercises three things at once.

1. Copy a large block of text — a few hundred lines is plenty.
2. In tui-do, `e` on a task, paste into the description, `Ctrl-S`, then `q` straight away.
3. Start again, `R`, and open the task.

**Pass:** every line is there, the first and last included, and the preview pane renders it.

**Fail:** truncation at either end, or a description that comes back empty.

**Note the size.** The largest description in the store before this was 16,987 bytes; the
paste made one of 131,204. Rendering that costs **7.7 ms** per draw, measured. That is under
a frame budget so it is not a bug — but if you ever paste something ten times larger again,
watch for the preview pane feeling sticky when the task is selected, and say so.

---

## G4 — BUG-9: emoji cut in half

1. Make a task whose title is a flag or a family emoji followed by text — 🇬🇧 or 👨‍👩‍👧 then
   `and some words after it`.
2. Narrow the terminal one column at a time, so the title column has to truncate through the
   emoji.

**Pass:** the emoji is either shown whole or gone, and the ellipsis is where the text stops.

**Fail:** a broken glyph — half a flag, two letters where the flag was, or a stray box —
or the rest of the row shifting sideways by a column as you resize.

The same with the description in the preview pane, where `wrap` rather than `truncate` does
the cutting.

---

## G5 — BUG-10: a column configured by percentage can vanish

1. In `~/.config/tui-do/config.yaml`, give a layout a column with `width_percent: 1`.
2. Start tui-do on a narrow terminal — 80 columns or fewer.

**Pass:** the column is narrow but shows something, or is dropped entirely and cleanly.

**Fail:** a column one cell wide containing only `…`, or blank rows where the title should
be. Worse with CJK text, which cannot be broken between characters.

**This one is already fixed** — it is here so the fix can be confirmed, and so it is caught
if it regresses.

---

## G6 — BUG-4: what happens when the server times out

Needs a proxy that can return a 408, so it is the hardest of these to stage by hand. If you
can put one in front of dev:

1. Edit a task, `Ctrl-S`.
2. Have the proxy answer that request with `408 Request Timeout`.

**Pass:** the change stays queued and retries. The status line still shows it pending.

**Fail — the bug:** the edit vanishes from the screen and a toast says the server refused
it. Nothing was decided by the server; a timeout is not a refusal, and the edit is gone.

If staging a 408 is impractical, this one is better left to a code-level check — say so and
it can be written as a unit test instead, since the classifier is a pure function.

---

## G7 — BUG-7: what a crash inside tui-do leaves behind

There is no way to trigger this on purpose from the interface, which is itself the finding:
it needs a panic, and every effect currently returns an error instead of panicking. Recorded
so that **if you ever see it, you know it is this**:

**The symptom.** tui-do keeps running but the screen goes wrong — the interface draws over
your shell's scrollback instead of its own screen, or the cursor and colours misbehave, and
quitting normally does not put the terminal right.

**If that ever happens:** capture the scrollback, run `reset` to fix your shell, and note
what you were doing. That is BUG-7 and the report is worth more than the reproduction.

---

## G8 — BUG-15: are tasks in an archived project deleted from the local store?

**The highest-value unknown, and it is a data-loss question.** This is a real check, not a
code test, and it needs you because it writes to dev.

1. `deploy/reset-dev.sh`, start tui-do, press `R` for a full sync.
2. Pick a small project — `Trust` had four tasks — and note its task titles.
3. In the **Vikunja web UI**, archive that project.
4. In tui-do, press `R` again for a full pull. Then press `R` a second time.
5. Look for those four tasks. Clear the filter with `t` so completed ones show too.

**Pass:** the tasks are still there.

**Fail:** they are gone from tui-do while the web UI still has them inside the archived
project. That means the full pull's listing did not mention them and the retain step deleted
them locally — the same shape as a bug that already destroyed archived projects once.

**Either answer is worth recording**, and a pass closes the finding.

6. Un-archive the project in the web UI, or `deploy/reset-dev.sh`.

---

## What is not checkable by hand

Recorded so nobody spends an evening trying.

- **BUG-2** — the reordering window is under 50 microseconds. No sequence of keystrokes can
  land inside it. Accepted and not being fixed; see `bugs.md`.
- **BUG-1** — reachable by hand in principle (press `a`, type a task with no due date,
  Enter, then look at which row the cursor is on), but only when the new task does not sort
  to the top, which depends on your layout's sort. It already has an automated proof in
  `crates/tui-do-smoke/tests/add_selection.rs`. **If you want to try it:** sort by due date,
  make sure some overdue tasks exist, then add a task with no date and see whether the cursor
  stays on it. If the cursor jumps to an overdue task at the top, that is BUG-1 — and the
  next `x` would delete that one.
- **BUG-14** — needs a create to be rejected and a delete to be rolled back in one sequence,
  which cannot be staged from the interface.
