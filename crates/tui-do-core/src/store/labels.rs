//! Reading and writing labels.
//!
//! Labels arrive by two routes: the dedicated `GET /labels` listing, and embedded in
//! every task that carries them. Both land here, through [`upsert_label`], so a label
//! picker works from whatever has been seen so far rather than only after a labels pull.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use tui_do_api::models::{Label, LabelId};

use super::sql::{escape_like, instant, joined_user, keep_ids, stamp, upsert_user};
use super::Store;
use crate::error::Result;

/// The columns a [`Label`] is built from, with its creator joined in.
const SELECT: &str = "SELECT labels.*,
                             creator.username AS creator_username,
                             creator.name     AS creator_name,
                             creator.email    AS creator_email
                        FROM labels
                        LEFT JOIN users AS creator ON creator.id = labels.created_by_id";

/// Which labels to return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelFilter {
    /// Case-insensitive substring of the title.
    pub search: Option<String>,

    /// At most this many rows.
    pub limit: Option<u32>,
}

/// What to order labels by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LabelOrder {
    /// By title, case-insensitively. What a picker wants.
    #[default]
    Title,
    /// By when the label was created.
    Created,
    /// By id, which is creation order and always unambiguous.
    Id,
}

/// An ordering, with a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LabelSort {
    /// The column.
    pub order: LabelOrder,
    /// Whether to reverse it.
    pub descending: bool,
}

impl LabelSort {
    /// The `ORDER BY` fragment. Every value here is a literal in this file.
    fn sql(self) -> &'static str {
        match (self.order, self.descending) {
            (LabelOrder::Title, false) => "title COLLATE NOCASE ASC, id ASC",
            (LabelOrder::Title, true) => "title COLLATE NOCASE DESC, id ASC",
            // Labels created before the store saw them have no timestamp; sort those
            // last for the same reason dateless tasks sort last.
            (LabelOrder::Created, false) => "created IS NULL, created ASC, id ASC",
            (LabelOrder::Created, true) => "created IS NULL, created DESC, id ASC",
            (LabelOrder::Id, false) => "id ASC",
            (LabelOrder::Id, true) => "id DESC",
        }
    }
}

impl Store {
    /// Replace the stored copy of every label in `labels`.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure. The whole batch is one
    /// transaction, so a partial page never lands.
    pub async fn upsert_labels(&self, labels: Vec<Label>) -> Result<usize> {
        let now = Utc::now();
        self.write(move |tx| {
            for label in &labels {
                upsert_label(tx, label, now)?;
            }
            Ok(labels.len())
        })
        .await
    }

