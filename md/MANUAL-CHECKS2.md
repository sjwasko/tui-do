# tui-do — the manual checks, part two

Continues `md/MANUAL-CHECKS.md`, which runs A through G and is closed. Lettered afresh
from `A` so neither file needs scrolling to reach its own end; everything new goes here.

Same rules as part one. What automated tests cannot tell us, each entry saying *what went
wrong* rather than just what to press. Live, not a fixture: run against the dev instance
with a warm cache. `deploy/reset-dev.sh` restores the seeded baseline.

Run it on the **release** binary — `~/.local/bin/tui-do` is a symlink to
`target/release/tui-do`, so a debug build proves the tests pass and changes nothing you
are looking at.

## What has been driven, and when

A check nobody has run is not a check. This is the only record of which of these have
faced a human, since the suite cannot tell you and a green CI run says nothing about any
of them.

| section | | last driven |
|---|---|---|
| **A1–A8** | the quick keys | **2026-08-28 — all pass** |
| **B1–B9** | dates | **2026-08-28 — all pass**, B8 and B9 on a re-run after fixes |
| **C1** | `tui-do add` syntax | **2026-08-28 — pass** |
| **D1** | a task shown twice | still unreproduced; see the section itself |
| **E1–E6** | short windows | **2026-08-28 — all pass** |
| **F1, F2, F4** | the form, `C-n`, `C-e` | **2026-08-30 — all pass** |
| **F5, F6** | asking first | **2026-08-30 — pass**, on the rewrite; the first draft could not be followed |
| **F7** | offline, then back | **2026-08-30 — pass**, after a wait the check did not warn about |
| **F3** | the adoption | the one nobody has driven; smoked in `crates/tui-do-smoke` instead |

Section F was driven on 2026-08-30, the day after it was written, and cost three changes
to the thing it was checking rather than to itself: `C-n` could not make a label with a
space in it, the colour field wanted six hex digits from a person who thinks in words, and
a queue inside its backoff had no key that meant "try now". F5 and F6 were rewritten
mid-drive, having asked for something nobody could do.

Part two was driven end to end for the first time on 2026-08-28, on the release binary.
Two entries failed on the first pass and were fixed the same day — B9, which was
implemented on one of its four surfaces, and E6, which was found by hand rather than by
this file and added to it afterwards.

**One anomaly is recorded and not explained.** During B8, task `#2100` (id 3951, titled
"Freelance packet due") came back carrying a due date of 2027-08-26 that its title does
not account for. `created` and `updated` are the same instant, so the date was set at
creation rather than by a later `D`. Every B8 token was put through the parser afterwards
— `p3`, `v1.2.3`, `covid-19`, `3rd`, `separate`, `13/13/26`, alone and appended to that
exact title — and every one survives whole with no date invented. The input that produced
it was not recorded and B8 passed on the re-run. If a title ever loses a word to a date
again, **write down what was typed**: that is the one fact the stored row does not keep.

---

## A. The quick keys

Every one of these is a *write*, so each check ends the same way: look at the web UI and
confirm the server agrees. Part one's section B applies to every one of them — these keys
queue the same mutations `e` does, through the same outbox.

**A1 — `p` refuses what it cannot hold.** The field lists 0 to 5 by number with Vikunja's
own words beside them and marks the one in force `current`. Type `0005`: the field must
stop at `00` and the last two keystrokes must do nothing at all. `9`, `p`, `-`, `.` and
space must be refused the same way — the limit is on the *keystroke*, so an invalid
priority is never something the form is holding. `05` is five; `55` is unreachable.
*This was broken:* `p` opened a fuzzy picker, `0005` matched none of its six rows, and
Enter then did nothing — a hard limit and a broken key look identical from the outside.

Arrows and digits must agree: `3` then `↓` highlights 4 **and** rewrites the field to `4`.
Backspace to empty puts the highlight back on what the task holds, so Enter after a full
rub-out changes nothing rather than setting 0. Picking the priority it already has must
queue **nothing** and say "Nothing changed".

Do this on a **short** window as well as a tall one — see E1, which is where the arrows
went wrong the first time.

