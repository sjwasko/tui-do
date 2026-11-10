//! Edits already on screen that a task load cannot yet be trusted to show.
//!
//! Every write is drawn the moment it is made and stored a moment later, by an effect of
//! its own. A load can be issued in between -- the reload one write asks for once it has
//! landed is issued while a *later* write is still on its way to the store -- and its
//! answer then shows that later task as it was before the user touched it. Taken
//! wholesale, it put a row the user had just marked done back under a held `t`, which
//! pressed it a second time: 17 duplicate writes in 1,767 when it was measured (BUG-22),
//! and with the other interleaving a `done = false` that silently undid the first.
//!
//! So the model keeps what it drew, keyed by a [`WriteSeq`] it hands the runtime with the
//! write, and replays it over every load. An entry stops being replayed once the store has
//! acknowledged it *and* a load issued after that acknowledgement has answered: that load
//! read a store which already held the write, so it is the truth and the copy is not.
//!
//! # Why there is no cap
//!
//! Nothing here is dropped by age or count, and that is a decision rather than an
//! omission. An entry goes in one of three ways: its write fails ([`Overlay::failed`]),
//! or it is acknowledged and then any task load issued afterwards answers. The runtime
//! sends a reload straight after every acknowledgement, so in practice an entry outlives
//! its write by one local read. What can keep one alive longer is task loads failing
//! repeatedly, and then the list is already frozen under a `StoreFailed` banner; the
//! entries are one mutation per edit the user made in that time, kilobytes at most.
//! Dropping an *acknowledged* entry early is safe only once a load issued after the
//! acknowledgement has answered, which is exactly the rule already applied. Dropping an
//! *unacknowledged* one is never safe, since it brings BUG-22 back for that task. A cap
//! would trade a bounded, user-sized amount of memory for a correctness bug.

use tui_do_core::models::{Label, Task, TaskId};
use tui_do_core::store::Mutation;

use crate::query::{Query, QueryId};

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
    /// For a create, the task's id once it is known, or `None` while the store has still
    /// to allocate it. Unused for every other mutation.
    created: Option<TaskId>,
    /// The task load generation in force when the store's acknowledgement arrived, or
    /// `None` while it has not. Any load issued under a later generation read a store that
    /// already held this write.
    acked_during: Option<QueryId>,
}

/// The edits a task load may not yet reflect, oldest first.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overlay {
    last: WriteSeq,
    entries: Vec<Unsettled>,
}

/// What [`land`] did to the list, for a caller that moves the selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Landed<'a> {
    /// The task with this id went in at the top.
    Inserted(TaskId),
    /// The row at this index left the list.
    Removed(usize),
    /// A label was renamed or recoloured, and the chips on the rows now carry this.
    Relabelled(&'a Label),
    /// Rows changed in place, or nothing matched.
    InPlace,
}

impl Overlay {
    /// Remember an edit that has just been drawn, and number it for the runtime.
    pub fn record(&mut self, mutation: Mutation) -> WriteSeq {
        self.last = self.last.next();
        let created = match &mutation {
            // The one place the placeholder is read. `0` is the value the store itself
            // treats as "allocate an id here"; an undone delete re-creates a task under
            // the id it already had, which is known from the start.
            Mutation::CreateTask { task } if task.id.get() != 0 => Some(task.id),
            _ => None,
        };
        self.entries.push(Unsettled {
            write: self.last,
            mutation,
            created,
            acked_during: None,
        });
        self.last
    }

    /// The store holds `write`. `current` is the task load generation in force as it says
    /// so.
    pub fn stored(&mut self, write: WriteSeq, current: QueryId) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.write == write) {
            entry.acked_during = Some(current);
        }
    }

    /// The store refused `write`. Nothing will ever hold it, so the next load is right.
    pub fn failed(&mut self, write: WriteSeq) {
        self.entries.retain(|entry| entry.write != write);
    }

    /// The store gave the task `write` created this id; it was drawn without one.
    pub fn created(&mut self, write: WriteSeq, id: TaskId) {
        for entry in &mut self.entries {
            if entry.write == write {
                if let Mutation::CreateTask { task } = &mut entry.mutation {
                    task.id = id;
                    entry.created = Some(id);
                }
            }
        }
    }

    /// A provisional task id has become the server's.
    pub fn retarget(&mut self, from: TaskId, to: TaskId) {
        for entry in &mut self.entries {
            entry.mutation.retarget(from, to);
            if entry.created == Some(from) {
                entry.created = Some(to);
            }
        }
    }

    /// A provisional label id has become the server's.
    pub fn retarget_label(
        &mut self,
        from: tui_do_core::models::LabelId,
        to: tui_do_core::models::LabelId,
    ) {
        for entry in &mut self.entries {
            entry.mutation.retarget_label(from, to);
        }
    }

    /// How many drawn edits a load may still not show.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether every drawn edit is known to be in what the last load showed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Lay the edits a load `answered` cannot be trusted with over what it found for
    /// `query`.
    ///
    /// Forgets first every edit acknowledged before that load was issued, since it found
    /// them already. What is left goes back on in the order it was made, and every step is
    /// idempotent, because the load may have caught some of it anyway: a create is not
    /// put in twice, nor put in at all where `query` would not show it; an update replaces
    /// only a row the load returned, so one the filter hides stays hidden; and a removal
    /// of a row that is not there removes nothing.
    pub fn replay(&mut self, answered: QueryId, tasks: &mut Vec<Task>, query: &Query) {
        self.entries
            .retain(|entry| entry.acked_during.is_none_or(|acked| acked >= answered));
        for (at, entry) in self.entries.iter().enumerate() {
            let later = &self.entries[at + 1..];
            match &entry.mutation {
                // A quick-add into another project, say: drawn at the top as it was made,
                // and gone again on the first load, which is what the reload did before
                // there was an overlay. Putting it back here would pin a task to a list
                // that does not contain it.
                Mutation::CreateTask { task }
                    if !query.admits(task) || self.already_loaded(entry, task, tasks) => {}
                // Only the newest edit to a task says what it should look like now. Laid
                // on in turn, a `t` followed by its undo would have the first take the
                // row away and the second find nothing to put back.
                Mutation::UpdateTask { after, .. }
                    if later.iter().any(|newer| updates(&newer.mutation, after.id)) => {}
                mutation => {
                    land(tasks, mutation, query.include_done);
                }
            }
        }
    }

    /// Whether a load already holds the task this create drew.
    ///
    /// By id once the store has said what it is. Until then, the store's copy can only be
    /// recognised by what it says: a provisional row in the same project with the same
    /// title, that no other create in here has claimed by id.
    fn already_loaded(&self, entry: &Unsettled, created: &Task, tasks: &[Task]) -> bool {
        if let Some(id) = entry.created {
            return tasks.iter().any(|task| task.id == id);
        }
        tasks.iter().any(|task| {
            task.id.get() < 0
                && task.project_id == created.project_id
                && task.title == created.title
                && !self
                    .entries
                    .iter()
                    .any(|other| other.created == Some(task.id))
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
pub(crate) fn land<'a>(
    tasks: &mut Vec<Task>,
    mutation: &'a Mutation,
    include_done: bool,
) -> Landed<'a> {
    match mutation {
        Mutation::CreateTask { task } => {
            // At the top, so it is visible the instant it exists. The reload puts it in
            // its sorted place.
            tasks.insert(0, (**task).clone());
            Landed::Inserted(task.id)
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
            Landed::Relabelled(after)
        }
        Mutation::CreateLabel { .. } => Landed::InPlace,
    }
}
