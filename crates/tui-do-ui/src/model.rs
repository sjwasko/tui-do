//! Everything the interface knows.
//!
//! One struct, no `Option<Modal>` paired with a `bool`, and no field that is only
//! meaningful when another field holds a particular value. The predecessor's application
//! state carried around a hundred fields including twenty-two `show_*_modal` booleans,
//! which is why every modal needed a branch in a 790-line function.

use chrono::{DateTime, Utc};
use tui_do_core::config::columns::ColumnLayout;
use tui_do_core::config::{Config, QuickAction, ViewConfig};
use tui_do_core::models::{Label, Project, ProjectId, Task, TaskId};
use tui_do_core::store::{Mutation, ProjectCounts};

use crate::geometry::{self, Frames};
use crate::keymap::{Context, Key};
use crate::modal::Modal;
use crate::query::{Query, QueryId, Scope};
use crate::sidebar::{SidebarState, SidebarTarget};
use crate::theme::Theme;

/// Which pane the keyboard is driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The project tree.
    Sidebar,
    /// The task list.
    #[default]
    List,
    /// The task preview.
    Preview,
}

impl Focus {
    /// The keymap context this focus implies.
    #[must_use]
    pub const fn context(self) -> Context {
        match self {
            Self::Sidebar => Context::Sidebar,
            Self::List => Context::List,
            // The preview has no bindings of its own; motion keys reach it as globals.
            Self::Preview => Context::Global,
        }
    }
}

/// Whether an optional pane is showing, and whether the user said so.
///
/// Three states rather than a boolean because "hidden because the terminal is narrow" and
/// "hidden because I hid it" must survive a resize differently. A resize re-evaluates
/// only panes still on `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneState {
    /// Follow the terminal width.
    #[default]
    Auto,
    /// The user asked for it.
    Shown,
    /// The user dismissed it.
    Hidden,
}

impl PaneState {
    /// Whether the user wants this pane at this width.
    ///
    /// Intent, not visibility. Whether there is *room* is [`crate::geometry::frames`]'s
    /// decision and only its decision — a second width rule here is how `z p` came to set
    /// a pane to `Shown` that never appeared, with nothing on screen to say why.
    #[must_use]
    pub const fn wanted(self, width: u16, auto_min: u16) -> bool {
        match self {
            Self::Auto => width >= auto_min,
            Self::Shown => true,
            Self::Hidden => false,
        }
    }

    /// The state that flips what is currently showing, pinning the result.
    #[must_use]
    pub const fn toggled(self, currently_visible: bool) -> Self {
        if currently_visible {
            Self::Hidden
        } else {
            Self::Shown
        }
    }
}

/// The optional panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Panes {
    /// The project tree.
    pub sidebar: PaneState,
    /// The task preview.
    pub preview: PaneState,
}

impl Panes {
    /// Whether the sidebar is wanted at this width.
    #[must_use]
    pub const fn sidebar_wanted(self, width: u16) -> bool {
        self.sidebar.wanted(width, geometry::SIDEBAR_AUTO_MIN)
    }

    /// Whether the preview is wanted at this width.
    #[must_use]
    pub const fn preview_wanted(self, width: u16) -> bool {
        self.preview.wanted(width, geometry::PREVIEW_AUTO_MIN)
    }
}

/// What the sync engine is doing, as far as the interface knows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SyncStatus {
    /// Nothing in flight.
    #[default]
    Idle,
    /// A pass is running.
    Working {
        /// What it is working on, already worded for display.
        detail: String,
    },
    /// The last pass could not finish. Queued changes are still queued.
    Failed {
        /// What went wrong.
        message: String,
    },
}

/// How loud a toast is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Confirmation of something the user did.
    Info,
    /// Something they should notice.
    Warning,
    /// Something went wrong.
    Error,
}

/// A transient message along the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// What it says.
    pub text: String,
    /// How loud.
    pub level: Level,
    /// Ticks left before it disappears. Counted down by [`crate::Msg::Tick`], because
    /// `update` has no clock of its own.
    pub ticks: u8,
}

impl Toast {
    /// Roughly five seconds at the runtime's tick cadence.
    pub const LIFETIME: u8 = 5;

