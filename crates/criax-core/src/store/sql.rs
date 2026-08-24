//! The conversions every table shares.
//!
//! Dates, `LIKE` escaping and the `users` table are needed by tasks, projects and labels
//! alike. They live here rather than in whichever module happened to need them first, so
//! that "unset is `NULL`, not year one" is one function and not three.

use chrono::{DateTime, Utc};
use rusqlite::{params, Row, Transaction};

use crate::error::Result;
use criax_api::models::{User, UserId};

/// Read a nullable timestamp column.
pub(super) fn instant(row: &Row<'_>, column: &str) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(column)?;
    Ok(raw
        .as_deref()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|dt| dt.with_timezone(&Utc)))
}

/// Render a timestamp for storage. `None` is `NULL`, not year one.
pub(super) fn stamp(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.to_rfc3339())
}

/// Escape the wildcards in a `LIKE` pattern.
///
/// Without this, searching for `100%` matches everything, and `_` matches any character.
pub(super) fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Load `ids` into a temporary `keep_ids` table for a `NOT IN` delete.
///
/// A pull's keep-list runs to thousands of ids on this dev instance alone, and an
/// `IN (?1, ?2, ...)` of that length exceeds SQLite's parameter limit. A temp table has
/// no such ceiling, and the delete reads the same either way.
pub(super) fn keep_ids(tx: &Transaction<'_>, ids: impl Iterator<Item = i64>) -> Result<()> {
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS keep_ids (id INTEGER PRIMARY KEY)")?;
    tx.execute("DELETE FROM keep_ids", [])?;
    let mut insert = tx.prepare("INSERT OR IGNORE INTO keep_ids (id) VALUES (?1)")?;
    for id in ids {
        insert.execute(params![id])?;
    }
    Ok(())
}

/// Write a user seen embedded in another object.
pub(super) fn upsert_user(tx: &Transaction<'_>, user: &User) -> Result<()> {
    tx.execute(
        "INSERT INTO users (id, username, name, email) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (id) DO UPDATE SET
            username = excluded.username, name = excluded.name,
            -- Only the authenticated user's email is ever populated; an empty one from
            -- an embedded copy must not erase the real one.
            email = CASE WHEN excluded.email = '' THEN users.email ELSE excluded.email END",
        params![user.id.get(), user.username, user.name, user.email],
    )?;
    Ok(())
}

/// Rebuild an embedded user from a row that joined `users` under a prefix.
///
/// `id_column` holds the foreign key, which is `0` rather than `NULL` when unset --
/// Vikunja's convention, kept because the columns arrived that way. The joined columns
/// are absent when the user has not been seen yet, so the id alone still produces a
/// `User`: an id with an empty username renders as "someone", where a `None` would
/// render as "nobody" and be wrong.
pub(super) fn joined_user(
    row: &Row<'_>,
    id_column: &str,
    prefix: &str,
) -> rusqlite::Result<Option<User>> {
    let id: i64 = row.get(id_column)?;
    if id == 0 {
        return Ok(None);
    }
    Ok(Some(User {
        id: UserId(id),
        username: row
            .get::<_, Option<String>>(format!("{prefix}_username").as_str())?
            .unwrap_or_default(),
        name: row
            .get::<_, Option<String>>(format!("{prefix}_name").as_str())?
            .unwrap_or_default(),
        email: row
            .get::<_, Option<String>>(format!("{prefix}_email").as_str())?
            .unwrap_or_default(),
        ..User::default()
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c:\\path"), "c:\\\\path");
        assert_eq!(escape_like("plain"), "plain");
    }

    #[test]
    fn a_timestamp_round_trips_and_none_stays_none() {
        assert_eq!(stamp(None), None);
        let when = DateTime::parse_from_rfc3339("2026-08-24T14:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let text = stamp(Some(when)).unwrap();
        assert_eq!(
            DateTime::parse_from_rfc3339(&text)
                .unwrap()
                .with_timezone(&Utc),
            when
        );
    }
}
