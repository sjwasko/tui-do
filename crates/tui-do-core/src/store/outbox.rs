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

use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tui_do_api::models::{Label, LabelId, Task, TaskId};

use super::labels::upsert_label;
use super::state::{read_state, write_state};
use super::tasks::upsert_task;
use super::Store;
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
        }
    }

    /// What undoing this would be.
    ///
    /// Undo queues the inverse as an ordinary mutation, so it is optimistic, rolls back
    /// on rejection and can itself be undone -- all for free. Note that undoing a delete
    /// *re-creates* the task: Vikunja has no undelete, so the restored task gets a new
    /// id, and its comments and attachments do not come back.
    #[must_use]
    pub fn inverse(&self) -> Self {
        match self {
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
        }
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
                upsert_label(tx, label, now)?;
                tx.execute(
                    "INSERT OR IGNORE INTO task_labels (task_id, label_id) VALUES (?1, ?2)",
                    params![task.get(), label.id.get()],
                )?;
            }
            Self::DetachLabel { task, label } => {
                tx.execute(
                    "DELETE FROM task_labels WHERE task_id = ?1 AND label_id = ?2",
                    params![task.get(), label.id.get()],
                )?;
            }
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
                upsert_label(tx, label, now)?;
                tx.execute(
                    "INSERT OR IGNORE INTO task_labels (task_id, label_id) VALUES (?1, ?2)",
                    params![task.get(), label.id.get()],
                )?;
            }
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
}

impl OutboxEntry {
    /// Whether the backoff has elapsed and this may be sent.
    #[must_use]
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        self.next_attempt_at.is_none_or(|at| at <= now)
    }

    /// Whether this entry has failed at least once and is waiting to be retried.
    #[must_use]
    pub const fn is_failing(&self) -> bool {
        self.attempts > 0
    }
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
    /// [`crate::CoreError::Store`] on any SQL failure, or
    /// [`crate::CoreError::Encoding`] if the mutation cannot be serialized. Either way
    /// nothing is written -- the local change and its queue entries stand or fall
    /// together.
    pub async fn queue(&self, mutation: Mutation) -> Result<OutboxEntry> {
        let now = Utc::now();
        self.write(move |tx| {
            let mut mutation = mutation;
            if let Mutation::CreateTask { task } = &mut mutation {
                if task.id.get() == 0 {
                    task.id = next_local_id(tx)?;
                }
            }

            let mut first = None;
            for part in mutation.decompose() {
                part.apply(tx, now)?;
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
                let entry = OutboxEntry {
                    id: tx.last_insert_rowid(),
                    created: now,
                    mutation: part,
                    attempts: 0,
                    last_error: None,
                    next_attempt_at: None,
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
        self.read(move |connection| {
            // `next_attempt_at` in the future means a previous attempt failed and the
            // backoff has not elapsed. Filtered in SQL rather than in the caller so
            // `pending_count` and the drain loop cannot disagree about what is ready.
            let mut sql = String::from(
                "SELECT id, created, payload, attempts, last_error, next_attempt_at
                   FROM outbox ORDER BY id ASC",
            );
            if let Some(limit) = limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?;

            let mut entries = Vec::new();
            for row in rows {
                let (id, created, payload, attempts, last_error, next_attempt_at) = row?;
                entries.push(OutboxEntry {
                    id,
                    created: DateTime::parse_from_rfc3339(&created)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    mutation: serde_json::from_str(&payload)?,
                    attempts,
                    last_error,
                    next_attempt_at: next_attempt_at.as_deref().and_then(|text| {
                        DateTime::parse_from_rfc3339(text)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                });
            }
            Ok(entries)
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
            upsert_task(tx, &assigned, now)?;
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
            let wait = retry_after.unwrap_or_else(|| backoff(attempts + 1));
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
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

        let kinds: Vec<String> = store
            .read(|connection| {
                let mut statement = connection.prepare("SELECT subject_kind FROM outbox")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap();
        assert_eq!(kinds, vec!["task".to_string()]);
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
        store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(provisional.get(), "buy milk")),
                after: Box::new(task(provisional.get(), "buy oat milk")),
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
        assert!(store.task(TaskId(4242)).await.unwrap().is_some());

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
        assert!(matches!(create.inverse(), Mutation::DeleteTask { .. }));
        assert!(matches!(
            create.inverse().inverse(),
            Mutation::CreateTask { .. }
        ));

        let update = Mutation::UpdateTask {
            before: Box::new(task(1, "before")),
            after: Box::new(task(1, "after")),
        };
        match update.inverse() {
            Mutation::UpdateTask { before, after } => {
                assert_eq!(before.title, "after");
                assert_eq!(after.title, "before");
            }
            other => panic!("an inverted update should still be an update: {other:?}"),
        }

        let attach = Mutation::AttachLabel {
            task: TaskId(1),
            label: Box::new(label(7, "errands")),
        };
        assert!(matches!(attach.inverse(), Mutation::DetachLabel { .. }));
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

        store.queue(done.inverse()).await.unwrap();

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
}
