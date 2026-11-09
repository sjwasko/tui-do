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
    is_provisional, is_provisional_label, Mutation, OutboxEntry, QueueHealth, Queued, Subject,
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
    /// How many outbox payloads the drain's reads have parsed. See
    /// [`Store::payload_decodes`].
    decodes: Arc<DecodeCount>,
}

/// Counts payload parses for [`Store::payload_decodes`]. Holds nothing, and counts
/// nothing, in a build without the `test-hooks` feature.
#[derive(Debug, Default)]
pub(crate) struct DecodeCount {
    #[cfg(any(test, feature = "test-hooks"))]
    count: std::sync::atomic::AtomicU64,
}

impl DecodeCount {
    /// One more payload parsed.
    #[allow(clippy::unused_self)]
    pub(crate) fn bump(&self) {
        #[cfg(any(test, feature = "test-hooks"))]
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
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

    /// Open an existing store for reading only: never creating, migrating or
    /// re-permissioning it.
    ///
    /// For `tui-do add --dry-run`, which promises to write nothing. [`Store::open`]
    /// creates the file and its directory, tightens their modes and runs every pending
    /// migration, so even a dry run through it could change the disk. This opens with
    /// `SQLITE_OPEN_READ_ONLY` and `query_only`, so a write through it fails rather than
    /// landing.
    ///
    /// # Errors
    /// [`CoreError::NotReady`] if there is no file at `path`, or its schema is older than
    /// this build's (a real run would upgrade it); [`CoreError::Config`] if it is newer;
    /// [`CoreError::Store`] if SQLite cannot open it.
    pub async fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || Self::open_read_only_blocking(path))
            .await
            .map_err(join_error)?
    }

    fn open_read_only_blocking(path: PathBuf) -> Result<Self> {
        if !path.is_file() {
            return Err(CoreError::NotReady(format!(
                "no local store yet at {}; run tui-do once to sync",
                path.display()
            )));
        }
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI;
        // A WAL database read through a plain read-only connection makes SQLite create
        // `-wal` and `-shm` beside it when they are missing, which is a write. They are
        // missing only when nothing has the store open and the last writer checkpointed,
        // so the file alone is the whole store and `immutable` reads it without either.
        // When they exist another tui-do may be writing, and the WAL has to be read.
        let connection = if sidecars(&path).iter().any(|sidecar| sidecar.exists()) {
            Connection::open_with_flags(&path, flags)?
        } else {
            Connection::open_with_flags(format!("{}?immutable=1", file_uri(&path)), flags)?
        };
        // Not `configure`: setting the journal mode is a write. These two only change
        // this connection.
        connection.execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 5000;")?;
        let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let target = schema::target_version();
        if current < target {
            return Err(CoreError::NotReady(format!(
                "the local store at {} is at schema version {current}, and this tui-do \
                 uses {target}; a real run would upgrade it first, which a dry run will \
                 not do. Run tui-do once, then try again",
                path.display()
            )));
        }
        if current > target {
            return Err(CoreError::Config {
                path: path.display().to_string(),
                reason: format!(
                    "this database is at schema version {current}, newer than the {target} \
                     this build of tui-do understands. Use a newer tui-do."
                ),
            });
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: StorePath::File(path),
            decodes: Arc::default(),
        })
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
            decodes: Arc::default(),
        })
    }

    fn open_blocking(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                // SEC-1. The store holds every task title, description and queued
                // mutation, and `create_dir_all` uses the process umask -- so this was
                // `0755` around a `0644` database, readable by every account on the
                // machine, while the API token beside it was `0600`.
                //
                // **The directory is the durable half.** SQLite creates `-wal` and `-shm`
                // itself, at umask, on every open, so a mode set on the database file is
                // undone by the next one. A directory nobody else can traverse holds
                // whatever SQLite puts inside it, now and later.
                crate::config::restrict_to_owner(parent);
            }
        }
        let mut connection = Connection::open(&path)?;
        configure(&connection)?;
        schema::migrate(&mut connection)?;
        // Belt to the directory's braces, and it also tightens a database that an older
        // version created permissive. `configure` has run, so the WAL exists by now.
        crate::config::restrict_to_owner(&path);
        for sidecar in sidecars(&path) {
            if sidecar.exists() {
                crate::config::restrict_to_owner(&sidecar);
            }
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: StorePath::File(path),
            decodes: Arc::default(),
        })
    }

    /// How many queued payloads have been parsed by [`Store::pending`] and the reads the
    /// drain makes, since this store was opened.
    ///
    /// For tests only, and absent from a build without the `test-hooks` feature. BUG-23:
    /// the drain re-read and re-parsed the whole queue before every entry it sent, which
    /// is quadratic in the depth of the queue, and a count is the one way to assert that
    /// it no longer does without timing it.
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    #[must_use]
    pub fn payload_decodes(&self) -> u64 {
        self.decodes
            .count
            .load(std::sync::atomic::Ordering::Relaxed)
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
            // Immediate, not rusqlite's deferred default. A deferred transaction takes
            // a read snapshot and upgrades on first write, and in WAL a snapshot another
            // connection has overtaken answers `SQLITE_BUSY_SNAPSHOT` -- which
            // `busy_timeout` does *not* retry away, because waiting could only deadlock.
            // `Store::queue` is exactly that shape: `next_local_id` reads `sync_state`
            // and then writes it, inside the caller's transaction. Invisible while one
            // process holds the only connection; reachable the moment two do
            // (`tui-do add` beside a running interface, and later an MCP server).
            //
            // The cost is a marginally longer lock hold, taken from the first statement
            // rather than the first write.
            let transaction =
                guard.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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

/// The `-wal` and `-shm` files SQLite keeps beside a WAL database.
fn sidecars(path: &Path) -> [PathBuf; 2] {
    ["-wal", "-shm"].map(|suffix| {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        PathBuf::from(sidecar)
    })
}

/// `path` as an SQLite `file:` URI, percent-encoding everything but unreserved characters
/// and `/`, so a `?` or `#` in a directory name cannot be read as the query.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(byte) {
            uri.push(char::from(*byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
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

    /// SEC-1. The store holds every task title, every description and every queued
    /// mutation, and it was created at the process umask -- `0644` inside a `0755`
    /// directory, readable by any account on the machine. The API token beside it is
    /// `0600`, via `config::write_private`: the codebase already knew how, and the store
    /// simply never did it.
    ///
    /// **The directory is the assertion that matters.** SQLite creates `-wal` and `-shm`
    /// itself, at umask, every time the store is opened -- so a mode set on the database
    /// file alone is undone by the next open, and the only thing that holds across all of
    /// them is a directory no other account can traverse.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_store_is_not_readable_by_other_accounts() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("tui-do-perms-{}", std::process::id()));
        let path = dir.join("tui-do.db");
        let _ = std::fs::remove_dir_all(&dir);

        let store = Store::open(&path).await.unwrap();
        assert_eq!(
            store.schema_version().await.unwrap(),
            schema::target_version()
        );

        let shared =
            |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o077;
        assert_eq!(
            shared(&dir),
            0,
            "another account can traverse the store directory"
        );
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(
                shared(&entry.path()),
                0,
                "another account can read {:?}",
                entry.file_name()
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

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

    /// Item 10(b). `write()` opened rusqlite's default *deferred* transaction, which
    /// takes a read snapshot and upgrades on the first write. In WAL, a transaction
    /// whose snapshot another connection has overtaken answers `SQLITE_BUSY_SNAPSHOT`
    /// (extended code 517) -- and `busy_timeout` does **not** retry that one away, since
    /// waiting could only deadlock. It is `Store::queue`'s exact shape: `next_local_id`
    /// reads `sync_state` and then writes it, in the caller's transaction.
    ///
    /// **Two `Store` handles on one file, not two processes.** SQLite locks per
    /// *connection*, so two connections reproduce the multi-process case exactly and can
    /// be driven deterministically -- and the interleaving is forced, never sampled:
    ///
    /// ```text
    ///   connection A                      connection B
    ///   BEGIN; read sync_state            (waiting)
    ///   signal "read done"  ----------->  BEGIN; write sync_state; COMMIT
    ///   sleep 250ms                          (WAL advances past A's snapshot)
    ///   write sync_state    <- SQLITE_BUSY_SNAPSHOT before the fix
    /// ```
    ///
    /// **The sleep is load-bearing and must not become a wait for B.** After the fix A
    /// holds the write lock from `BEGIN`, B blocks at its own `BEGIN IMMEDIATE`, and an
    /// A that waited for B's commit would wait forever. 250 ms against a 5000 ms
    /// `busy_timeout` is a 20x margin: it sets the ordering, it is not a race window.
    #[tokio::test]
    async fn a_write_that_has_already_read_survives_another_connection_committing() {
        let dir = std::env::temp_dir().join(format!("tui-do-snapshot-{}", std::process::id()));
        let path = dir.join("tui-do.db");
        let _ = std::fs::remove_dir_all(&dir);

        // Two `open`s, not two clones: a clone shares the one connection and the mutex
        // around it, which is precisely what hides this today.
        let a = Store::open(&path).await.unwrap();
        let b = Store::open(&path).await.unwrap();

        let (read_done, wait_for_read) = tokio::sync::oneshot::channel::<()>();

        let writer = tokio::spawn(async move {
            a.write(move |tx| {
                let before = state::read_state(tx, LAST_PULL)?;
                assert_eq!(before, None, "the store should start with no watermark");
                let _ = read_done.send(());
                std::thread::sleep(std::time::Duration::from_millis(250));
                state::write_state(tx, LAST_PULL, "written by A")?;
                Ok(())
            })
            .await
        });

        wait_for_read.await.expect("A never reported its read");
        // A different key, so both writes are visible afterwards; the snapshot is
        // invalidated by any commit on the database, not only by one to the same row.
        b.set_state(LAST_RECONCILE, "written by B").await.unwrap();

        writer
            .await
            .unwrap()
            .expect("A's write was refused: a deferred snapshot B overtook answers 517");

        assert_eq!(
            b.state(LAST_PULL).await.unwrap().as_deref(),
            Some("written by A"),
            "A's write did not land"
        );
        assert_eq!(
            b.state(LAST_RECONCILE).await.unwrap().as_deref(),
            Some("written by B"),
            "B's write did not land"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A read-only open of a path with nothing at it says so, and leaves nothing behind:
    /// no file, no directory, no WAL. `Store::open` would have made all three.
    #[tokio::test]
    async fn a_read_only_open_of_a_missing_store_creates_nothing() {
        let dir = std::env::temp_dir().join(format!("tui-do-ro-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tui-do.db");

        let error = Store::open_existing_read_only(&path).await.unwrap_err();
        assert!(matches!(error, CoreError::NotReady(_)), "{error}");
        assert!(error.to_string().contains("run tui-do once"), "{error}");
        assert!(!dir.exists(), "the open created {}", dir.display());
    }

    /// An older store is refused, not upgraded: the file is the same bytes afterwards.
    #[tokio::test]
    async fn a_read_only_open_does_not_migrate_an_older_store() {
        let dir = std::env::temp_dir().join(format!("tui-do-ro-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tui-do.db");
        drop(Store::open(&path).await.unwrap());
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(&format!(
                    "PRAGMA user_version = {}",
                    schema::target_version() - 1
                ))
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let error = Store::open_existing_read_only(&path).await.unwrap_err();
        assert!(matches!(error, CoreError::NotReady(_)), "{error}");
        assert!(error.to_string().contains("would upgrade it"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the store changed");
        for sidecar in sidecars(&path) {
            assert!(!sidecar.exists(), "the open left {}", sidecar.display());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_path_is_escaped_into_a_uri() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/why?#%.db")),
            "file:/tmp/a%20b/why%3F%23%25.db"
        );
    }

    /// A current store opens, reads, and refuses a write.
    #[tokio::test]
    async fn a_read_only_store_reads_and_will_not_write() {
        let dir = std::env::temp_dir().join(format!("tui-do-ro-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tui-do.db");
        drop(Store::open(&path).await.unwrap());

        let store = Store::open_existing_read_only(&path).await.unwrap();
        assert_eq!(
            store.schema_version().await.unwrap(),
            schema::target_version()
        );
        assert!(store.set_state("probe", "x").await.is_err());
        assert_eq!(store.state("probe").await.unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// While another connection has the store open, what it wrote may still be only in
    /// the WAL, and a read-only open must see it rather than the stale file.
    #[tokio::test]
    async fn a_read_only_open_sees_what_a_live_writer_has_not_checkpointed() {
        let dir = std::env::temp_dir().join(format!("tui-do-ro-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tui-do.db");
        let writer = Store::open(&path).await.unwrap();
        writer.set_state("probe", "fresh").await.unwrap();

        let reader = Store::open_existing_read_only(&path).await.unwrap();
        assert_eq!(
            reader.state("probe").await.unwrap().as_deref(),
            Some("fresh")
        );

        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
