//! A string that does not print itself.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A credential: an API token, a JWT, or a password.
///
/// The point is the [`fmt::Debug`] impl. Every model in this crate derives `Debug`, the
/// workspace warns on types that do not, and `tracing` events routinely format whole
/// structs — so a bare `String` password is one `debug!` away from being written to a log
/// file. Wrapping it makes that mistake impossible to make by accident.
///
/// [`Serialize`] deliberately still emits the real value: the login body has to reach the
/// server. Redaction protects logs, not the wire.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a credential.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The underlying value, for putting in an `Authorization` header.
    ///
    /// Named to be conspicuous at the call site: every use of it is a place the secret
    /// escapes the wrapper, and there should be very few.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the credential is empty, which is never valid.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str("Secret(empty)")
        } else {
            // The length is safe to show and makes "I pasted the wrong thing" debuggable.
            write!(f, "Secret(redacted, {} chars)", self.0.chars().count())
        }
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Best effort only, and labelled as such: `clear` keeps the allocation, so the
        // pushes below overwrite the same bytes rather than a fresh buffer. It cannot
        // reach any copy the value already made (a `String` handed to `new`, a formatted
        // header), and the optimiser is free to elide it. Without `unsafe` — which the
        // workspace forbids — this is as far as it goes; it is not a substitute for
        // keeping secrets out of swap.
        let len = self.0.len();
        self.0.clear();
        for _ in 0..len {
            self.0.push('\0');
        }
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        String::deserialize(de).map(Self)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_value() {
        let secret = Secret::new("hunter2");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert!(!format!("{secret}").contains("hunter2"));
        assert_eq!(format!("{secret:?}"), "Secret(redacted, 7 chars)");
    }

    #[test]
    fn debug_of_a_containing_struct_is_also_clean() {
        // The realistic failure: a `debug!("{client:?}")` somewhere in the effect runtime.
        // Only ever read through the derived `Debug`, which the dead-code lint
        // does not count as a use.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Session {
            user: &'static str,
            token: Secret,
        }
        let rendered = format!(
            "{:?}",
            Session {
                user: "swasko",
                token: Secret::new("eyJhbGciOiJIUzI1NiJ9.payload.signature"),
            }
        );
        assert!(rendered.contains("swasko"));
        assert!(!rendered.contains("eyJ"));
    }

    #[test]
    fn serialization_still_sends_the_real_value() {
        assert_eq!(
            serde_json::to_string(&Secret::new("tk_abc")).unwrap(),
            "\"tk_abc\""
        );
    }

    #[test]
    fn round_trips_through_json() {
        let parsed: Secret = serde_json::from_str("\"tk_abc\"").unwrap();
        assert_eq!(parsed.expose(), "tk_abc");
    }
}
