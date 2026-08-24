//! Newtype identifiers.
//!
//! Vikunja's ids are all bare `i64` on the wire, and several of them appear side by side
//! in the same call — `PUT /tasks/{task}/labels/{label}`, `DELETE
//! /projects/{projectID}/views/{view}/buckets/{bucketID}`. Distinct types make
//! transposing two arguments a compile error rather than a puzzling 404.

use std::fmt;

/// Declare an id newtype over `i64` with the usual conversions.
macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            serde::Serialize, serde::Deserialize, Default,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl $name {
            /// The underlying value, for building a URL or a SQL parameter.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl From<i64> for $name {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for i64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

id_type!(
    /// Identifies a task.
    TaskId
);
id_type!(
    /// Identifies a project.
    ProjectId
);
id_type!(
    /// Identifies a label.
    LabelId
);
id_type!(
    /// Identifies a user.
    UserId
);
id_type!(
    /// Identifies a project view (List, Gantt, Table, Kanban).
    ViewId
);
id_type!(
    /// Identifies a comment on a task.
    ///
    /// Distinct from [`TaskId`] because `DELETE /tasks/{taskID}/comments/{commentID}`
    /// takes both, adjacent and both numeric.
    CommentId
);
id_type!(
    /// Identifies a file attached to a task.
    AttachmentId
);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_transparent_on_the_wire() {
        // Vikunja sends a bare integer; the newtype must not add a wrapper object.
        let id: TaskId = serde_json::from_str("42").unwrap();
        assert_eq!(id, TaskId(42));
        assert_eq!(serde_json::to_string(&id).unwrap(), "42");
    }

    #[test]
    fn ids_display_as_plain_numbers_for_url_building() {
        assert_eq!(format!("/tasks/{}", TaskId(7)), "/tasks/7");
    }
}