    /// A toast at [`Level::Info`].
    #[must_use]
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: Level::Info,
            ticks: Self::LIFETIME,
        }
    }

    /// A toast at [`Level::Warning`].
    #[must_use]
    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: Level::Warning,
            ticks: Self::LIFETIME,
        }
    }

    /// A toast at [`Level::Error`].
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: Level::Error,
            ticks: Self::LIFETIME,
        }
    }
}

/// The always-visible state of the application itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// What sync is doing.
    pub sync: SyncStatus,
    /// When the last pass finished.
    pub last_sync: Option<DateTime<Utc>>,
    /// Local changes not yet accepted by the server.
    pub queued: usize,
    /// The current transient message, if any.
    pub toast: Option<Toast>,
}

/// What the store last answered with.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// The current task list, already ordered by the store.
    pub tasks: Vec<Task>,
    /// Every project, including pseudo-projects; the sidebar filters them.
    pub projects: Vec<Project>,
    /// Every label, for the picker and for colouring.
    pub labels: Vec<Label>,
    /// Per-project counts for the sidebar.
    pub counts: ProjectCounts,
    /// Whether a task query is in flight. Distinguishes "no tasks" from "not yet".
    pub loading: bool,
    /// The last store failure, shown as a banner rather than a modal so the rest of the
    /// interface stays usable.
    pub error: Option<String>,
    /// Whether the list hit [`crate::query::TASK_LIMIT`].
    pub truncated: bool,
}

/// The task list's own cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskList {
    /// The selected task, by id rather than by index: a reload that reorders or drops
    /// rows must not silently move the cursor onto a different task.
    pub selected: Option<TaskId>,
    /// First visible row.
    pub offset: usize,
    /// How far the preview is scrolled.
    pub preview_scroll: u16,
}

/// Which view of the tasks is showing.
///
/// One variant today. Phase 5 adds the full task screen and Phase 6 the Vikunja views,
/// and each is a variant plus one arm — never a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Screen {
    /// The task list.
    #[default]
    Tasks,
}

/// Everything the interface knows.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// Which view is showing.
    pub screen: Screen,
    /// The modal stack. The last entry owns the keyboard.
    pub modals: Vec<Modal>,
    /// Which pane the keyboard is driving.
    pub focus: Focus,
    /// Keys of a chord already pressed, waiting for the rest.
    pub pending: Vec<Key>,
    /// Which optional panes are showing.
    pub panes: Panes,
    /// The project tree's state.
    pub sidebar: SidebarState,
    /// The task list's cursor.
    pub list: TaskList,
    /// What the store last answered with.
    pub data: Snapshot,
    /// What the list is asking for.
    pub query: Query,
    /// The id of that request; older answers are dropped.
    pub query_id: QueryId,
    /// The layouts the user configured, or the built-in ones.
    pub layouts: Vec<ColumnLayout>,
    /// Which of them is active.
    pub layout_ix: usize,
    /// Sync state and transient messages.
    pub status: Status,
    /// The colours. Set once by the runtime, which is what knows how much colour the
    /// terminal can show; `tui-do-ui` never sniffs a variable.
    pub theme: Theme,
    /// Terminal size, as columns by rows.
    pub size: (u16, u16),
    /// The last time the runtime told us about. `update` never reads a clock.
    pub now: DateTime<Utc>,
    /// Inverses of what has been done, newest last. Session-scoped: an inverse built
    /// against yesterday's state would meet a task the server has changed since, and
    /// lose in a way that is hard to explain.
    pub undo: Vec<Mutation>,
    /// Inverses of what has been undone. Cleared by any new edit.
    pub redo: Vec<Mutation>,
    /// Where a task with no project named goes, as written in the config.
    ///
    /// Held as the user wrote it rather than as an id, because a title can only be
    /// resolved once the projects have loaded and this is built before they have.
    pub default_project: Option<String>,
    /// The single-key edits the user configured, in the order they wrote them.
    ///
    /// Held as written, names and all, for the same reason [`Model::default_project`] is:
    /// a project title can only be resolved once the projects have loaded, and this is
    /// built before they have.
    pub quick_actions: Vec<QuickAction>,
    /// Cleared when the user quits; the runtime stops when this goes false.
    pub running: bool,
}

