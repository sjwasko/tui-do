//! Edits already on screen that a task load cannot yet be trusted to show.
//!
//! Every write is drawn the moment it is made and stored a moment later, by an effect of
//! its own. A load can be issued in between -- the reload one write asks for once it has
//! landed is issued while a *later* write is still on its way to the store -- and its
//! answer then shows that later task as it was before the user touched it. Taken
//! wholesale, it put a row the user had just marked done back under a held `d`, which
//! pressed it a second time: 17 duplicate writes in 1,767 when it was measured (BUG-22),
//! and with the other interleaving a `done = false` that silently undid the first.
//!
//! So the model keeps what it drew, keyed by a [`WriteSeq`] it hands the runtime with the
//! write, and replays it over every load. An entry stops being replayed once the store has
//! acknowledged it *and* a load issued after that acknowledgement has answered: that load
//! read a store which already held the write, so it is the truth and the copy is not.

use tui_do_core::models::{Task, TaskId};
use tui_do_core::store::Mutation;

use crate::query::QueryId;

/// Names one write the model asked the runtime to store.
///
/// Allocated by the model, in the order the writes were made, and carried back on the
/// acknowledgement so the model knows which of its drawn edits the store now holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct WriteSeq(u64);

impl WriteSeq {
    /// The next number in sequence.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// One drawn edit, and how far the store has got with it.
#[derive(Debug, Clone, PartialEq)]
struct Unsettled {
    write: WriteSeq,
    mutation: Mutation,
    /// The task query current when the store acknowledged the write, or `None` while it
    /// has not. A load issued under a later id read a store that already held it.
    stored_under: Option<QueryId>,
}

/// The edits a task load may not yet reflect, oldest first.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overlay {
    last: WriteSeq,
    writes: Vec<Unsettled>,
}

/// What [`land`] did to the list, for a caller that moves the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Landed {
    /// A row went in at the top.
    Inserted,
    /// The row at this index left the list.
    Removed(usize),
    /// Rows changed in place, or nothing matched.
    InPlace,
}

impl Overlay {
    /// Remember an edit that has just been drawn, and number it for the runtime.
    pub fn record(&mut self, mutation: Mutation) -> WriteSeq {
        self.last = self.last.next();
        self.writes.push(Unsettled {
            write: self.last,
            mutation,
            stored_under: None,
        });
        self.last
    }

    /// The store holds `write`. `current` is the task query in force as it says so.
    pub fn stored(&mut self, write: WriteSeq, current: QueryId) {
        if let Some(entry) = self.writes.iter_mut().find(|entry| entry.write == write) {
            entry.stored_under = Some(current);
        }
    }

    /// The store refused `write`. Nothing will ever hold it, so the next load is right.
    pub fn failed(&mut self, write: WriteSeq) {
        self.writes.retain(|entry| entry.write != write);
    }

    /// The store gave the task `write` created this id; it was drawn without one.
    pub fn created(&mut self, write: WriteSeq, id: TaskId) {
        for entry in &mut self.writes {
            if entry.write == write {
                if let Mutation::CreateTask { task } = &mut entry.mutation {
                    task.id = id;
                }
            }
        }
    }

    /// A provisional task id has become the server's.
    pub fn retarget(&mut self, from: TaskId, to: TaskId) {
        for entry in &mut self.writes {
            entry.mutation.retarget(from, to);
        }
    }

    /// A provisional label id has become the server's.
    pub fn retarget_label(
        &mut self,
        from: tui_do_core::models::LabelId,
        to: tui_do_core::models::LabelId,
    ) {
        for entry in &mut self.writes {
            entry.mutation.retarget_label(from, to);
        }
    }

    /// How many drawn edits a load may still not show.
    #[must_use]
    pub fn len(&self) -> usize {
        self.writes.len()
    }