    /// Read one label.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn label(&self, id: LabelId) -> Result<Option<Label>> {
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    &format!("{SELECT} WHERE labels.id = ?1"),
                    params![id.get()],
                    row_to_label,
                )
                .optional()?)
        })
        .await
    }

    /// Read the label with this exact title, ignoring case.
    ///
    /// Quick-add writes `*urgent`, and something has to turn that into an id. Vikunja
    /// allows two labels differing only in case, so ties are broken by id -- the older
    /// one wins, which is the one a user typing a familiar name means.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn label_named(&self, title: impl Into<String>) -> Result<Option<Label>> {
        let title = title.into();
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "{SELECT} WHERE labels.title = ?1 COLLATE NOCASE
                         ORDER BY labels.id ASC LIMIT 1"
                    ),
                    params![title],
                    row_to_label,
                )
                .optional()?)
        })
        .await
    }

    /// Read labels matching `filter`, ordered by `sort`.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn labels(&self, filter: LabelFilter, sort: LabelSort) -> Result<Vec<Label>> {
        self.read(move |connection| {
            let mut sql = String::from(SELECT);
            let mut values: Vec<rusqlite::types::Value> = Vec::new();

            if let Some(search) = &filter.search {
                sql.push_str(" WHERE labels.title LIKE ?1 ESCAPE '\\' COLLATE NOCASE");
                values.push(format!("%{}%", escape_like(search)).into());
            }
            sql.push_str(" ORDER BY ");
            sql.push_str(sort.sql());
            if let Some(limit) = filter.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }

            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(values), row_to_label)?;
            let mut labels = Vec::new();
            for label in rows {
                labels.push(label?);
            }
            Ok(labels)
        })
        .await
    }

    /// How many labels are stored.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn label_count(&self) -> Result<i64> {
        self.read(|connection| {
            Ok(connection.query_row("SELECT count(*) FROM labels", [], |row| row.get(0))?)
        })
        .await
    }

    /// Remove labels the server no longer has, given the ids it does.
    ///
    /// The cascade takes their `task_labels` rows with them, so a label deleted on
    /// another device disappears from every task here too.
    ///
    /// Call this only after a *complete* labels pull. `GET /labels` answers with
    /// "all labels which are either created by the user or associated with a task the
    /// user has at least read-access to", so it is the right listing to retain against.
    /// A filtered or searched one is not.
    ///
    /// A label still attached to a stored task is kept regardless. It demonstrably
    /// exists -- the task carries it -- and deleting it would cascade the attachment
    /// away. That matters most for a task with unsent local changes, which the pull
    /// skips: nothing would put its labels back. The link disappearing is the task
    /// pull's job to notice, not this one's.
    ///
    /// A label queued to be created is kept regardless too, mirroring `retain_tasks`'s
    /// outbox exemption. `pull_lists` runs on every pull, including startup, before a
    /// `CreateLabel` queued moments earlier has ever been sent -- so without this, the
    /// listing (which by definition does not name a label the server has not seen) erased
    /// the local row while the outbox entry survived. That leaves the server holding a
    /// label the local store has no row for, not yet a duplicate -- a duplicate only
    /// follows if the user, seeing it vanish, retypes it.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn retain_labels(&self, keep: Vec<LabelId>) -> Result<usize> {
        self.write(move |tx| {
            keep_ids(tx, keep.iter().map(|id| id.get()))?;
            Ok(tx.execute(
                "DELETE FROM labels
                  WHERE id NOT IN (SELECT id FROM keep_ids)
                    AND id NOT IN (SELECT label_id FROM task_labels)
                    AND id NOT IN (SELECT subject_id FROM outbox
                                    WHERE subject_id IS NOT NULL AND subject_kind = 'label')",
                [],
            )?)
        })
        .await
    }
}

