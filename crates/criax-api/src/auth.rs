//! How the client proves who it is.
//!
//! Vikunja accepts two credentials, and the spec's own description says both travel the
//! same way: `Authorization: Bearer <token>`. The difference is lifetime, and it is the
//! whole reason this is an enum rather than a string.
//!
//! - A **scoped API token** (`tk_…`, created in the web UI under Settings → API tokens)
//!   is long-lived and cannot be renewed. When it expires the user makes a new one.
//! - A **JWT** from `POST /login` is short-lived and *is* renewable, via
//!   `POST /user/token/refresh` — which reads the refresh cookie that login set, not the
//!   JWT, so it only works on a client that keeps cookies.
//!
//! cria supports neither properly: it takes an API token from config and has no concept
//! of expiry, so a session that outlives its token fails with an unexplained 401.

use crate::models::Login;
use crate::secret::Secret;

/// What the user configured criax to authenticate with.
#[derive(Debug, Clone)]
pub enum Credentials {
    /// A scoped API token created in Vikunja's settings.
    ApiToken(Secret),

    /// A username and password, exchanged for a JWT by `POST /login`.
    Password(Box<Login>),
}

impl Credentials {
    /// A scoped API token.
    #[must_use]
    pub fn api_token(token: impl Into<Secret>) -> Self {
        Self::ApiToken(token.into())
    }

    /// A username and password.
    #[must_use]
    pub fn password(username: impl Into<String>, password: impl Into<Secret>) -> Self {
        Self::Password(Box::new(Login::new(username, password)))
    }

    /// Whether using these credentials requires a round trip before any other request.
    #[must_use]
    pub fn needs_login(&self) -> bool {
        matches!(self, Self::Password(_))
    }
}

/// The credential the client is currently sending.
#[derive(Debug, Clone, Default)]
pub(crate) enum Session {
    /// No credential. `/info` and `/login` still work; nothing else does.
    #[default]
    Anonymous,

    /// A static API token. Sent on every request, never refreshed.
    ApiToken(Secret),

    /// A JWT from `/login`, renewable while the refresh cookie is still valid.
    Jwt(Secret),
}

impl Session {
    /// The value for the `Authorization` header, if there is one.
    pub(crate) fn bearer(&self) -> Option<&Secret> {
        match self {
            Self::Anonymous => None,
            Self::ApiToken(token) | Self::Jwt(token) => Some(token),
        }
    }

    /// Whether a 401 on this session is worth answering with a token refresh.
    ///
    /// Only true for a JWT: retrying an API token would send the identical rejected
    /// credential a second time, turning one failure into two.
    pub(crate) fn is_refreshable(&self) -> bool {
        matches!(self, Self::Jwt(_))
    }
}

/// How the client is currently authenticated, for `criax doctor` and status display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// Not authenticated.
    Anonymous,
    /// Using a static API token.
    ApiToken,
    /// Using a JWT from a password login.
    Jwt,
}

impl From<&Session> for AuthKind {
    fn from(session: &Session) -> Self {
        match session {
            Session::Anonymous => Self::Anonymous,
            Session::ApiToken(_) => Self::ApiToken,
            Session::Jwt(_) => Self::Jwt,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn only_a_jwt_session_is_worth_refreshing() {
        assert!(Session::Jwt(Secret::new("jwt")).is_refreshable());
        assert!(!Session::ApiToken(Secret::new("tk_x")).is_refreshable());
        assert!(!Session::Anonymous.is_refreshable());
    }

    #[test]
    fn an_anonymous_session_sends_no_header() {
        assert!(Session::Anonymous.bearer().is_none());
        assert_eq!(
            Session::ApiToken(Secret::new("tk_x"))
                .bearer()
                .map(Secret::expose),
            Some("tk_x")
        );
    }

    #[test]
    fn credentials_do_not_leak_through_debug() {
        let rendered = format!("{:?}", Credentials::password("swasko", "hunter2"));
        assert!(!rendered.contains("hunter2"));
        let rendered = format!("{:?}", Credentials::api_token("tk_secret"));
        assert!(!rendered.contains("tk_secret"));
    }
}