    /// Whether every drawn edit is known to be in what the last load showed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    /// Lay the edits a load `answered` cannot be trusted with over what it found.
    ///
    /// Forgets first every edit stored before that load was issued, since it found them
    /// already. What is left goes back on in the order it was made, and every step is
    /// idempotent, because the load may have caught some of it anyway: a create is not
    /// put in twice, an update replaces only a row the load returned -- one the filter
    /// hides stays hidden -- and a removal of a row that is not there removes nothing.
    pub fn replay(&mut self, answered: QueryId, tasks: &mut Vec<Task>, include_done: bool) {
        self.writes
            .retain(|entry| entry.stored_under.is_none_or(|stored| stored >= answered));
        for (at, entry) in self.writes.iter().enumerate() {
            let later = &self.writes[at + 1..];
            match &entry.mutation {
                Mutation::CreateTask { task } if self.already_loaded(task, tasks) => {}
                // Only the newest edit to a task says what it should look like now. Laid
                // on in turn, a `d` followed by its undo would have the first take the
                // row away and the second find nothing to put back.
                Mutation::UpdateTask { after, .. }
                    if later.iter().any(|newer| updates(&newer.mutation, after.id)) => {}
                mutation => {
                    land(tasks, mutation, include_done);
                }
            }
        }
    }

    /// Whether a load already holds the task this create drew.
    ///
    /// By id once the store has said what it is. Until then the row carries the
    /// placeholder `0`, and the store's copy can only be recognised by what it says: a
    /// provisional row in the same project with the same title, that no other create in
    /// here has claimed by id.
    fn already_loaded(&self, created: &Task, tasks: &[Task]) -> bool {
        if created.id != TaskId::default() {
            return tasks.iter().any(|task| task.id == created.id);
        }
        tasks.iter().any(|task| {
            task.id.get() < 0
                && task.project_id == created.project_id
                && task.title == created.title
                && !self.writes.iter().any(|entry| {
                    matches!(&entry.mutation, Mutation::CreateTask { task: other } if other.id == task.id)
                })
        })
    }
}

fn updates(mutation: &Mutation, id: TaskId) -> bool {
    matches!(mutation, Mutation::UpdateTask { after, .. } if after.id == id)
}

/// Put one edit on a task list, as the user sees it applied.
///
/// The half of an optimistic edit that touches the rows, shared by the edit as it is made
/// and by [`Overlay::replay`] over a load. The selection is the caller's: an edit as it is
/// made moves the cursor, and a replay must leave it where the user put it.
pub(crate) fn land(tasks: &mut Vec<Task>, mutation: &Mutation, include_done: bool) -> Landed {
    match mutation {
        Mutation::CreateTask { task } => {
            // At the top, so it is visible the instant it exists. The reload puts it in
            // its sorted place.
            tasks.insert(0, (**task).clone());
            Landed::Inserted
        }
        Mutation::UpdateTask { after, .. } => {
            let Some(at) = tasks.iter().position(|task| task.id == after.id) else {
                return Landed::InPlace;
            };
            // A task marked done leaves a list that is not showing done tasks, and it
            // leaves it *here* rather than on the reload that follows. See `apply_locally`
            // for why the cursor needs it.
            if after.done && !include_done {
                tasks.remove(at);
                Landed::Removed(at)
            } else {
                tasks[at] = (**after).clone();
                Landed::InPlace
            }
        }
        Mutation::DeleteTask { before } => {
            match tasks.iter().position(|task| task.id == before.id) {
                Some(at) => {
                    tasks.remove(at);
                    Landed::Removed(at)
                }
                None => Landed::InPlace,
            }
        }
        Mutation::AttachLabel { task, label } => {
            if let Some(existing) = tasks.iter_mut().find(|row| row.id == *task) {
                if !existing.labels.iter().any(|held| held.id == label.id) {
                    existing.labels.push((**label).clone());
                }
            }
            Landed::InPlace
        }
        Mutation::DetachLabel { task, label } => {
            if let Some(existing) = tasks.iter_mut().find(|row| row.id == *task) {
                existing.labels.retain(|held| held.id != label.id);
            }
            Landed::InPlace
        }
        // The chips on each row. The label snapshots the model keeps beside the list are
        // `apply_locally`'s, because a load does not replace them.
        Mutation::UpdateLabel { after, .. } => {
            for task in tasks.iter_mut() {
                for held in &mut task.labels {
                    if held.id == after.id {
                        *held = (**after).clone();
                    }
                }
            }
            Landed::InPlace
        }
        Mutation::CreateLabel { .. } => Landed::InPlace,
    }
}
