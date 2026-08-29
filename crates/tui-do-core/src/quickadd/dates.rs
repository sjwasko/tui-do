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

    // The spaced form of the same date: `27 aug 2026`, `august 27 2026`. Placed after
    // `in 3 days` so it cannot steal that phrase.
    if words.len() == 3 {
        if let Some(date) = three_part_date(&words[0], &words[1], &words[2]) {
            return Some(date);
        }
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
        if let (Some(month), Some(day)) = (month_name(&words[0]), day_number(&words[1])) {
            return on_or_after(today, month, day);
        }
        if let (Some(day), Some(month)) = (day_number(&words[0]), month_name(&words[1])) {
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

// `month` used to live here: it took a word's first three letters and read them as a
// month, on the reasoning that `feb 17` had already been split into words so a prefix was
// safe. It was not. A word is a word, and the prefix match made a date out of the first
// one in "Decide 3 things to do" -- `dec` plus a bare number -- which both moved the due
// date to 3 December and ate "Decide" out of the title. `month_name` is whole-word and
// covers the same spellings plus `sept`, so it is the only reading of a month there is.

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
    let parts = date_parts(word)?;
    match parts.len() {
        3 => three_part_date(&parts[0], &parts[1], &parts[2]),
        2 => two_part_date(&parts[0], &parts[1], today),
        _ => None,
    }
}

/// Split a written date into its parts.
///
/// On `/`, `-` or `.`, and — when there is no separator at all — on each boundary between
/// digits and letters, which is what makes the military `27aug26` one token rather than
/// three. A part that is neither all digits nor all letters is not part of a date, and a
/// single part is not a date at all: a bare `2026` is a number in a title.
fn date_parts(word: &str) -> Option<Vec<String>> {
    let word = word.trim_matches(['.', '/', '-']);
    if word.is_empty() {
        return None;
    }

    let parts: Vec<String> = if word.contains(['/', '-', '.']) {
        word.split(['/', '-', '.']).map(str::to_string).collect()
    } else {
        let mut parts: Vec<String> = Vec::new();
        let mut run_is_digit: Option<bool> = None;
        for c in word.chars() {
            let is_digit = if c.is_ascii_digit() {
                true
            } else if c.is_ascii_alphabetic() {
                false
            } else {
                return None;
            };
            match parts.last_mut() {
                Some(last) if run_is_digit == Some(is_digit) => last.push(c),
                _ => {
                    parts.push(c.to_string());
                    run_is_digit = Some(is_digit);
                }
            }
        }
        parts
    };

    (parts.len() >= 2 && parts.len() <= 3 && parts.iter().all(|part| !part.is_empty()))
        .then_some(parts)
}

/// A date carrying its year: `27/08/26`, `8/27/2026`, `2026-08-27`, `27aug26`, `aug-27-26`.
///
/// Read day-first, then month-first, then year-first, and the first reading that names a
/// real day wins. That order is what lets `27/08/26` and `8/27/26` both mean 27 August
/// without either having to be declared the house style — only one of them can be read
/// day-first, so the other falls through to the next reading on its own.
///
/// Day-first leads because it is what Vikunja documents and what the web UI shows, so a
/// date that is genuinely ambiguous — `08/09/26`, where both numbers could be either —
/// is 8 September in both clients rather than one day here and another there. The cost is
/// that `YY/MM/DD` is only reached when the two readings ahead of it are impossible:
/// `26/08/27` is 26 August 2027, not 27 August 2026.
fn three_part_date(a: &str, b: &str, c: &str) -> Option<NaiveDate> {
    // A month by name pins itself. The two numbers around it can then only be the day and
    // the year, so there is nothing left to guess.
    if let Some(month) = month_name(b) {
        return ymd(year_of(c), Some(month), number(a))
            .or_else(|| ymd(year_of(a), Some(month), number(c)));
    }
    if let Some(month) = month_name(a) {
        return ymd(year_of(c), Some(month), number(b));
    }
    if !is_number(a) || !is_number(b) || !is_number(c) {
        return None;
    }
    // A four-digit year can only be a year, so `2026/08/27` needs no guessing at all.
    if a.len() == 4 {
        return ymd(year_of(a), number(b), number(c));
    }
    ymd(year_of(c), number(b), number(a))
        .or_else(|| ymd(year_of(c), number(a), number(b)))
        .or_else(|| ymd(year_of(a), number(b), number(c)))
}

/// A date with no year: `27/08`, `8/27`, `27aug`, `aug27`.
///
/// Resolved into the coming twelve months, the same as the spaced `feb 17`.
fn two_part_date(a: &str, b: &str, today: NaiveDate) -> Option<NaiveDate> {
    if let Some(month) = month_name(a) {
        return on_or_after(today, month, number(b)?);
    }
    if let Some(month) = month_name(b) {
        return on_or_after(today, month, number(a)?);
    }
    if !is_number(a) || !is_number(b) {
        return None;
    }
    // Day-first, then month-first, for the reason `three_part_date` explains.
    on_or_after(today, number(b)?, number(a)?)
        .or_else(|| on_or_after(today, number(a)?, number(b)?))
}

/// Whether every character is a digit.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

/// A written number, of any width.
fn number(text: &str) -> Option<u32> {
    text.parse().ok()
}

/// A written year.
///
/// Two digits pivot where `strftime` puts them: `00`–`68` are this century, `69`–`99` the
/// last. A three-digit year is nobody's shorthand and is refused.
fn year_of(text: &str) -> Option<i32> {
    let value: i32 = text.parse().ok()?;
    match text.len() {
        4 => Some(value),
        1 | 2 => Some(if value <= 68 {
            2000 + value
        } else {
            1900 + value
        }),
        _ => None,
    }
}

/// Assemble a date from parts that may each have failed to parse.
///
/// Takes `Option`s so a reading can be tried and discarded without the `?` on one part
/// abandoning the readings that come after it.
fn ymd(year: Option<i32>, month: Option<u32>, day: Option<u32>) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year?, month?, day?)
}