The **edit form's** Priority field takes the same limit — `e`, Tab to Priority, and try
`0005` there too. It used to accept any text and report "not a priority between 0 and 5"
on save.

**A2 — `D` opens on the date the task already has.** Prefilled, not empty: `D` then Enter
on an untouched field is a no-op, which is what stops a hesitant press from clearing a
date. `C-u` clears the field, and the status line shows what the parser made of the text
*as it is typed* — `next friday` should resolve before Enter commits to it. An empty field
submitted with Enter clears the date; `Esc` abandons without changing anything.

**A3 — `m` offers only projects that accept writes.** No `Favorites`, no `My Open Tasks`,
no `Inbox` pseudo-project, nothing archived — they reject writes, and Vikunja invents them
into `/projects` anyway. The task must move in the web UI, and the list on screen must
stop showing it if you were looking at the project it left.

**A4 — `l` ticks the labels the task holds.** Space toggles, Enter applies, `Esc` changes
nothing. Applying an unchanged set must queue nothing. Watch the *shape* of what it
queues: labels do not travel in the task body, so a change must appear as `AttachLabel`
and `DetachLabel` rows and never as an `UpdateTask` carrying a new list — that write would
be one the server silently ignores.

**A5 — `l` is the one task key the sidebar keeps.** With the sidebar focused, `l` expands a
project, because that is the vim meaning nobody should have to unlearn. In the list it
opens the label form. It does nothing with the *preview* focused, which is the cost of
that trade; `:` reaches it by name from anywhere.

**A6 — `Space` runs what the config says.** With `quick_actions` set, `Space` lists them
with the key on the left; the key applies it and the menu closes. An unconfigured key
leaves the menu up rather than closing on a keystroke that did nothing, and a second
`Space` closes it. A label quick action **toggles** — press it twice and the label goes on
and comes back off.

**A7 — A quick action naming something that does not exist reports it when pressed.**
Point one at a project or label that is not there. It must say so at the keystroke, not at
startup, and queue nothing. With no `quick_actions` at all, `Space` names the config key
rather than opening an empty box.

**A8 — Every quick key with nothing selected says so.** Filter the list down to nothing,
then `p`, `D`, `m`, `l` and `Space` in turn. Each must say "No task selected" and open no
modal — a key that quietly does nothing reads as a broken key.

---

## B. Dates, which people write a dozen ways

`D` hands its text to the same parser quick-add uses, so anything here can be checked from
either. The status line shows what the parser made of it *as it is typed*, which is the
only place these are visible before they are committed — read it rather than pressing
Enter and checking afterwards.

**B1 — The same day, written both ways.** `27/08/26` and `8/27/26` must both be 27 August
2026. Neither is the house style: only one of them can be read day-first, so the other
falls through to month-first on its own. Likewise `24/12/2026` and `12/24/2026` are both
Christmas Eve. *This was broken:* only day-first parsed, so `12/24/2026` was left sitting
in the title as though it were a word.

**B2 — An ambiguous date goes to Vikunja's reading.** `08/09/26` is **8 September**, not 9
August. Both numbers could be either, so the tie has to go somewhere, and it goes where
the web UI would put it — otherwise the same input means two different days in the two
clients. This is the one rule worth remembering, and it is the only case where being
explicit (`8 sep 26`) is worth the extra keystrokes.

**B3 — The military form.** `27Aug26`, `27aug2026`, `27-Aug-26` and `aug-27-26` are all 27
August 2026. Day-first here too, so `26Aug27` is the **26th of August 2027** — the same
rule, not a special case.

**B4 — Four digits lead.** `2026-08-27`, `2026/08/27` and `2026.08.27` need no guessing at
all: a four-digit part can only be a year.

**B5 — Two-digit years pivot where `strftime` puts them.** `68` is 2068 and `69` is 1969.
Worth knowing before typing a birthday.

**B6 — Months spell out to any length.** `27/september/2026`, `27sept26`, `27 August 2026`
and `august 27 2026`. Single-digit days and months are as good as two: `3/9/2026`.

**B7 — A year-less date still resolves forward.** `17/02`, `2/17` and `17feb` all land on
the next 17 February, the same rule `feb 17` has always followed.

