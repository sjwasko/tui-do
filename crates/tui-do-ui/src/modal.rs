//! The modal stack.
//!
//! A modal is state, not a flag. There is no `show_help_modal: bool` anywhere in tui-do:
//! opening one pushes a variant onto [`crate::Model::modals`] and closing it pops. Illegal
//! combinations — a picker open with no picker state — are unrepresentable rather than
//! merely unlikely.
//!
//! Modals are **exclusive**. The top of the stack receives every key except quit and
//! resize, with no fall-through to the screen underneath, because "sometimes it leaks" is
//! how the predecessor's key handling grew to 790 lines.

use crossterm::event::{KeyCode, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use tui_do_core::models::{Label, LabelId, ProjectId, Task};

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
            // Readline's kill-line, and the reason a prefilled field is practical: `D`
            // opens on the date the task already has, and replacing it would otherwise
            // mean ten presses of Backspace before the first useful keystroke.
            KeyCode::Char('u') if key.mods.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
                true
            }
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
    /// Move the selected task into this project.
    ///
    /// Distinct from [`Pick::Project`], which *shows* one. Both carry a `ProjectId` and
    /// mean opposite things, which is precisely why they are two variants and not one
    /// with a flag beside it.
    MoveTo(ProjectId),
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
    /// A project to move the selected task into.
    MoveProject,
}

impl PickerKind {
    /// The modal's title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Project => "Go to project",
            // The only place the picker's `C-e` is ever advertised: it is a modal-local
            // key, so the help modal -- which is rendered from `KEYMAP` -- cannot know
            // about it, and a picker has no footer to put it in.
            Self::Label => "Go to label  —  C-e edits",
            Self::Command => "Run a command",
            Self::MoveProject => "Move to project",
        }
    }
}

