# Label lifecycle implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let tui-do create, rename and recolour labels through the outbox, closing the
four places in the interface that say "tui-do cannot create labels yet".

**Architecture:** A queued mutation's subject becomes a `Subject { Task, Label }` enum
backed by a new `subject_kind` column, so a provisional label id and a provisional task id
can both be `-1` without colliding in an untyped column. `CreateLabel` and `UpdateLabel`
join the existing five `Mutation` variants and ride the same optimistic-write, adoption,
rollback and undo machinery. Two things are specific to labels: a create that is being
*retried* reads `GET /labels?s=<title>` first and adopts what it finds, because a replayed
create is otherwise undetectable, and a rejected create takes the attaches naming it down
with it even though their subject is the task.

**Tech Stack:** Rust 2021, `rusqlite` (bundled), `reqwest`, `tokio`, ratatui, `wiremock`
for API tests.

**Spec:** `md/2026-08-28-label-creation-design.md` — read it first, especially "What was
measured, 2026-08-29" and "The decisions, taken 2026-08-29".

## Global Constraints

- **Rule 1 — the render loop never awaits I/O.** `tui-do-ui` stays synchronous: `update`
  returns `Vec<Effect>` and never touches a `Store` or a `Client`. `a_pure_ui_names_no_io`
  greps the crate for `Store`, `.await`, `spawn_blocking` and the three crate names.
- **Rule 2 — no `show_x_modal: bool`.** New screen state is an `enum Screen` variant or a
  `Modal` variant, never a bool beside an `Option`.
- **Rule 3 — no endpoint that is not in `spec/vikunja.json`.** Every path used here
  (`/labels`, `/labels/{id}`) is already declared in `endpoints.rs` and covered by the
  conformance test.
- **Rule 4 — pagination is never assumed.** Anything that lists goes through `Pager`.
- **Rule 5 — writes are optimistic and merged before they are sent.** Every write is a
  `Mutation` through `Store::queue`; nothing calls a client method directly from the UI.
- **Workspace lints deny `unwrap`, `panic`, `todo`, `dbg!` and forbid `unsafe`** in
  production code. Test modules may `#![allow]` them at module level, as the existing ones
  do.
- **Migrations are append-only.** A mistake is corrected by adding a migration, never by
  editing one that has shipped. This plan adds exactly one: **v5**.
- **Commit messages** describe the behaviour and why, in the voice of the existing log.
  Every commit ends with the `Co-Authored-By:` and `Claude-Session:` trailers already in
  use on this branch.
- **Build `--release` before saying anything is ready to try**, because
  `~/.local/bin/tui-do` is a symlink to `target/release/tui-do`.
- **Dev server only:** `https://dev-box.example.net:8443`. Prod
  (`prod-box`) is read-only, always.

## File Structure

| file | responsibility after this plan |
|---|---|
| `crates/tui-do-core/src/store/schema.rs` | gains migration **v5**: `outbox.subject_kind` |
| `crates/tui-do-core/src/store/outbox.rs` | `Subject`, `CreateLabel`, `UpdateLabel`, provisional label ids, `settle_create_label`, and the five subject query sites |
| `crates/tui-do-core/src/sync/mod.rs` | `transmit` arms for the two new mutations, reconcile-before-retry, the label arms of `is_already_done`, the widened rejection blast radius, `SyncEvent::Adopted` carrying a `Subject` |
| `crates/tui-do-api/src/models/label.rs` | `Label::merge_onto`, mirroring `Task::merge_onto` |
| `crates/tui-do-api/src/client.rs` | `labels_named`, the title search a retried create reconciles against |
| `crates/tui-do-ui/src/update.rs` | `adopt` splits by subject kind; the `l` form, the picker and the edit form learn to create |
| `crates/tui-do-ui/src/modal.rs` | `LabelsState` gains a create affordance; `Submission::CreateLabel` |
| `crates/tui-do/src/runtime/mod.rs` | `tui-do add` stops apologising and queues creates |
| `CLAUDE.md`, `PLAN.md`, `md/MANUAL-CHECKS2.md` | the rules and the checks that came out of it |

---

### Task 1: `Subject` — a typed subject, and the column that carries it

