//! Vikunja's quick-add magic.
//!
//! One line of text becomes a task:
//!
//! ```text
//! Renew the domain *admin +Infra !3 due friday at 9am every 12 months
//! ```
//!
//! `*` is a label, `@` an assignee, `+` a project, `!1`–`!5` a priority; dates are
//! phrases (`due friday`, `start monday`, `in 3 days`, `eom`); `every N units` repeats.
//! Values with spaces are quoted or bracketed: `*"needs review"`, `+[Home Renovation]`.
//!
//! Tokens may appear anywhere in the line. Labels, projects, users, priorities and repeats
//! leave the title wherever they are. A date is always read, but leaves the title only
//! when nothing follows it except whitespace, punctuation and other tokens; anywhere else
//! it stays exactly as typed. The rule is `ends_the_line`.
//!
//! Written against Vikunja's documented syntax rather than ported from cria, whose
//! version has three problems worth not reproducing: it prints debug output on `stdout`
//! from inside a TUI, it scrapes dates with a regex that swallows the rest of the title,
//! and it stamps naive local times as UTC so every date is off by the machine's offset.
//!
//! [`parse`] is pure and takes `now` as an argument. Nothing here reads the clock, which
//! is what lets the tests pin exact instants.
//!
//! [`Parsed::summary`] says back what a line will create, and [`Parsed::canonical`] how it
//! is best written; `tui-do add --dry-run` prints both.

mod dates;
mod echo;
mod words;

use std::ops::Range;

use chrono::{DateTime, TimeZone, Utc};

pub use dates::{DateField, Unit};
pub use echo::name_token;

/// How often a task repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repeat {
    /// How many of `unit` between occurrences.
    pub every: u32,

    /// The unit.
    pub unit: Unit,
}

impl Repeat {
    /// The interval in seconds, for Vikunja's `repeat_after`.
    #[must_use]
    pub fn seconds(self) -> i64 {
        i64::from(self.every) * self.unit.seconds()
    }

    /// Whether this should be sent as `repeat_mode: monthly` instead of an interval.
    ///
    /// A month is not a fixed number of seconds, and Vikunja has a mode that says so.
    #[must_use]
    pub fn is_monthly(self) -> bool {
        self.unit == Unit::Month && self.every == 1
    }
}

/// What a piece of the input turned into, for syntax highlighting as the user types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A `*label`.
    Label,
    /// An `@assignee`.
    Assignee,
    /// A `+project`.
    Project,
    /// A `!1`-`!5`.
    Priority,
    /// A due-date phrase.
    DueDate,
    /// A start-date phrase.
    StartDate,
    /// An `every N units` phrase.
    Repeat,
}

/// A recognised span of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What it was recognised as.
    pub kind: TokenKind,

    /// Where it sits in the input, in bytes.
    pub range: Range<usize>,
}

/// The result of reading one line of quick-add input.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    /// The title, with every recognised token removed -- except a date that something
    /// else follows, which stays where it was typed. See [`ends_the_line`].
    pub title: String,

    /// Label titles, in the order given.
    pub labels: Vec<String>,

    /// Assignee usernames, in the order given.
    pub assignees: Vec<String>,

    /// Project title, if one was named.
    pub project: Option<String>,

    /// Priority, 1 to 5.
    pub priority: Option<u8>,

    /// When the task is due.
    pub due_date: Option<DateTime<Utc>>,

    /// When work should start.
    pub start_date: Option<DateTime<Utc>>,

    /// How often it repeats.
    pub repeat: Option<Repeat>,

    /// Every recognised span, in input order.
    ///
    /// The quick-add modal highlights these live, so the user can see what tui-do
    /// understood before committing to it.
    pub tokens: Vec<Token>,
}

impl Parsed {
    /// Whether anything at all was recognised beyond a plain title.
    #[must_use]
    pub fn has_magic(&self) -> bool {
        !self.tokens.is_empty()
    }
}