**B8 — A word that merely looks like a date stays in the title.** This is the check that
matters most, because the parser now splits on the boundary between digits and letters to
reach `27aug26` at all — so `p3`, `v1.2.3`, `covid-19`, `3rd`, `separate` and `13/13/26`
all reach the date code and none of them may come back a date. Add a task whose title
contains each and confirm the title survives whole.

**B9 — A date in the past is allowed, and said loudly, by all three ways in.** There are
three: `D`, the edit form's Due field, and a bare date in quick-add. Try `27/08/2024`
through each. Every one must accept it — overdue is a real state and backdating something
you have been carrying is a real thing to want — and every one must answer in the
**warning** colour saying "that date has passed". A quiet "Due 2 years ago" would bless
`2024` typed where `2026` was meant. The status line shows the same before Enter, in the
overdue colour, which is the signal that costs nothing to read.

*This was broken:* only `D` said anything. The edit form answered "Saved" and quick-add
answered "Added", which is exactly the quiet confirmation the rule exists to prevent, and
`tui-do add` said nothing either. Check the fourth surface too:
`tui-do add 'Test 27/08/2024'` must print a `Note:` line.

Saving the edit form on a task that was *already* overdue, without touching its date, must
say plainly "Saved" — a warning that fires when nothing moved is one the user learns to
read past. Note the field compares by **day**, not by instant, because the field shows a
date and the parser gives it 23:59.

If this turns out to be the wrong trade, the alternative is refusing dates before today
outright and needing a flag to override — say so and it is a small change.

---

## C. What `tui-do add` understands, which is not what the keys do

**C1 — The keys are not the syntax.** `tui-do add 'Go to the shop p3 D 26Aug27 l Scooby'`
lands with the whole tail in the **title**, and it is right to: `p`, `D` and `l` are
*interface keys*, not add syntax. The syntax is `!3` for priority, `*Scooby` for a label,
`+Project`, `@user`, and a bare date. The same line written `Go to the shop !3 26Aug27
*Scooby` must set all three. Verified on dev 2026-08-27 as task `#1228` (id 3939), which
landed titled `Go to the Grocery Store p3 D 26Aug27 l Scooby` with priority 0, no due
date and no labels — only `+Groceries` was understood.

Worth deciding rather than leaving: there is no legend on `tui-do add` the way there is on
the `a` prompt, which shows `*label +project !1-5 @user a date` the moment it opens.

---

## D. Seen once, not yet explained

**D1 — A created task appearing twice.** Reported 2026-08-27: `a` a task, and the list
showed it both as a provisional row (a negative id, drawn as `#-050`) and as the adopted
one (`#2093`). By the time it was investigated the server held exactly one task, the
store held exactly one row, and the outbox was empty — so whatever produced it had
already reconciled and it could not be reproduced.

`settle_create` deletes the provisional row inside the same transaction that upserts the
server's copy, so the *store* is never able to hold both. That points at the model rather
than the store: `adopt` retargets the row in `model.data.tasks` and then reloads, and a
reload landing out of order with an adoption is the shape to look for. **Unmeasured.**
If it happens again, note whether a sync was running and whether `r` had just been
pressed.

---

## E. Short windows, where the drawing gives up

The task list was fixed for this in part one's E5. Everything else that draws a list had
the same fault and was fixed together; these are the checks that keep it fixed. **Tile
the terminal short — ten or twelve rows — before starting.**

**E1 — The priority field scrolls to its highlight.** `p` on a task, then hold `↓`. The
highlighted row must stay on screen the whole way round, including the wrap from 5 back to
0. *This was broken:* the modal is sized to its content and then clipped to whatever the
terminal has, and the body truncated instead of scrolling — so the highlight walked off
the bottom edge and **the arrow keys read as doing nothing at all**. They were working the
whole time; there was simply nothing left on screen to show it.

**E2 — Every other list modal, the same way.** `g p` (projects), `g l` (labels), `:` (the
palette) and `l` (the label form) all hold lists and all clip the same way. Type enough to
leave a long list, then hold `↓` and watch the highlight stay put.

