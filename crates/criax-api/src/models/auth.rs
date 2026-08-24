//! Login request and token response.

use serde::{Deserialize, Serialize};

use crate::secret::Secret;

/// The body of `POST /login` (`user.Login` in the spec).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Login {
    /// The username to log in as. Vikunja also accepts the email address here.
    pub username: String,

    /// The password.
    pub password: Secret,

    /// The current TOTP passcode, for accounts with two-factor enabled.
    ///
    /// Always sent, empty when not applicable: the server only looks at it when the
    /// account has TOTP turned on, and omitting the field entirely is not better.
    #[serde(default)]
    pub totp_passcode: String,

    /// Ask for a long-lived JWT — Vikunja's "remember me".
    ///
    /// criax sets this: a terminal client that has to re-prompt for a password mid-session
    /// is worse than one holding a longer-lived token, and the token is refreshable either
    /// way.
    #[serde(default)]
    pub long_token: bool,
}

impl Login {
    /// A login for a username and password, asking for a long-lived token.
    #[must_use]
    pub fn new(username: impl Into<String>, password: impl Into<Secret>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            totp_passcode: String::new(),
            long_token: true,
        }
    }

    /// Attach a TOTP passcode.
    #[must_use]
    pub fn with_totp(mut self, passcode: impl Into<String>) -> Self {
        self.totp_passcode = passcode.into();
        self
    }
}

/// The response from `POST /login` and `POST /user/token/refresh` (`auth.Token`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Token {
    /// The JWT to send as `Authorization: Bearer <token>`.
    pub token: Secret,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn login_serializes_the_fields_vikunja_expects() {
        let body = serde_json::to_value(Login::new("swasko", "hunter2")).unwrap();
        assert_eq!(body["username"], "swasko");
        assert_eq!(body["password"], "hunter2");
        assert_eq!(body["long_token"], true);
        assert_eq!(body["totp_passcode"], "");
    }

    #[test]
    fn a_login_does_not_print_its_password() {
        let rendered = format!("{:?}", Login::new("swasko", "hunter2"));
        assert!(rendered.contains("swasko"));
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn token_parses_the_server_response() {
        let token: Token = serde_json::from_str(r#"{"token":"eyJhbGci.x.y"}"#).unwrap();
        assert_eq!(token.token.expose(), "eyJhbGci.x.y");
    }
}
