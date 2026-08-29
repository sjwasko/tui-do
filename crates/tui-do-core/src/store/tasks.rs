//! Reading and writing tasks.
//!
//! The conversion between Vikunja's wire shape and the local one lives here and only
//! here: unset dates become `NULL` on the way in and Go's zero time on the way out, via
//! the API crate's helpers. Every rule that has to hold everywhere is cheapest to enforce
//! in one function.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use tui_do_api::models::{ProjectId, Task, TaskId, TaskReminder, User};

use super::labels::{labels_for, upsert_label};
use super::sql::{escape_like, instant, joined_user, keep_ids, stamp, upsert_user};
use super::Store;
use crate::error::Result;

/// The columns a [`Task`] is built from, with its creator joined in. Labels and
/// assignees are filled separately, since a task has many of each.
const SELECT: &str = "SELECT tasks.*,
                             creator.username AS creator_username,
                             creator.name     AS creator_name,
                             creator.email    AS creator_email
                        FROM tasks
                        LEFT JOIN users AS creator ON creator.id = tasks.created_by_id";

/// Which tasks to return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFilter {
    /// Only tasks in this project.
    pub project: Option<ProjectId>,

    /// Only done, or only not-done. `None` returns both.
    pub done: Option<bool>,

    /// Only tasks carrying this label.
    pub label: Option<tui_do_api::models::LabelId>,

    /// Only favourited tasks, or only unfavourited. `None` returns both.
    ///
    /// Vikunja presents favourites as pseudo-project `-1`, but the tasks themselves keep
    /// their real `project_id`, so this is a column predicate rather than a project.
    pub favorite: Option<bool>,

    /// Case-insensitive substring of the title.
    pub search: Option<String>,

    /// At most this many rows.
    pub limit: Option<u32>,
}

/// What [`Store::upsert_tasks_from_server`] did with a page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerApply {
    /// Tasks written.
    pub stored: usize,
    /// Tasks left alone because they have unsent local changes.
    pub skipped: usize,
}

/// How many tasks sit in one place, split by whether they are finished.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskCount {
    /// Tasks not yet done.
    pub open: i64,
    /// Tasks done.
    pub done: i64,
}

impl TaskCount {
    /// Open plus done.
    #[must_use]
    pub const fn total(self) -> i64 {
        self.open + self.done
    }
}

/// Counts for every project holding a task, and for favourites.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectCounts {
    /// Keyed by project. A project with no tasks is absent, not zero.
    pub by_project: HashMap<ProjectId, TaskCount>,
    /// Favourited tasks, which cut across projects.
    pub favorites: TaskCount,
}

impl ProjectCounts {
    /// The count for one project, zero when it holds nothing.
    #[must_use]
    pub fn for_project(&self, project: ProjectId) -> TaskCount {
        self.by_project.get(&project).copied().unwrap_or_default()
    }
}

/// What to order by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskOrder {
    /// By due date, with dateless tasks last.
    ///
    /// Last, not first: SQLite sorts `NULL` before everything, and a list where every
    /// undated task crowds out the ones actually due is useless.
    #[default]
    DueDate,
    /// By priority, highest first.
    Priority,
    /// By title, case-insensitively.
    Title,
    /// By the manual position a view assigns.
    Position,
    /// By start date, with startless tasks last, for the same reason as [`Self::DueDate`].
    StartDate,
    /// By when the task was created.
    Created,
    /// By when the task was last modified.
    Updated,
    /// By completion, open first.
    Status,
    /// By how complete the task is.
    PercentDone,
    /// By id, which is creation order and always unambiguous.
    Id,
}

/// An ordering, with a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskSort {
    /// The column.
    pub order: TaskOrder,
    /// Whether to reverse it.
    pub descending: bool,
}

