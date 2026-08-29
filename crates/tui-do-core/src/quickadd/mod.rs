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
//! Written against Vikunja's documented syntax rather than ported from cria, whose
//! version has three problems worth not reproducing: it prints debug output on `stdout`
//! from inside a TUI, it scrapes dates with a regex that swallows the rest of the title,
//! and it stamps naive local times as UTC so every date is off by the machine's offset.
//!
//! [`parse`] is pure and takes `now` as an argument. Nothing here reads the clock, which
//! is what lets the tests pin exact instants.

mod dates;
mod words;

use std::ops::Range;

use chrono::{DateTime, TimeZone, Utc};

pub use dates::{DateField, Unit};

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
    /// The title, with every recognised token removed.
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
/// against, so the caller passes `Local::now()` and tests pass whatever they like.
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

    for found in dates::find(&remaining, now) {
        match found.field {
            DateField::Due => parsed.due_date = Some(found.at),
            DateField::Start => parsed.start_date = Some(found.at),
        }
        parsed.tokens.push(Token {
            kind: match found.field {
                DateField::Due => TokenKind::DueDate,
                DateField::Start => TokenKind::StartDate,
            },
            range: found.range.clone(),
        });
        consumed.push(found.range);
    }

    parsed.tokens.sort_by_key(|t| t.range.start);
    parsed.title = words::remove_ranges(input, &consumed);
    parsed
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
    fn dates_come_out_of_the_title() {
        let parsed = p("Call Bob due tomorrow about the invoice");
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
    fn unicode_survives_intact() {
        let parsed = p("Réserver le café *déjeuner +Vacances");
        assert_eq!(parsed.title, "Réserver le café");
        assert_eq!(parsed.labels, vec!["déjeuner"]);
        assert_eq!(parsed.project.as_deref(), Some("Vacances"));
    }
}