impl Model {
    /// A model showing `scope`, configured by `config`.
    ///
    /// Takes the scope already resolved rather than resolving it, because the precedence
    /// — configured project, then last session's, then everything — needs the project
    /// list, and `update` cannot go and read one. See [`landing_scope`].
    #[must_use]
    pub fn new(config: &Config, scope: Scope, now: DateTime<Utc>, size: (u16, u16)) -> Self {
        let layouts = config.view.effective_layouts();
        let layout_ix = config
            .view
            .active_layout
            .as_deref()
            .and_then(|name| layouts.iter().position(|layout| layout.name == name))
            .unwrap_or(0);
        let sort = layouts
            .get(layout_ix)
            .map(crate::query::sort_for)
            .unwrap_or_default();

        Self {
            screen: Screen::default(),
            modals: Vec::new(),
            focus: Focus::default(),
            pending: Vec::new(),
            panes: Panes::default(),
            sidebar: SidebarState {
                selected: match scope {
                    Scope::Project(id) => SidebarTarget::Project(id),
                    Scope::Favorites => SidebarTarget::Favorites,
                    _ => SidebarTarget::AllTasks,
                },
                ..SidebarState::default()
            },
            list: TaskList::default(),
            data: Snapshot {
                loading: true,
                ..Snapshot::default()
            },
            query: Query {
                scope,
                // Deliberately not seeded from `view.default_filter`: that is a Vikunja
                // filter expression such as `done = false && priority >= 3`, and the
                // local store answers a title substring. Treating one as the other would
                // silently return nothing. Filter expressions arrive with saved filters
                // in Phase 6, through the views API that evaluates them.
                search: None,
                include_done: false,
                sort,
            },
            query_id: QueryId::default(),
            layouts,
            layout_ix,
            status: Status::default(),
            theme: Theme::default(),
            size,
            now,
            undo: Vec::new(),
            redo: Vec::new(),
            default_project: config.view.default_project.clone(),
            quick_actions: config.quick_actions.clone(),
            running: true,
        }
    }

    /// The active column layout.
    #[must_use]
    pub fn layout(&self) -> &ColumnLayout {
        self.layouts
            .get(self.layout_ix)
            .unwrap_or_else(|| &EMPTY_LAYOUT)
    }

    /// Where the panes are, at the current size.
    #[must_use]
    pub fn frames(&self) -> Frames {
        let (width, height) = self.size;
        geometry::frames(
            width,
            height,
            self.panes.sidebar_wanted(width),
            self.preview_wanted(),
        )
    }

    /// Whether the preview pane is wanted *and* has something to show.
    ///
    /// It needs something to preview: an empty list would otherwise draw an empty box
    /// where a third of the task list used to be.
    #[must_use]
    pub fn preview_wanted(&self) -> bool {
        self.panes.preview_wanted(self.size.0) && self.selected_task().is_some()
    }

    /// Whether the sidebar is actually on screen.
    ///
    /// Asks the layout rather than the pane state, so "is it showing" has one answer.
    #[must_use]
    pub fn sidebar_showing(&self) -> bool {
        self.frames().sidebar.is_some()
    }

    /// Whether the preview is actually on screen.
    #[must_use]
    pub fn preview_showing(&self) -> bool {
        self.frames().preview.is_some()
    }

    /// The selected task, if it is still in the list.
    #[must_use]
    pub fn selected_task(&self) -> Option<&Task> {
        let id = self.list.selected?;
        self.data.tasks.iter().find(|task| task.id == id)
    }

    /// Where the selection sits in the current list.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.list.selected?;
        self.data.tasks.iter().position(|task| task.id == id)
    }

    /// The project a task belongs to, for the list's project column.
    #[must_use]
    pub fn project(&self, id: ProjectId) -> Option<&Project> {
        self.data.projects.iter().find(|project| project.id == id)
    }

    /// The keymap context the focused pane implies.
    #[must_use]
    pub fn context(&self) -> Context {
        self.focus.context()
    }

    /// Show a transient message.
    pub fn toast(&mut self, toast: Toast) {
        self.status.toast = Some(toast);
    }
}