**Files:**
- Modify: `crates/tui-do-core/src/store/schema.rs` (append migration v5)
- Modify: `crates/tui-do-core/src/store/outbox.rs` (`Subject`, `subject()`, five query sites)
- Modify: `crates/tui-do-core/src/sync/mod.rs` (`push`'s `blocked` set, `SyncEvent::Rejected`)
- Test: the existing test modules in both files

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub enum Subject { Task(TaskId), Label(LabelId) }
  impl Subject {
      pub const fn kind(self) -> &'static str;   // "task" | "label"
      pub const fn id(self) -> i64;
      pub const fn task(self) -> Option<TaskId>;
  }
  impl Mutation { pub fn subject(&self) -> Subject; }
  ```

This task changes no behaviour. Every existing test must still pass, and the point of
doing it alone is that a reviewer can see that.

- [ ] **Step 1: Write the failing test**

In `crates/tui-do-core/src/store/outbox.rs`'s test module:

```rust
#[test]
fn a_queued_entry_records_what_kind_of_thing_it_acts_on() {
    // `subject_id` is a bare INTEGER and provisional ids count down from -1 for each
    // kind, so task -1 and label -1 are the same value in the same column. The kind is
    // what keeps `retain_tasks` and `settle_create` from confusing them.
    let store = store();
    let entry = block_on(store.queue(Mutation::CreateTask {
        task: Box::new(task(0, "written")),
    }))
    .unwrap();
    assert_eq!(entry.mutation.subject().kind(), "task");

    let kinds: Vec<String> = block_on(store.read(|connection| {
        let mut statement = connection.prepare("SELECT subject_kind FROM outbox")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }))
    .unwrap();
    assert_eq!(kinds, vec!["task".to_string()]);
}
```

Copy the `store()`, `task()` and `block_on` helpers the module already uses; do not
invent new ones.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p tui-do-core --lib outbox::tests::a_queued_entry_records`
Expected: FAIL — `no such column: subject_kind`.

- [ ] **Step 3: Add migration v5**

Append to `MIGRATIONS` in `schema.rs`, after the v4 entry:

```rust
    // v5 -- what kind of thing an outbox entry acts on.
    //
    // `subject_id` is untyped, and provisional ids count down from -1 for each kind, so
    // a locally created task and a locally created label would both be -1 in the same
    // column. Two queries break silently on that: `retain_tasks` spares a task whose id
    // matches a queued label's subject, and `settle_create` rewrites a label entry's
    // payload with a `TaskId`. Every existing row is a task -- there was nothing else to
    // queue -- so the default backfills them correctly.
    r"
    ALTER TABLE outbox ADD COLUMN subject_kind TEXT NOT NULL DEFAULT 'task';
    CREATE INDEX outbox_by_subject_kind ON outbox (subject_kind, subject_id);
    ",
```

- [ ] **Step 4: Add the `Subject` enum**

In `outbox.rs`, above `Mutation`:

```rust
/// What a queued mutation acts on.
///
/// Not a bare id: `outbox.subject_id` is an untyped `INTEGER`, and provisional ids count
/// down from `-1` **per kind**, so a locally created task and a locally created label are
/// both `-1`. The kind is stored beside the id and every query that means "tasks" says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subject {
    /// A task, by id.
    Task(TaskId),
    /// A label, by id.
    Label(LabelId),
}

impl Subject {
    /// The short name stored in `outbox.subject_kind`.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Task(_) => "task",
            Self::Label(_) => "label",
        }
    }

    /// The bare id, for the column.
    #[must_use]
    pub const fn id(self) -> i64 {
        match self {
            Self::Task(id) => id.get(),
            Self::Label(id) => id.get(),
        }
    }

    /// The task, when this is one. `None` for a label, which is the answer every
    /// task-shaped caller wants.
    #[must_use]
    pub const fn task(self) -> Option<TaskId> {
        match self {
            Self::Task(id) => Some(id),
            Self::Label(_) => None,
        }
    }
}
```

- [ ] **Step 5: Change `Mutation::subject` to return it**

```rust
    /// What this entry acts on.
    #[must_use]
    pub fn subject(&self) -> Subject {
        match self {
            Self::CreateTask { task } => Subject::Task(task.id),
            Self::UpdateTask { after, .. } => Subject::Task(after.id),
            Self::DeleteTask { before } => Subject::Task(before.id),
            Self::AttachLabel { task, .. } | Self::DetachLabel { task, .. } => Subject::Task(*task),
        }
    }
```

- [ ] **Step 6: Update the five sites the compiler will now point at**

1. The `INSERT` in `Store::queue` — write both columns:

```rust
                tx.execute(
                    "INSERT INTO outbox (created, kind, payload, subject_id, subject_kind)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        now.to_rfc3339(),
                        part.kind(),
                        serde_json::to_string(&part)?,
                        part.subject().id(),
                        part.subject().kind(),
                    ],
                )?;
```

2. `Store::is_pending` — it asks about a task, so it says so:

```rust
                "SELECT exists(SELECT 1 FROM outbox
                                WHERE subject_id = ?1 AND subject_kind = 'task')",
```

3. `retain_tasks` in `crates/tui-do-core/src/store/tasks.rs` — find the
   `id NOT IN (SELECT subject_id FROM outbox WHERE subject_id IS NOT NULL)` guard and add
   `AND subject_kind = 'task'` inside the subselect.

4. `settle_create`'s lookup of entries still naming the provisional id:

```rust
                    tx.prepare(
                        "SELECT id, payload FROM outbox
                          WHERE subject_id = ?1 AND subject_kind = 'task'",
                    )?;
```

   and its `UPDATE outbox SET payload = ?1, subject_id = ?2 WHERE id = ?3` stays as it is —
   the kind does not change when a task is adopted.

5. `Sync::push`'s `blocked` set and its rejection filter, both in `sync/mod.rs`. Change the
   set's type to `HashSet<Subject>` and leave the comparisons alone; they compare whole
   subjects now. `SyncEvent::Rejected.subject` becomes a `Subject`, and every construction
   and match of it follows.

- [ ] **Step 7: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS, including the new test. Nothing else should have changed behaviour; if a
test now fails, the change was not mechanical and the difference is the thing to explain.

- [ ] **Step 8: Clippy and format**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/tui-do-core crates/tui-do-ui crates/tui-do
git commit -F - <<'EOF'
Say what kind of thing a queued change acts on, before two kinds exist

`outbox.subject_id` is a bare INTEGER, and provisional ids count down
from -1. With only tasks queueable that was unambiguous. Labels are
about to be queueable, and a per-kind counter makes task -1 and label -1
the same value in the same column -- where `retain_tasks` would spare a
task whose id matched a queued label, and `settle_create` would rewrite
a label entry's payload with a TaskId. Neither fails loudly.

So the subject is a `Subject { Task, Label }` and the column has a kind
beside it. No behaviour changes here; every existing test still passes,
which is the point of doing it on its own.
EOF
```

---

### Task 2: `CreateLabel`, and a provisional id it cannot share

**Files:**
- Modify: `crates/tui-do-core/src/store/outbox.rs`
- Modify: `crates/tui-do-ui/src/update.rs` (`edit`, for the `inverse` signature change)
- Test: `crates/tui-do-core/src/store/outbox.rs` test module

**Interfaces:**
- Consumes: `Subject` from Task 1.
- Produces:
  ```rust
  Mutation::CreateLabel { label: Box<Label> }
  pub fn is_provisional_label(label: LabelId) -> bool;
  impl Mutation { pub fn inverse(&self) -> Option<Self>; }   // was -> Self
  ```

**Why `inverse` becomes an `Option`:** the decision is create, rename and recolour with
**no delete**, so a create has no inverse to queue. `edit` in `update.rs` pushes
`mutation.inverse()` onto the undo stack; with `None` it pushes nothing and `u` reaches
past the create to whatever came before it, which is the honest behaviour — a label the
server has made is not something `u` can take back.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_created_label_gets_a_provisional_id_of_its_own() {
    // Its own counter, not the task counter: the kind column tells them apart, and
    // sharing one would make the two id spaces depend on each other for no gain.
    let store = store();
    let first = block_on(store.queue(Mutation::CreateLabel {
        label: Box::new(Label { title: "next".into(), ..Default::default() }),
    }))
    .unwrap();
    let Mutation::CreateLabel { label } = &first.mutation else {
        panic!("queue changed the mutation kind");
    };
    assert_eq!(label.id, LabelId(-1));
    assert!(is_provisional_label(label.id));
    assert_eq!(first.mutation.subject(), Subject::Label(LabelId(-1)));

    // And a task queued after it still gets -1 of its own.
    let task_entry = block_on(store.queue(Mutation::CreateTask {
        task: Box::new(task(0, "unrelated")),
    }))
    .unwrap();
    assert_eq!(task_entry.mutation.subject(), Subject::Task(TaskId(-1)));
}

#[test]
fn queueing_a_label_writes_it_to_the_store_immediately() {
    // Rule 5: the local store is changed at the keystroke, not when the server answers.
    let store = store();
    block_on(store.queue(Mutation::CreateLabel {
        label: Box::new(Label { title: "next".into(), hex_color: "4287f5".into(), ..Default::default() }),
    }))
    .unwrap();
    let labels = block_on(store.labels(LabelFilter::default(), LabelSort::default())).unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].title, "next");
}

#[test]
fn a_create_cannot_be_undone_because_there_is_no_delete() {
    let mutation = Mutation::CreateLabel {
        label: Box::new(Label { title: "next".into(), ..Default::default() }),
    };
    assert!(mutation.inverse().is_none());
}
```

Use whatever the module's existing label helper is; if there is none, construct `Label`
literally as above.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p tui-do-core --lib outbox::tests`
Expected: FAIL — `no variant named CreateLabel`.

- [ ] **Step 3: Add the variant and its five arms**

```rust
    /// A label the server has not seen. `label.id` is provisional until it answers.
    ///
    /// Only ever queued by the interface, which is the half that knows which labels
    /// exist -- `decompose` splits a task write into requests and has no idea.
    CreateLabel {
        /// The label as typed.
        label: Box<Label>,
    },
```

- `kind()` → `"create_label"`.
- `subject()` → `Subject::Label(label.id)`.
- `retarget` → leave it alone; it retargets *tasks*, and Task 3 adds the label
  equivalent as a separate method so the two cannot be confused.
- `inverse()` → `None` (see step 5).
- `apply()` → `upsert_label(tx, label, now)?;`
- `rollback()` → `tx.execute("DELETE FROM labels WHERE id = ?1", params![label.id.get()])?;`
- `decompose()` → falls through to the `other => vec![other]` arm; no change needed.

- [ ] **Step 4: Give labels their own provisional counter**

Beside `next_local_id`:

```rust
/// The state key holding the next provisional label id.
const NEXT_LOCAL_LABEL_ID: &str = "next_local_label_id";

/// Allocate the next provisional label id: -1, then -2, and so on.
///
/// A counter of its own rather than a share of the task counter. `outbox.subject_kind`
/// already tells the two apart, so there is nothing to gain by coupling them, and a
/// shared counter would mean a label create consuming an id a task create then skips --
/// harmless, and confusing to read in the queue.
fn next_local_label_id(tx: &Transaction<'_>) -> Result<LabelId> {
    let next: i64 = read_state(tx, NEXT_LOCAL_LABEL_ID)?
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(-1);
    write_state(tx, NEXT_LOCAL_LABEL_ID, &(next - 1).to_string())?;
    Ok(LabelId(next))
}

/// Whether a label id was assigned locally and the server has never seen it.
#[must_use]
pub fn is_provisional_label(label: LabelId) -> bool {
    label.get() < 0
}
```

And in `Store::queue`, beside the `CreateTask` id assignment:

```rust
            if let Mutation::CreateLabel { label } = &mut mutation {
                if label.id.get() == 0 {
                    label.id = next_local_label_id(tx)?;
                }
            }
```

- [ ] **Step 5: Make `inverse` fallible**

```rust
    /// What undoing this would be, when it can be undone.
    ///
    /// `None` for [`Self::CreateLabel`]: undoing it means deleting a label, and a delete
    /// is the one label operation `u` cannot honestly reverse -- the label would come
    /// back with a new id, detached from everything it was on. So a create is not pushed
    /// onto the undo stack at all and `u` reaches past it.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
```

Wrap every existing arm's value in `Some(...)` and add `Self::CreateLabel { .. } => None`.
Then in `crates/tui-do-ui/src/update.rs`:

```rust
fn edit(model: &mut Model, mutation: Mutation) -> Vec<Effect> {
    // A mutation with no inverse -- creating a label -- is simply not remembered, so `u`
    // reaches past it to the last change that can be taken back.
    if let Some(back) = mutation.inverse() {
        model.undo.push(back);
    }
    model.redo.clear();
    apply(model, mutation)
}
```

Fix the other `inverse()` call sites the compiler finds (the undo and redo handlers) the
same way: skip what cannot be inverted rather than unwrapping.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p tui-do-core --lib outbox::tests && cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/tui-do-core crates/tui-do-ui
git commit -F - <<'EOF'
Queue a label the server has not seen yet
EOF
```

---

### Task 3: Adoption — swapping a provisional label id for the server's

**Files:**
- Modify: `crates/tui-do-core/src/store/outbox.rs` (`retarget_label`, `settle_create_label`)
- Test: same module

**Interfaces:**
- Consumes: `Subject`, `CreateLabel`, `is_provisional_label`.
- Produces:
  ```rust
  impl Mutation { pub fn retarget_label(&mut self, from: LabelId, to: LabelId); }
  impl Store {
      pub async fn settle_create_label(&self, entry: i64, provisional: LabelId, assigned: Label) -> Result<()>;
  }
  ```

A label has **more** holders than a task: the `labels` row, every `task_labels` row naming
it, and the whole `Label` value carried inside queued `AttachLabel` and `DetachLabel`
entries — whose subject is the *task*, so a subject-keyed lookup will not find them.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn adopting_a_label_moves_everything_that_named_it() {
    let store = store();
    let created = block_on(store.queue(Mutation::CreateLabel {
        label: Box::new(Label { id: LabelId(0), title: "next".into(), ..Default::default() }),
    }))
    .unwrap();
    let provisional = LabelId(-1);

    // An attach queued behind it, whose subject is the task and whose payload carries
    // the provisional label by value. This is the entry a subject lookup cannot find.
    block_on(store.queue(Mutation::AttachLabel {
        task: TaskId(7),
        label: Box::new(Label { id: provisional, title: "next".into(), ..Default::default() }),
    }))
    .unwrap();

    block_on(store.settle_create_label(
        created.id,
        provisional,
        Label { id: LabelId(41), title: "next".into(), ..Default::default() },
    ))
    .unwrap();

    let queued = block_on(store.pending(None)).unwrap();
    assert_eq!(queued.len(), 1, "the create should have been dropped");
    let Mutation::AttachLabel { label, .. } = &queued[0].mutation else {
        panic!("the attach is gone");
    };
    assert_eq!(label.id, LabelId(41), "the attach still names the provisional label");

    let labels = block_on(store.labels(LabelFilter::default(), LabelSort::default())).unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].id, LabelId(41));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p tui-do-core --lib adopting_a_label_moves_everything`
Expected: FAIL — `no method named settle_create_label`.

- [ ] **Step 3: Add `retarget_label`**

```rust
    /// Point this mutation at a different label id.
    ///
    /// Separate from [`Self::retarget`] rather than a second argument to it: the two id
    /// spaces are unrelated and a single method taking both would be one typo away from
    /// renumbering a task with a label's id.
    pub fn retarget_label(&mut self, from: LabelId, to: LabelId) {
        let swap = |id: &mut LabelId| {
            if *id == from {
                *id = to;
            }
        };
        match self {
            Self::CreateLabel { label } => swap(&mut label.id),
            Self::AttachLabel { label, .. } | Self::DetachLabel { label, .. } => swap(&mut label.id),
            Self::CreateTask { task } => {
                for label in &mut task.labels {
                    swap(&mut label.id);
                }
            }
            Self::UpdateTask { before, after } => {
                for label in before.labels.iter_mut().chain(after.labels.iter_mut()) {
                    swap(&mut label.id);
                }
            }
            Self::DeleteTask { before } => {
                for label in &mut before.labels {
                    swap(&mut label.id);
                }
            }
        }
    }