/// Read one line of quick-add input.
///
/// `now` supplies both the current instant and the timezone relative dates resolve
/// against, so the runtime passes its clock in `Zone::Local` (see [`crate::zone`]) and
/// tests pass whatever zone they like.
pub fn parse<Tz: TimeZone>(input: &str, now: &DateTime<Tz>) -> Parsed {
    let mut parsed = Parsed::default();
    let mut consumed: Vec<Range<usize>> = Vec::new();

    let all_words = words::split(input);

    // Prefixed tokens first: they are unambiguous, and removing them keeps a label like
    // *tomorrow from being read as a date.
    let mut remaining = Vec::new();
    for word in all_words {
        match prefixed(&word.text) {
            Some((kind, value)) if !value.is_empty() => {
                let range = word.range.clone();
                match kind {
                    TokenKind::Label => parsed.labels.push(value),
                    TokenKind::Assignee => parsed.assignees.push(value),
                    // Last project wins: retyping one should correct it, not be ignored.
                    TokenKind::Project => parsed.project = Some(value),
                    TokenKind::Priority => match value.parse::<u8>() {
                        Ok(priority @ 1..=5) => parsed.priority = Some(priority),
                        // `!` followed by anything else is ordinary punctuation.
                        _ => {
                            remaining.push(word);
                            continue;
                        }
                    },
                    _ => {}
                }
                parsed.tokens.push(Token {
                    kind,
                    range: range.clone(),
                });
                consumed.push(range);
            }
            _ => remaining.push(word),
        }
    }

    // `every N units`, before dates, so "every 2 weeks" is not read as a date phrase.
    if let Some((repeat, range, used)) = repeat_phrase(&remaining) {
        parsed.repeat = Some(repeat);
        parsed.tokens.push(Token {
            kind: TokenKind::Repeat,
            range: range.clone(),
        });
        consumed.push(range);
        remaining.retain(|w| !used.contains(&w.range.start));
    }

    let dates = dates::find(&remaining, now, dates::Keyword::Absent);
    for date in &dates {
        match date.field {
            DateField::Due => parsed.due_date = Some(date.at),
            DateField::Start => parsed.start_date = Some(date.at),
        }
        parsed.tokens.push(Token {
            kind: match date.field {
                DateField::Due => TokenKind::DueDate,
                DateField::Start => TokenKind::StartDate,
            },
            range: date.range.clone(),
        });
    }

    // A date leaves the title only from the end (BUG-29). Judged last to first, so that
    // of two dates at the end, the later one being removed lets the earlier one go too.
    for date in dates.iter().rev() {
        if ends_the_line(input, date.range.end, &consumed) {
            consumed.push(date.range.clone());
        }
    }

    parsed.tokens.sort_by_key(|t| t.range.start);
    parsed.title = words::remove_ranges(input, &consumed);
    parsed
}

/// Whether nothing meaningful follows byte `from` of `input`, so a date ending there may
/// be taken out of the title.
///
/// What may follow: whitespace, punctuation, and text inside `removed` -- the tokens that
/// leave the title wherever they are (`*label`, `+project`, `@user`, `!N`, a repeat) and
/// any later date that already qualified. Anything else -- a letter, a digit, an emoji --
/// means the date is part of a sentence, and it stays in the title exactly as typed, keyword and all:
/// cutting it out of the middle left `goto the Azores on Is that the date?` (BUG-29).
///
/// Only what *follows* is looked at. Punctuation the date's own last word carries
/// (`tomorrow.`) is inside its range and goes with it; punctuation before it belongs to
/// the text before it, and stays.
fn ends_the_line(input: &str, from: usize, removed: &[Range<usize>]) -> bool {
    input[from..].char_indices().all(|(offset, ch)| {
        let at = from + offset;
        removed.iter().any(|range| range.contains(&at)) || ch.is_whitespace() || is_trailing(ch)
    })
}

/// Punctuation that can end a line without being something the user meant to keep a date
/// in front of.
///
/// Beyond ASCII: closing quotes, an ellipsis and dashes, which end a phrase. Opening marks
/// (`“`, `«`, `¿`) are deliberately not, since they start text that follows.
fn is_trailing(ch: char) -> bool {
    ch.is_ascii_punctuation() || matches!(ch, '…' | '–' | '—' | '’' | '”' | '»')
}

