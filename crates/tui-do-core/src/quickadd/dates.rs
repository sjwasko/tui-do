//! Resolving the date phrases Vikunja's quick-add magic documents.
//!
//! Two things are different from the parser this replaces.
//!
//! **Phrases are matched from a vocabulary, not scraped with a regex.** cria matches
//! `due\s+([^@+*!]+)`, which swallows everything up to the next magic character — so
//! "Call Bob due tomorrow about the invoice" hands "tomorrow about the invoice" to a
//! natural-language date parser and takes whatever comes back. Here a phrase is a
//! longest match against a known grammar, and it consumes exactly the words it matched.
//!
//! **Local time is converted, not relabelled.** cria builds a naive local date and calls
//! `.and_utc()` on it, which asserts that 17:00 in New York is 17:00 UTC. Every date it
//! produces is wrong by the machine's UTC offset. Everything here resolves against a
//! caller-supplied `now` in the caller's own timezone and converts properly — which is
//! also what makes these tests deterministic rather than dependent on where they run.

use std::ops::Range;

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};

use super::words::Word;

/// Which field a phrase was attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    /// A due date: `due friday`, or a bare date phrase.
    Due,
    /// A start date: `start monday` or `start:monday`.
    Start,
}

/// A date phrase found in the input.
#[derive(Debug, Clone)]
pub struct DateMatch {
    /// Which field it sets.
    pub field: DateField,

    /// The resolved instant.
    pub at: DateTime<Utc>,

    /// The byte range the whole phrase occupied, keyword included.
    pub range: Range<usize>,
}

/// When a date with no time of day means "due".
///
/// End of day: "due Friday" is satisfied any time on Friday, and a task that turns
/// overdue at midnight on the day it is due would be wrong every time.
const DUE_TIME: (u32, u32) = (23, 59);

/// When a date with no time of day means "start".
///
/// Start of day, for the mirror-image reason: work on a task starting Monday can begin
/// Monday morning.
const START_TIME: (u32, u32) = (0, 0);

/// Find every date phrase in `words`.
///
/// Later phrases for the same field win, so a user who retypes a date gets the one they
/// meant. Returns matches in input order.
pub fn find<Tz: TimeZone>(words: &[Word], now: &DateTime<Tz>) -> Vec<DateMatch> {
    let mut found = Vec::new();
    let mut index = 0;

    while index < words.len() {
        // An explicit keyword names the field and the phrase follows it.
        let (field, phrase_start) = match keyword_at(words, index) {
            Some((field, next)) => (field, next),
            None => (DateField::Due, index),
        };
        let explicit = phrase_start != index;

        if let Some((at, consumed)) = phrase(words, phrase_start, now, field) {
            let start = words[index].range.start;
            let end = words[phrase_start + consumed - 1].range.end;
            found.push(DateMatch {
                field,
                at,
                range: start..end,
            });
            index = phrase_start + consumed;
            continue;
        }

        // `due` with nothing recognisable after it is just a word in the title.
        index += if explicit { phrase_start - index } else { 1 };
    }

    found
}

/// Recognise `due`, `start`, or `start:` at `index`, returning where the phrase begins.
fn keyword_at(words: &[Word], index: usize) -> Option<(DateField, usize)> {
    let word = words.get(index)?;
    match word.trimmed() {
        "due" => Some((DateField::Due, index + 1)),
        "start" | "starts" | "starting" => Some((DateField::Start, index + 1)),
        // `start:monday` -- the colon form cria supports, with the value attached.
        other if other.starts_with("start:") && other.len() > "start:".len() => {
            Some((DateField::Start, index))
        }
        _ => None,
    }
}

/// Match the longest known date phrase beginning at `index`.
///
/// Returns the instant and how many words it consumed.
fn phrase<Tz: TimeZone>(
    words: &[Word],
    index: usize,
    now: &DateTime<Tz>,
    field: DateField,
) -> Option<(DateTime<Utc>, usize)> {
    let today = now.date_naive();

    // `start:monday` carries its value in the same word.
    if let Some(word) = words.get(index) {
        if let Some(value) = word.trimmed().strip_prefix("start:") {
            let date = simple_phrase(&[value.to_string()], today)?;
            return Some((at_time(date, field, words, index + 1, now)?, 1));
        }
    }

    // Try longest first so "next week" wins over "next" and "end of month" over "end".
    for length in (1..=4.min(words.len() - index)).rev() {
        let slice: Vec<String> = words[index..index + length]
            .iter()
            .map(|w| w.trimmed().to_string())
            .collect();
        if let Some(date) = simple_phrase(&slice, today) {
            let at = at_time(date, field, words, index + length, now)?;
            // A trailing `at 5pm` extends the phrase by two more words.
            let extra = usize::from(time_at(words, index + length).is_some()) * 2;
            return Some((at, length + extra));
        }
    }
    None
}

