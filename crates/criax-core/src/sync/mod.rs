//! The sync engine: the only thing in criax that talks to the server.
//!
//! It runs in two directions and they are deliberately separate calls.
//! [`Sync::push`] drains the outbox, oldest entry first. [`Sync::pull`] refreshes the
//! store from the server. [`Sync::once`] does the first and then the second, which is
//! the order that matters: sending local changes before reading means the answer already
//! contains them, instead of arriving stale and having to be skipped.
//!
//! Nothing here decides *when* to run. The effect runtime owns the timer, because a
//! sync engine with its own loop inside it is a second scheduler to reason about and
//! the one thing this architecture is trying to avoid is code that decides for itself
//! when to block.
//!
//! # A failed push stops the queue rather than skipping past it
//!
//! Entries are ordered, and later ones assume earlier ones landed -- an edit to a task
//! the server has not been told about yet cannot be sent. So a transient failure ends
//! the pass and leaves the rest queued. Only a *rejection* removes an entry, and it
//! takes everything queued behind it for the same task with it: those changes were built
//! on a state the server has refused to have.
//!
//! # What counts as permanent
//!
//! A 4xx is the server's considered answer and retrying it will get the same one, so the
//! change is rolled back and the user is told. Everything else -- a dropped connection,
//! a rate limit, a 5xx, an unparseable response -- keeps the entry. That last case is a
//! judgment call: a response we could not read may mean the write *did* land, so a retry
//! risks a duplicate. A duplicate task is an annoyance and a lost one is a bug report, so
//! the entry stays.

use criax_api::models::{ProjectId, Task, TaskId};
use criax_api::{ApiError, Client, TaskQuery};
use tokio::sync::mpsc::UnboundedSender;

use crate::error::Result;
use crate::store::{Mutation, OutboxEntry, Store, CURRENT_USER, LAST_PULL, PAGE_CAP};

/// Which direction a pass is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Sending queued local changes.
    Push,
    /// Reading the server's state into the store.
    Pull,
}

/// Which collection a pull is working through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Projects and their metadata.
    Projects,
    /// Labels.
    Labels,
    /// Tasks, which is the paginated one.
    Tasks,
}

/// What a push did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PushReport {
    /// Entries the server accepted.
    pub sent: usize,
    /// Entries the server refused, which have been rolled back.
    pub rejected: usize,
    /// Entries left queued for another attempt.
    pub deferred: usize,
}

impl PushReport {
    /// Whether anything is still waiting to be sent.
    #[must_use]
    pub fn is_complete(self) -> bool {
        self.deferred == 0
    }
}

/// What a pull did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullReport {
    /// Projects stored.
    pub projects: usize,
    /// Labels stored.
    pub labels: usize,
    /// Tasks stored.
    pub tasks: usize,
    /// Tasks left alone because they have unsent local changes.
    pub skipped: usize,
    /// Rows removed because the server no longer has them.
    pub removed: usize,
}

/// What a full pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// The push half.
    pub push: PushReport,
    /// The pull half.
    pub pull: PullReport,
}

/// Something worth telling the UI about.
///
/// The effect runtime turns these into `Msg`s. They are a plain enum rather than a
/// callback so that the engine has no idea a UI exists.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncEvent {
    /// A pass began.
    Started(Phase),

    /// A pull is working through a collection.
    Progress {
        /// Which collection.
        stage: Stage,
        /// How many rows have been stored so far.
        stored: usize,
        /// How many pages there are, when the server said.
        pages: Option<u32>,
    },

    /// The server refused a queued change, which has been rolled back.
    ///
    /// The only event the user must be shown: their edit has just disappeared from the
    /// screen and they are owed an explanation.
    Rejected {
        /// The task the change was about.
        subject: TaskId,
        /// What the change was, as [`Mutation::kind`] spells it.
        kind: String,
        /// What the server said.
        message: String,
    },

    /// A pass finished.
    Finished(SyncReport),

    /// A pass could not complete. Queued changes are still queued.
    Failed {
        /// Which half failed.
        phase: Phase,
        /// What went wrong.
        message: String,
    },
}