/// Indices of `titles` matching `query`, best match first.
///
/// Shared by every modal that filters as it is typed, so "does this match what I typed"
/// has one answer rather than one per modal. Ties break on the original order, which is
/// what stops the list reshuffling under someone who is still typing.
fn fuzzy_order<'a>(titles: impl Iterator<Item = &'a str>, query: &str) -> Vec<usize> {
    let titles: Vec<&str> = titles.collect();
    if query.is_empty() {
        return (0..titles.len()).collect();
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, usize)> = titles
        .iter()
        .enumerate()
        .filter_map(|(index, title)| {
            matcher
                .fuzzy_match(title, query)
                .map(|score| (score, index))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
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
        self.matches = fuzzy_order(
            self.candidates
                .iter()
                .map(|candidate| candidate.title.as_str()),
            self.input.value(),
        );
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
    /// Choosing one task's priority.
    Priority(PriorityState),
    /// Typing one task's due date.
    Due(DueState),
    /// Ticking labels on and off one task.
    Labels(LabelsState),
    /// Renaming or recolouring one label.
    LabelEdit(LabelEditState),
    /// Waiting for a configured quick-action key.
    QuickActions(QuickActionsState),
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
    /// Set the selected task's priority to this, 0 to 5.
    Priority(i64),
    /// Set the selected task's due date from this text. Empty clears it.
    Due(String),
    /// Make these, exactly these, the selected task's labels.
    Labels(Vec<LabelId>),
    /// Create a label with this title. Always an [`Outcome::Update`]: the form that
    /// asked stays open, because the user is part-way through deciding one task's
    /// labels and closing it would throw away every tick they had already made.
    CreateLabel(String),
    /// Open the label form over this label. Always an [`Outcome::Update`], from both
    /// surfaces that list labels: the list the user pressed the key in stays open
    /// underneath, because they are part-way through ticking labels or picking a filter
    /// and a rename is not a reason to throw that away.
    EditLabel(LabelId),
    /// Rename and recolour this label, in one request and one undo step.
    ///
    /// Both fields whether or not both were touched: a partial body clears what it omits,
    /// measured on dev 2026-08-29.
    EditedLabel {
        /// The label as the form opened over it, which becomes the mutation's `before`.
        ///
        /// Carried rather than looked up again, because `Label::merge_onto` decides "did
        /// the user change this field" by comparing `before` with `after` -- so `before`
        /// has to be *what the user started from*, not whatever the pool holds by the
        /// time they press Enter. `Msg::LabelsLoaded` fires on every pull, rename and
        /// rejection, and the open form is deliberately not refreshed from it, so the two
        /// genuinely differ: with a re-read `before`, a colour another box changed while
        /// this form was open reads as *this* user's edit and gets overwritten with a
        /// value they never typed, and an Enter over a rename that landed underneath
        /// queues a write that reverts it.
        before: Box<Label>,
        /// Its new title.
        title: String,
        /// Its new colour: six hex digits, or empty for "the interface picks one".
        hex_color: String,
    },
    /// Run the configured quick action at this index.
    QuickAction(usize),
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
    /// The project name the form was opened with.
    ///
    /// Two projects can share a title -- dev carries two called "Inbox" -- so resolving
    /// this field by name can land on a different project than the one the task is in.
    /// Keeping what was shown lets `update` tell "untouched" from "retyped", and leave
    /// the id alone in the first case.
    pub project_was: String,
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
            project_was: project_name.to_string(),
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
            project_was: self.project_was.trim().to_string(),
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
    /// The project name the form was opened with, to tell an untouched field from a
    /// retyped one when two projects share a name.
    pub project_was: String,
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
            // The same hard limit the `p` key has. The form used to take any text here
            // and report "not a priority between 0 and 5" on save, which is a slower and
            // less honest way of saying the field cannot hold it.
            KeyCode::Char(c)
                if self.focus == EditField::Priority
                    && !key.mods.contains(KeyModifiers::CONTROL) =>
            {
                let mut candidate = self.priority.value().to_string();
                candidate.push(c);
                if is_priority_text(&candidate) {
                    self.priority.press(key);
                }
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

/// The highest priority Vikunja has.
pub const MAX_PRIORITY: i64 = 5;

/// Whether a priority field may hold `text` — empty, `0`–`5`, or `00`–`05`.
///
/// The field is checked *before* the keystroke lands rather than after, so an invalid
/// priority is never something the form can be holding. That is the difference between a
/// hard limit and an error message: `0005` cannot be typed at all, instead of being typed
/// and then rejected on save.
#[must_use]
pub fn is_priority_text(text: &str) -> bool {
    match text.as_bytes() {
        [] => true,
        [only] => only.is_ascii_digit() && i64::from(only - b'0') <= MAX_PRIORITY,
        // A leading zero is the only two-character form: `05` is five, `55` is nothing.
        [b'0', second] => second.is_ascii_digit() && i64::from(second - b'0') <= MAX_PRIORITY,
        _ => false,
    }
}

/// One task's priority, 0 to 5.
///
/// A field of its own rather than the fuzzy picker every other list uses, because a
/// priority is a number in a fixed range and fuzzy-matching a number is nonsense: `0005`
/// typed into the picker matched no candidate at all, so Enter did nothing and the key
/// read as broken. Here the keystroke is refused instead — only a digit, and only one
/// that leaves a valid priority behind, ever reaches the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityState {
    /// What has been typed. Always empty or a valid priority; never anything else.
    pub typed: String,
    /// The row under the cursor, which is what Enter applies.
    pub selected: i64,
    /// What the task holds now, marked on its row.
    pub current: i64,
}

impl PriorityState {
    /// A field over a task whose priority is `current`.
    #[must_use]
    pub fn new(current: i64) -> Self {
        Self {
            typed: String::new(),
            selected: current.clamp(0, MAX_PRIORITY),
            current,
        }
    }

    /// Take a digit, if it leaves the field valid. Returns whether it was taken.
    fn digit(&mut self, c: char) -> bool {
        let mut candidate = self.typed.clone();
        candidate.push(c);
        if !c.is_ascii_digit() || !is_priority_text(&candidate) {
            return false;
        }
        // Safe by construction: `is_priority_text` accepted it, so it is one or two
        // digits naming 0 to 5.
        self.selected = i64::from(c as u8 - b'0');
        self.typed = candidate;
        true
    }

    /// Move the highlight, rewriting the field to match so the two cannot disagree.
    fn step(&mut self, delta: i64) {
        self.selected = (self.selected + delta).rem_euclid(MAX_PRIORITY + 1);
        self.typed = self.selected.to_string();
    }
}

impl ModalView for PriorityState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Dismiss,
            KeyCode::Enter => Outcome::Submit(Submission::Priority(self.selected)),
            KeyCode::Down | KeyCode::Tab => {
                self.step(1);
                Outcome::Consumed
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.step(-1);
                Outcome::Consumed
            }
            KeyCode::Backspace => {
                self.typed.pop();
                if self.typed.is_empty() {
                    // Emptying the field puts the highlight back where it started, so
                    // Enter after a full rub-out changes nothing rather than setting 0.
                    self.selected = self.current.clamp(0, MAX_PRIORITY);
                }
                Outcome::Consumed
            }
            KeyCode::Char('u') if key.mods.contains(KeyModifiers::CONTROL) => {
                self.typed.clear();
                self.selected = self.current.clamp(0, MAX_PRIORITY);
                Outcome::Consumed
            }
            // Every other key, printable or not, is refused outright. That is what the
            // hard limit means: there is no keystroke that puts a `9` or a `p` in here.
            KeyCode::Char(c) => {
                self.digit(c);
                Outcome::Consumed
            }
            _ => Outcome::Consumed,
        }
    }

    fn title(&self) -> String {
        "Set priority  —  0 to 5".to_string()
    }
}

/// One task's due date, being typed.
///
/// A field rather than a picker because a date is not a list: the quick-add parser takes
/// `tomorrow`, `next friday` and `24/12/2026`, and no menu of presets covers what someone
/// will actually want. The status line shows what the parser made of it as it is typed,
/// so the guess is visible before Enter commits to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueState {
    /// What has been typed. Prefilled with the date the task already has.
    pub input: TextInput,
}

impl DueState {
    /// A field prefilled with `current`, which is empty when the task has no due date.
    #[must_use]
    pub fn new(current: &str) -> Self {
        Self {
            input: TextInput::new(current),
        }
    }
}

