//! `Zone::Local` applies the machine's daylight-saving rules: BUG-20, end to end.
//!
//! This is the test of the *wiring*. `quickadd::dates`'s own `across_dst` tests hand the
//! parser a `chrono_tz` zone directly, and the parser was always generic over the zone,
//! so they passed before the fix too. What BUG-20 got wrong was the clock the runtime
//! passed in: `Local::now().fixed_offset()`. Every case here builds its `now` the way
//! `runtime::now()` does — an instant converted with `.with_timezone(&Zone::Local)` — and
//! each fails if that conversion is reduced to a fixed offset.
//!
//! The zone comes from `TZ`, which a test cannot set for itself without `unsafe` and
//! without racing every other test in the process. So the test re-runs this binary with
//! `TZ` set and asserts in the child. The zone is a POSIX rule string rather than an IANA
//! name, so it needs no zoneinfo files and runs the same in a bare container — which is
//! also the musl build's situation, since chrono parses both itself.
// `print_stdout` for the one line the child prints to say it finished; see `FINISHED`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::process::Command;

use chrono::{DateTime, TimeZone, Utc};
use tui_do_core::quickadd::parse;
use tui_do_core::{Zone, ZonedTime};

/// New York's rules as a POSIX `TZ`: EST/EDT, second Sunday of March to first of November,
/// changing at 02:00 local.
const EASTERN: &str = "EST5EDT,M3.2.0,M11.1.0";

const CHILD: &str = "TUI_DO_ZONE_TEST_CHILD";

/// Printed by the child as its last act, so the parent knows every case ran. A filter
/// that matched nothing exits 0 too, and this cannot be printed by anything but a child
/// that got to the end of `in_eastern`.
const FINISHED: &str = "zone child: every case checked";

#[test]
fn a_local_zone_resolves_a_date_at_the_offset_in_force_on_it() {
    if std::env::var_os(CHILD).is_some() {
        in_eastern();
        println!("{FINISHED}");
        return;
    }

    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_local_zone_resolves_a_date_at_the_offset_in_force_on_it",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("TZ", EASTERN)
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(FINISHED),
        "the child did not run the cases:\n{stdout}"
    );
}

/// The clock as `runtime::now()` builds it, at a pinned instant so no case depends on
/// today's date.
fn clock(utc: DateTime<Utc>) -> ZonedTime {
    utc.with_timezone(&Zone::Local)
}

/// Friday 2027-01-15, 10:00 EST.
fn in_winter() -> ZonedTime {
    clock(Utc.with_ymd_and_hms(2027, 1, 15, 15, 0, 0).unwrap())
}

/// Wednesday 2026-08-26, 10:00 EDT.
fn in_summer() -> ZonedTime {
    clock(Utc.with_ymd_and_hms(2026, 8, 26, 14, 0, 0).unwrap())
}

/// What the local wall clock reads at the instant `input` resolved to, with the offset
/// in force at that instant.
fn shown(input: &str, now: &ZonedTime) -> String {
    let parsed = parse(input, now);
    let at = parsed
        .due_date
        .or(parsed.start_date)
        .unwrap_or_else(|| panic!("a date should be found in {input:?}"));
    at.with_timezone(&Zone::Local)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string()
}

/// Runs only in the child, where `TZ` is Eastern.
fn in_eastern() {
    // The clock itself must carry the zone's offset for its own instant.
    assert_eq!(in_winter().to_rfc3339(), "2027-01-15T10:00:00-05:00");
    assert_eq!(in_summer().to_rfc3339(), "2026-08-26T10:00:00-04:00");

    // (typed when, input, what the wall clock must read, what a frozen offset gave)
    let cases = [
        // The four rows of the table in bugs.md.
        (
            in_winter(),
            "Fireworks due 2027-07-04",
            "2027-07-04 23:59 -04:00",
            "2027-07-05 00:59, a day late",
        ),
        (
            in_summer(),
            "Taxes due 2027-02-24",
            "2027-02-24 23:59 -05:00",
            "2027-02-24 22:59, an hour early",
        ),
        (
            in_winter(),
            "Fireworks start 2027-07-04",
            "2027-07-04 00:00 -04:00",
            "2027-07-04 01:00, an hour late",
        ),
        (
            in_summer(),
            "Taxes start 2027-02-24",
            "2027-02-24 00:00 -05:00",
            "2027-02-23 23:00, a day early",
        ),
        // 2027-03-14 02:30 never happens: 02:00 EST jumps to 03:00 EDT, and the first
        // real instant after it is 03:30.
        (
            in_summer(),
            "Call due 2027-03-14 at 2:30am",
            "2027-03-14 03:30 -04:00",
            "2027-03-14 01:30 -05:00",
        ),
        // 2027-11-07 01:30 happens twice, at EDT then EST; the earlier reading wins.
        (
            in_winter(),
            "Call due 2027-11-07 at 1:30am",
            "2027-11-07 01:30 -04:00",
            "2027-11-07 01:30 -05:00, the later one",
        ),
    ];

    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|(now, input, wanted, frozen)| {
            let got = shown(input, now);
            (got != *wanted).then(|| {
                format!("{input:?}: got {got}, wanted {wanted} (a frozen offset gives {frozen})")
            })
        })
        .collect();
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
