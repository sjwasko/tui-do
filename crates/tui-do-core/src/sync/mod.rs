//! The sync engine: the only thing in tui-do that talks to the server.
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
//! # One entry is one request
//!
//! Every queued mutation maps to a single call, which is what makes a retry safe. An
//! entry that took two -- create the task, then attach its label -- could have the first
//! land and the second fail, and replaying it would create the task twice. The splitting
//! happens where the entries are written, in [`crate::store::Mutation::decompose`], so
//! that this loop never has to ask what has already happened.
//!
//! # What counts as permanent
//!
//! A 4xx is the server's considered answer and retrying it will get the same one, so the
//! change is rolled back and the user is told. Everything else -- a dropped connection,
//! a rate limit, a 5xx, an unparseable response -- keeps the entry. That last case is a
//! judgment call: a response we could not read may mean the write *did* land, so a retry
//! risks a duplicate. A duplicate task is an annoyance and a lost one is a bug report, so
//! the entry stays.

use tokio::sync::mpsc::UnboundedSender;
use tui_do_api::models::{Label, ProjectId, Task, TaskId};
use tui_do_api::{ApiError, Client, TaskQuery};

use crate::error::Result;
use crate::store::{
    Mutation, OutboxEntry, Store, Subject, CURRENT_USER, LAST_PULL, LAST_RECONCILE, PAGE_CAP,
};

/// How far back an incremental pull reaches beyond the watermark.
///
/// The watermark is stamped from *this* machine's clock and compared against the
/// server's `updated` column, so the two disagreeing by a few seconds is enough to drop
/// a task that changed in the gap — silently, and permanently, because the next pull
/// asks from an even later point. Reaching back re-fetches a handful of tasks that were
/// already stored, which costs one page and changes nothing: an upsert of a row that is
/// already right is a no-op.
const CLOCK_SKEW_MARGIN_SECONDS: i64 = 120;

/// Which direction a pass is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Sending queued local changes.
    Push,
    /// Reading the server's state into the store.
    Pull,
}