impl ModalView for DueState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Dismiss,
            // Deliberately submits an empty field rather than treating it as a cancel:
            // clearing a due date is a thing people want, and Esc is already the way to
            // back out without changing anything.
            KeyCode::Enter => Outcome::Submit(Submission::Due(self.input.value().to_string())),
            _ => {
                self.input.press(key);
                Outcome::Consumed
            }
        }
    }

    fn title(&self) -> String {
        "Due date".to_string()
    }
}

/// Labels being ticked on and off one task.
///
/// Multi-select, so Enter cannot mean "choose this one" the way it does in a picker: it
/// means "these are the labels now". `chosen` is the whole answer rather than a list of
/// changes, because `update` is what knows the task's labels and can work out the attach
/// and detach sets against them -- the same split the edit form already makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelsState {
    /// The filter typed so far.
    pub input: TextInput,
    /// Every label that exists, unfiltered.
    pub labels: Vec<Label>,
    /// The ids ticked right now, starting from the ones the task holds.
    pub chosen: Vec<LabelId>,
    /// Indices into `labels`, best match first.
    pub matches: Vec<usize>,
    /// Which match is highlighted, as a position in `matches`.
    pub selected: usize,
    /// Titles this form has asked to create and has not yet seen come back.
    ///
    /// The interface never learns the provisional id a create was given —
    /// `Store::queue` allocates it inside its own transaction — so between the
    /// keystroke and the reload the form has no way to recognise its own label. This
    /// is that gap, written down: it is what [`Self::absorb_created`] matches against,
    /// and what stops [`Self::creatable`] offering the same title twice.
    pub awaiting: Vec<String>,
}

impl LabelsState {
    /// A form over `labels`, with the task's own `held` already ticked.
    #[must_use]
    pub fn new(labels: Vec<Label>, held: Vec<LabelId>) -> Self {
        let matches = (0..labels.len()).collect();
        Self {
            input: TextInput::default(),
            labels,
            chosen: held,
            matches,
            selected: 0,
            awaiting: Vec::new(),
        }
    }

    /// The title this form would create, if the user asked.
    ///
    /// `None` when the box is empty, when a label of that name already exists, or when
    /// this form has already asked for it and is waiting for the reload. Offering a
    /// duplicate is offering to make the pool worse: Vikunja will hold two labels with
    /// one title without complaint, and the pool is global to every project, so the
    /// mistake is everyone's and nobody's to clean up.
    ///
    /// The offer does *not* depend on the filter matching nothing. The filter is fuzzy,
    /// so `next` matches an existing `next steps` — a user who wants a plain `next` is
    /// looking at a non-empty list and still wants the key to work.
    ///
    /// Both comparisons fold **ASCII only**, matching `resolve_labels` and
    /// `Client::labels_named`, so `Über` and `über` read as different titles and each can
    /// be created beside the other. Deliberately in step with the rest of the codebase
    /// rather than right in isolation: an interface that disagreed with the retry path
    /// about what a duplicate is would be worse than one that folds narrowly everywhere.
    #[must_use]
    pub fn creatable(&self) -> Option<&str> {
        let typed = self.input.value().trim();
        if typed.is_empty() {
            return None;
        }
        if self
            .labels
            .iter()
            .any(|label| label.title.eq_ignore_ascii_case(typed))
        {
            return None;
        }
        if self
            .awaiting
            .iter()
            .any(|title| title.eq_ignore_ascii_case(typed))
        {
            return None;
        }
        Some(typed)
    }

    /// Take a fresh label snapshot, adopting and ticking what this form asked to create.
    ///
    /// The form holds its own clone of the label list, and has to: it also shows labels
    /// the task carries that `model.data.labels` has not caught up with. So a reload
    /// does not reach it, and this is how it hears about the label it asked for.
    ///
    /// Deliberately narrow on both counts. Only a title in `awaiting` is taken, because
    /// a background sync reloads labels too and a form that absorbed everything would
    /// reshuffle itself under a user mid-keystroke. And only those are *ticked*, because
    /// ticking is an edit to this task: a label somebody created on another box has no
    /// business landing on it.
    ///
    /// Matched by title rather than by id because the id is precisely what the interface
    /// does not know — `Store::queue` allocates the provisional one inside its own
    /// transaction. Titles are not unique on the server, so what actually holds is
    /// weaker than "one label": this takes the *first* label carrying the title whose id
    /// the form has not got. If a pull lands a same-title label from another box between
    /// the keystroke and the naming reload, that box's id can win and the local
    /// provisional is then never shown here. Same accepted class as two boxes creating
    /// one name at once, which nothing without a server-side unique constraint can fix —
    /// and the wrong id is still a real label with the right title, which the next pull
    /// shows in the list either way.
    ///
    /// Case-insensitive, and **ASCII-only** folding, matching `resolve_labels` and
    /// `Client::labels_named`: `Über` does not match `über`. The whole codebase folds
    /// this way, and moving one site would make the interface disagree with itself about
    /// what counts as a duplicate. All three would have to move together.
    pub fn absorb_created(&mut self, known: &[Label]) {
        if self.awaiting.is_empty() {
            return;
        }
        for label in known {
            if self.labels.iter().any(|held| held.id == label.id) {
                continue;
            }
            let Some(at) = self
                .awaiting
                .iter()
                .position(|title| title.eq_ignore_ascii_case(&label.title))
            else {
                continue;
            };
            self.awaiting.remove(at);
            self.labels.push(label.clone());
            if !self.chosen.contains(&label.id) {
                self.chosen.push(label.id);
            }
        }
        self.refilter();
    }

