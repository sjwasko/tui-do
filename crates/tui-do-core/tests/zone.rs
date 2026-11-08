//! `Zone::Local` applies the machine's daylight-saving rules: BUG-20, end to end.
//!
//! The zone comes from `TZ`, which a test cannot set for itself without `unsafe` and
//! without racing every other test in the process. So the test re-runs this binary with
//! `TZ` set and asserts in the child. The zone is a POSIX rule string rather than an IANA
//! name, so it needs no zoneinfo files and runs the same in a bare container — which is
//! also the musl build's situation, since chrono parses both itself.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use chrono::{TimeZone, Utc};
use tui_do_core::quickadd::parse;
use tui_do_core::Zone;

/// New York's rules as a POSIX `TZ`: EST/EDT, second Sunday of March to first of November.
const EASTERN: &str = "EST5EDT,M3.2.0,M11.1.0";

const CHILD: &str = "TUI_DO_ZONE_TEST_CHILD";

#[test]
fn a_local_zone_resolves_a_date_at_the_offset_in_force_on_it() {
    if std::env::var_os(CHILD).is_some() {
        in_eastern();
        return;
    }

    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_local_zone_resolves_a_date_at_the_offset_in_force_on_it",
            "--test-threads=1",
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
    // A filter that matched nothing would also exit 0.
    assert!(
        stdout.contains("1 passed"),
        "the child ran nothing:\n{stdout}"
    );
}

/// Runs only in the child, where `TZ` is Eastern.
fn in_eastern() {
    // Typed in January, at EST.
    let now = Utc
        .with_ymd_and_hms(2027, 1, 15, 15, 0, 0)
        .unwrap()
        .with_timezone(&Zone::Local);
    assert_eq!(now.to_rfc3339(), "2027-01-15T10:00:00-05:00");

    // Resolved at EDT, the offset in force on the 4th of July. With January's offset
    // frozen this was 2027-07-05T04:59Z: 00:59 on the 5th, a day late.
    let parsed = parse("Fireworks due 2027-07-04", &now);
    let due = parsed.due_date.expect("a due date");
    assert_eq!(due.to_rfc3339(), "2027-07-05T03:59:00+00:00");

    // And read back in the same zone it is the 4th, at -04:00.
    assert_eq!(
        due.with_timezone(&Zone::Local).to_rfc3339(),
        "2027-07-04T23:59:00-04:00"
    );
}
