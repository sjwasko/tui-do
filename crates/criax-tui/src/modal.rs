//! The modal stack.
//!
//! A modal is state, not a flag. There is no `show_help_modal: bool` anywhere in criax:
//! opening one pushes a variant onto [`crate::Model::modals`] and closing it pops. Illegal
//! combinations — a picker open with no picker state — are unrepresentable rather than
//! merely unlikely.
//!
//! Modals are **exclusive**. The top of the stack receives every key except quit and
//! resize, with no fall-through to the screen underneath, because "sometimes it leaks" is
//! how the predecessor's key handling grew to 790 lines.

use criax_core::models::{LabelId, ProjectId};
use crossterm::event::KeyCode;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::keymap::{help_rows, Action, Context, Key};

/// A single-line text field.
///
/// Cursor position is counted in characters, not bytes: a task title with an em dash in
/// it should not make the left arrow key panic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    value: String,
    cursor: usize,
}

impl TextInput {
    /// A field holding `value`, with the cursor at the end.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    /// The text so far.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Where the cursor sits, in characters from the start.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Apply a key press. Returns whether it was one this field acts on.
    ///
    /// Named `press` rather than `handle` so it cannot be confused with the
    /// [`ModalView::handle`] that wraps it — one returns "did I use this key", the other
    /// returns what should happen to the modal.
    pub fn press(&mut self, key: Key) -> bool {
        match key.code {
            KeyCode::Char(c) if !key.mods.contains(crossterm::event::KeyModifiers::CONTROL) => {
                let at = self.byte_offset(self.cursor);
                self.value.insert(at, c);
                self.cursor += 1;
                true
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte_offset(self.cursor - 1);
                self.value.remove(at);
                self.cursor -= 1;
                true
            }
            KeyCode::Delete if self.cursor < self.len() => {
                let at = self.byte_offset(self.cursor);
                self.value.remove(at);
                true
            }
            KeyCode::Left if self.cursor > 0 => {
                self.cursor -= 1;
                true
            }
            KeyCode::Right if self.cursor < self.len() => {
                self.cursor += 1;
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.len();
                true
            }
            _ => false,
        }
    }

    fn len(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_offset(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map_or(self.value.len(), |(at, _)| at)
    }
}

/// What choosing a candidate means.
///
/// The payload rather than a bare id, so a command picker cannot submit a project and a
/// project picker cannot submit an action. The alternative -- an `i64` plus the picker's
/// kind to interpret it -- makes that mistake a runtime possibility for no gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Show this project.
    Project(ProjectId),
    /// Filter by this label.
    Label(LabelId),
    /// Run this action, exactly as its key would.
    Command(Action),
}

/// One thing a picker can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What choosing it does.
    pub pick: Pick,
    /// What the user reads and types against.
    pub title: String,
    /// Shown dimmed on the right. The palette puts the key here, so using it by name
    /// teaches the binding.
    pub hint: String,
}

impl Candidate {
    /// A candidate with no hint.
    #[must_use]
    pub fn new(pick: Pick, title: impl Into<String>) -> Self {
        Self {
            pick,
            title: title.into(),
            hint: String::new(),
        }
    }

    /// The same, with a hint on the right.
    #[must_use]
    pub fn hinted(pick: Pick, title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            pick,
            title: title.into(),
            hint: hint.into(),
        }
    }
}

/// What a picker picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// A project to show.
    Project,
    /// A label to filter by.
    Label,
    /// A command to run.
    Command,
}

impl PickerKind {
    /// The modal's title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Project => "Go to project",
            Self::Label => "Go to label",
            Self::Command => "Run a command",
        }
    }
}

/// A fuzzy picker over a fixed candidate list.
#[derive(Debug, Clone)]
pub struct PickerState {
    /// What is being picked.
    pub kind: PickerKind,
    /// The query typed so far.
    pub input: TextInput,
    /// Everything offered, unfiltered.
    pub candidates: Vec<Candidate>,
    /// Indices into `candidates`, best match first.
    pub matches: Vec<usize>,
    /// Which match is highlighted, as a position in `matches`.
    pub selected: usize,
}

impl PartialEq for PickerState {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.input == other.input
            && self.candidates == other.candidates
            && self.matches == other.matches
            && self.selected == other.selected
    }
}

impl PickerState {
    /// A picker over `candidates`, with nothing typed.
    #[must_use]
    pub fn new(kind: PickerKind, candidates: Vec<Candidate>) -> Self {
        let matches = (0..candidates.len()).collect();
        Self {
            kind,
            input: TextInput::default(),
            candidates,
            matches,
            selected: 0,
        }
    }

