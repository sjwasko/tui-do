//! The keymap: one table, two consumers.
//!
//! [`KEYMAP`] is the only place a key is bound to behaviour. `update` dispatches through
//! it and the help modal renders it, so the two cannot drift apart — which is the whole
//! reason it is a table of data rather than a `match` arm per key.
//!
//! [`Action`] is the stable name in the middle. Letting a user rebind keys later is then
//! a config change that rewrites the left-hand column, not a rewrite of `update`.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One key press, normalised.
///
/// Terminals report `Shift`+`h` as `Char('H')` *with* the shift modifier set, so a table
/// written as `Key::char('H')` would never match. [`Key::from_event`] drops the redundant
/// modifier and keeps the case, making the character the single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// Which key.
    pub code: KeyCode,
    /// Modifiers still meaningful after normalisation.
    pub mods: KeyModifiers,
}

impl Key {
    /// A key with no modifiers.
    #[must_use]
    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::NONE,
        }
    }

    /// An unmodified character.
    #[must_use]
    pub const fn char(c: char) -> Self {
        Self::plain(KeyCode::Char(c))
    }

    /// A control chord such as `Ctrl-d`.
    #[must_use]
    pub const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: KeyModifiers::CONTROL,
        }
    }

    /// Normalise a terminal event into a table-comparable key.
    ///
    /// Returns `None` for key *releases* and repeats we do not act on: a release event
    /// carries the same code as its press, so acting on both would double every motion
    /// on terminals that report the kinds separately.
    #[must_use]
    pub fn from_event(event: KeyEvent) -> Option<Self> {
        if event.kind == KeyEventKind::Release {
            return None;
        }
        let mut mods = event.modifiers;
        if let KeyCode::Char(c) = event.code {
            if c.is_uppercase() {
                mods.remove(KeyModifiers::SHIFT);
            }
        }
        // Neither is ever meaningful to a binding, and leaving them set would make an
        // otherwise-matching key miss.
        mods.remove(KeyModifiers::NONE);
        Some(Self {
            code: event.code,
            mods,
        })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "C-")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "M-")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::BackTab => write!(f, "S-Tab"),
            KeyCode::Left => write!(f, "←"),
            KeyCode::Right => write!(f, "→"),
            KeyCode::Up => write!(f, "↑"),
            KeyCode::Down => write!(f, "↓"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            KeyCode::PageUp => write!(f, "PgUp"),
            KeyCode::PageDown => write!(f, "PgDn"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// A sequence of keys that triggers a binding. One element for a plain key, two for a
/// chord such as `g g`.
pub type Chord = &'static [Key];

/// Where a binding applies.
///
/// `Enter` means "open the selected project" in the sidebar and "open the selected task"
/// in the list. Contexts are what let one key mean both without a special case in
/// `update`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Context {
    /// Applies wherever the task screen is showing.
    #[default]
    Global,
    /// Only while the sidebar has focus.
    Sidebar,
    /// Only while the task list has focus.
    List,
}

/// How the help modal groups a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// Moving around.
    Navigation,
    /// Changing a task.
    Task,
    /// Changing what is shown.
    View,
    /// Everything about the application itself.
    Application,
}

impl Group {
    /// The heading the help modal prints.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Task => "Tasks",
            Self::View => "View",
            Self::Application => "Application",
        }
    }

    /// Every group, in the order help lists them.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Navigation, Self::Task, Self::View, Self::Application]
    }
}

