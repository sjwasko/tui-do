# tui-do — creating labels: the design, before any code

Written 2026-08-28 at the point where four places in the interface say "tui-do cannot
create labels yet" and nothing in `PLAN.md` owns fixing it. Same spirit as
`md/2026-08-25-phase-4-design.md`: the decisions and why, agreed before the code.

**Nothing here is built.** One decision in it — what a queued mutation's *subject* is —
is cheap to take now and expensive to take after the first `CreateLabel` has been written,
which is the whole reason this document exists rather than a branch.

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

## What has to be measured before any of this is written

`CLAUDE.md`'s standing rule is that the spec describes what the server means, not what it
emits. Three unknowns, none of them answerable from `spec/vikunja.json`:

1. **A duplicate title.** Vikunja's labels are per-user and, as far as anyone here knows,
   not unique — but "as far as anyone here knows" is how the `403`-on-detach surprise got
   in. Create two labels called `urgent` on dev and record what happens.
2. **`PUT /labels` with `id: 0`.** Creating a task needed the path value written into the
   body because Vikunja binds the path first and the body second. `PUT /labels` has no
   path parameter, so the same trap should not exist — should, not does.
3. **Replay.** Creating a label that already exists is the third arm of the
   `is_already_done` table, and it does not have one yet. A lost response means a retry;
   what does the retry answer?

## Still yours to decide

**1. Create only, or the whole lifecycle?** `update_label` and `delete_label` exist on the
client and would need `RenameLabel` / `DeleteLabel` mutations of their own. Create alone
closes the gap the interface complains about; the rest is a bigger surface, and deleting a
label is destructive in a way `u` cannot fully undo (the label comes back with a new id and
detaches from everything it was on).

**2. Where does it appear?** Three candidates, not exclusive: a "create" affordance in the
`l` form, where the user is already looking at the label list; `*newlabel` in quick-add
creating it on the spot; a create action in the `g l` picker.

**3. Should `*newlabel` create silently?** It is the one path where the user did not
obviously ask for a label to exist — they typed a task. A typo becomes a permanent label
on the server, and labels are the kind of thing people curate. A confirmation, or a
config flag, or simply not doing it in quick-add at all.

**4. Which phase owns it?** `PLAN.md` assigns it to none. It is not Phase 5 work (markdown,
detail screen, comments, attachments) and it is not Phase 6 (views). It is a Phase 4 gap
found after Phase 4 was signed off.