**E3 — The project pane scrolls.** Focus the sidebar with `S-Tab` and hold `j` past the
bottom of the pane. The tree must follow the selection, and `k` must bring it back to the
top. *This was broken:* the sidebar carried an `offset` from the first day and **nothing
ever wrote to it**, so the selection walked on into rows nobody could see. Collapsing a
project with `h` must settle it too — the tree gets shorter and every row below moves.

**E6 — `d` holds its place down a list, the way `x` does.** With completed tasks hidden,
put the cursor a few rows down and press `d` repeatedly. Each press must tick the task off,
drop it from the list, and leave the cursor on the row that slid up into the gap — so a run
of presses works straight down the list. The last row is the exception: with nothing below
it the cursor steps up rather than vanishing. *This was broken:* `d` left the row in place
with a tick, the reload then dropped it, and `TasksLoaded` cannot tell "the selected row
left this list" from "this is a different list" — so it fell back to the first row and
every press sent the cursor to the top.

With `t` on, so completed tasks are shown, the opposite must hold: the row belongs in the
list either way, so it stays put and simply gains a tick.

`u` immediately after must bring the row back. It returns on the reload the runtime fires
after every write, not instantly, so give it a moment before calling it broken.

**E4 — A tree that shrinks does not leave an empty pane.** Scroll the sidebar to the
bottom, then do something that shortens it: collapse a parent, or let a pull drop a
project. The pane must not draw blank over a list that is still there.

**E5 — Help folds into two columns rather than scrolling.** On a normal terminal — 120
columns by 40 rows or better — `?` must open with the whole reference on screen: the
title plain `Keys` with no `j/k scrolls`, `Navigation` at the head of the left column,
`Application` and `q / C-c  Quit` visible at the foot of the right one. *This was the
state before:* thirty-five bindings and four headings need forty-four rows, so help
opened already scrolled, with the key for quitting below the fold on the one screen whose
whole job is saying which key does what.

Then narrow the terminal to eighty columns. Two columns no longer fit, so it must go back
to one, scroll, and say `j/k scrolls` in the title — not draw two columns of truncated
descriptions. The fold is only ever at a heading, so no section is split across the
gutter; the empty space under the shorter column is that rule's cost and is deliberate.

---

## F. Making a label, which the interface used to apologise for

Labels can be created, renamed and recoloured as of 2026-08-29 — designed in
`md/2026-08-28-label-creation-design.md`, built out of sequence between Phase 4 and Phase
5. Before that, four places in the interface said in words that tui-do could not do it.

Every one of these is a **write into a pool Vikunja shares across every project**, so each
check ends the way section A's do: look at the web UI and confirm the server agrees. There
is deliberately no delete — see "What is known to be wrong" at the end, and clear up after
yourself with `deploy/reset-dev.sh`.

**F1 — `l` opens on an empty pool.** Press `l` on a task with no labels. The form opens,
and where the list would be it says ` no labels yet — type a name`. *This was broken:* the
form refused to open and toasted "No labels exist yet" — an apology, delivered on the one
screen from which a label can now be made. If dev's pool is not empty, delete its labels
in the web UI first: this is the only check that needs the empty case, and the old toast
is what made it unreachable.

**F2 — `C-n` makes a label the pool has not got.** Press `l` on a task. The box at the
top of the form does two jobs, and this check is about the second: it fuzzy-filters the
list below it, *and* it is where the name of a new label is typed. Type something no
label is called — `probe-alpha` — and the footer offers `C-n creates "probe-alpha"`.

**One word only, and that is the Space key, not a rule about labels.** Space toggles the
tick on the highlighted row, so it never reaches the box: nothing typed here can hold a
space, and `C-n` therefore only ever makes a single-word label. Vikunja is perfectly happy
with `in progress` — the two ways to make one are `C-n` then `C-e`, which is the rename
form and does take spaces, or quick-add's bracket forms, `*"in progress"` and
`*[in progress]`. Driven 2026-08-30; see the end of this section.

**When the offer appears is the part to get right, because it is not "the filter found
nothing".** It is "no label has this title" — an exact match, ignoring ASCII case, and
nothing to do with what the fuzzy filter turned up. Drive both halves:

