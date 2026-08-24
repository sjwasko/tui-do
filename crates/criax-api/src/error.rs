//! Error type for the Vikunja API client.
//!
//! Deliberately distinguishes transport failures from HTTP status failures from
//! deserialization failures: the sync engine retries the first, surfaces the second to the
//! user, and treats the third as a spec drift worth reporting loudly.

use std::fmt;

/// Result alias for API calls.
pub type Result<T> = std::result::Result<T, ApiError>;

/// A Vikunja error body.
///
/// Both shapes the server uses parse into this: `web.HTTPError` (`{"code": 1011,
/// "message": "..."}`) and `models.Message` (`{"message": "..."}`). The spec is explicit
/// that the `code` matters — "You should always check for the status code in the
/// response, not only the http status code" — because one HTTP status covers many
/// distinct failures. It is carried through to [`ApiError`] rather than discarded.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ErrorBody {
    /// Vikunja's own error code, documented at <https://vikunja.io/docs/errors/>.
    /// Absent on the handful of endpoints that answer with a bare message.
    #[serde(default)]
    pub code: Option<i64>,

    /// Human-readable explanation.
    #[serde(default)]
    pub message: String,
}

impl ErrorBody {
    /// Parse an error body, falling back to the raw text when it is not JSON.
    ///
    /// A proxy or a crashed server can answer with HTML, and that text is more useful to
    /// the user than "expected value at line 1".
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_else(|_| Self {
            code: None,
            message: raw.trim().chars().take(200).collect(),
        })
    }
}

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
        /// Vikunja's own error code, when it sent one.
        code: Option<i64>,
        /// Server-supplied message, or the raw body when it was not JSON.
        message: String,
    },

    /// Credentials are missing, expired, or insufficient.
    #[error("not authorized: {message}")]
    Unauthorized {
        /// Vikunja's own error code, when it sent one. `11` is the expired-token code.
        code: Option<i64>,
        /// Server-supplied message.
        message: String,
    },

    /// The server is rate limiting us (429).
    ///
    /// Vikunja rate limits the auth endpoints hard — ten requests per window on
    /// `/login` and `/user/token/refresh` — so this is a normal condition to handle,
    /// not an exotic one.
    #[error("rate limited by Vikunja: {message}")]
    RateLimited {
        /// How long to wait before retrying, when the server said.
        retry_after: Option<std::time::Duration>,
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
    /// `spec/vikunja.json` was captured from. Refresh the spec with
    /// `cargo xtask fetch-spec` and read the diff.
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

    /// A request was built from a path that is not in [`crate::endpoints::ALL`].
    ///
    /// The conformance test proves every constant in that list exists in the spec; this
    /// is the runtime half of the same rule, and it means no code path can reach an
    /// endpoint the spec has never seen — not even by formatting a URL by hand.
    #[error("refusing to call {template}: not a known Vikunja endpoint")]
    UnknownEndpoint {
        /// The path template that was rejected.
        template: String,
    },

    /// The server sent more data than the client is willing to hold in memory.
    ///
    /// A guard against a broken or hostile server, not a limit users should ever meet:
    /// the largest legitimate response here is one page of tasks.
    #[error("response from {url} exceeded {limit} bytes")]
    ResponseTooLarge {
        /// The URL that answered.
        url: String,
        /// The cap that was exceeded, in bytes.
        limit: usize,
    },

    /// A request needs credentials the client does not have.
    ///
    /// Raised before sending, so an unconfigured client fails with something actionable
    /// instead of a 401 from the server.
    #[error("no credentials configured: {action} requires being logged in")]
    NotAuthenticated {
        /// What was being attempted.
        action: &'static str,
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
            Self::Transport { .. } | Self::Server { .. } | Self::RateLimited { .. } => true,
            Self::Rejected { status, .. } => *status == 429,
            Self::Unauthorized { .. }
            | Self::Deserialize { .. }
            | Self::InvalidUrl { .. }
            | Self::UnknownEndpoint { .. }
            | Self::ResponseTooLarge { .. }
            | Self::NotAuthenticated { .. } => false,
        }
    }

    /// How long to wait before a retry, when the server asked for a specific delay.
    #[must_use]
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Vikunja's own error code, where the server sent one.
    #[must_use]
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Rejected { code, .. } | Self::Unauthorized { code, .. } => *code,
            _ => None,
        }
    }

    /// Build the right variant for a failing HTTP status.
    ///
    /// Kept here rather than in the client so every call site classifies statuses the
    /// same way: 401/403 are an auth problem the user must fix, 429 is a wait, 5xx is
    /// the server's fault and worth retrying, everything else is a rejection.
    pub(crate) fn from_status(
        status: u16,
        body: &ErrorBody,
        retry_after: Option<std::time::Duration>,
    ) -> Self {
        match status {
            401 | 403 => Self::Unauthorized {
                code: body.code,
                message: body.message.clone(),
            },
            429 => Self::RateLimited {
                retry_after,
                message: body.message.clone(),
            },
            500..=599 => Self::Server {
                status,
                message: body.message.clone(),
            },
            _ => Self::Rejected {
                status,
                code: body.code,
                message: body.message.clone(),
            },
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
            ApiError::RateLimited { .. } => write!(f, "rate limited"),
            ApiError::Deserialize { .. } => write!(f, "spec drift"),
            ApiError::InvalidUrl { .. } => write!(f, "bad url"),
            ApiError::UnknownEndpoint { .. } => write!(f, "unknown endpoint"),
            ApiError::ResponseTooLarge { .. } => write!(f, "response too large"),
            ApiError::NotAuthenticated { .. } => write!(f, "not logged in"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_http_error_shape() {
        let body = ErrorBody::parse(r#"{"code":1011,"message":"Wrong username or password."}"#);
        assert_eq!(body.code, Some(1011));
        assert_eq!(body.message, "Wrong username or password.");
    }

    #[test]
    fn parses_the_bare_message_shape() {
        // /user/token/refresh answers with this one: a message and no code.
        let body = ErrorBody::parse(r#"{"message":"No refresh token provided."}"#);
        assert_eq!(body.code, None);
        assert_eq!(body.message, "No refresh token provided.");
    }

    #[test]
    fn falls_back_to_raw_text_for_non_json_bodies() {
        // A reverse proxy in the way answers with HTML, and that is worth showing.
        let body = ErrorBody::parse("<html>502 Bad Gateway</html>");
        assert_eq!(body.code, None);
        assert_eq!(body.message, "<html>502 Bad Gateway</html>");
    }

    #[test]
    fn statuses_map_to_the_variant_the_sync_engine_expects() {
        let body = ErrorBody {
            code: Some(11),
            message: "expired".into(),
        };
        assert!(matches!(
            ApiError::from_status(401, &body, None),
            ApiError::Unauthorized { .. }
        ));
        assert!(matches!(
            ApiError::from_status(403, &body, None),
            ApiError::Unauthorized { .. }
        ));
        assert!(matches!(
            ApiError::from_status(429, &body, None),
            ApiError::RateLimited { .. }
        ));
        assert!(matches!(
            ApiError::from_status(503, &body, None),
            ApiError::Server { .. }
        ));
        assert!(matches!(
            ApiError::from_status(404, &body, None),
            ApiError::Rejected { .. }
        ));
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        let body = ErrorBody::default();
        assert!(ApiError::from_status(500, &body, None).is_retryable());
        assert!(ApiError::from_status(429, &body, None).is_retryable());
        assert!(!ApiError::from_status(400, &body, None).is_retryable());
        assert!(!ApiError::from_status(401, &body, None).is_retryable());
    }

    #[test]
    fn the_vikunja_error_code_survives_classification() {
        let body = ErrorBody {
            code: Some(4004),
            message: "task does not exist".into(),
        };
        assert_eq!(ApiError::from_status(404, &body, None).code(), Some(4004));
    }
}
