//! Tab completion and `?` help for a quick-add line.
//!
//! Shared by the two places a task is typed: `tui-do add` run with no text, and the
//! interface's `a` prompt. Both hold a [`QuickAddInput`], and what it can complete is
//! handed to it as a [`Vocabulary`] -- plain names, read by whoever opened the prompt.
//! Nothing here reads a store (Rule 1), so the runtime reads one for the command line and
//! the interface passes what its model already holds.
//!
//! Modelled on Cisco IOS: Tab completes, a second Tab lists, and `?` says what can come
//! next. Unlike IOS the line is prose, and prose has question marks in it -- the user's
//! own task did -- so `?` is help only where nothing else could be meant: straight after
//! a sigil (`+?`), straight after `due ` or `start `, or on an empty line. Anywhere else
//! it is the character. F1 is always help.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyModifiers};
use tui_do_core::models::{Label, Project};

use crate::keymap::Key;
use crate::modal::TextInput;

/// The names a quick-add line can complete to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vocabulary {
    /// Project titles, for `+`.
    pub projects: Vec<String>,
    /// Label titles, for `*`.
    pub labels: Vec<String>,
    /// Usernames, for `@`.
    pub users: Vec<String>,
}

impl Vocabulary {
    /// The names in these lists that a task can be filed under or tagged with.
    ///
    /// Only real, unarchived projects: `/projects` also returns saved filters with
    /// negative ids, and those reject writes. Each list is sorted ignoring case, with
    /// duplicates dropped, so a completion never offers the same word twice.
    #[must_use]
    pub fn new(
        projects: &[Project],
        labels: &[Label],
        users: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            projects: tidy(
                projects
                    .iter()
                    .filter(|project| project.id.get() > 0 && !project.is_archived)
                    .map(|project| project.title.clone()),
            ),
            labels: tidy(labels.iter().map(|label| label.title.clone())),
            users: tidy(users),
        }
    }

    fn names(&self, slot: Slot) -> Vec<String> {
        match slot {
            Slot::Project => self.projects.clone(),
            Slot::Label => self.labels.clone(),
            Slot::User => self.users.clone(),
            Slot::Priority => ["1", "2", "3", "4", "5"].map(String::from).to_vec(),
            Slot::Date => DATE_WORDS.iter().map(|word| (*word).to_string()).collect(),
        }
    }
}

/// Sorted ignoring case, blanks and repeats dropped.
fn tidy(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = names
        .into_iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// The date words a `due ` or `start ` completes to. Every one is checked against the
/// parser by a test, so none of them can be offered and then not understood.
pub const DATE_WORDS: &[&str] = &[
    "today",
    "tomorrow",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "next week",
    "next month",
    "next year",
    "next weekend",
    "next monday",
    "next tuesday",
    "next wednesday",
    "next thursday",
    "next friday",
    "next saturday",
    "next sunday",
    "this week",
    "this weekend",
    "end of week",
    "end of month",
    "end of year",
    "eow",
    "eom",
    "eoy",
];

/// What is being typed at the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// After `+`.
    Project,
    /// After `*`.
    Label,
    /// After `@`.
    User,
    /// After `!`.
    Priority,
    /// After `due ` or `start `.
    Date,
}

impl Slot {
    const fn from_sigil(c: char) -> Option<Self> {
        match c {
            '+' => Some(Self::Project),
            '*' => Some(Self::Label),
            '@' => Some(Self::User),
            '!' => Some(Self::Priority),
            _ => None,
        }
    }

    const fn sigil(self) -> &'static str {
        match self {
            Self::Project => "+",
            Self::Label => "*",
            Self::User => "@",
            Self::Priority => "!",
            Self::Date => "",
        }
    }
}

/// Which help `?` or F1 asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Help {
    /// Every token, briefly.
    General,
    /// What can go in one slot.
    Slot(Slot),
}