- With `work` in the pool, type `wor`. The list still shows `work`, because the filter is
  fuzzy — and the footer must *still* offer `C-n creates "wor"`, because no label is
  called `wor`. A filter that found something is not evidence the name is taken.
- Type `work`, then `WORK`. Both times the offer must be gone. Case folds; leading and
  trailing spaces are trimmed before the comparison, so ` work ` is also taken.

Now press `C-n` on a name that is free. A toast says `Created label …` and the status
line gains one queued change immediately. The label itself appears in the list a beat
later, already **ticked** — the tick rides in on the reload that follows the local write,
not on the server's answer, so it is quick but need not be the same frame. The name stays
in the box, so what you watch is the offer you just used being replaced by the row it
made: `wor` is now a label called `wor`, and `C-n` no longer offers anything.

Press `C-n` a second time on that same name while the queue count is still up. Nothing
must happen, and the offer must stay gone — the form remembers the titles it has asked
for, so a create still in flight cannot be queued twice by an impatient second press.

Then Enter, which applies the ticks in the ordinary way and closes the form. Check the web
UI: the label is in the shared pool, and it is on the task.

Note what does *not* create: Enter, which applies ticks and nothing else. Adding to a pool
every project shares never shares a key with a reflex — the same rule `y` follows in F5.

**F3 — the adoption reaches the form that is still open. The one that matters.** Do F2 and
then, *without closing the form*, press `r`. (A write starts a push by itself, so this may
already have happened; watch the queue count reach zero.) The label must keep its name and
its tick **in the form you are looking at**, and Enter must still attach it — check the
web UI shows it on the task.

A created label carries a provisional negative id until the server names it, and every
holder of that id has to move at the same moment: the store rows, the queued attach,
`model.data.labels`, the labels inside every task, the undo and redo stacks, the filter
scope, the `g l` picker's candidates, the edit form's `before` task, and the open `l`
form's own cloned list. That last one is the holder every implementer of this feature came
closest to missing, and its failure is **silence** — the form resolves its ticks against a
renumbered list, matches nothing, queues no mutation and says "Nothing changed". So a pass
here is not "no error appeared". It is the label reaching the task.

**Smoked automatically as of 2026-08-30**, in `crates/tui-do-smoke/tests/labels.rs`. That
crate exists for this: `tui-do-ui`'s tests hand `update` a `SyncEvent::Adopted` by hand and
`tui-do-core`'s prove a push emits one, and neither can prove they are the *same* event.
The smoke drives `l`, the name, `C-n` and Enter through the real `update`, the real store
and the real engine against a mock server, and asserts the task ends up carrying the id the
server issued. Deleting the engine's label adoption makes it fail; that was checked rather
than assumed.

It does not retire this check. The harness reimplements the effect runtime rather than
running it, so the runtime's own wiring is the one link it cannot see, and a mock server is
not Vikunja. What the smoke buys is that F3 no longer has to be driven to find a *break* —
only to confirm the parts nothing automated touches.

**F4 — `C-e` renames and recolours, from both lists.** Highlight a label in the `l` form
and press `C-e`; then do the same from `g l`, where the key is advertised in the title
rather than a footer. Tab moves between Title and Colour, Enter saves, Esc abandons. A
colour is a **name** or six hex digits, and anything else is refused *in the form* rather
than queued — a server rejection rolls the whole write back, taking the rename with it.
An empty title is refused the same way.

**The names, added 2026-08-30 because hex is a bad thing to ask a person for.** The form
lists all eight on its own bottom row — `red orange yellow green blue purple pink grey` —
and takes two it does not list: `gray`, which is `grey`, and `none`, which is empty.
Type `blue` and the chip beside the field turns blue *as you type*, before Enter: the
name resolves where the drawing can see it, so the preview is the colour that will be
saved. What goes on the wire is always the hex, because Vikunja stores nothing else —
check the web UI and you will see `3498db`, not the word.

Hex still works, and is the only way to anything outside the eight. Worth typing in the
same visit: `#3498db`, taken with the hash trimmed; `f00`, refused, because six digits
means six and not a shorthand; `navy`, refused rather than guessed at, and left in the box
as typed so the message can be about it; and an **empty** box, which is legal and means
"let tui-do pick" — that label draws in the accent style, not in black.

