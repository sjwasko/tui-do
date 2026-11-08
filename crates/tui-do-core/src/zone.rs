//! The timezone the interface reads the clock in.
//!
//! A date the user types is resolved in their zone, and a zone is more than today's UTC
//! offset: `due 2027-07-04` typed in January must be resolved at the offset in force on
//! 4 July, not the one in force in January. The runtime used to stamp `now` as a
//! `DateTime<FixedOffset>`, which froze today's offset and threw the zone's rules away,
//! so a date across a daylight-saving boundary landed an hour out — and because a due
//! date defaults to 23:59 and a start date to 00:00, an hour out is often a day out.
//! That was BUG-20.
//!
//! [`Zone`] carries the rules without a zone database. `Zone::Local` delegates to
//! [`chrono::Local`], which on unix reads `TZ` and `/etc/localtime` with chrono's own
//! parser and caches the result per thread, so it works in the static musl build and
//! costs a lookup rather than a read per call. `Zone::Fixed` is a bare offset, for tests
//! that must not depend on the machine they run on.
//!
//! Nothing here reads the clock. The runtime does, once per tick.

use std::fmt;

use chrono::{
    DateTime, FixedOffset, Local, MappedLocalTime, NaiveDate, NaiveDateTime, Offset, TimeZone,
};

/// The user's timezone: the machine's, or a fixed offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The machine's zone, with its daylight-saving rules. What the runtime uses.
    Local,
    /// A single offset with no rules, for tests and the smoke harness only.
    ///
    /// Production always uses [`Zone::Local`]. A fixed offset is exactly what BUG-20
    /// was: a date across a daylight-saving change resolved at the wrong offset.
    Fixed(FixedOffset),
}

/// An instant in the user's zone. What `Model.now` and every clock reading is.
pub type ZonedTime = DateTime<Zone>;

/// Pin an instant to its own fixed offset, for a `now` that does not depend on the
/// machine's zone.
///
/// For tests and the smoke harness only. Production always stamps its clock in
/// [`Zone::Local`]; see [`Zone::Fixed`] for why.
#[must_use]
pub fn pinned(at: DateTime<FixedOffset>) -> ZonedTime {
    at.with_timezone(&Zone::Fixed(*at.offset()))
}

/// The offset a [`Zone`] gives one instant, and the zone it came from.
///
/// `DateTime<Tz>` stores only `Tz::Offset` and rebuilds the zone from it with
/// [`TimeZone::from_offset`]. If the offset were a bare `FixedOffset`, a `DateTime<Zone>`
/// could not say whether it was `Local` or `Fixed`, and `now.timezone()` — which is how
/// every date the user types is resolved — would hand back a fixed offset again, which
/// is exactly the bug. So the variant travels with the offset.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ZoneOffset {
    zone: Zone,
    offset: FixedOffset,
}

impl Offset for ZoneOffset {
    fn fix(&self) -> FixedOffset {
        self.offset
    }
}

// Shown as the offset alone, the way `FixedOffset` shows itself, so a `Debug` of a
// `DateTime<Zone>` reads like any other `DateTime`.
impl fmt::Debug for ZoneOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.offset, f)
    }
}

impl fmt::Display for ZoneOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.offset, f)
    }
}

impl Zone {
    fn tag(self, offset: FixedOffset) -> ZoneOffset {
        ZoneOffset { zone: self, offset }
    }
}

impl TimeZone for Zone {
    type Offset = ZoneOffset;

    fn from_offset(offset: &ZoneOffset) -> Zone {
        offset.zone
    }

    fn offset_from_local_date(&self, local: &NaiveDate) -> MappedLocalTime<ZoneOffset> {
        self.offset_from_local_datetime(&local.and_time(chrono::NaiveTime::MIN))
    }

    fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> MappedLocalTime<ZoneOffset> {
        match *self {
            Zone::Local => Local.offset_from_local_datetime(local),
            Zone::Fixed(offset) => offset.offset_from_local_datetime(local),
        }
        .map(|offset| self.tag(offset))
    }

    fn offset_from_utc_date(&self, utc: &NaiveDate) -> ZoneOffset {
        self.offset_from_utc_datetime(&utc.and_time(chrono::NaiveTime::MIN))
    }

    fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> ZoneOffset {
        let offset = match *self {
            Zone::Local => Local.offset_from_utc_datetime(utc),
            Zone::Fixed(offset) => offset.fix(),
        };
        self.tag(offset)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn winter() -> DateTime<FixedOffset> {
        FixedOffset::west_opt(5 * 3600)
            .unwrap()
            .with_ymd_and_hms(2027, 1, 15, 10, 0, 0)
            .unwrap()
    }

    #[test]
    fn a_local_instant_still_knows_it_is_local() {
        // The whole reason `ZoneOffset` carries the variant: without it this would
        // come back as `Fixed`, and every date typed would be resolved without rules.
        let now = Utc::now().with_timezone(&Zone::Local);
        assert_eq!(now.timezone(), Zone::Local);
        assert_eq!((now + Duration::days(200)).timezone(), Zone::Local);
        assert_eq!(now.offset().fix(), Local::now().offset().fix());
    }

    #[test]
    fn a_pinned_instant_keeps_its_offset() {
        let now = pinned(winter());
        let fixed = FixedOffset::west_opt(5 * 3600).unwrap();
        assert_eq!(now.timezone(), Zone::Fixed(fixed));
        assert_eq!(now, winter());
        assert_eq!(now.to_rfc3339(), "2027-01-15T10:00:00-05:00");
        assert_eq!(format!("{now:?}"), format!("{:?}", winter()));
        // A fixed offset has no rules, so six months on it is still -05:00.
        assert_eq!((now + Duration::days(180)).offset().fix(), fixed);
    }
}
