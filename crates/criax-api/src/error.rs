//! Error type for the Vikunja API client.
//!
//! Deliberately distinguishes transport failures from HTTP status failures from
//! deserialization failures: the sync engine retries the first, surfaces the second to the
//! user, and treats the third as a spec drift worth reporting loudly.

use std::fmt;

/// Result alias for API calls.
pub type Result<T> = std::result::Result<T, ApiError>;

/// A failure while talking to a Vikunja server.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The request never produced a response: DNS, TLS, connection refused, timeout.
    #[error("could not reach the Vikunja server at {url}: {source}")]
    Transport {
        /// The URL that was being requested.
        url: String,
        /// The underlying transport failure.
        #[source]
        source: reqwest::Error,
    },

    /// The server rejected the request (4xx).
    #[error("Vikunja rejected the request: {status} {message}")]
    Rejected {
        /// HTTP status returned by the server.
        status: u16,
        /// Server-supplied message, or the raw body when it was not JSON.
        message: String,
    },

    /// Credentials are missing, expired, or insufficient.
    #[error("not authorized: {message}")]
    Unauthorized {
        /// Server-supplied message.
        message: String,
    },

    /// The server failed (5xx).
    #[error("Vikunja server error: {status} {message}")]
    Server {
        /// HTTP status returned by the server.
        status: u16,
        /// Server-supplied message, or the raw body when it was not JSON.
        message: String,
    },

    /// The response did not match the shape declared in the OpenAPI spec.
    ///
    /// This usually means the server is a different Vikunja version than
    /// `spec/vikunja.json` was captured from. Refresh the spec and re-run codegen.
    #[error("unexpected response shape from {url} (spec drift?): {source}")]
    Deserialize {
        /// The URL whose response could not be parsed.
        url: String,
        /// The underlying serde failure.
        #[source]
        source: serde_json::Error,
    },

    /// The configured base URL could not be parsed or joined.
    #[error("invalid Vikunja URL {url}: {reason}")]
    InvalidUrl {
        /// The offending URL.
        url: String,
        /// Why it could not be used.
        reason: String,
    },
}

impl ApiError {
    /// Whether retrying the same request unchanged could plausibly succeed.
    ///
    /// The sync engine uses this to decide between re-queueing an outbox entry and
    /// surfacing the failure to the user.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::Server { .. } => true,
            Self::Rejected { status, .. } => *status == 429,
            Self::Unauthorized { .. } | Self::Deserialize { .. } | Self::InvalidUrl { .. } => false,
        }
    }
}

/// Rendering helper so callers can log a compact one-line form.
#[derive(Debug)]
pub struct Brief<'a>(pub &'a ApiError);

impl fmt::Display for Brief<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ApiError::Transport { .. } => write!(f, "unreachable"),
            ApiError::Rejected { status, .. } | ApiError::Server { status, .. } => {
                write!(f, "http {status}")
            }
            ApiError::Unauthorized { .. } => write!(f, "unauthorized"),
            ApiError::Deserialize { .. } => write!(f, "spec drift"),
            ApiError::InvalidUrl { .. } => write!(f, "bad url"),
        }
    }
}