/// Apply a trailing `at <time>`, or the field's default time of day.
fn at_time<Tz: TimeZone>(
    date: NaiveDate,
    field: DateField,
    words: &[Word],
    after: usize,
    now: &DateTime<Tz>,
) -> Option<DateTime<Utc>> {
    let (hour, minute) = time_at(words, after).unwrap_or(match field {
        DateField::Due => DUE_TIME,
        DateField::Start => START_TIME,
    });
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    to_utc(date.and_time(time), now)
}

/// Recognise `at 5pm`, `at 17:00`, `at 5:30pm`.
fn time_at(words: &[Word], index: usize) -> Option<(u32, u32)> {
    if !words.get(index)?.is("at") {
        return None;
    }
    parse_clock(words.get(index + 1)?.trimmed())
}

/// Parse a bare clock time.
fn parse_clock(raw: &str) -> Option<(u32, u32)> {
    let (digits, suffix) = match raw.strip_suffix("am") {
        Some(rest) => (rest, Some(false)),
        None => match raw.strip_suffix("pm") {
            Some(rest) => (rest, Some(true)),
            None => (raw, None),
        },
    };
    let digits = digits.trim();

    let (hour, minute) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (digits.parse::<u32>().ok()?, 0),
    };
    if minute > 59 {
        return None;
    }

    let hour = match suffix {
        // 12am is midnight and 12pm is noon; every other hour just shifts by twelve.
        Some(true) if hour < 12 => hour + 12,
        Some(false) if hour == 12 => 0,
        Some(_) => hour,
        None => hour,
    };
    (hour < 24).then_some((hour, minute))
}

/// Resolve a date phrase given as already-lowercased words.
fn simple_phrase(words: &[String], today: NaiveDate) -> Option<NaiveDate> {
    let joined = words.join(" ");
    match joined.as_str() {
        "today" => return Some(today),
        "tomorrow" => return today.checked_add_days(Days::new(1)),
        "yesterday" => return today.checked_sub_days(Days::new(1)),
        "next week" => return today.checked_add_days(Days::new(7)),
        "this week" | "later this week" => return end_of_week(today),
        "last week" => return today.checked_sub_days(Days::new(7)),
        "later next week" => return today.checked_add_days(Days::new(7)).and_then(end_of_week),
        "next month" => return today.checked_add_months(Months::new(1)),
        "last month" => return today.checked_sub_months(Months::new(1)),
        "next year" => return today.checked_add_months(Months::new(12)),
        "this weekend" | "next weekend" => return next_weekday(today, chrono::Weekday::Sat, true),
        "eow" | "end of week" => return end_of_week(today),
        "eom" | "end of month" => return end_of_month(today),
        "eoy" | "end of year" => return NaiveDate::from_ymd_opt(today.year(), 12, 31),
        _ => {}
    }

    // `next monday`, `this friday`, `last tuesday`
    if words.len() == 2 {
        if let Some(weekday) = weekday(&words[1]) {
            return match words[0].as_str() {
                "next" => next_weekday(today, weekday, false),
                "this" => next_weekday(today, weekday, true),
                "last" => previous_weekday(today, weekday),
                _ => None,
            };
        }
    }

    // `in 3 days`, `in 2 weeks`
    if words.len() == 3 && words[0] == "in" {
        let amount: u64 = words[1].parse().ok()?;
        return match unit(&words[2])? {
            Unit::Day => today.checked_add_days(Days::new(amount)),
            Unit::Week => today.checked_add_days(Days::new(amount.checked_mul(7)?)),
            Unit::Month => today.checked_add_months(Months::new(u32::try_from(amount).ok()?)),
            Unit::Year => {
                today.checked_add_months(Months::new(u32::try_from(amount).ok()?.checked_mul(12)?))
            }
            // "in 3 hours" is a time, not a date; handled by the caller's default time.
            Unit::Hour => Some(today),
        };
    }

    if words.len() == 1 {
        // A bare weekday means the next one.
        if let Some(weekday) = weekday(&words[0]) {
            return next_weekday(today, weekday, false);
        }
        if let Some(date) = literal_date(&words[0], today) {
            return Some(date);
        }
    }

    // `feb 17`, `17 feb`, `february 17th`
    if words.len() == 2 {
        if let (Some(month), Some(day)) = (month(&words[0]), day_number(&words[1])) {
            return on_or_after(today, month, day);
        }
        if let (Some(day), Some(month)) = (day_number(&words[0]), month(&words[1])) {
            return on_or_after(today, month, day);
        }
    }

    None
}