/// What is shown under the line: a list of matches, or help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aside {
    /// One line saying what the items are.
    pub heading: String,
    /// The items, laid out by [`Aside::lines`].
    pub items: Vec<String>,
}

impl Aside {
    /// The aside as at most `rows` lines of at most `width` cells: the heading, then the
    /// items flowed across the width two spaces apart. When they do not all fit, the
    /// last line says how many were left out rather than cutting off silently.
    #[must_use]
    pub fn lines(&self, width: u16, rows: usize) -> Vec<String> {
        let room = usize::from(width.max(1));
        // Each flowed line, with how many items it holds.
        let mut flowed: Vec<(String, usize)> = Vec::new();
        for item in &self.items {
            let fits = flowed.last().is_some_and(|(line, _)| {
                usize::from(crate::rows::display_width(line))
                    + 2
                    + usize::from(crate::rows::display_width(item))
                    <= room
            });
            match flowed.last_mut() {
                Some((line, count)) if fits => {
                    line.push_str("  ");
                    line.push_str(item);
                    *count += 1;
                }
                _ => flowed.push((item.clone(), 1)),
            }
        }
        let mut lines = vec![self.heading.clone()];
        let body = rows.saturating_sub(1);
        if flowed.len() <= body {
            lines.extend(flowed.into_iter().map(|(line, _)| line));
        } else {
            let kept = body.saturating_sub(1);
            let shown: usize = flowed.iter().take(kept).map(|(_, count)| count).sum();
            lines.extend(flowed.into_iter().take(kept).map(|(line, _)| line));
            lines.push(format!("… and {} more", self.items.len() - shown));
        }
        lines
            .into_iter()
            .map(|line| crate::rows::truncate(&line, width))
            .collect()
    }
}

/// What a Tab press would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The bytes of the line to replace, ending at the cursor.
    pub range: Range<usize>,
    /// What to put there, or `None` when Tab cannot get any further on its own.
    pub text: Option<String>,
    /// Every name that matches what was typed, as it would be written in the line.
    pub matches: Vec<String>,
}

/// The partial token being typed at the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Partial {
    /// What it is.
    slot: Slot,
    /// Where it starts in the line, in bytes: after the sigil, at any quote or bracket.
    start: usize,
    /// The quote or bracket it was opened with, if any.
    open: Option<char>,
}

/// What is being typed at the end of `before`, the text before the cursor.
fn slot_at(before: &str) -> Option<Partial> {
    // An opened `+[Home Imp` first, since the name in it may already hold a space.
    if let Some(open) = open_bracket(before) {
        return Some(open);
    }
    let word_start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(at, c)| at + c.len_utf8());
    let word = &before[word_start..];
    if let Some(slot) = word.chars().next().and_then(Slot::from_sigil) {
        let rest = &word[1..];
        if slot == Slot::Priority && !rest.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        return Some(Partial {
            slot,
            start: word_start + 1,
            open: None,
        });
    }
    date_at(before).map(|start| Partial {
        slot: Slot::Date,
        start,
        open: None,
    })
}

/// A sigil opened with a quote or bracket and not closed yet, at a word start.
fn open_bracket(before: &str) -> Option<Partial> {
    let mut last = None;
    let mut previous: Option<char> = None;
    let mut chars = before.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let at_word_start = previous.is_none_or(char::is_whitespace);
        if let (true, Some(slot), Some(&(_, open))) =
            (at_word_start, Slot::from_sigil(c), chars.peek())
        {
            if matches!(open, '[' | '(' | '"' | '\'') && slot != Slot::Priority {
                last = Some((slot, at + c.len_utf8(), open));
            }
        }
        previous = Some(c);
    }
    let (slot, start, open) = last?;
    let inside = &before[start + open.len_utf8()..];
    (!inside.contains(closer(open))).then_some(Partial {
        slot,
        start,
        open: Some(open),
    })
}

const fn closer(open: char) -> char {
    match open {
        '[' => ']',
        '(' => ')',
        other => other,
    }
}