/// What the server did with one entry.
enum Sent {
    /// A task was created, and this is the server's copy of it.
    Created(Box<Task>),
    /// A task was updated, and this is the server's copy of it.
    Updated(Box<Task>),
    /// It landed and there is nothing to store.
    Done,
}

/// Reconciles the local store with a Vikunja server.
#[derive(Debug, Clone)]
pub struct Sync {
    client: Client,
    store: Store,
    events: Option<UnboundedSender<SyncEvent>>,
}

impl Sync {
    /// Build an engine over a client and a store.
    #[must_use]
    pub fn new(client: Client, store: Store) -> Self {
        Self {
            client,
            store,
            events: None,
        }
    }

    /// Report progress to `events`.
    ///
    /// Optional so a test, or a one-shot CLI command, can run a pass without wiring up a
    /// channel to drain.
    #[must_use]
    pub fn with_events(mut self, events: UnboundedSender<SyncEvent>) -> Self {
        self.events = Some(events);
        self
    }

    /// Push, then pull.
    ///
    /// A push failure does not skip the pull: the server still has news, and the queue
    /// keeps what could not be sent.
    ///
    /// # Errors
    /// Whatever the pull returns. A push failure is reported as an event and in the
    /// report rather than as an error, because the queue surviving *is* the handling.
    pub async fn once(&self) -> Result<SyncReport> {
        let push = self.push().await?;
        let pull = self.pull().await?;
        let report = SyncReport { push, pull };
        self.emit(SyncEvent::Finished(report));
        Ok(report)
    }

    /// Drain the outbox, oldest entry first.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] if the queue itself cannot be read or written. A
    /// server failure is not an error here: it is recorded against the entry, which
    /// stays queued.
    pub async fn push(&self) -> Result<PushReport> {
        self.emit(SyncEvent::Started(Phase::Push));
        let mut report = PushReport::default();
        let mut previous: Option<i64> = None;

        loop {
            let pending = self.store.pending(None).await?;
            let Some(entry) = pending.first().cloned() else {
                break;
            };
            // An entry that survives a successful send would spin this loop against the
            // server forever. It cannot happen, and if it ever does it stops here.
            if previous == Some(entry.id) {
                tracing::error!(entry = entry.id, "a sent entry stayed queued; stopping");
                break;
            }
            previous = Some(entry.id);

            match self.deliver(&entry).await? {
                Ok(()) => report.sent += 1,
                Err(error) if is_permanent(&error) => {
                    let subject = entry.mutation.subject();
                    let message = error.to_string();
                    // Everything queued behind this for the same task was built on a
                    // state the server has refused to have, so it goes too.
                    let doomed: Vec<OutboxEntry> = pending
                        .into_iter()
                        .filter(|queued| queued.mutation.subject() == subject)
                        .collect();
                    report.rejected += doomed.len();
                    self.store.discard_all(doomed).await?;
                    tracing::warn!(%subject, error = %message, "the server refused a queued change");
                    self.emit(SyncEvent::Rejected {
                        subject,
                        kind: entry.mutation.kind().to_string(),
                        message,
                    });
                }
                Err(error) => {
                    // An ordered queue: stop rather than send what comes after it.
                    let message = error.to_string();
                    self.store.defer(entry.id, message.clone()).await?;
                    report.deferred = pending.len();
                    tracing::debug!(error = %message, "deferring the queue");
                    self.emit(SyncEvent::Failed {
                        phase: Phase::Push,
                        message,
                    });
                    break;
                }
            }
        }

        Ok(report)
    }

