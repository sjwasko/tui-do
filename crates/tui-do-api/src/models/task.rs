//! Tasks and the types that hang off them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::datetime::Timestamp;
use super::ids::{AttachmentId, CommentId, ProjectId, TaskId};
use super::label::Label;
use super::user::User;

/// A task.
///
/// Field presence varies by endpoint: a task from a list endpoint carries labels and
/// assignees but no comments, while `GET /tasks/{id}` fills in more. Every field is
/// therefore `#[serde(default)]` — an absent field means "not requested", not "empty".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Task {
    /// Server-assigned identifier.
    #[serde(default)]
    pub id: TaskId,

    /// The project this task belongs to.
    #[serde(default)]
    pub project_id: ProjectId,

    /// Task title.
    #[serde(default)]
    pub title: String,

    /// Longer description. Vikunja's web editor stores HTML here, so this is not
    /// necessarily plain text or Markdown — see the renderer in `tui-do-ui`.
    #[serde(default)]
    pub description: String,

    /// Whether the task is complete.
    #[serde(default)]
    pub done: bool,

    /// When it was completed. Unset while `done` is false.
    #[serde(default)]
    pub done_at: Timestamp,

    /// Priority from 1 (low) to 5 (DO NOW); `0` means unset.
    #[serde(default)]
    pub priority: i64,

    /// Per-project sequence number, the `42` in `WORK-42`.
    #[serde(default)]
    pub index: i64,

    /// Human-readable identifier such as `WORK-42`. Empty when the project has no prefix.
    #[serde(default)]
    pub identifier: String,

    /// Whether the current user has favourited it.
    #[serde(default)]
    pub is_favorite: bool,

    /// Completion percentage, 0.0 to 1.0.
    ///
    /// Note the scale: Vikunja stores a fraction, not a percentage, despite the name.
    #[serde(default)]
    pub percent_done: f64,

    /// Manual sort position. Always `0` unless the task was fetched through a view
    /// endpoint, because positions are stored per view.
    #[serde(default)]
    pub position: f64,

    /// When the task is due.
    #[serde(default)]
    pub due_date: Timestamp,

    /// When work should begin.
    #[serde(default)]
    pub start_date: Timestamp,

    /// When work should finish.
    #[serde(default)]
    pub end_date: Timestamp,

    /// Repeat interval in seconds; `0` when the task does not repeat.
    #[serde(default)]
    pub repeat_after: i64,

    /// How the repeat interval is applied.
    #[serde(default)]
    pub repeat_mode: RepeatMode,

    /// Colour override as six hex digits, without a leading `#`.
    #[serde(default)]
    pub hex_color: String,

    /// Labels attached to the task.
    #[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]
    pub labels: Vec<Label>,

    /// Users assigned to the task.
    ///
    /// List endpoints populate this, verified against dev — so an empty list means
    /// nobody is assigned, not that the endpoint declined to say. That is what makes it
    /// safe to pass a listed task straight back to `update_task`, which replaces the
    /// task from the body and would otherwise unassign everyone.
    #[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]
    pub assignees: Vec<User>,

    /// Who created it.
    #[serde(default)]
    pub created_by: Option<User>,

    /// Reminders set on the task.
    #[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]
    pub reminders: Vec<TaskReminder>,

    /// Attachments. Only populated by endpoints that return a full task.
    #[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]
    pub attachments: Vec<TaskAttachment>,

    /// Comments. Only populated by `GET /tasks/{id}`.
    #[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]
    pub comments: Vec<TaskComment>,

    /// Number of comments, available on list endpoints where `comments` is not.
    #[serde(default)]
    pub comment_count: i64,

    /// Related tasks, grouped by how they relate.
    #[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]
    pub related_tasks: HashMap<RelationKind, Vec<Task>>,

    /// The bucket this task sits in, when fetched through a Kanban view.
    #[serde(default)]
    pub bucket_id: i64,

    /// When it was created.
    #[serde(default)]
    pub created: Timestamp,

    /// When it was last modified.
    #[serde(default)]
    pub updated: Timestamp,
}

impl Task {
    /// The best available label for this task in a list.
    #[must_use]
    pub fn display_identifier(&self) -> String {
        if self.identifier.is_empty() {
            format!("#{}", self.id)
        } else {
            self.identifier.clone()
        }
    }