/// Read a field that holds nothing but a due date, such as the edit form's Due.
///
/// Understands everything [`parse`] does, and also a bare `27.8` or `27/08` with no `due`
/// in front of it: in a sentence those need the keyword, because they read as well as a
/// decimal or a fraction, but in a date field a date is the only thing they can be.
#[must_use]
pub fn parse_due<Tz: TimeZone>(input: &str, now: &DateTime<Tz>) -> Option<DateTime<Utc>> {
    dates::find(&words::split(input), now, dates::Keyword::Given)
        .into_iter()
        .rev()
        .find(|found| found.field == DateField::Due)
        .map(|found| found.at)
}

/// A stored due date as the text a field that edits it starts with, which is what
/// [`parse_due`] reads back: the edit form's Due and the `D` prompt (BUG-30).
///
/// In `now`'s zone, because the field is read back in it: formatting the UTC instant
/// showed the next day for a 23:59 due date anywhere west of UTC. ISO `2027-03-10`,
/// because a four-digit year parses with or without `due` in front and cannot be read
/// day-first or month-first. A time other than [`DateField::Due`]'s default is shown as
/// `2027-03-10 at 14:00`, so retyping only the day keeps the hour.
///
/// It round-trips to the minute with one exception: the second reading of a repeated
/// hour. `2027-11-07 at 01:30` in New York names two instants, and [`parse_due`] takes
/// the earlier (BUG-20), so a prefill of the EST 01:30 reads back as the EDT one. A field
/// left untouched is never parsed, so that only matters when it is retyped, and the
/// parser has no syntax for an offset to disambiguate it with.
#[must_use]
pub fn due_text<Tz: TimeZone>(due: Option<DateTime<Utc>>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    use chrono::Timelike as _;
    let Some(due) = due else {
        return String::new();
    };
    let local = due.with_timezone(&now.timezone());
    if (local.hour(), local.minute()) == DateField::Due.default_time() {
        local.format("%Y-%m-%d").to_string()
    } else {
        local.format("%Y-%m-%d at %H:%M").to_string()
    }
}

/// Split a `*label`, `@assignee`, `+project` or `!3` into its kind and value.
///
/// Quoted and bracketed forms carry values with spaces. They are recognised here on a
/// whole word, so `2*3` and `a@b` stay in the title: a magic token has to start one.
fn prefixed(word: &str) -> Option<(TokenKind, String)> {
    let mut chars = word.chars();
    let kind = match chars.next()? {
        '*' => TokenKind::Label,
        '@' => TokenKind::Assignee,
        '+' => TokenKind::Project,
        '!' => TokenKind::Priority,
        _ => return None,
    };
    let rest = chars.as_str();
    Some((kind, unquote(rest)))
}

/// Strip a matched pair of quotes or brackets.
fn unquote(value: &str) -> String {
    for (open, close) in [('"', '"'), ('\'', '\''), ('[', ']'), ('(', ')')] {
        if let Some(inner) = value.strip_prefix(open).and_then(|v| v.strip_suffix(close)) {
            return inner.to_string();
        }
    }
    value.to_string()
}

