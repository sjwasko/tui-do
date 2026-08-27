//! The user type, as returned inline on tasks and projects.

use serde::{Deserialize, Serialize};

use super::datetime::Timestamp;
use super::ids::UserId;

/// A Vikunja user.
///
/// Appears both standalone (`GET /user`) and embedded in tasks as `created_by` and
/// `assignees`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct User {
    /// Server-assigned identifier.
    #[serde(default)]
    pub id: UserId,

    /// Login name. Always present.
    #[serde(default)]
    pub username: String,

    /// Display name. Frequently empty, in which case fall back to `username`.
    #[serde(default)]
    pub name: String,

    /// Only populated for the authenticated user; empty for other users on a task.
    #[serde(default)]
    pub email: String,

    /// When the account was created.
    #[serde(default)]
    pub created: Timestamp,

    /// When the account was last modified.
    #[serde(default)]
    pub updated: Timestamp,
}

impl User {
    /// The best available human-readable name.
    ///
    /// Vikunja leaves `name` empty for accounts that never set a display name, and the
    /// web UI falls back to the username in that case.
    #[must_use]
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.username
        } else {
            &self.name
        }
    }
}

/// The body of `PUT /tasks/{taskID}/assignees` — assigning a user to a task.
///
/// The spec calls this definition `models.TaskAssginee`; the typo is upstream's, and the
/// conformance test spells it that way deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskAssignee {
    /// The user to assign.
    pub user_id: UserId,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn display_name_falls_back_to_username() {
        let named = User {
            username: "swasko".into(),
            name: "Steve".into(),
            ..User::default()
        };
        assert_eq!(named.display_name(), "Steve");

        let unnamed = User {
            username: "swasko".into(),
            name: String::new(),
            ..User::default()
        };
        assert_eq!(unnamed.display_name(), "swasko");

        let whitespace = User {
            username: "swasko".into(),
            name: "   ".into(),
            ..User::default()
        };
        assert_eq!(whitespace.display_name(), "swasko");
    }

    #[test]
    fn deserializes_a_sparse_embedded_user() {
        // Assignees on a task arrive without email or timestamps.
        let user: User = serde_json::from_str(r#"{"id":3,"username":"swasko"}"#).unwrap();
        assert_eq!(user.id, UserId(3));
        assert_eq!(user.display_name(), "swasko");
        assert_eq!(user.created.get(), None);
    }
}
