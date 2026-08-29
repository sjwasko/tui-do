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

/// The result of replaying a label edit onto the server's current copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelMerge {
    /// The label to send.
    pub label: Label,

    /// Fields the user's value was written over, because both sides had changed them.
    /// The user wins — refusing would lose what they just typed — but they are owed the
    /// news that they wrote over somebody.
    pub collisions: Vec<&'static str>,
}

impl Label {
    /// Replay a local edit onto the server's current copy.
    ///
    /// `self` is what the user wants, `before` what they started from, and `server` what
    /// the server holds now. A field the user changed takes their value; everything else
    /// keeps the server's. On a true collision — both sides changed the same field — the
    /// user's value wins and the field is named, because they are the one sitting there.
    ///
    /// The same three-way merge as [`Task::merge_onto`](super::Task::merge_onto), and for
    /// the same reasons. Vikunja offers no conditional write for a label any more than for
    /// a task — no version, no ETag — and a partial body clears what it omits: measured on
    /// dev 2026-08-29, a `POST /labels/12` carrying only `title` cleared `hex_color` to
    /// `""`. So a rename must send the whole label, and a whole label read minutes ago
    /// reverts whatever another box changed since. tui-do is multi-instance, so that is
    /// the ordinary case rather than a race.
    ///
    /// `server` is destructured exhaustively on purpose: a new field on [`Label`] then
    /// fails to compile here rather than silently keeping the server's value, which is how
    /// a field quietly becomes uneditable.
    #[must_use]
    pub fn merge_onto(&self, before: &Self, server: Self) -> LabelMerge {
        let after = self;
        let mut collisions: Vec<&'static str> = Vec::new();

        let Self {
            id,
            title,
            description,
            hex_color,
            created_by,
            created,
            updated,
        } = server;

        macro_rules! merge {
            ($field:ident) => {{
                if before.$field == after.$field {
                    $field
                } else {
                    if $field != before.$field {
                        collisions.push(stringify!($field));
                    }
                    after.$field.clone()
                }
            }};
        }

        LabelMerge {
            label: Self {
                // The server's id, never the user's copy of it. The body's `id` beats the
                // path — `POST /labels/12` carrying `"id": 13` updated label 13, left 12
                // untouched and answered with 13, measured on dev 2026-08-29 — and
                // `Client::update_label` builds the path from the body it is given. Taking
                // the id from the row that was just read means the two cannot disagree,
                // and a queued edit that somehow carries a stale id writes nothing rather
                // than writing to somebody else's label.
                id,
                title: merge!(title),
                description: merge!(description),
                hex_color: merge!(hex_color),
                // Server-owned: nothing in the interface edits them, so merging them would
                // only ever be a way to send the server its own value back stale.
                created_by,
                created,
                updated,
            },
            collisions,
        }
    }
}

/// The body of `PUT /tasks/{task}/labels` — attaching an existing label to a task.
///
/// A whole struct for one field, because that is what the endpoint takes: labels are not
/// set by sending a task with a `labels` array, they are attached and detached one at a
/// time through their own endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LabelTask {
    /// The label to attach.
    pub label_id: LabelId,
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

    #[test]
    fn a_rename_keeps_a_colour_someone_else_changed() {
        // The same three-way merge as `Task::merge_onto`, and for the same reason: Vikunja
        // has no conditional write, a partial body clears what it omits, and tui-do is
        // multi-instance. Without this, box B's rename reverts box A's recolour.
        let before = Label {
            id: LabelId(41),
            title: "next".into(),
            hex_color: "aaaaaa".into(),
            ..Default::default()
        };
        let after = Label {
            title: "next up".into(),
            ..before.clone()
        };
        let server = Label {
            hex_color: "4287f5".into(),
            ..before.clone()
        };

        let merged = after.merge_onto(&before, server);
        assert_eq!(merged.label.title, "next up");
        assert_eq!(merged.label.hex_color, "4287f5");
        assert!(merged.collisions.is_empty());
    }

    #[test]
    fn a_true_collision_is_named_and_the_users_value_wins() {
        let before = Label {
            id: LabelId(41),
            title: "next".into(),
            ..Default::default()
        };
        let after = Label {
            title: "next up".into(),
            ..before.clone()
        };
        let server = Label {
            title: "upcoming".into(),
            ..before.clone()
        };

        let merged = after.merge_onto(&before, server);
        assert_eq!(merged.label.title, "next up");
        assert_eq!(merged.collisions, vec!["title"]);
    }

    #[test]
    fn the_merged_label_takes_its_id_from_the_server() {
        // The body's `id` beats the path -- `POST /labels/12` carrying `"id": 13` updated
        // label 13 and left 12 untouched, measured on dev 2026-08-29 -- and
        // `Client::update_label` derives the path from the body it is handed. So this is
        // the one place a mismatched pair could be built, and the server's id is the only
        // one that names a row the server has.
        let before = Label {
            id: LabelId(41),
            title: "next".into(),
            ..Default::default()
        };
        let after = Label {
            id: LabelId(13),
            title: "next up".into(),
            ..before.clone()
        };
        let server = Label {
            id: LabelId(41),
            ..before.clone()
        };

        let merged = after.merge_onto(&before, server);
        assert_eq!(merged.label.id, LabelId(41));
        assert!(
            !merged.collisions.contains(&"id"),
            "an id is not a field the user edits, so it cannot collide"
        );
    }

    #[test]
    fn a_recolour_and_a_rename_of_the_same_label_both_land() {
        // One variant covers title, colour and description because they are one request.
        // A merge that only carried one of them would silently drop the rest.
        let before = Label {
            id: LabelId(41),
            title: "next".into(),
            description: "soon".into(),
            hex_color: "aaaaaa".into(),
            ..Default::default()
        };
        let after = Label {
            title: "next up".into(),
            description: "very soon".into(),
            hex_color: "4287f5".into(),
            ..before.clone()
        };

        let merged = after.merge_onto(&before, before.clone());
        assert_eq!(merged.label.title, "next up");
        assert_eq!(merged.label.description, "very soon");
        assert_eq!(merged.label.hex_color, "4287f5");
        assert!(merged.collisions.is_empty(), "nobody else changed anything");
    }
}