/// How much of the server a pull asks for.
///
/// The two differ in exactly one way that matters, and it is not speed: only a
/// [`Reach::Full`] pull can tell that something was *deleted*. A filtered listing names
/// what changed, and a task removed on another client changes nothing it could name — so
/// an incremental pull must not run the retain step, and the rows it leaves behind are
/// the price of not fetching seventy-eight pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// Every task the user can see, and delete every local row the listing did not
    /// mention. What startup and the timer ask for.
    #[default]
    Full,
    /// Only tasks the server says changed since the watermark, and delete nothing.
    ///
    /// Falls back to [`Self::Full`] when there is no watermark to ask from, which is
    /// every first run.
    Incremental,
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
    /// The oldest `updated` among the tasks this pull skipped, if any.
    ///
    /// The watermark is held back to this, so the next incremental pull asks for them
    /// again once the queued change has settled.
    pub oldest_skipped: Option<chrono::DateTime<chrono::Utc>>,
    /// Rows removed because the server no longer has them.
    pub removed: usize,
    /// Which kind of pull this was, so the interface can say so.
    ///
    /// Worth reporting rather than inferring: "`r` did not remove the task I deleted in
    /// the browser" is a reasonable thing to be confused by, and the answer is which
    /// pass ran.
    pub reach: Reach,
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

    /// A queued edit was written over a change someone else had already made.
    ///
    /// Only for a *true* collision: the user changed a field, and so had another client
    /// since this box last pulled. The user's value wins -- refusing it would lose what
    /// they just typed, and they are the one sitting there -- but overwriting someone
    /// silently is how a fleet loses work nobody can account for.
    ///
    /// A [`Subject`] rather than a `TaskId` because a label is overwritable on exactly
    /// the same terms: `POST /labels/{id}` replaces the label from the body, there is no
    /// conditional write to ask for, and two boxes renaming one label collide the way two
    /// boxes renaming one task do.
    Overwrote {
        /// What was written.
        subject: Subject,
        /// The names of the fields whose concurrent change was overwritten.
        fields: Vec<String>,
    },

    /// The server refused a queued change, which has been rolled back.
    ///
    /// The only event the user must be shown: their edit has just disappeared from the
    /// screen and they are owed an explanation.
    Rejected {
        /// What the change was about.
        subject: Subject,
        /// What the change was, as [`Mutation::kind`] spells it.
        kind: String,
        /// What the server said.
        message: String,
    },

    /// The server accepted a create and named the thing it created.
    ///
    /// A created task or label carries a provisional, negative id until the server
    /// answers, and the store swaps it for the real one. Anything still holding the
    /// provisional id is then holding an id no server has ever seen: an edit made against
    /// it is sent as `POST /tasks/-14` and comes back `404 This task does not exist`. The
    /// interface is one such holder — its list, its selection and its undo stack — so the
    /// swap has to be told, not inferred from a later pull that a push-only pass never
    /// runs.
    ///
    /// A label has *more* holders than a task, which is why the two ids are a
    /// [`Subject`] rather than a `TaskId`: a label id sits on every task carrying that
    /// label, in the filter the user is looking at, in the undo stack — and, most easily
    /// missed, in an open `Modal::Labels`, which is drawn from a list it read before the
    /// server had named anything.
    ///
    /// Both fields are the same kind. A `Task` provisional is never answered by a `Label`
    /// assignment; a receiver may match the pair and ignore a mismatch.
    Adopted {
        /// What the thing was called locally.
        provisional: Subject,
        /// What the server called it.
        assigned: Subject,
    },

    /// The push half finished, whether or not a pull follows.
    ///
    /// Separate from [`Self::Finished`] because a push can be asked for on its own — an
    /// edit wants its one request sent, not seventy-eight pages fetched — and without
    /// this the interface would have no way to learn that the sending it was told about
    /// had ended.
    Pushed(PushReport),

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
    /// A label was created, and this is the server's copy of it.
    ///
    /// Which may be the copy an *earlier* attempt of the same entry created: a retry
    /// reads before it writes, and what it finds is settled exactly as a fresh create is.
    LabelCreated(Box<Label>),
    /// A label was renamed or recoloured, and this is the server's copy of it.
    LabelUpdated(Box<Label>),
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

    /// Push, then pull everything.
    ///
    /// A push failure does not skip the pull: the server still has news, and the queue
    /// keeps what could not be sent.
    ///
    /// # Errors
    /// Whatever the pull returns. A push failure is reported as an event and in the
    /// report rather than as an error, because the queue surviving *is* the handling.
    pub async fn once(&self) -> Result<SyncReport> {
        self.pass(Reach::Full).await
    }

    /// Push, then pull only what changed.
    ///
    /// What `r` asks for. Measured against dev: an unfiltered pull is 78 pages and 15
    /// seconds, and the same instance filtered to three days of changes is one page.
    ///
    /// # Errors
    /// As [`Self::once`].
    pub async fn delta(&self) -> Result<SyncReport> {
        self.pass(Reach::Incremental).await
    }

    async fn pass(&self, reach: Reach) -> Result<SyncReport> {
        let push = self.push().await?;
        let pull = self.pull_with(reach).await?;
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
        // Ordering is a contract *per task*: two edits to one task must arrive in the
        // order they were made, or the server ends in a state the queue never described.
        // Two edits to *different* tasks have no such relationship. Stopping the whole
        // queue at the first failure honoured the contract by giving up far more than it
        // required -- one unreachable task, or one entry the server keeps 500ing on,
        // held back every other change the user had made. On a fleet, where a box may
        // carry a long backlog, that is the difference between one stuck task and a box
        // that has stopped syncing.
        let mut blocked: std::collections::HashSet<Subject> = std::collections::HashSet::new();
        // Guards against an entry that survives its own successful send, which would
        // otherwise spin this loop against the server forever.
        let mut attempted: std::collections::HashSet<i64> = std::collections::HashSet::new();
        let now = chrono::Utc::now();

        loop {
            let pending = self.store.pending(None).await?;
            let Some(entry) = pending
                .iter()
                .find(|queued| {
                    !attempted.contains(&queued.id)
                        && !blocked.contains(&queued.mutation.subject())
                        && queued.is_due(now)
                })
                .cloned()
            else {
                break;
            };
            attempted.insert(entry.id);

            match self.deliver(&entry).await? {
                Ok(()) => report.sent += 1,
                Err(error) if is_already_done(&entry.mutation, &error) => {
                    // The server has nothing to do because it is already in the state
                    // this entry asked for. Rolling back would undo the user's intent to
                    // report success at it.
                    tracing::debug!(kind = entry.mutation.kind(), "already in that state");
                    self.store.complete(entry.id).await?;
                    report.sent += 1;
                }
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
                    // Block this task and move on to the next one. Anything else queued
                    // for the same task stays behind it, which is the part of the
                    // ordering that actually matters.
                    let subject = entry.mutation.subject();
                    let message = error.to_string();
                    self.store
                        .defer(entry.id, message.clone(), error.retry_after())
                        .await?;
                    blocked.insert(subject);
                    tracing::debug!(%subject, error = %message, "deferring this task's queue");
                    self.emit(SyncEvent::Failed {
                        phase: Phase::Push,
                        message,
                    });
                }
            }
        }

        // Whatever is still in the queue when the drain runs out of eligible entries:
        // blocked tasks, entries still inside their backoff, and anything behind them.
        report.deferred = usize::try_from(self.store.pending_count().await?).unwrap_or(0);
        self.emit(SyncEvent::Pushed(report));
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
                let (provisional, named) = (task.id, assigned.id);
                self.store
                    .settle_create(entry.id, provisional, *assigned)
                    .await?;
                // After the store has swapped the id, never before: an interface that
                // renumbered first and then met a store failure would be pointing at a
                // row that does not exist.
                self.emit(SyncEvent::Adopted {
                    provisional: Subject::Task(provisional),
                    assigned: Subject::Task(named),
                });
            }
            (Sent::LabelCreated(assigned), Mutation::CreateLabel { label }) => {
                let (provisional, named) = (label.id, assigned.id);
                self.store
                    .settle_create_label(entry.id, provisional, *assigned)
                    .await?;
                // As above: the store first, then the news. A label has more holders than
                // a task -- every task carrying it, the filter, an open label modal --
                // and all of them are downstream of the row actually existing.
                self.emit(SyncEvent::Adopted {
                    provisional: Subject::Label(provisional),
                    assigned: Subject::Label(named),
                });
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
            (Sent::LabelUpdated(updated), Mutation::UpdateLabel { after, .. }) => {
                self.store.complete(entry.id).await?;
                // Worth keeping for the same reason the task answer is, and with the same
                // guard. The answer carries the *merged* label -- including whatever
                // fields another box changed that this edit did not touch -- so storing
                // it is how the user's picker learns about the colour someone else set,
                // instead of waiting for the next pull. And only once nothing else is
                // queued for this label, or the first of two renames would put its title
                // back over the second.
                if !self.store.is_pending_label(after.id).await? {
                    self.store.upsert_labels(vec![*updated]).await?;
                }
            }
            _ => self.store.complete(entry.id).await?,
        }
        Ok(Ok(()))
    }

    /// Make the request, and nothing else.
    async fn transmit(&self, entry: &OutboxEntry) -> std::result::Result<Sent, ApiError> {
        Ok(match &entry.mutation {
            Mutation::CreateTask { task } => Sent::Created(Box::new(
                self.client.create_task(task.project_id, task).await?,
            )),
            Mutation::UpdateTask { before, after } => {
                // Read the server's current copy and replay the user's edit onto it,
                // rather than sending a task that may have been read minutes ago.
                //
                // Vikunja replaces the task from the body and has no conditional write --
                // no version, no ETag -- so a stale body reverts whatever another box
                // changed in the meantime. With a fleet of machines against one server
                // that is the ordinary case, not a race. This does not make concurrent
                // editing safe; it narrows the window from "since this box last pulled"
                // to one request.
                //
                // A 404 here means the task was deleted elsewhere. That is a 4xx, so
                // `is_permanent` treats it as final and the optimistic write rolls back
                // with a toast -- which is the right answer: there is nothing to update.
                let current = self.client.task(after.id).await?;
                let merged = after.merge_onto(before, current);
                if !merged.collisions.is_empty() {
                    self.emit(SyncEvent::Overwrote {
                        subject: Subject::Task(after.id),
                        fields: merged.collisions.iter().map(|f| (*f).to_string()).collect(),
                    });
                }
                let updated = self.client.update_task(&merged.task).await?;
                Sent::Updated(Box::new(with_labels(updated, &merged.task)))
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
            Mutation::CreateLabel { label } => {
                // A create that has already failed once reads before it writes. Titles
                // are not unique and `PUT /labels` ignores the body's id, so a replayed
                // create answers `201` and a *second* label, with nothing in the response
                // to tell it from the first -- `is_already_done` has nothing to match on
                // and deliberately has no arm for this. Measured on dev 2026-08-29:
                // creating `tui-do probe alpha` twice answered `201` twice with two
                // different ids, and `0` and `-7` in the body both came back
                // server-assigned.
                //
                // Gated on the entry having already failed rather than done always,
                // because a *first* attempt has no earlier attempt of its own to find:
                // everything it could adopt belongs to somebody else.
                //
                // The gate narrows that, it does not close it. A retry whose first
                // attempt never reached the server -- a connect timeout, a DNS failure --
                // is indistinguishable here from one whose response was lost, and it will
                // adopt another box's same-titled label and quietly discard the
                // `hex_color` the user queued with theirs. The trade is deliberate: a
                // duplicate the user can see is worse than a colour they can re-pick, and
                // nothing can tell the two retries apart without a unique constraint the
                // server does not have. So this protects against *our own* replay, and
                // only mostly.
                if entry.is_failing() {
                    let existing = self.client.labels_named(&label.title).await?;
                    if existing.len() > 1 {
                        // Titles are not unique, so the server may hold several exact
                        // matches -- one per lost response, plus anything another box
                        // made. Its listing order is undefined, so which one is adopted
                        // is not deterministic and the rest are orphaned where nobody is
                        // looking. Say so somewhere a log can find it.
                        tracing::warn!(
                            title = %label.title,
                            found = existing.len(),
                            "several labels share this title; adopting the first listed and leaving the rest"
                        );
                    }
                    if let Some(existing) = existing.into_iter().next() {
                        return Ok(Sent::LabelCreated(Box::new(existing)));
                    }
                }
                Sent::LabelCreated(Box::new(self.client.create_label(label).await?))
            }
            Mutation::UpdateLabel { before, after } => {
                // Read, merge, write -- the same as `UpdateTask` and for the same
                // reasons. A partial body clears what it omits (measured on dev
                // 2026-08-29: a `POST /labels/12` carrying only `title` cleared
                // `hex_color` to ""), so a write must carry the whole label; and Vikunja
                // offers no conditional write, so a whole label assembled from what this
                // box last saw reverts whatever another box changed since. With a fleet
                // against one server that is the ordinary case, not a race.
                //
                // A 404 here means the label was deleted elsewhere, and
                // `is_already_done` treats that as the outcome the entry wanted rather
                // than as a rejection to roll back.
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
        })
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
        self.pull_with(Reach::Full).await
    }

    /// Refresh the store from the server, reaching as far as `reach` says.
    ///
    /// # Errors
    /// As [`Self::pull`].
    pub async fn pull_with(&self, reach: Reach) -> Result<PullReport> {
        self.emit(SyncEvent::Started(Phase::Pull));
        // Stamped *before* the requests, not after. A task edited while the pages were
        // being fetched may or may not have landed in one of them, and a watermark taken
        // at the end would put it in the past of a pull that never saw it -- so the next
        // incremental pull would skip it and it would be wrong until a full one ran.
        let started = chrono::Utc::now();
        let mut report = PullReport::default();

        match self.pull_inner(reach, &mut report).await {
            Ok(()) => {
                // A task the pull *saw* but did not store -- because a local change was
                // still queued for it -- has to stay inside the next pull's window, or
                // the server's version of it is lost until someone edits it again. Its
                // `updated` is already behind `started`, so advancing to `started` would
                // put it in the past of a pull that deliberately ignored it.
                //
                // Clamped rather than simply not advanced: an outbox entry that never
                // settles would otherwise freeze the watermark for good, and there is no
                // dead-letter to rescue it.
                let watermark = report
                    .oldest_skipped
                    .map_or(started, |oldest| oldest.min(started));
                self.store
                    .set_state(LAST_PULL, watermark.to_rfc3339())
                    .await?;
                // `LAST_RECONCILE` is not clamped: a skipped task was still *named* by
                // the listing, so the retain step saw it and the claim "nothing else is
                // gone" holds as of `started`.
                if report.reach == Reach::Full {
                    self.store
                        .set_state(LAST_RECONCILE, started.to_rfc3339())
                        .await?;
                }
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

    /// Pull the project and label lists, and nothing else.
    ///
    /// The two collections a name has to resolve against, and the only two that are cheap
    /// — one request each, where the task listing is seventy-eight pages against a real
    /// instance. `tui-do add` uses this when a `+project` or `*label` matches nothing it
    /// has cached, which is the difference between working on a machine that has never
    /// synced and refusing to.
    ///
    /// # Errors
    /// Whatever the server or the store returns.
    pub async fn pull_lists(&self) -> Result<PullReport> {
        let mut report = PullReport::default();

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

        Ok(report)
    }

    async fn pull_inner(&self, reach: Reach, report: &mut PullReport) -> Result<()> {
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

        let lists = self.pull_lists().await?;
        report.projects = lists.projects;
        report.labels = lists.labels;
        report.removed += lists.removed;

        // An incremental pull with no watermark is a full one: there is nothing to ask
        // "since" and "since the epoch" is every task anyway. Decided here rather than
        // at the call site so that the report says what actually happened.
        let since = match reach {
            Reach::Full => None,
            Reach::Incremental => self.store.last_pull().await?,
        };
        report.reach = if since.is_some() {
            Reach::Incremental
        } else {
            Reach::Full
        };

        // Unfiltered: every task the user can see, done ones included -- confirmed on
        // dev, where the listing returned the same 1,942 done tasks that `done = true`
        // does. A view would not do: a project's default List view filters
        // `done = false`, so pulling through one would make every completed task look
        // deleted to the retain step below.
        //
        // The filtered form asks the same question of a narrower window. It is the same
        // collection endpoint, so the same reasoning about done tasks holds.
        let query = match since {
            None => TaskQuery::new(),
            Some(watermark) => {
                let from = watermark - chrono::TimeDelta::seconds(CLOCK_SKEW_MARGIN_SECONDS);
                let from = from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                tracing::debug!(%from, "pulling only what changed");
                TaskQuery::new().filter(format!("updated > '{from}'"))
            }
        };
        let mut pager = self.client.tasks(&query)?;
        let mut seen: Vec<TaskId> = Vec::new();
        while let Some(page) = pager.next_page().await? {
            seen.extend(page.items.iter().map(|task| task.id));
            let applied = self.store.upsert_tasks_from_server(page.items).await?;
            report.tasks += applied.stored;
            report.skipped += applied.skipped;
            if let Some(oldest) = applied.oldest_skipped {
                report.oldest_skipped = Some(
                    report
                        .oldest_skipped
                        .map_or(oldest, |seen| seen.min(oldest)),
                );
            }
            self.emit(SyncEvent::Progress {
                stage: Stage::Tasks,
                stored: report.tasks,
                pages: pager.total_pages(),
            });
        }
        // Only a full listing may delete. `seen` from a filtered one names what changed,
        // and every unchanged task in the store is absent from it -- retaining against
        // that list would erase all but the last few days of the user's tasks. This is
        // the whole cost of an incremental pull and the reason `R` exists.
        if report.reach == Reach::Full {
            report.removed += self.store.retain_tasks(Vec::new(), seen).await?;
        }

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

/// Whether a refusal means the server had already done what was asked.
///
/// Deleting a task another device deleted first answers `404`, and both halves of a label
/// change have an equivalent. Each is the outcome the entry wanted. Treating them as
/// rejections would roll the change back — resurrecting a task the user deliberately
/// deleted, or putting back a label they took off while telling them it failed.
///
/// The shapes are **measured against the dev instance, not inferred**, because the spec
/// describes none of them:
///
/// | asked | answered | when |
/// |---|---|---|
/// | delete a task that is already gone | `404` | 2026-08-24 |
/// | attach a label that is already attached | `400`, Vikunja code `8001`, "This label already exists on the task." | 2026-08-24 |
/// | detach a label that is already detached | **`403 Forbidden`**, with no code and no message beyond "Forbidden" | 2026-08-24 |
/// | rename a label that is already gone | `404`, Vikunja code `8002`, "This label does not exist." | 2026-08-29 |
/// | delete a label that is already gone | `404`, code `8002`, the same | 2026-08-29 |
///
/// The rename is the one a fleet actually produces: box A deletes a label, box B's queued
/// rename replays against it. Without the arm the `404` is a 4xx like any other, so the
/// rename rolls back — putting the old title on a local row for a label the server no
/// longer has, and toasting an error about it. The delete row is measured and recorded
/// here for the `DeleteLabel` that does not exist yet; there is deliberately no arm for a
/// mutation that cannot be queued.
///
/// The detach's `403` is the surprise, and it is why this function exists in this shape:
/// the obvious reading of "already detached" is `404`, and a client that assumes it undoes
/// a detach the user asked for every time a retry replays.
///
/// Treating *any* `403` on a detach as "already done" does swallow a genuine permission
/// failure, and that is the deliberate trade: the label is then still on the server's
/// copy, so the next pull puts it back — the user sees the label return, which is the
/// truth. The alternative rolls back a detach the server has already honoured, which
/// leaves the local row wrong and shows an error for something that worked.
///
/// Deliberately narrow otherwise. A `404` creating a task means the *project* is gone,
/// which is a real rejection, and a `404` updating one means the task is gone, which the
/// user should hear about.
///
/// `CreateLabel` has no arm here and will never get one. Measured on dev 2026-08-29:
/// creating the same title twice answers `201` twice with two different ids, so a replayed
/// create *succeeds* and there is no error to match on — the duplicate is invisible from
/// here. What guards it instead is a read: [`Sync::transmit`] asks
/// [`tui_do_api::Client::labels_named`] what exists before an entry that has already
/// failed writes again.
fn is_already_done(mutation: &Mutation, error: &ApiError) -> bool {
    /// Vikunja's code for "this label already exists on the task".
    const LABEL_ALREADY_ATTACHED: i64 = 8001;

    /// Vikunja's code for "this label does not exist".
    const LABEL_GONE: i64 = 8002;

    matches!(
        (mutation, error),
        (
            Mutation::DeleteTask { .. } | Mutation::DetachLabel { .. },
            ApiError::Rejected { status: 404, .. },
        ) | (Mutation::DetachLabel { .. }, ApiError::Forbidden { .. })
            | (
                Mutation::AttachLabel { .. },
                ApiError::Rejected {
                    status: 400,
                    code: Some(LABEL_ALREADY_ATTACHED),
                    ..
                },
            )
            // Keyed on the code, not on the status alone -- unlike the task delete above,
            // where a 404 can only mean the task. A rename reads before it writes, so a
            // 404 here could come from either request, and a 404 that means something
            // else is a real rejection the user should hear about.
            | (
                Mutation::UpdateLabel { .. },
                ApiError::Rejected {
                    status: 404,
                    code: Some(LABEL_GONE),
                    ..
                },
            )
    )
}

/// Carry the labels an edit intended onto the server's answer.
///
/// Written when the server was believed not to echo labels in a task write's response.
/// Measured since, against dev: **it does** — a write that carried one label answered
/// with one label, matching the task. So this is belt and braces rather than the load
/// -bearing step it was documented as.
///
/// It stays because the measurement is one shape of one write, and the failure it guards
/// against is silent: an answer stored verbatim that dropped labels would leave the local
/// row wrong until the next pull, and the labels it restores are by construction the ones
/// the server just confirmed. `decompose` leaves an `UpdateTask` carrying the labels it is
/// *not* changing; anything it is changing has its own entry, and the caller only stores
/// this answer once none are left queued.
fn with_labels(mut task: Task, intended: &Task) -> Task {
    task.labels = intended.labels.clone();
    task
}
