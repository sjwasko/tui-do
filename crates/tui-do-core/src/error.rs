//! Error type for the core domain layer.

/// Result alias for core operations.
pub type Result<T> = std::result::Result<T, CoreError>;

/// A failure in the store, sync engine, parser or configuration layer.
///
/// `#[non_exhaustive]` since `Refused` was added (BUG-27): a match outside this crate
/// needs a wildcard arm, so the next new variant is not a breaking change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// The local SQLite store failed.
    #[error("local store error: {0}")]
    Store(#[from] rusqlite::Error),

    /// Talking to the Vikunja server failed.
    #[error(transparent)]
    Api(#[from] tui_do_api::ApiError),

    /// The configuration file could not be read or parsed.
    #[error("config error in {path}: {reason}")]
    Config {
        /// Path to the offending config file.
        path: String,
        /// What was wrong with it.
        reason: String,
    },

    /// A queued mutation could not be encoded or decoded.
    ///
    /// The outbox stores mutations as JSON, so this is either a bug in this build or an
    /// entry written by one that spelled a variant differently.
    #[error("could not encode a queued change: {0}")]
    Encoding(#[from] serde_json::Error),

    /// The store would not queue a change, because it could only ever fail on the wire.
    ///
    /// An attach or a detach naming a provisional label whose `CreateLabel` is no longer
    /// queued: no server will ever issue that id, so the request is a 4xx that discards
    /// every other entry queued for the task (BUG-27).
    #[error("not queued: {0}")]
    Refused(String),

    /// An I/O operation outside the store failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