    /// Take the pool's copy of every label this form already holds.
    ///
    /// The counterpart to [`Self::absorb_created`], and deliberately the opposite shape:
    /// that one *adds* what this form asked for and ticks it, this one only rewrites
    /// labels already on the list and ticks nothing. A label can be renamed from this
    /// form now, and the optimistic new title is written straight into these clones --
    /// so when the store rolls that write back, or another box renames the same label,
    /// the reload has to be able to write it back out again. Without this the pool says
    /// `urgent` while the row the user is looking at says `critical`: the rename they
    /// were just told had failed.
    ///
    /// The order is left alone on purpose. `matches` is fuzzy-ranked by title, so
    /// re-sorting here would reshuffle the list under someone mid-keystroke; the next
    /// key they press refilters against the new titles.
    pub fn refresh(&mut self, known: &[Label]) {
        for held in &mut self.labels {
            if let Some(fresh) = known.iter().find(|label| label.id == held.id) {
                held.clone_from(fresh);
            }
        }
    }

    /// The label under the cursor.
    #[must_use]
    pub fn current(&self) -> Option<&Label> {
        self.matches
            .get(self.selected)
            .and_then(|index| self.labels.get(*index))
    }

    /// Whether `label` is ticked.
    #[must_use]
    pub fn is_chosen(&self, label: LabelId) -> bool {
        self.chosen.contains(&label)
    }

    /// Tick the highlighted label, or untick it.
    fn toggle(&mut self) {
        let Some(id) = self.current().map(|label| label.id) else {
            return;
        };
        match self.chosen.iter().position(|held| *held == id) {
            Some(at) => {
                self.chosen.remove(at);
            }
            None => self.chosen.push(id),
        }
    }

    fn refilter(&mut self) {
        self.matches = fuzzy_order(
            self.labels.iter().map(|label| label.title.as_str()),
            self.input.value(),
        );
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }
}

impl ModalView for LabelsState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Dismiss,
            KeyCode::Enter => Outcome::Submit(Submission::Labels(self.chosen.clone())),
            // Space toggles rather than typing a space. A label whose title has one in it
            // is still reachable: the filter is fuzzy, so `inprog` finds `in progress`.
            KeyCode::Char(' ') => {
                self.toggle();
                Outcome::Consumed
            }
            // Ctrl-N creates, and Enter above still means "apply the ticks I made". A
            // fast typist filtering to a name that does not exist must not be able to
            // add to a global label pool by reflex, so the two never share a key.
            //
            // Handled here rather than in `KEYMAP`, like Space and Enter above: the
            // modal stack takes every key before the table is consulted, so a row there
            // would be a row the user could not reach. What advertises it is the offer
            // the form draws when — and only when — `creatable` is `Some`.
            KeyCode::Char('n') if key.mods.contains(KeyModifiers::CONTROL) => {
                match self.creatable() {
                    Some(title) => {
                        let title = title.to_string();
                        self.awaiting.push(title.clone());
                        Outcome::Update(Submission::CreateLabel(title))
                    }
                    // Nothing typed, or a label of that name already exists. Consumed
                    // rather than passed to the field, which would ignore it anyway.
                    None => Outcome::Consumed,
                }
            }
            // Ctrl-E edits the highlighted label, the same key the `g l` picker uses --
            // a label edited from here and one edited from there must be the same
            // operation, not two that can drift apart.
            //
            // Handled here rather than in `KEYMAP` for the same reason Ctrl-N is: the
            // modal stack takes every key before the table is consulted, so a row there
            // would be a row nobody could reach. The footer is what advertises it.
            KeyCode::Char('e') if key.mods.contains(KeyModifiers::CONTROL) => {
                match self.current() {
                    Some(label) => Outcome::Update(Submission::EditLabel(label.id)),
                    // Nothing matched the filter, so there is nothing to rename.
                    None => Outcome::Consumed,
                }
            }
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
        "Labels  —  Space toggles, Enter applies, Esc cancels".to_string()
    }
}

/// Which field of the label form has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelField {
    /// The label's text.
    Title,
    /// Its six hex digits, without a leading `#`.
    Colour,
}

impl LabelField {
    /// What this field is called on screen.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Colour => "Colour",
        }
    }

    /// The other one. Two fields, so `Tab` and `S-Tab` are the same move.
    #[must_use]
    const fn other(self) -> Self {
        match self {
            Self::Title => Self::Colour,
            Self::Colour => Self::Title,
        }
    }
}

