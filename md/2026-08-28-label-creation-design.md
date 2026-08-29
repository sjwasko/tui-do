# tui-do — creating labels: the design, before any code

Written 2026-08-28 at the point where four places in the interface say "tui-do cannot
create labels yet" and nothing in `PLAN.md` owns fixing it. Same spirit as
`md/2026-08-25-phase-4-design.md`: the decisions and why, agreed before the code.

**Revised 2026-08-29**: the three unknowns are measured, the two open decisions are
taken, and one thing this document assumed would work does not — see "What was measured".
The one line of code that came out of that session is the `update_label` verb fix; the
feature itself is still unbuilt.

**One decision here — what a queued mutation's *subject* is — is cheap to take now and
expensive to take after the first `CreateLabel` has been written**, which is the whole
reason this document exists rather than a branch.

## Where the gap actually is

The wire half is done and proven. `Client::create_label`, `update_label` and
`delete_label` all exist, and `tests/live.rs` creates a label against dev and attaches it.

What is missing is one variant. `Mutation` has `CreateTask`, `UpdateTask`, `DeleteTask`,
`AttachLabel`, `DetachLabel` — and rule 5 says every write goes through the outbox, so
with no `CreateLabel` there is no way to ask for one. The interface says so in four
places rather than failing silently, which is the right holding position:

| where | what it says |
|---|---|
| `l` with no labels anywhere | "No labels exist yet" — the form refuses to open rather than showing an empty box |
| the edit form's Labels field | "No label called X — tui-do cannot create labels yet" |
| the `a` quick-add prompt | "no label called X" in the toast; the task is still created |
| `tui-do add` | "No label called X — tui-do cannot create labels yet, so it was left off." |

## The decision that has to be taken first

`Mutation::subject()` returns a `TaskId`. That signature is load-bearing in five places:

1. the `outbox.subject_id` column, and the `outbox_by_subject` index over it;
2. `Store::is_pending(task)`, which the sync engine consults before storing the server's
   echo of a write;
3. `retain_tasks`, whose delete is guarded by
   `id NOT IN (SELECT subject_id FROM outbox WHERE subject_id IS NOT NULL)` — this is what
   stops a pull from deleting a task created offline;
4. `settle_create`, which finds queued entries still naming a provisional id with
   `SELECT id, payload FROM outbox WHERE subject_id = ?`;
5. the rejection path in `Sync::push`, which discards everything queued behind a rejection
   `filter(|queued| queued.mutation.subject() == subject)`.

**`subject_id` is a bare `INTEGER` with no type tag.** Provisional task ids come from
`next_local_id`, counting down from `-1`. If labels get a counter of their own it will
also start at `-1` — and then provisional task `-1` and provisional label `-1` are the
same value in the same untyped column. Two of the five sites above break silently:

- `retain_tasks` would spare a task whose id happens to match a queued *label's* subject,
  or — worse, and in the other direction — the guard stops meaning what it says;
- `settle_create`, adopting task `-1`, would rewrite the payload of a `CreateLabel` entry
  whose subject is label `-1`, calling `Mutation::retarget` on it with a `TaskId`.

Neither fails loudly. Both corrupt the queue.

### Three ways out

**A — a `Subject` enum, and a kind column.** *Recommended.*

```rust
pub enum Subject { Task(TaskId), Label(LabelId) }
```

`subject()` returns it; `outbox` gains `subject_kind TEXT NOT NULL`; the five query sites
gain `AND subject_kind = 'task'`. Costs a schema migration and touches all five, once,
while there are five. Extends to projects without another decision, which matters because
Phase 6 is the views API and projects are the obvious next thing to create locally.

**B — one shared id space.** Keep `subject_id` a task id and allocate provisional label
ids from the *same* counter, so `-1` is either a task or a label but never both.

No schema change and no enum. Rejected anyway: `subject_id` then means "a task id, except
when it is a label id", and the guard in `retain_tasks` reads as protecting a task when it
may be protecting a label. That is precisely the illegal-state-representable shape that
rule 2 of `CLAUDE.md` exists to keep out of this codebase, moved from a struct into a
column where it is harder to see.

**C — a second queue for label lifecycle mutations**, drained before the task queue.

Rejected. The queue's central invariant is that entries are ordered and later ones assume
earlier ones landed. An `AttachLabel` assumes its `CreateLabel` landed, and across two
queues there is no ordering to assume. Two queues is two orderings and one of them has to
know about the other.

## Adoption: the second expensive-later decision

A created label carries a provisional id until the server answers, exactly as a task does,
and everything holding that id has to move together. `SyncEvent::Adopted` is currently

```rust
Adopted { provisional: TaskId, assigned: TaskId }
```

and the doc comment on `adopt` in `update.rs` already spells out why missing a single
holder is fatal: the next write goes to `/tasks/-14` and comes back `404` about the task
the server just created.

A label has **more** holders than a task, and one of them is easy to miss:

- the `labels` row, and every `task_labels` row naming it;
- queued `AttachLabel` / `DetachLabel` entries, which carry the whole `Label` by value;
- `model.data.labels`;
- the `labels` vector inside every `Task` in `model.data.tasks` — a label's id appears in
  the task rows too, not just in the label list;
- the undo and redo stacks, whose mutations carry whole `Label` values;
- **an open `Modal::Labels`**, whose `LabelsState` holds cloned labels. Adopting while
  that form is on screen, which is exactly when a label was just created, would leave the
  form holding an id no server has seen.

So `Adopted` carries a `Subject` on both sides, and `adopt` splits by kind.

## Who decides that a label is new

Not `decompose`. It splits a task write into one request each and has no idea which labels
exist — that knowledge lives in the UI, in `resolve_labels`, which already returns
`(found, missing)` and today only uses `missing` to compose an apology.

