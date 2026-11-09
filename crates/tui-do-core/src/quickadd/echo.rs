//! Saying back what a quick-add line will create, and how it is best written.
//!
//! Two views of one [`Parsed`], both pure. [`Parsed::summary`] names every field that was
//! set, for a person or an agent to check before anything is queued. [`Parsed::canonical`]
//! rewrites the line in the recommended form, which parses back to the same task: the
//! title first, then `+project`, `*label`s, `@user`s, `!N`, `due YYYY-MM-DD`,
//! `start YYYY-MM-DD` and the repeat. Dates go last, after their keyword, because a date
//! leaves the title only from the end of the line (BUG-29), and in ISO form with the
//! keyword, which no reading of BUG-24's rules can take for a number.
//!
//! Dates are written in the zone the line was parsed in, so a date across a
//! daylight-saving change reads back on the day it was typed for (BUG-20).

use std::fmt::Display;

use chrono::{DateTime, TimeZone, Timelike, Utc};

use super::{unquote, DateField, Parsed, Repeat, Unit};

/// Which of the two ways a date is written.
#[derive(Clone, Copy)]
enum Wording {
    /// For reading: `due Fri 27 Aug 2027`, `starts Mon 24 Aug 2026 at 09:00`.
    Prose,
    /// For typing back in: `due 2027-08-27`, `start 2026-08-24 at 09:00`. ISO, after its
    /// keyword, which parses anywhere in a line.
    Line,
}

impl DateField {
    /// One date in words, as `tui-do add` names it: `due Fri 27 Aug 2027`, or
    /// `starts Mon 24 Aug 2026 at 09:00`.
    ///
    /// The time of day is shown only when it is not the field's [`default_time`], the
    /// same one the parser fills in, so a plain date stays short.
    ///
    /// [`default_time`]: DateField::default_time
    #[must_use]
    pub fn describe<Tz: TimeZone>(self, at: DateTime<Utc>, zone: &Tz) -> String
    where
        Tz::Offset: Display,
    {
        self.write(at, zone, Wording::Prose)
    }

    /// One date in either wording. The two differ only in the keyword and the day's
    /// format; the time rule is shared, so the summary and the canonical line can never
    /// disagree about whether a time was set.
    fn write<Tz: TimeZone>(self, at: DateTime<Utc>, zone: &Tz, wording: Wording) -> String
    where
        Tz::Offset: Display,
    {
        let (word, day) = match (wording, self) {
            (Wording::Prose, Self::Due) => ("due", "%a %-d %b %Y"),
            (Wording::Prose, Self::Start) => ("starts", "%a %-d %b %Y"),
            (Wording::Line, Self::Due) => ("due", "%Y-%m-%d"),
            (Wording::Line, Self::Start) => ("start", "%Y-%m-%d"),
        };
        let local = at.with_timezone(zone);
        let day = local.format(day);
        if (local.hour(), local.minute()) == self.default_time() {
            format!("{word} {day}")
        } else {
            format!("{word} {day} at {}", local.format("%H:%M"))
        }
    }
}

impl Repeat {
    /// `every week`, `every 2 weeks` -- the phrase the parser reads it back from.
    fn phrase(self) -> String {
        let unit = match self.unit {
            Unit::Hour => "hour",
            Unit::Day => "day",
            Unit::Week => "week",
            Unit::Month => "month",
            Unit::Year => "year",
        };
        if self.every == 1 {
            format!("every {unit}")
        } else {
            format!("every {} {unit}s", self.every)
        }
    }
}

impl Parsed {
    /// One line naming exactly what this will create, every set field and nothing else:
    ///
    /// ```text
    /// title "Call the dentist" · +Personal · *health · @admin · !3 · due Thu 12 Nov 2026 · every week
    /// ```
    ///
    /// `zone` is the zone the line was parsed in -- `now.timezone()` of the clock that
    /// was handed to [`parse`](super::parse). Names are as typed; whether they match a
    /// project or label in the store is the caller's question, since this reads no store.
    #[must_use]
    pub fn summary<Tz: TimeZone>(&self, zone: &Tz) -> String
    where
        Tz::Offset: Display,
    {
        let mut parts = Vec::new();
        if !self.title.is_empty() {
            parts.push(format!("title \"{}\"", self.title));
        }
        parts.extend(self.names());
        if let Some(at) = self.due_date {
            parts.push(DateField::Due.describe(at, zone));
        }
        if let Some(at) = self.start_date {
            parts.push(DateField::Start.describe(at, zone));
        }
        if let Some(repeat) = self.repeat {
            parts.push(repeat.phrase());
        }
        parts.join(" · ")
    }

