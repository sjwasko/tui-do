//! Everything that can happen, as data.
//!
//! A `Msg` is the only way the outside world reaches the model. The effect runtime turns
//! terminal events, store answers and [`tui_do_core::SyncEvent`]s into these; `update`
//! turns them into state changes and [`crate::Effect`]s. Nothing else crosses the line.

use chrono::{DateTime, Utc};
use crossterm::event::KeyEvent;
use tui_do_core::models::{Label, Project, Task};
use tui_do_core::store::ProjectCounts;
use tui_do_core::SyncEvent;

use crate::query::QueryId;

/// Something that happened.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Msg {
    /// A key was pressed.
    Key(KeyEvent),

    /// The terminal was resized, to these columns and rows.
    Resize(u16, u16),

    /// The clock advanced.
    ///
    /// Carries the time rather than reading it, because `update` is pure: "two minutes
    /// ago" and "overdue" are decisions about now, and now has to arrive from outside.
    Tick(DateTime<Utc>),

    /// The store answered a task query.
    TasksLoaded {
        /// Which query this answers. A stale id is dropped.
        id: QueryId,
        /// The rows, already ordered by the store.
        tasks: Vec<Task>,
    },

    /// The store answered with the project list.
    ProjectsLoaded(Vec<Project>),

    /// The store answered with the label list.
    LabelsLoaded(Vec<Label>),

    /// The store answered with per-project task counts.
    CountsLoaded(ProjectCounts),

    /// How many changes the outbox is still holding.
    PendingLoaded(usize),

    /// Something changed underneath the model; read it all again.
    ///
    /// Sent by the runtime after a write lands, so the list shows what was actually
    /// stored rather than what the model optimistically drew.
    Reload,

    /// A store read failed. The interface stays usable and says so.
    StoreFailed(String),

    /// The sync engine reported something.
    Sync(SyncEvent),
}