```

(`UpdateLabel` gains an arm in Task 5; the compiler will say so.)

- [ ] **Step 4: Add `settle_create_label`**

```rust
    /// Replace a locally created label with the one the server assigned an id to.
    ///
    /// The same order as [`Self::settle_create`], and for the same reason: the server's
    /// row goes in first, `task_labels` is re-pointed at it, and only then is the
    /// provisional row deleted, because SQLite cascades a delete but not an update.
    ///
    /// Unlike a task, the entries that have to move are **not** found by subject: an
    /// `AttachLabel` queued behind this create has the *task* as its subject and carries
    /// the label by value inside its payload. So every queued entry is re-encoded.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure, or
    /// [`crate::CoreError::Encoding`] if a queued payload cannot be re-encoded.
    pub async fn settle_create_label(
        &self,
        entry: i64,
        provisional: LabelId,
        assigned: Label,
    ) -> Result<()> {
        let now = Utc::now();
        self.write(move |tx| {
            upsert_label(tx, &assigned, now)?;
            tx.execute(
                "UPDATE OR IGNORE task_labels SET label_id = ?1 WHERE label_id = ?2",
                params![assigned.id.get(), provisional.get()],
            )?;
            tx.execute("DELETE FROM labels WHERE id = ?1", params![provisional.get()])?;
            tx.execute("DELETE FROM outbox WHERE id = ?1", params![entry])?;

            let queued: Vec<(i64, String)> = {
                let mut statement = tx.prepare("SELECT id, payload FROM outbox")?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut all = Vec::new();
                for row in rows {
                    all.push(row?);
                }
                all
            };
            for (id, payload) in queued {
                let mut mutation: Mutation = serde_json::from_str(&payload)?;
                mutation.retarget_label(provisional, assigned.id);
                tx.execute(
                    "UPDATE outbox SET payload = ?1, subject_id = ?2 WHERE id = ?3",
                    params![
                        serde_json::to_string(&mutation)?,
                        mutation.subject().id(),
                        id
                    ],
                )?;
            }
            Ok(())
        })
        .await
    }
```

Note `subject_id` is rewritten too: a queued `UpdateLabel` for the label being adopted has
the label as its subject, and it must follow.

- [ ] **Step 5: Run the test**

Run: `cargo test -p tui-do-core --lib adopting_a_label_moves_everything`
Expected: PASS.

- [ ] **Step 6: Prove the payload rewrite is load-bearing**

Temporarily delete the `for (id, payload) in queued` loop, re-run the test, and confirm it
fails with the attach still naming `LabelId(-1)`. Restore it. This is the project's
standard: every fix has a test that was watched to fail without it.

- [ ] **Step 7: Run the workspace suite and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add crates/tui-do-core
git commit -F - <<'EOF'
Move everything that named a label when the server names it
EOF
```

---

### Task 4: Sending a create, and the read that stops a retry duplicating it

**Files:**
- Modify: `crates/tui-do-api/src/client.rs` (`labels_named`)
- Modify: `crates/tui-do-core/src/sync/mod.rs` (`Sent`, `transmit`, `deliver`, `SyncEvent::Adopted`)
- Test: `crates/tui-do-api/tests/client.rs`, `crates/tui-do-core/tests/sync.rs`

**Interfaces:**
- Consumes: `settle_create_label`, `Subject`.
- Produces:
  ```rust
  impl Client { pub async fn labels_named(&self, title: &str) -> Result<Vec<Label>>; }
  enum Sent { ..., LabelCreated(Box<Label>) }
  SyncEvent::Adopted { provisional: Subject, assigned: Subject }
  ```

**The finding this task exists for:** label titles are not unique and `PUT /labels` ignores
the body's id, so a replayed create answers `201` and a *second* label, with nothing in the
response to distinguish it. `is_already_done` cannot help. What replaces it: an entry whose
`attempts > 0` reads first.

- [ ] **Step 1: Write the failing API test**

In `crates/tui-do-api/tests/client.rs`:

```rust
#[tokio::test]
async fn labels_can_be_looked_up_by_title() {
    // What a retried create reconciles against. `s` is Vikunja's search parameter and it
    // matches substrings, so the caller compares titles exactly -- asking for "next"
    // must not adopt "next week".
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/labels"))
        .and(query_param("s", "next"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "1")
                .set_body_json(vec![
                    json!({"id": 41, "title": "next"}),
                    json!({"id": 42, "title": "next week"}),
                ]),
        )
        .expect(1)
        .mount(&server)
        .await;

    let found = client(&server).labels_named("next").await.expect("search");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, tui_do_api::models::LabelId(41));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p tui-do-api --test client labels_can_be_looked_up_by_title`
Expected: FAIL — `no method named labels_named`.

- [ ] **Step 3: Implement `labels_named`**

In `client.rs`, beside `all_labels`:

