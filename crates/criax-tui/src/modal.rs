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

use criax_core::models::{LabelId, ProjectId, Task};
use crossterm::event::{KeyCode, KeyModifiers};
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
    multiline: bool,
}

impl TextInput {
    /// A field holding `value`, with the cursor at the end.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            multiline: false,
        }
    }

    /// A field that takes Enter as a newline rather than leaving it to the modal.
    ///
    /// A description is the one task field that is genuinely several lines, and flattening
    /// one because the editor could not hold it would lose the user's text.
    #[must_use]
    pub fn multiline(value: impl Into<String>) -> Self {
        Self {
            multiline: true,
            ..Self::new(value)
        }
    }

    /// Whether Enter belongs to this field.
    #[must_use]
    pub const fn is_multiline(&self) -> bool {
        self.multiline
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
            KeyCode::Enter if self.multiline => {
                let at = self.byte_offset(self.cursor);
                self.value.insert(at, '\n');
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
    /// Typing a new task in quick-add syntax.
    Add(TextInput),
    /// Choosing a project or a label.
    Picker(PickerState),
    /// Editing every field of one task.
    Edit(Box<EditState>),
}

/// What a modal decided about a key.
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub enum Submission {
    /// Filter the list by this text. Empty clears the search.
    Search(String),
    /// Do what the chosen candidate says.
    Picked(Pick),
    /// Create a task from this quick-add text.
    Add(String),
    /// Apply this filled-in edit form.
    Edited(Box<EditDraft>),
}

/// Which field of the edit form has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    /// The task's title.
    Title,
    /// Its description, which may be several lines.
    Description,
    /// 0 for none, 1 to 5.
    Priority,
    /// Anything the quick-add parser understands: `tomorrow`, `24/12/2026`, `friday`.
    Due,
    /// A project name, resolved the way `+project` is.
    Project,
    /// Comma-separated label names, resolved the way `*label` is.
    Labels,
}

impl EditField {
    /// Every field, in the order they are shown and tabbed through.
    pub const ALL: [Self; 6] = [
        Self::Title,
        Self::Description,
        Self::Priority,
        Self::Due,
        Self::Project,
        Self::Labels,
    ];

    /// What this field is called on screen.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Description => "Description",
            Self::Priority => "Priority",
            Self::Due => "Due",
            Self::Project => "Project",
            Self::Labels => "Labels",
        }
    }

    fn step(self, forward: bool) -> Self {
        let at = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        let count = Self::ALL.len();
        let next = if forward {
            (at + 1) % count
        } else {
            (at + count - 1) % count
        };
        Self::ALL[next]
    }
}

/// The edit form, over one task.
#[derive(Debug, Clone, PartialEq)]
pub struct EditState {
    /// The task as it was.
    ///
    /// Kept whole rather than by id: it is what the mutation's `before` needs, what a
    /// rejection restores, and -- because assignees travel in the task body and an empty
    /// list clears them -- the only safe thing to build the write from.
    pub before: Box<Task>,
    /// The title field.
    pub title: TextInput,
    /// The description field, which takes Enter as a newline.
    pub description: TextInput,
    /// The priority field.
    pub priority: TextInput,
    /// The due-date field.
    pub due: TextInput,
    /// The project field.
    pub project: TextInput,
    /// The labels field.
    pub labels: TextInput,
    /// Which field has the keyboard.
    pub focus: EditField,
}

impl EditState {
    /// A form filled in from `task`.
    ///
    /// `project_name` and the label names are passed in rather than looked up, because
    /// the modal cannot reach the model and a form that showed ids would be a form nobody
    /// could edit.
    #[must_use]
    pub fn new(task: &Task, project_name: &str) -> Self {
        let labels = task
            .labels
            .iter()
            .map(|label| label.title.clone())
            .collect::<Vec<_>>()
            .join(", ");
        Self {
            title: TextInput::new(task.title.clone()),
            description: TextInput::multiline(task.description.clone()),
            priority: TextInput::new(task.priority.to_string()),
            due: TextInput::new(
                task.due_date
                    .get()
                    .map(|due| due.format("%d/%m/%Y").to_string())
                    .unwrap_or_default(),
            ),
            project: TextInput::new(project_name.to_string()),
            labels: TextInput::new(labels),
            focus: EditField::Title,
            before: Box::new(task.clone()),
        }
    }

