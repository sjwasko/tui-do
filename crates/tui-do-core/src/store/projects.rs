//! Reading and writing projects, and the views defined on them.
//!
//! # Pseudo-projects are stored, not hidden
//!
//! `/projects` answers with `-1` Favorites, `-2` My Open Tasks and `-3` Inbox alongside
//! the real ones. They belong in the sidebar, so they are stored; they reject writes, so
//! [`ProjectFilter`] leaves them out unless asked for. The default is the one that
//! cannot offer a user a project the server will refuse to create a task in.
//!
//! # An empty view list means "not loaded", not "none"
//!
//! `GET /projects` returns projects without their views -- they come from
//! `GET /projects/{id}/views`. Every real project has at least a List view, so an empty
//! `views` is the listing's silence rather than the truth, and replacing the stored
//! views with it would erase what the views call fetched. Upserting a project with no
//! views therefore leaves the stored ones alone.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use tui_do_api::models::{Project, ProjectId, ProjectView, ViewId, ViewKind};

use super::sql::{escape_like, instant, joined_user, keep_ids, stamp, upsert_user};
use super::Store;
use crate::error::Result;

/// The columns a [`Project`] is built from, with its owner joined in.
const SELECT: &str = "SELECT projects.*,
                             owner.username AS owner_username,
                             owner.name     AS owner_name,
                             owner.email    AS owner_email
                        FROM projects
                        LEFT JOIN users AS owner ON owner.id = projects.owner_id";

/// Which projects to return.
///
/// The default excludes pseudo-projects, which is what a picker offering a write target
/// needs. A sidebar that wants Favorites asks for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFilter {
    /// Only children of this project. `ProjectId(0)` selects top-level projects.
    pub parent: Option<ProjectId>,

    /// Only archived, or only not. `None` returns both.
    pub archived: Option<bool>,

    /// Only favourites, or only not. `None` returns both.
    pub favorite: Option<bool>,

    /// Case-insensitive substring of the title.
    pub search: Option<String>,

    /// Include the server's pseudo-projects (`id < 0`).
    ///
    /// They cannot be written to, so this stays `false` for anything that offers a
    /// project to create a task in.
    pub include_pseudo: bool,

    /// At most this many rows.
    pub limit: Option<u32>,
}

/// What to order projects by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectOrder {
    /// By the manual position the server assigns, which is sidebar order.
    #[default]
    Position,
    /// By title, case-insensitively.
    Title,
    /// By id, which is creation order and always unambiguous.
    Id,
}

/// An ordering, with a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProjectSort {
    /// The column.
    pub order: ProjectOrder,
    /// Whether to reverse it.
    pub descending: bool,
}

impl ProjectSort {
    /// The `ORDER BY` fragment. Every value here is a literal in this file.
    fn sql(self) -> &'static str {
        match (self.order, self.descending) {
            (ProjectOrder::Position, false) => "position ASC, id ASC",
            (ProjectOrder::Position, true) => "position DESC, id ASC",
            (ProjectOrder::Title, false) => "title COLLATE NOCASE ASC, id ASC",
            (ProjectOrder::Title, true) => "title COLLATE NOCASE DESC, id ASC",
            (ProjectOrder::Id, false) => "id ASC",
            (ProjectOrder::Id, true) => "id DESC",
        }
    }
}

impl Store {
    /// Replace the stored copy of every project in `projects`.
    ///
    /// A project's views are written only when it carries some; see the module note.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure. The whole batch is one
    /// transaction, so a partial page never lands.
    pub async fn upsert_projects(&self, projects: Vec<Project>) -> Result<usize> {
        let now = Utc::now();
        self.write(move |tx| {
            for project in &projects {
                upsert_project(tx, project, now)?;
            }
            Ok(projects.len())
        })
        .await
    }