One pair is worth typing as hex on purpose. The colour is the chip's *background* and
`Theme::label` picks the text from its luminance — black above 0.55, white below — so
`e67e22` (0.555, must draw dark on light) and `27ae60` (0.548, must draw light on dark)
sit either side of the line by half a percent and are what proves the flip still works.
They are also `orange` and `green`, so the names reach the same two colours.

Then `Esc` back to the list and press `u`. The label must come back with **both** its old
title and its old colour, even if you only changed one: a partial body clears what it
omits (measured on dev 2026-08-29 — a body carrying only `title` cleared `hex_color` to
`""`), so the write always carries both and so does the undo.

`u` straight after a **create** is different, and deliberately: it undoes whatever you did
*before* the create. Nothing about a create is on the undo stack, because taking one back
would mean deleting a label, and the label would return with a new id detached from
everything it was on.

**F5 — an unknown `*label` asks before it makes one, and takes no for an answer.** Four
surfaces ask the same question through the same code. Drive them in this order; each is a
few keystrokes and the fourth is a shell command.

1. **Quick-add.** Press `a`, type `Call the VA *waiting`, Enter. No label is called
   `waiting`, so instead of the task being added a box appears, titled
   `No such label  —  y creates, n leaves it off`, listing `waiting` under it and saying
   `Labels are shared by every project; typos are forever.`
2. **Say no.** Press `n`. The task is added without the label and a **warning-coloured**
   toast reads `Added "Call the VA" — no label called waiting`. Check the status line: it
   should show one queued change, the task. Nothing else was queued, and the web UI has no
   label called `waiting`.
3. **The three keys that mean no.** Repeat step 1 twice more and answer `Esc`, then
   `Enter`. Both must do exactly what `n` did — task kept, label left off. `Esc` is the
   one worth being deliberate about: everywhere else in tui-do it means "back out,
   changing nothing", and here backing out would throw away the line you typed, because
   the prompt that held it has already closed. Every key that closes this box keeps the
   task; only the label is in question. Any *other* key — `q`, `j`, a digit — must do
   nothing at all, leaving the box up.
4. **The edit form asks it too.** Open a task with `e`, put `waiting` in the Labels field,
   `Ctrl-S`. The same box, the same three answers.
5. **The command line cannot ask, so it tells.** Run
   `tui-do add 'Call the VA *waiting'`. It must print, after queueing the task:
   `No label called waiting — it was left off. Pass --create-labels next time to have
   tui-do create it; this task is already queued, so re-running now would add a second
   one.` The "next time" matters and is not padding: re-running the command to get the
   label would add a second task.

**F6 — say yes, and the box holds still until the label is real.** The same line as F5,
`y` this time. `y` and only `y`: Enter is the key that *submitted* the quick-add a frame
ago, so this box opens under a finger already resting on it, and a second press must not
be able to add to a pool every project shares.

The box stays up and its bottom line changes from the typos warning to `Creating…`. It has
to wait: the interface never learns the new label's id any other way, and the task has to
carry it. Against dev this is usually too fast to read — that is fine, and step 2 below is
how you see it properly. Then the box closes, the task is added carrying `waiting`, and
the web UI shows **one** label called `waiting`, on that task.

Two things to drive deliberately, because both are silent when they break:

1. **A second `y` while it says `Creating…` must do nothing.** A label title is not unique
   — creating `waiting` twice answers `201` twice with two different ids and nothing in
   either response to tell them apart — so a second yes would leave a duplicate nobody can
   sort out afterwards.
2. **`n` still works while it says `Creating…`**, and does *not* recall the create. The
   label gets made and the task simply does not carry it. That is deliberate: this is the
   key that guarantees the box can always be closed, and a version of it that waited on a
   server would not be.

**Both need the create held open, which `test-scripts/go-offline.sh` does** — the address
is unroutable, so the connect hangs rather than failing. Run it, restart tui-do, then drive
F6 again: the box will sit on `Creating…` for as long as you like. Hammer `y`; the web UI
must show no second `waiting` when the queue finally drains. Then repeat and press `n`
instead, and confirm afterwards that `waiting` exists but is not on the task.
`test-scripts/restore-config.sh` when done, and `r` to drain (see F7).