/// A `+`, `*` or `@` opened with a bracket or quote that nothing closes.
///
/// The parser reads `+[Home Improvement]` whole only once it has its `]`. Without one it
/// falls back to splitting on spaces, so `+[Home` becomes the project and the rest goes
/// into the title -- which is exactly what a Tab that completed as far as `+[Home ` and
/// then an Enter would produce. So neither prompt sends a line while this says something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unclosed {
    /// The token as far as its first word: `+[Home`.
    pub token: String,
    /// What would close it.
    pub close: char,
}

impl Unclosed {
    /// One line saying what is wrong and what to do.
    #[must_use]
    pub fn hint(&self) -> String {
        format!(
            "{} is not closed: finish the name and type {}, or press Tab",
            self.token, self.close
        )
    }
}

/// The first token in `line` whose bracket or quote is never closed, read the way the
/// parser splits words: a sigil and an opener at a word start take everything to the
/// matching close. `!` is left out, because an unclosed `![` is only ever title text.
#[must_use]
pub fn unclosed(line: &str) -> Option<Unclosed> {
    let mut at = 0;
    while at < line.len() {
        at += line[at..].find(|c: char| !c.is_whitespace())?;
        let mut chars = line[at..].chars();
        let sigil = chars.next().and_then(Slot::from_sigil);
        let open = chars.next();
        if let (Some(slot), Some(open @ ('[' | '(' | '"' | '\''))) = (sigil, open) {
            let inside = at + 1 + open.len_utf8();
            match line[inside..].find(closer(open)) {
                Some(offset) => {
                    at = inside + offset + closer(open).len_utf8();
                    continue;
                }
                None if slot != Slot::Priority => {
                    let end = line[at..]
                        .find(char::is_whitespace)
                        .map_or(line.len(), |offset| at + offset);
                    return Some(Unclosed {
                        token: line[at..end].to_string(),
                        close: closer(open),
                    });
                }
                None => {}
            }
        }
        at = line[at..]
            .find(char::is_whitespace)
            .map_or(line.len(), |offset| at + offset);
    }
    None
}

/// Where a date phrase being typed after `due ` or `start ` begins, if one is.
///
/// The phrase may already be a word or two in (`due next w`), so this looks back over the
/// last few words for the keyword, and only counts what follows it as a date being typed
/// while some date word still begins that way. `due tomorrow buy` is a title by then.
fn date_at(before: &str) -> Option<usize> {
    let words: Vec<(usize, &str)> = word_spans(before);
    let ends_in_space = before.ends_with(char::is_whitespace);
    // The words already finished, and the one being typed (empty after a space).
    let finished = if ends_in_space {
        words.len()
    } else {
        words.len().saturating_sub(1)
    };
    for back in 1..=3.min(finished) {
        let (at, word) = words[finished - back];
        if word.eq_ignore_ascii_case("due") || word.eq_ignore_ascii_case("start") {
            let start = at + word.len();
            let start = start
                + before[start..]
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(before.len() - start);
            let partial = before[start..].to_lowercase();
            return DATE_WORDS
                .iter()
                .any(|date| date.starts_with(&partial))
                .then_some(start);
        }
    }
    None
}

fn word_spans(text: &str) -> Vec<(usize, &str)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(from)) => {
                spans.push((from, &text[from..at]));
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    if let Some(from) = start {
        spans.push((from, &text[from..]));
    }
    spans
}

/// Whether a name written into the line is all of it, and so is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extent {
    /// The whole name: close the bracket or quote.
    Whole,
    /// Only the prefix several names share: leave it open for the rest.
    Prefix,
}

