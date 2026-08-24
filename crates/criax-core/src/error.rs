//! Error type for the core domain layer.

/// Result alias for core operations.
pub type Result<T> = std::result::Result<T, CoreError>;

/// A failure in the store, sync engine, parser or configuration layer.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The local SQLite store failed.
    #[error("local store error: {0}")]
    Store(#[from] rusqlite::Error),

    /// Talking to the Vikunja server failed.
    #[error(transparent)]
    Api(#[from] criax_api::ApiError),

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

    /// An I/O operation outside the store failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