**F7 — offline, then back: one label, not two.** `test-scripts/go-offline.sh`, create a
label from the `l` form, and watch the status line settle on `1 queued (1 failing)` — the
address is unroutable, so the first attempt hangs before it fails. Then
`test-scripts/restore-config.sh` and let it drain: **one** label on the server, with the
colour you gave it.

**"Let it drain" can mean waiting five minutes, and the check used to leave that out.**
A failed entry carries a `next_attempt_at`, and *every* pass skips it until then — the
timer's, the startup one, and the `r` you press yourself. Restoring the URL does not reach
the queue, and neither does restarting: driven on 2026-08-30 the entry came due fourteen
seconds *after* the startup pass had already looked at it, so nothing tried again until
the five-minute timer came round. For those five minutes the status line said
`2 queued (1 failing)` and `last_error` still named `10.255.255.1` — the address from the
last attempt, not the one it would use next. That reads exactly like a wedge and is not
one. Count the wait before calling it a failure, and remember the queued count is one per
*mutation*: a task made offline with a label on it is two, not one.

**`r` is now the answer to that wait**, added the same day: a sync you ask for retries
everything queued, where the timer still waits the backoff out. So the check has a second
half now — restore the config, press `r`, and the queue must drain *then*, not five
minutes later. Driving it without pressing anything still works and still takes as long as
the timer takes.

What this cannot reach by hand is the case the read-before-retry exists for — a create
that reached the server whose *response* was lost. A retry finds its own label with
`GET /labels?s=<title>` and adopts it instead of making a second. If you can arrange one
(cut the network between the request and the answer), do: it is the one path here with no
way to fail loudly.

### What is known to be wrong, and is not being fixed yet

Recorded here rather than left in a plan that gets deleted. None of these are regressions;
each was found during the build on 2026-08-29, judged, and deliberately left. If one of
them bites, it will look like a bug in the checks above.

- **No route back from a typo.** A user who types `*waitng` in quick-add can only create
  it or drop it, then re-edit the task; the box has no "let me fix the line" answer,
  because the prompt that held the text is already closed. Editing the line back into
  existence is a bigger change than the sharpness justified at the time.
- **`Esc` then `l` inside the reload window can queue a duplicate create.** The guard that
  stops a second `C-n` on the same name lives in the form's own state, so closing the form
  and reopening it before the label comes back reopens the window. One keystroke, one
  round trip wide, one duplicate the user can see. Closing it properly needs model-level
  state.
- **A rolled-back `DeleteTask` can resurrect a provisional label row.** If a create is
  rejected and a delete of a task carrying that label is rolled back afterwards, the
  restore re-writes the task with the label the rejection had removed. Pre-existing shape,
  documented in the code, not reachable by the checks above.
- **Case is folded ASCII-only, everywhere.** `café` and `CAFÉ` are two labels; `cafe` and
  `CAFE` are one. Four places fold this way and they were left agreeing with each other
  rather than made half-right.
- **A retry can adopt somebody else's label.** The read-before-retry cannot tell a retry
  whose response was lost from one that never reached the server, so it may adopt a
  same-titled label another box made and quietly drop the colour you queued with yours.
  The trade is deliberate: a duplicate you can see is worse than a colour you can re-pick.
- **Two boxes creating the same title at the same moment still make two labels.** Nothing
  can prevent that without a unique constraint Vikunja does not have.
- **`C-n` cannot make a label with a space in it.** Space is the tick key in the `l` form,
  so it never reaches the name box, and a title typed there is one word by construction.
  Found by driving F2 on 2026-08-30. Not a data limit — the rename form (`C-e`) and
  quick-add's `*"in progress"` both make multi-word labels — so the honest description is
  that one of three creation routes is narrower than the other two, silently. Fixing it
  means either moving the tick off Space or making `C-n` open the rename form pre-filled,
  and both are design changes rather than corrections.
- **Cosmetic, unfixed:** the undo toast writes label titles unquoted where a task's title
  is quoted (`Undone — errands`), and the `l` form's footer reserves a separator's width
  even when it is showing only one hint, costing about one character of title.
