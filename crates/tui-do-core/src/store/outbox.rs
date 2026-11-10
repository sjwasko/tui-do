//! The outbox: local mutations that have not reached the server yet.
//!
//! Rule 5 in one file. A write mutates the store and records what it did, in one
//! transaction, and returns; the sync engine drains the queue afterwards. The UI never
//! waits for the network, and the queue can never hold an entry for a change the store
//! did not make, or the reverse.
//!
//! Each [`Mutation`] knows three things about itself, and keeping them adjacent is the
//! point of the type: how to [`apply`](Mutation::apply) it locally, how to
//! [`rollback`](Mutation::rollback) that when the server refuses, and what its
//! [`inverse`](Mutation::inverse) is. Undo is the inverse queued as a fresh mutation
//! rather than a second mechanism with its own bugs.
//!
//! # A created task has an id before the server has seen it
//!
//! Something has to identify a task the moment it is typed -- the list renders it, the
//! user can edit it again, and both happen before any request is made. Creates therefore
//! take a *provisional* id, negative and allocated locally, and
//! [`Store::settle_create`] swaps in the server's row once it answers, rewriting any
//! queued entry that still refers to the old id. Negative ids never collide with
//! Vikunja's, which are positive.
//!
//! # One entry, one request
//!
//! Assignees are part of [`Mutation::UpdateTask`], because `POST /tasks/{id}` replaces
//! them from the task body. Labels are not: that same body's `labels` field is ignored,
//! and they attach and detach through their own endpoints. So a caller may hand
//! [`Store::queue`] a task carrying labels, and it is split here into a task mutation
//! and one label mutation per change — see [`Mutation::decompose`].
//!
//! That split is not tidiness. An entry that took two requests could have the first one
//! land and the second fail, and the queue has nowhere to record "half done": a retry
//! replays both, creating a duplicate task or re-deleting a label that is already gone.
//! One entry, one request, so a retry means exactly what it says.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tui_do_api::models::{Label, LabelId, Task, TaskId};

use super::labels::{attach_label, is_being_created, upsert_label};
use super::state::{read_state, write_state};
use super::tasks::{read_task, upsert_task};
use super::{DecodeCount, Store};
use crate::error::Result;

/// The `sync_state` key holding the next provisional task id.
const NEXT_LOCAL_ID: &str = "next_local_task_id";

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

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.kind(), self.id())
    }
}

/// A local change, queued for the server.
///
/// Serialized into `outbox.payload` as JSON. Variants are added, never renamed: an
/// entry written by an earlier build has to still parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mutation", rename_all = "snake_case")]
pub enum Mutation {
    /// A task the server has not seen. `task.id` is provisional until it answers.
    CreateTask {
        /// The task as typed, including its assignees.
        task: Box<Task>,
    },

    /// A change to an existing task, carrying what it looked like before.
    ///
    /// `before` is what a rejection restores and what an undo re-applies, so it is
    /// stored rather than re-read: by the time either happens, the row has moved on.
    UpdateTask {
        /// The task as it was.
        before: Box<Task>,
        /// The task as it should be.
        after: Box<Task>,
    },

    /// A deletion, carrying the task so a rejection can put it back.
    DeleteTask {
        /// The task as it was.
        before: Box<Task>,
    },

    /// A label attached to a task through `PUT /tasks/{task}/labels`.
    AttachLabel {
        /// The task it goes on.
        task: TaskId,
        /// The whole label, so the local row exists even if labels have not been pulled.
        label: Box<Label>,
    },

    /// A label detached through `DELETE /tasks/{task}/labels/{label}`.
    DetachLabel {
        /// The task it comes off.
        task: TaskId,
        /// The whole label, so an undo can put it back without a lookup.
        label: Box<Label>,
    },

    /// A label the server has not seen. `label.id` is provisional until it answers.
    ///
    /// Only ever queued by the interface, which is the half that knows which labels
    /// exist -- `decompose` splits a task write into requests and has no idea.
    CreateLabel {
        /// The label as typed.
        label: Box<Label>,
    },

    /// A change to an existing label -- its title, its colour or its description.
    ///
    /// One variant for all three rather than a `RenameLabel` and a `RecolourLabel`,
    /// because they are one request and one undo step: `POST /labels/{id}` replaces the
    /// label from the body, so changing the title and the colour separately would be two
    /// writes where the second reverts nothing and the first is pointless work.
    ///
    /// `before` is what a rejection restores and what an undo re-applies, so it is stored
    /// rather than re-read -- by the time either happens the row has moved on.
    UpdateLabel {
        /// The label as it was.
        before: Box<Label>,
        /// The label as it should be.
        after: Box<Label>,
    },
}