impl TaskSort {
    /// The `ORDER BY` fragment.
    ///
    /// Built from an enum rather than a caller-supplied string, so there is no path from
    /// user input to SQL text. Every value here is a literal in this file.
    fn sql(self) -> &'static str {
        match (self.order, self.descending) {
            // `due_date IS NULL` sorts 0 before 1, putting real dates first either way.
            (TaskOrder::DueDate, false) => "due_date IS NULL, due_date ASC, id ASC",
            (TaskOrder::DueDate, true) => "due_date IS NULL, due_date DESC, id ASC",
            (TaskOrder::StartDate, false) => "start_date IS NULL, start_date ASC, id ASC",
            (TaskOrder::StartDate, true) => "start_date IS NULL, start_date DESC, id ASC",
            (TaskOrder::Updated, false) => "updated ASC, id ASC",
            (TaskOrder::Updated, true) => "updated DESC, id ASC",
            (TaskOrder::Status, false) => "done ASC, id ASC",
            (TaskOrder::Status, true) => "done DESC, id ASC",
            (TaskOrder::PercentDone, false) => "percent_done ASC, id ASC",
            (TaskOrder::PercentDone, true) => "percent_done DESC, id ASC",
            (TaskOrder::Priority, false) => "priority ASC, id ASC",
            (TaskOrder::Priority, true) => "priority DESC, id ASC",
            (TaskOrder::Title, false) => "title COLLATE NOCASE ASC, id ASC",
            (TaskOrder::Title, true) => "title COLLATE NOCASE DESC, id ASC",
            (TaskOrder::Position, false) => "position ASC, id ASC",
            (TaskOrder::Position, true) => "position DESC, id ASC",
            (TaskOrder::Created, false) => "created ASC, id ASC",
            (TaskOrder::Created, true) => "created DESC, id ASC",
            (TaskOrder::Id, false) => "id ASC",
            (TaskOrder::Id, true) => "id DESC",
        }
    }
}

impl Store {
    /// Replace the stored copy of every task in `tasks`.
    ///
    /// Used by a sync pull. Labels and assignees are replaced wholesale for each task,
    /// since the server's answer is authoritative about both.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure. The whole batch is one
    /// transaction, so a partial page never lands.
    pub async fn upsert_tasks(&self, tasks: Vec<Task>) -> Result<usize> {
        let now = Utc::now();
        self.write(move |tx| {
            for task in &tasks {
                upsert_task(tx, task, now)?;
            }
            Ok(tasks.len())
        })
        .await
    }