/// What a key does.
///
/// Deliberately describes intent rather than mechanism: `MoveDown` is one action whether
/// the sidebar, the list or the preview has focus, and `update` routes it by focus. Three
/// actions would mean three bindings and three help lines for one concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Move the selection down one row.
    MoveDown,
    /// Move the selection up one row.
    MoveUp,
    /// Move down by a screenful.
    PageDown,
    /// Move up by a screenful.
    PageUp,
    /// Jump to the first row.
    Top,
    /// Jump to the last row.
    Bottom,
    /// Move focus to the next visible pane.
    FocusNext,
    /// Move focus to the previous visible pane.
    FocusPrevious,
    /// Back out: to the task list, then out of a search.
    Back,
    /// Expand the selected project, or open it if already expanded.
    ExpandOrOpen,
    /// Collapse the selected project, or move to its parent.
    CollapseOrParent,
    /// Open whatever is selected in the sidebar.
    OpenProject,
    /// Open the selected task.
    OpenTask,
    /// Show or hide the sidebar.
    ToggleSidebar,
    /// Show or hide the preview pane.
    TogglePreview,
    /// Show completed tasks alongside open ones.
    ToggleDoneTasks,
    /// Switch to the next column layout.
    NextLayout,
    /// Switch to the previous column layout.
    PreviousLayout,
    /// Search the current list.
    Search,
    /// Jump to a project by name.
    GotoProject,
    /// Jump to a label by name.
    GotoLabel,
    /// Add a task, typed in quick-add syntax.
    AddTask,
    /// Open the edit form over the selected task.
    EditTask,
    /// Mark the selected task done, or not done.
    ToggleDone,
    /// Set the selected task's priority.
    SetPriority,
    /// Set the selected task's due date.
    SetDue,
    /// Move the selected task into another project.
    MoveTask,
    /// Add labels to the selected task, or take them off.
    SetLabels,
    /// Wait for a configured quick-action key.
    QuickAction,
    /// Delete the selected task.
    DeleteTask,
    /// Take back the last change.
    Undo,
    /// Put back what was undone.
    Redo,
    /// Sync now rather than waiting for the timer, fetching only what changed.
    SyncNow,
    /// Sync now, fetching everything, so that deletions made elsewhere are noticed.
    SyncFull,
    /// Show the help modal.
    Help,
    /// Run a command by name.
    CommandPalette,
    /// Leave tui-do.
    Quit,
}

impl Action {
    /// Whether the command palette offers this action.
    ///
    /// Motions are not commands. Typing four letters to move down one row is absurd, and
    /// six motion entries would crowd out the things actually worth searching for. The
    /// palette itself is excluded for the obvious reason.
    #[must_use]
    pub const fn is_command(self) -> bool {
        !matches!(
            self,
            Self::MoveDown
                | Self::MoveUp
                | Self::PageDown
                | Self::PageUp
                | Self::Top
                | Self::Bottom
                | Self::CommandPalette
        )
    }
}

/// One row of the keymap.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    /// The chords that trigger it. More than one means alternatives, not a sequence.
    pub keys: &'static [Chord],
    /// What it does.
    pub action: Action,
    /// Where it applies.
    pub context: Context,
    /// Which help section it appears under.
    pub group: Group,
    /// The one-line description help prints.
    pub doc: &'static str,
}