/// Write `name` as it goes in the line after its sigil: bracketed when it holds a space
/// (the form the parser takes whole), in whatever quote the user opened, and closed only
/// when `extent` is [`Extent::Whole`].
fn written(slot: Slot, name: &str, open: Option<char>, extent: Extent) -> String {
    if slot == Slot::Date {
        return name.to_string();
    }
    let open = open.or_else(|| name.contains(char::is_whitespace).then_some('['));
    match (open, extent) {
        (Some(open), Extent::Whole) => format!("{open}{name}{}", closer(open)),
        (Some(open), Extent::Prefix) => format!("{open}{name}"),
        (None, _) => name.to_string(),
    }
}

/// The longest prefix every name shares, ignoring case, in the first name's spelling.
fn common_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut end = first.len();
    for name in &names[1..] {
        let shared = first
            .char_indices()
            .zip(name.chars())
            .find(|((_, a), b)| !a.to_lowercase().eq(b.to_lowercase()))
            .map_or_else(
                || {
                    first
                        .char_indices()
                        .nth(name.chars().count())
                        .map_or(first.len(), |(at, _)| at)
                },
                |((at, _), _)| at,
            );
        end = end.min(shared);
    }
    first[..end].to_string()
}

/// What Tab does at the end of `before`, the text before the cursor.
///
/// `None` when the cursor is not in a slot -- after no sigil and no `due ` -- or when
/// nothing matches. A single match is completed whole, closed, with a space after it; several
/// are completed as far as they agree, and [`Completion::matches`] holds them for a
/// second Tab to list.
#[must_use]
pub fn complete(before: &str, vocabulary: &Vocabulary) -> Option<Completion> {
    let Partial { slot, start, open } = slot_at(before)?;
    let partial = &before[start..];
    let typed = open.map_or(partial, |open| &partial[open.len_utf8()..]);
    let lower = typed.to_lowercase();
    let names: Vec<String> = vocabulary
        .names(slot)
        .into_iter()
        .filter(|name| name.to_lowercase().starts_with(&lower))
        .collect();
    if names.is_empty() {
        return None;
    }
    let matches = names
        .iter()
        .map(|name| {
            format!(
                "{}{}",
                slot.sigil(),
                written(slot, name, open, Extent::Whole)
            )
        })
        .collect();
    let text = if let [only] = names.as_slice() {
        Some(format!("{} ", written(slot, only, open, Extent::Whole)))
    } else {
        let prefix = common_prefix(&names);
        (prefix.chars().count() > typed.chars().count())
            .then(|| written(slot, &prefix, open, Extent::Prefix))
    };
    Some(Completion {
        range: start..before.len(),
        text,
        matches,
    })
}

/// Whether `?` typed with `before` ahead of the cursor asks for help, and which.
///
/// Only on an empty line, straight after a sigil that starts a word, or straight after
/// `due ` or `start `. Anywhere else it is a question mark, because titles have them.
#[must_use]
pub fn help_at(value: &str, before: &str) -> Option<Help> {
    if value.is_empty() {
        return Some(Help::General);
    }
    let mut back = before.chars().rev();
    let last = back.next()?;
    if let Some(slot) = Slot::from_sigil(last) {
        return back
            .next()
            .is_none_or(char::is_whitespace)
            .then_some(Help::Slot(slot));
    }
    if last.is_whitespace() {
        let word = before.split_whitespace().next_back()?;
        if word.eq_ignore_ascii_case("due") || word.eq_ignore_ascii_case("start") {
            return Some(Help::Slot(Slot::Date));
        }
    }
    None
}