    /// Read one task, with its labels and assignees.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn task(&self, id: TaskId) -> Result<Option<Task>> {
        self.read(move |connection| {
            let Some(mut task) = connection
                .query_row(
                    &format!("{SELECT} WHERE tasks.id = ?1"),
                    params![id.get()],
                    row_to_task,
                )
                .optional()?
            else {
                return Ok(None);
            };
            task.labels = labels_for(connection, id)?;
            task.assignees = assignees_for(connection, id)?;
            task.reminders = reminders_for(connection, id)?;
            Ok(Some(task))
        })
        .await
    }

    /// Read tasks matching `filter`, ordered by `sort`.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn tasks(&self, filter: TaskFilter, sort: TaskSort) -> Result<Vec<Task>> {
        self.read(move |connection| {
            let mut sql = String::from(SELECT);
            let mut clauses: Vec<String> = Vec::new();
            let mut values: Vec<rusqlite::types::Value> = Vec::new();

            if let Some(label) = filter.label {
                sql.push_str(" JOIN task_labels ON task_labels.task_id = tasks.id");
                clauses.push(format!("task_labels.label_id = ?{}", values.len() + 1));
                values.push(label.get().into());
            }
            if let Some(project) = filter.project {
                clauses.push(format!("tasks.project_id = ?{}", values.len() + 1));
                values.push(project.get().into());
            }
            if let Some(done) = filter.done {
                clauses.push(format!("tasks.done = ?{}", values.len() + 1));
                values.push(i64::from(done).into());
            }
            if let Some(favorite) = filter.favorite {
                clauses.push(format!("tasks.is_favorite = ?{}", values.len() + 1));
                values.push(i64::from(favorite).into());
            }
            if let Some(search) = &filter.search {
                clauses.push(format!(
                    "tasks.title LIKE ?{} ESCAPE '\\' COLLATE NOCASE",
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
            let rows = statement.query_map(rusqlite::params_from_iter(values), row_to_task)?;

            let mut tasks: Vec<Task> = Vec::new();
            for task in rows {
                tasks.push(task?);
            }
            for task in &mut tasks {
                task.labels = labels_for(connection, task.id)?;
                task.assignees = assignees_for(connection, task.id)?;
                task.reminders = reminders_for(connection, task.id)?;
            }
            Ok(tasks)
        })
        .await
    }

    /// How many tasks are stored, in total and undone.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn task_counts(&self) -> Result<(i64, i64)> {
        self.read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*), coalesce(sum(done = 0), 0) FROM tasks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .await
    }

    /// Open and done counts for every project that holds a task, plus favourites.
    ///
    /// One grouped query rather than a count per project: the sidebar redraws whenever
    /// the store changes, and thirty round trips through `spawn_blocking` to render a
    /// column of numbers is thirty too many. Projects holding no tasks are absent rather
    /// than zero -- the caller has the project list and knows what a missing key means.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn project_task_counts(&self) -> Result<ProjectCounts> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT project_id,
                        coalesce(sum(done = 0), 0),
                        coalesce(sum(done = 1), 0)
                   FROM tasks
                  GROUP BY project_id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    ProjectId(row.get(0)?),
                    TaskCount {
                        open: row.get(1)?,
                        done: row.get(2)?,
                    },
                ))
            })?;
            let mut by_project = HashMap::new();
            for row in rows {
                let (project, count) = row?;
                by_project.insert(project, count);
            }

            let favorites = connection.query_row(
                "SELECT coalesce(sum(done = 0), 0), coalesce(sum(done = 1), 0)
                   FROM tasks WHERE is_favorite = 1",
                [],
                |row| {
                    Ok(TaskCount {
                        open: row.get(0)?,
                        done: row.get(1)?,
                    })
                },
            )?;

            Ok(ProjectCounts {
                by_project,
                favorites,
            })
        })
        .await
    }

    /// Store tasks a pull returned, leaving anything with unsent local changes alone.
    ///
    /// The server's copy of a task the user has just edited is older than what is on
    /// their screen, and writing it would undo the edit in front of them -- then the
    /// push would redo it, which is worse than either. Skipping is what makes an
    /// optimistic write hold until it is settled.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn upsert_tasks_from_server(&self, tasks: Vec<Task>) -> Result<ServerApply> {
        let now = Utc::now();
        self.write(move |tx| {
            let mut applied = ServerApply::default();
            let mut is_pending =
                tx.prepare("SELECT exists(SELECT 1 FROM outbox WHERE subject_id = ?1)")?;
            for task in &tasks {
                let pending: i64 =
                    is_pending.query_row(params![task.id.get()], |row| row.get(0))?;
                if pending == 1 {
                    applied.skipped += 1;
                    continue;
                }
                upsert_task(tx, task, now)?;
                applied.stored += 1;
            }
            Ok(applied)
        })
        .await
    }

    /// Remove tasks that the server no longer has.
    ///
    /// Takes the ids that *do* exist, because that is what a full pull produces. Only
    /// tasks in `projects` are considered, so a pull of one project cannot delete
    /// another project's tasks; pass an empty `projects` for a pull of everything.
    ///
    /// A task with unsent local changes is never removed. The server has not been told
    /// about it -- a task created offline has an id the server has never seen, and could
    /// not be in any keep-list -- so its absence proves nothing.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn retain_tasks(&self, projects: Vec<ProjectId>, keep: Vec<TaskId>) -> Result<usize> {
        self.write(move |tx| {
            let project_ids: Vec<i64> = projects.iter().map(|id| id.get()).collect();
            let mut removed = 0;

            keep_ids(tx, keep.iter().map(|id| id.get()))?;

            if project_ids.is_empty() {
                removed += tx.execute(
                    "DELETE FROM tasks
                      WHERE id NOT IN (SELECT id FROM keep_ids) AND id NOT IN (SELECT subject_id FROM outbox WHERE subject_id IS NOT NULL)",
                    [],
                )?;
            } else {
                let mut statement = tx.prepare(
                    "DELETE FROM tasks
                      WHERE project_id = ?1 AND id NOT IN (SELECT id FROM keep_ids)
                        AND id NOT IN (SELECT subject_id FROM outbox WHERE subject_id IS NOT NULL)",
                )?;
                for project in &project_ids {
                    removed += statement.execute(params![project])?;
                }
            }
            Ok(removed)
        })
        .await
    }
}