/// The layout used when the configured list is somehow empty. Never rendered in practice;
/// it exists so [`Model::layout`] can return a reference without unwrapping.
static EMPTY_LAYOUT: std::sync::LazyLock<ColumnLayout> =
    std::sync::LazyLock::new(|| ColumnLayout {
        name: "default".to_string(),
        description: None,
        columns: vec![tui_do_core::config::columns::ColumnSpec::new(
            tui_do_core::config::columns::Column::Title,
        )],
    });

/// Which scope to open with.
///
/// Precedence: the configured project, then the one the last session was showing, then
/// everything. A configured name that matches nothing loses to the remembered project
/// rather than to an error — the config may simply be ahead of a sync.
#[must_use]
pub fn landing_scope(
    view: &ViewConfig,
    remembered: Option<ProjectId>,
    projects: &[Project],
) -> Scope {
    if let Some(spec) = view.default_project.as_deref() {
        let spec = spec.trim();
        let real = || projects.iter().filter(|project| project.id.get() > 0);
        // `#12` names a project by id. Two projects may share a title, so a title alone
        // cannot always say which one was meant.
        let found = spec
            .strip_prefix('#')
            .and_then(|n| n.parse::<i64>().ok())
            .and_then(|id| real().find(|project| project.id.get() == id))
            .or_else(|| real().find(|project| project.title.eq_ignore_ascii_case(spec)));
        if let Some(project) = found {
            return Scope::Project(project.id);
        }
    }
    match remembered {
        Some(id) if projects.iter().any(|project| project.id == id) => Scope::Project(id),
        _ => Scope::All,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn project(id: i64, title: &str) -> Project {
        Project {
            id: ProjectId(id),
            title: title.to_string(),
            ..Project::default()
        }
    }

    #[test]
    fn an_auto_pane_follows_the_width_and_a_pinned_one_is_never_second_guessed() {
        let auto = PaneState::Auto;
        assert!(auto.wanted(120, geometry::SIDEBAR_AUTO_MIN));
        assert!(!auto.wanted(80, geometry::SIDEBAR_AUTO_MIN));

        // A pin is intent, and intent does not depend on the width. Whether there is room
        // is `geometry::frames`'s call and nobody else's -- a second width rule here is
        // what made `z p` set a pane that never appeared.
        let pinned = PaneState::Shown;
        assert!(pinned.wanted(80, geometry::SIDEBAR_AUTO_MIN));
        assert!(pinned.wanted(40, geometry::SIDEBAR_AUTO_MIN));

        assert!(!PaneState::Hidden.wanted(200, geometry::SIDEBAR_AUTO_MIN));
    }

    #[test]
    fn toggling_pins_the_opposite_of_what_is_showing() {
        assert_eq!(PaneState::Auto.toggled(true), PaneState::Hidden);
        assert_eq!(PaneState::Auto.toggled(false), PaneState::Shown);
        assert_eq!(PaneState::Hidden.toggled(false), PaneState::Shown);
    }

    #[test]
    fn the_configured_project_wins_the_landing_and_a_stale_name_falls_back() {
        let projects = vec![project(1, "Work"), project(2, "Personal")];
        let view = ViewConfig {
            default_project: Some("personal".to_string()),
            ..ViewConfig::default()
        };
        assert_eq!(
            landing_scope(&view, Some(ProjectId(1)), &projects),
            Scope::Project(ProjectId(2))
        );

        let stale = ViewConfig {
            default_project: Some("Deleted".to_string()),
            ..ViewConfig::default()
        };
        assert_eq!(
            landing_scope(&stale, Some(ProjectId(1)), &projects),
            Scope::Project(ProjectId(1))
        );
        assert_eq!(landing_scope(&stale, None, &projects), Scope::All);
    }

    #[test]
    fn a_remembered_project_that_no_longer_exists_lands_on_everything() {
        let projects = vec![project(1, "Work")];
        assert_eq!(
            landing_scope(&ViewConfig::default(), Some(ProjectId(99)), &projects),
            Scope::All
        );
    }

    #[test]
    fn the_active_layout_comes_from_the_config_by_name() {
        let mut config = Config::example();
        config.view.active_layout = Some("compact".to_string());
        let model = Model::new(&config, Scope::All, Utc::now(), (120, 40));
        assert_eq!(model.layout().name, "compact");
    }
}