/// Write one label.
///
/// Shared with the task upsert, which sees labels embedded in tasks.
pub(super) fn upsert_label(tx: &Transaction<'_>, label: &Label, now: DateTime<Utc>) -> Result<()> {
    if let Some(creator) = &label.created_by {
        upsert_user(tx, creator)?;
    }
    tx.execute(
        "INSERT INTO labels (
            id, title, description, hex_color, created_by_id, created, updated, synced_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (id) DO UPDATE SET
            title = excluded.title, description = excluded.description,
            hex_color = excluded.hex_color,
            -- An embedded copy without a creator must not erase a known one; Vikunja
            -- only lets a label's owner edit it, so this decides what the UI offers.
            created_by_id = CASE WHEN excluded.created_by_id = 0
                                 THEN labels.created_by_id ELSE excluded.created_by_id END,
            created = excluded.created, updated = excluded.updated,
            synced_at = excluded.synced_at",
        params![
            label.id.get(),
            label.title,
            label.description,
            label.hex_color,
            label.created_by.as_ref().map_or(0, |u| u.id.get()),
            stamp(label.created.get()),
            stamp(label.updated.get()),
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// The labels attached to a task.
pub(super) fn labels_for(
    connection: &Connection,
    task: tui_do_api::models::TaskId,
) -> Result<Vec<Label>> {
    let mut statement = connection.prepare(&format!(
        "{SELECT} JOIN task_labels ON task_labels.label_id = labels.id
          WHERE task_labels.task_id = ?1
          ORDER BY labels.title COLLATE NOCASE"
    ))?;
    let rows = statement.query_map(params![task.get()], row_to_label)?;
    let mut labels = Vec::new();
    for label in rows {
        labels.push(label?);
    }
    Ok(labels)
}

/// Build a label from a row produced by [`SELECT`].
fn row_to_label(row: &Row<'_>) -> rusqlite::Result<Label> {
    Ok(Label {
        id: row.get::<_, i64>("id")?.into(),
        title: row.get("title")?,
        description: row.get("description")?,
        hex_color: row.get("hex_color")?,
        created_by: joined_user(row, "created_by_id", "creator")?,
        created: instant(row, "created")?.into(),
        updated: instant(row, "updated")?.into(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::store::Mutation;
    use tui_do_api::models::{ProjectId, Task, TaskId, User, UserId};

    fn label(id: i64, title: &str) -> Label {
        Label {
            id: LabelId(id),
            title: title.to_string(),
            ..Label::default()
        }
    }

    #[tokio::test]
    async fn a_label_round_trips_through_the_store() {
        let store = Store::in_memory().unwrap();
        let mut original = label(3, "urgent");
        original.description = "drop everything".into();
        original.hex_color = "e8384f".into();
        original.created_by = Some(User {
            id: UserId(1),
            username: "swasko".into(),
            ..User::default()
        });

        store.upsert_labels(vec![original.clone()]).await.unwrap();
        let read = store.label(LabelId(3)).await.unwrap().expect("the label");

        assert_eq!(read.title, "urgent");
        assert_eq!(read.description, "drop everything");
        assert_eq!(read.hex_color, "e8384f");
        assert_eq!(read.created_by.unwrap().username, "swasko");
    }

    #[tokio::test]
    async fn a_missing_label_is_none_rather_than_an_error() {
        let store = Store::in_memory().unwrap();
        assert!(store.label(LabelId(404)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upserting_updates_rather_than_duplicating() {
        let store = Store::in_memory().unwrap();
        store.upsert_labels(vec![label(1, "first")]).await.unwrap();
        store.upsert_labels(vec![label(1, "second")]).await.unwrap();

        assert_eq!(store.label_count().await.unwrap(), 1);
        assert_eq!(
            store.label(LabelId(1)).await.unwrap().unwrap().title,
            "second"
        );
    }

    #[tokio::test]
    async fn an_embedded_label_does_not_erase_its_known_owner() {
        // `GET /labels` says who created a label; the copy embedded in a task does not
        // always. Losing it would make the UI offer to edit a label the server will
        // refuse to let this user change.
        let store = Store::in_memory().unwrap();
        let mut owned = label(1, "urgent");
        owned.created_by = Some(User {
            id: UserId(7),
            username: "swasko".into(),
            ..User::default()
        });
        store.upsert_labels(vec![owned]).await.unwrap();

        // The same label, as a task's payload would carry it.
        store.upsert_labels(vec![label(1, "urgent")]).await.unwrap();

        let read = store.label(LabelId(1)).await.unwrap().unwrap();
        assert_eq!(read.created_by.expect("the owner survived").id, UserId(7));
    }

    #[tokio::test]
    async fn a_label_seen_only_on_a_task_is_still_pickable() {
        // The picker must work before any labels pull has happened.
        let store = Store::in_memory().unwrap();
        let task = Task {
            id: TaskId(1),
            project_id: ProjectId(1),
            title: "tagged".into(),
            labels: vec![label(9, "seen-on-a-task")],
            ..Task::default()
        };
        store.upsert_tasks(vec![task]).await.unwrap();

        let all = store
            .labels(LabelFilter::default(), LabelSort::default())
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "seen-on-a-task");
    }

    #[tokio::test]
    async fn lookup_by_name_ignores_case_and_prefers_the_older_label() {
        // Quick-add's `*urgent` has to resolve to an id, and Vikunja allows two labels
        // whose titles differ only in case.
        let store = Store::in_memory().unwrap();
        store
            .upsert_labels(vec![
                label(2, "Urgent"),
                label(5, "URGENT"),
                label(9, "later"),
            ])
            .await
            .unwrap();

        let found = store.label_named("urgent").await.unwrap().expect("a match");
        assert_eq!(found.id, LabelId(2));
        assert!(store.label_named("nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn searching_is_case_insensitive_and_treats_wildcards_literally() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_labels(vec![
                label(1, "Deep Work"),
                label(2, "100% done"),
                label(3, "other"),
            ])
            .await
            .unwrap();

        let found = store
            .labels(
                LabelFilter {
                    search: Some("deep".into()),
                    ..LabelFilter::default()
                },
                LabelSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(found.len(), 1);

        let literal = store
            .labels(
                LabelFilter {
                    search: Some("100%".into()),
                    ..LabelFilter::default()
                },
                LabelSort::default(),
            )
            .await
            .unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].id, LabelId(2));
    }

    #[tokio::test]
    async fn labels_sort_by_title_by_default_and_ties_break_on_id() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_labels(vec![label(3, "beta"), label(1, "Alpha"), label(2, "alpha")])
            .await
            .unwrap();
        let ids: Vec<i64> = store
            .labels(LabelFilter::default(), LabelSort::default())
            .await
            .unwrap()
            .iter()
            .map(|l| l.id.get())
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn retaining_keeps_a_label_a_stored_task_still_carries() {
        // A label on a task exists, whatever `/labels` chose to list. Deleting it would
        // cascade the attachment away -- and for a task the pull skipped because it has
        // unsent changes, nothing would ever put it back.
        let store = Store::in_memory().unwrap();
        let task = Task {
            id: TaskId(1),
            project_id: ProjectId(1),
            title: "tagged".into(),
            labels: vec![label(9, "seen only on a task")],
            ..Task::default()
        };
        store.upsert_tasks(vec![task]).await.unwrap();

        // A labels pull that did not mention label 9 at all.
        let removed = store.retain_labels(vec![LabelId(1)]).await.unwrap();

        assert_eq!(removed, 0);
        assert!(store.label(LabelId(9)).await.unwrap().is_some());
        assert_eq!(
            store.task(TaskId(1)).await.unwrap().unwrap().labels.len(),
            1,
            "the task lost a label to a labels pull"
        );
    }

    #[tokio::test]
    async fn retaining_removes_a_label_nothing_refers_to_any_more() {
        // The ordinary case: a label deleted on another device, on no stored task. The
        // cascade is asserted through a task that had it and no longer does, because
        // that is the sequence a pull produces -- the task pass drops the link, the
        // labels pass then collects the label.
        let store = Store::in_memory().unwrap();
        let mut task = Task {
            id: TaskId(1),
            project_id: ProjectId(1),
            title: "tagged".into(),
            labels: vec![label(1, "kept"), label(2, "deleted server-side")],
            ..Task::default()
        };
        store.upsert_tasks(vec![task.clone()]).await.unwrap();

        // The server's next answer for this task no longer carries label 2.
        task.labels = vec![label(1, "kept")];
        store.upsert_tasks(vec![task]).await.unwrap();

        let removed = store.retain_labels(vec![LabelId(1)]).await.unwrap();
        assert_eq!(removed, 1);
        assert!(store.label(LabelId(2)).await.unwrap().is_none());

        let read = store.task(TaskId(1)).await.unwrap().unwrap();
        assert_eq!(read.labels.len(), 1);
        assert_eq!(read.labels[0].title, "kept");

        let links: i64 = store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM task_labels", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(links, 1, "the cascade left a dangling link");
    }

    #[tokio::test]
    async fn a_provisional_label_survives_a_pull_that_did_not_mention_it() {
        // `retain_tasks` protects a locally created row with an outbox exemption;
        // `retain_labels` had none. A label created offline and not yet attached to any
        // task was erased by the very next `pull_lists` -- which runs on every pull,
        // including startup, before the create had ever been sent -- while the outbox
        // entry survived. The server ends up with a label the local store has no row for;
        // a duplicate only follows if the user, seeing it vanish, retypes it.
        let store = Store::in_memory().unwrap();
        store
            .queue(Mutation::CreateLabel {
                label: Box::new(Label {
                    title: "not yet on the server".into(),
                    ..Label::default()
                }),
            })
            .await
            .unwrap();

        // A pull whose listing does not name the provisional label at all.
        let removed = store.retain_labels(vec![LabelId(1)]).await.unwrap();

        assert_eq!(removed, 0, "the provisional label should have been spared");
        assert!(
            store.label(LabelId(-1)).await.unwrap().is_some(),
            "a label queued to be created was deleted before it could be sent"
        );
    }
}