    /// The candidate under the cursor.
    #[must_use]
    pub fn current(&self) -> Option<&Candidate> {
        self.matches
            .get(self.selected)
            .and_then(|index| self.candidates.get(*index))
    }

    fn refilter(&mut self) {
        let query = self.input.value();
        if query.is_empty() {
            self.matches = (0..self.candidates.len()).collect();
        } else {
            let matcher = SkimMatcherV2::default();
            let mut scored: Vec<(i64, usize)> = self
                .candidates
                .iter()
                .enumerate()
                .filter_map(|(index, candidate)| {
                    matcher
                        .fuzzy_match(&candidate.title, query)
                        .map(|score| (score, index))
                })
                .collect();
            // Highest score first, ties broken by the original order so the list does not
            // shuffle while the user is still typing.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            self.matches = scored.into_iter().map(|(_, index)| index).collect();
        }
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }
}

/// An incremental search.
///
/// Holds what the list was filtered by *before* the search opened, because the results
/// update as the user types: without it, `Esc` could only clear the filter, never restore
/// the one they started from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    /// What has been typed.
    pub input: TextInput,
    /// The filter to restore if the search is abandoned.
    pub original: Option<String>,
}

impl SearchState {
    /// A search that starts from `original`, prefilled with it.
    #[must_use]
    pub fn new(original: Option<String>) -> Self {
        Self {
            input: TextInput::new(original.clone().unwrap_or_default()),
            original,
        }
    }
}

/// Help, rendered from the keymap table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HelpState {
    /// Which pane's bindings to include, alongside the global ones.
    pub context: Context,
    /// First visible line, for scrolling.
    pub offset: usize,
}

/// An open modal.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// The key reference.
    Help(HelpState),
    /// Searching the current list, incrementally.
    Search(SearchState),
    /// Choosing a project or a label.
    Picker(PickerState),
}

/// What a modal decided about a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Handled; nothing else should see it.
    Consumed,
    /// Close me.
    Dismiss,
    /// Close me and do this.
    Submit(Submission),
    /// Do this, but stay open. What makes a search filter as it is typed.
    Update(Submission),
}

/// What a modal asks `update` to do when it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    /// Filter the list by this text. Empty clears the search.
    Search(String),
    /// Do what the chosen candidate says.
    Picked(Pick),
}

/// Behaviour every modal has.
///
/// The trait exists so the modal stack does not care which variant is on top, and so a
/// new modal is a type plus one match arm rather than a branch in a key dispatcher.
pub trait ModalView {
    /// React to a key.
    fn handle(&mut self, key: Key) -> Outcome;

    /// The title drawn in the border.
    fn title(&self) -> String;
}

impl ModalView for HelpState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                // Bounded by the content, so the last press cannot scroll the text off
                // the top and leave the reader staring at an empty box.
                let last = help_rows(self.context).len().saturating_sub(1);
                self.offset = (self.offset + 1).min(last);
                Outcome::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.offset = self.offset.saturating_sub(1);
                Outcome::Consumed
            }
            // Any other key dismisses: a help screen you have to learn to close is a
            // poor joke to play on someone who just pressed `?`.
            _ => Outcome::Dismiss,
        }
    }

    fn title(&self) -> String {
        "Keys".to_string()
    }
}

impl ModalView for SearchState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            // Abandoning restores what was showing before, which is not the same as
            // clearing: a search opened over an existing filter has one to go back to.
            KeyCode::Esc => Outcome::Submit(Submission::Search(
                self.original.clone().unwrap_or_default(),
            )),
            // The results are already filtered; Enter just gets the prompt out of the way.
            KeyCode::Enter => Outcome::Dismiss,
            _ => {
                if self.input.press(key) {
                    Outcome::Update(Submission::Search(self.input.value().to_string()))
                } else {
                    Outcome::Consumed
                }
            }
        }
    }

    fn title(&self) -> String {
        "Search".to_string()
    }
}

impl ModalView for PickerState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Dismiss,
            KeyCode::Enter => match self.current() {
                Some(candidate) => Outcome::Submit(Submission::Picked(candidate.pick)),
                // Nothing matched what they typed; closing silently would look like the
                // key was swallowed.
                None => Outcome::Consumed,
            },
            KeyCode::Down | KeyCode::Tab => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1) % self.matches.len();
                }
                Outcome::Consumed
            }
            KeyCode::Up | KeyCode::BackTab => {
                if !self.matches.is_empty() {
                    self.selected = self
                        .selected
                        .checked_sub(1)
                        .unwrap_or(self.matches.len() - 1);
                }
                Outcome::Consumed
            }
            _ => {
                if self.input.press(key) {
                    self.refilter();
                }
                Outcome::Consumed
            }
        }
    }

    fn title(&self) -> String {
        self.kind.title().to_string()
    }
}

