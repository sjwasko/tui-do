//! The local SQLite store: the UI's source of truth.
//!
//! Everything the interface renders comes from here, and nothing renders from a network
//! response directly. That is what makes tui-do start instantly, work offline, and never
//! block a frame on a server — the whole point of the rewrite.
//!
//! # Nothing here blocks the caller
//!
//! `rusqlite` is synchronous, and a synchronous call on the async runtime's worker thread
//! stalls every other task on it. Every method here is `async` and runs its work inside
//! [`tokio::task::spawn_blocking`], so the blocking is real but confined. Making the
//! methods async rather than documenting "call this from spawn_blocking" is deliberate:
//! the rule then holds because it cannot be forgotten.
//!
//! # One task type, not two
//!
//! The store speaks [`tui_do_api::models::Task`] rather than a parallel domain struct.
//! cria carries two task models — `VikunjaTask` and `Task` — and conversions between them
//! are a standing source of dropped fields. There is one here, and the store adds local
//! bookkeeping alongside it rather than inside a copy of it.

mod labels;
mod outbox;
mod projects;
mod schema;
mod sql;
mod state;
mod tasks;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;

use crate::error::{CoreError, Result};

pub use labels::{LabelFilter, LabelOrder, LabelSort};
pub use outbox::{
    is_provisional, is_provisional_label, Mutation, OutboxEntry, QueueHealth, Subject,
};
pub use projects::{ProjectFilter, ProjectOrder, ProjectSort};
pub use state::{CURRENT_USER, LAST_PROJECT, LAST_PULL, LAST_RECONCILE, PAGE_CAP};
pub use tasks::{ProjectCounts, ServerApply, TaskCount, TaskFilter, TaskOrder, TaskSort};

/// A handle to the local store.
///
/// Cheap to clone; every clone shares one connection, serialised by a mutex. A pool would
/// buy concurrency SQLite cannot use for writes anyway, and the reads tui-do makes are
/// milliseconds against a few thousand rows.
#[derive(Debug, Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    path: StorePath,
}

/// Where a store lives, for error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StorePath {
    /// A file on disk.
    File(PathBuf),
    /// A private in-memory database, used by tests.
    Memory,
}

impl std::fmt::Display for StorePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::Memory => f.write_str("<memory>"),
        }
    }
}

impl Store {
    /// Open, creating and migrating the database if needed.
    ///
    /// # Errors
    /// [`CoreError::Store`] if the file cannot be opened or a migration fails, or
    /// [`CoreError::Config`] if the database was written by a newer tui-do.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || Self::open_blocking(path))
            .await
            .map_err(join_error)?
    }

    /// Where the database lives when nothing overrides it.
    ///
    /// `$TUI_DO_DB` wins, so a second instance can be pointed at a scratch database
    /// without touching the real one; otherwise it is the XDG data directory, which is
    /// where a cache that can be deleted and rebuilt belongs -- this is not config.
    ///
    /// # Errors
    /// [`CoreError::Config`] when the platform reports no data directory.
    pub fn default_path() -> Result<PathBuf> {
        if let Some(from_env) = std::env::var_os("TUI_DO_DB") {
            if !from_env.is_empty() {
                return Ok(PathBuf::from(from_env));
            }
        }
        dirs::data_dir()
            .map(|dir| dir.join("tui-do").join("tui-do.db"))
            .ok_or_else(|| crate::CoreError::Config {
                path: "$XDG_DATA_HOME".to_string(),
                reason: "no data directory could be determined for this user".to_string(),
            })
    }

    /// Open a private in-memory store.
    ///
    /// Synchronous, and the one constructor that is: tests want a store without a
    /// runtime, and there is no I/O to keep off the async threads.
    ///
    /// # Errors
    /// [`CoreError::Store`] if the schema cannot be created.
    pub fn in_memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection)?;
        schema::migrate(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: StorePath::Memory,
        })
    }

    fn open_blocking(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut connection = Connection::open(&path)?;
        configure(&connection)?;
        schema::migrate(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: StorePath::File(path),
        })
    }

    /// Where this store lives, for diagnostics.
    #[must_use]
    pub fn location(&self) -> String {
        self.path.to_string()
    }

    /// The schema version currently on disk.
    ///
    /// # Errors
    /// [`CoreError::Store`] if the pragma cannot be read.
    pub async fn schema_version(&self) -> Result<i64> {
        self.read(|connection| Ok(connection.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .await
    }

    /// Run `work` against the connection on a blocking thread.
    ///
    /// The lock is taken inside the closure and released when it returns, so it is never
    /// held across an await point.
    pub(crate) async fn read<T, F>(&self, work: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let guard = connection.lock().unwrap_or_else(PoisonError::into_inner);
            work(&guard)
        })
        .await
        .map_err(join_error)?
    }

    /// Run `work` inside a transaction that commits when it returns `Ok`.
    ///
    /// Rolling back on error is what makes an optimistic write and its outbox entry one
    /// atomic act: the UI can never show a change that was not queued, and the queue can
    /// never hold an entry for a change the UI did not make.
    pub(crate) async fn write<T, F>(&self, work: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut guard = connection.lock().unwrap_or_else(PoisonError::into_inner);
            let transaction = guard.transaction()?;
            let value = work(&transaction)?;
            transaction.commit()?;
            Ok(value)
        })
        .await
        .map_err(join_error)?
    }
}