    /// Whether a priority was actually set.
    ///
    /// Vikunja uses `0` rather than null for "no priority", so a bare comparison would
    /// treat unset tasks as lower priority than "low".
    #[must_use]
    pub fn has_priority(&self) -> bool {
        self.priority > 0
    }

    /// Whether the task is overdue relative to `now`.
    ///
    /// Completed tasks are never overdue, and a task with no due date cannot be.
    #[must_use]
    pub fn is_overdue(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        !self.done && self.due_date.get().is_some_and(|due| due < now)
    }

    /// Whether this task repeats on completion.
    #[must_use]
    pub fn repeats(&self) -> bool {
        self.repeat_after > 0 || self.repeat_mode != RepeatMode::AfterAmount
    }
}

/// How a repeating task's next occurrence is calculated.
///
/// Serialised as an integer. Note that the spec's *prose* for `repeat_mode` claims the
/// third variant is `3`; the enum declaration and `x-enum-varnames` in the same document
/// both say `2`. The enum is authoritative — the description is wrong upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(from = "i64", into = "i64")]
pub enum RepeatMode {
    /// Repeat after the interval in `repeat_after`.
    #[default]
    AfterAmount,
    /// Repeat monthly, ignoring `repeat_after`.
    Monthly,
    /// Repeat measured from the completion date rather than the previous due date.
    FromCurrentDate,
    /// A mode this client does not know about; preserved so it round-trips unchanged.
    Unknown(i64),
}

impl From<i64> for RepeatMode {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::AfterAmount,
            1 => Self::Monthly,
            2 => Self::FromCurrentDate,
            other => Self::Unknown(other),
        }
    }
}

impl From<RepeatMode> for i64 {
    fn from(value: RepeatMode) -> Self {
        match value {
            RepeatMode::AfterAmount => 0,
            RepeatMode::Monthly => 1,
            RepeatMode::FromCurrentDate => 2,
            RepeatMode::Unknown(other) => other,
        }
    }
}

/// How two tasks relate.
///
/// Used as a map key in [`Task::related_tasks`], so it must be a string enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RelationKind {
    /// Relation kind the server did not classify.
    #[default]
    Unknown,
    /// The related task is a child of this one.
    Subtask,
    /// The related task is the parent of this one.
    Parenttask,
    /// Loosely related.
    Related,
    /// This task duplicates the related one.
    DuplicateOf,
    /// The related task duplicates this one.
    Duplicates,
    /// This task blocks the related one.
    Blocking,
    /// This task is blocked by the related one.
    Blocked,
    /// This task comes before the related one.
    Precedes,
    /// This task comes after the related one.
    Follows,
    /// Copied from the related task.
    CopiedFrom,
    /// Copied to the related task.
    CopiedTo,
}

/// A reminder attached to a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskReminder {
    /// When the reminder fires.
    #[serde(default)]
    pub reminder: Timestamp,

    /// Offset in seconds from the anchor date, for relative reminders.
    #[serde(default)]
    pub relative_period: i64,
}

/// A file attached to a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskAttachment {
    /// Server-assigned identifier.
    #[serde(default)]
    pub id: AttachmentId,

    /// The task this is attached to.
    #[serde(default)]
    pub task_id: TaskId,

    /// Who uploaded it.
    #[serde(default)]
    pub created_by: Option<User>,

    /// When it was uploaded.
    #[serde(default)]
    pub created: Timestamp,
}

/// A comment on a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskComment {
    /// Server-assigned identifier.
    #[serde(default)]
    pub id: CommentId,

    /// Comment body. HTML, like task descriptions.
    #[serde(default)]
    pub comment: String,

    /// Who wrote it.
    #[serde(default)]
    pub author: Option<User>,

    /// When it was posted.
    #[serde(default)]
    pub created: Timestamp,

    /// When it was last edited.
    #[serde(default)]
    pub updated: Timestamp,
}