/// Units accepted after `in` and `every`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Hours.
    Hour,
    /// Days.
    Day,
    /// Weeks.
    Week,
    /// Months.
    Month,
    /// Years.
    Year,
}

impl Unit {
    /// How many seconds one of these is, for Vikunja's `repeat_after`.
    ///
    /// Months are the approximation Vikunja itself uses when a repeat is expressed in
    /// seconds; `repeat_mode: monthly` is the exact form and is what a monthly repeat
    /// should actually use.
    #[must_use]
    pub fn seconds(self) -> i64 {
        match self {
            Self::Hour => 3_600,
            Self::Day => 86_400,
            Self::Week => 604_800,
            Self::Month => 2_592_000,
            Self::Year => 31_536_000,
        }
    }
}

/// Recognise a unit word, singular or plural.
#[must_use]
pub fn unit(word: &str) -> Option<Unit> {
    match word {
        "hour" | "hours" | "hourly" => Some(Unit::Hour),
        "day" | "days" | "daily" => Some(Unit::Day),
        "week" | "weeks" | "weekly" => Some(Unit::Week),
        "month" | "months" | "monthly" => Some(Unit::Month),
        "year" | "years" | "yearly" | "annually" => Some(Unit::Year),
        _ => None,
    }
}

/// Recognise a weekday name or its three-letter abbreviation.
fn weekday(word: &str) -> Option<chrono::Weekday> {
    use chrono::Weekday::{Fri, Mon, Sat, Sun, Thu, Tue, Wed};
    match word {
        "monday" | "mon" => Some(Mon),
        "tuesday" | "tue" | "tues" => Some(Tue),
        "wednesday" | "wed" => Some(Wed),
        "thursday" | "thu" | "thur" | "thurs" => Some(Thu),
        "friday" | "fri" => Some(Fri),
        "saturday" | "sat" => Some(Sat),
        "sunday" | "sun" => Some(Sun),
        _ => None,
    }
}