impl Mutation {
    /// A short stable name, stored alongside the payload.
    ///
    /// Duplicates what the JSON already says, deliberately: it makes the queue readable
    /// in `sqlite3` and lets a future retry policy select by kind without parsing every
    /// row.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CreateTask { .. } => "create_task",
            Self::UpdateTask { .. } => "update_task",
            Self::DeleteTask { .. } => "delete_task",
            Self::AttachLabel { .. } => "attach_label",
            Self::DetachLabel { .. } => "detach_label",
            Self::CreateLabel { .. } => "create_label",
            Self::UpdateLabel { .. } => "update_label",
        }
    }

    /// Whether this makes something new on the server.
    ///
    /// A create is the one mutation a replay duplicates rather than repeats, so it is
    /// marked sent before it goes out (BUG-33), and its settle rewrites what is queued
    /// behind it. Exhaustive so a new variant has to answer.
    #[must_use]
    pub const fn is_create(&self) -> bool {
        match self {
            Self::CreateTask { .. } | Self::CreateLabel { .. } => true,
            Self::UpdateTask { .. }
            | Self::DeleteTask { .. }
            | Self::AttachLabel { .. }
            | Self::DetachLabel { .. }
            | Self::UpdateLabel { .. } => false,
        }
    }

    /// What this entry acts on.
    #[must_use]
    pub fn subject(&self) -> Subject {
        match self {
            Self::CreateTask { task } => Subject::Task(task.id),
            Self::UpdateTask { after, .. } => Subject::Task(after.id),
            Self::DeleteTask { before } => Subject::Task(before.id),
            Self::AttachLabel { task, .. } | Self::DetachLabel { task, .. } => Subject::Task(*task),
            Self::CreateLabel { label } => Subject::Label(label.id),
            Self::UpdateLabel { after, .. } => Subject::Label(after.id),
        }
    }

    /// Point this mutation at a different task id.
    ///
    /// Used when the server assigns a real id to a locally created task and entries
    /// queued behind the create still name the provisional one.
    pub fn retarget(&mut self, from: TaskId, to: TaskId) {
        let swap = |id: &mut TaskId| {
            if *id == from {
                *id = to;
            }
        };
        match self {
            Self::CreateTask { task } => swap(&mut task.id),
            Self::UpdateTask { before, after } => {
                swap(&mut before.id);
                swap(&mut after.id);
            }
            Self::DeleteTask { before } => swap(&mut before.id),
            Self::AttachLabel { task, .. } | Self::DetachLabel { task, .. } => swap(task),
            // Retargets tasks; a label create or edit has none to swap. The label
            // equivalent is `retarget_label`, added separately so the two id spaces cannot
            // be confused.
            Self::CreateLabel { .. } | Self::UpdateLabel { .. } => {}
        }
    }

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
            Self::AttachLabel { label, .. } | Self::DetachLabel { label, .. } => {
                swap(&mut label.id);
            }
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
            // Both halves. `before` is what a rejection restores and what an undo
            // re-applies, so one left on the provisional id would resurrect a row the
            // store has already deleted.
            Self::UpdateLabel { before, after } => {
                swap(&mut before.id);
                swap(&mut after.id);
            }
        }
    }

    /// The label id this entry's own request sends, when it sends one.
    ///
    /// This is what depends on a `CreateLabel`: an attach or a detach puts the id in its
    /// path, and a rename in its path and body. Exhaustive over [`Mutation`] rather than a
    /// wildcard arm, so a new variant fails to compile here rather than silently
    /// answering "sends no label".
    ///
    /// `None` for the three task-carrying variants even when their task holds labels.
    /// `CLAUDE.md` records, measured, that a task body's `labels` field is ignored by the
    /// server, and a delete sends no body at all -- so none of them asks the server for a
    /// label. What they carry is [`Self::strip_label`]'s business instead. `None` for
    /// `CreateLabel` too: the id it carries is the one it defines, and the server ignores
    /// it.
    #[must_use]
    pub fn wire_label(&self) -> Option<LabelId> {
        match self {
            Self::AttachLabel { label, .. } | Self::DetachLabel { label, .. } => Some(label.id),
            Self::UpdateLabel { after, .. } => Some(after.id),
            Self::CreateTask { .. }
            | Self::UpdateTask { .. }
            | Self::DeleteTask { .. }
            | Self::CreateLabel { .. } => None,
        }
    }

    /// Take a label off every task this entry carries.
    ///
    /// For a label that will never exist: a provisional one whose create the server
    /// refused. A task-carrying entry is still the user's change to everything else, so
    /// it loses the label rather than being dropped -- and the label has to go, because
    /// `decompose` turns a `CreateTask`'s labels into one `AttachLabel` each, which would
    /// send the id no server issued. The variants that [`Self::wire_label`] answers for
    /// are left alone; an entry that *is* about the label has nothing to strip it from.
    pub fn strip_label(&mut self, label: LabelId) {
        match self {
            Self::CreateTask { task } => task.drop_label(label),
            Self::UpdateTask { before, after } => {
                before.drop_label(label);
                after.drop_label(label);
            }
            Self::DeleteTask { before } => before.drop_label(label),
            Self::AttachLabel { .. }
            | Self::DetachLabel { .. }
            | Self::CreateLabel { .. }
            | Self::UpdateLabel { .. } => {}
        }
    }

    /// What undoing this would be, when it can be undone.
    ///
    /// Undo queues the inverse as an ordinary mutation, so it is optimistic, rolls back
    /// on rejection and can itself be undone -- all for free. Note that undoing a delete
    /// *re-creates* the task: Vikunja has no undelete, so the restored task gets a new
    /// id, and its comments and attachments do not come back.
    ///
    /// `None` for [`Self::CreateLabel`]: undoing it means deleting a label, and a delete
    /// is the one label operation `U` cannot honestly reverse -- the label would come
    /// back with a new id, detached from everything it was on. So a create is not pushed
    /// onto the undo stack at all and `U` reaches past it.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        Some(match self {
            Self::CreateTask { task } => Self::DeleteTask {
                before: task.clone(),
            },
            Self::UpdateTask { before, after } => Self::UpdateTask {
                before: after.clone(),
                after: before.clone(),
            },
            Self::DeleteTask { before } => Self::CreateTask {
                task: before.clone(),
            },
            Self::AttachLabel { task, label } => Self::DetachLabel {
                task: *task,
                label: label.clone(),
            },
            Self::DetachLabel { task, label } => Self::AttachLabel {
                task: *task,
                label: label.clone(),
            },
            Self::UpdateLabel { before, after } => Self::UpdateLabel {
                before: after.clone(),
                after: before.clone(),
            },
            Self::CreateLabel { .. } => return None,
        })
    }

    /// Split this into mutations that each take exactly one request.
    ///
    /// A task write cannot carry labels — the server ignores the body's `labels` field —
    /// so a create or an edit that changes them becomes a task mutation followed by one
    /// [`Mutation::AttachLabel`] or [`Mutation::DetachLabel`] per change. Callers do not
    /// have to know that: they set labels on the task and queue it.
    ///
    /// The pieces apply in order and their effects compose to the original, so the local
    /// store ends up exactly where the undivided mutation would have left it.
    fn decompose(self) -> Vec<Self> {
        match self {
            Self::CreateTask { mut task } if !task.labels.is_empty() => {
                let labels = std::mem::take(&mut task.labels);
                let id = task.id;
                let mut parts = vec![Self::CreateTask { task }];
                parts.extend(labels.into_iter().map(|label| Self::AttachLabel {
                    task: id,
                    label: Box::new(label),
                }));
                parts
            }
            Self::UpdateTask { before, mut after } => {
                let attached: Vec<Label> = after
                    .labels
                    .iter()
                    .filter(|label| !before.labels.iter().any(|had| had.id == label.id))
                    .cloned()
                    .collect();
                let detached: Vec<Label> = before
                    .labels
                    .iter()
                    .filter(|label| !after.labels.iter().any(|keeps| keeps.id == label.id))
                    .cloned()
                    .collect();
                if attached.is_empty() && detached.is_empty() {
                    return vec![Self::UpdateTask { before, after }];
                }

                // The task mutation is left responsible for everything except labels,
                // so its rollback restores exactly what it changed and no more.
                let id = after.id;
                after.labels.clone_from(&before.labels);
                let mut parts = vec![Self::UpdateTask { before, after }];
                parts.extend(attached.into_iter().map(|label| Self::AttachLabel {
                    task: id,
                    label: Box::new(label),
                }));
                parts.extend(detached.into_iter().map(|label| Self::DetachLabel {
                    task: id,
                    label: Box::new(label),
                }));
                parts
            }
            other => vec![other],
        }
    }

    /// Make this change in the local store.
    fn apply(&self, tx: &Transaction<'_>, now: DateTime<Utc>) -> Result<()> {
        match self {
            Self::CreateTask { task } => upsert_task(tx, task, now)?,
            Self::UpdateTask { after, .. } => upsert_task(tx, after, now)?,
            Self::DeleteTask { before } => {
                tx.execute("DELETE FROM tasks WHERE id = ?1", params![before.id.get()])?;
            }
            Self::AttachLabel { task, label } => {
                attach_label(tx, *task, label, now)?;
            }
            Self::DetachLabel { task, label } => {
                tx.execute(
                    "DELETE FROM task_labels WHERE task_id = ?1 AND label_id = ?2",
                    params![task.get(), label.id.get()],
                )?;
            }
            Self::CreateLabel { label } => upsert_label(tx, label, now)?,
            Self::UpdateLabel { after, .. } => upsert_label(tx, after, now)?,
        }
        Ok(())
    }

    /// Put the local store back the way it was.
    fn rollback(&self, tx: &Transaction<'_>, now: DateTime<Utc>) -> Result<()> {
        match self {
            Self::CreateTask { task } => {
                tx.execute("DELETE FROM tasks WHERE id = ?1", params![task.id.get()])?;
            }
            Self::UpdateTask { before, .. } => upsert_task(tx, before, now)?,
            Self::DeleteTask { before } => upsert_task(tx, before, now)?,
            Self::AttachLabel { task, label } => {
                tx.execute(
                    "DELETE FROM task_labels WHERE task_id = ?1 AND label_id = ?2",
                    params![task.get(), label.id.get()],
                )?;
            }
            Self::DetachLabel { task, label } => {
                attach_label(tx, *task, label, now)?;
            }
            Self::CreateLabel { label } => {
                tx.execute("DELETE FROM labels WHERE id = ?1", params![label.id.get()])?;
            }
            Self::UpdateLabel { before, .. } => upsert_label(tx, before, now)?,
        }
        Ok(())
    }
}

/// A queued mutation, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEntry {
    /// Queue position; entries are drained in this order.
    pub id: i64,
    /// When it was queued.
    pub created: DateTime<Utc>,
    /// What to do.
    pub mutation: Mutation,
    /// How many times the sync engine has tried and failed.
    pub attempts: i64,
    /// What went wrong last time, for the UI to show.
    pub last_error: Option<String>,
    /// The earliest this may be tried again, or `None` for "as soon as possible".
    ///
    /// Set by [`Store::defer`] from the backoff schedule, or from the server's
    /// `Retry-After` when it sent one.
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// When a create was last put on the wire, or `None` if it never has been.
    ///
    /// Set by [`Store::mark_sent`] before the request leaves, so it survives a process
    /// that dies before the answer is stored (BUG-33). Only creates carry it.
    pub sent_at: Option<DateTime<Utc>>,
}

impl OutboxEntry {
    /// Whether the backoff has elapsed and this may be sent.
    #[must_use]
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        is_due(self.next_attempt_at, now)
    }

    /// Whether this entry has failed at least once and is waiting to be retried.
    #[must_use]
    pub const fn is_failing(&self) -> bool {
        self.attempts > 0
    }

    /// Whether an earlier attempt may already have reached the server.
    ///
    /// True after a recorded failure, and also after a request that went out and whose
    /// answer was never stored -- a process killed mid-request leaves `attempts` at zero
    /// but `sent_at` set (BUG-33). This is what arms a create's read-before-retry.
    #[must_use]
    pub const fn may_have_been_sent(&self) -> bool {
        self.is_failing() || self.sent_at.is_some()
    }
}

/// Where a queued entry stands, without its payload: what the drain needs to decide
/// whether it may be sent next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Queued {
    /// Queue position, as [`OutboxEntry::id`].
    pub id: i64,
    /// What it acts on, as [`Mutation::subject`] says -- the store keeps the two equal.
    pub subject: Subject,
    /// As [`OutboxEntry::attempts`].
    pub attempts: i64,
    /// As [`OutboxEntry::next_attempt_at`].
    pub next_attempt_at: Option<DateTime<Utc>>,
}

impl Queued {
    /// Whether the backoff has elapsed. As [`OutboxEntry::is_due`].
    #[must_use]
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        is_due(self.next_attempt_at, now)
    }
}

/// An outbox row as read, before its payload is parsed.
struct StoredEntry {
    id: i64,
    created: String,
    payload: String,
    attempts: i64,
    last_error: Option<String>,
    next_attempt_at: Option<String>,
    sent_at: Option<String>,
}