/// Pragmas every connection needs.
fn configure(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        -- Readers do not block the writer, which is what lets a sync pass write while
        -- the UI reads without either waiting.
        PRAGMA journal_mode = WAL;
        -- Durable enough: WAL plus NORMAL loses at most the last transaction on a power
        -- cut, and every row here can be re-fetched from the server.
        PRAGMA synchronous = NORMAL;
        -- The cascade rules in the schema are decoration without this. SQLite defaults it
        -- off, per connection, forever.
        PRAGMA foreign_keys = ON;
        -- Rather than failing instantly if the other connection is mid-write.
        PRAGMA busy_timeout = 5000;
        ",
    )?;
    Ok(())
}

/// Turn a `spawn_blocking` join failure into a store error.
///
/// Only reachable if the blocking task panicked, which for this code means a bug.
fn join_error(error: tokio::task::JoinError) -> CoreError {
    CoreError::Config {
        path: "<store>".to_string(),
        reason: format!("a store operation did not complete: {error}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_in_memory_store_is_migrated_on_creation() {
        let store = Store::in_memory().unwrap();
        assert_eq!(
            store.schema_version().await.unwrap(),
            schema::target_version()
        );
        assert_eq!(store.location(), "<memory>");
    }

    #[tokio::test]
    async fn opening_creates_the_file_and_its_parent_directory() {
        let dir = std::env::temp_dir().join(format!("tui-do-store-test-{}", std::process::id()));
        let path = dir.join("nested").join("tui-do.db");
        let _ = std::fs::remove_dir_all(&dir);

        let store = Store::open(&path).await.unwrap();
        assert!(path.exists(), "the database file should have been created");
        assert_eq!(
            store.schema_version().await.unwrap(),
            schema::target_version()
        );

        // Reopening an existing store must not re-run migrations or lose anything.
        drop(store);
        let reopened = Store::open(&path).await.unwrap();
        assert_eq!(
            reopened.schema_version().await.unwrap(),
            schema::target_version()
        );

        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn foreign_keys_are_actually_on() {
        // SQLite defaults this off per connection, so the schema's cascades would be
        // decoration. Cheap to assert, and easy to lose in a future refactor.
        let store = Store::in_memory().unwrap();
        let enabled: i64 = store
            .read(|c| Ok(c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(enabled, 1);
    }

    #[tokio::test]
    async fn a_failed_write_rolls_back_entirely() {
        // The property optimistic writes depend on: a task row and its outbox entry
        // either both land or neither does.
        let store = Store::in_memory().unwrap();
        let result: Result<()> = store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO tasks (id, project_id, title, synced_at) VALUES (1,1,'t','now')",
                    [],
                )?;
                Err(CoreError::Config {
                    path: "test".into(),
                    reason: "deliberate".into(),
                })
            })
            .await;
        assert!(result.is_err());

        let count: i64 = store
            .read(|c| Ok(c.query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(count, 0, "the failed transaction left a row behind");
    }

    #[tokio::test]
    async fn clones_share_one_database() {
        let store = Store::in_memory().unwrap();
        let clone = store.clone();
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO tasks (id, project_id, title, synced_at) VALUES (1,1,'t','now')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let count: i64 = clone
            .read(|c| Ok(c.query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}
