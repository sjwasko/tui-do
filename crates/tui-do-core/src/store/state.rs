//! `sync_state`: the few facts that have to survive a restart.
//!
//! A key-value table rather than a one-row wide one, because the set of things worth
//! remembering grows and a schema migration per fact is a poor trade. Values are text;
//! callers parse what they wrote.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::Store;
use crate::error::Result;

/// The watermark: every server-side change up to this instant has been seen.
///
/// Advanced by both kinds of pull, and set to the moment a pull *started* rather than
/// the moment it finished — a task edited while seventy-eight pages were being fetched
/// may or may not have made it into the pages, and the next incremental pull has to ask
/// for it again either way.
pub const LAST_PULL: &str = "last_pull";

/// When a pull last reconciled deletions, which only a full one does.
///
/// Separate from [`LAST_PULL`] because they stopped being the same fact the moment an
/// incremental pull existed: it can say "everything up to here has been seen" without
/// being able to say "and nothing else is gone", since a listing filtered to what
/// changed never mentions what was deleted.
pub const LAST_RECONCILE: &str = "last_reconcile";

/// The server's `max_items_per_page`, read from `/info`.
///
/// Stored so an offline start still knows the page size the last online session saw,
/// rather than guessing one and silently dropping everything past it.
pub const PAGE_CAP: &str = "page_cap";

/// The id of the authenticated user.
pub const CURRENT_USER: &str = "current_user_id";

/// The project the interface was last showing, so a restart lands where the user left.
///
/// The UI shares this table rather than owning one of its own: it is a generic key-value
/// store, and a schema migration to hold a single string would cost more than the `ui.`
/// prefix that keeps the two namespaces apart.
pub const LAST_PROJECT: &str = "ui.last_project";

/// Read a stored value.
pub(super) fn read_state(connection: &Connection, key: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT value FROM sync_state WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// Write a value, replacing any previous one.
pub(super) fn write_state(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

impl Store {
    /// Read a sync-state value.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn state(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_string();
        self.read(move |connection| read_state(connection, &key))
            .await
    }

    /// Write a sync-state value.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn set_state(&self, key: &str, value: impl Into<String>) -> Result<()> {
        let key = key.to_string();
        let value = value.into();
        self.write(move |tx| write_state(tx, &key, &value)).await
    }

    /// The watermark: every server-side change up to here has been seen.
    ///
    /// `None` means no pull has ever completed, which is what makes an incremental pull
    /// fall back to a full one rather than asking for "everything since the epoch".
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn last_pull(&self) -> Result<Option<DateTime<Utc>>> {
        self.timestamp(LAST_PULL).await
    }

    /// When a full pull last reconciled deletions, if one ever has.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn last_reconcile(&self) -> Result<Option<DateTime<Utc>>> {
        self.timestamp(LAST_RECONCILE).await
    }

    /// Read a state value that was written as RFC 3339.
    ///
    /// A value that will not parse reads as absent rather than as an error: the caller's
    /// answer to "no watermark" is to fetch everything, which is the safe thing to do
    /// about a watermark nobody can read.
    async fn timestamp(&self, key: &str) -> Result<Option<DateTime<Utc>>> {
        Ok(self.state(key).await?.and_then(|raw| {
            DateTime::parse_from_rfc3339(&raw)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }))
    }

    /// The server's page cap as last seen, if a session has ever read `/info`.
    ///
    /// # Errors
    /// [`crate::CoreError::Store`] on any SQL failure.
    pub async fn page_cap(&self) -> Result<Option<u32>> {
        Ok(self
            .state(PAGE_CAP)
            .await?
            .and_then(|raw| raw.parse::<u32>().ok()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_value_round_trips_and_replaces_rather_than_duplicating() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.state(PAGE_CAP).await.unwrap(), None);

        store.set_state(PAGE_CAP, "50").await.unwrap();
        store.set_state(PAGE_CAP, "100").await.unwrap();

        assert_eq!(store.page_cap().await.unwrap(), Some(100));
        let rows: i64 = store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM sync_state", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn an_unparseable_value_reads_as_absent_rather_than_failing() {
        // A hand-edited database, or a value written by a build that spelled it
        // differently, must not stop tui-do from starting.
        let store = Store::in_memory().unwrap();
        store.set_state(PAGE_CAP, "lots").await.unwrap();
        store.set_state(LAST_PULL, "yesterday").await.unwrap();

        assert_eq!(store.page_cap().await.unwrap(), None);
        assert_eq!(store.last_pull().await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_last_pull_time_round_trips() {
        let store = Store::in_memory().unwrap();
        let when = Utc::now();
        store.set_state(LAST_PULL, when.to_rfc3339()).await.unwrap();
        let read = store.last_pull().await.unwrap().expect("a timestamp");
        assert_eq!(read.timestamp(), when.timestamp());
    }
}