So the UI queues, in order:

```
CreateLabel { label }        provisional id, one per missing name
CreateTask  { task }         carrying those provisional labels
AttachLabel { task, label }  emitted by decompose, naming the provisional label id
```

The queue is strictly ordered and a failure stops it rather than skipping past, so the
attach cannot outrun the create. Adoption retargets the attach when the label lands. No
new knowledge in `tui-do-core`, and `decompose` is untouched.

## The rejection blast radius needs widening

`Sync::push` treats a 4xx as final: it rolls the change back and discards everything else
queued **for the same subject**, because those changes were built on a state the server
has refused to have.

A rejected `CreateLabel` breaks that rule's assumption. The entries that must die with it
are the `AttachLabel`s that name the label — and their subject is the *task*, not the
label. Same-subject filtering will not find them, and leaving them queued sends an attach
for a label id no server has ever seen.

So the discard rule becomes: everything sharing the subject, **plus** everything whose
payload references the rejected label. That is a genuine new case, it has no equivalent
today, and it wants its own test with the arm removed.

## What was measured, 2026-08-29

Measured against dev with `curl`, then pinned in
`crates/tui-do-api/tests/live.rs::a_task_round_trips_through_create_read_update_delete`.
All of it is in `CLAUDE.md` now; what follows is what each finding does to this design.

**`Client::update_label` was broken, and this is what found it.** It sent
`PUT /labels/{id}`, which is what `spec/vikunja.json` documents. The server answers
`405 Method Not Allowed`; `OPTIONS /labels/{id}` replies
`Allow: OPTIONS, DELETE, GET, POST`. Nothing has ever called the method, so nothing ever
saw the 405 — and the conformance test compares path templates, not verbs, so it never
could. Fixed to `POST`, with an assertion in the live test that fails when the verb goes
back.

**1. A duplicate title is allowed.** Two labels called `tui-do probe alpha`, two `201`s,
two different ids. Labels are not unique.

**2. `PUT /labels` ignores the body's `id`.** Sent `0`, got a server id; sent `-7`, got a
server id. Good news, and one worry removed: a `CreateLabel` payload carrying a
provisional negative id is harmless on the wire.

**3. A replayed create is undetectable, and this is the finding that changes the design.**
Because titles are not unique and the body's id is ignored, a retry does not answer "you
already did this" — it answers `201` and a second label. Nothing in the response
distinguishes it. So `is_already_done` **cannot** grow an arm for `CreateLabel`, which is
what this document assumed it would.

What replaces it: **a `CreateLabel` that is being retried reads first.** Before a second
attempt, ask `GET /labels?s=<title>` (the parameter works; it matched all four probes) and
if a label with that exact title already exists, adopt its id instead of creating. This is
the same shape as `Task::merge_onto` — *look at the server before writing, because this
box is not the only writer* — applied to the one mutation where a lost response is
otherwise unrecoverable.

Two honest limits, both worth saying out loud rather than discovering later:

- It protects against **our own retry**, not against two boxes creating `next` at the same
  moment. Nothing can protect against that without a unique constraint the server does not
  have.
- If the user genuinely wanted a second label with the same title, a retry adopts the
  first instead. That is the right trade — two labels called `next` in a global pool is the
  bad state, not the good one — and it only ever happens on a retry, never on a first
  attempt.

**4. A partial body clears what it omits**, the same as a task: `POST /labels/12` carrying
only `title` cleared `hex_color` to `""`. A rename sends the whole label.

**5. The body's `id` beats the path.** `POST /labels/12` carrying `"id": 13` updated label
**13** and left 12 alone. Worse than the task version of this trap, which 404s: this one
succeeds against the wrong row. `update_label` now takes the whole label and derives the
path from it, so the two cannot disagree.

**6. Rename and delete of a label that is gone both answer `404` code `8002`.** Those are
arms `is_already_done` can have, and should: another box deleting a label while this one
had a rename queued is an ordinary fleet event, not a rejection to roll back.

### What the measurements add to the build

- `Mutation::CreateLabel` needs a **reconcile-before-retry** step, driven by the outbox's
  `attempts` column, which already exists and now drives backoff.
- `is_already_done` gains two label arms (`404`/`8002` on rename and on delete) and
  deliberately **no** arm for create.
- `RenameLabel` carries the whole `Label`, and — for the same reason `UpdateTask` does —
  should read the server's copy and merge onto it before writing. A `Label::merge_onto`
  mirroring `Task::merge_onto`, destructured exhaustively so a new field fails to compile
  rather than becoming quietly uneditable. Three editable fields (`title`, `description`,
  `hex_color`) makes this cheap; skipping it means box B's rename silently reverts box A's
  recolour.

## The decisions, taken 2026-08-29

**1. Create, rename and recolour. No delete.** Delete is the one operation `u` cannot
honestly reverse: the label comes back with a new id, detached from everything it was on.
Deleting stays a web-UI job until someone asks for it, and if it is ever built it needs a
confirmation and an honest "this cannot be undone" rather than a broken undo.

**2. `*newlabel` in quick-add confirms before it creates.** Everywhere else the user is
looking at a label list and asking for a new one; in quick-add they typed a task, and the
label pool is global, so a typo becomes a permanent entry that pollutes autocomplete in
every project forever. The confirmation is the difference between one keystroke to reject
and a curation job later.

**3. Where it appears:** the `l` form and the `g l` picker both grow a create affordance —
that is where the user is already looking at the list — and quick-add creates only through
the confirmation in decision 2.

**4. Which phase owns it:** none. It is a Phase 4 gap found after Phase 4 was signed off,
and it is being built before Phase 5 at the user's direction, 2026-08-29.