impl Modal {
    /// The modal as its behaviour.
    pub fn as_view_mut(&mut self) -> &mut dyn ModalView {
        match self {
            Self::Help(state) => state,
            Self::Search(state) => state,
            Self::Picker(state) => state,
        }
    }

    /// React to a key.
    pub fn handle(&mut self, key: Key) -> Outcome {
        self.as_view_mut().handle(key)
    }

    /// The title drawn in the border.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Help(state) => state.title(),
            Self::Search(state) => state.title(),
            Self::Picker(state) => state.title(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key(c: char) -> Key {
        Key::char(c)
    }

    fn code(code: KeyCode) -> Key {
        Key::plain(code)
    }

    #[test]
    fn typing_and_editing_move_the_cursor_by_characters_not_bytes() {
        let mut input = TextInput::default();
        for c in "café".chars() {
            input.press(Key::char(c));
        }
        assert_eq!(input.value(), "café");
        assert_eq!(input.cursor(), 4);

        input.press(code(KeyCode::Left));
        input.press(code(KeyCode::Backspace));
        assert_eq!(input.value(), "caé");
    }

    #[test]
    fn a_picker_orders_by_match_quality_and_submits_the_highlighted_one() {
        let mut picker = PickerState::new(
            PickerKind::Project,
            vec![
                Candidate::new(Pick::Project(ProjectId(1)), "Personal"),
                Candidate::new(Pick::Project(ProjectId(2)), "Work"),
                Candidate::new(Pick::Project(ProjectId(3)), "Weekend work"),
            ],
        );
        picker.handle(key('w'));
        picker.handle(key('o'));
        assert_eq!(picker.current().unwrap().title, "Work");

        let outcome = picker.handle(code(KeyCode::Enter));
        assert_eq!(
            outcome,
            Outcome::Submit(Submission::Picked(Pick::Project(ProjectId(2))))
        );
    }

    #[test]
    fn a_picker_with_no_matches_does_not_submit_something_arbitrary() {
        let mut picker = PickerState::new(
            PickerKind::Label,
            vec![Candidate::new(Pick::Label(LabelId(7)), "urgent")],
        );
        for c in "zzz".chars() {
            picker.handle(key(c));
        }
        assert!(picker.current().is_none());
        assert_eq!(picker.handle(code(KeyCode::Enter)), Outcome::Consumed);
    }

    #[test]
    fn picker_selection_wraps_in_both_directions() {
        let mut picker = PickerState::new(
            PickerKind::Project,
            vec![
                Candidate::new(Pick::Project(ProjectId(1)), "one"),
                Candidate::new(Pick::Project(ProjectId(2)), "two"),
            ],
        );
        picker.handle(code(KeyCode::Up));
        assert_eq!(picker.current().unwrap().pick, Pick::Project(ProjectId(2)));
        picker.handle(code(KeyCode::Down));
        assert_eq!(picker.current().unwrap().pick, Pick::Project(ProjectId(1)));
    }

    #[test]
    fn a_search_filters_as_it_is_typed_and_enter_only_puts_the_prompt_away() {
        let mut modal = Modal::Search(SearchState::new(None));
        // Every keystroke asks for the narrower list, without closing.
        assert_eq!(
            modal.handle(key('b')),
            Outcome::Update(Submission::Search("b".to_string()))
        );
        assert_eq!(
            modal.handle(key('u')),
            Outcome::Update(Submission::Search("bu".to_string()))
        );
        // The results are already filtered, so Enter has nothing left to apply.
        assert_eq!(modal.handle(code(KeyCode::Enter)), Outcome::Dismiss);
    }

    #[test]
    fn abandoning_a_search_restores_the_filter_it_opened_over() {
        // Not the same as clearing: a search started over an existing filter has
        // something to go back to, and typing has already replaced it.
        let mut modal = Modal::Search(SearchState::new(Some("urgent".to_string())));
        modal.handle(code(KeyCode::Backspace));
        assert_eq!(
            modal.handle(code(KeyCode::Esc)),
            Outcome::Submit(Submission::Search("urgent".to_string()))
        );

        let mut fresh = Modal::Search(SearchState::new(None));
        fresh.handle(key('x'));
        assert_eq!(
            fresh.handle(code(KeyCode::Esc)),
            Outcome::Submit(Submission::Search(String::new())),
            "an empty string clears the filter"
        );
    }

    #[test]
    fn help_scrolls_but_any_other_key_closes_it() {
        let mut modal = Modal::Help(HelpState::default());
        assert_eq!(modal.handle(key('j')), Outcome::Consumed);
        assert_eq!(modal.handle(key('x')), Outcome::Dismiss);
    }
}
