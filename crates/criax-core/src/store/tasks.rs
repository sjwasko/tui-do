//! Reading and writing tasks.
//!
//! The conversion between Vikunja's wire shape and the local one lives here and only
//! here: unset dates become `NULL` on the way in and Go's zero time on the way out, via
//! the API crate's helpers. Every rule that has to hold everywhere is cheapest to enforce
//! in one function.

use chrono::{DateTime, Utc};
use criax_api::models::{Label, ProjectId, Task, TaskId, User, UserId};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use super::Store;
use crate::error::Result;

/// Which tasks to return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFilter {
    /// Only tasks in this project.
    pub project: Option<ProjectId>,

    /// Only done, or only not-done. `None` returns both.
    pub done: Option<bool>,

    /// Only tasks carrying this label.
    pub label: Option<criax_api::models::LabelId>,

    /// Case-insensitive substring of the title.
    pub search: Option<String>,

    /// At most this many rows.
    pub limit: Option<u32>,
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
    /// By when the task was created.
    Created,
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
                    "SELECT * FROM tasks WHERE id = ?1",
                    params![id.get()],
                    row_to_task,
                )
                .optional()?
            else {
                return Ok(None);
            };
            task.labels = labels_for(connection, id)?;
            task.assignees = assignees_for(connection, id)?;
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
            let mut sql = String::from("SELECT tasks.* FROM tasks");
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

    /// Remove tasks that the server no longer has.
    ///
    /// Takes the ids that *do* exist, because that is what a full pull produces. Only
    /// tasks in `projects` are considered, so a pull of one project cannot delete
    /// another project's tasks.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn retain_tasks(&self, projects: Vec<ProjectId>, keep: Vec<TaskId>) -> Result<usize> {
        self.write(move |tx| {
            let keep_ids: Vec<i64> = keep.iter().map(|id| id.get()).collect();
            let project_ids: Vec<i64> = projects.iter().map(|id| id.get()).collect();
            let mut removed = 0;

            // Chunked so a very large keep-list cannot exceed SQLite's parameter limit.
            tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS keep_ids (id INTEGER PRIMARY KEY)")?;
            tx.execute("DELETE FROM keep_ids", [])?;
            {
                let mut insert = tx.prepare("INSERT OR IGNORE INTO keep_ids (id) VALUES (?1)")?;
                for id in &keep_ids {
                    insert.execute(params![id])?;
                }
            }

            if project_ids.is_empty() {
                removed += tx.execute(
                    "DELETE FROM tasks WHERE id NOT IN (SELECT id FROM keep_ids)",
                    [],
                )?;
            } else {
                let mut statement = tx.prepare(
                    "DELETE FROM tasks
                      WHERE project_id = ?1 AND id NOT IN (SELECT id FROM keep_ids)",
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
fn upsert_task(tx: &Transaction<'_>, task: &Task, now: DateTime<Utc>) -> Result<()> {
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
        upsert_label_row(tx, label, now)?;
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

    if let Some(creator) = &task.created_by {
        upsert_user(tx, creator)?;
    }

    Ok(())
}

/// Write a label seen on a task, so a label picker works before labels are pulled.
fn upsert_label_row(tx: &Transaction<'_>, label: &Label, now: DateTime<Utc>) -> Result<()> {
    tx.execute(
        "INSERT INTO labels (id, title, description, hex_color, created, updated, synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (id) DO UPDATE SET
            title = excluded.title, description = excluded.description,
            hex_color = excluded.hex_color, updated = excluded.updated,
            synced_at = excluded.synced_at",
        params![
            label.id.get(),
            label.title,
            label.description,
            label.hex_color,
            stamp(label.created.get()),
            stamp(label.updated.get()),
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Write a user seen on a task.
fn upsert_user(tx: &Transaction<'_>, user: &User) -> Result<()> {
    tx.execute(
        "INSERT INTO users (id, username, name, email) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (id) DO UPDATE SET
            username = excluded.username, name = excluded.name,
            -- Only the authenticated user's email is ever populated; an empty one from
            -- an embedded copy must not erase the real one.
            email = CASE WHEN excluded.email = '' THEN users.email ELSE excluded.email END",
        params![user.id.get(), user.username, user.name, user.email],
    )?;
    Ok(())
}

/// The labels attached to a task.
fn labels_for(connection: &Connection, task: TaskId) -> Result<Vec<Label>> {
    let mut statement = connection.prepare(
        "SELECT labels.id, labels.title, labels.description, labels.hex_color
           FROM labels JOIN task_labels ON task_labels.label_id = labels.id
          WHERE task_labels.task_id = ?1
          ORDER BY labels.title COLLATE NOCASE",
    )?;
    let rows = statement.query_map(params![task.get()], |row| {
        Ok(Label {
            id: row.get::<_, i64>(0)?.into(),
            title: row.get(1)?,
            description: row.get(2)?,
            hex_color: row.get(3)?,
            ..Label::default()
        })
    })?;
    let mut labels = Vec::new();
    for label in rows {
        labels.push(label?);
    }
    Ok(labels)
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
        created_by: match row.get::<_, i64>("created_by_id")? {
            0 => None,
            id => Some(User {
                id: UserId(id),
                ..User::default()
            }),
        },
        ..Task::default()
    })
}

/// Read a nullable timestamp column.
fn instant(row: &Row<'_>, column: &str) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(column)?;
    Ok(raw
        .as_deref()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|dt| dt.with_timezone(&Utc)))
}

/// Render a timestamp for storage. `None` is `NULL`, not year one.
fn stamp(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.to_rfc3339())
}

/// Escape the wildcards in a `LIKE` pattern.
///
/// Without this, searching for `100%` matches everything, and `_` matches any character.
fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use criax_api::models::LabelId;

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