impl StoredEntry {
    /// From `id, created, payload, attempts, last_error, next_attempt_at, sent_at`, in
    /// that order.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            created: row.get(1)?,
            payload: row.get(2)?,
            attempts: row.get(3)?,
            last_error: row.get(4)?,
            next_attempt_at: row.get(5)?,
            sent_at: row.get(6)?,
        })
    }

    fn decode(self, decodes: &DecodeCount) -> Result<OutboxEntry> {
        Ok(OutboxEntry {
            id: self.id,
            created: DateTime::parse_from_rfc3339(&self.created)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            mutation: decode(decodes, &self.payload)?,
            attempts: self.attempts,
            last_error: self.last_error,
            next_attempt_at: parse_instant(self.next_attempt_at.as_deref()),
            sent_at: parse_instant(self.sent_at.as_deref()),
        })
    }
}

/// A stored timestamp, or `None` for one that is absent or unreadable.
fn parse_instant(text: Option<&str>) -> Option<DateTime<Utc>> {
    text.and_then(|text| {
        DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    })
}

/// Whether a backoff ending at `next_attempt_at` has elapsed by `now`. `None` is "as
/// soon as possible". One rule for [`OutboxEntry`] and [`Queued`], which both carry it.
fn is_due(next_attempt_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    next_attempt_at.is_none_or(|at| at <= now)
}

/// The shortest wait before a failed entry is tried again.
const BACKOFF_FLOOR: Duration = Duration::from_secs(5);

/// The longest. A server that has been down for an hour is not helped by being asked
/// every five seconds, and the user is not helped by waiting a day after it comes back.
const BACKOFF_CEILING: Duration = Duration::from_secs(15 * 60);

/// How long to wait before attempt number `attempts`.
///
/// Exponential from [`BACKOFF_FLOOR`], capped at [`BACKOFF_CEILING`]. `attempts` was
/// recorded from the first commit and never read, so a failing entry was retried at
/// full speed on every pass; the only thing keeping that from being a hot loop was the
/// thirty-second floor on the sync timer.
///
/// No jitter, deliberately: every box in a fleet keeps its own queue and fails at its own
/// time, so there is no thundering herd to spread out, and a predictable delay is easier
/// to explain to someone watching a status line.
fn backoff(attempts: i64) -> Duration {
    let steps = u32::try_from(attempts.saturating_sub(1).clamp(0, 16)).unwrap_or(0);
    BACKOFF_FLOOR
        .saturating_mul(2_u32.saturating_pow(steps))
        .min(BACKOFF_CEILING)
}

/// What the outbox looks like, for the status line.
///
/// `failing` and `last_error` exist because `attempts` and `last_error` were recorded
/// from the first commit and never read by anything. A change that the server kept
/// refusing sat in the queue with the user told nothing beyond a count that would not go
/// down.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueHealth {
    /// Everything still waiting to be sent.
    pub queued: usize,
    /// How many of those have failed at least once.
    pub failing: usize,
    /// What the most recent failure said, if any.
    pub last_error: Option<String>,
}

