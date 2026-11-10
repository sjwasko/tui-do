//! Vikunja's date encoding.
//!
//! Vikunja is a Go service backed by SQL columns that are `NOT NULL`, so "no date" is
//! not `null` on the wire — it is Go's zero `time.Time`, serialised as
//! `"0001-01-01T00:00:00Z"`. A task with no due date therefore arrives carrying a due
//! date in the year 1, and naive parsing produces a task that looks 2000 years overdue.
//!
//! The OpenAPI spec is no help here: every date field is declared as a bare
//! `{"type": "string"}` with no `format`, so a code generator would emit `String` and
//! push the problem downstream.
//!
//! Everything in this module funnels through [`parse`], which maps the zero value —
//! and empty strings, which some endpoints return instead — to `None`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The wire form Vikunja uses for an unset date: Go's zero `time.Time`.
const ZERO: &str = "0001-01-01T00:00:00Z";

/// Whether a parsed instant is Go's zero time, possibly in another zone's clothing.
///
/// The zero instant can arrive rendered in another zone -- `0000-12-31T19:00:00-05:00`,
/// or with a local-mean-time offset that RFC 3339 truncates, leaving it seconds short of
/// the zero. An offset is never as much as a day, so anything that is still on or before
/// 1 January of year 1 in UTC is the zero in disguise. Everything after is a real date.
///
/// This was a cutoff at the year 1900, on the reasoning that "no real task has a date
/// before 1970". A date is the user's to choose, though, and reading one as unset made
/// the next update send the zero time and clear it on the server. On 2026-11-10 the dev
/// server spelled unset as the literal UTC `0001-01-01T00:00:00Z` while rendering real
/// dates at `-04:00`, so the shifted forms are tolerated rather than expected.
fn is_zero_time(instant: DateTime<Utc>) -> bool {
    use chrono::Datelike;
    instant.year() < 1 || (instant.year() == 1 && instant.ordinal() == 1)
}

/// Parse a Vikunja timestamp, mapping its several spellings of "unset" to `None`.
#[must_use]
pub fn parse(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == ZERO {
        return None;
    }
    let parsed = DateTime::parse_from_rfc3339(trimmed)
        .ok()?
        .with_timezone(&Utc);
    (!is_zero_time(parsed)).then_some(parsed)
}

/// Render a timestamp for Vikunja, using its zero value for `None`.
///
/// Sending `null` where Vikunja expects a `time.Time` is rejected; sending the zero
/// value is how a date is cleared.
#[must_use]
pub fn render(value: Option<DateTime<Utc>>) -> String {
    match value {
        Some(dt) => dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        None => ZERO.to_string(),
    }
}

/// serde adapter for `Option<DateTime<Utc>>` fields on Vikunja models.
///
/// Use as `#[serde(with = "crate::models::datetime::optional")]`.
pub mod optional {
    use super::{parse, render};
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Deserialize a Vikunja timestamp into `Option<DateTime<Utc>>`.
    ///
    /// # Errors
    /// Never fails on a malformed date: an unparseable value becomes `None` rather than
    /// failing the whole response. One bad timestamp should not cost the user their task
    /// list.
    pub fn deserialize<'de, D>(de: D) -> Result<Option<DateTime<Utc>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Option<String> = Option::deserialize(de)?;
        Ok(raw.as_deref().and_then(parse))
    }

    /// Serialize `Option<DateTime<Utc>>` in Vikunja's expected form.
    ///
    /// # Errors
    /// Propagates any failure from the underlying serializer.
    pub fn serialize<S>(value: &Option<DateTime<Utc>>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.serialize_str(&render(*value))
    }
}

/// A timestamp that is always present on the wire but may still mean "unset".
///
/// Deserializes with the same rules as [`optional`]; exists so struct fields can read
/// as `Timestamp` rather than repeating the `serde(with = ...)` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timestamp(pub Option<DateTime<Utc>>);

impl Timestamp {
    /// The wrapped instant, if the field was actually set.
    #[must_use]
    pub fn get(self) -> Option<DateTime<Utc>> {
        self.0
    }
}

impl From<Option<DateTime<Utc>>> for Timestamp {
    fn from(value: Option<DateTime<Utc>>) -> Self {
        Self(value)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Option<String> = Option::deserialize(de)?;
        Ok(Self(raw.as_deref().and_then(parse)))
    }
}

impl Serialize for Timestamp {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.serialize_str(&render(self.0))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn zero_value_is_unset() {
        assert_eq!(parse("0001-01-01T00:00:00Z"), None);
    }

    #[test]
    fn empty_string_is_unset() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
    }

    #[test]
    fn timezone_shifted_zero_value_is_still_unset() {
        // The zero value converted out of UTC lands in year 0 or year 1 depending on
        // direction. Both must read as unset, which is why the cutoff is a year and not
        // an equality check against the literal.
        assert_eq!(parse("0001-01-01T05:00:00+05:00"), None);
        assert_eq!(parse("0000-12-31T19:00:00-05:00"), None);
    }

    #[test]
    fn a_real_date_at_or_before_1900_is_kept() {
        // It read as unset, so the next update sent the zero time and cleared a real
        // date on the server. Only Go's zero instant means unset.
        for raw in [
            "1900-01-01T00:00:00Z",
            "1899-06-15T12:00:00Z",
            "1066-10-14T09:00:00Z",
            "0002-01-01T00:00:00Z",
        ] {
            let parsed = parse(raw);
            assert!(parsed.is_some(), "{raw} read as unset");
            assert_eq!(parse(&render(parsed)), parsed, "{raw}");
        }
    }

    #[test]
    fn the_zero_instant_in_go_local_mean_time_is_still_unset() {
        // Go renders the zero instant in America/New_York with the zone's LMT offset,
        // -4:56:02, which RFC 3339 truncates to -04:56: two seconds short of the zero.
        assert_eq!(parse("0000-12-31T19:03:58-04:56"), None);
    }

    #[test]
    fn real_dates_survive() {
        let parsed = parse("2026-08-24T13:45:00Z").expect("a real date should parse");
        assert_eq!(
            parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-08-24T13:45:00Z"
        );
    }

    #[test]
    fn offsets_normalise_to_utc() {
        let parsed = parse("2026-08-24T09:45:00-04:00").expect("offset date should parse");
        assert_eq!(
            parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-08-24T13:45:00Z"
        );
    }

    #[test]
    fn garbage_is_unset_rather_than_an_error() {
        assert_eq!(parse("not a date"), None);
        assert_eq!(parse("2026-13-45T99:99:99Z"), None);
    }

    #[test]
    fn none_renders_as_the_zero_value() {
        assert_eq!(render(None), ZERO);
    }

    #[test]
    fn render_round_trips_through_parse() {
        let original = parse("2026-08-24T13:45:00Z");
        assert_eq!(parse(&render(original)), original);
    }

    #[test]
    fn timestamp_deserializes_from_json() {
        let unset: Timestamp = serde_json::from_str("\"0001-01-01T00:00:00Z\"").unwrap();
        assert_eq!(unset.get(), None);
        let null: Timestamp = serde_json::from_str("null").unwrap();
        assert_eq!(null.get(), None);
        let set: Timestamp = serde_json::from_str("\"2026-08-24T13:45:00Z\"").unwrap();
        assert!(set.get().is_some());
    }
}