```rust
    /// Every label whose title is exactly `title`.
    ///
    /// `GET /labels?s=` searches, and it matches substrings, so the exact comparison
    /// happens here: a retried `CreateLabel` uses this to find the label its lost
    /// response created, and adopting "next week" when the user asked for "next" would be
    /// worse than the duplicate it is avoiding.
    ///
    /// Case-insensitive, matching `resolve_labels` in the interface, because Vikunja will
    /// happily hold `Next` and `next` and the user means one thing by them.
    ///
    /// # Errors
    /// Any failure from any page.
    pub async fn labels_named(&self, title: &str) -> Result<Vec<Label>> {
        self.require_auth("searching labels")?;
        let call = Call::new(Method::GET, self.resolve(endpoints::LABELS, &[])?)
            .with_query("s", title);
        let found = Pager::new(self.clone(), call, self.page_size())
            .collect_all()
            .await?;
        Ok(found
            .into_iter()
            .filter(|label| label.title.eq_ignore_ascii_case(title))
            .collect())
    }
```

`Call::with_query(name, value)` takes one pair at a time and is `pub(crate)`, which is
why this lives on `Client` rather than being assembled by a caller.

- [ ] **Step 4: Run the API test**

Run: `cargo test -p tui-do-api --test client labels_can_be_looked_up_by_title`
Expected: PASS.

- [ ] **Step 5: Write the failing sync test**

In `crates/tui-do-core/tests/sync.rs`. That file already has the scaffolding: `engine(&server, &store)`
returns `(Sync, UnboundedReceiver<SyncEvent>)` over a store the test owns, `mount_task_read`
shows how a read-before-write is mocked, and `events(&mut rx)` drains what was emitted.
There is **no** `Sync::store()` — the test holds the `Store` itself and queues through it.

```rust
#[tokio::test]
async fn a_retried_label_create_adopts_the_one_the_lost_response_made() {
    // Measured on dev 2026-08-29: creating the same title twice answers 201 twice with
    // two different ids, and nothing in the response says which. So a create that has
    // already failed once looks before it writes -- and finding its own earlier attempt
    // is the whole point.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/labels"))
        .and(query_param("s", "next"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "1")
                .set_body_json(vec![json!({"id": 41, "title": "next"})]),
        )
        .expect(1)
        .mount(&server)
        .await;
    // No PUT mock at all: a second create is a test failure, not a fallback.

    let store = store();
    let (sync, mut _rx) = engine(&server, &store);
    let entry = store
        .queue(Mutation::CreateLabel {
            label: Box::new(Label { title: "next".into(), ..Default::default() }),
        })
        .await
        .unwrap();
    store
        .defer(entry.id, "connection reset".into(), None)
        .await
        .unwrap();
    // Past the backoff, so the drain will pick it up.
    store.clear_backoff_for_test(entry.id).await.unwrap();

    let report = sync.push().await.unwrap();
    assert_eq!(report.sent, 1);
    let labels = store.labels(LabelFilter::default(), LabelSort::default()).await.unwrap();
    assert_eq!(labels[0].id, LabelId(41));
}
```

If the store has no test helper for clearing a backoff, add one behind `#[cfg(test)]` in
`outbox.rs` rather than sleeping — the suite must not wait five seconds.

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test -p tui-do-core --lib a_retried_label_create_adopts`
Expected: FAIL — no `CreateLabel` arm in `transmit`.

- [ ] **Step 7: Implement the transmit arm**

Add to `Sent`:

```rust
    /// A label was created, and this is the server's copy of it.
    LabelCreated(Box<Label>),
```

In `transmit`:

```rust
            Mutation::CreateLabel { label } => {
                // A create that has already failed once reads before it writes. Titles
                // are not unique and `PUT /labels` ignores the body's id, so a replayed
                // create answers 201 and a second label with nothing to tell it from the
                // first -- `is_already_done` has nothing to match on, and deliberately
                // has no arm for this. Measured on dev 2026-08-29.
                //
                // This protects against *our own* retry, not against two boxes creating
                // the same label at the same moment. Nothing can protect against that
                // without a unique constraint the server does not have.
                if entry.attempts > 0 {
                    if let Some(existing) = self
                        .client
                        .labels_named(&label.title)
                        .await?
                        .into_iter()
                        .next()
                    {
                        return Ok(Sent::LabelCreated(Box::new(existing)));
                    }
                }
                Sent::LabelCreated(Box::new(self.client.create_label(label).await?))
            }
```

`transmit` takes `&entry` already, so `entry.attempts` is in scope; if it takes only the
mutation, widen it to the entry rather than threading a bare count.

- [ ] **Step 8: Settle it in `deliver`**

```rust
            (Sent::LabelCreated(assigned), Mutation::CreateLabel { label }) => {
                let (provisional, named) = (label.id, assigned.id);
                self.store
                    .settle_create_label(entry.id, provisional, *assigned)
                    .await?;
                self.emit(SyncEvent::Adopted {
                    provisional: Subject::Label(provisional),
                    assigned: Subject::Label(named),
                });
            }
```

Change `SyncEvent::Adopted`'s two fields to `Subject` and update the task arm to wrap in
`Subject::Task(...)`. The doc comment on the variant already explains why every holder has
to move together; extend it to say that a label has more holders than a task, and name the
open `Modal::Labels` as the one most easily missed.

- [ ] **Step 9: Run both tests, then the workspace**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 10: Prove the reconcile is load-bearing**

Change `if entry.attempts > 0` to `if false`, re-run the sync test, and confirm it fails
because the unmocked `PUT` is called. Restore it.

- [ ] **Step 11: Commit**

```bash
git add crates/tui-do-api crates/tui-do-core
git commit -F - <<'EOF'
Look before retrying a label create, because a replay is undetectable
EOF
```

---

### Task 5: `UpdateLabel` — rename and recolour, merged before it is sent

**Files:**
- Modify: `crates/tui-do-api/src/models/label.rs` (`Label::merge_onto`)
- Modify: `crates/tui-do-core/src/store/outbox.rs` (the variant)
- Modify: `crates/tui-do-core/src/sync/mod.rs` (`transmit`, `is_already_done`)
- Test: all three

**Interfaces:**
- Consumes: `Subject`, `retarget_label`.
- Produces:
  ```rust
  Mutation::UpdateLabel { before: Box<Label>, after: Box<Label> }
  impl Label { pub fn merge_onto(&self, before: &Self, server: Self) -> LabelMerge; }
  pub struct LabelMerge { pub label: Label, pub collisions: Vec<&'static str> }
  ```

One variant covers rename, recolour and description, because they are one request and one
undo step. `UpdateLabel`, not `RenameLabel`, for that reason.

- [ ] **Step 1: Write the failing merge test**

In `crates/tui-do-api/src/models/label.rs`:

```rust
#[test]
fn a_rename_keeps_a_colour_someone_else_changed() {
    // The same three-way merge as `Task::merge_onto`, and for the same reason: Vikunja
    // has no conditional write, a partial body clears what it omits, and tui-do is
    // multi-instance. Without this, box B's rename reverts box A's recolour.
    let before = Label { id: LabelId(41), title: "next".into(), hex_color: "aaaaaa".into(), ..Default::default() };
    let after = Label { title: "next up".into(), ..before.clone() };
    let server = Label { hex_color: "4287f5".into(), ..before.clone() };

    let merged = after.merge_onto(&before, server);
    assert_eq!(merged.label.title, "next up");
    assert_eq!(merged.label.hex_color, "4287f5");
    assert!(merged.collisions.is_empty());
}

#[test]
fn a_true_collision_is_named_and_the_users_value_wins() {
    let before = Label { id: LabelId(41), title: "next".into(), ..Default::default() };
    let after = Label { title: "next up".into(), ..before.clone() };
    let server = Label { title: "upcoming".into(), ..before.clone() };

    let merged = after.merge_onto(&before, server);
    assert_eq!(merged.label.title, "next up");
    assert_eq!(merged.collisions, vec!["title"]);
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p tui-do-api --lib label::tests`
Expected: FAIL — `no method named merge_onto`.

- [ ] **Step 3: Implement `merge_onto`**

Mirror `Task::merge_onto` exactly, including the **exhaustive destructure**:

```rust
/// The result of replaying a label edit onto the server's copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelMerge {
    /// What to send.
    pub label: Label,
    /// Fields the user's value was written over, because both sides changed them.
    pub collisions: Vec<&'static str>,
}

impl Label {
    /// Replay this edit onto the server's current copy.
    ///
    /// `self` is what the user wants, `before` what they started from, `server` what the
    /// server holds now. A field the user changed takes their value; everything else
    /// keeps the server's. On a true collision -- both changed it -- the user's value
    /// wins and the field is named, because they are the one sitting there and refusing
    /// would lose what they just typed.
    ///
    /// `server` is destructured exhaustively on purpose: a new field on `Label` fails to
    /// compile here rather than silently keeping the server's value, which is how a field
    /// quietly becomes uneditable.
    #[must_use]
    pub fn merge_onto(&self, before: &Self, server: Self) -> LabelMerge {
        let Self {
            id,
            title,
            description,
            hex_color,
            created_by,
            created,
            updated,
        } = server;
        let mut collisions = Vec::new();
        let mut take = |name: &'static str, mine: &String, was: &String, theirs: String| {
            if mine == was {
                return theirs;
            }
            if theirs != *was {
                collisions.push(name);
            }
            mine.clone()
        };
        let merged = Self {
            id,
            title: take("title", &self.title, &before.title, title),
            description: take("description", &self.description, &before.description, description),
            hex_color: take("hex_color", &self.hex_color, &before.hex_color, hex_color),
            created_by,
            created,
            updated,
        };
        LabelMerge { label: merged, collisions }
    }
}
```

Note `id` comes from the server's copy, not the user's: the body's `id` beats the path, and
this is the one place that could send a mismatched pair.

- [ ] **Step 4: Add the mutation variant**

```rust
    /// A change to an existing label -- its title, its colour or its description.
    ///
    /// One variant for all three because they are one request and one undo step.
    UpdateLabel {
        /// The label as it was.
        before: Box<Label>,
        /// The label as it should be.
        after: Box<Label>,
    },