/// Write one task and its links.
pub(super) fn upsert_task(tx: &Transaction<'_>, task: &Task, now: DateTime<Utc>) -> Result<()> {
    tx.execute(
        "INSERT INTO tasks (
            id, project_id, title, description, done, done_at, priority, percent_done,
            position, due_date, start_date, end_date, repeat_after, repeat_mode,
            hex_color, identifier, task_index, is_favorite, bucket_id, comment_count,
            created_by_id, created, updated, synced_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
         )
         ON CONFLICT (id) DO UPDATE SET
            project_id = excluded.project_id, title = excluded.title,
            description = excluded.description, done = excluded.done,
            done_at = excluded.done_at, priority = excluded.priority,
            percent_done = excluded.percent_done, position = excluded.position,
            due_date = excluded.due_date, start_date = excluded.start_date,
            end_date = excluded.end_date, repeat_after = excluded.repeat_after,
            repeat_mode = excluded.repeat_mode, hex_color = excluded.hex_color,
            identifier = excluded.identifier, task_index = excluded.task_index,
            is_favorite = excluded.is_favorite, bucket_id = excluded.bucket_id,
            comment_count = excluded.comment_count,
            created_by_id = excluded.created_by_id, created = excluded.created,
            updated = excluded.updated, synced_at = excluded.synced_at",
        params![
            task.id.get(),
            task.project_id.get(),
            task.title,
            task.description,
            i64::from(task.done),
            stamp(task.done_at.get()),
            task.priority,
            task.percent_done,
            task.position,
            stamp(task.due_date.get()),
            stamp(task.start_date.get()),
            stamp(task.end_date.get()),
            task.repeat_after,
            i64::from(task.repeat_mode),
            task.hex_color,
            task.identifier,
            task.index,
            i64::from(task.is_favorite),
            task.bucket_id,
            task.comment_count,
            task.created_by.as_ref().map_or(0, |u| u.id.get()),
            stamp(task.created.get()),
            stamp(task.updated.get()),
            now.to_rfc3339(),
        ],
    )?;

    // Labels and assignees are replaced rather than merged: the server's answer is the
    // whole truth about both, and a merge would resurrect ones removed elsewhere.
    tx.execute(
        "DELETE FROM task_labels WHERE task_id = ?1",
        params![task.id.get()],
    )?;
    for label in &task.labels {
        upsert_label(tx, label, now)?;
        tx.execute(
            "INSERT OR IGNORE INTO task_labels (task_id, label_id) VALUES (?1, ?2)",
            params![task.id.get(), label.id.get()],
        )?;
    }

    tx.execute(
        "DELETE FROM task_assignees WHERE task_id = ?1",
        params![task.id.get()],
    )?;
    for user in &task.assignees {
        upsert_user(tx, user)?;
        tx.execute(
            "INSERT OR IGNORE INTO task_assignees (task_id, user_id) VALUES (?1, ?2)",
            params![task.id.get(), user.id.get()],
        )?;
    }

    // Replaced wholesale, like labels and assignees. Reminders are stored at all because
    // `POST /tasks/{id}` replaces them from the request body: a task that round-tripped
    // through a store with nowhere to keep them went back carrying `"reminders": []` and
    // the server deleted them.
    tx.execute(
        "DELETE FROM task_reminders WHERE task_id = ?1",
        params![task.id.get()],
    )?;
    for reminder in &task.reminders {
        tx.execute(
            "INSERT OR IGNORE INTO task_reminders (task_id, reminder, relative_period)
             VALUES (?1, ?2, ?3)",
            params![
                task.id.get(),
                stamp(reminder.reminder.get()),
                reminder.relative_period
            ],
        )?;
    }

    if let Some(creator) = &task.created_by {
        upsert_user(tx, creator)?;
    }

    Ok(())
}