    /// The line rewritten in the recommended form, which parses back to the same task.
    ///
    /// Title, `+project`, `*label`s, `@user`s, `!N`, `due YYYY-MM-DD` (with ` at HH:MM`
    /// when the time is not the default), `start YYYY-MM-DD`, then the repeat. A name with
    /// a space in it is bracketed, `+[Home Improvement]`. The title is kept exactly,
    /// including a date that stayed in it mid-sentence: the field it set is written
    /// again at the end, where a later date for the same field wins.
    ///
    /// One exception to the order: if the title itself contains the word `every`, the
    /// repeat goes first, because the parser reads the first `every` in the line.
    ///
    /// `zone` must be the zone the line was parsed in. Use [`Parsed::same_fields`] to
    /// check a re-parse, if a caller wants to be sure before recommending it.
    #[must_use]
    pub fn canonical<Tz: TimeZone>(&self, zone: &Tz) -> String
    where
        Tz::Offset: Display,
    {
        let repeat = self.repeat.map(Repeat::phrase);
        let repeat_first = repeat.is_some()
            && self
                .title
                .split_whitespace()
                .any(|word| word.eq_ignore_ascii_case("every"));

        let mut parts = Vec::new();
        if repeat_first {
            parts.extend(repeat.clone());
        }
        if !self.title.is_empty() {
            parts.push(self.title.clone());
        }
        parts.extend(self.names());
        if let Some(at) = self.due_date {
            parts.push(DateField::Due.write(at, zone, Wording::Line));
        }
        if let Some(at) = self.start_date {
            parts.push(DateField::Start.write(at, zone, Wording::Line));
        }
        if !repeat_first {
            parts.extend(repeat);
        }
        parts.join(" ")
    }

    /// Whether two parses describe the same task: every field but the token spans, which
    /// say where things were typed rather than what they were.
    #[must_use]
    pub fn same_fields(&self, other: &Self) -> bool {
        let Self {
            title,
            labels,
            assignees,
            project,
            priority,
            due_date,
            start_date,
            repeat,
            tokens: _,
        } = self;
        *title == other.title
            && *labels == other.labels
            && *assignees == other.assignees
            && *project == other.project
            && *priority == other.priority
            && *due_date == other.due_date
            && *start_date == other.start_date
            && *repeat == other.repeat
    }

    /// `+project`, `*label`s, `@user`s and `!N`, in that order, each written so the
    /// parser reads it back whole.
    fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(project) = &self.project {
            names.push(format!("+{}", wrapped(project)));
        }
        names.extend(
            self.labels
                .iter()
                .map(|label| format!("*{}", wrapped(label))),
        );
        names.extend(
            self.assignees
                .iter()
                .map(|user| format!("@{}", wrapped(user))),
        );
        if let Some(priority) = self.priority {
            names.push(format!("!{priority}"));
        }
        names
    }
}