```

- `kind()` → `"update_label"`; `subject()` → `Subject::Label(after.id)`.
- `inverse()` → `Some(Self::UpdateLabel { before: after.clone(), after: before.clone() })`.
- `apply()` → `upsert_label(tx, after, now)?;`
- `rollback()` → `upsert_label(tx, before, now)?;`
- `retarget_label()` → swap both `before.id` and `after.id`.

- [ ] **Step 5: Write the failing transmit test**

```rust
#[tokio::test]
async fn a_label_update_replays_the_edit_onto_the_servers_copy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/labels/41"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id": 41, "title": "next", "hex_color": "4287f5"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/labels/41"))
        .and(body_partial_json(json!({"title": "next up", "hex_color": "4287f5"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id": 41, "title": "next up", "hex_color": "4287f5"}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let store = store();
    let (sync, mut _rx) = engine(&server, &store);
    store
        .queue(Mutation::UpdateLabel {
            before: Box::new(Label { id: LabelId(41), title: "next".into(), hex_color: "aaaaaa".into(), ..Default::default() }),
            after: Box::new(Label { id: LabelId(41), title: "next up".into(), hex_color: "aaaaaa".into(), ..Default::default() }),
        })
        .await
        .unwrap();
    assert_eq!(sync.push().await.unwrap().sent, 1);
}
```

The `body_partial_json` on `hex_color` is the assertion that matters: the local `before`
and `after` both say `aaaaaa`, and the request must carry the server's `4287f5`.

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test -p tui-do-core --lib a_label_update_replays`
Expected: FAIL — no `UpdateLabel` arm in `transmit`.

- [ ] **Step 7: Implement the transmit arm**

```rust
            Mutation::UpdateLabel { before, after } => {
                // Read, merge, write -- the same as `UpdateTask` and for the same
                // reasons. A partial body clears what it omits (measured: a body carrying
                // only `title` cleared `hex_color` to ""), and there is no conditional
                // write to ask for, so a stale body reverts whatever another box changed.
                let current = self.client.label(after.id).await?;
                let merged = after.merge_onto(before, current);
                if !merged.collisions.is_empty() {
                    self.emit(SyncEvent::Overwrote {
                        subject: Subject::Label(after.id),
                        fields: merged.collisions.iter().map(|f| (*f).to_string()).collect(),
                    });
                }
                Sent::LabelUpdated(Box::new(self.client.update_label(&merged.label).await?))
            }
```

`SyncEvent::Overwrote.subject` becomes a `Subject` alongside `Rejected`'s. Add
`Client::label(id)` (`GET /labels/{id}`) if it does not exist — the path is already in
`endpoints.rs` as `LABEL`.

`Sent::LabelUpdated` settles through `deliver`'s catch-all `_ => self.store.complete(...)`
unless you want the server's copy stored; store it, the way `Sent::Updated` does, guarded
by "nothing else queued for this label".

- [ ] **Step 8: Add the two `is_already_done` arms**

```rust
    /// Vikunja's code for "this label does not exist".
    const LABEL_GONE: i64 = 8002;
```

and, in the `matches!`:

```rust
            | (
                Mutation::UpdateLabel { .. },
                ApiError::Rejected { status: 404, code: Some(LABEL_GONE), .. },
            )
```

Extend the doc comment's table with the two rows measured on 2026-08-29 (rename and delete
of a gone label both answer `404`/`8002`) and state explicitly that **`CreateLabel` has no
arm and cannot have one**, with the reason — otherwise the next reader adds it.

Each arm gets a test that fails when the arm is removed.

- [ ] **Step 9: Run the workspace suite, clippy, commit**

```bash
git add crates/tui-do-api crates/tui-do-core
git commit -F - <<'EOF'
Rename and recolour a label, merged onto the server's copy first
EOF
```

---

### Task 6: A rejected create takes its attaches with it

**Files:**
- Modify: `crates/tui-do-core/src/sync/mod.rs` (`push`'s rejection branch)
- Test: same module

**Interfaces:**
- Consumes: `Subject`, `CreateLabel`.
- Produces: `fn references(mutation: &Mutation, label: LabelId) -> bool` (private).

`push` discards everything queued **for the same subject** on a 4xx. A rejected
`CreateLabel`'s dependants are `AttachLabel` entries whose subject is the *task*, so
same-subject filtering will not find them, and leaving them queued sends an attach for a
label id no server has ever seen.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_refused_label_create_takes_the_attaches_that_named_it() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/labels"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"code": 4001, "message": "invalid"}),
        ))
        .mount(&server)
        .await;

    let store = store();
    let (sync, mut _rx) = engine(&server, &store);
    store
        .queue(Mutation::CreateLabel {
            label: Box::new(Label { title: "next".into(), ..Default::default() }),
        })
        .await
        .unwrap();
    store
        .queue(Mutation::AttachLabel {
            task: TaskId(7),
            label: Box::new(Label { id: LabelId(-1), title: "next".into(), ..Default::default() }),
        })
        .await
        .unwrap();

    sync.push().await.unwrap();
    assert!(
        store.pending(None).await.unwrap().is_empty(),
        "an attach naming a label the server refused to create is still queued"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p tui-do-core --lib a_refused_label_create_takes`
Expected: FAIL — the attach survives.

- [ ] **Step 3: Widen the discard**

```rust
/// Whether this mutation would send an id that a rejected create was going to define.
///
/// A rejected `CreateLabel` is the one case where "everything queued for the same
/// subject" is not enough: an `AttachLabel` built on it has the *task* as its subject and
/// carries the label by value, so a subject filter cannot see it -- and sending it means
/// asking the server to attach a label id it has never issued.
fn references(mutation: &Mutation, label: LabelId) -> bool {
    match mutation {
        Mutation::AttachLabel { label: carried, .. }
        | Mutation::DetachLabel { label: carried, .. } => carried.id == label,
        Mutation::UpdateLabel { after, .. } => after.id == label,
        Mutation::CreateTask { task } => task.labels.iter().any(|l| l.id == label),
        Mutation::UpdateTask { after, .. } => after.labels.iter().any(|l| l.id == label),
        Mutation::CreateLabel { .. } | Mutation::DeleteTask { .. } => false,
    }
}
```

and in the rejection branch:

```rust
                    let doomed: Vec<OutboxEntry> = pending
                        .into_iter()
                        .filter(|queued| {
                            queued.mutation.subject() == subject
                                || matches!(subject, Subject::Label(id) if references(&queued.mutation, id))
                        })
                        .collect();
```

- [ ] **Step 4: Run it, then remove the `||` clause and watch it fail again, then restore**

- [ ] **Step 5: Run the workspace suite and commit**

```bash
git add crates/tui-do-core
git commit -F - <<'EOF'
Take the attaches down with the label create the server refused
EOF
```

---

### Task 7: The interface adopts a label

**Files:**
- Modify: `crates/tui-do-ui/src/update.rs` (`adopt`)
- Modify: `crates/tui-do-ui/src/msg.rs` if `Msg` mirrors `SyncEvent::Adopted`
- Test: `crates/tui-do-ui/src/update.rs` or `crates/tui-do-ui/tests/update.rs`

**Interfaces:**
- Consumes: `SyncEvent::Adopted { provisional: Subject, assigned: Subject }`.
- Produces: `fn adopt_label(model: &mut Model, provisional: LabelId, assigned: LabelId) -> Vec<Effect>`.

A label's holders in the model: `model.data.labels`, the `labels` vector inside **every**
`Task` in `model.data.tasks`, the undo and redo stacks, and an open `Modal::Labels`, whose
`LabelsState` holds cloned labels and a `chosen: Vec<LabelId>` — and which is on screen at
exactly the moment a label was just created.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn adopting_a_label_reaches_the_open_form_that_created_it() {
    // The form is open at exactly the moment this happens -- the user created the label
    // from it -- and it holds cloned labels and a list of ticked ids. Leaving it behind
    // means the next tick sends an id no server has seen.
    let mut model = model_with_a_task_carrying(LabelId(-1));
    model.modals.push(Modal::Labels(LabelsState::new(
        vec![Label { id: LabelId(-1), title: "next".into(), ..Default::default() }],
        vec![LabelId(-1)],
    )));

    let _ = update(&mut model, Msg::Adopted {
        provisional: Subject::Label(LabelId(-1)),
        assigned: Subject::Label(LabelId(41)),
    });

    assert!(model.data.labels.iter().all(|l| l.id == LabelId(41)));
    assert!(model.data.tasks[0].labels.iter().all(|l| l.id == LabelId(41)));
    let Some(Modal::Labels(state)) = model.modals.last() else {
        panic!("the form closed");
    };
    assert_eq!(state.chosen, vec![LabelId(41)]);
    assert_eq!(state.labels[0].id, LabelId(41));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p tui-do-ui adopting_a_label_reaches_the_open_form`
Expected: FAIL.

- [ ] **Step 3: Split `adopt` by kind**

```rust
fn adopt(model: &mut Model, provisional: Subject, assigned: Subject) -> Vec<Effect> {
    match (provisional, assigned) {
        (Subject::Task(from), Subject::Task(to)) => adopt_task(model, from, to),
        (Subject::Label(from), Subject::Label(to)) => adopt_label(model, from, to),
        // A create is answered by a create of the same kind. A mismatch is a bug in the
        // sync engine, and renumbering something at random would hide it.
        _ => Vec::new(),
    }
}
```

`adopt_task` is today's body, unchanged. `adopt_label`:

```rust
/// Swap a provisional label id for the one the server gave it.
///
/// A label has more holders than a task, and the one most easily missed is the open
/// `Modal::Labels` -- which is on screen precisely when this happens, because that is
/// where the label was created.
fn adopt_label(model: &mut Model, provisional: LabelId, assigned: LabelId) -> Vec<Effect> {
    let swap = |id: &mut LabelId| {
        if *id == provisional {
            *id = assigned;
        }
    };
    for label in &mut model.data.labels {
        swap(&mut label.id);
    }
    for task in &mut model.data.tasks {
        for label in &mut task.labels {
            swap(&mut label.id);
        }
    }
    for mutation in model.undo.iter_mut().chain(model.redo.iter_mut()) {
        mutation.retarget_label(provisional, assigned);
    }
    if let Some(Modal::Labels(state)) = model.modals.last_mut() {
        for label in &mut state.labels {
            swap(&mut label.id);
        }
        for id in &mut state.chosen {
            swap(id);
        }
    }
    // The store now holds the server's own copy of the row.
    reload_labels_and_tasks(model)
}
```

Use whatever the crate's existing reload helper is; if only `reload_tasks` exists, return
`[Effect::LoadLabels]` alongside it.

- [ ] **Step 4: Run the test, then delete the `Modal::Labels` block and watch it fail again**

- [ ] **Step 5: Workspace suite, clippy, commit**

```bash
git add crates/tui-do-ui
git commit -F - <<'EOF'
Renumber a label everywhere the interface is holding it
EOF
```

---

### Task 8: Creating from the `l` form

**Files:**
- Modify: `crates/tui-do-ui/src/modal.rs` (`LabelsState`, `Submission`)
- Modify: `crates/tui-do-ui/src/update.rs` (the `Action::Labels` handler and the submission)
- Test: `crates/tui-do-ui/src/modal.rs`, `crates/tui-do-ui/tests/render.rs` (golden)

**Interfaces:**
- Consumes: `Mutation::CreateLabel`.
- Produces: `Submission::CreateLabel(String)`.

Where the user is already looking at the label list and finding it does not have what they
want. The form filters as it is typed; when nothing matches, the footer offers to create
what was typed.

- [ ] **Step 1: Write the failing modal test**

```rust
#[test]
fn a_label_form_with_no_match_offers_to_create_what_was_typed() {
    let mut state = LabelsState::new(
        vec![Label { id: LabelId(1), title: "urgent".into(), ..Default::default() }],
        Vec::new(),
    );
    for c in "next".chars() {
        let _ = state.key(Key::char(c));
    }
    assert!(state.matches.is_empty());
    assert_eq!(state.creatable(), Some("next"));

    // Ctrl-N creates; Enter still ticks, so a fast typist cannot create by reflex.
    assert_eq!(
        state.key(Key::ctrl('n')),
        Outcome::Submit(Submission::CreateLabel("next".into()))
    );
}

#[test]
fn a_form_does_not_offer_to_create_a_label_that_exists() {
    let mut state = LabelsState::new(
        vec![Label { id: LabelId(1), title: "urgent".into(), ..Default::default() }],
        Vec::new(),
    );
    for c in "urgent".chars() {
        let _ = state.key(Key::char(c));
    }
    assert_eq!(state.creatable(), None);
}
```

Match the modal's actual key-handling signature; `Outcome`/`Submission` are in `modal.rs`.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p tui-do-ui --lib modal::tests`
Expected: FAIL — `no method named creatable`.

- [ ] **Step 3: Implement**

```rust
    /// The title this form would create, if the user asked.
    ///
    /// `None` when the box is empty or when a label of that name already exists --
    /// offering to create a duplicate is offering to make the global pool worse, and
    /// Vikunja will happily hold two labels with one name.
    #[must_use]
    pub fn creatable(&self) -> Option<&str> {
        let typed = self.input.text().trim();
        if typed.is_empty() {
            return None;
        }
        if self.labels.iter().any(|l| l.title.eq_ignore_ascii_case(typed)) {
            return None;
        }
        Some(typed)
    }
```

Bind `Ctrl-N` in the form's key handler to `Outcome::Submit(Submission::CreateLabel(...))`
when `creatable()` is `Some`, and render the offer in the form's footer.

- [ ] **Step 4: Handle the submission**

In `update.rs`, where `Submission::Labels` is handled:

```rust
        Submission::CreateLabel(title) => {
            let label = Label { title, ..Default::default() };
            let mut effects = edit(model, Mutation::CreateLabel { label: Box::new(label) });
            model.toast(Toast::info(format!("Created label {title}")));
            effects.push(Effect::LoadLabels);
            effects
        }
```

Also delete the `if labels.is_empty()` early return that says "No labels exist yet" and
open the form instead — with nothing in it, the user types a name and creates one, which
is exactly the case that message existed to apologise for.

- [ ] **Step 5: Run the tests, re-bless the goldens**

Run: `cargo test -p tui-do-ui`, then `TUI_DO_BLESS=1 cargo test -p tui-do-ui --test render`
if a golden screen shows the form's footer. **Read the diff before committing it.**

- [ ] **Step 6: Commit**

```bash
git add crates/tui-do-ui
git commit -F - <<'EOF'
Create a label from the form that just told you it has not got one
EOF
```

---

### Task 9: One form that edits a label, reachable from both places that list them

**Files:**
- Modify: `crates/tui-do-ui/src/modal.rs` (`Modal::LabelEdit`, `LabelEditState`, `Submission::EditedLabel`)
- Modify: `crates/tui-do-ui/src/update.rs` (opening it, and handling the submission)
- Modify: `crates/tui-do-ui/src/keymap.rs` (the binding, so the help modal learns it)
- Modify: `crates/tui-do-ui/src/view.rs` (drawing it)
- Test: `crates/tui-do-ui/src/modal.rs`, `crates/tui-do-ui/tests/update.rs`, the help golden

**Interfaces:**
- Consumes: `Mutation::UpdateLabel`, `Label::merge_onto`.
- Produces:
  ```rust
  Modal::LabelEdit(LabelEditState)
  pub struct LabelEditState { pub label: Label, pub title: TextInput, pub hex: TextInput, pub field: LabelField }
  pub enum LabelField { Title, Colour }
  Submission::EditedLabel { id: LabelId, title: String, hex_color: String }
  ```

Rename and recolour are one request, one undo step and one `UpdateLabel`, so they are one
form with two fields rather than two keys with two prompts. It opens on the highlighted
label from **both** surfaces that list labels — the `l` form from Task 8 and the `g l`
picker — because a label edited from one place must not be a different thing from a label
edited in the other.

- [ ] **Step 1: Write the failing modal test**

```rust
#[test]
fn the_label_form_starts_from_what_the_label_is_now() {
    let state = LabelEditState::new(Label {
        id: LabelId(41),
        title: "next".into(),
        hex_color: "4287f5".into(),
        ..Default::default()
    });
    assert_eq!(state.title.text(), "next");
    assert_eq!(state.hex.text(), "4287f5");
    assert_eq!(state.field, LabelField::Title);
}

#[test]
fn submitting_the_label_form_carries_both_fields() {
    let mut state = LabelEditState::new(Label {
        id: LabelId(41),
        title: "next".into(),
        hex_color: "4287f5".into(),
        ..Default::default()
    });
    for _ in 0.."next".len() {
        let _ = state.key(Key::plain(KeyCode::Backspace));
    }
    for c in "next up".chars() {
        let _ = state.key(Key::char(c));
    }
    assert_eq!(
        state.key(Key::plain(KeyCode::Enter)),
        Outcome::Submit(Submission::EditedLabel {
            id: LabelId(41),
            title: "next up".into(),
            hex_color: "4287f5".into(),
        })
    );
}

#[test]
fn a_colour_that_is_not_six_hex_digits_is_refused_in_the_form() {
    // Rather than queued and rejected by the server minutes later, which rolls the
    // rename back with it.
    let mut state = LabelEditState::new(Label { id: LabelId(41), ..Default::default() });
    let _ = state.key(Key::plain(KeyCode::Tab));
    for c in "nope".chars() {
        let _ = state.key(Key::char(c));
    }
    assert_eq!(state.key(Key::plain(KeyCode::Enter)), Outcome::Consumed);
    assert!(state.error.is_some());
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p tui-do-ui --lib modal::tests`
Expected: FAIL — `LabelEditState` does not exist.

- [ ] **Step 3: Implement the state**

```rust
/// Which field of the label form has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelField {
    /// The label's text.
    Title,
    /// Its six hex digits, without a leading `#`.
    Colour,
}

/// Editing one label's title and colour.
///
/// Both at once because they are one `POST /labels/{id}`, one undo step and one row in
/// the queue -- and because a partial body clears what it omits, so the write carries
/// both whether or not the user touched both.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelEditState {
    /// The label as it was when the form opened, which becomes the mutation's `before`.
    pub label: Label,
    /// The title being typed.
    pub title: TextInput,
    /// The colour being typed.
    pub hex: TextInput,
    /// Which field has the keyboard.
    pub field: LabelField,
    /// Why the last submission was refused, if it was.
    pub error: Option<String>,
}