/// A month written out: `jan` through `dec`, `sept`, or the full name.
///
/// Whole-word, unlike [`month`], which matches on the first three letters because the
/// spaced `feb 17` form has already been split into words by then. Here the part comes
/// out of a run of letters that was never separated from anything, so `sep` inside
/// `separate` would otherwise make a date out of a word.
fn month_name(text: &str) -> Option<u32> {
    const MONTHS: [(&str, &str, u32); 12] = [
        ("jan", "january", 1),
        ("feb", "february", 2),
        ("mar", "march", 3),
        ("apr", "april", 4),
        ("may", "may", 5),
        ("jun", "june", 6),
        ("jul", "july", 7),
        ("aug", "august", 8),
        ("sep", "september", 9),
        ("oct", "october", 10),
        ("nov", "november", 11),
        ("dec", "december", 12),
    ];
    MONTHS.iter().find_map(|(short, full, number)| {
        (text == *short || text == *full || (*number == 9 && text == "sept")).then_some(*number)
    })
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
    fn a_date_only_one_reading_can_explain_is_read_that_way() {
        // The pair the user asked for: American and European spellings of 27 August 2026.
        // Neither has to be declared the house style -- `8/27/26` cannot be read
        // day-first, so it falls through to month-first on its own.
        assert_eq!(local("due 27/08/26"), "2026-08-27 23:59");
        assert_eq!(local("due 8/27/26"), "2026-08-27 23:59");
        assert_eq!(local("due 12/24/2026"), "2026-12-24 23:59");
        assert_eq!(local("due 24/12/2026"), "2026-12-24 23:59");
    }

    #[test]
    fn an_ambiguous_date_is_day_first_the_way_vikunja_reads_it() {
        // Both numbers could be either, so the tie has to go somewhere. It goes to the
        // reading the web UI would show, or the same input would mean two different days
        // in the two clients.
        assert_eq!(local("due 08/09/26"), "2026-09-08 23:59");
        assert_eq!(local("due 8/9/2026"), "2026-09-08 23:59");
    }

    #[test]
    fn a_four_digit_year_leads_and_needs_no_guessing() {
        assert_eq!(local("due 2027-02-17"), "2027-02-17 23:59");
        assert_eq!(local("due 2027/02/17"), "2027-02-17 23:59");
        assert_eq!(local("due 2027.02.17"), "2027-02-17 23:59");
    }

    #[test]
    fn the_military_form_needs_no_separators() {
        assert_eq!(local("due 27aug26"), "2026-08-27 23:59");
        assert_eq!(local("due 27Aug2026"), "2026-08-27 23:59");
        assert_eq!(local("due 27-aug-26"), "2026-08-27 23:59");
        assert_eq!(local("due aug-27-26"), "2026-08-27 23:59");
        // Day-first here too: `26aug27` is the 26th in 2027, not the 27th in 2026.
        assert_eq!(local("due 26aug27"), "2027-08-26 23:59");
    }

    #[test]
    fn a_month_can_be_spelled_out_at_any_length() {
        assert_eq!(local("due 27/september/2026"), "2026-09-27 23:59");
        assert_eq!(local("due 27sept26"), "2026-09-27 23:59");
        assert_eq!(local("due 27 august 2026"), "2026-08-27 23:59");
        assert_eq!(local("due august 27 2026"), "2026-08-27 23:59");
    }

    #[test]
    fn a_single_digit_day_or_month_is_as_good_as_two() {
        assert_eq!(local("due 3/9/2026"), "2026-09-03 23:59");
        assert_eq!(local("due 03/09/2026"), "2026-09-03 23:59");
        assert_eq!(local("due 3sep26"), "2026-09-03 23:59");
    }

    #[test]
    fn a_two_digit_year_pivots_where_strftime_puts_it() {
        assert_eq!(local("due 27/08/68"), "2068-08-27 23:59");
        assert_eq!(local("due 27/08/69"), "1969-08-27 23:59");
    }

    #[test]
    fn a_year_less_date_still_resolves_forward() {
        // February has passed in 2026, so it means 2027 -- the same rule `feb 17` follows.
        assert_eq!(local("due 17/02"), "2027-02-17 23:59");
        assert_eq!(local("due 2/17"), "2027-02-17 23:59");
        assert_eq!(local("due 17feb"), "2027-02-17 23:59");
    }

    #[test]
    fn a_word_that_merely_looks_like_a_date_is_left_in_the_title() {
        // Every one of these reaches `literal_date`, and none of them may come back a
        // date: splitting on letter/digit runs is what makes `p3` and `v1.2.3` reachable
        // at all, and a false positive here silently changes what the user typed.
        for word in [
            "p3", "v1.2.3", "covid-19", "separate", "3rd", "2026", "13/13/26",
        ] {
            assert!(
                literal_date(word, now().date_naive()).is_none(),
                "{word} was read as a date"
            );
        }
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