/// The reminders set on a task.
///
/// Loaded with every task rather than on demand, because the reason they are stored is
/// that `update_task` sends them back — a task read for editing must carry them or the
/// server deletes them.
fn reminders_for(connection: &Connection, task: TaskId) -> Result<Vec<TaskReminder>> {
    let mut statement = connection.prepare(
        "SELECT reminder, relative_period FROM task_reminders
          WHERE task_id = ?1
          ORDER BY reminder, relative_period",
    )?;
    let rows = statement.query_map(params![task.get()], |row| {
        Ok(TaskReminder {
            reminder: instant(row, "reminder")?.into(),
            relative_period: row.get("relative_period")?,
        })
    })?;
    let mut reminders = Vec::new();
    for reminder in rows {
        reminders.push(reminder?);
    }
    Ok(reminders)
}

/// The users assigned to a task.
fn assignees_for(connection: &Connection, task: TaskId) -> Result<Vec<User>> {
    let mut statement = connection.prepare(
        "SELECT users.id, users.username, users.name, users.email
           FROM users JOIN task_assignees ON task_assignees.user_id = users.id
          WHERE task_assignees.task_id = ?1
          ORDER BY users.username COLLATE NOCASE",
    )?;
    let rows = statement.query_map(params![task.get()], |row| {
        Ok(User {
            id: row.get::<_, i64>(0)?.into(),
            username: row.get(1)?,
            name: row.get(2)?,
            email: row.get(3)?,
            ..User::default()
        })
    })?;
    let mut users = Vec::new();
    for user in rows {
        users.push(user?);
    }
    Ok(users)
}

