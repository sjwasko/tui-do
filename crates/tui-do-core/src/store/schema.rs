//! The local schema, and the migrations that produce it.
//!
//! Versioned with SQLite's own `user_version` pragma rather than a table of our own:
//! it is already there, it is atomic with the transaction that bumps it, and it cannot
//! drift out of sync with the schema it describes.
//!
//! Migrations are append-only. Editing one that has shipped changes the schema of
//! databases that already ran it and will not run it again, so a mistake is corrected by
//! adding a migration, never by rewriting one.
//!
//! # Dates are NULL here, not year one
//!
//! Vikunja spells "unset" as Go's zero time because its columns are `NOT NULL`. tui-do has
//! no such constraint and no reason to inherit the workaround, so date columns are
//! nullable and unset means `NULL`. The conversion happens at the store boundary, in one
//! place, which is the only way a rule like that survives.

use rusqlite::{Connection, Transaction};

use crate::error::Result;

/// Every migration, in order. The index plus one is the schema version it produces.
const MIGRATIONS: &[&str] = &[
    // v1 -- the initial schema.
    r"
    CREATE TABLE projects (
        id                INTEGER PRIMARY KEY,
        title             TEXT    NOT NULL,
        description       TEXT    NOT NULL DEFAULT '',
        identifier        TEXT    NOT NULL DEFAULT '',
        hex_color         TEXT    NOT NULL DEFAULT '',
        parent_project_id INTEGER NOT NULL DEFAULT 0,
        is_archived       INTEGER NOT NULL DEFAULT 0,
        is_favorite       INTEGER NOT NULL DEFAULT 0,
        position          REAL    NOT NULL DEFAULT 0,
        owner_id          INTEGER NOT NULL DEFAULT 0,
        created           TEXT,
        updated           TEXT,
        synced_at         TEXT    NOT NULL
    );

    CREATE TABLE users (
        id       INTEGER PRIMARY KEY,
        username TEXT NOT NULL,
        name     TEXT NOT NULL DEFAULT '',
        email    TEXT NOT NULL DEFAULT ''
    );

    CREATE TABLE labels (
        id          INTEGER PRIMARY KEY,
        title       TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        hex_color   TEXT NOT NULL DEFAULT '',
        created     TEXT,
        updated     TEXT,
        synced_at   TEXT NOT NULL
    );

    CREATE TABLE tasks (
        id            INTEGER PRIMARY KEY,
        project_id    INTEGER NOT NULL,
        title         TEXT    NOT NULL,
        description   TEXT    NOT NULL DEFAULT '',
        done          INTEGER NOT NULL DEFAULT 0,
        done_at       TEXT,
        priority      INTEGER NOT NULL DEFAULT 0,
        percent_done  REAL    NOT NULL DEFAULT 0,
        position      REAL    NOT NULL DEFAULT 0,
        due_date      TEXT,
        start_date    TEXT,
        end_date      TEXT,
        repeat_after  INTEGER NOT NULL DEFAULT 0,
        repeat_mode   INTEGER NOT NULL DEFAULT 0,
        hex_color     TEXT    NOT NULL DEFAULT '',
        identifier    TEXT    NOT NULL DEFAULT '',
        task_index    INTEGER NOT NULL DEFAULT 0,
        is_favorite   INTEGER NOT NULL DEFAULT 0,
        bucket_id     INTEGER NOT NULL DEFAULT 0,
        comment_count INTEGER NOT NULL DEFAULT 0,
        created_by_id INTEGER NOT NULL DEFAULT 0,
        created       TEXT,
        updated       TEXT,
        synced_at     TEXT    NOT NULL
    );

    CREATE INDEX tasks_by_project ON tasks (project_id);
    CREATE INDEX tasks_by_done    ON tasks (done);
    CREATE INDEX tasks_by_due     ON tasks (due_date) WHERE due_date IS NOT NULL;

    CREATE TABLE task_labels (
        task_id  INTEGER NOT NULL REFERENCES tasks (id)  ON DELETE CASCADE,
        label_id INTEGER NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
        PRIMARY KEY (task_id, label_id)
    );
    CREATE INDEX task_labels_by_label ON task_labels (label_id);

    CREATE TABLE task_assignees (
        task_id INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
        user_id INTEGER NOT NULL,
        PRIMARY KEY (task_id, user_id)
    );

    CREATE TABLE saved_filters (
        id          INTEGER PRIMARY KEY,
        title       TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        filter      TEXT NOT NULL DEFAULT '',
        created     TEXT,
        updated     TEXT,
        synced_at   TEXT NOT NULL
    );

    -- Whatever the sync engine needs to remember between runs: last full pull, the
    -- server's page cap, the authenticated user. One row per key rather than a wide
    -- table nobody remembers the columns of.
    CREATE TABLE sync_state (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    -- Pending local mutations, in the order they were made. Rule 5: `update` writes here
    -- and to the table above it in the same transaction, so the UI never waits for the
    -- network and never shows a write that was not recorded.
    CREATE TABLE outbox (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        created    TEXT    NOT NULL,
        kind       TEXT    NOT NULL,
        payload    TEXT    NOT NULL,
        attempts   INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        -- What the entry acts on, so a rollback can find the row to restore.
        subject_id INTEGER
    );
    CREATE INDEX outbox_by_subject ON outbox (subject_id);
    ",
    // v2 -- labels remember who owns them, and projects remember their views.
    //
    // Vikunja only lets a label's creator edit it, so without the owner the UI cannot
    // tell which labels it may offer to rename. Views are how the web frontend actually
    // loads tasks (`/projects/{id}/views/{view}/tasks`) and where bucket positions live,
    // so Phase 6 needs their ids stored rather than re-fetched on every keystroke.
    r"
    ALTER TABLE labels ADD COLUMN created_by_id INTEGER NOT NULL DEFAULT 0;

    CREATE TABLE project_views (
        id         INTEGER PRIMARY KEY,
        project_id INTEGER NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
        title      TEXT    NOT NULL,
        -- The wire spelling ('list', 'kanban', ...), so an unknown kind from a future
        -- server survives a round trip instead of being flattened to a number.
        view_kind  TEXT    NOT NULL,
        position   REAL    NOT NULL DEFAULT 0
    );
    CREATE INDEX project_views_by_project ON project_views (project_id);
    ",
    // v3 -- reminders.
    //
    // Not a display feature: `POST /tasks/{id}` replaces a task's reminders from the
    // request body, the way it replaces assignees, and `Task` serialises every field. So
    // a task that had been through a store with nowhere to keep reminders went back to
    // the server carrying `"reminders": []` and lost them. Measured on dev 2026-08-29 by
    // `a_task_update_does_not_wipe_reminders_it_was_not_told_about`: one reminder in,
    // zero out. Keeping them here is what makes read-mutate-write safe.
    //
    // No id column: Vikunja identifies a reminder by its contents, and the pair is what
    // travels on the wire.
    r"
    CREATE TABLE task_reminders (
        task_id         INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
        reminder        TEXT,
        relative_period INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (task_id, reminder, relative_period)
    );
    ",
    // v4 -- when a deferred outbox entry may be tried again.
    //
    // `attempts` was counted from the beginning and never read: a failing entry was
    // retried at full speed on every pass, and the only thing keeping that from being a
    // hot loop was the thirty-second floor on the sync timer. NULL means "as soon as
    // possible", which is what every existing row means.
    r"
    ALTER TABLE outbox ADD COLUMN next_attempt_at TEXT;
    ",
    // v5 -- what kind of thing an outbox entry acts on.
    //
    // `subject_id` is untyped, and provisional ids count down from -1 for each kind, so
    // a locally created task and a locally created label would both be -1 in the same
    // column. Two queries break silently on that: `retain_tasks` spares a task whose id
    // matches a queued label's subject, and `settle_create` rewrites a label entry's
    // payload with a `TaskId`. Every existing row is a task -- there was nothing else to
    // queue -- so the default backfills them correctly.
    r"
    ALTER TABLE outbox ADD COLUMN subject_kind TEXT NOT NULL DEFAULT 'task';
    CREATE INDEX outbox_by_subject_kind ON outbox (subject_kind, subject_id);
    ",
];

/// The schema version this build expects.
#[must_use]
pub fn target_version() -> i64 {
    // A migration list longer than i64 is not a situation worth handling.
    i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX)
}

