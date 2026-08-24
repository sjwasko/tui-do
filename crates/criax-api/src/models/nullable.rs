//! Vikunja's other spelling of "empty".
//!
//! Go marshals a nil slice or map as `null`, not `[]`, and Vikunja leaves its collection
//! fields nil whenever the endpoint did not populate them. So a task from `GET /tasks`
//! arrives with `"reminders": null`, `"labels": null`, `"assignees": null` — while the
//! spec declares all three as arrays.
//!
//! `#[serde(default)]` does not cover this. It applies when a field is *absent*, and
//! these fields are present with a null value, so deserialization fails outright with
//! `invalid type: null, expected a sequence` — taking the whole page of tasks with it.
//!
//! This is the same class of problem as [`super::datetime`]: the spec describes the type
//! the server means, not the JSON it emits. Route every collection field through
//! [`null_as_default`].

use serde::{Deserialize, Deserializer};

/// Deserialize `null` as `T::default()` rather than as a type error.
///
/// Use as `#[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]`
/// — `default` is still needed for the field being absent entirely, which also happens.
///
/// # Errors
/// Propagates any failure from deserializing a non-null value.
pub fn null_as_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Sample {
        #[serde(default, deserialize_with = "null_as_default")]
        items: Vec<i64>,
        #[serde(default, deserialize_with = "null_as_default")]
        map: HashMap<String, i64>,
    }

    #[test]
    fn null_becomes_empty() {
        let parsed: Sample = serde_json::from_str(r#"{"items": null, "map": null}"#).unwrap();
        assert_eq!(parsed, Sample::default());
    }

    #[test]
    fn an_absent_field_still_works() {
        let parsed: Sample = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, Sample::default());
    }

    #[test]
    fn real_values_are_untouched() {
        let parsed: Sample = serde_json::from_str(r#"{"items": [1, 2], "map": {"a": 3}}"#).unwrap();
        assert_eq!(parsed.items, vec![1, 2]);
        assert_eq!(parsed.map.get("a"), Some(&3));
    }

    #[test]
    fn a_wrong_type_is_still_an_error() {
        // Null is the only thing being forgiven; a string where an array belongs is still
        // spec drift worth failing on.
        assert!(serde_json::from_str::<Sample>(r#"{"items": "nope"}"#).is_err());
    }
}