    /// The field with the keyboard.
    pub fn current(&mut self) -> &mut TextInput {
        match self.focus {
            EditField::Title => &mut self.title,
            EditField::Description => &mut self.description,
            EditField::Priority => &mut self.priority,
            EditField::Due => &mut self.due,
            EditField::Project => &mut self.project,
            EditField::Labels => &mut self.labels,
        }
    }

    /// One field's text, for drawing.
    #[must_use]
    pub fn field(&self, which: EditField) -> &TextInput {
        match which {
            EditField::Title => &self.title,
            EditField::Description => &self.description,
            EditField::Priority => &self.priority,
            EditField::Due => &self.due,
            EditField::Project => &self.project,
            EditField::Labels => &self.labels,
        }
    }

    /// What the user typed, for `update` to resolve against the projects and labels it
    /// knows about. The modal deliberately resolves nothing itself.
    #[must_use]
    pub fn draft(&self) -> EditDraft {
        EditDraft {
            before: self.before.clone(),
            title: self.title.value().to_string(),
            description: self.description.value().to_string(),
            priority: self.priority.value().trim().to_string(),
            due: self.due.value().trim().to_string(),
            project: self.project.value().trim().to_string(),
            labels: self.labels.value().to_string(),
        }
    }
}

/// A filled-in edit form, before any of it has been understood.
///
/// Every field is still the text the user typed. Resolving a project name, a label list
/// or a date needs the model, and `update` is where the model lives -- so the modal hands
/// over strings and `update` decides what they mean, the same way the quick-add prompt
/// does.
#[derive(Debug, Clone, PartialEq)]
pub struct EditDraft {
    /// The task as it was, for the mutation's `before`.
    pub before: Box<Task>,
    /// The new title.
    pub title: String,
    /// The new description.
    pub description: String,
    /// Priority, as typed.
    pub priority: String,
    /// The due date, as typed.
    pub due: String,
    /// The project name, as typed.
    pub project: String,
    /// The label names, comma-separated, as typed.
    pub labels: String,
}

impl ModalView for EditState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Dismiss,
            // Saving is its own chord because Enter belongs to the description, and a
            // form that saved on Enter could not hold a second line.
            KeyCode::Char('s') if key.mods.contains(KeyModifiers::CONTROL) => {
                Outcome::Submit(Submission::Edited(Box::new(self.draft())))
            }
            KeyCode::Tab => {
                self.focus = self.focus.step(true);
                Outcome::Consumed
            }
            KeyCode::BackTab => {
                self.focus = self.focus.step(false);
                Outcome::Consumed
            }
            // Enter moves on, except in the one field that is genuinely several lines.
            KeyCode::Enter if !self.current().is_multiline() => {
                self.focus = self.focus.step(true);
                Outcome::Consumed
            }
            _ => {
                self.current().press(key);
                Outcome::Consumed
            }
        }
    }

    fn title(&self) -> String {
        "Edit task  —  Tab moves, Ctrl-S saves, Esc cancels".to_string()
    }
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

impl ModalView for TextInput {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Dismiss,
            KeyCode::Enter => Outcome::Submit(Submission::Add(self.value().to_string())),
            _ => {
                self.press(key);
                Outcome::Consumed
            }
        }
    }

    fn title(&self) -> String {
        "Add a task".to_string()
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
            Self::Add(state) => state,
            Self::Picker(state) => state,
            Self::Edit(state) => state.as_mut(),
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
            Self::Edit(state) => state.title(),
            Self::Help(state) => state.title(),
            Self::Search(state) => state.title(),
            Self::Add(state) => state.title(),
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
