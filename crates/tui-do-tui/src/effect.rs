//! Side effects, described rather than performed.
//!
//! `update` returns these; the effect runtime in the binary crate executes them and sends
//! the results back as [`crate::Msg`]. They carry fully-built `tui-do-core` values so the
//! runtime decides nothing — it looks up no state and makes no choice about what to load.

use tui_do_core::models::ProjectId;
use tui_do_core::store::{Mutation, TaskFilter, TaskSort};

use crate::query::QueryId;

/// A side effect requested by `update`.
///
/// `Eq` is deliberately absent: a `Mutation` carries a `Task`, and a task carries
/// `percent_done`, which is a float. Tests compare these with `assert_eq!` on
/// `PartialEq`, which is all they need.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Effect {
    /// Read tasks, and answer with [`crate::Msg::TasksLoaded`] carrying `id`.
    LoadTasks {
        /// Correlates the answer with the request that is still current.
        id: QueryId,
        /// What to read.
        filter: TaskFilter,
        /// In what order.
        sort: TaskSort,
    },

    /// Read the project list.
    LoadProjects,

    /// Read the label list.
    LoadLabels,

    /// Read per-project task counts for the sidebar.
    LoadCounts,

    /// Read how many changes are still queued, and answer with
    /// [`crate::Msg::PendingLoaded`].
    ///
    /// The outbox is shared: `tui-do add` in another terminal writes to the same one, and
    /// so does a second interface. A count carried forward from the last sync report is
    /// only ever right by luck.
    LoadPending,

    /// Remember the project to open on next launch. `None` means "everything".
    RememberProject(Option<ProjectId>),

    /// Apply a change locally and queue it for the server.
    ///
    /// One effect for every kind of write, because `Store::queue` already does both
    /// halves in a single transaction. The model has *already* applied it to its own
    /// snapshot by the time this runs — a task list that waits for SQLite before showing
    /// a tick is the lag this project exists to remove.
    Apply(Mutation),

    /// Run a sync pass now rather than waiting for the timer.
    SyncNow,

    /// Leave the application.
    Quit,
}
