//! At most one of each read in flight, and at most one waiting behind it.
//!
//! BUG-23. Every write the interface makes is followed by `Msg::Reload`, which asks for
//! the task list, the projects, the labels, the counts and the queue's health. The task
//! list alone is 190-530 ms against 3,877 tasks, and a held key presses 24 times a second,
//! so spawning one read per request asked for more time on the store's one connection
//! than a second holds. The push shares that connection: with the reloads running it got
//! 1.2 reads a second where it got 33.6 without them, and after the key came up hundreds
//! of reloads were still queued on the lock ahead of it.
//!
//! The interface only ever wants the newest answer -- `TasksLoaded` drops any id that is
//! not current -- so a request that arrives while one is in flight replaces whatever was
//! waiting, and the waiting one runs when the flight lands. Any request is therefore
//! answered by a read that *started after it was made*, which is the property a reload
//! after a write depends on: the write has committed before `Msg::Reload` is sent, so
//! that read sees it.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use tui_do_core::store::{TaskFilter, TaskSort};
use tui_do_ui::QueryId;

use super::{spawn_effect, LoopTx};

/// One kind of read: whether one is running, and the newest request waiting behind it.
#[derive(Debug)]
pub(super) struct Coalesce<R> {
    slot: Mutex<Slot<R>>,
}

#[derive(Debug)]
struct Slot<R> {
    running: bool,
    waiting: Option<R>,
}

impl<R> Default for Coalesce<R> {
    fn default() -> Self {
        Self {
            slot: Mutex::new(Slot {
                running: false,
                waiting: None,
            }),
        }
    }
}

impl<R> Coalesce<R> {
    /// `Some` when the caller should run `request` now; `None` when one is already
    /// running and `request` has taken the place of whatever was waiting.
    fn admit(&self, request: R) -> Option<R> {
        let mut slot = self.lock();
        if slot.running {
            slot.waiting = Some(request);
            None
        } else {
            slot.running = true;
            Some(request)
        }
    }

    /// The run has landed: the request to run next, or `None` and the kind goes idle.
    ///
    /// Taken under the same lock as [`Self::admit`], so a request cannot arrive between
    /// "nothing is waiting" and "nothing is running" and be left with neither.
    fn finish(&self) -> Option<R> {
        let mut slot = self.lock();
        let next = slot.waiting.take();
        if next.is_none() {
            slot.running = false;
        }
        next
    }

    /// Back to idle, forgetting whatever was waiting. For a run that panicked.
    fn reset(&self) {
        let mut slot = self.lock();
        slot.running = false;
        slot.waiting = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slot<R>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Puts a kind back to idle if its runner ends any way but by running out of requests.
///
/// A panicking read is caught by [`spawn_effect`] and its future dropped afterwards, not
/// mid-unwind, so `std::thread::panicking` cannot see it; an aborted one never unwinds at
/// all. Either would leave the kind marked running, refusing every later request in this
/// runtime without a word, so the guard asks instead whether the loop finished.
struct Running<'a, R> {
    kind: &'a Coalesce<R>,
    finished: bool,
}

impl<R> Drop for Running<'_, R> {
    fn drop(&mut self) {
        if !self.finished {
            self.kind.reset();
        }
    }
}

/// Run `work(request)`, or leave `request` waiting behind the run already going.
///
/// Answers the runner's handle when this call started one, for a test to wait on.
pub(super) fn coalesced<R, F, Fut>(
    kind: &Arc<Coalesce<R>>,
    tx: &LoopTx,
    request: R,
    work: F,
) -> Option<tokio::task::JoinHandle<()>>
where
    R: Send + 'static,
    F: Fn(R) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let first = kind.admit(request)?;
    let kind = Arc::clone(kind);
    Some(spawn_effect(tx.clone(), async move {
        let mut running = Running {
            kind: &kind,
            finished: false,
        };
        let mut next = Some(first);
        while let Some(request) = next {
            work(request).await;
            next = kind.finish();
        }
        running.finished = true;
    }))
}

/// What a task-list request carries. Whatever `Effect::LoadTasks` grows goes in here,
/// so the request that finally runs is the newest one, all of it.
pub(super) type TaskRequest = (QueryId, TaskFilter, TaskSort);

/// The reads `Msg::Reload` asks for, one gate each.
///
/// Belongs to one runtime: made with its [`LoopTx`], so a queued request always runs
/// against the store and the channel of the loop that asked for it.
#[derive(Debug, Default)]
pub(super) struct Reads {
    pub(super) tasks: Arc<Coalesce<TaskRequest>>,
    pub(super) projects: Arc<Coalesce<()>>,
    pub(super) labels: Arc<Coalesce<()>>,
    pub(super) users: Arc<Coalesce<()>>,
    pub(super) counts: Arc<Coalesce<()>>,
    pub(super) pending: Arc<Coalesce<()>>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use std::sync::Arc;

    use tokio::sync::{mpsc, Notify};

    use super::*;

    #[tokio::test]
    async fn requests_made_during_a_read_become_one_read_with_the_newest() {
        let kind: Arc<Coalesce<u32>> = Arc::default();
        let (tx, _rx) = LoopTx::channel();
        let (ran_tx, mut ran) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());

        let work = {
            let release = Arc::clone(&release);
            move |request: u32| {
                let (ran_tx, release) = (ran_tx.clone(), Arc::clone(&release));
                async move {
                    let _ = ran_tx.send(request);
                    // Held in flight until the test says so, standing in for a store
                    // read that takes half a second.
                    if request == 0 {
                        release.notified().await;
                    }
                }
            }
        };

        coalesced(&kind, &tx, 0, work.clone());
        // And under its own id. BUG-22 trusts a load's answer for a write only when the
        // load's id is newer than the one in force when the write landed, so relabelling
        // a read that started earlier with a newer id would make it lie about what it saw.
        assert_eq!(ran.recv().await, Some(0), "the first request runs at once");
        for request in 1..=50 {
            coalesced(&kind, &tx, request, work.clone());
        }
        release.notify_one();

        assert_eq!(
            ran.recv().await,
            Some(50),
            "the read after the flight is the newest request, not the next one"
        );
        // Every sender but the gate's own clones is gone once the runner exits.
        drop(work);
        let mut later = Vec::new();
        while let Some(request) = ran.recv().await {
            later.push(request);
        }
        assert!(
            later.is_empty(),
            "fifty requests ran more than two reads: {later:?}"
        );
        assert!(
            kind.admit(51).is_some(),
            "the kind went idle once the queue emptied"
        );
    }

    #[test]
    fn a_request_arriving_as_a_run_ends_is_never_stranded() {
        // `finish` decides "idle" under the lock `admit` takes, so a request is either
        // waiting when `finish` looks or is admitted to run on its own.
        let kind = Coalesce::default();
        assert_eq!(kind.admit(1), Some(1));
        assert_eq!(kind.admit(2), None);
        assert_eq!(kind.finish(), Some(2));
        assert_eq!(kind.finish(), None);
        assert_eq!(kind.admit(3), Some(3));
    }

    #[tokio::test]
    async fn a_read_that_panics_does_not_leave_its_kind_running() {
        let kind: Arc<Coalesce<u32>> = Arc::default();
        let (tx, mut rx) = LoopTx::channel();
        let runner = coalesced(&kind, &tx, 1, |_| async { panic!("the read fell over") });
        runner.unwrap().await.unwrap();
        assert!(
            rx.fatal.recv().await.is_some(),
            "the panic was not reported"
        );
        assert_eq!(kind.admit(2), Some(2), "the kind stayed marked running");
    }
}