/// Recognise `every 2 weeks`, `every week`, `every 3 days`.
fn repeat_phrase(words: &[words::Word]) -> Option<(Repeat, Range<usize>, Vec<usize>)> {
    let start = words.iter().position(|w| w.is("every"))?;

    // `every week`
    if let Some(next) = words.get(start + 1) {
        if let Some(unit) = dates::unit(next.trimmed()) {
            return Some((
                Repeat { every: 1, unit },
                words[start].range.start..next.range.end,
                vec![words[start].range.start, next.range.start],
            ));
        }

        // `every 2 weeks`
        if let (Ok(every), Some(unit_word)) = (next.trimmed().parse::<u32>(), words.get(start + 2))
        {
            if let Some(unit) = dates::unit(unit_word.trimmed()) {
                if every > 0 {
                    return Some((
                        Repeat { every, unit },
                        words[start].range.start..unit_word.range.end,
                        vec![
                            words[start].range.start,
                            next.range.start,
                            unit_word.range.start,
                        ],
                    ));
                }
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    /// Wednesday 2026-08-26 10:00, five hours behind UTC.
    fn now() -> DateTime<FixedOffset> {
        FixedOffset::west_opt(5 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 8, 26, 10, 0, 0)
            .unwrap()
    }

    fn p(input: &str) -> Parsed {
        parse(input, &now())
    }

    fn local(at: Option<DateTime<Utc>>) -> String {
        at.map(|d| {
            d.with_timezone(&now().timezone())
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "none".to_string())
    }

    #[test]
    fn a_plain_title_stays_a_plain_title() {
        let parsed = p("Buy milk");
        assert_eq!(parsed.title, "Buy milk");
        assert!(!parsed.has_magic());
        assert_eq!(
            parsed,
            Parsed {
                title: "Buy milk".into(),
                ..Parsed::default()
            }
        );
    }

    #[test]
    fn the_documented_example_parses_whole() {
        let parsed = p("Renew the domain *admin +Infra !3 due friday at 9am every 12 months");
        assert_eq!(parsed.title, "Renew the domain");
        assert_eq!(parsed.labels, vec!["admin"]);
        assert_eq!(parsed.project.as_deref(), Some("Infra"));
        assert_eq!(parsed.priority, Some(3));
        assert_eq!(local(parsed.due_date), "2026-08-28 09:00");
        assert_eq!(
            parsed.repeat,
            Some(Repeat {
                every: 12,
                unit: Unit::Month
            })
        );
    }

    #[test]
    fn several_labels_and_assignees_are_kept_in_order() {
        let parsed = p("Review PR *urgent *backend @alice @bob");
        assert_eq!(parsed.labels, vec!["urgent", "backend"]);
        assert_eq!(parsed.assignees, vec!["alice", "bob"]);
        assert_eq!(parsed.title, "Review PR");
    }

    #[test]
    fn quoted_and_bracketed_values_carry_spaces() {
        let parsed = p(r#"Ship it *"needs review" +[Home Renovation] @'Ada Lovelace'"#);
        assert_eq!(parsed.labels, vec!["needs review"]);
        assert_eq!(parsed.project.as_deref(), Some("Home Renovation"));
        assert_eq!(parsed.assignees, vec!["Ada Lovelace"]);
        assert_eq!(parsed.title, "Ship it");
    }

    #[test]
    fn the_last_project_wins() {
        // Retyping a project should correct it rather than be silently ignored.
        let parsed = p("Task +Work +Home");
        assert_eq!(parsed.project.as_deref(), Some("Home"));
    }

    #[test]
    fn priority_must_be_one_to_five() {
        assert_eq!(p("Fix it !1").priority, Some(1));
        assert_eq!(p("Fix it !5").priority, Some(5));
        // Out of range, so it is punctuation and stays in the title.
        let parsed = p("Fix it !9");
        assert_eq!(parsed.priority, None);
        assert_eq!(parsed.title, "Fix it !9");
    }

    #[test]
    fn punctuation_that_is_not_a_token_stays_in_the_title() {
        // A magic token has to start a word, so arithmetic and email survive.
        let parsed = p("Email bob@example.com about 2*3 and the cost +tax");
        assert!(parsed.assignees.is_empty());
        assert!(parsed.labels.is_empty());
        assert_eq!(parsed.project.as_deref(), Some("tax"));
        assert_eq!(parsed.title, "Email bob@example.com about 2*3 and the cost");
    }

    #[test]
    fn an_exclamation_mark_is_still_punctuation() {
        let parsed = p("Ship it already!");
        assert_eq!(parsed.title, "Ship it already!");
        assert_eq!(parsed.priority, None);
    }

    #[test]
    fn a_label_named_like_a_date_is_not_read_as_one() {
        // Prefixed tokens are removed before date matching, so this stays a label.
        let parsed = p("Plan *tomorrow");
        assert_eq!(parsed.labels, vec!["tomorrow"]);
        assert_eq!(parsed.due_date, None);
        assert_eq!(parsed.title, "Plan");
    }

    #[test]
    fn a_date_at_the_end_comes_out_of_the_title() {
        // This read "Call Bob due tomorrow about the invoice" until BUG-29: mid-sentence,
        // the date now stays, which `a_date_mid_sentence_stays_in_the_title` asserts.
        let parsed = p("Call Bob about the invoice due tomorrow");
        assert_eq!(parsed.title, "Call Bob about the invoice");
        assert_eq!(local(parsed.due_date), "2026-08-27 23:59");
    }

    #[test]
    fn a_bare_date_phrase_sets_the_due_date() {
        let parsed = p("Standup tomorrow at 9am");
        assert_eq!(parsed.title, "Standup");
        assert_eq!(local(parsed.due_date), "2026-08-27 09:00");
    }

    #[test]
    fn start_and_due_are_separate_fields() {
        let parsed = p("Migrate the database start monday due friday");
        assert_eq!(parsed.title, "Migrate the database");
        assert_eq!(local(parsed.start_date), "2026-08-31 00:00");
        assert_eq!(local(parsed.due_date), "2026-08-28 23:59");
    }

    #[test]
    fn repeats_parse_with_and_without_a_count() {
        assert_eq!(
            p("Water plants every week").repeat,
            Some(Repeat {
                every: 1,
                unit: Unit::Week
            })
        );
        assert_eq!(
            p("Standup every 2 days").repeat,
            Some(Repeat {
                every: 2,
                unit: Unit::Day
            })
        );
        assert_eq!(p("Water plants every week").title, "Water plants");
        assert!(p("Rent every month").repeat.unwrap().is_monthly());
        assert_eq!(p("Standup every 2 days").repeat.unwrap().seconds(), 172_800);
    }

    #[test]
    fn every_without_a_unit_is_left_in_the_title() {
        let parsed = p("Check every server");
        assert_eq!(parsed.repeat, None);
        assert_eq!(parsed.title, "Check every server");
    }

    #[test]
    fn tokens_are_reported_in_input_order_for_highlighting() {
        let input = "Fix *bug +Work !2 due tomorrow";
        let parsed = parse(input, &now());
        let kinds: Vec<TokenKind> = parsed.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Label,
                TokenKind::Project,
                TokenKind::Priority,
                TokenKind::DueDate,
            ]
        );
        // Every reported span must actually be the text it claims.
        assert_eq!(&input[parsed.tokens[0].range.clone()], "*bug");
        assert_eq!(&input[parsed.tokens[3].range.clone()], "due tomorrow");
    }

    #[test]
    fn a_bare_prefix_character_is_not_a_token() {
        let parsed = p("Rate this * out of 5");
        assert!(parsed.labels.is_empty());
        assert_eq!(parsed.title, "Rate this * out of 5");
    }

    #[test]
    fn an_empty_input_produces_an_empty_task() {
        assert_eq!(p("").title, "");
        assert_eq!(p("   ").title, "");
    }

    #[test]
    fn a_title_of_only_magic_leaves_an_empty_title() {
        // The caller decides what to do about it; the parser does not invent a title.
        let parsed = p("*label +Project !1");
        assert_eq!(parsed.title, "");
        assert_eq!(parsed.labels, vec!["label"]);
    }

    #[test]
    fn an_ordinary_word_that_starts_like_a_month_is_not_a_date() {
        // A three-letter prefix match read `Dec`ide as December, so "Decide 3 things to
        // do" became a task called "things to do" due on 3 December -- a date the user
        // never typed, and two words gone from the title. The month reading is whole-word
        // now, so each of these is a title and nothing else.
        for input in [
            "Decide 3 things to do",
            "Run marathon 26",
            "Augment 4 diagrams",
            "Januar 5 revisions",
        ] {
            let parsed = p(input);
            assert_eq!(
                parsed.title, input,
                "{input:?} lost words to a date that is not there"
            );
            assert_eq!(local(parsed.due_date), "none", "{input:?}");
        }
    }

    #[test]
    fn a_month_and_a_day_still_read_as_a_date() {
        // The whole-word rule must not cost the spellings people actually use. `sept` is
        // the one four-letter form, and it is why `month_name` is a table rather than a
        // truncation.
        for (input, title, due) in [
            ("Taxes due apr 15", "Taxes", "2027-04-15 23:59"),
            ("Taxes due april 15", "Taxes", "2027-04-15 23:59"),
            ("Taxes due 15 apr", "Taxes", "2027-04-15 23:59"),
            ("Party due sept 9", "Party", "2026-09-09 23:59"),
            ("Party due september 9", "Party", "2026-09-09 23:59"),
        ] {
            let parsed = p(input);
            assert_eq!(parsed.title, title, "{input:?}");
            assert_eq!(local(parsed.due_date), due, "{input:?}");
        }
    }

    #[test]
    fn a_number_in_prose_is_not_a_date() {
        // BUG-24. Each of these used to lose the number from the title *and* gain a due
        // date, several of them a year or more out. Both halves are asserted, because a
        // fix that stopped setting the date while still eating the text would pass a test
        // that only checked one.
        for input in [
            "llvm 1.6 GB",
            "llvm 1.6 GB and rust 432 MB",
            "costs 2.5 hours",
            "costs 2.5 hours of work",
            "ratio was 16.9 percent",
            "add 1/2 cup sugar",
            "takes 3-5 days",
            "score 10-12",
            "room 4.12",
            "ratio 16.9",
            "upgrade to 2.6.10",
            "bump to 1.2.3",
            "budget is 3.50 for this",
            "pages 27.08.26",
            "pick 1 2 3",
            // A four-digit year first and dots between is a version number's shape.
            "release 2026.1.5",
            "release 2026.08.27",
        ] {
            let parsed = p(input);
            assert_eq!(parsed.title, input, "{input:?} lost text to a date");
            assert_eq!(
                local(parsed.due_date),
                "none",
                "{input:?} gained a due date"
            );
            assert_eq!(
                local(parsed.start_date),
                "none",
                "{input:?} gained a start date"
            );
            assert!(
                !parsed.has_magic(),
                "{input:?} would be highlighted as a date"
            );
        }
    }

    #[test]
    fn a_bare_numeric_date_after_a_keyword_still_parses() {
        for (input, due, start) in [
            ("Call mom due 27.8", "2026-08-27 23:59", "none"),
            ("Call mom due 27/08", "2026-08-27 23:59", "none"),
            ("Call mom due 27-8", "2026-08-27 23:59", "none"),
            ("Call mom due 1.2.30", "2030-02-01 23:59", "none"),
            ("Call mom due 27.08.26", "2026-08-27 23:59", "none"),
            ("Call mom start 27/08", "none", "2026-08-27 00:00"),
            ("Call mom starting 27.8", "none", "2026-08-27 00:00"),
            ("Call mom start:27.8", "none", "2026-08-27 00:00"),
            ("Call mom due 2026.1.5", "2026-01-05 23:59", "none"),
            ("Call mom due 2026.08.27", "2026-08-27 23:59", "none"),
        ] {
            let parsed = p(input);
            assert_eq!(parsed.title, "Call mom", "{input:?}");
            assert_eq!(local(parsed.due_date), due, "{input:?}");
            assert_eq!(local(parsed.start_date), start, "{input:?}");
        }
    }

    #[test]
    fn the_keyword_is_decisive_even_before_a_decimal() {
        // The rule is the keyword, not a guess at what follows it: `due 2.5 hours` is a
        // date, 2 May. Typing `due` is asking for one. Since BUG-29 `hours` following it
        // keeps the phrase in the title; at the end it comes out.
        let parsed = p("due 2.5 hours");
        assert_eq!(parsed.title, "due 2.5 hours");
        assert_eq!(local(parsed.due_date), "2027-05-02 23:59");
        let parsed = p("hours due 2.5");
        assert_eq!(parsed.title, "hours");
        assert_eq!(local(parsed.due_date), "2027-05-02 23:59");
    }

    #[test]
    fn an_unambiguous_date_needs_no_keyword() {
        // A four-digit year or a month name cannot be a decimal, a fraction or a version.
        for (input, due) in [
            ("Call mom 2026-08-27", "2026-08-27 23:59"),
            ("Call mom 27/08/2026", "2026-08-27 23:59"),
            ("Call mom 27.8.2026", "2026-08-27 23:59"),
            ("Call mom 27aug", "2026-08-27 23:59"),
            ("Call mom 27aug26", "2026-08-27 23:59"),
            ("Call mom aug 27", "2026-08-27 23:59"),
            ("Call mom 27 aug 2026", "2026-08-27 23:59"),
            ("Call mom august 27", "2026-08-27 23:59"),
            ("Call mom tomorrow", "2026-08-27 23:59"),
        ] {
            let parsed = p(input);
            assert_eq!(parsed.title, "Call mom", "{input:?}");
            assert_eq!(local(parsed.due_date), due, "{input:?}");
        }
    }

    #[test]
    fn a_due_field_reads_a_bare_numeric_date() {
        // The edit form's Due field and the `d` prompt hold nothing but a date, so the
        // keyword is implied there.
        for (input, due) in [
            ("27.8", "2026-08-27 23:59"),
            ("27/08", "2026-08-27 23:59"),
            ("1.2.30", "2030-02-01 23:59"),
            ("due 27.8", "2026-08-27 23:59"),
            ("tomorrow", "2026-08-27 23:59"),
            ("2026-08-27", "2026-08-27 23:59"),
        ] {
            assert_eq!(local(parse_due(input, &now())), due, "{input:?}");
        }
        assert_eq!(parse_due("not a date", &now()), None);
        assert_eq!(parse_due("start monday", &now()), None);
    }

    #[test]
    fn a_date_mid_sentence_stays_in_the_title() {
        // BUG-29, the user's own sentence. The date is still set; the title is what they
        // typed, full stop after the year and all.
        let input = "I can't wait to goto the Azores on March 10, 2027. Is that the date?";
        let parsed = p(input);
        assert_eq!(parsed.title, input);
        assert_eq!(local(parsed.due_date), "2027-03-10 23:59");
        assert_eq!(parsed.tokens.len(), 1);
        assert_eq!(parsed.tokens[0].kind, TokenKind::DueDate);
        assert_eq!(&input[parsed.tokens[0].range.clone()], "March 10, 2027.");

        for (input, due) in [
            ("tomorrow buy milk", "2026-08-27 23:59"),
            ("meet on aug 27 at the cafe", "2026-08-27 23:59"),
            (
                "Call Bob due tomorrow about the invoice",
                "2026-08-27 23:59",
            ),
        ] {
            let parsed = p(input);
            assert_eq!(parsed.title, input, "{input:?}");
            assert_eq!(local(parsed.due_date), due, "{input:?}");
            assert!(parsed.has_magic(), "{input:?} would not be highlighted");
        }
    }

    #[test]
    fn a_date_at_the_end_still_comes_out() {
        for (input, title) in [
            ("call mom tomorrow", "call mom"),
            // The full stop is part of the date's word, so it goes with it.
            ("call mom tomorrow.", "call mom"),
            ("call mom tomorrow  ", "call mom"),
            // Punctuation belonging to the text before the date is not the date's.
            ("Is it booked? tomorrow", "Is it booked?"),
        ] {
            let parsed = p(input);
            assert_eq!(parsed.title, title, "{input:?}");
            assert_eq!(local(parsed.due_date), "2026-08-27 23:59", "{input:?}");
        }

        // Tokens that are removed themselves do not count as something following it.
        let parsed = p("pay rent due friday *bills !3");
        assert_eq!(parsed.title, "pay rent");
        assert_eq!(local(parsed.due_date), "2026-08-28 23:59");
        assert_eq!(parsed.labels, vec!["bills"]);
        assert_eq!(parsed.priority, Some(3));

        let parsed = p("water plants tomorrow every week +Home");
        assert_eq!(parsed.title, "water plants");
        assert_eq!(local(parsed.due_date), "2026-08-27 23:59");

        // A quoted or bracketed project is one removed token, spaces and all.
        for input in [
            r#"pay rent due friday +"Home Improvement""#,
            "pay rent due friday +[Home Improvement]",
        ] {
            let parsed = p(input);
            assert_eq!(parsed.title, "pay rent", "{input:?}");
            assert_eq!(local(parsed.due_date), "2026-08-28 23:59", "{input:?}");
            assert_eq!(
                parsed.project.as_deref(),
                Some("Home Improvement"),
                "{input:?}"
            );
        }

        // A word that is not a token does count, even one that only looks like one, and
        // so does a non-ASCII letter or an emoji: those are text, not punctuation.
        for input in ["fix it tomorrow !9", "tomorrow café", "tomorrow 🎉"] {
            let parsed = p(input);
            assert_eq!(parsed.title, input, "{input:?}");
            assert_eq!(local(parsed.due_date), "2026-08-27 23:59", "{input:?}");
        }
    }

    #[test]
    fn two_dates_are_each_judged_by_what_follows_them() {
        // Both at the end: each is followed only by the other, which is itself removed.
        let parsed = p("Migrate the database start monday due friday.");
        assert_eq!(parsed.title, "Migrate the database");
        assert_eq!(local(parsed.start_date), "2026-08-31 00:00");
        assert_eq!(local(parsed.due_date), "2026-08-28 23:59");

        // One mid-sentence, one at the end: only the last comes out, both are set.
        let parsed = p("start monday on the report due friday");
        assert_eq!(parsed.title, "start monday on the report");
        assert_eq!(local(parsed.start_date), "2026-08-31 00:00");
        assert_eq!(local(parsed.due_date), "2026-08-28 23:59");
        assert_eq!(parsed.tokens.len(), 2);
    }

    #[test]
    fn unicode_survives_intact() {
        let parsed = p("Réserver le café *déjeuner +Vacances");
        assert_eq!(parsed.title, "Réserver le café");
        assert_eq!(parsed.labels, vec!["déjeuner"]);
        assert_eq!(parsed.project.as_deref(), Some("Vacances"));
    }

    #[test]
    fn due_text_is_iso_in_the_readers_zone() {
        // 2027-03-11T04:59Z is 10 March 23:59 at -05:00 (BUG-30).
        let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
        assert_eq!(due_text(Some(due), &now()), "2027-03-10");
        let at_two = Utc.with_ymd_and_hms(2027, 3, 10, 19, 0, 0).unwrap();
        assert_eq!(due_text(Some(at_two), &now()), "2027-03-10 at 14:00");
        assert_eq!(due_text(None, &now()), "");
    }

    #[test]
    fn due_text_reads_back_to_the_same_instant() {
        for due in [
            Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap(),
            Utc.with_ymd_and_hms(2027, 3, 10, 19, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 12, 1, 5, 30, 0).unwrap(),
        ] {
            let text = due_text(Some(due), &now());
            assert_eq!(parse_due(&text, &now()), Some(due), "{text:?}");
        }
    }

    #[test]
    fn due_text_in_a_repeated_hour_reads_back_as_the_earlier_one() {
        use chrono_tz::America::New_York;
        // 2027-11-07 01:30 happens twice in New York: 05:30Z at EDT, then 06:30Z at EST.
        let now = New_York.with_ymd_and_hms(2027, 1, 15, 10, 0, 0).unwrap();
        let first = Utc.with_ymd_and_hms(2027, 11, 7, 5, 30, 0).unwrap();
        let second = Utc.with_ymd_and_hms(2027, 11, 7, 6, 30, 0).unwrap();

        assert_eq!(due_text(Some(first), &now), "2027-11-07 at 01:30");
        assert_eq!(due_text(Some(second), &now), "2027-11-07 at 01:30");
        assert_eq!(parse_due("2027-11-07 at 01:30", &now), Some(first));
        // The documented exception: retyped, the second reading becomes the first.
        // Untouched, the form never parses it, so the stored instant stands.
        assert_ne!(parse_due(&due_text(Some(second), &now), &now), Some(second));
    }
}