/// Editing one label's title and colour.
///
/// Both at once because they are one `POST /labels/{id}`, one undo step and one row in
/// the queue -- and because a partial body clears what it omits (measured on dev
/// 2026-08-29: a body carrying only `title` cleared `hex_color` to `""`), so the write
/// carries both whether or not the user touched both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelEditState {
    /// The label as it was when the form opened, which becomes the mutation's `before`.
    ///
    /// Kept whole rather than by id for the same reason [`EditState`] keeps its task: it
    /// is the `before` half of a three-way merge, and a `before` assembled later from
    /// whatever the pool holds by then is not what the user started from.
    pub label: Label,
    /// The title being typed.
    pub title: TextInput,
    /// The colour being typed.
    pub hex: TextInput,
    /// Which field has the keyboard.
    pub field: LabelField,
    /// Why the last submission was refused, if it was.
    pub error: Option<String>,
}

impl LabelEditState {
    /// A form over `label`, with both fields filled in from it.
    #[must_use]
    pub fn new(label: Label) -> Self {
        Self {
            title: TextInput::new(label.title.clone()),
            hex: TextInput::new(label.hex_color.clone()),
            label,
            field: LabelField::Title,
            error: None,
        }
    }

    /// The colour as it would go on the wire: trimmed, and without the leading `#`.
    ///
    /// Vikunja stores six bare hex digits, but `#4287f5` is what a colour picker puts on
    /// the clipboard and [`crate::theme::parse_hex`] already tolerates it -- so the form
    /// takes the hash and drops it rather than refusing a paste for a character the rest
    /// of the codebase ignores.
    #[must_use]
    pub fn colour(&self) -> &str {
        self.hex.value().trim().trim_start_matches('#')
    }

    /// The title as it would go on the wire.
    #[must_use]
    pub fn typed_title(&self) -> &str {
        self.title.value().trim()
    }

    /// Whether the colour is something Vikunja will take: six hex digits, or empty for
    /// "the interface picks one".
    #[must_use]
    pub fn colour_is_valid(&self) -> bool {
        let hex = self.colour();
        hex.is_empty() || (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
    }

    /// Why this form cannot be sent as it stands, if it cannot.
    ///
    /// Refused *here* rather than queued and rejected minutes later, because a server
    /// rejection rolls the whole mutation back -- taking the rename with it, along with
    /// whatever else the user had done in between.
    #[must_use]
    fn refusal(&self) -> Option<String> {
        if self.typed_title().is_empty() {
            // Not measured against the server, and deliberately not left to it: a label
            // with no title is one nobody can find again in any of the three lists that
            // name it, and the only way back would be the id the interface never shows.
            return Some("A label needs a title".to_string());
        }
        if !self.colour_is_valid() {
            return Some("A colour is six hex digits, like 4287f5 — or empty".to_string());
        }
        None
    }

    /// The field with the keyboard.
    fn current(&mut self) -> &mut TextInput {
        match self.field {
            LabelField::Title => &mut self.title,
            LabelField::Colour => &mut self.hex,
        }
    }

    /// One field's text, for drawing.
    #[must_use]
    pub const fn field(&self, which: LabelField) -> &TextInput {
        match which {
            LabelField::Title => &self.title,
            LabelField::Colour => &self.hex,
        }
    }
}

impl ModalView for LabelEditState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Dismiss,
            // Enter saves, unlike the task form, because neither field is multi-line and
            // there is nothing else for it to mean.
            KeyCode::Enter => match self.refusal() {
                Some(why) => {
                    self.error = Some(why);
                    Outcome::Consumed
                }
                None => Outcome::Submit(Submission::EditedLabel {
                    before: Box::new(self.label.clone()),
                    title: self.typed_title().to_string(),
                    hex_color: self.colour().to_string(),
                }),
            },
            KeyCode::Tab | KeyCode::BackTab => {
                self.field = self.field.other();
                Outcome::Consumed
            }
            _ => {
                let typed = self.current().press(key);
                // Re-derived rather than cleared, because the two are not the same thing:
                // the message is about a *field*, and typing in the other one does not
                // fix it. Clearing on any keystroke made a bad colour's message vanish as
                // soon as the user tabbed back to the title and typed, leaving the hint
                // row saying only the generic rule until the next Enter reported the same
                // refusal again. Asked only while one stands, so a half-typed colour is
                // never nagged about before Enter.
                if typed && self.error.is_some() {
                    self.error = self.refusal();
                }
                Outcome::Consumed
            }
        }
    }

    fn title(&self) -> String {
        "Edit label  —  Tab moves, Enter saves, Esc cancels".to_string()
    }
}

/// The configured quick actions, waiting for one of their keys.
///
/// Holds `(key, description)` pairs rather than the config type, so the modal cannot
/// resolve a project name or a priority itself -- it reports which row was pressed and
/// `update` decides what that row means, the same way every other modal here works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickActionsState {
    /// One row per configured action: the key that triggers it, and what it does.
    pub rows: Vec<(char, String)>,
}

impl QuickActionsState {
    /// A menu over `rows`.
    #[must_use]
    pub fn new(rows: Vec<(char, String)>) -> Self {
        Self { rows }
    }
}