impl LabelEditState {
    /// A form over `label`, with both fields filled in from it.
    #[must_use]
    pub fn new(label: Label) -> Self {
        Self {
            title: TextInput::from(label.title.as_str()),
            hex: TextInput::from(label.hex_color.as_str()),
            label,
            field: LabelField::Title,
            error: None,
        }
    }

    /// Whether `hex` is something Vikunja will take: six hex digits, or empty for
    /// "the interface picks one".
    #[must_use]
    pub fn colour_is_valid(&self) -> bool {
        let hex = self.hex.text().trim();
        hex.is_empty() || (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
    }
}
```

Follow the `EditState` and `DueState` key handlers for the `key` method: `Tab` moves
between fields, `Esc` dismisses, `Enter` submits, and an invalid colour sets `error` and
returns `Outcome::Consumed` rather than submitting.

- [ ] **Step 4: Open it from both surfaces**

In `update.rs`, in the `l` form's key handling and in the picker's, on the same key:

```rust
        // The same key from both places that list labels, so a label edited from the
        // task form and one edited from `g l` are the same operation.
        Submission::EditLabel(id) => {
            let Some(label) = model.data.labels.iter().find(|l| l.id == id).cloned() else {
                model.toast(Toast::info("That label is not loaded"));
                return Vec::new();
            };
            model.modals.push(Modal::LabelEdit(LabelEditState::new(label)));
            Vec::new()
        }
```

- [ ] **Step 5: Write the failing update test**

```rust
#[test]
fn editing_a_label_queues_one_update_carrying_both_states() {
    let mut model = model_with_labels(vec![Label {
        id: LabelId(41),
        title: "next".into(),
        hex_color: "4287f5".into(),
        ..Default::default()
    }]);
    let effects = update(&mut model, Msg::Submitted(Submission::EditedLabel {
        id: LabelId(41),
        title: "next up".into(),
        hex_color: "e8384f".into(),
    }));

    let Some(Effect::Apply(Mutation::UpdateLabel { before, after })) = effects.first() else {
        panic!("no update queued");
    };
    assert_eq!(before.title, "next");
    assert_eq!(before.hex_color, "4287f5");
    assert_eq!(after.title, "next up");
    assert_eq!(after.hex_color, "e8384f");
    // And it is undoable, unlike a create.
    assert_eq!(model.undo.len(), 1);
}
```

- [ ] **Step 6: Handle the submission**

```rust
        Submission::EditedLabel { id, title, hex_color } => {
            let Some(before) = model.data.labels.iter().find(|l| l.id == id).cloned() else {
                return Vec::new();
            };
            let after = Label { title, hex_color, ..before.clone() };
            if after == before {
                return Vec::new();
            }
            edit(
                model,
                Mutation::UpdateLabel {
                    before: Box::new(before),
                    after: Box::new(after),
                },
            )
        }
```

- [ ] **Step 7: Bind the key in `KEYMAP`**

Add one row with a `doc` string. The help modal is rendered from this table, so a binding
that is not in it does not exist as far as the user is concerned — and the golden that
draws the help modal will change, which is the check that it landed.

- [ ] **Step 8: Run the tests, re-bless the goldens, read the diff**

Run: `cargo test -p tui-do-ui`, then
`TUI_DO_BLESS=1 cargo test -p tui-do-ui --test render`, then `git diff` the golden
directory and confirm the only change is the new help row and the new form.

- [ ] **Step 9: Commit**

```bash
git add crates/tui-do-ui
git commit -F - <<'EOF'
Rename and recolour a label in one form, from either place that lists them
EOF
```

---

### Task 10: `*newlabel` in quick-add asks first

**Files:**
- Modify: `crates/tui-do-ui/src/update.rs` (the quick-add submission and the edit form's Labels field)
- Modify: `crates/tui-do-ui/src/modal.rs` (a confirmation modal variant)
- Test: `crates/tui-do-ui/src/update.rs`

**Interfaces:**
- Consumes: `Mutation::CreateLabel`, `resolve_labels`.
- Produces: `Modal::ConfirmLabels(ConfirmLabelsState)`.

**The decision this implements:** everywhere else the user is looking at a label list and
asking for a new one. In quick-add they typed a *task*, the label pool is global, and a
typo becomes a permanent entry that pollutes autocomplete in every project forever. So an
unknown `*label` prompts before it creates.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn an_unknown_label_in_quick_add_asks_before_it_creates() {
    let mut model = model_with_labels(vec![Label { id: LabelId(1), title: "urgent".into(), ..Default::default() }]);
    let effects = update(&mut model, Msg::Submitted(Submission::Add(
        "Call the VA *urgent *waiting".into(),
    )));

    // Nothing is queued yet: the task waits behind the answer, because creating the task
    // and then attaching a label the user rejected would leave it half done.
    assert!(effects.iter().all(|e| !matches!(e, Effect::Apply(_))));
    let Some(Modal::ConfirmLabels(state)) = model.modals.last() else {
        panic!("no confirmation");
    };
    assert_eq!(state.unknown, vec!["waiting".to_string()]);
}

#[test]
fn confirming_creates_the_label_then_the_task() {
    // Order is the contract: the create is queued first so the attach behind it can be
    // retargeted when the server names the label.
    let mut model = model_awaiting_label_confirmation();
    let effects = update(&mut model, Msg::Submitted(Submission::ConfirmLabels(true)));
    let kinds: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Apply(m) => Some(m.kind()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec!["create_label", "create_task"]);
}

#[test]
fn declining_adds_the_task_without_the_label() {
    let mut model = model_awaiting_label_confirmation();
    let effects = update(&mut model, Msg::Submitted(Submission::ConfirmLabels(false)));
    let kinds: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Apply(m) => Some(m.kind()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec!["create_task"]);
}
```

- [ ] **Step 2: Run and watch them fail. Step 3: implement.**

Add `Modal::ConfirmLabels(ConfirmLabelsState)` — a modal variant, per rule 2, never a bool.
`ConfirmLabelsState` holds the parsed draft and the unknown names, so the answer resumes
exactly the submission that was interrupted:

```rust
/// Waiting for an answer about labels a quick-add asked for and the server has not got.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmLabelsState {
    /// The names that do not exist yet.
    pub unknown: Vec<String>,
    /// The task the user was adding, held until the answer comes.
    pub pending: Box<Task>,
}
```

Queue in the order `CreateLabel` … then `CreateTask` carrying the provisional labels;
`decompose` turns the create's labels into `AttachLabel` entries behind it, and adoption
retargets them. That ordering is the design document's, and it is what makes the attach
unable to outrun the create.

- [ ] **Step 4: Replace the two apology strings** at `update.rs:1190` and the edit form's
  Labels field with the same confirmation path.

- [ ] **Step 5: Run the tests, re-bless goldens, read the diff, commit**

```bash
git add crates/tui-do-ui
git commit -F - <<'EOF'
Ask before a typo in a task line becomes a label forever
EOF
```

---

### Task 11: `tui-do add` stops apologising

**Files:**
- Modify: `crates/tui-do/src/main.rs` (`AddArgs`)
- Modify: `crates/tui-do/src/runtime/mod.rs:729-734`
- Test: the test module at the foot of `crates/tui-do/src/runtime/mod.rs`

**Interfaces:**
- Consumes: `Mutation::CreateLabel`.
- Produces: `AddArgs::create_labels: bool`.

There is no interface to confirm in, so the CLI takes a flag. Silent creation from a
non-interactive command is the same hazard the quick-add confirmation exists to prevent,
and worse for being unattended — `tui-do add` is the thing people put in scripts.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn an_unknown_label_is_left_off_unless_the_flag_says_otherwise() {
    let store = scratch_store().await;
    let built = build_task("Call the VA *waiting", &[], &[]);
    assert_eq!(built.unknown_labels, vec!["waiting".to_string()]);

    queue_add(&store, built.clone(), false).await.unwrap();
    let queued = store.pending(None).await.unwrap();
    assert_eq!(
        queued.iter().map(|e| e.mutation.kind()).collect::<Vec<_>>(),
        vec!["create_task"]
    );
}

#[tokio::test]
async fn the_flag_queues_the_label_before_the_task_that_carries_it() {
    // Order is the contract: the create is queued first so the attach `decompose` puts
    // behind the task can be retargeted when the server names the label.
    let store = scratch_store().await;
    let built = build_task("Call the VA *waiting", &[], &[]);
    queue_add(&store, built, true).await.unwrap();
    let queued = store.pending(None).await.unwrap();
    assert_eq!(
        queued.iter().map(|e| e.mutation.kind()).collect::<Vec<_>>(),
        vec!["create_label", "create_task", "attach_label"]
    );
}
```

Use the module's own helpers for building a task and a scratch store; if `queue_add` does
not exist as a seam, extract it from `run_add` as part of step 3 — `run_add` prints and
syncs, and neither belongs in a test.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p tui-do an_unknown_label_is_left_off`
Expected: FAIL — no `queue_add`, or the wrong kinds queued.

- [ ] **Step 3: Add the flag**

In `main.rs`, beside `offline`:

```rust
    /// Create any label the task names that does not exist yet.
    ///
    /// Off by default: labels are one global pool shared by every project, so a typo in
    /// a task line otherwise becomes a permanent entry that pollutes completion
    /// everywhere. The interface asks; a command that may be running unattended cannot,
    /// so it takes an instruction instead.
    #[arg(long)]
    create_labels: bool,
```

- [ ] **Step 4: Queue the creates, in order**

Replacing the apology at `runtime/mod.rs:729`:

```rust
    if !built.unknown_labels.is_empty() {
        if create_labels {
            // Before the task, so the attach `decompose` emits behind it names a label
            // the queue has already asked the server for.
            for title in &built.unknown_labels {
                store
                    .queue(tui_do_core::store::Mutation::CreateLabel {
                        label: Box::new(tui_do_api::models::Label {
                            title: title.clone(),
                            ..Default::default()
                        }),
                    })
                    .await
                    .context("could not queue the label")?;
            }
            println!("Created label {}.", built.unknown_labels.join(", "));
        } else {
            println!(
                "No label called {} — pass --create-labels to make it, or it is left off.",
                built.unknown_labels.join(", ")
            );
        }
    }
```

The `CreateTask` queued after this carries those labels by their provisional ids, so
`decompose` puts an `AttachLabel` behind it and adoption retargets it. That means the
label ids have to be threaded from the creates into `built.task.labels` before the task is
queued — do that in the same loop rather than re-reading the store.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p tui-do`
Expected: PASS.

- [ ] **Step 6: Check the help and the completions**

Run: `cargo run --release -- add --help` and confirm the flag reads the way it should.
Regenerate completions if the build does not: `cargo run --release -- completions bash > ~/.local/share/bash-completion/completions/tui-do`.

- [ ] **Step 7: Commit**

```bash
git add crates/tui-do
git commit -F - <<'EOF'
Let `tui-do add` create a label, when it is told to
EOF
```

---

### Task 12: Write down what changed, and what a human still has to check

**Files:**
- Modify: `CLAUDE.md`, `PLAN.md`, `md/MANUAL-CHECKS2.md`
- Create: `md/<date>-continuity.md` if the session ends here

- [ ] **Step 1: `CLAUDE.md`** — under rule 5, say that a queued mutation's subject is a
  `Subject`, not a task id, and why. The label wire-format findings are already there from
  2026-08-29; add the one rule that came out of building on them: **a `CreateLabel` that
  has already failed reads before it writes, and `is_already_done` deliberately has no arm
  for a create.**

- [ ] **Step 2: `PLAN.md`** — the label lifecycle is a Phase 4 gap closed out of sequence,
  before Phase 5. Say so where the phase list would otherwise imply it never existed.

- [ ] **Step 3: `md/MANUAL-CHECKS2.md`** — add a section a human drives on the **release**
  binary:
  1. `l` on a task with no labels anywhere: the form opens rather than refusing.
  2. Type a new name, `Ctrl-N`: the label appears ticked, and the status line shows one
     queued change.
  3. `r`: the queue empties and the label keeps its name in the form that is still open.
     *(This is the adoption path that reaches the open modal — the one most easily missed.)*
  4. Rename it, then `u`: it comes back, with its colour.
  5. `a`, then `Call the VA *waiting`: the confirmation appears; decline, and the task is
     added without the label.
  6. Repeat and accept: the label exists and is attached.
  7. Offline (`test-scripts/go-offline.sh`), create a label, come back: one label, not two.

- [ ] **Step 4: Build release, run everything, commit**

```bash
cargo build --workspace --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git add CLAUDE.md PLAN.md md
git commit -F - <<'EOF'
Write down how a label gets made
EOF
git push origin main
```

- [ ] **Step 5: Run the live tests against dev**

```sh
./run-live-tests-token.sh ~/.config/tui-do/token
```

Expected: all nine pass, including
`a_task_round_trips_through_create_read_update_delete`, which now covers the rename.

---

## What this plan deliberately does not do

- **No `DeleteLabel`.** Decision 1. `u` cannot honestly reverse it — the label returns with
  a new id, detached from everything it was on. If it is ever built it needs a confirmation
  and an honest "this cannot be undone", not a broken undo.
- **No dead-letter or attempt cap** on the new mutations, matching the existing policy: a
  permanent failure is a 4xx, which `is_permanent` already rolls back, and what keeps
  retrying may genuinely recover.
- **No protection against two boxes creating the same label at the same moment.** Nothing
  can provide it without a unique constraint the server does not have. The reconcile in
  Task 4 covers *this box's own retry*, which is the case that is otherwise unrecoverable.