/// Recognise a month name or its three-letter abbreviation.
fn month(word: &str) -> Option<u32> {
    let short: String = word.chars().take(3).collect();
    match short.as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

/// Parse `17` or `17th`.
fn day_number(word: &str) -> Option<u32> {
    let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &word[digits.len()..];
    if !matches!(rest, "" | "st" | "nd" | "rd" | "th") {
        return None;
    }
    let day = digits.parse().ok()?;
    (1..=31).contains(&day).then_some(day)
}

/// Parse a written-out date.
///
/// `YYYY-MM-DD` and `DD.MM.YYYY` are unambiguous. **`DD/MM/YYYY` is read day-first**,
/// following Vikunja's own documentation, which gives `17/02/2021` as 17 February — not
/// the American reading. cria used a US dialect here, so the same input meant different
/// days in the two clients.
fn literal_date(word: &str, today: NaiveDate) -> Option<NaiveDate> {
    if let Ok(date) = NaiveDate::parse_from_str(word, "%Y-%m-%d") {
        return Some(date);
    }
    for format in ["%d.%m.%Y", "%d/%m/%Y", "%d-%m-%Y"] {
        if let Ok(date) = NaiveDate::parse_from_str(word, format) {
            return Some(date);
        }
    }
    // Year-less forms resolve within the coming twelve months.
    for format in ["%d.%m.", "%d/%m"] {
        if let Ok(parsed) = NaiveDate::parse_from_str(word, format) {
            return on_or_after(today, parsed.month(), parsed.day());
        }
    }
    None
}

/// The next occurrence of `month`/`day`, this year or next.
fn on_or_after(today: NaiveDate, month: u32, day: u32) -> Option<NaiveDate> {
    let this_year = NaiveDate::from_ymd_opt(today.year(), month, day)?;
    if this_year >= today {
        Some(this_year)
    } else {
        NaiveDate::from_ymd_opt(today.year() + 1, month, day)
    }
}

/// The coming `target` weekday. `include_today` decides whether today counts.
fn next_weekday(
    today: NaiveDate,
    target: chrono::Weekday,
    include_today: bool,
) -> Option<NaiveDate> {
    let current = today.weekday().num_days_from_monday();
    let wanted = target.num_days_from_monday();
    let mut ahead = (wanted + 7 - current) % 7;
    if ahead == 0 && !include_today {
        ahead = 7;
    }
    today.checked_add_days(Days::new(u64::from(ahead)))
}

/// The most recent `target` weekday before today.
fn previous_weekday(today: NaiveDate, target: chrono::Weekday) -> Option<NaiveDate> {
    let current = today.weekday().num_days_from_monday();
    let wanted = target.num_days_from_monday();
    let mut behind = (current + 7 - wanted) % 7;
    if behind == 0 {
        behind = 7;
    }
    today.checked_sub_days(Days::new(u64::from(behind)))
}

/// The Sunday ending the current week.
fn end_of_week(today: NaiveDate) -> Option<NaiveDate> {
    next_weekday(today, chrono::Weekday::Sun, true)
}

/// The last day of the current month.
fn end_of_month(today: NaiveDate) -> Option<NaiveDate> {
    let first_next = today.with_day(1)?.checked_add_months(Months::new(1))?;
    first_next.checked_sub_days(Days::new(1))
}

/// Interpret a naive local datetime in the caller's timezone and convert to UTC.
///
/// The step cria skips. During a daylight-saving spring-forward the wall clock time may
/// not exist; the later reading is the useful one, since it is the first real instant at
/// or after what the user asked for.
fn to_utc<Tz: TimeZone>(naive: chrono::NaiveDateTime, now: &DateTime<Tz>) -> Option<DateTime<Utc>> {
    let zone = now.timezone();
    match zone.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earlier, _) => Some(earlier.with_timezone(&Utc)),
        chrono::LocalResult::None => {
            // The hour was skipped. Step forward until a real instant appears.
            for extra in 1..=4 {
                let shifted = naive.with_hour((naive.hour() + extra) % 24)?;
                if let chrono::LocalResult::Single(dt) = zone.from_local_datetime(&shifted) {
                    return Some(dt.with_timezone(&Utc));
                }
            }
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::quickadd::words::split;
    use chrono::FixedOffset;

    /// A fixed "now" so these tests do not depend on the clock or the machine's zone.
    ///
    /// Wednesday 2026-08-26, 10:00, in a zone five hours behind UTC.
    fn now() -> DateTime<FixedOffset> {
        FixedOffset::west_opt(5 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 8, 26, 10, 0, 0)
            .unwrap()
    }

    fn resolve(input: &str) -> Option<DateMatch> {
        find(&split(input), &now()).into_iter().next()
    }

    /// The local wall-clock rendering of a match, which is what the user meant.
    fn local(input: &str) -> String {
        let matched = resolve(input).expect("a date should be found in {input}");
        matched
            .at
            .with_timezone(&now().timezone())
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }

    #[test]
    fn a_local_date_is_converted_not_relabelled() {
        // The bug in cria: it produces 2026-08-26T23:59Z, which is 18:59 local -- the
        // task turns overdue five hours early, every day.
        let matched = resolve("due today").unwrap();
        assert_eq!(matched.at.to_rfc3339(), "2026-08-27T04:59:00+00:00");
        assert_eq!(local("due today"), "2026-08-26 23:59");
    }

    #[test]
    fn today_and_tomorrow() {
        assert_eq!(local("due today"), "2026-08-26 23:59");
        assert_eq!(local("due tomorrow"), "2026-08-27 23:59");
        assert_eq!(local("due yesterday"), "2026-08-25 23:59");
    }

    #[test]
    fn a_due_date_ends_the_day_and_a_start_date_begins_it() {
        // "due Friday" is satisfied any time on Friday; work starting Friday can begin
        // Friday morning.
        assert_eq!(local("due friday"), "2026-08-28 23:59");
        assert_eq!(local("start friday"), "2026-08-28 00:00");
    }

    #[test]
    fn a_bare_weekday_means_the_next_one_not_today() {
        // now() is a Wednesday.
        assert_eq!(local("wednesday"), "2026-09-02 23:59");
        assert_eq!(local("this wednesday"), "2026-08-26 23:59");
        assert_eq!(local("next monday"), "2026-08-31 23:59");
        assert_eq!(local("last monday"), "2026-08-24 23:59");
    }

    #[test]
    fn relative_periods() {
        assert_eq!(local("next week"), "2026-09-02 23:59");
        assert_eq!(local("in 3 days"), "2026-08-29 23:59");
        assert_eq!(local("in 2 weeks"), "2026-09-09 23:59");
        assert_eq!(local("next month"), "2026-09-26 23:59");
    }

    #[test]
    fn end_of_period_shorthands() {
        // Week ends Sunday.
        assert_eq!(local("eow"), "2026-08-30 23:59");
        assert_eq!(local("end of week"), "2026-08-30 23:59");
        assert_eq!(local("eom"), "2026-08-31 23:59");
        assert_eq!(local("end of month"), "2026-08-31 23:59");
        assert_eq!(local("end of year"), "2026-12-31 23:59");
    }

    #[test]
    fn a_time_of_day_overrides_the_default() {
        assert_eq!(local("due today at 5pm"), "2026-08-26 17:00");
        assert_eq!(local("due today at 17:30"), "2026-08-26 17:30");
        assert_eq!(local("due tomorrow at 9am"), "2026-08-27 09:00");
        assert_eq!(local("due today at 12am"), "2026-08-26 00:00");
        assert_eq!(local("due today at 12pm"), "2026-08-26 12:00");
    }

    #[test]
    fn written_dates_are_day_first_as_vikunja_documents() {
        // 17/02 is 17 February, not 2 May. cria read this the American way, so the same
        // input meant different days in the two clients.
        assert_eq!(local("due 17/02/2027"), "2027-02-17 23:59");
        assert_eq!(local("due 17.02.2027"), "2027-02-17 23:59");
        assert_eq!(local("due 2027-02-17"), "2027-02-17 23:59");
    }

    #[test]
    fn month_names_resolve_forward() {
        // February has passed in 2026, so it means 2027.
        assert_eq!(local("due feb 17"), "2027-02-17 23:59");
        assert_eq!(local("due 17 feb"), "2027-02-17 23:59");
        assert_eq!(local("due december 25th"), "2026-12-25 23:59");
    }

    #[test]
    fn the_phrase_consumes_only_what_it_matched() {
        // cria's regex takes everything to the next magic character, so the rest of the
        // sentence ends up inside the date expression.
        let input = "Call Bob due tomorrow about the invoice";
        let matched = resolve(input).unwrap();
        assert_eq!(&input[matched.range], "due tomorrow");
    }

    #[test]
    fn a_trailing_time_is_part_of_the_phrase() {
        let input = "Standup due tomorrow at 9am sharp";
        let matched = resolve(input).unwrap();
        assert_eq!(&input[matched.range], "due tomorrow at 9am");
    }

    #[test]
    fn start_and_due_are_both_found() {
        let matches = find(&split("Ship it start monday due friday"), &now());
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].field, DateField::Start);
        assert_eq!(matches[1].field, DateField::Due);
    }

    #[test]
    fn the_colon_form_of_start_works() {
        let matches = find(&split("Ship it start:eom"), &now());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].field, DateField::Start);
        assert_eq!(
            matches[0]
                .at
                .with_timezone(&now().timezone())
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "2026-08-31 00:00"
        );
    }

    #[test]
    fn a_word_that_is_not_a_date_is_left_alone() {
        assert!(resolve("Buy milk").is_none());
        assert!(resolve("Read the manual").is_none());
    }

    #[test]
    fn due_followed_by_nothing_recognisable_is_just_a_word() {
        // "Pay the dues" must not become a dateless task with a mangled title.
        assert!(resolve("Pay the dues next door").is_none());
    }

    #[test]
    fn a_nonsense_time_does_not_produce_a_date() {
        assert_eq!(parse_clock("25"), None);
        assert_eq!(parse_clock("5:99"), None);
        assert_eq!(parse_clock("abc"), None);
        assert_eq!(parse_clock("5pm"), Some((17, 0)));
    }

    #[test]
    fn end_of_month_handles_february() {
        let feb = NaiveDate::from_ymd_opt(2028, 2, 10).unwrap();
        assert_eq!(
            end_of_month(feb),
            NaiveDate::from_ymd_opt(2028, 2, 29),
            "2028 is a leap year"
        );
        let dec = NaiveDate::from_ymd_opt(2026, 12, 10).unwrap();
        assert_eq!(end_of_month(dec), NaiveDate::from_ymd_opt(2026, 12, 31));
    }

    #[test]
    fn units_carry_their_length() {
        assert_eq!(unit("days"), Some(Unit::Day));
        assert_eq!(unit("weekly"), Some(Unit::Week));
        assert_eq!(unit("fortnight"), None);
        assert_eq!(Unit::Week.seconds(), 604_800);
    }
}