    /// Replace the views stored for one project.
    ///
    /// Takes the answer of `GET /projects/{id}/views`, which is authoritative about all
    /// of them. An empty list is honoured here, unlike in an upsert: this call means the
    /// views were asked for.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn set_project_views(
        &self,
        project: ProjectId,
        views: Vec<ProjectView>,
    ) -> Result<()> {
        self.write(move |tx| {
            replace_views(tx, project, &views)?;
            Ok(())
        })
        .await
    }

    /// Read one project, with its owner and views.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn project(&self, id: ProjectId) -> Result<Option<Project>> {
        self.read(move |connection| {
            let Some(mut project) = connection
                .query_row(
                    &format!("{SELECT} WHERE projects.id = ?1"),
                    params![id.get()],
                    row_to_project,
                )
                .optional()?
            else {
                return Ok(None);
            };
            project.views = views_for(connection, id)?;
            Ok(Some(project))
        })
        .await
    }

    /// Read the real project with this exact title, ignoring case.
    ///
    /// Quick-add writes `+Work`, and something has to turn that into an id.
    /// Pseudo-projects are never returned, since the only reason to resolve a name is to
    /// put a task in it. An archived project *is* returned -- it exists and the user
    /// named it -- so callers check [`Project::accepts_writes`] before writing.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn project_named(&self, title: impl Into<String>) -> Result<Option<Project>> {
        let title = title.into();
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "{SELECT} WHERE projects.title = ?1 COLLATE NOCASE AND projects.id > 0
                         ORDER BY projects.id ASC LIMIT 1"
                    ),
                    params![title],
                    row_to_project,
                )
                .optional()?)
        })
        .await
    }

    /// Read projects matching `filter`, ordered by `sort`, each with its views.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn projects(&self, filter: ProjectFilter, sort: ProjectSort) -> Result<Vec<Project>> {
        self.read(move |connection| {
            let mut sql = String::from(SELECT);
            let mut clauses: Vec<String> = Vec::new();
            let mut values: Vec<rusqlite::types::Value> = Vec::new();

            if !filter.include_pseudo {
                clauses.push("projects.id > 0".to_string());
            }
            if let Some(parent) = filter.parent {
                clauses.push(format!(
                    "projects.parent_project_id = ?{}",
                    values.len() + 1
                ));
                values.push(parent.get().into());
            }
            if let Some(archived) = filter.archived {
                clauses.push(format!("projects.is_archived = ?{}", values.len() + 1));
                values.push(i64::from(archived).into());
            }
            if let Some(favorite) = filter.favorite {
                clauses.push(format!("projects.is_favorite = ?{}", values.len() + 1));
                values.push(i64::from(favorite).into());
            }
            if let Some(search) = &filter.search {
                clauses.push(format!(
                    "projects.title LIKE ?{} ESCAPE '\\' COLLATE NOCASE",
                    values.len() + 1
                ));
                values.push(format!("%{}%", escape_like(search)).into());
            }

            if !clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&clauses.join(" AND "));
            }
            sql.push_str(" ORDER BY ");
            sql.push_str(sort.sql());
            if let Some(limit) = filter.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }

            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(values), row_to_project)?;
            let mut projects: Vec<Project> = Vec::new();
            for project in rows {
                projects.push(project?);
            }
            for project in &mut projects {
                project.views = views_for(connection, project.id)?;
            }
            Ok(projects)
        })
        .await
    }

    /// How many projects are stored: real ones, and pseudo ones.
    ///
    /// Reported separately because a count that includes Favorites does not match the
    /// number the web UI's sidebar shows, and explaining that once beats explaining it
    /// every time someone compares.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn project_counts(&self) -> Result<(i64, i64)> {
        self.read(|connection| {
            Ok(connection.query_row(
                "SELECT coalesce(sum(id > 0), 0), coalesce(sum(id < 0), 0) FROM projects",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .await
    }

    /// Remove projects the server no longer has, given the ids it does, and their tasks.
    ///
    /// The tasks go too because they went with the project server-side, and nothing else
    /// would ever remove them: `tasks.project_id` is deliberately not a foreign key, so
    /// a task can be stored before its project has been pulled. A task with unsent local
    /// changes is spared, exactly as in [`Store::retain_tasks`].
    ///
    /// Call this only after a *complete* projects pull.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn retain_projects(&self, keep: Vec<ProjectId>) -> Result<usize> {
        self.write(move |tx| {
            keep_ids(tx, keep.iter().map(|id| id.get()))?;
            let orphaned: Vec<i64> = {
                let mut statement = tx
                    .prepare("SELECT id FROM projects WHERE id NOT IN (SELECT id FROM keep_ids)")?;
                let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
                let mut ids = Vec::new();
                for id in rows {
                    ids.push(id?);
                }
                ids
            };
            {
                // Same guard as `retain_tasks`: a task with unsent local changes is
                // never removed by a pull. Without it, a project the listing dropped --
                // deleted by someone else, or simply missing from one response -- takes
                // the user's queued edits with it, and the entry left behind resurrects
                // the task as a ghost when the server answers 404.
                //
                // Only the tasks. `outbox.subject_kind = 'task'` is what makes
                // `subject_id` a task id here -- without that filter the same subquery
                // against `projects` would keep a project alive whenever its id happened
                // to match a pending task's, or (once labels are queueable) spare a task
                // whose id happened to match a pending label's.
                let mut delete_tasks = tx.prepare(
                    "DELETE FROM tasks
                      WHERE project_id = ?1
                        AND id NOT IN (SELECT subject_id FROM outbox
                                        WHERE subject_id IS NOT NULL AND subject_kind = 'task')",
                )?;
                for id in &orphaned {
                    delete_tasks.execute(params![id])?;
                }
            }
            Ok(tx.execute(
                "DELETE FROM projects WHERE id NOT IN (SELECT id FROM keep_ids)",
                [],
            )?)
        })
        .await
    }
}

/// Write one project, its owner, and its views if it carries any.
fn upsert_project(tx: &Transaction<'_>, project: &Project, now: DateTime<Utc>) -> Result<()> {
    if let Some(owner) = &project.owner {
        upsert_user(tx, owner)?;
    }
    tx.execute(
        "INSERT INTO projects (
            id, title, description, identifier, hex_color, parent_project_id,
            is_archived, is_favorite, position, owner_id, created, updated, synced_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (id) DO UPDATE SET
            title = excluded.title, description = excluded.description,
            identifier = excluded.identifier, hex_color = excluded.hex_color,
            parent_project_id = excluded.parent_project_id,
            is_archived = excluded.is_archived, is_favorite = excluded.is_favorite,
            position = excluded.position,
            -- An embedded copy without an owner must not erase a known one.
            owner_id = CASE WHEN excluded.owner_id = 0
                            THEN projects.owner_id ELSE excluded.owner_id END,
            created = excluded.created, updated = excluded.updated,
            synced_at = excluded.synced_at",
        params![
            project.id.get(),
            project.title,
            project.description,
            project.identifier,
            project.hex_color,
            project.parent_project_id.get(),
            i64::from(project.is_archived),
            i64::from(project.is_favorite),
            project.position,
            project.owner.as_ref().map_or(0, |u| u.id.get()),
            stamp(project.created.get()),
            stamp(project.updated.get()),
            now.to_rfc3339(),
        ],
    )?;

    // Empty means the listing did not populate them, not that there are none.
    if !project.views.is_empty() {
        replace_views(tx, project.id, &project.views)?;
    }
    Ok(())
}

/// Replace every view stored for one project.
fn replace_views(tx: &Transaction<'_>, project: ProjectId, views: &[ProjectView]) -> Result<()> {
    tx.execute(
        "DELETE FROM project_views WHERE project_id = ?1",
        params![project.get()],
    )?;
    for view in views {
        tx.execute(
            "INSERT INTO project_views (id, project_id, title, view_kind, position)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                project_id = excluded.project_id, title = excluded.title,
                view_kind = excluded.view_kind, position = excluded.position",
            params![
                view.id.get(),
                // The view's own `project_id` is empty on some payloads; the project it
                // was fetched for is the one that is always right.
                project.get(),
                view.title,
                view_kind_name(view.view_kind),
                view.position,
            ],
        )?;
    }
    Ok(())
}

/// The views defined on a project, in the order the web UI shows them.
fn views_for(connection: &Connection, project: ProjectId) -> Result<Vec<ProjectView>> {
    let mut statement = connection.prepare(
        "SELECT id, project_id, title, view_kind, position
           FROM project_views WHERE project_id = ?1
          ORDER BY position ASC, id ASC",
    )?;
    let rows = statement.query_map(params![project.get()], |row| {
        Ok(ProjectView {
            id: ViewId(row.get::<_, i64>(0)?),
            project_id: ProjectId(row.get::<_, i64>(1)?),
            title: row.get(2)?,
            view_kind: view_kind_from_name(&row.get::<_, String>(3)?),
            position: row.get(4)?,
        })
    })?;
    let mut views = Vec::new();
    for view in rows {
        views.push(view?);
    }
    Ok(views)
}

/// The stored spelling of a view kind. Matches the wire spelling, deliberately.
fn view_kind_name(kind: ViewKind) -> &'static str {
    match kind {
        ViewKind::List => "list",
        ViewKind::Gantt => "gantt",
        ViewKind::Table => "table",
        ViewKind::Kanban => "kanban",
        ViewKind::Unknown => "unknown",
    }
}