/// Bring `connection` up to [`target_version`].
///
/// Each migration runs in its own transaction together with the version bump, so an
/// interrupted upgrade leaves the database at a version that matches its contents.
///
/// # Errors
/// [`crate::CoreError::Store`] if a migration fails, or if the database is newer than
/// this build understands — which is a downgrade, and silently continuing would corrupt
/// data a later version wrote.
pub fn migrate(connection: &mut Connection) -> Result<()> {
    let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let target = target_version();

    if current > target {
        return Err(crate::CoreError::Config {
            path: "<store>".to_string(),
            reason: format!(
                "this database is at schema version {current}, newer than the {target} \
                 this build of tui-do understands. Use a newer tui-do, or move the file \
                 aside and let it re-sync."
            ),
        });
    }

    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = i64::try_from(index).unwrap_or(i64::MAX) + 1;
        if version <= current {
            continue;
        }
        let transaction = connection.transaction()?;
        apply(&transaction, sql, version)?;
        transaction.commit()?;
        tracing::debug!(version, "applied store migration");
    }

    Ok(())
}

/// Run one migration and record the version it produced.
fn apply(transaction: &Transaction<'_>, sql: &str, version: i64) -> Result<()> {
    transaction.execute_batch(sql)?;
    // `PRAGMA user_version` does not accept a bound parameter, and `version` is a
    // number this module produced from a slice index -- not user input.
    transaction.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn migrated() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate(&mut connection).unwrap();
        connection
    }

    fn table_names(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        names
    }

    #[test]
    fn migrating_creates_every_table_the_plan_calls_for() {
        let connection = migrated();
        let tables = table_names(&connection);
        for expected in [
            "tasks",
            "projects",
            "labels",
            "saved_filters",
            "sync_state",
            "outbox",
            "task_labels",
            "task_assignees",
            "users",
            "project_views",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}; have {tables:?}"
            );
        }
    }

    #[test]
    fn the_version_is_recorded_so_it_does_not_run_twice() {
        let mut connection = migrated();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, target_version());

        // Running again is a no-op, not a "table already exists" error.
        migrate(&mut connection).unwrap();
        assert_eq!(
            table_names(&connection).len(),
            table_names(&migrated()).len()
        );
    }

    #[test]
    fn an_existing_database_is_upgraded_in_place_rather_than_rebuilt() {
        // The append-only rule only pays off if a partial database actually migrates,
        // so run v1 alone, put a row in it, and check the row is still there after v2.
        let mut connection = Connection::open_in_memory().unwrap();
        let transaction = connection.transaction().unwrap();
        apply(&transaction, MIGRATIONS[0], 1).unwrap();
        transaction.commit().unwrap();
        connection
            .execute(
                "INSERT INTO labels (id, title, synced_at) VALUES (1, 'urgent', 'now')",
                [],
            )
            .unwrap();

        migrate(&mut connection).unwrap();

        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, target_version());
        let (title, owner): (String, i64) = connection
            .query_row(
                "SELECT title, created_by_id FROM labels WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "urgent", "the upgrade lost a row");
        assert_eq!(owner, 0, "the new column should default rather than fail");
        assert!(table_names(&connection)
            .iter()
            .any(|t| t == "project_views"));
    }

    #[test]
    fn a_database_from_a_newer_tui_do_is_refused_rather_than_used() {
        // Silently continuing would let this build write rows a later schema expects to
        // look different. Refusing is the only safe answer, and it says what to do.
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(&format!("PRAGMA user_version = {}", target_version() + 5))
            .unwrap();
        let err = migrate(&mut connection).expect_err("a downgrade should be refused");
        assert!(err.to_string().contains("newer"), "unhelpful: {err}");
    }

    #[test]
    fn unset_dates_are_null_rather_than_go_zero_time() {
        // The store does not inherit Vikunja's workaround for NOT NULL columns.
        let connection = migrated();
        connection
            .execute(
                "INSERT INTO tasks (id, project_id, title, synced_at) VALUES (1, 1, 't', 'now')",
                [],
            )
            .unwrap();
        let due: Option<String> = connection
            .query_row("SELECT due_date FROM tasks WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(due, None);
    }

    #[test]
    fn deleting_a_task_takes_its_links_with_it() {
        let connection = migrated();
        connection
            .execute_batch("PRAGMA foreign_keys = ON")
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO tasks (id, project_id, title, synced_at) VALUES (1, 1, 't', 'now');
                 INSERT INTO labels (id, title, synced_at) VALUES (7, 'urgent', 'now');
                 INSERT INTO task_labels (task_id, label_id) VALUES (1, 7);
                 INSERT INTO task_assignees (task_id, user_id) VALUES (1, 3);
                 DELETE FROM tasks WHERE id = 1;",
            )
            .unwrap();
        let links: i64 = connection
            .query_row("SELECT count(*) FROM task_labels", [], |row| row.get(0))
            .unwrap();
        let assignees: i64 = connection
            .query_row("SELECT count(*) FROM task_assignees", [], |row| row.get(0))
            .unwrap();
        assert_eq!(links, 0);
        assert_eq!(assignees, 0);
    }
}
