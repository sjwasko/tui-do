//! Projects and their views.

use serde::{Deserialize, Serialize};

use super::datetime::Timestamp;
use super::ids::{ProjectId, ViewId};
use super::user::User;

/// A project — Vikunja's container for tasks, nestable via `parent_project_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Project {
    /// Server-assigned identifier.
    ///
    /// Negative ids are pseudo-projects: `-1` is "Favorites". They appear in listings
    /// but reject writes.
    #[serde(default)]
    pub id: ProjectId,

    /// Project name.
    #[serde(default)]
    pub title: String,

    /// Longer description. May contain HTML, since the web UI edits it richly.
    #[serde(default)]
    pub description: String,

    /// Short prefix used to build task identifiers such as `WORK-42`.
    #[serde(default)]
    pub identifier: String,

    /// Background colour as six hex digits, without a leading `#`. Often empty.
    #[serde(default)]
    pub hex_color: String,

    /// Parent for nested projects; `0` when top-level.
    #[serde(default)]
    pub parent_project_id: ProjectId,

    /// Whether the project is archived. Archived projects are read-only.
    #[serde(default)]
    pub is_archived: bool,

    /// Whether the current user has favourited it.
    #[serde(default)]
    pub is_favorite: bool,

    /// Manual sort position within its parent.
    #[serde(default)]
    pub position: f64,

    /// The owning user.
    #[serde(default)]
    pub owner: Option<User>,

    /// The views defined on this project (List, Gantt, Table, Kanban).
    ///
    /// Only populated by endpoints that return a full project.
    #[serde(default)]
    pub views: Vec<ProjectView>,

    /// When it was created.
    #[serde(default)]
    pub created: Timestamp,

    /// When it was last modified.
    #[serde(default)]
    pub updated: Timestamp,
}

impl Project {
    /// Whether this is a server-provided pseudo-project rather than a real one.
    ///
    /// Vikunja exposes "Favorites" as project `-1`. It lists like any other project but
    /// cannot be edited, and creating a task "in" it is meaningless.
    #[must_use]
    pub fn is_pseudo(&self) -> bool {
        self.id.get() < 0
    }

    /// Whether tasks can be created in and moved to this project.
    #[must_use]
    pub fn accepts_writes(&self) -> bool {
        !self.is_pseudo() && !self.is_archived
    }
}

/// How a project view presents its tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ViewKind {
    /// Flat list. The default view.
    #[default]
    List,
    /// Timeline.
    Gantt,
    /// Spreadsheet-style columns.
    Table,
    /// Bucketed board.
    Kanban,
    /// A view kind this client does not know about.
    ///
    /// Present so a future Vikunja that adds a view kind degrades to "cannot render
    /// this one" instead of failing to deserialize the whole project.
    #[serde(other)]
    Unknown,
}

/// A saved presentation of a project's tasks.
///
/// Views matter beyond display: `GET /projects/{id}/views/{view}/tasks` is how the web
/// frontend actually loads tasks, and bucket positions are stored per view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProjectView {
    /// Server-assigned identifier.
    #[serde(default)]
    pub id: ViewId,

    /// The project this view belongs to.
    #[serde(default)]
    pub project_id: ProjectId,

    /// View name as shown in the UI.
    #[serde(default)]
    pub title: String,

    /// How this view presents tasks.
    #[serde(default)]
    pub view_kind: ViewKind,

    /// Ordering among a project's views.
    #[serde(default)]
    pub position: f64,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn favorites_pseudo_project_is_recognised() {
        let favorites = Project {
            id: ProjectId(-1),
            title: "Favorites".into(),
            ..Project::default()
        };
        assert!(favorites.is_pseudo());
        assert!(!favorites.accepts_writes());
    }

    #[test]
    fn archived_projects_reject_writes() {
        let archived = Project {
            id: ProjectId(4),
            is_archived: true,
            ..Project::default()
        };
        assert!(!archived.is_pseudo());
        assert!(!archived.accepts_writes());
    }

    #[test]
    fn ordinary_projects_accept_writes() {
        let ordinary = Project {
            id: ProjectId(4),
            ..Project::default()
        };
        assert!(ordinary.accepts_writes());
    }

    #[test]
    fn known_view_kinds_parse() {
        for (raw, expected) in [
            ("\"list\"", ViewKind::List),
            ("\"gantt\"", ViewKind::Gantt),
            ("\"table\"", ViewKind::Table),
            ("\"kanban\"", ViewKind::Kanban),
        ] {
            let parsed: ViewKind = serde_json::from_str(raw).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn unknown_view_kind_degrades_instead_of_failing() {
        // A future Vikunja adding a view kind must not break loading every project.
        let parsed: ViewKind = serde_json::from_str("\"timeline3d\"").unwrap();
        assert_eq!(parsed, ViewKind::Unknown);
    }
}