/// The text a help request shows.
#[must_use]
pub fn help(which: Help, vocabulary: &Vocabulary) -> Aside {
    let names = |slot: Slot, none: &str| -> Vec<String> {
        let names = vocabulary.names(slot);
        if names.is_empty() {
            vec![none.to_string()]
        } else {
            names
                .iter()
                .map(|name| {
                    format!(
                        "{}{}",
                        slot.sigil(),
                        written(slot, name, None, Extent::Whole)
                    )
                })
                .collect()
        }
    };
    match which {
        Help::General => Aside {
            heading: "Anywhere in the line:".to_string(),
            items: GENERAL_HELP
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        },
        Help::Slot(Slot::Project) => Aside {
            heading: "+project -- file it in a project, by title or +#id:".to_string(),
            items: names(Slot::Project, "(no projects synced yet -- run tui-do once)"),
        },
        Help::Slot(Slot::Label) => Aside {
            heading: "*label -- attach a label that exists:".to_string(),
            items: names(Slot::Label, "(no labels synced yet)"),
        },
        Help::Slot(Slot::User) => Aside {
            heading: "@user -- assign someone:".to_string(),
            items: names(Slot::User, "(no users known yet)"),
        },
        Help::Slot(Slot::Priority) => Aside {
            heading: "!priority:".to_string(),
            items: vec![
                "!1 lowest".to_string(),
                "!2".to_string(),
                "!3".to_string(),
                "!4".to_string(),
                "!5 highest".to_string(),
            ],
        },
        Help::Slot(Slot::Date) => Aside {
            heading: "a date: a word, a month name, or digits with a four-digit year".to_string(),
            items: DATE_WORDS
                .iter()
                .map(|word| (*word).to_string())
                .chain(
                    [
                        "in 3 days",
                        "aug 27",
                        "27aug26",
                        "2026-08-27",
                        "27.8",
                        "… at 5pm",
                    ]
                    .map(String::from),
                )
                .collect(),
        },
    }
}

/// The general help, one token to a line.
const GENERAL_HELP: &[&str] = &[
    "+project   +Legal, +[Home Improvement], +#12",
    "*label     *urgent, *[needs review]",
    "@user      @admin",
    "!1 .. !5   priority, lowest to highest",
    "due <date> tomorrow, friday, next week, 2026-08-27",
    "start <date>   when work can begin",
    "every <n> <unit>   every 2 weeks, every month",
    "Tab completes  ? after + * @ ! due start lists  Enter adds  Esc cancels",
];

/// A quick-add line with completion and help: the field, what it can complete to, and
/// what is shown under it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuickAddInput {
    /// The line itself.
    pub input: TextInput,
    vocabulary: Vocabulary,
    /// The line as the last Tab left it, so a second Tab on the same line lists instead
    /// of completing again.
    tabbed: Option<String>,
    aside: Option<Aside>,
}

impl QuickAddInput {
    /// An empty line that completes from `vocabulary`.
    #[must_use]
    pub fn new(vocabulary: Vocabulary) -> Self {
        Self {
            vocabulary,
            ..Self::default()
        }
    }

    /// The text so far.
    #[must_use]
    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// What is shown under the line, if anything.
    #[must_use]
    pub const fn aside(&self) -> Option<&Aside> {
        self.aside.as_ref()
    }

    /// Apply a key press. Returns whether it was used.
    ///
    /// Tab, F1 and a `?` that asks for help are taken here; everything else goes to the
    /// field, and clears whatever was shown under it.
    pub fn press(&mut self, key: Key) -> bool {
        match key.code {
            KeyCode::Tab => {
                self.tab();
                true
            }
            KeyCode::F(1) => {
                self.aside = Some(help(Help::General, &self.vocabulary));
                true
            }
            KeyCode::Char('?') if !key.mods.contains(KeyModifiers::CONTROL) => {
                let before = &self.input.value()[..self.input.cursor()];
                if let Some(which) = help_at(self.input.value(), before) {
                    self.aside = Some(help(which, &self.vocabulary));
                    self.tabbed = None;
                    true
                } else {
                    self.typed(key)
                }
            }
            _ => self.typed(key),
        }
    }

    /// The line to add, or `None` with the reason shown under it.
    ///
    /// Refused while a bracket or quote is open (see [`unclosed`]): sending it would put
    /// half the name in the title.
    pub fn submit(&mut self) -> Option<String> {
        if let Some(open) = unclosed(self.input.value()) {
            self.aside = Some(Aside {
                heading: open.hint(),
                items: Vec::new(),
            });
            self.tabbed = None;
            return None;
        }
        Some(self.input.value().to_string())
    }