/// Build a task from a row of the `tasks` table. Labels and assignees are filled after.
fn row_to_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get::<_, i64>("id")?.into(),
        project_id: row.get::<_, i64>("project_id")?.into(),
        title: row.get("title")?,
        description: row.get("description")?,
        done: row.get::<_, i64>("done")? != 0,
        done_at: instant(row, "done_at")?.into(),
        priority: row.get("priority")?,
        percent_done: row.get("percent_done")?,
        position: row.get("position")?,
        due_date: instant(row, "due_date")?.into(),
        start_date: instant(row, "start_date")?.into(),
        end_date: instant(row, "end_date")?.into(),
        repeat_after: row.get("repeat_after")?,
        repeat_mode: row.get::<_, i64>("repeat_mode")?.into(),
        hex_color: row.get("hex_color")?,
        identifier: row.get("identifier")?,
        index: row.get("task_index")?,
        is_favorite: row.get::<_, i64>("is_favorite")? != 0,
        bucket_id: row.get("bucket_id")?,
        comment_count: row.get("comment_count")?,
        created: instant(row, "created")?.into(),
        updated: instant(row, "updated")?.into(),
        created_by: joined_user(row, "created_by_id", "creator")?,
        ..Task::default()
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use tui_do_api::models::{Label, LabelId, User, UserId};

    fn task(id: i64, title: &str) -> Task {
        Task {
            id: TaskId(id),
            project_id: ProjectId(1),
            title: title.to_string(),
            ..Task::default()
        }
    }

    fn at(text: &str) -> Option<DateTime<Utc>> {
        Some(
            DateTime::parse_from_rfc3339(text)
                .unwrap()
                .with_timezone(&Utc),
        )
    }

    #[tokio::test]
    async fn a_task_round_trips_through_the_store() {
        let store = Store::in_memory().unwrap();
        let mut original = task(1, "Write the store");
        original.description = "with tests".into();
        original.priority = 4;
        original.due_date = at("2026-09-01T12:00:00Z").into();
        original.percent_done = 0.25;
        original.identifier = "CORE-1".into();

        store.upsert_tasks(vec![original.clone()]).await.unwrap();
        let read = store.task(TaskId(1)).await.unwrap().expect("the task");

        assert_eq!(read.title, original.title);
        assert_eq!(read.description, original.description);
        assert_eq!(read.priority, 4);
        assert_eq!(read.due_date.get(), original.due_date.get());
        assert!((read.percent_done - 0.25).abs() < f64::EPSILON);
        assert_eq!(read.identifier, "CORE-1");
    }

    #[tokio::test]
    async fn reminders_survive_the_store_because_a_write_sends_them_back() {
        // Measured on dev 2026-08-29: `POST /tasks/{id}` replaces a task's reminders from
        // the request body, so a task read out of a store that did not keep them went
        // back carrying `"reminders": []` and the server deleted them. Every optimistic
        // edit -- renaming a task, ticking it off -- destroyed its reminders silently.
        // Keeping them here is the whole fix; the assertion is that a read-mutate-write
        // cycle still has something to send.
        let store = Store::in_memory().unwrap();
        let mut original = task(1, "Renew the passport");
        original.reminders = vec![
            TaskReminder {
                reminder: at("2026-09-01T09:00:00Z").into(),
                relative_period: 0,
            },
            TaskReminder {
                reminder: None.into(),
                relative_period: -3600,
            },
        ];

        store.upsert_tasks(vec![original.clone()]).await.unwrap();
        let read = store.task(TaskId(1)).await.unwrap().expect("the task");
        assert_eq!(read.reminders.len(), 2, "{:?}", read.reminders);
        assert!(read
            .reminders
            .iter()
            .any(|r| r.reminder.get() == at("2026-09-01T09:00:00Z")));
        assert!(read.reminders.iter().any(|r| r.relative_period == -3600));

        // The list path loads them too: an edit made from the task list is the common
        // case, and it is the one that was losing them.
        let listed = store
            .tasks(TaskFilter::default(), TaskSort::default())
            .await
            .unwrap();
        let from_list = listed.iter().find(|t| t.id == TaskId(1)).expect("listed");
        assert_eq!(from_list.reminders.len(), 2);

        // And they are replaced wholesale rather than accumulated, like labels.
        let mut fewer = read.clone();
        fewer.reminders.truncate(1);
        store.upsert_tasks(vec![fewer]).await.unwrap();
        let again = store.task(TaskId(1)).await.unwrap().expect("the task");
        assert_eq!(again.reminders.len(), 1);
    }

    #[tokio::test]
    async fn an_unset_date_stays_unset_rather_than_becoming_year_one() {
        // The store does not inherit Vikunja's zero-time convention; the boundary
        // converts, and this is the assertion that keeps it honest.
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "no dates")]).await.unwrap();

        let read = store.task(TaskId(1)).await.unwrap().unwrap();
        assert_eq!(read.due_date.get(), None);
        assert_eq!(read.start_date.get(), None);
        assert_eq!(read.done_at.get(), None);

        let stored: Option<String> = store
            .read(|c| Ok(c.query_row("SELECT due_date FROM tasks WHERE id = 1", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(
            stored, None,
            "the column should be NULL, not a year-1 string"
        );
    }

    #[tokio::test]
    async fn upserting_the_same_task_updates_rather_than_duplicating() {
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "first")]).await.unwrap();
        store.upsert_tasks(vec![task(1, "second")]).await.unwrap();

        assert_eq!(store.task_counts().await.unwrap(), (1, 1));
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "second"
        );
    }

    #[tokio::test]
    async fn labels_and_assignees_are_replaced_not_merged() {
        // A merge would resurrect a label removed on another device.
        let store = Store::in_memory().unwrap();
        let mut with_two = task(1, "labelled");
        with_two.labels = vec![
            Label {
                id: LabelId(1),
                title: "a".into(),
                ..Label::default()
            },
            Label {
                id: LabelId(2),
                title: "b".into(),
                ..Label::default()
            },
        ];
        with_two.assignees = vec![
            User {
                id: UserId(1),
                username: "alice".into(),
                ..User::default()
            },
            User {
                id: UserId(2),
                username: "bob".into(),
                ..User::default()
            },
        ];
        store.upsert_tasks(vec![with_two.clone()]).await.unwrap();
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().labels.len(),
            2
        );

        let mut with_one = with_two.clone();
        with_one.labels.truncate(1);
        with_one.assignees.truncate(1);
        store.upsert_tasks(vec![with_one]).await.unwrap();

        let read = store.task(TaskId(1)).await.unwrap().unwrap();
        assert_eq!(read.labels.len(), 1);
        assert_eq!(read.labels[0].title, "a");
        assert_eq!(read.assignees.len(), 1);
        assert_eq!(read.assignees[0].username, "alice");
    }

    #[tokio::test]
    async fn a_missing_task_is_none_rather_than_an_error() {
        let store = Store::in_memory().unwrap();
        assert!(store.task(TaskId(404)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn filters_combine() {
        let store = Store::in_memory().unwrap();
        let mut done = task(1, "finished");
        done.done = true;
        let mut other_project = task(2, "elsewhere");
        other_project.project_id = ProjectId(2);
        let mut labelled = task(3, "tagged");
        labelled.labels = vec![Label {
            id: LabelId(9),
            title: "urgent".into(),
            ..Label::default()
        }];

        store
            .upsert_tasks(vec![done, other_project, labelled, task(4, "plain")])
            .await
            .unwrap();

        let open_in_one = store
            .tasks(
                TaskFilter {
                    project: Some(ProjectId(1)),
                    done: Some(false),
                    ..TaskFilter::default()
                },
                TaskSort::default(),
            )
            .await
            .unwrap();
        let titles: Vec<&str> = open_in_one.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["tagged", "plain"]);

        let by_label = store
            .tasks(
                TaskFilter {
                    label: Some(LabelId(9)),
                    ..TaskFilter::default()
                },
                TaskSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(by_label.len(), 1);
        assert_eq!(by_label[0].title, "tagged");
    }

    #[tokio::test]
    async fn favourites_are_a_predicate_not_a_project() {
        // Vikunja shows favourites as pseudo-project -1, but the tasks keep their real
        // project. Filtering by project -1 would return nothing at all.
        let store = Store::in_memory().unwrap();
        let mut starred = task(1, "starred");
        starred.is_favorite = true;
        starred.project_id = ProjectId(7);
        store
            .upsert_tasks(vec![starred, task(2, "ordinary")])
            .await
            .unwrap();

        let favorites = store
            .tasks(
                TaskFilter {
                    favorite: Some(true),
                    ..TaskFilter::default()
                },
                TaskSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0].title, "starred");
        assert_eq!(favorites[0].project_id, ProjectId(7));
    }

    #[tokio::test]
    async fn counts_come_back_per_project_split_by_done() {
        let store = Store::in_memory().unwrap();
        let mut finished = task(1, "finished");
        finished.done = true;
        let mut elsewhere = task(2, "elsewhere");
        elsewhere.project_id = ProjectId(2);
        let mut starred = task(3, "starred");
        starred.is_favorite = true;
        store
            .upsert_tasks(vec![finished, elsewhere, starred])
            .await
            .unwrap();

        let counts = store.project_task_counts().await.unwrap();
        assert_eq!(counts.for_project(ProjectId(1)).open, 1);
        assert_eq!(counts.for_project(ProjectId(1)).done, 1);
        assert_eq!(counts.for_project(ProjectId(2)).total(), 1);
        assert_eq!(counts.favorites.open, 1);

        // A project holding nothing is absent rather than zero, and reads as zero anyway.
        assert!(!counts.by_project.contains_key(&ProjectId(3)));
        assert_eq!(counts.for_project(ProjectId(3)), TaskCount::default());
    }

    #[tokio::test]
    async fn dateless_tasks_sort_last_not_first() {
        // SQLite puts NULL first. A list where every undated task crowds out the ones
        // actually due is worse than useless -- and with 3,869 of 3,876 dev tasks
        // dateless, this is the common case, not an edge one.
        let store = Store::in_memory().unwrap();
        let mut soon = task(1, "soon");
        soon.due_date = at("2026-09-01T12:00:00Z").into();
        let mut later = task(2, "later");
        later.due_date = at("2026-12-01T12:00:00Z").into();

        store
            .upsert_tasks(vec![task(3, "whenever"), later, soon])
            .await
            .unwrap();

        let ordered = store
            .tasks(TaskFilter::default(), TaskSort::default())
            .await
            .unwrap();
        let titles: Vec<&str> = ordered.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["soon", "later", "whenever"]);
    }

    #[tokio::test]
    async fn sorting_is_stable_on_ties() {
        // Every ordering falls back to id, so a redraw cannot shuffle equal rows.
        let store = Store::in_memory().unwrap();
        store
            .upsert_tasks(vec![task(3, "c"), task(1, "a"), task(2, "b")])
            .await
            .unwrap();
        let sort = TaskSort {
            order: TaskOrder::Priority,
            descending: true,
        };
        let first: Vec<i64> = store
            .tasks(TaskFilter::default(), sort)
            .await
            .unwrap()
            .iter()
            .map(|t| t.id.get())
            .collect();
        assert_eq!(first, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn search_is_case_insensitive_and_treats_wildcards_literally() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_tasks(vec![
                task(1, "Renew the DOMAIN"),
                task(2, "Pay 100% of the invoice"),
                task(3, "unrelated"),
            ])
            .await
            .unwrap();

        let found = store
            .tasks(
                TaskFilter {
                    search: Some("domain".into()),
                    ..TaskFilter::default()
                },
                TaskSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(found.len(), 1);

        // Without escaping, "%" is a wildcard and this matches everything.
        let literal = store
            .tasks(
                TaskFilter {
                    search: Some("100%".into()),
                    ..TaskFilter::default()
                },
                TaskSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].id, TaskId(2));
    }

    #[tokio::test]
    async fn a_limit_is_respected() {
        let store = Store::in_memory().unwrap();
        let many: Vec<Task> = (1..=10).map(|i| task(i, &format!("task {i}"))).collect();
        store.upsert_tasks(many).await.unwrap();

        let page = store
            .tasks(
                TaskFilter {
                    limit: Some(3),
                    ..TaskFilter::default()
                },
                TaskSort {
                    order: TaskOrder::Id,
                    descending: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[0].id, TaskId(1));
    }

    #[tokio::test]
    async fn retaining_removes_only_what_the_server_dropped() {
        let store = Store::in_memory().unwrap();
        let mut elsewhere = task(9, "another project");
        elsewhere.project_id = ProjectId(2);
        store
            .upsert_tasks(vec![
                task(1, "kept"),
                task(2, "deleted server-side"),
                elsewhere,
            ])
            .await
            .unwrap();

        // A pull of project 1 must not touch project 2.
        let removed = store
            .retain_tasks(vec![ProjectId(1)], vec![TaskId(1)])
            .await
            .unwrap();
        assert_eq!(removed, 1);
        assert!(store.task(TaskId(1)).await.unwrap().is_some());
        assert!(store.task(TaskId(2)).await.unwrap().is_none());
        assert!(
            store.task(TaskId(9)).await.unwrap().is_some(),
            "a project-scoped pull deleted another project's tasks"
        );
    }

    #[tokio::test]
    async fn a_batch_of_tasks_lands_all_or_nothing() {
        let store = Store::in_memory().unwrap();
        let batch: Vec<Task> = (1..=200).map(|i| task(i, &format!("task {i}"))).collect();
        assert_eq!(store.upsert_tasks(batch).await.unwrap(), 200);
        assert_eq!(store.task_counts().await.unwrap(), (200, 200));
    }
}
