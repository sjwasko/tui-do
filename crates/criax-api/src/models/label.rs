//! Labels.

use serde::{Deserialize, Serialize};

use super::datetime::Timestamp;
use super::ids::LabelId;
use super::user::User;

/// A label, as attached to tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Label {
    /// Server-assigned identifier.
    #[serde(default)]
    pub id: LabelId,

    /// The label text.
    #[serde(default)]
    pub title: String,

    /// Optional longer description. Rarely set.
    #[serde(default)]
    pub description: String,

    /// Background colour as six hex digits, without a leading `#`.
    ///
    /// Empty when the user never picked one, in which case the UI supplies a default.
    #[serde(default)]
    pub hex_color: String,

    /// Who created the label.
    #[serde(default)]
    pub created_by: Option<User>,

    /// When it was created.
    #[serde(default)]
    pub created: Timestamp,

    /// When it was last modified.
    #[serde(default)]
    pub updated: Timestamp,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_a_label_from_a_task_payload() {
        let json = r#"{
            "id": 12,
            "title": "urgent",
            "description": "",
            "hex_color": "e8384f",
            "created_by": {"id": 1, "username": "swasko"},
            "created": "2026-01-02T03:04:05Z",
            "updated": "0001-01-01T00:00:00Z"
        }"#;
        let label: Label = serde_json::from_str(json).unwrap();
        assert_eq!(label.id, LabelId(12));
        assert_eq!(label.title, "urgent");
        assert_eq!(label.hex_color, "e8384f");
        assert!(label.created.get().is_some());
        // The zero timestamp must not become a year-1 date.
        assert_eq!(label.updated.get(), None);
    }
}
