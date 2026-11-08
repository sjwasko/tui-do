//! The fixed clock the unit tests in this crate share.
//!
//! Pinned to an instant and to `Zone::Fixed`, so no test depends on today's date or on
//! the zone of the machine it runs on.
#![allow(clippy::unwrap_used)]

use chrono::{DateTime, TimeZone, Utc};
use tui_do_core::ZonedTime;

/// Monday 2026-08-24, 12:00 UTC.
pub(crate) fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
}

/// The same instant as the reader's clock shows it, at a zero offset. A test that needs a
/// real offset pins its own with `tui_do_core::zone::pinned`.
pub(crate) fn here() -> ZonedTime {
    tui_do_core::zone::pinned(now().fixed_offset())
}