    /// Send one entry and settle it in the store.
    ///
    /// The two failure kinds are kept apart in the signature: the outer `Err` is a local
    /// store failure, which is a real error and ends the pass, while the inner one is
    /// the server's answer, which the caller weighs against [`is_permanent`].
    async fn deliver(&self, entry: &OutboxEntry) -> Result<std::result::Result<(), ApiError>> {
        let outcome = match self.transmit(entry).await {
            Ok(outcome) => outcome,
            Err(error) => return Ok(Err(error)),
        };

        match (outcome, &entry.mutation) {
            (Sent::Created(assigned), Mutation::CreateTask { task }) => {
                self.store
                    .settle_create(entry.id, task.id, *assigned)
                    .await?;
            }
            (Sent::Updated(updated), Mutation::UpdateTask { after, .. }) => {
                self.store.complete(entry.id).await?;
                // The server fills in `updated`, `index` and `identifier`, so its answer
                // is worth keeping -- but only once nothing else is queued for this
                // task, or storing it would briefly undo an edit already made.
                if !self.store.is_pending(after.id).await? {
                    self.store.upsert_tasks(vec![*updated]).await?;
                }
            }
            _ => self.store.complete(entry.id).await?,
        }
        Ok(Ok(()))
    }

    /// Make the request, and nothing else.
    async fn transmit(&self, entry: &OutboxEntry) -> std::result::Result<Sent, ApiError> {
        Ok(match &entry.mutation {
            Mutation::CreateTask { task } => {
                let assigned = self.client.create_task(task.project_id, task).await?;
                // The body's `labels` field is ignored by the server, so labels the task
                // was created with have to be attached one at a time. Doing it here
                // rather than making callers queue a second mutation is what keeps "I
                // typed *errands and it vanished" from being a bug report.
                for label in &task.labels {
                    self.client.add_label_to_task(assigned.id, label.id).await?;
                }
                Sent::Created(Box::new(with_labels(assigned, task)))
            }
            Mutation::UpdateTask { before, after } => {
                let updated = self.client.update_task(after).await?;
                self.sync_labels(after.id, before, after).await?;
                Sent::Updated(Box::new(with_labels(updated, after)))
            }
            Mutation::DeleteTask { before } => {
                self.client.delete_task(before.id).await?;
                Sent::Done
            }
            Mutation::AttachLabel { task, label } => {
                self.client.add_label_to_task(*task, label.id).await?;
                Sent::Done
            }
            Mutation::DetachLabel { task, label } => {
                self.client.remove_label_from_task(*task, label.id).await?;
                Sent::Done
            }
        })
    }

    /// Attach and detach whatever an edit changed about a task's labels.
    ///
    /// `POST /tasks/{id}` ignores the body's `labels`, so an edit that added one would
    /// silently lose it. Assignees need none of this: they *are* the body.
    async fn sync_labels(
        &self,
        task: TaskId,
        before: &Task,
        after: &Task,
    ) -> std::result::Result<(), ApiError> {
        for label in &after.labels {
            if !before.labels.iter().any(|had| had.id == label.id) {
                self.client.add_label_to_task(task, label.id).await?;
            }
        }
        for label in &before.labels {
            if !after.labels.iter().any(|keeps| keeps.id == label.id) {
                self.client.remove_label_from_task(task, label.id).await?;
            }
        }
        Ok(())
    }

    /// Refresh the store from the server.
    ///
    /// Tasks with unsent local changes are left alone, and so are tasks the server has
    /// never seen: a task created offline must survive the pull that happens the moment
    /// the connection comes back.
    ///
    /// # Errors
    /// Any API failure, or [`crate::CoreError::Store`] if the store cannot be written.
    /// A pull is all-or-nothing about its own bookkeeping: [`crate::store::LAST_PULL`]
    /// only moves when every stage finished.
    pub async fn pull(&self) -> Result<PullReport> {
        self.emit(SyncEvent::Started(Phase::Pull));
        let mut report = PullReport::default();

        match self.pull_inner(&mut report).await {
            Ok(()) => {
                self.store
                    .set_state(LAST_PULL, chrono::Utc::now().to_rfc3339())
                    .await?;
                Ok(report)
            }
            Err(error) => {
                self.emit(SyncEvent::Failed {
                    phase: Phase::Pull,
                    message: error.to_string(),
                });
                Err(error)
            }
        }
    }