impl ModalView for QuickActionsState {
    fn handle(&mut self, key: Key) -> Outcome {
        match key.code {
            // Space cancels as well as Esc, so the key that opened this closes it -- which
            // is what a second press of a mode key should do.
            KeyCode::Esc | KeyCode::Char(' ') => Outcome::Dismiss,
            KeyCode::Char(c) => match self.rows.iter().position(|(bound, _)| *bound == c) {
                Some(index) => Outcome::Submit(Submission::QuickAction(index)),
                // Stays open on an unconfigured key. The menu listing every key that *is*
                // configured is on screen, so there is nothing left to explain.
                None => Outcome::Consumed,
            },
            _ => Outcome::Consumed,
        }
    }

    fn title(&self) -> String {
        "Quick actions".to_string()
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
            // The second surface that lists labels, on the same key as the first. Only
            // over a label: there is nothing behind a project or a command for this form
            // to edit, and `Pick` is what says which is which.
            //
            // Modal-local like the label form's, and advertised the same way -- in the
            // title, because a picker has no footer and this one is worth naming whether
            // or not anything is highlighted.
            KeyCode::Char('e') if key.mods.contains(KeyModifiers::CONTROL) => {
                match self.current().map(|candidate| candidate.pick) {
                    Some(Pick::Label(id)) => Outcome::Update(Submission::EditLabel(id)),
                    _ => Outcome::Consumed,
                }
            }
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
            Self::Priority(state) => state,
            Self::Due(state) => state,
            Self::Labels(state) => state,
            Self::LabelEdit(state) => state,
            Self::QuickActions(state) => state,
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
            Self::Priority(state) => state.title(),
            Self::Due(state) => state.title(),
            Self::Labels(state) => state.title(),
            Self::LabelEdit(state) => state.title(),
            Self::QuickActions(state) => state.title(),
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

    fn a_label(id: i64, title: &str) -> Label {
        Label {
            id: LabelId(id),
            title: title.to_string(),
            ..Label::default()
        }
    }

    #[test]
    fn space_toggles_a_label_and_a_title_with_a_space_in_it_is_still_reachable() {
        let mut form = LabelsState::new(
            vec![a_label(1, "urgent"), a_label(2, "in progress")],
            vec![],
        );
        // Space is the toggle, so the filter can never hold one -- which would be a
        // problem if the match were a substring. It is fuzzy, so it is not.
        for c in "inprog".chars() {
            form.handle(key(c));
        }
        assert_eq!(form.current().unwrap().title, "in progress");

        form.handle(key(' '));
        assert!(form.is_chosen(LabelId(2)));
        assert_eq!(
            form.input.value(),
            "inprog",
            "the space did not type itself"
        );

        form.handle(key(' '));
        assert!(!form.is_chosen(LabelId(2)), "and it toggles back off");

        assert_eq!(
            form.handle(code(KeyCode::Enter)),
            Outcome::Submit(Submission::Labels(Vec::new()))
        );
    }

    #[test]
    fn toggling_with_nothing_matched_changes_nothing_rather_than_panicking() {
        let mut form = LabelsState::new(vec![a_label(1, "urgent")], vec![]);
        for c in "zzz".chars() {
            form.handle(key(c));
        }
        assert!(form.current().is_none());
        form.handle(key(' '));
        assert!(form.chosen.is_empty());
    }

    #[test]
    fn a_label_form_with_no_match_offers_to_create_what_was_typed() {
        let mut form = LabelsState::new(vec![a_label(1, "urgent")], vec![]);
        for c in "next".chars() {
            form.handle(key(c));
        }
        assert!(form.matches.is_empty());
        assert_eq!(form.creatable(), Some("next"));

        // Ctrl-N creates; Enter still ticks, so a fast typist cannot create by reflex.
        assert_eq!(
            form.handle(Key::ctrl('n')),
            Outcome::Update(Submission::CreateLabel("next".to_string())),
            "the form stays open: the user is still deciding this task's labels"
        );
        assert_eq!(
            form.handle(code(KeyCode::Enter)),
            Outcome::Submit(Submission::Labels(Vec::new())),
            "and Enter still means 'apply the ticks I made'"
        );
    }

    #[test]
    fn a_form_does_not_offer_to_create_a_label_that_exists() {
        let mut form = LabelsState::new(vec![a_label(1, "urgent")], vec![]);
        for c in "urgent".chars() {
            form.handle(key(c));
        }
        assert_eq!(form.creatable(), None);
        assert_eq!(
            form.handle(Key::ctrl('n')),
            Outcome::Consumed,
            "the key does nothing rather than making the global pool worse"
        );
    }

    #[test]
    fn a_form_does_not_offer_twice_while_the_first_create_is_still_in_flight() {
        // The provisional id is allocated by the store, so the form cannot see the label
        // it just asked for until the reload comes back. Without `awaiting`, a second
        // Ctrl-N in that window queues a second `CreateLabel` for the same title -- two
        // labels called `next` in a pool that is global to every project.
        let mut form = LabelsState::new(vec![a_label(1, "urgent")], vec![]);
        for c in "next".chars() {
            form.handle(key(c));
        }
        assert!(matches!(form.handle(Key::ctrl('n')), Outcome::Update(_)));
        assert_eq!(form.creatable(), None);
        assert_eq!(form.handle(Key::ctrl('n')), Outcome::Consumed);
    }

    #[test]
    fn an_empty_box_offers_nothing_and_a_created_label_comes_back_ticked() {
        let mut form = LabelsState::new(Vec::new(), Vec::new());
        assert_eq!(form.creatable(), None, "there is nothing to create yet");
        assert_eq!(form.handle(Key::ctrl('n')), Outcome::Consumed);

        for c in "next".chars() {
            form.handle(key(c));
        }
        assert!(matches!(form.handle(Key::ctrl('n')), Outcome::Update(_)));

        // The reload names it. Only what this form asked for is taken, and only that is
        // ticked -- `other` was created on another box and is nobody's business here.
        form.absorb_created(&[a_label(-1, "next"), a_label(7, "other")]);
        assert_eq!(form.labels, vec![a_label(-1, "next")]);
        assert_eq!(form.chosen, vec![LabelId(-1)]);
        assert_eq!(form.current().map(|l| l.title.as_str()), Some("next"));

        // And it is not offered a second time, nor taken a second time.
        assert_eq!(form.creatable(), None);
        form.absorb_created(&[a_label(-1, "next")]);
        assert_eq!(form.labels.len(), 1);
    }

    fn a_coloured_label(id: i64, title: &str, hex: &str) -> Label {
        Label {
            id: LabelId(id),
            title: title.to_string(),
            hex_color: hex.to_string(),
            ..Label::default()
        }
    }

    #[test]
    fn the_label_form_starts_from_what_the_label_is_now() {
        let state = LabelEditState::new(a_coloured_label(41, "next", "4287f5"));
        assert_eq!(state.title.value(), "next");
        assert_eq!(state.hex.value(), "4287f5");
        assert_eq!(state.field, LabelField::Title);
        assert!(state.error.is_none());
    }

    #[test]
    fn submitting_the_label_form_carries_both_fields() {
        // One request, one undo step, one `UpdateLabel` -- so a rename that never touched
        // the colour still sends it, because a partial body clears what it omits.
        let mut form =
            Modal::LabelEdit(LabelEditState::new(a_coloured_label(41, "next", "4287f5")));
        for _ in 0.."next".len() {
            form.handle(code(KeyCode::Backspace));
        }
        for c in "next up".chars() {
            form.handle(key(c));
        }
        assert_eq!(
            form.handle(code(KeyCode::Enter)),
            Outcome::Submit(Submission::EditedLabel {
                before: Box::new(a_coloured_label(41, "next", "4287f5")),
                title: "next up".to_string(),
                hex_color: "4287f5".to_string(),
            })
        );
    }

    #[test]
    fn tab_moves_between_the_two_fields_and_esc_abandons_both() {
        let mut form = LabelEditState::new(a_coloured_label(41, "next", "4287f5"));
        assert_eq!(form.handle(code(KeyCode::Tab)), Outcome::Consumed);
        assert_eq!(form.field, LabelField::Colour);
        for _ in 0.."4287f5".len() {
            form.handle(code(KeyCode::Backspace));
        }
        for c in "e8384f".chars() {
            form.handle(key(c));
        }
        assert_eq!(form.title.value(), "next", "the title field is untouched");
        assert_eq!(form.handle(code(KeyCode::BackTab)), Outcome::Consumed);
        assert_eq!(form.field, LabelField::Title);
        assert_eq!(
            form.handle(code(KeyCode::Enter)),
            Outcome::Submit(Submission::EditedLabel {
                before: Box::new(a_coloured_label(41, "next", "4287f5")),
                title: "next".to_string(),
                hex_color: "e8384f".to_string(),
            })
        );

        let mut abandoned = LabelEditState::new(a_coloured_label(41, "next", "4287f5"));
        abandoned.handle(key('x'));
        assert_eq!(abandoned.handle(code(KeyCode::Esc)), Outcome::Dismiss);
    }

    #[test]
    fn a_colour_that_is_not_six_hex_digits_is_refused_in_the_form() {
        // Rather than queued and rejected by the server minutes later, which rolls the
        // whole mutation back -- taking the rename with it.
        let mut form = LabelEditState::new(a_coloured_label(41, "next", ""));
        form.handle(code(KeyCode::Tab));
        for c in "nope".chars() {
            form.handle(key(c));
        }
        assert_eq!(form.handle(code(KeyCode::Enter)), Outcome::Consumed);
        assert!(
            form.error.as_deref().is_some_and(|why| why.contains("hex")),
            "the form says why: {:?}",
            form.error
        );

        // Typing in the *other* field does not fix a bad colour, so the message stands.
        form.handle(code(KeyCode::Tab));
        for c in "!".chars() {
            form.handle(key(c));
        }
        assert!(
            form.error.is_some(),
            "the title has nothing to do with the colour"
        );

        // And it does not outlive the mistake it is actually about.
        form.handle(code(KeyCode::Tab));
        form.handle(Key::ctrl('u'));
        for c in "e8384f".chars() {
            form.handle(key(c));
        }
        assert!(form.error.is_none());
    }

    #[test]
    fn an_empty_colour_is_a_colour_and_a_leading_hash_is_taken_off() {
        // Empty is what Vikunja holds for a label nobody coloured, and what clearing one
        // has to send. `#4287f5` is what a colour picker puts on the clipboard;
        // `theme::parse_hex` already tolerates the hash, and the wire format has none.
        let mut cleared = LabelEditState::new(a_coloured_label(41, "next", "4287f5"));
        cleared.handle(code(KeyCode::Tab));
        cleared.handle(Key::ctrl('u'));
        assert_eq!(cleared.hex.value(), "");
        assert_eq!(
            cleared.handle(code(KeyCode::Enter)),
            Outcome::Submit(Submission::EditedLabel {
                before: Box::new(a_coloured_label(41, "next", "4287f5")),
                title: "next".to_string(),
                hex_color: String::new(),
            })
        );

        let mut hashed = LabelEditState::new(a_coloured_label(41, "next", "#4287f5"));
        assert_eq!(
            hashed.handle(code(KeyCode::Enter)),
            Outcome::Submit(Submission::EditedLabel {
                before: Box::new(a_coloured_label(41, "next", "#4287f5")),
                title: "next".to_string(),
                hex_color: "4287f5".to_string(),
            })
        );
    }

    #[test]
    fn a_label_cannot_be_renamed_to_nothing() {
        // Not measured against the server, and that is the point: a label with no title
        // is one nobody can find again in any of the three lists that name it, and the
        // only way back would be the id the interface never shows.
        let mut form = LabelEditState::new(a_coloured_label(41, "next", "4287f5"));
        for _ in 0.."next".len() {
            form.handle(code(KeyCode::Backspace));
        }
        assert_eq!(form.handle(code(KeyCode::Enter)), Outcome::Consumed);
        assert!(form.error.is_some());
    }

    #[test]
    fn both_lists_of_labels_open_the_same_form_on_the_highlighted_one() {
        // A label edited from the task form and one edited from `g l` must be the same
        // operation, so both surfaces ask for it the same way -- and both stay open,
        // because `Submit` would pop the form the user is still working in.
        let mut ticks = LabelsState::new(vec![a_label(1, "urgent"), a_label(2, "next")], vec![]);
        ticks.handle(code(KeyCode::Down));
        assert_eq!(
            ticks.handle(Key::ctrl('e')),
            Outcome::Update(Submission::EditLabel(LabelId(2)))
        );

        let mut picker = PickerState::new(
            PickerKind::Label,
            vec![
                Candidate::new(Pick::Label(LabelId(1)), "urgent"),
                Candidate::new(Pick::Label(LabelId(2)), "next"),
            ],
        );
        picker.handle(code(KeyCode::Down));
        assert_eq!(
            picker.handle(Key::ctrl('e')),
            Outcome::Update(Submission::EditLabel(LabelId(2)))
        );

        // Only the label picker. There is nothing to edit behind a project or a command.
        let mut projects = PickerState::new(
            PickerKind::Project,
            vec![Candidate::new(Pick::Project(ProjectId(1)), "Work")],
        );
        assert_eq!(projects.handle(Key::ctrl('e')), Outcome::Consumed);
    }

    #[test]
    fn editing_nothing_does_nothing_rather_than_panicking() {
        let mut ticks = LabelsState::new(vec![a_label(1, "urgent")], vec![]);
        for c in "zzz".chars() {
            ticks.handle(key(c));
        }
        assert!(ticks.current().is_none());
        assert_eq!(ticks.handle(Key::ctrl('e')), Outcome::Consumed);

        let mut picker = PickerState::new(
            PickerKind::Label,
            vec![Candidate::new(Pick::Label(LabelId(1)), "urgent")],
        );
        for c in "zzz".chars() {
            picker.handle(key(c));
        }
        assert_eq!(picker.handle(Key::ctrl('e')), Outcome::Consumed);
    }

    #[test]
    fn the_due_field_submits_an_empty_value_but_esc_still_abandons() {
        let mut modal = Modal::Due(DueState::new("24/12/2026"));
        for _ in 0..10 {
            modal.handle(code(KeyCode::Backspace));
        }
        // Clearing the date is a request, not a cancellation.
        assert_eq!(
            modal.handle(code(KeyCode::Enter)),
            Outcome::Submit(Submission::Due(String::new()))
        );

        let mut abandoned = Modal::Due(DueState::new("24/12/2026"));
        abandoned.handle(key('x'));
        assert_eq!(abandoned.handle(code(KeyCode::Esc)), Outcome::Dismiss);
    }

    #[test]
    fn help_scrolls_but_any_other_key_closes_it() {
        let mut modal = Modal::Help(HelpState::default());
        assert_eq!(modal.handle(key('j')), Outcome::Consumed);
        assert_eq!(modal.handle(key('x')), Outcome::Dismiss);
    }
}
