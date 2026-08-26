# criax — the manual checks

What automated tests cannot tell us. Everything here has caught at least one real defect
that a green suite did not, which is why each entry says *what went wrong* rather than
just what to press.

Live, not a fixture: run against the dev instance with a warm cache. `deploy/reset-dev.sh`
restores the seeded baseline if a check makes a mess worth undoing.

---

## A. The two `PLAN.md` calls non-negotiable

**A1 — Offline is not a degraded mode.**
With the server unreachable, criax starts instantly, renders the cached list, accepts edits
into the outbox, and never freezes.

Use an *unroutable* address rather than a refused one — `https://10.255.255.1:8443` in a
scratch config, `CRIAX_CONFIG=... criax`. A refused connection fails in milliseconds and
proves nothing; a hanging connect is what froze the predecessor.

Pass: the full list draws, `j`/`k` move, `d` marks done, the status line says it cannot
reach the server, and `q` quits cleanly — all while the connect is still pending.

**A2 — Production refuses.**
`criax --config` a file pointing at `prod-box` and it must refuse to start, naming the flag.
With `--i-know-this-is-prod` it starts. `criax add` takes the same path and must refuse too.

---

## B. Writes reaching the server

**B1 — An edit lands.** `d` on a task, then check the web UI. Seconds, not minutes.

**B2 — An edit made during the startup pull lands.** Press `d` within the first ten
seconds, while it is still fetching 78 pages. *This was broken:* the push queued behind
the pull and then behind the five-minute timer.

**B3 — Quitting does not strand a push.** Edit, then quit within a second or two. It should
print `Sending 1 queued change…` and `Sent.` *This was broken:* the change sat in the
outbox until the next launch.

**B4 — Offline edits drain on reconnect.** Go offline, make several edits, watch `N queued`
climb in the status line, reconnect. They should drain with no prompting.

**B5 — `criax add` from a shell.** With and without a reachable server. Offline it must say
the task is queued, and the next run must send it.

**B6 — An edit to a task you just made lands.** `a` a task, wait for it to appear in the
web UI, then `d` it. *This was broken:* a created task carries a provisional negative id
until the server names it, and a push-only pass ends in `Pushed`, which does not reload —
so the interface went on holding `-14` and sent `POST /tasks/-14`, answered `404 This task
does not exist` about the task it had just created. Try `u` straight after a create too:
the undo stack names the same id.

**B7 — A task added from a shell shows up on `r`.** Leave the interface open, `criax add
'something'` in another terminal, then press `r`. *This was broken twice over:* the pass
emitted `Finished` last and the runtime aborted the event forwarder before it was
delivered, so the pull wrote to the store and the screen was never told; and `r` pressed
while a pass was running was dropped outright. `r` should also be **instant** for the
local part — the task is already in the store, so nothing needs fetching to draw it.

---

## C. Undo

**C1 — Undo takes back anything.** `d`, `x`, `a` — `u` reverses each, and the list shows it
immediately.

**C2 — Undo goes as deep as the session.** Several edits, `u` all the way back, then one
more: `Nothing to undo` rather than something arbitrary.

**C3 — Undoing a delete re-creates.** The task comes back with a **new id**. Vikunja has no
undelete; this is the documented trade, not a bug. Check the id changed in the web UI.

**C4 — A new edit clears redo.** `d`, `u`, then a different edit — `C-r` must not resurrect
the abandoned branch.

---

## D. A rejection, with the real server

**D1 — The server refusing a write rolls it back.** Go offline, edit a task, delete that
task from the web UI, reconnect. The push meets a 404 on a task that is gone: the change
must roll back on screen *and* toast an explanation. Tested against mocks; only this
exercises the real shapes.

**D2 — Two writers.** Edit the same task in the web UI and in criax while offline, then
reconnect. criax's write is newer and should win, and the next pull should agree with
itself rather than flickering.

---

## E. Layout, which only eyes can judge

**E1 — Three widths.** Full screen, half screen, and a phone. `z s` and `z p` toggle the
panes; a pane that cannot fit must *say so* and say something actionable. *This was
broken:* below 70 columns `z p` set a pane that never appeared, silently, and no keystroke
would bring it back.

**E2 — Termux.** Portrait refuses the preview; landscape lays it out. `L` reaches the
compact layout, which is the one a phone wants.

**E3 — Nothing overruns.** A long toast must not walk over the key hints, and the header
must keep a gap between the breadcrumb and the sync indicator at every width.

**E4 — The help modal is complete.** `?` — the last row must be visible, or the title must
say `j/k scrolls`. *This was broken:* adding one binding pushed the last row off a fixed
height with nothing to indicate it.

**E5 — A short window still scrolls.** Tile the terminal so that fewer rows fit than the
list holds, then hold `j` past the last visible one. The list must follow. *This was
broken:* a row wraps to as many as three lines, and the scroll maths counted lines, so
eighteen tasks in an eighteen-line body drew seven and never moved — the selection walked
down behind a list that had decided it was already showing everything. `PageDown` had the
same fault and jumped about three screens for every one it showed.

**E6 — The pane being driven is the brighter one.** `Tab` through the panes. *This was
broken:* the unfocused selection used `REVERSED`, which swaps each span's own colour into
its background, so a row carrying a due-soon date became a yellow bar — on the pane that
was *not* taking the keys, while the focused pane wore a quiet dark blue.

---

## F. Reading real data

**F1 — Descriptions are text, not markup.** Open a task whose description came from the web
editor. No `<p>` or `<a href>` on screen. *This was broken.*

**F2 — Angle brackets survive.** Find a task whose description contains `<http://…>` or
`CAM_<CAMERA_MAC>_NAME`. It must still be there. *This was broken:* it was deleted up to
the next `>`.

**F3 — Search narrows as you type**, with a live count, and the list stays visible behind
the prompt.

---

## G. Things that are correct but look wrong

Not bugs. Listed so they are not reported as such.

- Two `Inbox` rows in the sidebar: dev genuinely has two real projects with that name
  (`#1` empty, `#12` with the tasks), plus a pseudo-project that is correctly filtered
  out. Vikunja makes `#1` for a new account and `seed-from-prod.sh` brought `#12` in from
  prod, which has only one. Deleting `#1` needs the account's default project moved off it
  first — the server answers `412`, code `3012`, "This project cannot be deleted because it
  is the default project of a user", and an API token cannot change that setting.

  `criax add +Inbox` says which one it chose, by id. **`criax add 'something'` does not**,
  because nothing was named to be ambiguous about — it takes the first real project called
  Inbox, which is `#1`, while the interface is usually showing `#12`. That is why a task
  added from a shell can be correctly stored, correctly pushed, and nowhere on screen.
- A description holding only an embedded image renders blank. There is no text in it;
  attachments arrive in Phase 5.
- `Table` and `Kanban` in the header do nothing. Phase 6 is the views API.
- Numeric dates are **day/month/year**, matching Vikunja: `24/12/2026` is Christmas Eve
  and `12/24/2026` parses as nothing and stays in the title.
- A repeating task toggled done: Vikunja advances its due date rather than marking it
  done. criax shows a tick optimistically and the reload corrects it. **Unmeasured** —
  worth watching.