    async fn pull_inner(&self, report: &mut PullReport) -> Result<()> {
        // The page cap comes from the server, never from a constant. Reading it also
        // teaches the client's paginators what to ask for.
        let info = self.client.info().await?;
        self.store
            .set_state(PAGE_CAP, self.client.page_size().to_string())
            .await?;
        tracing::debug!(version = %info.version, cap = self.client.page_size(), "server info");

        let user = self.client.current_user().await?;
        self.store
            .set_state(CURRENT_USER, user.id.to_string())
            .await?;

        let projects = self.client.all_projects().await?;
        let project_ids: Vec<ProjectId> = projects.iter().map(|project| project.id).collect();
        report.projects = self.store.upsert_projects(projects).await?;
        report.removed += self.store.retain_projects(project_ids).await?;
        self.emit(SyncEvent::Progress {
            stage: Stage::Projects,
            stored: report.projects,
            pages: None,
        });

        let labels = self.client.all_labels().await?;
        let label_ids = labels.iter().map(|label| label.id).collect();
        report.labels = self.store.upsert_labels(labels).await?;
        report.removed += self.store.retain_labels(label_ids).await?;
        self.emit(SyncEvent::Progress {
            stage: Stage::Labels,
            stored: report.labels,
            pages: None,
        });

        // Unfiltered: every task the user can see, done ones included. A view would not
        // do -- a project's default List view filters `done = false`, so pulling through
        // one would make every completed task look deleted to the retain step below.
        let mut pager = self.client.tasks(&TaskQuery::new())?;
        let mut seen: Vec<TaskId> = Vec::new();
        while let Some(page) = pager.next_page().await? {
            seen.extend(page.items.iter().map(|task| task.id));
            let applied = self.store.upsert_tasks_from_server(page.items).await?;
            report.tasks += applied.stored;
            report.skipped += applied.skipped;
            self.emit(SyncEvent::Progress {
                stage: Stage::Tasks,
                stored: report.tasks,
                pages: pager.total_pages(),
            });
        }
        report.removed += self.store.retain_tasks(Vec::new(), seen).await?;

        Ok(())
    }

    /// Fetch and store one project's views.
    ///
    /// Separate from [`Sync::pull`] on purpose: views come from their own endpoint, one
    /// request per project, and a pull that fetched them for all thirty projects on the
    /// dev instance would spend thirty requests on data only the project currently on
    /// screen needs.
    ///
    /// # Errors
    /// Any API failure, or [`crate::CoreError::Store`].
    pub async fn pull_project_views(&self, project: ProjectId) -> Result<usize> {
        let views = self.client.project_views(project).await?;
        let count = views.len();
        self.store.set_project_views(project, views).await?;
        Ok(count)
    }

    /// Send an event, if anyone is listening.
    ///
    /// A closed channel means the UI is gone, which is not this engine's problem.
    fn emit(&self, event: SyncEvent) {
        if let Some(events) = &self.events {
            let _ = events.send(event);
        }
    }
}

/// Whether the server has given its final answer.
///
/// 4xx only. A rate limit, a 5xx, a dropped connection or an unreadable body all leave
/// the entry queued -- see the module note on why the unreadable one goes this way.
fn is_permanent(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Rejected { .. } | ApiError::Forbidden { .. }
    )
}

/// Carry the labels an edit intended onto the server's answer.
///
/// The server does not echo labels in a task write's response -- they are not part of
/// that body in either direction -- so storing its answer verbatim would drop them from
/// the local row until the next pull.
fn with_labels(mut task: Task, intended: &Task) -> Task {
    task.labels = intended.labels.clone();
    task
}