/// Read a view kind back. Anything unrecognised degrades rather than failing the row.
fn view_kind_from_name(name: &str) -> ViewKind {
    match name {
        "list" => ViewKind::List,
        "gantt" => ViewKind::Gantt,
        "table" => ViewKind::Table,
        "kanban" => ViewKind::Kanban,
        _ => ViewKind::Unknown,
    }
}

/// Build a project from a row produced by [`SELECT`]. Views are filled after.
fn row_to_project(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get::<_, i64>("id")?.into(),
        title: row.get("title")?,
        description: row.get("description")?,
        identifier: row.get("identifier")?,
        hex_color: row.get("hex_color")?,
        parent_project_id: row.get::<_, i64>("parent_project_id")?.into(),
        is_archived: row.get::<_, i64>("is_archived")? != 0,
        is_favorite: row.get::<_, i64>("is_favorite")? != 0,
        position: row.get("position")?,
        owner: joined_user(row, "owner_id", "owner")?,
        views: Vec::new(),
        created: instant(row, "created")?.into(),
        updated: instant(row, "updated")?.into(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use tui_do_api::models::{Label, LabelId, Task, TaskId, User, UserId};

    fn project(id: i64, title: &str) -> Project {
        Project {
            id: ProjectId(id),
            title: title.to_string(),
            ..Project::default()
        }
    }

    fn view(id: i64, title: &str, kind: ViewKind, position: f64) -> ProjectView {
        ProjectView {
            id: ViewId(id),
            project_id: ProjectId(1),
            title: title.to_string(),
            view_kind: kind,
            position,
        }
    }

    #[tokio::test]
    async fn a_project_round_trips_through_the_store() {
        let store = Store::in_memory().unwrap();
        let mut original = project(4, "Work");
        original.description = "the day job".into();
        original.identifier = "WORK".into();
        original.hex_color = "1973ff".into();
        original.parent_project_id = ProjectId(2);
        original.is_favorite = true;
        original.position = 65_536.0;
        original.owner = Some(User {
            id: UserId(1),
            username: "swasko".into(),
            ..User::default()
        });

        store.upsert_projects(vec![original.clone()]).await.unwrap();
        let read = store
            .project(ProjectId(4))
            .await
            .unwrap()
            .expect("the project");

        assert_eq!(read.title, "Work");
        assert_eq!(read.identifier, "WORK");
        assert_eq!(read.hex_color, "1973ff");
        assert_eq!(read.parent_project_id, ProjectId(2));
        assert!(read.is_favorite);
        assert_eq!(read.owner.unwrap().username, "swasko");
    }

    #[tokio::test]
    async fn a_missing_project_is_none_rather_than_an_error() {
        let store = Store::in_memory().unwrap();
        assert!(store.project(ProjectId(404)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pseudo_projects_are_stored_but_left_out_by_default() {
        // They belong in a sidebar and must never be offered as a write target, so the
        // default filter is the safe one.
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![
                project(-1, "Favorites"),
                project(-3, "Inbox"),
                project(1, "Work"),
            ])
            .await
            .unwrap();

        let offered = store
            .projects(ProjectFilter::default(), ProjectSort::default())
            .await
            .unwrap();
        let titles: Vec<&str> = offered.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["Work"]);
        assert!(offered.iter().all(|p| p.accepts_writes()));

        let sidebar = store
            .projects(
                ProjectFilter {
                    include_pseudo: true,
                    ..ProjectFilter::default()
                },
                ProjectSort {
                    order: ProjectOrder::Id,
                    descending: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(sidebar.len(), 3);
        assert_eq!(store.project_counts().await.unwrap(), (1, 2));
    }

    #[tokio::test]
    async fn views_survive_a_listing_that_does_not_carry_them() {
        // `GET /projects` returns no views. Treating that as "this project has none"
        // would erase what `GET /projects/{id}/views` fetched a moment earlier.
        let store = Store::in_memory().unwrap();
        let mut full = project(1, "Work");
        full.views = vec![
            view(11, "List", ViewKind::List, 100.0),
            view(12, "Kanban", ViewKind::Kanban, 200.0),
        ];
        store.upsert_projects(vec![full]).await.unwrap();

        store
            .upsert_projects(vec![project(1, "Work")])
            .await
            .unwrap();

        let read = store.project(ProjectId(1)).await.unwrap().unwrap();
        assert_eq!(read.views.len(), 2, "a listing erased the stored views");
        assert_eq!(read.views[0].view_kind, ViewKind::List);
        assert_eq!(read.views[1].title, "Kanban");
    }

    #[tokio::test]
    async fn setting_views_replaces_them_including_with_nothing() {
        // This call means the views were asked for, so its answer is the whole truth.
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![project(1, "Work")])
            .await
            .unwrap();
        store
            .set_project_views(
                ProjectId(1),
                vec![
                    view(11, "List", ViewKind::List, 100.0),
                    view(12, "Gantt", ViewKind::Gantt, 200.0),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .project(ProjectId(1))
                .await
                .unwrap()
                .unwrap()
                .views
                .len(),
            2
        );

        store
            .set_project_views(ProjectId(1), vec![view(11, "List", ViewKind::List, 100.0)])
            .await
            .unwrap();
        let read = store.project(ProjectId(1)).await.unwrap().unwrap();
        assert_eq!(read.views.len(), 1);
        assert_eq!(read.views[0].id, ViewId(11));
    }

    #[tokio::test]
    async fn an_unknown_view_kind_round_trips_as_unknown() {
        // A future Vikunja view kind must degrade to "cannot render this one", not fail
        // the row and take the project with it.
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![project(1, "Work")])
            .await
            .unwrap();
        store
            .set_project_views(
                ProjectId(1),
                vec![view(11, "Timeline3D", ViewKind::Unknown, 1.0)],
            )
            .await
            .unwrap();
        let read = store.project(ProjectId(1)).await.unwrap().unwrap();
        assert_eq!(read.views[0].view_kind, ViewKind::Unknown);
    }

    #[tokio::test]
    async fn filters_combine() {
        let store = Store::in_memory().unwrap();
        let mut archived = project(2, "Old");
        archived.is_archived = true;
        let mut child = project(3, "Sub");
        child.parent_project_id = ProjectId(1);
        let mut favorite = project(4, "Starred");
        favorite.is_favorite = true;

        store
            .upsert_projects(vec![project(1, "Work"), archived, child, favorite])
            .await
            .unwrap();

        let writable = store
            .projects(
                ProjectFilter {
                    archived: Some(false),
                    ..ProjectFilter::default()
                },
                ProjectSort {
                    order: ProjectOrder::Id,
                    descending: false,
                },
            )
            .await
            .unwrap();
        let titles: Vec<&str> = writable.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["Work", "Sub", "Starred"]);

        let children = store
            .projects(
                ProjectFilter {
                    parent: Some(ProjectId(1)),
                    ..ProjectFilter::default()
                },
                ProjectSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].title, "Sub");

        let favorites = store
            .projects(
                ProjectFilter {
                    favorite: Some(true),
                    ..ProjectFilter::default()
                },
                ProjectSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0].title, "Starred");
    }

    #[tokio::test]
    async fn top_level_projects_are_the_ones_with_no_parent() {
        let store = Store::in_memory().unwrap();
        let mut child = project(2, "Sub");
        child.parent_project_id = ProjectId(1);
        store
            .upsert_projects(vec![project(1, "Work"), child])
            .await
            .unwrap();

        let roots = store
            .projects(
                ProjectFilter {
                    parent: Some(ProjectId(0)),
                    ..ProjectFilter::default()
                },
                ProjectSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].title, "Work");
    }

    #[tokio::test]
    async fn searching_is_case_insensitive_and_treats_wildcards_literally() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![project(1, "Home Renovation"), project(2, "100% Rust")])
            .await
            .unwrap();

        let found = store
            .projects(
                ProjectFilter {
                    search: Some("renovation".into()),
                    ..ProjectFilter::default()
                },
                ProjectSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(found.len(), 1);

        let literal = store
            .projects(
                ProjectFilter {
                    search: Some("100%".into()),
                    ..ProjectFilter::default()
                },
                ProjectSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].id, ProjectId(2));
    }

    #[tokio::test]
    async fn projects_sort_by_position_by_default() {
        // Position is sidebar order, and it is not id order.
        let store = Store::in_memory().unwrap();
        let mut first = project(3, "first");
        first.position = 1.0;
        let mut second = project(1, "second");
        second.position = 2.0;
        store.upsert_projects(vec![second, first]).await.unwrap();

        let ordered = store
            .projects(ProjectFilter::default(), ProjectSort::default())
            .await
            .unwrap();
        let titles: Vec<&str> = ordered.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["first", "second"]);
    }

    #[tokio::test]
    async fn lookup_by_name_ignores_case_and_never_answers_with_a_pseudo_project() {
        // Quick-add's `+Work` resolves here, and resolving it to Favorites would produce
        // a task the server refuses to create.
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![project(-1, "Favorites"), project(2, "Work")])
            .await
            .unwrap();

        assert_eq!(
            store
                .project_named("work")
                .await
                .unwrap()
                .expect("a match")
                .id,
            ProjectId(2)
        );
        assert!(store.project_named("Favorites").await.unwrap().is_none());
        assert!(store.project_named("nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn retaining_projects_spares_a_task_with_unsent_changes() {
        // A project can vanish from one listing -- deleted by someone else, or simply
        // missing from a response -- and the cascade would take the user's queued edit
        // with it. The entry left behind then resurrects the task as a ghost when the
        // server answers 404 for a task it has never heard of.
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![project(1, "Work")])
            .await
            .unwrap();
        let edited = Task {
            id: TaskId(10),
            project_id: ProjectId(1),
            title: "original".into(),
            ..Task::default()
        };
        store.upsert_tasks(vec![edited.clone()]).await.unwrap();
        store
            .queue(crate::store::Mutation::UpdateTask {
                before: Box::new(edited.clone()),
                after: Box::new(Task {
                    title: "edited".into(),
                    ..edited
                }),
            })
            .await
            .unwrap();

        store.retain_projects(Vec::new()).await.unwrap();

        assert!(store.project(ProjectId(1)).await.unwrap().is_none());
        let survivor = store
            .task(TaskId(10))
            .await
            .unwrap()
            .expect("a queued edit was deleted with its project");
        assert_eq!(survivor.title, "edited");
    }

    #[tokio::test]
    async fn retaining_projects_does_not_spare_a_task_whose_id_collides_with_a_queued_labels_subject(
    ) {
        // `subject_id` is untyped and provisional ids count down from -1 per kind, so a
        // queued label can share an id with a real task -- here, both are 10. Without
        // `subject_kind` in the cascade's guard, the label's queue entry reads as "task
        // 10 has unsent changes", sparing it from its project's deletion and leaving a
        // task row pointing at a project that no longer exists.
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![project(1, "deleted server-side")])
            .await
            .unwrap();
        store
            .upsert_tasks(vec![Task {
                id: TaskId(10),
                project_id: ProjectId(1),
                title: "orphaned".into(),
                ..Task::default()
            }])
            .await
            .unwrap();

        // No `Mutation` variant produces a label subject yet -- that arrives with the
        // outbox's next task -- so the collision is built directly: an entry whose
        // `subject_id` matches task 10 but whose `subject_kind` says `label`.
        let payload = serde_json::to_string(&crate::store::Mutation::AttachLabel {
            task: TaskId(10),
            label: Box::new(Label {
                id: LabelId(1),
                title: "urgent".into(),
                ..Label::default()
            }),
        })
        .unwrap();
        store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO outbox (created, kind, payload, subject_id, subject_kind)
                     VALUES ('2026-08-29T00:00:00Z', 'attach_label', ?1, 10, 'label')",
                    params![payload],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        store.retain_projects(Vec::new()).await.unwrap();

        assert!(store.project(ProjectId(1)).await.unwrap().is_none());
        assert!(
            store.task(TaskId(10)).await.unwrap().is_none(),
            "a queued label sharing task 10's id spared it from its project's cascade"
        );
    }

    #[tokio::test]
    async fn retaining_removes_dropped_projects_their_views_and_their_tasks() {
        // Nothing else would ever remove those tasks: `tasks.project_id` is not a
        // foreign key, so the cascade cannot do it.
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![project(1, "kept"), project(2, "deleted server-side")])
            .await
            .unwrap();
        store
            .set_project_views(ProjectId(2), vec![view(21, "List", ViewKind::List, 1.0)])
            .await
            .unwrap();
        store
            .upsert_tasks(vec![
                Task {
                    id: TaskId(10),
                    project_id: ProjectId(1),
                    title: "survives".into(),
                    ..Task::default()
                },
                Task {
                    id: TaskId(11),
                    project_id: ProjectId(2),
                    title: "goes with its project".into(),
                    ..Task::default()
                },
            ])
            .await
            .unwrap();

        let removed = store.retain_projects(vec![ProjectId(1)]).await.unwrap();
        assert_eq!(removed, 1);
        assert!(store.project(ProjectId(2)).await.unwrap().is_none());
        assert!(store.task(TaskId(10)).await.unwrap().is_some());
        assert!(store.task(TaskId(11)).await.unwrap().is_none());

        let orphan_views: i64 = store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM project_views", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(orphan_views, 0, "the view cascade left a row behind");
    }
}