/// A Kanban bucket within a project view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Bucket {
    /// Server-assigned identifier.
    #[serde(default)]
    pub id: i64,

    /// Bucket name.
    #[serde(default)]
    pub title: String,

    /// Ordering among the view's buckets.
    #[serde(default)]
    pub position: f64,

    /// Maximum tasks allowed; `0` means unlimited.
    #[serde(default)]
    pub limit: i64,

    /// Number of tasks in the bucket.
    #[serde(default)]
    pub count: i64,

    /// Who created it.
    ///
    /// The whole user, not an id: the spec gives `models.Bucket.created_by` as
    /// `allOf: [user.User]`, so the server sends an object here and an `i64` would fail
    /// to deserialize -- taking the entire buckets response down with it. Latent until
    /// Phase 6 wires `VIEW_BUCKETS`, and invisible to the conformance test, which
    /// compares field *names* against the spec and never their types.
    #[serde(default)]
    pub created_by: Option<User>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn at(y: i32, m: u32, d: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0)
            .single()
            .expect("valid test timestamp")
    }

    #[test]
    fn unset_priority_is_distinguishable_from_low() {
        let unset = Task::default();
        assert!(!unset.has_priority());
        let low = Task {
            priority: 1,
            ..Task::default()
        };
        assert!(low.has_priority());
    }

    #[test]
    fn overdue_requires_a_due_date_in_the_past_and_an_open_task() {
        let now = at(2026, 8, 24);

        let no_due = Task::default();
        assert!(!no_due.is_overdue(now));

        let past = Task {
            due_date: Some(at(2026, 8, 1)).into(),
            ..Task::default()
        };
        assert!(past.is_overdue(now));

        let future = Task {
            due_date: Some(at(2026, 9, 1)).into(),
            ..Task::default()
        };
        assert!(!future.is_overdue(now));

        let done = Task {
            due_date: Some(at(2026, 8, 1)).into(),
            done: true,
            ..Task::default()
        };
        assert!(!done.is_overdue(now));
    }

    #[test]
    fn a_task_with_no_dates_is_not_ancient() {
        // The regression this guards: Vikunja sends the zero time for unset dates, so
        // naive parsing yields a year-1 due date and every task reads as overdue.
        let json = r#"{
            "id": 1, "title": "no dates",
            "due_date": "0001-01-01T00:00:00Z",
            "start_date": "0001-01-01T00:00:00Z",
            "end_date": "0001-01-01T00:00:00Z"
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.due_date.get(), None);
        assert!(!task.is_overdue(at(2026, 8, 24)));
    }

    #[test]
    fn display_identifier_falls_back_to_the_id() {
        let with_prefix = Task {
            identifier: "WORK-42".into(),
            ..Task::default()
        };
        assert_eq!(with_prefix.display_identifier(), "WORK-42");

        let without = Task {
            id: TaskId(42),
            ..Task::default()
        };
        assert_eq!(without.display_identifier(), "#42");
    }

    #[test]
    fn repeat_mode_round_trips_and_tolerates_unknown_values() {
        for raw in [0, 1, 2] {
            let mode: RepeatMode = serde_json::from_str(&raw.to_string()).unwrap();
            assert_eq!(serde_json::to_string(&mode).unwrap(), raw.to_string());
        }
        // An unrecognised mode is preserved rather than silently rewritten to 0, so a
        // round-trip through tui-do does not corrupt a task it does not understand.
        let future: RepeatMode = serde_json::from_str("7").unwrap();
        assert_eq!(future, RepeatMode::Unknown(7));
        assert_eq!(serde_json::to_string(&future).unwrap(), "7");
    }

    #[test]
    fn related_tasks_deserialize_keyed_by_relation_kind() {
        let json = r#"{
            "id": 1, "title": "parent",
            "related_tasks": {"subtask": [{"id": 2, "title": "child"}]}
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        let subtasks = task
            .related_tasks
            .get(&RelationKind::Subtask)
            .expect("subtask relation present");
        assert_eq!(subtasks.len(), 1);
        assert_eq!(subtasks[0].title, "child");
    }

    #[test]
    fn absent_collections_default_to_empty() {
        // List endpoints omit comments and attachments entirely.
        let task: Task = serde_json::from_str(r#"{"id":1,"title":"t"}"#).unwrap();
        assert!(task.labels.is_empty());
        assert!(task.comments.is_empty());
        assert!(task.related_tasks.is_empty());
    }
}