impl Store {
    /// How many changes are queued, and how many of them are in trouble.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn queue_health(&self) -> Result<QueueHealth> {
        self.read(|connection| {
            let queued: i64 =
                connection.query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))?;
            let failing: i64 = connection.query_row(
                "SELECT count(*) FROM outbox WHERE attempts > 0",
                [],
                |row| row.get(0),
            )?;
            // The most recently recorded failure, which is the one worth showing: an
            // older entry's error is usually the same outage seen earlier.
            let last_error: Option<String> = connection
                .query_row(
                    "SELECT last_error FROM outbox
                      WHERE last_error IS NOT NULL
                      ORDER BY id DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(QueueHealth {
                queued: usize::try_from(queued).unwrap_or(0),
                failing: usize::try_from(failing).unwrap_or(0),
                last_error,
            })
        })
        .await
    }

    /// Apply `mutation` locally and queue it for the server, in one transaction.
    ///
    /// Returns the entry for the change itself. It may not be the only row written: a
    /// task carrying label changes is split by [`Mutation::decompose`] into one entry
    /// per request, and the returned entry is the first of them. Its mutation may also
    /// differ from the one passed in — a create is assigned its provisional id here, and
    /// its labels move to entries of their own.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure,
    /// [`crate::CoreError::Encoding`] if the mutation cannot be serialized, or
    /// [`crate::CoreError::Refused`] if any part would attach or detach a provisional
    /// label whose `CreateLabel` is no longer queued. In every case nothing is written,
    /// not even a provisional id -- the local change, its queue entries and the id
    /// counters stand or fall together in one transaction.
    pub async fn queue(&self, mutation: Mutation) -> Result<OutboxEntry> {
        let now = Utc::now();
        self.write(move |tx| {
            let mut mutation = mutation;
            if let Mutation::CreateTask { task } = &mut mutation {
                if task.id.get() == 0 {
                    task.id = next_local_id(tx)?;
                }
            }
            if let Mutation::CreateLabel { label } = &mut mutation {
                if label.id.get() == 0 {
                    label.id = next_local_label_id(tx)?;
                }
            }

            let mut first = None;
            for part in mutation.decompose() {
                // An id no server will issue. The interface forgets a refused create's
                // label everywhere it holds one, and this is the backstop for a holder
                // it missed: refusing here is a toast, where sending it would be a 4xx
                // that takes every other entry queued for the task with it.
                if let Mutation::AttachLabel { label, .. } | Mutation::DetachLabel { label, .. } =
                    &part
                {
                    if is_provisional_label(label.id) && !is_being_created(tx, label.id)? {
                        return Err(crate::CoreError::Refused(format!(
                            "label \"{}\" was never created",
                            label.title
                        )));
                    }
                }
                // The entry goes in before the change is applied, or `attach_label`'s
                // guard would refuse the label a `CreateLabel` is creating.
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
                // Read before `apply`, whose own inserts would move it.
                let id = tx.last_insert_rowid();
                part.apply(tx, now)?;
                let entry = OutboxEntry {
                    id,
                    created: now,
                    mutation: part,
                    attempts: 0,
                    last_error: None,
                    next_attempt_at: None,
                    sent_at: None,
                };
                first.get_or_insert(entry);
            }

            first.ok_or_else(|| crate::CoreError::Config {
                path: "<outbox>".to_string(),
                reason: "a mutation decomposed into nothing".to_string(),
            })
        })
        .await
    }

    /// The queued entries, oldest first.
    ///
    /// Order is the whole contract: an update queued behind a create has to reach the
    /// server after it, and two edits to one task have to arrive in the order they were
    /// made or the older one wins.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure, or
    /// [`crate::CoreError::Encoding`] if a stored payload cannot be parsed.
    pub async fn pending(&self, limit: Option<u32>) -> Result<Vec<OutboxEntry>> {
        let decodes = Arc::clone(&self.decodes);
        self.read(move |connection| {
            // Every queued entry, including ones whose `next_attempt_at` is still in the
            // future. **Deliberately unfiltered**, and an earlier version of this comment
            // claimed the opposite: filtering the backoff out in SQL would break `r` and
            // `R`, which pass `Backoff::Ignore` precisely so a user who has just fixed
            // their network can retry everything queued. Whether an entry is *due* is the
            // drain's decision, not this query's, because only the drain knows which
            // trigger asked.
            let mut sql = String::from(
                "SELECT id, created, payload, attempts, last_error, next_attempt_at, sent_at
                   FROM outbox ORDER BY id ASC",
            );
            if let Some(limit) = limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map([], StoredEntry::from_row)?;
            let mut entries = Vec::new();
            for row in rows {
                entries.push(row?.decode(&decodes)?);
            }
            Ok(entries)
        })
        .await
    }

    /// Where every entry queued after `after` stands, oldest first, without its payload.
    ///
    /// What the drain reads to choose what to send next. BUG-23: it used to call
    /// [`Self::pending`] before every entry, which parses every payload in the queue each
    /// time -- 23.7 ms at 1,781 entries, so about 40 s of re-reading in a drain that deep.
    /// This reads four small columns, and with `after` set only the rows a pass has not
    /// seen yet; ids only grow, so those are exactly what has been queued since.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure, or
    /// [`crate::CoreError::Encoding`] for a row whose subject has to be read from its
    /// payload and cannot be.
    pub async fn queued_after(&self, after: i64) -> Result<Vec<Queued>> {
        let decodes = Arc::clone(&self.decodes);
        self.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, subject_kind, subject_id, attempts, next_attempt_at,
                        CASE WHEN subject_id IS NULL OR subject_kind NOT IN ('task', 'label')
                             THEN payload END
                   FROM outbox WHERE id > ?1 ORDER BY id ASC",
            )?;
            let rows = statement.query_map(params![after], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?;
            let mut queued = Vec::new();
            for row in rows {
                let (id, kind, subject_id, attempts, next_attempt_at, payload) = row?;
                let subject = match (kind.as_str(), subject_id, payload) {
                    ("task", Some(subject), _) => Subject::Task(TaskId(subject)),
                    ("label", Some(subject), _) => Subject::Label(LabelId(subject)),
                    // Every insert writes both columns, so this row came from somewhere
                    // else. The payload knows what it acts on.
                    (_, _, Some(payload)) => decode(&decodes, &payload)?.subject(),
                    // Unreachable while `payload` is `NOT NULL`. Refused rather than
                    // skipped: a row the drain cannot see is a change it never sends, and
                    // the count on the status line would still include it.
                    (kind, subject, None) => {
                        return Err(crate::CoreError::Config {
                            path: "<outbox>".to_string(),
                            reason: format!(
                                "entry {id} has subject {kind}/{subject:?} and no payload"
                            ),
                        })
                    }
                };
                queued.push(Queued {
                    id,
                    subject,
                    attempts,
                    next_attempt_at: parse_instant(next_attempt_at.as_deref()),
                });
            }
            Ok(queued)
        })
        .await
    }

    /// One queued entry, payload and all, or `None` if it has gone.
    ///
    /// The drain reads it at the moment it is about to send it, which is what keeps a
    /// payload that an earlier entry's settle rewrote -- `settle_create`,
    /// `settle_create_label` -- from going out in its old shape.
    ///
    /// # Errors
    /// As [`Self::pending`].
    pub async fn entry(&self, id: i64) -> Result<Option<OutboxEntry>> {
        let decodes = Arc::clone(&self.decodes);
        self.read(move |connection| {
            connection
                .query_row(
                    "SELECT id, created, payload, attempts, last_error, next_attempt_at, sent_at
                       FROM outbox WHERE id = ?1",
                    params![id],
                    StoredEntry::from_row,
                )
                .optional()?
                .map(|stored| stored.decode(&decodes))
                .transpose()
        })
        .await
    }

    /// The mutations of the entries named, by id. An entry that has gone is left out.
    ///
    /// # Errors
    /// As [`Self::pending`].
    pub async fn mutations(&self, ids: Vec<i64>) -> Result<HashMap<i64, Mutation>> {
        /// Well under SQLite's bound-parameter limit, which is 32,766 since 3.32.
        const CHUNK: usize = 500;
        let decodes = Arc::clone(&self.decodes);
        self.read(move |connection| {
            let mut found = HashMap::with_capacity(ids.len());
            for chunk in ids.chunks(CHUNK) {
                let marks = vec!["?"; chunk.len()].join(", ");
                let mut statement = connection.prepare(&format!(
                    "SELECT id, payload FROM outbox WHERE id IN ({marks})"
                ))?;
                let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (id, payload) = row?;
                    found.insert(id, decode(&decodes, &payload)?);
                }
            }
            Ok(found)
        })
        .await
    }

    /// How many mutations are waiting.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn pending_count(&self) -> Result<i64> {
        self.read(|connection| {
            Ok(connection.query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))?)
        })
        .await
    }

    /// How many entries are queued with an id of `id` or below.
    ///
    /// What the drain checks its own picture of the queue against: rows it has already
    /// seen change only through it, while anything new lands above `id`.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn pending_through(&self, id: i64) -> Result<i64> {
        self.read(move |connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM outbox WHERE id <= ?1",
                params![id],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// Whether this task has unsent local changes.
    ///
    /// A pull must not overwrite one: the server's copy is older than what the user is
    /// looking at, and replacing it would undo their edit in front of them.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn is_pending(&self, task: TaskId) -> Result<bool> {
        self.read(move |connection| {
            Ok(connection.query_row(
                "SELECT exists(SELECT 1 FROM outbox
                                WHERE subject_id = ?1 AND subject_kind = 'task')",
                params![task.get()],
                |row| row.get::<_, i64>(0),
            )? == 1)
        })
        .await
    }

    /// Whether this label has unsent local changes.
    ///
    /// The label half of [`Self::is_pending`], and a separate method rather than an
    /// argument to it because `outbox.subject_id` is an untyped column: provisional ids
    /// count down from `-1` per kind, so task `-1` and label `-1` are the same value in
    /// the same column and only `subject_kind` tells them apart.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn is_pending_label(&self, label: LabelId) -> Result<bool> {
        self.read(move |connection| {
            Ok(connection.query_row(
                "SELECT exists(SELECT 1 FROM outbox
                                WHERE subject_id = ?1 AND subject_kind = 'label')",
                params![label.get()],
                |row| row.get::<_, i64>(0),
            )? == 1)
        })
        .await
    }

    /// Record that `entry` is about to be put on the wire.
    ///
    /// Written before the request, not after the answer, because the answer is exactly
    /// what a killed process never stores. A create that finds this set on its next
    /// attempt reads before it writes instead of making a second task or label (BUG-33).
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn mark_sent(&self, entry: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.write(move |tx| {
            tx.execute(
                "UPDATE outbox SET sent_at = ?1 WHERE id = ?2",
                params![now, entry],
            )?;
            Ok(())
        })
        .await
    }

    /// Drop an entry the server accepted. The local row already says the right thing.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn complete(&self, entry: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM outbox WHERE id = ?1", params![entry])?;
            Ok(())
        })
        .await
    }

    /// Replace a locally created task with the one the server assigned an id to.
    ///
    /// The server's row goes in first, the provisional row's `task_labels` and
    /// `task_assignees` are re-pointed at it, and only then is the provisional row
    /// deleted. Order matters twice over: SQLite cascades a delete but not an update, so
    /// the children have to be moved while both rows exist -- and deleting first would
    /// cascade away links that only exist locally, such as a label attached by an entry
    /// still queued behind this create.
    ///
    /// Any entry still queued against the provisional id is retargeted, which is why a
    /// create must be the first thing drained for its task.
    ///
    /// Two cases do not store the server's answer as it came:
    ///
    /// - **The task was deleted before the create was sent.** The provisional row is gone
    ///   and a `DeleteTask` for it is queued, so writing row `assigned.id` would put back
    ///   a task the user deleted, and nothing but a full pull would remove it again. No
    ///   row is written; the delete is still retargeted, so the server's copy goes too,
    ///   and a refused delete restores `before` under the real id as any rollback does.
    /// - **Something else is still queued for it.** The answer is the task as created,
    ///   before the edits queued behind it, so storing it would show the pre-edit copy
    ///   until they are sent. The local row is kept instead, with the fields only the
    ///   server can assign taken from the answer.
    ///
    /// Cancelling the unsent create when the delete is queued was the alternative, and
    /// is not safe: a create whose request went out and whose answer was lost (BUG-33)
    /// may already exist on the server, and cancelling it would orphan that task with
    /// nothing queued to delete it. This way the server is always told.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure, or
    /// [`crate::CoreError::Encoding`] if a queued payload cannot be re-encoded.
    pub async fn settle_create(
        &self,
        entry: i64,
        provisional: TaskId,
        assigned: Task,
    ) -> Result<()> {
        let now = Utc::now();
        self.write(move |tx| {
            let local = read_task(tx, provisional)?;
            let queued_for = |kind: Option<&str>| -> Result<bool> {
                Ok(tx.query_row(
                    "SELECT exists(SELECT 1 FROM outbox
                                    WHERE subject_id = ?1 AND subject_kind = 'task'
                                      AND id != ?2 AND (?3 IS NULL OR kind = ?3))",
                    params![provisional.get(), entry, kind],
                    |row| row.get::<_, i64>(0),
                )? == 1)
            };
            let deleted = local.is_none() && queued_for(Some("delete_task"))?;
            if !deleted {
                let row = match local {
                    Some(mut local) if queued_for(None)? => {
                        local.id = assigned.id;
                        local.identifier.clone_from(&assigned.identifier);
                        local.index = assigned.index;
                        local.created = assigned.created;
                        local.updated = assigned.updated;
                        if local.created_by.is_none() {
                            local.created_by.clone_from(&assigned.created_by);
                        }
                        local
                    }
                    _ => assigned.clone(),
                };
                upsert_task(tx, &row, now)?;
            }
            // `OR IGNORE` because the server's answer may already carry the same link;
            // the row is then dropped with the provisional task below.
            tx.execute(
                "UPDATE OR IGNORE task_labels SET task_id = ?1 WHERE task_id = ?2",
                params![assigned.id.get(), provisional.get()],
            )?;
            tx.execute(
                "UPDATE OR IGNORE task_assignees SET task_id = ?1 WHERE task_id = ?2",
                params![assigned.id.get(), provisional.get()],
            )?;
            tx.execute(
                "DELETE FROM tasks WHERE id = ?1",
                params![provisional.get()],
            )?;
            tx.execute("DELETE FROM outbox WHERE id = ?1", params![entry])?;

            let queued: Vec<(i64, String)> = {
                let mut statement = tx.prepare(
                    "SELECT id, payload FROM outbox
                      WHERE subject_id = ?1 AND subject_kind = 'task'",
                )?;
                let rows = statement.query_map(params![provisional.get()], |row| {
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
                mutation.retarget(provisional, assigned.id);
                tx.execute(
                    "UPDATE outbox SET payload = ?1, subject_id = ?2 WHERE id = ?3",
                    params![serde_json::to_string(&mutation)?, assigned.id.get(), id],
                )?;
            }
            Ok(())
        })
        .await
    }

    /// Replace a locally created label with the one the server assigned an id to.
    ///
    /// The same order as [`Self::settle_create`], and for the same reason: the server's
    /// row goes in first, `task_labels` is re-pointed at it, and only then is the
    /// provisional row deleted, because SQLite cascades a delete but not an update.
    ///
    /// Unlike a task, the entries that have to move are **not** found by subject: an
    /// `AttachLabel` queued behind this create has the *task* as its subject and carries
    /// the label by value inside its payload. So every queued entry is re-encoded, and
    /// `subject_id` is rewritten too -- a queued mutation whose subject *is* the label,
    /// an `UpdateLabel`, must follow just the same. That path is exercised by
    /// `a_rename_queued_behind_a_create_follows_the_label_to_its_real_id`.
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
            // `OR IGNORE` because the label may already be attached under its real id --
            // an entry ahead of this one on the wire, or the server's own answer for the
            // create carrying the same link back. The provisional's `(task, -1)` row is
            // then left behind and the cascade below removes it; the real link survives
            // either way.
            tx.execute(
                "UPDATE OR IGNORE task_labels SET label_id = ?1 WHERE label_id = ?2",
                params![assigned.id.get(), provisional.get()],
            )?;
            tx.execute(
                "DELETE FROM labels WHERE id = ?1",
                params![provisional.get()],
            )?;
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

    /// Record a failed attempt and leave the entry queued for another try.
    ///
    /// For transport failures, rate limits and 5xx -- anything where the server may yet
    /// accept the change. The local row keeps the user's version meanwhile.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn defer(
        &self,
        entry: i64,
        error: String,
        retry_after: Option<Duration>,
    ) -> Result<()> {
        self.write(move |tx| {
            let attempts: i64 = tx
                .query_row(
                    "SELECT attempts FROM outbox WHERE id = ?1",
                    params![entry],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            // The server's `Retry-After` is honoured, but not further than tui-do's own
            // ceiling. It is a hint from a machine that may be misconfigured, and an
            // oversized one -- a proxy answering in milliseconds-as-seconds, say -- would
            // otherwise push `next_attempt_at` weeks out and silence this entry's
            // automatic retries entirely. `r` and `R` would still force it, but the user
            // has no way to know they need to.
            let wait = retry_after
                .unwrap_or_else(|| backoff(attempts + 1))
                .min(BACKOFF_CEILING);
            let next =
                Utc::now() + chrono::TimeDelta::from_std(wait).unwrap_or(chrono::TimeDelta::zero());
            tx.execute(
                "UPDATE outbox
                    SET attempts = attempts + 1, last_error = ?1, next_attempt_at = ?2
                  WHERE id = ?3",
                params![error, next.to_rfc3339(), entry],
            )?;
            Ok(())
        })
        .await
    }

    /// Undo an entry's local change and drop it, because the server refused it.
    ///
    /// For 4xx: the change is never going to be accepted, so the store must stop showing
    /// it. The caller tells the user; silently reverting a task in front of someone is
    /// worse than the rejection itself.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn discard(&self, entry: &OutboxEntry) -> Result<()> {
        self.discard_all(vec![entry.clone()]).await
    }

    /// Undo several entries and drop them, as one transaction.
    ///
    /// Rolled back newest first, which is the only order that reconstructs the original:
    /// two edits to one task leave it holding the second, and undoing the first without
    /// undoing the second would restore a state that never existed.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure. Nothing is undone unless
    /// everything is.
    pub async fn discard_all(&self, entries: Vec<OutboxEntry>) -> Result<()> {
        let now = Utc::now();
        self.write(move |tx| {
            let mut entries = entries;
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.id));
            for entry in &entries {
                entry.mutation.rollback(tx, now)?;
                tx.execute("DELETE FROM outbox WHERE id = ?1", params![entry.id])?;
            }
            Ok(())
        })
        .await
    }
}

