//! Reading and writing tasks.
//!
//! The conversion between Vikunja's wire shape and the local one lives here and only
//! here: unset dates become `NULL` on the way in and Go's zero time on the way out, via
//! the API crate's helpers. Every rule that has to hold everywhere is cheapest to enforce
//! in one function.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use tui_do_api::models::{ProjectId, ReminderRelation, Task, TaskId, TaskReminder, User};

use super::labels::{attach_label, labels_for_tasks};
use super::sql::{escape_like, group_by_task, instant, joined_user, keep_ids, stamp, upsert_user};
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
    /// The oldest `updated` among the tasks that were skipped.
    ///
    /// The pull saw these and chose not to store them, so the watermark must not move
    /// past them or no incremental pull will ever ask for them again. See
    /// [`crate::sync::Sync::pull_with`].
    pub oldest_skipped: Option<DateTime<Utc>>,
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

    /// The username of every user the store has seen, sorted ignoring case.
    ///
    /// There is no user list to pull: a user is stored when a task arrives naming them as
    /// its creator or an assignee. So this is everyone who has created or been given a
    /// task this box knows, which is what `@` completes from in `tui-do add`.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn usernames(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = self
            .users()
            .await?
            .into_iter()
            .map(|user| user.username)
            .collect();
        // Sorted already; two ids under one name are one name to type.
        names.dedup();
        Ok(names)
    }

    /// Every user the store has seen, with the id an assignment needs, sorted by
    /// username ignoring case.
    ///
    /// The same people as [`Self::usernames`]: there is no user list to pull, so this is
    /// everyone who has created or been given a task this box knows. Quick-add resolves
    /// `@name` against it, because the server assigns by id, not by name (BUG-34).
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn users(&self) -> Result<Vec<User>> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, username, name FROM users WHERE username <> ''
                  ORDER BY username COLLATE NOCASE, id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(User {
                    id: tui_do_api::models::UserId(row.get(0)?),
                    username: row.get(1)?,
                    name: row.get(2)?,
                    ..User::default()
                })
            })?;
            let mut users = Vec::new();
            for user in rows {
                users.push(user?);
            }
            Ok(users)
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
            fill_children(connection, std::slice::from_mut(&mut task))?;
            Ok(Some(task))
        })
        .await
    }

    /// Read tasks matching `filter`, ordered by `sort`.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn tasks(&self, filter: TaskFilter, sort: TaskSort) -> Result<Vec<Task>> {
        self.read(move |connection| read_tasks(connection, &filter, sort))
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
            let mut is_pending = tx.prepare(
                "SELECT exists(SELECT 1 FROM outbox
                                WHERE subject_id = ?1 AND subject_kind = 'task')",
            )?;
            for task in &tasks {
                let pending: i64 =
                    is_pending.query_row(params![task.id.get()], |row| row.get(0))?;
                if pending == 1 {
                    applied.skipped += 1;
                    // Remember how far back the watermark has to stay. Without this the
                    // server's copy of a task with a queued change is dropped on the
                    // floor and never asked for again: the pull advances past it, and
                    // `updated` only moves when someone edits it *again*.
                    if let Some(updated) = task.updated.get() {
                        applied.oldest_skipped = Some(
                            applied
                                .oldest_skipped
                                .map_or(updated, |oldest| oldest.min(updated)),
                        );
                    }
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
    /// A task in an **archived** project is never deleted here, whatever the listing said.
    ///
    /// Measured on dev 2026-08-31: an unfiltered `GET /tasks` returned 3,879 tasks over 78
    /// pages and did not include any of the four in an archived project, which the server
    /// still holds and still serves from `GET /projects/{id}/tasks`. So the listing is
    /// *incomplete* with respect to archived projects, and absence from it is not evidence
    /// that a task is gone — the same reasoning that stops `Reach::Incremental` retaining
    /// at all.
    ///
    /// Without this guard every full pull deleted the user's archived work, silently and
    /// repeatedly. It is the second half of a bug whose first half was fixed by passing
    /// `is_archived=true` to `GET /projects`: that made the *projects* survive, and their
    /// tasks went on being deleted underneath them.
    pub async fn retain_tasks(&self, projects: Vec<ProjectId>, keep: Vec<TaskId>) -> Result<usize> {
        self.write(move |tx| {
            let project_ids: Vec<i64> = projects.iter().map(|id| id.get()).collect();
            let mut removed = 0;

            keep_ids(tx, keep.iter().map(|id| id.get()))?;

            if project_ids.is_empty() {
                removed += tx.execute(
                    "DELETE FROM tasks
                      WHERE id NOT IN (SELECT id FROM keep_ids)
                        AND id NOT IN (SELECT subject_id FROM outbox WHERE subject_id IS NOT NULL AND subject_kind = 'task')
                        AND project_id NOT IN (SELECT id FROM projects WHERE is_archived = 1)",
                    [],
                )?;
            } else {
                let mut statement = tx.prepare(
                    "DELETE FROM tasks
                      WHERE project_id = ?1 AND id NOT IN (SELECT id FROM keep_ids)
                        AND id NOT IN (SELECT subject_id FROM outbox WHERE subject_id IS NOT NULL AND subject_kind = 'task')
                        AND project_id NOT IN (SELECT id FROM projects WHERE is_archived = 1)",
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
        attach_label(tx, task.id, label, now)?;
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
            "INSERT OR IGNORE INTO task_reminders
                (task_id, reminder, relative_period, relative_to)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                task.id.get(),
                stamp(reminder.reminder.get()),
                reminder.relative_period,
                reminder
                    .relative_to
                    .as_ref()
                    .map_or("", ReminderRelation::as_str),
            ],
        )?;
    }

    if let Some(creator) = &task.created_by {
        upsert_user(tx, creator)?;
    }

    Ok(())
}

/// The tasks matching `filter`, ordered by `sort`, each with its labels, assignees and
/// reminders. The body of [`Store::tasks`].
fn read_tasks(connection: &Connection, filter: &TaskFilter, sort: TaskSort) -> Result<Vec<Task>> {
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
    fill_children(connection, &mut tasks)?;
    Ok(tasks)
}

/// Fill in the labels, assignees and reminders of `tasks`: one statement per table,
/// however many tasks there are.
///
/// BUG-28. This was three statements per task, about 5,800 for the open list on dev,
/// and a held key keeps one task-list read running back to back on the store's one
/// connection, so the push got about 1.4 sends a second. Each child table is read once
/// for every id at the same time and the rows dealt out in memory, in the order the
/// per-task statements gave them.
fn fill_children(connection: &Connection, tasks: &mut [Task]) -> Result<()> {
    if tasks.is_empty() {
        return Ok(());
    }
    let ids: Vec<TaskId> = tasks.iter().map(|task| task.id).collect();
    let mut labels = labels_for_tasks(connection, &ids)?;
    let mut assignees = assignees_for_tasks(connection, &ids)?;
    let mut reminders = reminders_for_tasks(connection, &ids)?;
    for task in tasks {
        task.labels = labels.remove(&task.id).unwrap_or_default();
        task.assignees = assignees.remove(&task.id).unwrap_or_default();
        task.reminders = reminders.remove(&task.id).unwrap_or_default();
    }
    Ok(())
}

/// The reminders set on each of `tasks`, keyed by task.
///
/// Loaded with every task rather than on demand, because the reason they are stored is
/// that `update_task` sends them back — a task read for editing must carry them or the
/// server deletes them.
fn reminders_for_tasks(
    connection: &Connection,
    tasks: &[TaskId],
) -> Result<HashMap<TaskId, Vec<TaskReminder>>> {
    group_by_task(
        connection,
        "SELECT task_id, reminder, relative_period, relative_to FROM task_reminders
          WHERE task_id IN (SELECT value FROM json_each(?1))
          ORDER BY task_id, reminder, relative_period, relative_to",
        tasks,
        |row| {
            Ok(TaskReminder {
                reminder: instant(row, "reminder")?.into(),
                relative_period: row.get("relative_period")?,
                relative_to: ReminderRelation::parse(&row.get::<_, String>("relative_to")?),
            })
        },
    )
}

/// The users assigned to each of `tasks`, keyed by task, each task's in username order.
fn assignees_for_tasks(
    connection: &Connection,
    tasks: &[TaskId],
) -> Result<HashMap<TaskId, Vec<User>>> {
    group_by_task(
        connection,
        "SELECT task_assignees.task_id, users.id, users.username, users.name, users.email
           FROM users JOIN task_assignees ON task_assignees.user_id = users.id
          WHERE task_assignees.task_id IN (SELECT value FROM json_each(?1))
          ORDER BY task_assignees.task_id, users.username COLLATE NOCASE, users.id",
        tasks,
        |row| {
            Ok(User {
                id: row.get::<_, i64>(1)?.into(),
                username: row.get(2)?,
                name: row.get(3)?,
                email: row.get(4)?,
                ..User::default()
            })
        },
    )
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
    use crate::models::Project;
    use tui_do_api::models::{Label, LabelId, User, UserId};

    /// The tasks of an archived project survive a retain that does not mention them.
    ///
    /// Driven by hand on 2026-08-31: a project archived in the web UI lost all four of its
    /// tasks from the local store on the next `R`, while the server still had them. The
    /// unfiltered `GET /tasks` that feeds `keep` omits them — measured, 3,879 tasks over 78
    /// pages with none of the four — so retaining against it deletes work the user can still
    /// see in the browser.
    ///
    /// The existing `an_archived_project_and_its_tasks_survive_a_pull` mocks the server's
    /// answer rather than observing it, so it proved the handling and not the premise.
    #[tokio::test]
    async fn an_archived_projects_tasks_are_not_deleted_by_a_listing_that_omits_them() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![
                Project {
                    id: ProjectId(1),
                    title: "Live".to_string(),
                    ..Project::default()
                },
                Project {
                    id: ProjectId(31),
                    title: "Archive-test".to_string(),
                    is_archived: true,
                    ..Project::default()
                },
            ])
            .await
            .unwrap();
        store
            .upsert_tasks(vec![
                task(1, "still listed"),
                Task {
                    id: TaskId(3880),
                    project_id: ProjectId(31),
                    title: "in the archived project".to_string(),
                    ..Task::default()
                },
            ])
            .await
            .unwrap();

        // What a full pull hands over: everything the listing named. The archived
        // project's task is absent because the server does not list it.
        store
            .retain_tasks(Vec::new(), vec![TaskId(1)])
            .await
            .unwrap();

        let ids: Vec<i64> = store
            .tasks(TaskFilter::default(), TaskSort::default())
            .await
            .unwrap()
            .iter()
            .map(|t| t.id.get())
            .collect();
        assert!(
            ids.contains(&3880),
            "the archived project's task was deleted by a listing that cannot name it: {ids:?}"
        );
        assert!(ids.contains(&1), "the listed task should still be here too");
    }

    /// And the per-project path, which a scoped pull takes.
    #[tokio::test]
    async fn the_same_holds_when_the_retain_names_projects() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_projects(vec![Project {
                id: ProjectId(31),
                title: "Archive-test".to_string(),
                is_archived: true,
                ..Project::default()
            }])
            .await
            .unwrap();
        store
            .upsert_tasks(vec![Task {
                id: TaskId(3881),
                project_id: ProjectId(31),
                title: "archived".to_string(),
                ..Task::default()
            }])
            .await
            .unwrap();

        store
            .retain_tasks(vec![ProjectId(31)], Vec::new())
            .await
            .unwrap();

        let remaining = store
            .tasks(TaskFilter::default(), TaskSort::default())
            .await
            .unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "the per-project retain deleted an archived project's task"
        );
    }
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
    async fn a_pull_does_not_skip_a_task_whose_id_collides_with_a_queued_labels_subject() {
        // `subject_id` is untyped and provisional ids count down from -1 per kind, so a
        // queued label can share an id with a real task -- here, both are 1. Without
        // `subject_kind` in this guard, the label's queue entry reads as "task 1 has
        // unsent changes", and the server's copy of task 1 is skipped forever: not just
        // this pull, but every one after it, since the watermark clamp holds `LAST_PULL`
        // back to a task that in fact has nothing queued against it.
        let store = Store::in_memory().unwrap();
        store.upsert_tasks(vec![task(1, "original")]).await.unwrap();

        // No `Mutation` variant produces a label subject yet -- that arrives with the
        // outbox's next task -- so the collision is built directly: an entry whose
        // `subject_id` matches task 1 but whose `subject_kind` says `label`. The payload
        // is a real serialized `Mutation::AttachLabel` rather than a placeholder, so a
        // future reader of the queue meets a value it can actually parse.
        let payload = serde_json::to_string(&crate::store::Mutation::AttachLabel {
            task: TaskId(1),
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
                     VALUES ('2026-08-29T00:00:00Z', 'attach_label', ?1, 1, 'label')",
                    params![payload],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let applied = store
            .upsert_tasks_from_server(vec![task(1, "the server's newer title")])
            .await
            .unwrap();

        assert_eq!(
            applied.skipped, 0,
            "a queued label sharing task 1's id made the pull think task 1 had unsent \
             changes"
        );
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().title,
            "the server's newer title"
        );
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
                relative_to: None,
            },
            TaskReminder {
                reminder: None.into(),
                relative_period: -3600,
                relative_to: Some(ReminderRelation::DueDate),
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
    async fn a_reminder_keeps_what_it_is_relative_to_through_the_store() {
        // Measured on dev 2026-11-10: an update carrying a relative reminder without its
        // `relative_to` is refused with `400` / 4022. A task read out of the store is the
        // body of the next write, so the store has to keep the relation. Two reminders
        // the same distance before different dates are different reminders, so the
        // relation is part of what identifies a row too.
        let store = Store::in_memory().unwrap();
        let original: Task = serde_json::from_value(serde_json::json!({
            "id": 1,
            "project_id": 1,
            "title": "Renew the passport",
            "reminders": [
                {"reminder": "2026-12-01T11:00:00Z", "relative_period": -3600, "relative_to": "due_date"},
                {"reminder": "2026-12-01T11:00:00Z", "relative_period": -3600, "relative_to": "start_date"},
                {"reminder": "2026-12-02T10:00:00Z", "relative_period": 0, "relative_to": ""}
            ]
        }))
        .unwrap();

        store.upsert_tasks(vec![original]).await.unwrap();
        let read = store.task(TaskId(1)).await.unwrap().expect("the task");

        let body = serde_json::to_value(&read).unwrap();
        let mut relations: Vec<String> = body["reminders"]
            .as_array()
            .expect("reminders")
            .iter()
            .map(|r| r["relative_to"].as_str().unwrap_or("").to_string())
            .collect();
        relations.sort();
        assert_eq!(relations, vec!["", "due_date", "start_date"], "{body}");
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
    async fn usernames_are_everyone_a_stored_task_names() {
        let store = Store::in_memory().unwrap();
        assert!(store.usernames().await.unwrap().is_empty());
        let mut assigned = task(1, "assigned");
        assigned.assignees = ["bob", "Alice"]
            .into_iter()
            .zip(1..)
            .map(|(name, id)| User {
                id: UserId(id),
                username: name.into(),
                ..User::default()
            })
            .collect();
        let mut again = task(2, "again");
        again.assignees = vec![assigned.assignees[0].clone()];
        store.upsert_tasks(vec![assigned, again]).await.unwrap();
        assert_eq!(store.usernames().await.unwrap(), vec!["Alice", "bob"]);
        // The same people, carrying the ids an assignment needs (BUG-34).
        let users = store.users().await.unwrap();
        let pairs: Vec<(i64, &str)> = users
            .iter()
            .map(|user| (user.id.get(), user.username.as_str()))
            .collect();
        assert_eq!(pairs, vec![(2, "Alice"), (1, "bob")]);
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

    /// BUG-28: the children of a task list are read once per table, not once per task.
    mod batched {
        use std::cell::Cell;

        use super::*;

        /// The per-task reads `Store::tasks` made before BUG-28, kept as the reference
        /// the batched reads must agree with.
        mod per_task {
            use super::*;
            use crate::store::labels::row_to_label;

            pub(super) fn labels_for(connection: &Connection, task: TaskId) -> Vec<Label> {
                let mut statement = connection
                    .prepare(
                        "SELECT labels.*,
                                creator.username AS creator_username,
                                creator.name     AS creator_name,
                                creator.email    AS creator_email
                           FROM labels
                           LEFT JOIN users AS creator ON creator.id = labels.created_by_id
                           JOIN task_labels ON task_labels.label_id = labels.id
                          WHERE task_labels.task_id = ?1
                          ORDER BY labels.title COLLATE NOCASE",
                    )
                    .unwrap();
                let rows = statement
                    .query_map(params![task.get()], row_to_label)
                    .unwrap();
                rows.map(rusqlite::Result::unwrap).collect()
            }

            pub(super) fn assignees_for(connection: &Connection, task: TaskId) -> Vec<User> {
                let mut statement = connection
                    .prepare(
                        "SELECT users.id, users.username, users.name, users.email
                           FROM users JOIN task_assignees ON task_assignees.user_id = users.id
                          WHERE task_assignees.task_id = ?1
                          ORDER BY users.username COLLATE NOCASE",
                    )
                    .unwrap();
                let rows = statement
                    .query_map(params![task.get()], |row| {
                        Ok(User {
                            id: row.get::<_, i64>(0)?.into(),
                            username: row.get(1)?,
                            name: row.get(2)?,
                            email: row.get(3)?,
                            ..User::default()
                        })
                    })
                    .unwrap();
                rows.map(rusqlite::Result::unwrap).collect()
            }

            pub(super) fn reminders_for(
                connection: &Connection,
                task: TaskId,
            ) -> Vec<TaskReminder> {
                let mut statement = connection
                    .prepare(
                        "SELECT reminder, relative_period, relative_to FROM task_reminders
                          WHERE task_id = ?1
                          ORDER BY reminder, relative_period, relative_to",
                    )
                    .unwrap();
                let rows = statement
                    .query_map(params![task.get()], |row| {
                        Ok(TaskReminder {
                            reminder: instant(row, "reminder")?.into(),
                            relative_period: row.get("relative_period")?,
                            relative_to: ReminderRelation::parse(
                                &row.get::<_, String>("relative_to")?,
                            ),
                        })
                    })
                    .unwrap();
                rows.map(rusqlite::Result::unwrap).collect()
            }
        }

        fn label(id: i64, title: &str, creator: Option<&User>) -> Label {
            Label {
                id: LabelId(id),
                title: title.to_string(),
                hex_color: format!("{id:06x}"),
                created_by: creator.cloned(),
                ..Label::default()
            }
        }

        fn user(id: i64, username: &str) -> User {
            User {
                id: UserId(id),
                username: username.to_string(),
                name: format!("Name {id}"),
                ..User::default()
            }
        }

        fn reminder(at_text: &str, relative_period: i64) -> TaskReminder {
            // A relative reminder carries its relation, as the server insists it must.
            TaskReminder {
                reminder: at(at_text).into(),
                relative_period,
                relative_to: (relative_period != 0).then_some(ReminderRelation::StartDate),
            }
        }

        /// A store whose tasks carry every shape of child: none, one, many, titles and
        /// usernames equal but for case, labels with and without a creator, absolute and
        /// relative reminders, done and open, favourite and not.
        async fn varied() -> Store {
            let store = Store::in_memory().unwrap();
            let creator = user(90, "maker");
            let labels = [
                label(1, "work", Some(&creator)),
                label(2, "Errands", None),
                // Three titles equal ignoring case, ids out of title order.
                label(9, "urgent", None),
                label(4, "URGENT", Some(&creator)),
                label(6, "Urgent", None),
                label(3, "alpha", None),
                label(5, "Zed", None),
            ];
            let users = [
                user(1, "swasko"),
                user(2, "Bob"),
                user(7, "bob"),
                user(3, "alice"),
                user(4, "carol"),
            ];
            let mut tasks = Vec::new();
            for id in 1..=60_i64 {
                let n = usize::try_from(id).unwrap();
                let mut task = Task {
                    id: TaskId(id),
                    project_id: ProjectId(1 + id % 3),
                    title: format!("task {}", 61 - id),
                    done: id % 4 == 0,
                    is_favorite: id % 5 == 0,
                    priority: id % 6,
                    due_date: if id % 2 == 0 {
                        at(&format!("2026-09-{:02}T10:00:00Z", 1 + id % 28)).into()
                    } else {
                        None.into()
                    },
                    created_by: Some(users[n % users.len()].clone()),
                    ..Task::default()
                };
                // Every subset size from none to all, rotated so orders vary.
                task.labels = (0..n % (labels.len() + 1))
                    .map(|k| labels[(n + k) % labels.len()].clone())
                    .collect();
                task.assignees = (0..n % (users.len() + 1))
                    .map(|k| users[(n * 3 + k) % users.len()].clone())
                    .collect();
                task.reminders = (0..n % 4)
                    .map(|k| {
                        reminder(
                            &format!("2026-10-{:02}T0{k}:00:00Z", 1 + (n * 7 + k) % 28),
                            if k == 2 { -3600 } else { 0 },
                        )
                    })
                    .collect();
                // Two reminders at the same instant, told apart only by their period.
                if id == 7 {
                    task.reminders.push(reminder("2026-10-05T00:00:00Z", 60));
                    task.reminders.push(reminder("2026-10-05T00:00:00Z", -60));
                }
                tasks.push(task);
            }
            store.upsert_tasks(tasks).await.unwrap();
            store
        }

        fn filters() -> Vec<(TaskFilter, TaskSort)> {
            let sorts = [
                TaskSort::default(),
                TaskSort {
                    order: TaskOrder::Title,
                    descending: true,
                },
                TaskSort {
                    order: TaskOrder::Priority,
                    descending: false,
                },
            ];
            let filters = [
                TaskFilter::default(),
                TaskFilter {
                    done: Some(false),
                    limit: Some(10_000),
                    ..TaskFilter::default()
                },
                TaskFilter {
                    label: Some(LabelId(4)),
                    ..TaskFilter::default()
                },
                TaskFilter {
                    project: Some(ProjectId(2)),
                    favorite: Some(false),
                    ..TaskFilter::default()
                },
                TaskFilter {
                    search: Some("task 1".to_string()),
                    limit: Some(5),
                    ..TaskFilter::default()
                },
                TaskFilter {
                    search: Some("nothing matches this".to_string()),
                    ..TaskFilter::default()
                },
            ];
            filters
                .iter()
                .flat_map(|filter| sorts.iter().map(|sort| (filter.clone(), *sort)))
                .collect()
        }

        /// Each of `batched` carries exactly what the per-task reads give for it.
        fn assert_as_per_task(store: &Store, batched: &[Task]) {
            let connection = store.connection.lock().unwrap();
            for task in batched {
                let id = task.id;
                assert_eq!(
                    task.labels,
                    per_task::labels_for(&connection, id),
                    "labels of {id:?}"
                );
                assert_eq!(
                    task.assignees,
                    per_task::assignees_for(&connection, id),
                    "assignees of {id:?}"
                );
                assert_eq!(
                    task.reminders,
                    per_task::reminders_for(&connection, id),
                    "reminders of {id:?}"
                );
            }
        }

        #[tokio::test]
        async fn a_batched_read_assembles_every_task_as_the_per_task_reads_did() {
            let store = varied().await;
            for (filter, sort) in filters() {
                let batched = store.tasks(filter.clone(), sort).await.unwrap();
                assert_as_per_task(&store, &batched);
            }
            // And `Store::task`, which reads its one task's children the same way.
            for id in 1..=60 {
                let one = store.task(TaskId(id)).await.unwrap().unwrap();
                assert_as_per_task(&store, std::slice::from_ref(&one));
            }
            // Not vacuous: the seed reaches the shapes the comparison is for.
            let all = store
                .tasks(TaskFilter::default(), TaskSort::default())
                .await
                .unwrap();
            assert!(all
                .iter()
                .any(|t| t.labels.is_empty() && t.assignees.is_empty()));
            assert!(all.iter().any(|t| t.labels.len() == 7));
            assert!(all.iter().any(|t| t.assignees.len() == 5));
            assert!(all.iter().any(|t| t.reminders.len() == 5));
            let urgent: Vec<_> = all
                .iter()
                .find(|t| t.labels.len() == 7)
                .unwrap()
                .labels
                .iter()
                .filter(|l| l.title.eq_ignore_ascii_case("urgent"))
                .map(|l| l.id.get())
                .collect();
            assert_eq!(urgent.len(), 3, "the case-equal titles all reach one task");
        }

        thread_local! {
            static STATEMENTS: Cell<u64> = const { Cell::new(0) };
        }

        fn counted(event: rusqlite::trace::TraceEvent<'_>) {
            if matches!(event, rusqlite::trace::TraceEvent::Stmt(..)) {
                STATEMENTS.with(|count| count.set(count.get() + 1));
            }
        }

        /// How many statements one open-list read runs over a store of `tasks` tasks,
        /// each carrying two labels, two assignees and two reminders.
        async fn statements_for(tasks: i64) -> u64 {
            let store = Store::in_memory().unwrap();
            let seeded = (1..=tasks)
                .map(|id| Task {
                    labels: vec![label(1 + id % 5, "a", None), label(10 + id % 5, "b", None)],
                    assignees: vec![user(1 + id % 3, "x"), user(10 + id % 3, "y")],
                    reminders: vec![
                        reminder("2026-10-01T00:00:00Z", 0),
                        reminder("2026-10-02T00:00:00Z", 0),
                    ],
                    ..task(id, "t")
                })
                .collect();
            store.upsert_tasks(seeded).await.unwrap();

            let mut connection = store.connection.lock().unwrap();
            // The trace callback fires on the thread that runs the statement, which is
            // this one: the read is called directly rather than through `spawn_blocking`.
            connection
                .trace_v2(
                    rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
                    Some(counted),
                )
                .unwrap();
            STATEMENTS.with(|count| count.set(0));
            let filter = TaskFilter {
                done: Some(false),
                limit: Some(10_000),
                ..TaskFilter::default()
            };
            let read = read_tasks(&connection, &filter, TaskSort::default()).unwrap();
            connection
                .trace_v2(
                    rusqlite::trace::TraceEventCodes::empty(),
                    None::<fn(rusqlite::trace::TraceEvent<'_>)>,
                )
                .unwrap();
            assert_eq!(read.len(), usize::try_from(tasks).unwrap());
            assert!(read
                .iter()
                .all(|t| t.labels.len() == 2 && t.reminders.len() == 2));
            STATEMENTS.with(Cell::get)
        }

        #[tokio::test]
        async fn a_task_list_read_runs_one_statement_per_table_however_many_tasks() {
            // The tasks themselves, then labels, assignees and reminders. Three per task
            // before BUG-28: 3 tasks were 10 statements and 400 were 1,201.
            assert_eq!(statements_for(3).await, 4);
            assert_eq!(statements_for(400).await, 4);
        }
    }
}