impl Binding {
    /// The chords, rendered the way help shows them: `g g`, or `j / ↓` for alternatives.
    #[must_use]
    pub fn keys_display(&self) -> String {
        self.keys
            .iter()
            .map(|chord| {
                chord
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

/// Shorthand for a one-key chord.
macro_rules! chord {
    ($($key:expr),+ $(,)?) => { &[$($key),+] as Chord };
}

/// Every binding in tui-do.
///
/// Two conflicts with vim are resolved deliberately and should stay resolved:
///
/// - `H` and `L` cycle column layouts, which is cria's meaning, not vim's screen-top and
///   screen-bottom. The vim meaning is reachable as `g g` and `G`.
/// - `g` is a prefix, not an action. `G` alone is bottom.
pub const KEYMAP: &[Binding] = &[
    Binding {
        keys: &[chord![Key::char('j')], chord![Key::plain(KeyCode::Down)]],
        action: Action::MoveDown,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Move down",
    },
    Binding {
        keys: &[chord![Key::char('k')], chord![Key::plain(KeyCode::Up)]],
        action: Action::MoveUp,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Move up",
    },
    Binding {
        keys: &[
            chord![Key::ctrl('d')],
            chord![Key::plain(KeyCode::PageDown)],
        ],
        action: Action::PageDown,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Page down",
    },
    Binding {
        keys: &[chord![Key::ctrl('u')], chord![Key::plain(KeyCode::PageUp)]],
        action: Action::PageUp,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Page up",
    },
    Binding {
        keys: &[
            chord![Key::char('g'), Key::char('g')],
            chord![Key::plain(KeyCode::Home)],
        ],
        action: Action::Top,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Jump to the top",
    },
    Binding {
        keys: &[chord![Key::char('G')], chord![Key::plain(KeyCode::End)]],
        action: Action::Bottom,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Jump to the bottom",
    },
    Binding {
        keys: &[chord![Key::plain(KeyCode::Tab)]],
        action: Action::FocusNext,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Focus the next pane",
    },
    Binding {
        keys: &[chord![Key::plain(KeyCode::BackTab)]],
        action: Action::FocusPrevious,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Focus the previous pane",
    },
    Binding {
        keys: &[chord![Key::plain(KeyCode::Esc)]],
        action: Action::Back,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Back to the list, then out of a search",
    },
    Binding {
        keys: &[chord![Key::char('g'), Key::char('p')]],
        action: Action::GotoProject,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Go to a project",
    },
    Binding {
        keys: &[chord![Key::char('g'), Key::char('l')]],
        action: Action::GotoLabel,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Go to a label",
    },
    Binding {
        keys: &[chord![Key::char('/')]],
        action: Action::Search,
        context: Context::Global,
        group: Group::Navigation,
        doc: "Search the current list",
    },
    Binding {
        keys: &[
            chord![Key::char('l')],
            chord![Key::plain(KeyCode::Right)],
            chord![Key::plain(KeyCode::Enter)],
        ],
        action: Action::ExpandOrOpen,
        context: Context::Sidebar,
        group: Group::Navigation,
        doc: "Expand, or open when already expanded",
    },
    Binding {
        keys: &[chord![Key::char('h')], chord![Key::plain(KeyCode::Left)]],
        action: Action::CollapseOrParent,
        context: Context::Sidebar,
        group: Group::Navigation,
        doc: "Collapse, or move to the parent",
    },
    Binding {
        keys: &[chord![Key::plain(KeyCode::Enter)]],
        action: Action::OpenTask,
        context: Context::List,
        group: Group::Navigation,
        doc: "Open the selected task",
    },
    Binding {
        keys: &[chord![Key::char('z'), Key::char('s')]],
        action: Action::ToggleSidebar,
        context: Context::Global,
        group: Group::View,
        doc: "Show or hide the sidebar",
    },
    Binding {
        keys: &[chord![Key::char('z'), Key::char('p')]],
        action: Action::TogglePreview,
        context: Context::Global,
        group: Group::View,
        doc: "Show or hide the preview pane",
    },
    Binding {
        keys: &[chord![Key::char('t')]],
        action: Action::ToggleDoneTasks,
        context: Context::Global,
        group: Group::View,
        doc: "Show or hide completed tasks",
    },
    Binding {
        keys: &[chord![Key::char('L')]],
        action: Action::NextLayout,
        context: Context::Global,
        group: Group::View,
        doc: "Next column layout",
    },
    Binding {
        keys: &[chord![Key::char('H')]],
        action: Action::PreviousLayout,
        context: Context::Global,
        group: Group::View,
        doc: "Previous column layout",
    },
    Binding {
        keys: &[chord![Key::char('a')]],
        action: Action::AddTask,
        context: Context::Global,
        group: Group::Task,
        doc: "Add a task",
    },
    Binding {
        keys: &[chord![Key::char('e')]],
        action: Action::EditTask,
        context: Context::Global,
        group: Group::Task,
        doc: "Edit the selected task",
    },
    Binding {
        keys: &[chord![Key::char('d')]],
        action: Action::ToggleDone,
        // Global, not list-only: the selected task exists whichever pane has focus, and
        // a key that quietly does nothing because the sidebar is focused is a key the
        // user reports as broken.
        context: Context::Global,
        group: Group::Task,
        doc: "Mark done, or not done",
    },
    Binding {
        keys: &[chord![Key::char('p')]],
        action: Action::SetPriority,
        context: Context::Global,
        group: Group::Task,
        doc: "Set priority",
    },
    Binding {
        keys: &[chord![Key::char('D')]],
        action: Action::SetDue,
        context: Context::Global,
        group: Group::Task,
        doc: "Set the due date",
    },
    Binding {
        keys: &[chord![Key::char('m')]],
        action: Action::MoveTask,
        context: Context::Global,
        group: Group::Task,
        doc: "Move to another project",
    },
    Binding {
        // List-only, where every other task key is global: `l` already means "expand" in
        // the sidebar, and that is the vim meaning nobody should have to unlearn. The
        // cost is that `l` does nothing while the preview has focus; `:` reaches it by
        // name from anywhere, and taking the sidebar's `l` away would be the worse trade.
        keys: &[chord![Key::char('l')]],
        action: Action::SetLabels,
        context: Context::List,
        group: Group::Task,
        doc: "Add or remove labels",
    },
    Binding {
        keys: &[chord![Key::char(' ')]],
        action: Action::QuickAction,
        context: Context::Global,
        group: Group::Task,
        doc: "Configured quick actions",
    },
    Binding {
        keys: &[chord![Key::char('x')]],
        action: Action::DeleteTask,
        context: Context::Global,
        group: Group::Task,
        doc: "Delete the task",
    },
    Binding {
        keys: &[chord![Key::char('u')]],
        action: Action::Undo,
        context: Context::Global,
        group: Group::Task,
        doc: "Undo the last change",
    },
    Binding {
        keys: &[chord![Key::ctrl('r')]],
        action: Action::Redo,
        context: Context::Global,
        group: Group::Task,
        doc: "Redo",
    },
    Binding {
        keys: &[chord![Key::char('r')]],
        action: Action::SyncNow,
        context: Context::Global,
        group: Group::Application,
        doc: "Sync changes",
    },
    Binding {
        keys: &[chord![Key::char('R')]],
        action: Action::SyncFull,
        context: Context::Global,
        group: Group::Application,
        doc: "Sync everything",
    },
    Binding {
        keys: &[chord![Key::char(':')]],
        action: Action::CommandPalette,
        context: Context::Global,
        group: Group::Application,
        doc: "Run a command by name",
    },
    Binding {
        keys: &[chord![Key::char('?')]],
        action: Action::Help,
        context: Context::Global,
        group: Group::Application,
        doc: "Show this help",
    },
    Binding {
        keys: &[chord![Key::char('q')], chord![Key::ctrl('c')]],
        action: Action::Quit,
        context: Context::Global,
        group: Group::Application,
        doc: "Quit",
    },
];

/// One line of the help modal.
///
/// Built here rather than in the renderer so the modal's height, its scroll limit and
/// what it draws are all counted from the same list. Three counts would be three chances
/// to disagree, which is how a help screen ends up able to scroll past its own end.
#[derive(Debug, Clone, Copy)]
pub enum HelpRow {
    /// A section heading.
    Heading(&'static str),
    /// A binding.
    Binding(&'static Binding),
    /// A blank line between sections.
    Blank,
}

/// The help modal's content, in order, for `context`.
#[must_use]
pub fn help_rows(context: Context) -> Vec<HelpRow> {
    let mut rows = Vec::new();
    for group in Group::all() {
        rows.push(HelpRow::Heading(group.heading()));
        rows.extend(
            bindings_in(*group, context)
                .into_iter()
                .map(HelpRow::Binding),
        );
        rows.push(HelpRow::Blank);
    }
    // The trailing blank is a separator with nothing after it.
    rows.pop();
    rows
}

/// What looking a key up produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// The key completed a binding.
    Action(Action),
    /// The key began a chord. The caller holds it and waits for the next one.
    Pending,
    /// The key is bound to nothing here.
    Unbound,
}

/// Resolve `key`, pressed after `pending`, in `context`.
///
/// A complete match wins over a prefix match, so a binding on `g` would shadow `g g`
/// rather than becoming unreachable. There is no such pair today and this makes adding
/// one a visible decision instead of a silent breakage.
#[must_use]
pub fn resolve(pending: &[Key], key: Key, context: Context) -> Resolved {
    let mut sequence = pending.to_vec();
    sequence.push(key);

    let applies =
        |binding: &&Binding| binding.context == Context::Global || binding.context == context;

    for binding in KEYMAP.iter().filter(applies) {
        if binding.keys.contains(&sequence.as_slice()) {
            return Resolved::Action(binding.action);
        }
    }
    for binding in KEYMAP.iter().filter(applies) {
        if binding
            .keys
            .iter()
            .any(|chord| chord.len() > sequence.len() && chord.starts_with(sequence.as_slice()))
        {
            return Resolved::Pending;
        }
    }
    Resolved::Unbound
}

/// The bindings in `group` that apply in `context`, for the help modal.
#[must_use]
pub fn bindings_in(group: Group, context: Context) -> Vec<&'static Binding> {
    KEYMAP
        .iter()
        .filter(|binding| {
            binding.group == group
                && (binding.context == Context::Global || binding.context == context)
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn press(c: char) -> Key {
        Key::char(c)
    }

    #[test]
    fn a_plain_key_resolves_to_its_action() {
        assert_eq!(
            resolve(&[], press('j'), Context::List),
            Resolved::Action(Action::MoveDown)
        );
    }

    #[test]
    fn a_prefix_waits_for_the_rest_of_the_chord() {
        assert_eq!(resolve(&[], press('g'), Context::List), Resolved::Pending);
        assert_eq!(
            resolve(&[press('g')], press('g'), Context::List),
            Resolved::Action(Action::Top)
        );
        assert_eq!(
            resolve(&[press('g')], press('p'), Context::List),
            Resolved::Action(Action::GotoProject)
        );
    }

    #[test]
    fn an_unbound_second_key_cancels_the_chord_rather_than_doing_something_else() {
        // `x` alone is unbound, but the point is that `g x` does not fall through to
        // whatever `x` would mean on its own.
        assert_eq!(
            resolve(&[press('g')], press('j'), Context::List),
            Resolved::Unbound
        );
    }

    #[test]
    fn enter_means_different_things_in_different_panes() {
        let enter = Key::plain(KeyCode::Enter);
        assert_eq!(
            resolve(&[], enter, Context::List),
            Resolved::Action(Action::OpenTask)
        );
        assert_eq!(
            resolve(&[], enter, Context::Sidebar),
            Resolved::Action(Action::ExpandOrOpen)
        );
    }

    #[test]
    fn a_sidebar_binding_is_invisible_from_the_list() {
        assert_eq!(resolve(&[], press('h'), Context::List), Resolved::Unbound);
        assert_eq!(
            resolve(&[], press('h'), Context::Sidebar),
            Resolved::Action(Action::CollapseOrParent)
        );
    }

    #[test]
    fn shift_h_is_the_layout_key_not_the_vim_one() {
        assert_eq!(
            resolve(&[], press('H'), Context::List),
            Resolved::Action(Action::PreviousLayout)
        );
    }

    #[test]
    fn an_uppercase_press_drops_the_redundant_shift_modifier() {
        let event = KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT);
        let key = Key::from_event(event).unwrap();
        assert_eq!(key, Key::char('H'));
        assert_eq!(
            resolve(&[], key, Context::List),
            Resolved::Action(Action::PreviousLayout)
        );
    }

    #[test]
    fn a_key_release_is_not_a_key_press() {
        let event = KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(Key::from_event(event).is_none());
    }

    #[test]
    fn every_binding_is_reachable_and_no_two_share_a_chord_in_one_context() {
        for (index, binding) in KEYMAP.iter().enumerate() {
            assert!(
                !binding.keys.is_empty(),
                "{:?} binds nothing",
                binding.action
            );
            for chord in binding.keys {
                assert!(!chord.is_empty(), "{:?} has an empty chord", binding.action);
                for other in KEYMAP.iter().skip(index + 1) {
                    let overlaps = binding.context == other.context
                        || binding.context == Context::Global
                        || other.context == Context::Global;
                    if overlaps {
                        assert!(
                            !other.keys.contains(chord),
                            "{:?} and {:?} both bind {}",
                            binding.action,
                            other.action,
                            binding.keys_display()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn help_renders_a_chord_as_a_sequence_and_alternatives_with_a_slash() {
        let top = KEYMAP
            .iter()
            .find(|binding| binding.action == Action::Top)
            .unwrap();
        assert_eq!(top.keys_display(), "g g / Home");

        let down = KEYMAP
            .iter()
            .find(|binding| binding.action == Action::MoveDown)
            .unwrap();
        assert_eq!(down.keys_display(), "j / ↓");
    }

    #[test]
    fn the_palette_offers_commands_and_not_motions() {
        let offered: Vec<Action> = KEYMAP
            .iter()
            .map(|binding| binding.action)
            .filter(|action| action.is_command())
            .collect();
        assert!(offered.contains(&Action::ToggleSidebar));
        assert!(offered.contains(&Action::SyncNow));
        assert!(!offered.contains(&Action::MoveDown));
        assert!(
            !offered.contains(&Action::CommandPalette),
            "the palette must not offer itself"
        );
    }

    #[test]
    fn the_palette_key_is_bound_and_free_of_conflicts() {
        assert_eq!(
            resolve(&[], press(':'), Context::List),
            Resolved::Action(Action::CommandPalette)
        );
    }

    #[test]
    fn every_group_has_something_in_it() {
        for group in Group::all() {
            assert!(
                !bindings_in(*group, Context::List).is_empty(),
                "{} is empty",
                group.heading()
            );
        }
    }
}
