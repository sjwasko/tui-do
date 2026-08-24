//! Side effects, described rather than performed.
//!
//! `update` returns these; the effect runtime in the binary crate executes them and sends
//! the results back as [`crate::Msg`]. They carry fully-built `criax-core` values so the
//! runtime decides nothing — it looks up no state and makes no choice about what to load.

use criax_core::models::ProjectId;
use criax_core::store::{TaskFilter, TaskSort};

use crate::query::QueryId;

/// A side effect requested by `update`.
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Remember the project to open on next launch. `None` means "everything".
    RememberProject(Option<ProjectId>),

    /// Run a sync pass now rather than waiting for the timer.
    SyncNow,

    /// Leave the application.
    Quit,
}