    fn typed(&mut self, key: Key) -> bool {
        self.aside = None;
        self.tabbed = None;
        self.input.press(key)
    }

    fn tab(&mut self) {
        let value = self.input.value().to_string();
        let before = &value[..self.input.cursor()];
        let Some(completion) = complete(before, &self.vocabulary) else {
            self.aside = None;
            self.tabbed = None;
            return;
        };
        let again = self.tabbed.as_deref() == Some(value.as_str());
        match completion.text {
            Some(mut text) if !again => {
                // No second space when one already follows the cursor.
                if text.ends_with(' ')
                    && value[self.input.cursor()..].starts_with(char::is_whitespace)
                {
                    text.pop();
                }
                self.input.replace(completion.range, &text);
                self.aside = None;
            }
            _ if completion.matches.len() > 1 && again => {
                self.aside = Some(Aside {
                    heading: format!("{} match:", completion.matches.len()),
                    items: completion.matches,
                });
            }
            _ => {}
        }
        self.tabbed = Some(self.input.value().to_string());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn vocabulary() -> Vocabulary {
        Vocabulary {
            projects: tidy(
                [
                    "Legal",
                    "Leisure",
                    "Home Improvement",
                    "Home Office",
                    "Inbox",
                ]
                .map(String::from),
            ),
            labels: tidy(["urgent", "upstream", "needs review"].map(String::from)),
            users: tidy(["admin", "alice"].map(String::from)),
        }
    }

    fn typed(text: &str) -> QuickAddInput {
        let mut line = QuickAddInput::new(vocabulary());
        for c in text.chars() {
            line.press(Key::char(c));
        }
        line
    }

    fn tab(line: &mut QuickAddInput) {
        line.press(Key::plain(KeyCode::Tab));
    }

    #[test]
    fn a_single_match_completes_whole() {
        let mut line = typed("Call the VA +Leg");
        tab(&mut line);
        assert_eq!(line.value(), "Call the VA +Legal ");
        let mut line = typed("*urg");
        tab(&mut line);
        assert_eq!(line.value(), "*urgent ");
        let mut line = typed("@ad");
        tab(&mut line);
        assert_eq!(line.value(), "@admin ");
    }

    #[test]
    fn the_only_project_starting_le_completes_from_le() {
        let mut line = QuickAddInput::new(Vocabulary {
            projects: vec!["Legal".into(), "Inbox".into()],
            ..Vocabulary::default()
        });
        for c in "+Le".chars() {
            line.press(Key::char(c));
        }
        tab(&mut line);
        assert_eq!(line.value(), "+Legal ");
    }

    #[test]
    fn case_does_not_matter_and_the_name_is_spelt_as_stored() {
        let mut line = typed("+leg");
        tab(&mut line);
        assert_eq!(line.value(), "+Legal ");
    }

    #[test]
    fn an_ambiguous_prefix_completes_as_far_as_it_agrees_then_lists() {
        let mut line = typed("+L");
        tab(&mut line);
        assert_eq!(line.value(), "+Le", "the longest common prefix");
        assert!(line.aside().is_none(), "one Tab does not list");
        tab(&mut line);
        assert_eq!(
            line.value(),
            "+Le",
            "the second Tab does not change the line"
        );
        let aside = line.aside().expect("the second Tab lists");
        assert_eq!(aside.items, vec!["+Legal", "+Leisure"]);
        // Typing on clears the list.
        line.press(Key::char('g'));
        assert!(line.aside().is_none());
        tab(&mut line);
        assert_eq!(line.value(), "+Legal ");
    }

    #[test]
    fn a_tab_that_gets_no_further_lists_on_the_next() {
        let mut line = typed("+Le");
        tab(&mut line);
        assert_eq!(line.value(), "+Le");
        assert!(line.aside().is_none());
        tab(&mut line);
        assert_eq!(line.aside().unwrap().items.len(), 2);
    }

    #[test]
    fn a_name_with_a_space_completes_in_the_bracket_form() {
        let mut line = typed("Fix the door +Home I");
        tab(&mut line);
        // `+Home` is a whole word to the parser and `I` the title, so this is not a slot.
        assert_eq!(line.value(), "Fix the door +Home I");

        let mut line = typed("Fix the door +Ho");
        tab(&mut line);
        assert_eq!(
            line.value(),
            "Fix the door +[Home ",
            "the common prefix opens the bracket so the next word stays in the name"
        );
        line.press(Key::char('I'));
        tab(&mut line);
        assert_eq!(line.value(), "Fix the door +[Home Improvement] ");

        let mut line = typed("*need");
        tab(&mut line);
        assert_eq!(line.value(), "*[needs review] ");
    }

    #[test]
    fn an_opened_quote_is_kept() {
        let mut line = typed("+\"Home O");
        tab(&mut line);
        assert_eq!(line.value(), "+\"Home Office\" ");
    }

    #[test]
    fn a_date_word_completes_after_due_or_start() {
        let mut line = typed("Pay rent due tom");
        tab(&mut line);
        assert_eq!(line.value(), "Pay rent due tomorrow ");
        let mut line = typed("Plan start next fr");
        tab(&mut line);
        assert_eq!(line.value(), "Plan start next friday ");
        let mut line = typed("due end of m");
        tab(&mut line);
        assert_eq!(line.value(), "due end of month ");
    }

    #[test]
    fn no_sigil_and_no_keyword_completes_nothing() {
        for text in [
            "Call the VA",
            "Le",
            "tom",
            "a+Le",
            "due tomorrow buy mi",
            "2*3",
        ] {
            let mut line = typed(text);
            tab(&mut line);
            assert_eq!(line.value(), text, "{text:?}");
            assert!(line.aside().is_none(), "{text:?}");
        }
    }

    #[test]
    fn a_completion_replaces_only_up_to_the_cursor() {
        let mut line = typed("+Leg buy milk");
        for _ in 0.."buy milk".len() + 1 {
            line.press(Key::plain(KeyCode::Left));
        }
        tab(&mut line);
        assert_eq!(
            line.value(),
            "+Legal buy milk",
            "no second space before the next word"
        );
    }

    #[test]
    fn question_mark_after_a_sigil_shows_help_and_inserts_nothing() {
        for (text, heading) in [
            ("Call the VA +", "+project"),
            ("*", "*label"),
            ("x @", "@user"),
            ("x !", "!priority"),
        ] {
            let mut line = typed(text);
            line.press(Key::char('?'));
            assert_eq!(line.value(), text, "the ? was inserted after {text:?}");
            let aside = line.aside().expect("help is shown");
            assert!(aside.heading.starts_with(heading), "{text:?}: {aside:?}");
        }
        let mut line = typed("+");
        line.press(Key::char('?'));
        assert!(line
            .aside()
            .unwrap()
            .items
            .contains(&"+[Home Improvement]".to_string()));
    }

    #[test]
    fn question_mark_after_due_or_on_an_empty_line_shows_help() {
        let mut line = typed("Pay rent due ");
        line.press(Key::char('?'));
        assert_eq!(line.value(), "Pay rent due ");
        assert!(line
            .aside()
            .unwrap()
            .items
            .contains(&"tomorrow".to_string()));

        let mut line = typed("start ");
        line.press(Key::char('?'));
        assert_eq!(line.value(), "start ");
        assert!(line.aside().is_some());

        let mut line = typed("");
        line.press(Key::char('?'));
        assert_eq!(line.value(), "");
        assert_eq!(line.aside().unwrap().heading, "Anywhere in the line:");
    }

    #[test]
    fn a_question_mark_anywhere_else_is_the_character() {
        for text in ["Is that the date?", "why?", "+Legal?", "a+?", "due?"] {
            let line = typed(text);
            assert_eq!(line.value(), text);
            assert!(line.aside().is_none(), "{text:?}");
        }
    }

    #[test]
    fn a_line_with_an_open_bracket_is_not_submitted() {
        // The Tab that completes as far as names agree leaves the bracket open.
        let mut line = typed("Fix the door +Ho");
        tab(&mut line);
        assert_eq!(line.value(), "Fix the door +[Home ");
        assert_eq!(line.submit(), None, "half a name would go in the title");
        let hint = &line.aside().expect("the reason is shown").heading;
        assert!(hint.starts_with("+[Home is not closed"), "{hint}");
        assert!(hint.contains(']'), "{hint}");

        // Finishing it, by hand or by Tab, lets it through.
        line.press(Key::char('O'));
        tab(&mut line);
        assert_eq!(
            line.submit().as_deref(),
            Some("Fix the door +[Home Office] ")
        );
    }

    #[test]
    fn unclosed_reads_the_line_as_the_parser_splits_it() {
        for (line, token) in [
            ("+[Home", "+[Home"),
            ("Ship it *\"needs review", "*\"needs"),
            ("@(Ada Lovelace", "@(Ada"),
            ("+[Done] then *[open one", "*[open"),
        ] {
            assert_eq!(
                unclosed(line).map(|open| open.token),
                Some(token.to_string()),
                "{line:?}"
            );
        }
        for line in [
            "",
            "+[Home Improvement] fix the door",
            "*\"needs review\" ship it",
            "don't forget the milk",
            "a+[b",
            "![nothing",
            "costs [a lot",
        ] {
            assert_eq!(unclosed(line), None, "{line:?}");
        }
    }

    #[test]
    fn f1_is_always_the_general_help() {
        let mut line = typed("Is that the date");
        line.press(Key::plain(KeyCode::F(1)));
        assert_eq!(line.value(), "Is that the date");
        assert_eq!(line.aside().unwrap().heading, "Anywhere in the line:");
    }

    #[test]
    fn every_date_word_is_one_the_parser_reads() {
        use chrono::TimeZone as _;
        let now = chrono::FixedOffset::west_opt(5 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 8, 26, 10, 0, 0)
            .unwrap();
        for word in DATE_WORDS {
            let parsed = tui_do_core::quickadd::parse(&format!("Pay rent due {word}"), &now);
            assert!(parsed.due_date.is_some(), "due {word} set no date");
            assert_eq!(parsed.title, "Pay rent", "due {word} stayed in the title");
        }
    }

    #[test]
    fn the_vocabulary_drops_pseudo_and_archived_projects() {
        use tui_do_core::models::ProjectId;
        let projects = [
            Project {
                id: ProjectId(-1),
                title: "Favorites".into(),
                ..Project::default()
            },
            Project {
                id: ProjectId(3),
                title: "Old".into(),
                is_archived: true,
                ..Project::default()
            },
            Project {
                id: ProjectId(4),
                title: "legal".into(),
                ..Project::default()
            },
            Project {
                id: ProjectId(5),
                title: "Admin".into(),
                ..Project::default()
            },
        ];
        let vocabulary = Vocabulary::new(&projects, &[], ["bob".to_string(), "bob".to_string()]);
        assert_eq!(vocabulary.projects, vec!["Admin", "legal"]);
        assert_eq!(vocabulary.users, vec!["bob"]);
    }

    #[test]
    fn a_long_list_says_how_many_it_left_out() {
        let aside = Aside {
            heading: "many:".into(),
            items: (0..40).map(|n| format!("+project{n:02}")).collect(),
        };
        let lines = aside.lines(40, 4);
        assert_eq!(lines.len(), 4);
        let last = lines.last().unwrap();
        assert!(last.starts_with("… and "), "{lines:?}");
        let shown: usize = lines[1..3].iter().map(|l| l.split("  ").count()).sum();
        assert_eq!(last, &format!("… and {} more", 40 - shown));
    }
}