/// A token's value, bracketed when it would not otherwise read back as itself.
///
/// A space would end the token; a value that starts with an opening quote or bracket, or
/// that the parser would unwrap, would be read differently. Brackets first, since a shell
/// leaves them alone; then whichever quotes the value does not contain.
fn wrapped(value: &str) -> String {
    let plain = !value.chars().any(char::is_whitespace)
        && !value.starts_with(['"', '\'', '[', '('])
        && unquote(value) == value;
    if plain {
        return value.to_string();
    }
    [('[', ']'), ('"', '"'), ('\'', '\''), ('(', ')')]
        .into_iter()
        .find(|&(_, close)| !value.contains(close))
        .map_or_else(
            || value.to_string(),
            |(open, close)| format!("{open}{value}{close}"),
        )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::parse;
    use super::*;
    use crate::zone::{pinned, ZonedTime};
    use chrono::FixedOffset;

    /// Thursday 2026-11-05 10:00, five hours behind UTC.
    fn now() -> ZonedTime {
        pinned(
            FixedOffset::west_opt(5 * 3600)
                .unwrap()
                .with_ymd_and_hms(2026, 11, 5, 10, 0, 0)
                .unwrap(),
        )
    }

    fn summary(input: &str) -> String {
        let clock = now();
        parse(input, &clock).summary(&clock.timezone())
    }

    fn canonical(input: &str) -> String {
        let clock = now();
        parse(input, &clock).canonical(&clock.timezone())
    }

    /// Every field, with the parse and the re-parse side by side when one differs.
    fn assert_round_trips(input: &str) {
        let clock = now();
        let first = parse(input, &clock);
        let line = first.canonical(&clock.timezone());
        let second = parse(&line, &clock);
        assert_eq!(
            second.title, first.title,
            "title, for {input:?} -> {line:?}"
        );
        assert_eq!(
            second.project, first.project,
            "project, for {input:?} -> {line:?}"
        );
        assert_eq!(
            second.labels, first.labels,
            "labels, for {input:?} -> {line:?}"
        );
        assert_eq!(
            second.assignees, first.assignees,
            "assignees, for {input:?} -> {line:?}"
        );
        assert_eq!(
            second.priority, first.priority,
            "priority, for {input:?} -> {line:?}"
        );
        assert_eq!(
            second.due_date, first.due_date,
            "due, for {input:?} -> {line:?}"
        );
        assert_eq!(
            second.start_date, first.start_date,
            "start, for {input:?} -> {line:?}"
        );
        assert_eq!(
            second.repeat, first.repeat,
            "repeat, for {input:?} -> {line:?}"
        );
        assert!(second.same_fields(&first), "{input:?} -> {line:?}");
        // And the canonical form is a fixed point: rewriting it changes nothing.
        assert_eq!(second.canonical(&clock.timezone()), line, "for {input:?}");
    }

    #[test]
    fn the_canonical_line_parses_back_to_the_same_task() {
        for input in [
            "Buy milk",
            "",
            "Call the dentist +Personal *health !3 due 2026-11-12",
            "Call the dentist tomorrow *health +Personal !3",
            "Renew the domain *admin +Infra !3 due friday at 9am every 12 months",
            "Review PR *urgent *backend @alice @bob",
            r#"Ship it *"needs review" +[Home Renovation] @'Ada Lovelace'"#,
            "+[Home Improvement] fix the gutter next week",
            "Task +Work +Home",
            "Fix it !9",
            "Fix it tomorrow !9",
            "Email bob@example.com about 2*3 and the cost +tax",
            // BUG-24: numbers in prose are not dates, and after a keyword they are.
            "llvm 1.6 GB and rust 432 MB",
            "add 1/2 cup sugar",
            "upgrade to 2.6.10",
            "call mom due 27.8",
            "due 2.5 hours",
            "hours due 2.5",
            // BUG-29: a date mid-sentence stays in the title and is still set.
            "I can't wait to goto the Azores on March 10, 2027. Is that the date?",
            "tomorrow buy milk",
            "meet on aug 27 at the cafe",
            "Call Bob due tomorrow about the invoice",
            "start monday on the report due friday",
            "start monday due friday",
            "fireworks start 2026-12-31 at 11pm due 2027-01-01 at 1am",
            "water the plants every 3 days",
            "check every room every week",
            "do it every day every week",
            "pay the bill due",
            "Café ☕ — 日本語 eom",
            "*[a]b] +#12 standup @x every hour",
            r#"*"[draft" *"'tis'" +"a]b c" memo"#,
            // `every` with punctuation is not the parser's `every`, capitalised it is.
            "check every, room every week",
            "Every. single day every 2 days",
            "do it Every day every week",
            // A title ending in a bare keyword, with a date of either kind after it.
            "get a head start due friday",
            "get a head start start monday",
            "this is overdue tomorrow",
            "pay the bill due !2 due 2026-12-01",
            "head start: the plan +Work",
        ] {
            assert_round_trips(input);
        }
    }

    /// A small deterministic generator, so the property test needs no new dependency and
    /// fails the same way every run.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(self.0 >> 33).unwrap() % n
        }
    }

    /// Lines built from prose, tokens, dates, numbers that are not dates, keywords left
    /// dangling, and punctuation, in random order: every one parses back to itself
    /// through its canonical line. That is stronger than what the echo needs, which only
    /// prints a line that does.
    #[test]
    fn generated_lines_round_trip_through_their_canonical_form() {
        const PIECES: &[&str] = &[
            "call",
            "the",
            "dentist",
            "buy",
            "milk",
            "on",
            "about",
            "report",
            "Is",
            "it",
            "booked?",
            "café",
            "🎉",
            "every",
            "Every",
            "every,",
            "due",
            "start",
            "starts",
            "at",
            "in",
            "next",
            "this",
            "week",
            "2",
            "days",
            "+Personal",
            "+[Home Improvement]",
            "+#12",
            "*health",
            "*\"needs review\"",
            "@alice",
            "@'Ada Lovelace'",
            "!3",
            "!9",
            "!",
            "+",
            "tomorrow",
            "tomorrow.",
            "next friday",
            "due friday at 5pm",
            "start monday",
            "aug 27",
            "27aug26",
            "2026-12-01",
            "due 27.8",
            "eom",
            "in 3 days",
            "every week",
            "every 2 months",
            "1.6",
            "GB",
            "1/2",
            "3-5",
            "2.6.10",
            "16.9",
            ".",
            ",",
            "—",
            "?",
            "at 9am",
            "start:monday",
        ];
        let clock = now();
        let mut rng = Lcg(0x7475_692d_646f);
        for _ in 0..400 {
            let length = 1 + rng.below(9);
            let line = (0..length)
                .map(|_| PIECES[rng.below(PIECES.len())])
                .collect::<Vec<_>>()
                .join(" ");
            let first = parse(&line, &clock);
            let best = first.canonical(&clock.timezone());
            let second = parse(&best, &clock);
            assert!(
                second.same_fields(&first),
                "{line:?} -> {best:?}\n first: {first:?}\nsecond: {second:?}"
            );
        }
    }

    #[test]
    fn the_canonical_line_is_in_the_recommended_order() {
        assert_eq!(
            canonical("tomorrow +Personal call the dentist *health !3 every week"),
            "tomorrow call the dentist +Personal *health !3 due 2026-11-06 every week"
        );
        assert_eq!(
            canonical("Call the dentist *health +Personal !3 next thursday"),
            "Call the dentist +Personal *health !3 due 2026-11-12"
        );
        assert_eq!(
            canonical("fix the gutter +\"Home Improvement\" start monday at 9am"),
            "fix the gutter +[Home Improvement] start 2026-11-09 at 09:00"
        );
        // A date already at the end in its best form needs no rewriting.
        assert_eq!(
            canonical("Call the dentist +Personal *health !3 due 2026-11-12"),
            "Call the dentist +Personal *health !3 due 2026-11-12"
        );
        // The parser reads the first `every`, so one in the title puts the repeat first.
        assert_eq!(
            canonical("do it every day every week"),
            "every day do it every week"
        );
    }

    #[test]
    fn a_value_is_wrapped_only_when_it_has_to_be() {
        assert_eq!(wrapped("Personal"), "Personal");
        assert_eq!(wrapped("#12"), "#12");
        assert_eq!(wrapped("Home Improvement"), "[Home Improvement]");
        assert_eq!(wrapped("a]b c"), "\"a]b c\"");
        assert_eq!(wrapped("[draft"), "[[draft]");
        assert_eq!(wrapped("'tis'"), "['tis']");
    }

    #[test]
    fn the_summary_names_every_field_that_was_set() {
        assert_eq!(
            summary("Call the dentist +Personal *health @admin !3 next thursday every week"),
            "title \"Call the dentist\" · +Personal · *health · @admin · !3 · \
             due Thu 12 Nov 2026 · every week"
        );
        assert_eq!(summary("Buy milk"), "title \"Buy milk\"");
        assert_eq!(summary(""), "");
        assert_eq!(
            summary("fix the gutter +[Home Improvement] start monday at 9am due friday at 5pm"),
            "title \"fix the gutter\" · +[Home Improvement] · due Fri 6 Nov 2026 at 17:00 · \
             starts Mon 9 Nov 2026 at 09:00"
        );
        assert_eq!(
            summary("water the plants every 3 days"),
            "title \"water the plants\" · every 3 days"
        );
        // A date kept in the title is still named, so it can be seen to have been read.
        assert_eq!(
            summary("tomorrow buy milk"),
            "title \"tomorrow buy milk\" · due Fri 6 Nov 2026"
        );
        // BUG-24: a decimal is not a date, and the summary says so by saying nothing.
        assert_eq!(summary("llvm 1.6 GB"), "title \"llvm 1.6 GB\"");
    }

    #[test]
    fn a_date_is_described_in_the_zone_it_was_typed_in() {
        // Typed in January in New York for a day in July: read back at July's offset,
        // the defaults stay defaults and no stray time appears.
        use chrono_tz::America::New_York;
        let january = New_York.with_ymd_and_hms(2027, 1, 15, 10, 0, 0).unwrap();
        let parsed = parse("fireworks start 2027-07-04 due 2027-07-04", &january);
        assert_eq!(
            parsed.summary(&New_York),
            "title \"fireworks\" · due Sun 4 Jul 2027 · starts Sun 4 Jul 2027"
        );
        assert_eq!(
            parsed.canonical(&New_York),
            "fireworks due 2027-07-04 start 2027-07-04"
        );
    }
}