/// Parse one stored payload, counting it. See [`Store::payload_decodes`].
fn decode(decodes: &DecodeCount, payload: &str) -> Result<Mutation> {
    decodes.bump();
    Ok(serde_json::from_str(payload)?)
}

/// Allocate the next provisional task id: -1, then -2, and so on.
///
/// Kept in `sync_state` rather than derived from `min(id)` so an id is never reused,
/// even after the row that held it has been settled and deleted.
fn next_local_id(tx: &Transaction<'_>) -> Result<TaskId> {
    let next: i64 = read_state(tx, NEXT_LOCAL_ID)?
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(-1);
    write_state(tx, NEXT_LOCAL_ID, &(next - 1).to_string())?;
    Ok(TaskId(next))
}

/// Whether an id was assigned locally and the server has never seen it.
#[must_use]
pub fn is_provisional(task: TaskId) -> bool {
    task.get() < 0
}

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::store::{LabelFilter, LabelSort};
    use tui_do_api::models::{LabelId, ProjectId, User, UserId};

    fn task(id: i64, title: &str) -> Task {
        Task {
            id: TaskId(id),
            project_id: ProjectId(1),
            title: title.to_string(),
            ..Task::default()
        }
    }

    fn label(id: i64, title: &str) -> Label {
        Label {
            id: LabelId(id),
            title: title.to_string(),
            ..Label::default()
        }
    }

    #[test]
    fn the_backoff_grows_and_then_stops_growing() {
        // Exponential from the floor so a blip costs seconds, capped so an outage that
        // lasted an hour does not leave the user waiting a day after it clears.
        assert_eq!(backoff(1), BACKOFF_FLOOR);
        assert_eq!(backoff(2), BACKOFF_FLOOR * 2);
        assert_eq!(backoff(3), BACKOFF_FLOOR * 4);
        assert_eq!(backoff(100), BACKOFF_CEILING, "it has to stop somewhere");
        // The shift is bounded before it is applied, so a large count cannot overflow it
        // into a small wait.
        assert_eq!(backoff(i64::MAX), BACKOFF_CEILING);
        assert_eq!(backoff(0), BACKOFF_FLOOR);
        assert_eq!(backoff(-1), BACKOFF_FLOOR);
    }

    #[tokio::test]
    async fn queueing_changes_the_store_and_records_the_change_together() {
        let store = Store::in_memory().unwrap();
        let entry = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "buy milk")),
            })
            .await
            .unwrap();

        // The UI can render it immediately, without waiting for a request.
        let stored = store
            .task(entry.mutation.subject().task().unwrap())
            .await
            .unwrap();
        assert_eq!(stored.expect("the task").title, "buy milk");
        assert_eq!(store.pending_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_queued_entry_records_what_kind_of_thing_it_acts_on() {
        // `subject_id` is a bare INTEGER and provisional ids count down from -1 for each
        // kind, so task -1 and label -1 are the same value in the same column. The kind
        // is what keeps `retain_tasks` and `settle_create` from confusing them.
        let store = Store::in_memory().unwrap();
        let entry = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "written")),
            })
            .await
            .unwrap();
        assert_eq!(entry.mutation.subject().kind(), "task");

        // Both halves: the kind, and that `subject_id` is the same id `subject()`
        // reports -- the placeholder that would drift if the five-argument `INSERT` in
        // `Store::queue` ever had `subject_id` and `subject_kind` swapped or misordered.
        let stored: Vec<(i64, String)> = store
            .read(|connection| {
                let mut statement =
                    connection.prepare("SELECT subject_id, subject_kind FROM outbox")?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap();
        assert_eq!(
            stored,
            vec![(entry.mutation.subject().id(), "task".to_string())]
        );
    }

    #[tokio::test]
    async fn a_created_task_gets_a_provisional_negative_id() {
        // Something has to identify it before the server has seen it, and negative ids
        // cannot collide with the positive ones Vikunja assigns.
        let store = Store::in_memory().unwrap();
        let first = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "one")),
            })
            .await
            .unwrap();
        let second = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "two")),
            })
            .await
            .unwrap();

        assert!(is_provisional(first.mutation.subject().task().unwrap()));
        assert_eq!(first.mutation.subject(), Subject::Task(TaskId(-1)));
        assert_eq!(second.mutation.subject(), Subject::Task(TaskId(-2)));
    }

    #[tokio::test]
    async fn provisional_ids_are_never_reused() {
        // A settled create frees its id; handing it out again would let a stale entry
        // retarget onto the wrong task.
        let store = Store::in_memory().unwrap();
        let first = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "one")),
            })
            .await
            .unwrap();
        store
            .settle_create(first.id, TaskId(-1), task(100, "one"))
            .await
            .unwrap();

        let second = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "two")),
            })
            .await
            .unwrap();
        assert_eq!(second.mutation.subject(), Subject::Task(TaskId(-2)));
    }

    #[tokio::test]
    async fn the_queue_drains_in_the_order_it_was_filled() {
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
        store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "original")),
                after: Box::new(task(1, "first edit")),
            })
            .await
            .unwrap();
        store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "first edit")),
                after: Box::new(task(1, "second edit")),
            })
            .await
            .unwrap();

        let pending = store.pending(None).await.unwrap();
        assert_eq!(pending.len(), 2);
        match &pending[0].mutation {
            Mutation::UpdateTask { after, .. } => assert_eq!(after.title, "first edit"),
            other => panic!("wrong entry first: {other:?}"),
        }
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "second edit"
        );
    }

    #[tokio::test]
    async fn settling_a_create_swaps_the_row_and_retargets_what_was_queued_behind_it() {
        // The edit made before the server answered must land on the real task, not on a
        // provisional id nothing will ever recognise.
        let store = Store::in_memory().unwrap();
        let created = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "buy milk")),
            })
            .await
            .unwrap();
        let provisional = created.mutation.subject().task().unwrap();
        let mut after = task(provisional.get(), "buy oat milk");
        after.assignees = vec![User {
            id: UserId(3),
            username: "alice".into(),
            ..User::default()
        }];
        store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(provisional.get(), "buy milk")),
                after: Box::new(after),
            })
            .await
            .unwrap();
        store
            .queue(Mutation::AttachLabel {
                task: provisional,
                label: Box::new(label(7, "errands")),
            })
            .await
            .unwrap();

        store
            .settle_create(created.id, provisional, task(4242, "buy oat milk"))
            .await
            .unwrap();

        assert!(store.task(provisional).await.unwrap().is_none());
        let settled = store
            .task(TaskId(4242))
            .await
            .unwrap()
            .expect("the settled task");
        // `task_labels` and `task_assignees` are re-pointed, not cascaded away, before the
        // provisional row is deleted -- without that, a label attached or a user assigned
        // by an entry still queued behind the create is lost the moment it settles.
        assert_eq!(
            settled.assignees.len(),
            1,
            "the assignee was cascaded away rather than re-pointed"
        );
        assert_eq!(settled.assignees[0].id, UserId(3));
        assert_eq!(
            settled.labels.len(),
            1,
            "the label link was cascaded away rather than re-pointed"
        );
        assert_eq!(settled.labels[0].id, LabelId(7));

        let pending = store.pending(None).await.unwrap();
        assert_eq!(pending.len(), 2, "the create should be gone, the rest kept");
        for entry in &pending {
            assert_eq!(
                entry.mutation.subject(),
                Subject::Task(TaskId(4242)),
                "an entry still points at the provisional id"
            );
        }
    }

    #[tokio::test]
    async fn a_rejected_update_puts_the_old_task_back() {
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
        let entry = store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "original")),
                after: Box::new(task(1, "edited")),
            })
            .await
            .unwrap();
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "edited"
        );

        store.discard(&entry).await.unwrap();

        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "original"
        );
        assert_eq!(store.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_rejected_create_leaves_nothing_behind() {
        let store = Store::in_memory().unwrap();
        let entry = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "doomed")),
            })
            .await
            .unwrap();
        store.discard(&entry).await.unwrap();

        assert!(store
            .task(entry.mutation.subject().task().unwrap())
            .await
            .unwrap()
            .is_none());
        assert_eq!(store.task_counts().await.unwrap(), (0, 0));
    }

    #[tokio::test]
    async fn a_rejected_delete_restores_the_task_with_its_assignees() {
        // The task body is what carries assignees, so a restore that dropped them would
        // unassign everyone -- the exact failure CLAUDE.md warns about.
        let store = Store::in_memory().unwrap();
        let mut original = task(1, "important");
        original.assignees = vec![User {
            id: UserId(3),
            username: "alice".into(),
            ..User::default()
        }];
        store.upsert_tasks(vec![original.clone()]).await.unwrap();

        let entry = store
            .queue(Mutation::DeleteTask {
                before: Box::new(original),
            })
            .await
            .unwrap();
        assert!(store.task(TaskId(1)).await.unwrap().is_none());

        store.discard(&entry).await.unwrap();
        let restored = store.task(TaskId(1)).await.unwrap().expect("restored");
        assert_eq!(restored.assignees.len(), 1);
        assert_eq!(restored.assignees[0].username, "alice");
    }

    #[tokio::test]
    async fn attaching_a_label_is_its_own_mutation_and_rolls_back_cleanly() {
        // Labels do not travel in the task body; they get their own endpoints, so they
        // get their own queue entries.
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "chores")]).await.unwrap();

        let entry = store
            .queue(Mutation::AttachLabel {
                task: TaskId(1),
                label: Box::new(label(7, "errands")),
            })
            .await
            .unwrap();
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().labels.len(),
            1
        );

        store.discard(&entry).await.unwrap();
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().labels.len(),
            0
        );
        // The label itself stays: it exists on the server regardless.
        assert!(store.label(LabelId(7)).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_rejection_takes_the_edits_queued_behind_it() {
        // Two edits to one task leave it holding the second. Undoing only the first
        // would restore a state that never existed, so both go, newest first.
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
        let first = store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "original")),
                after: Box::new(task(1, "first edit")),
            })
            .await
            .unwrap();
        let second = store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "first edit")),
                after: Box::new(task(1, "second edit")),
            })
            .await
            .unwrap();

        store.discard_all(vec![first, second]).await.unwrap();

        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "original"
        );
        assert_eq!(store.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_wild_retry_after_cannot_silence_an_entry() {
        // A misconfigured server or proxy can send a `Retry-After` of any size. Honouring
        // it verbatim pushed `next_attempt_at` arbitrarily far out and stopped this entry
        // retrying on its own, with nothing on screen to say so.
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "t")]).await.unwrap();
        let entry = store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "t")),
                after: Box::new(task(1, "edited")),
            })
            .await
            .unwrap();

        store
            .defer(
                entry.id,
                "slow down".into(),
                Some(Duration::from_secs(60 * 60 * 24 * 30)),
            )
            .await
            .unwrap();

        let pending = store.pending(None).await.unwrap();
        let next = pending[0]
            .next_attempt_at
            .expect("a deferred entry is scheduled");
        let wait = next - Utc::now();
        assert!(
            wait <= chrono::TimeDelta::from_std(BACKOFF_CEILING).unwrap(),
            "a 30-day Retry-After was honoured verbatim: {wait}"
        );
    }

    #[tokio::test]
    async fn a_deferred_entry_stays_queued_with_its_error() {
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "t")]).await.unwrap();
        let entry = store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "t")),
                after: Box::new(task(1, "edited")),
            })
            .await
            .unwrap();

        store
            .defer(entry.id, "connection refused".into(), None)
            .await
            .unwrap();

        let pending = store.pending(None).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].attempts, 1);
        assert_eq!(pending[0].last_error.as_deref(), Some("connection refused"));
        // The user still sees their edit while it waits.
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "edited"
        );
    }

    #[tokio::test]
    async fn an_accepted_entry_leaves_the_local_change_in_place() {
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "t")]).await.unwrap();
        let entry = store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "t")),
                after: Box::new(task(1, "edited")),
            })
            .await
            .unwrap();

        store.complete(entry.id).await.unwrap();
        assert_eq!(store.pending_count().await.unwrap(), 0);
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "edited"
        );
    }

    #[tokio::test]
    async fn pending_tasks_are_identifiable_so_a_pull_can_leave_them_alone() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_tasks(vec![task(1, "edited locally"), task(2, "untouched")])
            .await
            .unwrap();
        store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, "edited locally")),
                after: Box::new(task(1, "edited")),
            })
            .await
            .unwrap();

        assert!(store.is_pending(TaskId(1)).await.unwrap());
        assert!(!store.is_pending(TaskId(2)).await.unwrap());
    }

    #[tokio::test]
    async fn every_mutation_inverts_to_the_one_that_undoes_it() {
        let create = Mutation::CreateTask {
            task: Box::new(task(1, "t")),
        };
        assert!(matches!(
            create.inverse(),
            Some(Mutation::DeleteTask { .. })
        ));
        assert!(matches!(
            create.inverse().and_then(|back| back.inverse()),
            Some(Mutation::CreateTask { .. })
        ));

        let update = Mutation::UpdateTask {
            before: Box::new(task(1, "before")),
            after: Box::new(task(1, "after")),
        };
        match update.inverse() {
            Some(Mutation::UpdateTask { before, after }) => {
                assert_eq!(before.title, "after");
                assert_eq!(after.title, "before");
            }
            other => panic!("an inverted update should still be an update: {other:?}"),
        }

        let attach = Mutation::AttachLabel {
            task: TaskId(1),
            label: Box::new(label(7, "errands")),
        };
        assert!(matches!(
            attach.inverse(),
            Some(Mutation::DetachLabel { .. })
        ));
    }

    #[tokio::test]
    async fn undo_is_the_inverse_queued_like_any_other_change() {
        // Not a second mechanism: it is optimistic, it queues, and it can be undone.
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
        let done = Mutation::UpdateTask {
            before: Box::new(task(1, "original")),
            after: Box::new(task(1, "edited")),
        };
        store.queue(done.clone()).await.unwrap();

        store.queue(done.inverse().unwrap()).await.unwrap();

        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "original"
        );
        assert_eq!(
            store.pending_count().await.unwrap(),
            2,
            "the undo is a queued mutation of its own"
        );
    }

    #[tokio::test]
    async fn a_stored_mutation_survives_a_round_trip_through_json() {
        // Entries outlive the process that wrote them; a payload that cannot be parsed
        // is a lost edit.
        let store = Store::in_memory().unwrap();
        let mut original = task(1, "with everything");
        original.description = "notes".into();
        original.priority = 5;
        original.assignees = vec![User {
            id: UserId(3),
            username: "alice".into(),
            ..User::default()
        }];
        store.upsert_tasks(vec![original.clone()]).await.unwrap();

        let queued = Mutation::UpdateTask {
            before: Box::new(original.clone()),
            after: Box::new(task(1, "changed")),
        };
        store.queue(queued.clone()).await.unwrap();

        let read = store.pending(None).await.unwrap();
        assert_eq!(read[0].mutation, queued);
    }

    #[tokio::test]
    async fn a_failure_mid_queue_writes_neither_the_change_nor_the_entry() {
        // The property rule 5 rests on. Deleting a task that does not exist is not an
        // error, so the failure is forced with a foreign key: a label link needs a task.
        let store = Store::in_memory().unwrap();
        let result = store
            .queue(Mutation::AttachLabel {
                task: TaskId(999),
                label: Box::new(label(7, "errands")),
            })
            .await;

        assert!(result.is_err(), "a link to a missing task should fail");
        assert_eq!(store.pending_count().await.unwrap(), 0);
        let orphans: i64 = store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM task_labels", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[tokio::test]
    async fn a_created_label_gets_a_provisional_id_of_its_own() {
        // Its own counter, not the task counter: the kind column tells them apart, and
        // sharing one would make the two id spaces depend on each other for no gain.
        let store = Store::in_memory().unwrap();
        let first = store
            .queue(Mutation::CreateLabel {
                label: Box::new(Label {
                    title: "next".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        let Mutation::CreateLabel { label } = &first.mutation else {
            panic!("queue changed the mutation kind");
        };
        assert_eq!(label.id, LabelId(-1));
        assert!(is_provisional_label(label.id));
        assert_eq!(first.mutation.subject(), Subject::Label(LabelId(-1)));

        // And a task queued after it still gets -1 of its own.
        let task_entry = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "unrelated")),
            })
            .await
            .unwrap();
        assert_eq!(task_entry.mutation.subject(), Subject::Task(TaskId(-1)));

        // A second label create must not collide with the first: two labels both
        // allocated -1 would meet in `upsert_label`'s `ON CONFLICT (id) DO UPDATE`, and
        // the second create would silently overwrite the first's row while both entries
        // still sat in the queue.
        let second = store
            .queue(Mutation::CreateLabel {
                label: Box::new(Label {
                    title: "another".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        assert_eq!(second.mutation.subject(), Subject::Label(LabelId(-2)));
    }

    #[tokio::test]
    async fn queueing_a_label_writes_it_to_the_store_immediately() {
        // Rule 5: the local store is changed at the keystroke, not when the server answers.
        // Colour is asserted, not just title: this feature's whole point is create *and*
        // recolour, so a test that never checked `hex_color` would pass even if
        // `upsert_label` dropped it.
        let store = Store::in_memory().unwrap();
        store
            .queue(Mutation::CreateLabel {
                label: Box::new(Label {
                    title: "next".into(),
                    hex_color: "4287f5".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        let labels = store
            .labels(LabelFilter::default(), LabelSort::default())
            .await
            .unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].id, LabelId(-1));
        assert_eq!(labels[0].title, "next");
        assert_eq!(labels[0].hex_color, "4287f5");
    }

    #[tokio::test]
    async fn a_create_cannot_be_undone_because_there_is_no_delete() {
        let mutation = Mutation::CreateLabel {
            label: Box::new(Label {
                title: "next".into(),
                ..Default::default()
            }),
        };
        assert!(mutation.inverse().is_none());
    }

    #[tokio::test]
    async fn a_rejected_label_create_leaves_nothing_behind() {
        // The label equivalent of `a_rejected_create_leaves_nothing_behind`: apply then
        // rollback must leave the store exactly as it was before the create was queued.
        let store = Store::in_memory().unwrap();
        let entry = store
            .queue(Mutation::CreateLabel {
                label: Box::new(Label {
                    title: "doomed".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        assert_eq!(store.label_count().await.unwrap(), 1);

        store.discard(&entry).await.unwrap();

        assert!(store.label(LabelId(-1)).await.unwrap().is_none());
        assert_eq!(store.label_count().await.unwrap(), 0);
        assert_eq!(store.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn renaming_a_label_changes_the_store_at_once_and_rolls_back_cleanly() {
        // Rule 5 for the label half: the row the picker reads changes at the keystroke,
        // and a rejection puts back exactly what was there -- colour included, because
        // one variant covers rename and recolour and a rollback that only restored the
        // title would leave the user's old colour lost.
        let store = Store::in_memory().unwrap();
        let was = Label {
            id: LabelId(41),
            title: "next".into(),
            hex_color: "aaaaaa".into(),
            ..Default::default()
        };
        store.upsert_labels(vec![was.clone()]).await.unwrap();

        let entry = store
            .queue(Mutation::UpdateLabel {
                before: Box::new(was.clone()),
                after: Box::new(Label {
                    title: "next up".into(),
                    hex_color: "4287f5".into(),
                    ..was.clone()
                }),
            })
            .await
            .unwrap();

        let edited = store.label(LabelId(41)).await.unwrap().expect("the label");
        assert_eq!(edited.title, "next up");
        assert_eq!(edited.hex_color, "4287f5");
        assert_eq!(entry.mutation.subject(), Subject::Label(LabelId(41)));
        assert_eq!(entry.mutation.kind(), "update_label");

        store.discard(&entry).await.unwrap();

        let restored = store.label(LabelId(41)).await.unwrap().expect("the label");
        assert_eq!(restored.title, "next");
        assert_eq!(restored.hex_color, "aaaaaa");
        assert_eq!(store.pending_count().await.unwrap(), 0);
    }

    #[test]
    fn a_label_rename_inverts_to_the_rename_that_undoes_it() {
        let rename = Mutation::UpdateLabel {
            before: Box::new(label(41, "next")),
            after: Box::new(label(41, "next up")),
        };
        match rename.inverse() {
            Some(Mutation::UpdateLabel { before, after }) => {
                assert_eq!(before.title, "next up");
                assert_eq!(after.title, "next");
            }
            other => panic!("an inverted rename should still be a rename: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rename_queued_behind_a_create_follows_the_label_to_its_real_id() {
        // The case `retarget_label` exists for, and the one a rename can produce on its
        // own: create a label, rename it before the server has answered. Both halves of
        // the rename carry the provisional id, and an entry left naming it would be sent
        // as `POST /labels/-1` -- a 404 about an id no server has ever seen, which is a
        // 4xx, which rolls the user's rename back.
        let store = Store::in_memory().unwrap();
        let created = store
            .queue(Mutation::CreateLabel {
                label: Box::new(Label {
                    title: "next".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        let provisional = LabelId(-1);
        store
            .queue(Mutation::UpdateLabel {
                before: Box::new(Label {
                    id: provisional,
                    title: "next".into(),
                    ..Default::default()
                }),
                after: Box::new(Label {
                    id: provisional,
                    title: "next up".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();

        store
            .settle_create_label(
                created.id,
                provisional,
                Label {
                    id: LabelId(41),
                    title: "next".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let queued = store.pending(None).await.unwrap();
        assert_eq!(queued.len(), 1, "the create should have been dropped");
        let Mutation::UpdateLabel { before, after } = &queued[0].mutation else {
            panic!("the rename is gone");
        };
        // Both halves: `before` is what a rejection restores and what an undo re-applies,
        // so a `before` left on the provisional id would resurrect a row the store has
        // deleted.
        assert_eq!(
            before.id,
            LabelId(41),
            "`before` still names the provisional"
        );
        assert_eq!(after.id, LabelId(41), "`after` still names the provisional");
        // And the row the queue is indexed by has to move with the payload, or the drain
        // blocks the wrong subject and `retain_labels` exempts the wrong label.
        assert_eq!(
            queued[0].mutation.subject(),
            Subject::Label(LabelId(41)),
            "the outbox subject still names the provisional"
        );
    }

    #[tokio::test]
    async fn adopting_a_label_moves_everything_that_named_it() {
        let store = Store::in_memory().unwrap();
        // `task_labels.task_id` is a foreign key; the brief's snippet queues an attach
        // against task 7 without the row existing first, which the schema does not allow.
        store.upsert_tasks(vec![task(7, "chores")]).await.unwrap();
        let created = store
            .queue(Mutation::CreateLabel {
                label: Box::new(Label {
                    id: LabelId(0),
                    title: "next".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        let provisional = LabelId(-1);

        // An attach queued behind it, whose subject is the task and whose payload carries
        // the provisional label by value. This is the entry a subject lookup cannot find.
        store
            .queue(Mutation::AttachLabel {
                task: TaskId(7),
                label: Box::new(Label {
                    id: provisional,
                    title: "next".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();

        store
            .settle_create_label(
                created.id,
                provisional,
                Label {
                    id: LabelId(41),
                    title: "next".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let queued = store.pending(None).await.unwrap();
        assert_eq!(queued.len(), 1, "the create should have been dropped");
        let Mutation::AttachLabel { label, .. } = &queued[0].mutation else {
            panic!("the attach is gone");
        };
        assert_eq!(
            label.id,
            LabelId(41),
            "the attach still names the provisional label"
        );

        let labels = store
            .labels(LabelFilter::default(), LabelSort::default())
            .await
            .unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].id, LabelId(41));

        // The re-point, not just the eventual state: without the `UPDATE OR IGNORE
        // task_labels` statement, `DELETE FROM labels WHERE id = -1` two lines below it
        // cascades the (7, -1) link away, and the assertions above would not have caught
        // it -- they only look at the outbox payload and the `labels` table, not the task.
        let read = store.task(TaskId(7)).await.unwrap().unwrap();
        assert_eq!(read.labels.len(), 1, "the attachment was cascaded away");
        assert_eq!(read.labels[0].id, LabelId(41));
    }

    /// Queue a provisional label, attach it to stored task 1, and hand back the create
    /// entry with the task as it now reads -- carrying label `-1`.
    async fn task_carrying_a_provisional_label(store: &Store) -> (OutboxEntry, Task) {
        store.upsert_tasks(vec![task(1, "chores")]).await.unwrap();
        let create = store
            .queue(Mutation::CreateLabel {
                label: Box::new(label(0, "errands")),
            })
            .await
            .unwrap();
        let Mutation::CreateLabel { label: created } = &create.mutation else {
            panic!("the create changed shape");
        };
        assert_eq!(created.id, LabelId(-1));
        store
            .queue(Mutation::AttachLabel {
                task: TaskId(1),
                label: created.clone(),
            })
            .await
            .unwrap();
        let carrying = store.task(TaskId(1)).await.unwrap().expect("the task");
        assert_eq!(carrying.labels.len(), 1);
        assert_eq!(carrying.labels[0].id, LabelId(-1));
        (create, carrying)
    }

    async fn negative_label_ids(store: &Store) -> Vec<i64> {
        store
            .read(|connection| {
                let mut statement = connection.prepare("SELECT id FROM labels WHERE id < 0")?;
                let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_rolled_back_delete_does_not_resurrect_a_rolled_back_label() {
        // BUG-14. `references` leaves a `DeleteTask` out of a rejected create's discard,
        // so its `before` still names label -1 after the create's rollback deleted it.
        // Rolling the delete back re-ran `upsert_task(before)` and wrote label -1 again:
        // a row with no queued create, so it never settles and never syncs.
        let store = Store::in_memory().unwrap();
        let (create, carrying) = task_carrying_a_provisional_label(&store).await;
        let delete = store
            .queue(Mutation::DeleteTask {
                before: Box::new(carrying),
            })
            .await
            .unwrap();

        store.discard(&create).await.unwrap();
        store.discard(&delete).await.unwrap();

        assert_eq!(negative_label_ids(&store).await, Vec::<i64>::new());
        let restored = store.task(TaskId(1)).await.unwrap().expect("restored");
        assert_eq!(restored.title, "chores");
        assert!(restored.labels.is_empty());
    }

    #[tokio::test]
    async fn a_rolled_back_update_does_not_resurrect_a_rolled_back_label() {
        // The same hole through `UpdateTask`: its rollback also runs
        // `upsert_task(before)`, and `references` leaves it queued for the same reason.
        let store = Store::in_memory().unwrap();
        let (create, carrying) = task_carrying_a_provisional_label(&store).await;
        let mut renamed = carrying.clone();
        renamed.title = "chores, renamed".into();
        let update = store
            .queue(Mutation::UpdateTask {
                before: Box::new(carrying),
                after: Box::new(renamed),
            })
            .await
            .unwrap();

        store.discard(&create).await.unwrap();
        store.discard(&update).await.unwrap();

        assert_eq!(negative_label_ids(&store).await, Vec::<i64>::new());
        let restored = store.task(TaskId(1)).await.unwrap().expect("restored");
        assert_eq!(restored.title, "chores");
        assert!(restored.labels.is_empty());
    }

    #[tokio::test]
    async fn a_rolled_back_detach_does_not_resurrect_a_rolled_back_label() {
        // `DetachLabel`'s rollback writes the label back too. A detach queued while the
        // create is alive is discarded with it, but one queued after -- an undo of the
        // attach, built from a copy still naming -1 -- is not. `queue` now refuses that
        // detach (BUG-27), so the entry is built by hand: the rollback guard stands on
        // its own, for a row written by a build that did not refuse it.
        let store = Store::in_memory().unwrap();
        let (create, carrying) = task_carrying_a_provisional_label(&store).await;
        store.discard(&create).await.unwrap();
        let detach = OutboxEntry {
            id: 9_999,
            created: Utc::now(),
            mutation: Mutation::DetachLabel {
                task: TaskId(1),
                label: Box::new(carrying.labels[0].clone()),
            },
            attempts: 0,
            last_error: None,
            next_attempt_at: None,
            sent_at: None,
        };

        store.discard(&detach).await.unwrap();

        assert_eq!(negative_label_ids(&store).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn a_label_change_naming_a_label_never_created_is_not_queued() {
        // BUG-27. Once a create is gone, no server will issue its id, so an attach or a
        // detach naming it is a 4xx that discards every other entry queued for the task.
        // Undoing a delete reaches the same attach through `decompose`.
        let store = Store::in_memory().unwrap();
        let (create, carrying) = task_carrying_a_provisional_label(&store).await;
        store.discard(&create).await.unwrap();
        let dead = Box::new(carrying.labels[0].clone());
        let before = store.pending(None).await.unwrap();

        for mutation in [
            Mutation::DetachLabel {
                task: TaskId(1),
                label: dead.clone(),
            },
            Mutation::AttachLabel {
                task: TaskId(1),
                label: dead.clone(),
            },
            Mutation::CreateTask {
                task: Box::new(carrying.clone()),
            },
        ] {
            let kind = mutation.kind();
            match store.queue(mutation).await {
                Err(crate::CoreError::Refused(reason)) => assert!(reason.contains("errands")),
                other => panic!("{kind} naming a dead label was not refused: {other:?}"),
            }
        }
        assert_eq!(
            store.pending(None).await.unwrap(),
            before,
            "a refusal left something queued"
        );
        assert_eq!(negative_label_ids(&store).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn a_provisional_label_still_being_created_survives_a_rolled_back_delete() {
        // The guard keys on the queued create, not on the sign of the id: while the
        // `CreateLabel` is queued, label -1 is real local state and a restored task has
        // to get it back.
        let store = Store::in_memory().unwrap();
        let (_create, carrying) = task_carrying_a_provisional_label(&store).await;
        let delete = store
            .queue(Mutation::DeleteTask {
                before: Box::new(carrying),
            })
            .await
            .unwrap();

        store.discard(&delete).await.unwrap();

        assert_eq!(negative_label_ids(&store).await, vec![-1]);
        let restored = store.task(TaskId(1)).await.unwrap().expect("restored");
        assert_eq!(restored.labels.len(), 1);
        assert_eq!(restored.labels[0].id, LabelId(-1));
    }

    #[tokio::test]
    async fn a_queued_task_create_does_not_pass_for_a_label_create() {
        // `subject_id` is untyped: provisional task -1 and label -1 share a value, so a
        // queued `CreateTask` for task -1 must not keep label -1 alive.
        let store = Store::in_memory().unwrap();
        let (create, carrying) = task_carrying_a_provisional_label(&store).await;
        store.discard(&create).await.unwrap();
        let made = store
            .queue(Mutation::CreateTask {
                task: Box::new(task(0, "other")),
            })
            .await
            .unwrap();
        assert_eq!(made.mutation.subject().task(), Some(TaskId(-1)));
        let delete = store
            .queue(Mutation::DeleteTask {
                before: Box::new(carrying),
            })
            .await
            .unwrap();

        store.discard(&delete).await.unwrap();

        assert_eq!(negative_label_ids(&store).await, Vec::<i64>::new());
    }
}
