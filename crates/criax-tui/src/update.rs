//! The one function that changes anything.
//!
//! `update` is pure and synchronous: it takes a message, mutates the model, and returns
//! the side effects it would like performed. It cannot await, cannot read a clock and
//! cannot touch a store — every one of those arrives as a [`Msg`] and leaves as an
//! [`Effect`]. That is what keeps the render loop from ever blocking.

use criax_core::models::{Project, ProjectId};
use criax_core::sync::{Phase, Stage};
use criax_core::SyncEvent;

use crate::effect::Effect;
use crate::keymap::{resolve, Action, Key, Resolved, KEYMAP};
use crate::modal::{
    Candidate, HelpState, Modal, Outcome, Pick, PickerKind, PickerState, SearchState, Submission,
};
use crate::model::{Focus, Model, SyncStatus, Toast};
use crate::msg::Msg;
use crate::query::{Scope, TASK_LIMIT};
use crate::sidebar::{self, SidebarTarget};

/// Apply a message.
pub fn update(model: &mut Model, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Key(event) => match Key::from_event(event) {
            Some(key) => on_key(model, key),
            None => Vec::new(),
        },
        Msg::Resize(width, height) => {
            model.size = (width, height);
            settle_focus(model);
            keep_selection_visible(model);
            Vec::new()
        }
        Msg::Tick(now) => {
            model.now = now;
            if let Some(toast) = &mut model.status.toast {
                toast.ticks = toast.ticks.saturating_sub(1);
                if toast.ticks == 0 {
                    model.status.toast = None;
                }
            }
            Vec::new()
        }
        Msg::TasksLoaded { id, tasks } => {
            // An answer to a query the user has already moved on from. Dropping it is the
            // whole reason the id exists.
            if id != model.query_id {
                return Vec::new();
            }
            model.data.truncated = tasks.len() as u32 >= TASK_LIMIT;
            model.data.tasks = tasks;
            model.data.loading = false;
            model.data.error = None;
            if model.selected_index().is_none() {
                model.list.selected = model.data.tasks.first().map(|task| task.id);
                model.list.preview_scroll = 0;
            }
            keep_selection_visible(model);
            Vec::new()
        }
        Msg::ProjectsLoaded(projects) => {
            model.data.projects = projects;
            Vec::new()
        }
        Msg::LabelsLoaded(labels) => {
            model.data.labels = labels;
            Vec::new()
        }
        Msg::CountsLoaded(counts) => {
            model.data.counts = counts;
            Vec::new()
        }
        Msg::StoreFailed(message) => {
            model.data.loading = false;
            model.data.error = Some(message.clone());
            model.toast(Toast::error(message));
            Vec::new()
        }
        Msg::Sync(event) => on_sync(model, event),
    }
}

/// React to a sync event.
///
/// A finished pass is the only one that reloads: the store has just changed underneath
/// the list, and nothing else would tell the interface about it.
fn on_sync(model: &mut Model, event: SyncEvent) -> Vec<Effect> {
    match event {
        SyncEvent::Started(phase) => {
            model.status.sync = SyncStatus::Working {
                detail: match phase {
                    Phase::Push => "sending".to_string(),
                    Phase::Pull => "fetching".to_string(),
                },
            };
            Vec::new()
        }
        SyncEvent::Progress { stage, stored, .. } => {
            let what = match stage {
                Stage::Projects => "projects",
                Stage::Labels => "labels",
                Stage::Tasks => "tasks",
            };
            model.status.sync = SyncStatus::Working {
                detail: format!("{stored} {what}"),
            };
            Vec::new()
        }
        SyncEvent::Rejected { kind, message, .. } => {
            // The one event the user must see: their edit has just vanished from the
            // screen and they are owed an explanation.
            model.toast(Toast::error(format!("{kind} rejected: {message}")));
            Vec::new()
        }
        SyncEvent::Finished(report) => {
            model.status.sync = SyncStatus::Idle;
            model.status.last_sync = Some(model.now);
            model.status.queued = report.push.deferred;
            reload_everything(model)
        }
        SyncEvent::Failed { message, .. } => {
            model.status.sync = SyncStatus::Failed { message };
            Vec::new()
        }
    }
}

/// Route a key press.
fn on_key(model: &mut Model, key: Key) -> Vec<Effect> {
    // Quit reaches through a modal. Nothing else does.
    if key == Key::ctrl('c') {
        return quit(model);
    }

    if let Some(modal) = model.modals.last_mut() {
        return match modal.handle(key) {
            Outcome::Consumed => Vec::new(),
            Outcome::Dismiss => {
                model.modals.pop();
                Vec::new()
            }
            Outcome::Submit(submission) => {
                model.modals.pop();
                on_submit(model, submission)
            }
            // Applied without closing. The re-query is safe to fire on every keystroke
            // precisely because of `QueryId`: answers to the text the user has already
            // typed past are dropped rather than raced into the list.
            Outcome::Update(submission) => on_submit(model, submission),
        };
    }

    match resolve(&model.pending, key, model.context()) {
        Resolved::Pending => {
            model.pending.push(key);
            Vec::new()
        }
        Resolved::Unbound => {
            model.pending.clear();
            Vec::new()
        }
        Resolved::Action(action) => {
            model.pending.clear();
            act(model, action)
        }
    }
}

/// Apply what a modal decided.
fn on_submit(model: &mut Model, submission: Submission) -> Vec<Effect> {
    match submission {
        Submission::Search(text) => {
            model.query.search = (!text.trim().is_empty()).then(|| text.trim().to_string());
            reload_tasks(model)
        }
        Submission::Picked(Pick::Project(id)) => show(model, Scope::Project(id)),
        Submission::Picked(Pick::Label(id)) => show(model, Scope::Label(id)),
        // A command chosen by name does exactly what its key does. One implementation,
        // so the two can never disagree about what "toggle the sidebar" means.
        Submission::Picked(Pick::Command(action)) => act(model, action),
    }
}

/// Perform an action.
fn act(model: &mut Model, action: Action) -> Vec<Effect> {
    let page = model.frames().list_rows().max(1) as isize;
    match action {
        Action::MoveDown => step(model, 1),
        Action::MoveUp => step(model, -1),
        Action::PageDown => step(model, page),
        Action::PageUp => step(model, -page),
        Action::Top => jump(model, true),
        Action::Bottom => jump(model, false),
        Action::FocusNext => {
            cycle_focus(model, true);
            Vec::new()
        }
        Action::FocusPrevious => {
            cycle_focus(model, false);
            Vec::new()
        }
        Action::Back => back(model),
        Action::ExpandOrOpen => expand_or_open(model),
        Action::CollapseOrParent => collapse_or_parent(model),
        Action::OpenProject => {
            let target = model.sidebar.selected;
            select_target(model, target)
        }
        Action::OpenTask => {
            if model.selected_task().is_some() {
                model.panes.preview = crate::model::PaneState::Shown;
                model.focus = Focus::Preview;
            }
            Vec::new()
        }
        Action::ToggleSidebar => {
            let showing = model.sidebar_showing();
            model.panes.sidebar = model.panes.sidebar.toggled(showing);
            settle_focus(model);
            if !showing && !model.sidebar_showing() {
                model.toast(Toast::info(no_room(Pane::Sidebar, model)));
            }
            Vec::new()
        }
        Action::TogglePreview => {
            let showing = model.preview_showing();
            model.panes.preview = model.panes.preview.toggled(showing);
            settle_focus(model);
            if !showing && !model.preview_showing() {
                let reason = if model.selected_task().is_none() {
                    "No task selected to preview".to_string()
                } else {
                    no_room(Pane::Preview, model)
                };
                model.toast(Toast::info(reason));
            }
            Vec::new()
        }
        Action::ToggleDoneTasks => {
            model.query.include_done = !model.query.include_done;
            model.toast(Toast::info(if model.query.include_done {
                "Showing completed tasks"
            } else {
                "Hiding completed tasks"
            }));
            reload_tasks(model)
        }
        Action::NextLayout => switch_layout(model, 1),
        Action::PreviousLayout => switch_layout(model, -1),
        Action::Search => {
            model
                .modals
                .push(Modal::Search(SearchState::new(model.query.search.clone())));
            Vec::new()
        }
        Action::GotoProject => {
            let candidates = model
                .data
                .projects
                .iter()
                .filter(|project| project.id.get() > 0 && !project.is_archived)
                .map(|project| Candidate::new(Pick::Project(project.id), &project.title))
                .collect();
            model.modals.push(Modal::Picker(PickerState::new(
                PickerKind::Project,
                sorted(candidates),
            )));
            Vec::new()
        }
        Action::GotoLabel => {
            let candidates = model
                .data
                .labels
                .iter()
                .map(|label| Candidate::new(Pick::Label(label.id), &label.title))
                .collect();
            model.modals.push(Modal::Picker(PickerState::new(
                PickerKind::Label,
                sorted(candidates),
            )));
            Vec::new()
        }
        Action::SyncNow => {
            model.status.sync = SyncStatus::Working {
                detail: "starting".to_string(),
            };
            vec![Effect::SyncNow]
        }
        Action::CommandPalette => {
            // Built from the keymap, in the focused pane's context, so the palette and
            // the help modal list the same things for the same reason.
            let candidates = KEYMAP
                .iter()
                .filter(|binding| {
                    binding.action.is_command()
                        && (binding.context == crate::keymap::Context::Global
                            || binding.context == model.context())
                })
                .map(|binding| {
                    Candidate::hinted(
                        Pick::Command(binding.action),
                        binding.doc,
                        binding.keys_display(),
                    )
                })
                .collect();
            model.modals.push(Modal::Picker(PickerState::new(
                PickerKind::Command,
                candidates,
            )));
            Vec::new()
        }
        Action::Help => {
            model.modals.push(Modal::Help(HelpState {
                context: model.context(),
                offset: 0,
            }));
            Vec::new()
        }
        Action::Quit => quit(model),
    }
}

/// Candidates in title order, which is what a picker with nothing typed should show.
fn sorted(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by_key(|candidate| candidate.title.to_lowercase());
    candidates
}

fn quit(model: &mut Model) -> Vec<Effect> {
    model.running = false;
    vec![Effect::Quit]
}

/// Move the focused pane's cursor by `delta` rows.
fn step(model: &mut Model, delta: isize) -> Vec<Effect> {
    match model.focus {
        Focus::Preview => {
            let scroll = i64::from(model.list.preview_scroll) + delta as i64;
            model.list.preview_scroll = scroll.clamp(0, i64::from(u16::MAX)) as u16;
            Vec::new()
        }
        Focus::List => {
            let Some(current) = model.selected_index() else {
                return Vec::new();
            };
            let last = model.data.tasks.len().saturating_sub(1);
            let next = (current as isize + delta).clamp(0, last as isize) as usize;
            model.list.selected = model.data.tasks.get(next).map(|task| task.id);
            model.list.preview_scroll = 0;
            keep_selection_visible(model);
            Vec::new()
        }
        Focus::Sidebar => {
            let targets = sidebar::targets(&sidebar::rows(
                &model.data.projects,
                &model.data.counts,
                &model.sidebar,
            ));
            let Some(current) = targets.iter().position(|t| *t == model.sidebar.selected) else {
                return Vec::new();
            };
            let last = targets.len().saturating_sub(1);
            let next = (current as isize + delta).clamp(0, last as isize) as usize;
            match targets.get(next) {
                Some(target) if *target != model.sidebar.selected => select_target(model, *target),
                _ => Vec::new(),
            }
        }
    }
}

/// Jump to the first or last row of the focused pane.
fn jump(model: &mut Model, first: bool) -> Vec<Effect> {
    match model.focus {
        Focus::Preview => {
            if first {
                model.list.preview_scroll = 0;
            }
            Vec::new()
        }
        Focus::List => {
            let task = if first {
                model.data.tasks.first()
            } else {
                model.data.tasks.last()
            };
            model.list.selected = task.map(|task| task.id);
            model.list.preview_scroll = 0;
            keep_selection_visible(model);
            Vec::new()
        }
        Focus::Sidebar => {
            let targets = sidebar::targets(&sidebar::rows(
                &model.data.projects,
                &model.data.counts,
                &model.sidebar,
            ));
            let target = if first {
                targets.first()
            } else {
                targets.last()
            };
            match target {
                Some(target) if *target != model.sidebar.selected => select_target(model, *target),
                _ => Vec::new(),
            }
        }
    }
}

/// Expand a collapsed project, or hand the keyboard to the list.
fn expand_or_open(model: &mut Model) -> Vec<Effect> {
    if let SidebarTarget::Project(id) = model.sidebar.selected {
        if has_children(&model.data.projects, id) && model.sidebar.collapsed.contains(&id) {
            model.sidebar.collapsed.remove(&id);
            return Vec::new();
        }
    }
    model.focus = Focus::List;
    Vec::new()
}

/// Collapse an expanded project, or move to its parent.
fn collapse_or_parent(model: &mut Model) -> Vec<Effect> {
    let SidebarTarget::Project(id) = model.sidebar.selected else {
        return Vec::new();
    };
    if has_children(&model.data.projects, id) && !model.sidebar.collapsed.contains(&id) {
        model.sidebar.collapsed.insert(id);
        return Vec::new();
    }
    match sidebar::parent_of(&model.data.projects, id) {
        Some(parent) => select_target(model, SidebarTarget::Project(parent)),
        None => Vec::new(),
    }
}

fn has_children(projects: &[Project], id: ProjectId) -> bool {
    projects
        .iter()
        .any(|project| project.parent_project_id == id && project.id != id)
}

/// Select a sidebar row, which loads it.
///
/// Moving the sidebar cursor shows that project immediately rather than waiting for
/// `Enter`. Racing queries are what [`crate::query::QueryId`] is for.
fn select_target(model: &mut Model, target: SidebarTarget) -> Vec<Effect> {
    model.sidebar.selected = target;
    let scope = match target {
        SidebarTarget::AllTasks => Scope::All,
        SidebarTarget::Favorites => Scope::Favorites,
        SidebarTarget::Project(id) => Scope::Project(id),
    };
    show(model, scope)
}

/// Show a scope, remembering it for next launch.
fn show(model: &mut Model, scope: Scope) -> Vec<Effect> {
    model.query.scope = scope;
    model.sidebar.selected = match scope {
        Scope::Project(id) => SidebarTarget::Project(id),
        Scope::Favorites => SidebarTarget::Favorites,
        Scope::All => SidebarTarget::AllTasks,
        // A label filter is not a place in the tree; leave the highlight where it was.
        Scope::Label(_) => model.sidebar.selected,
    };
    let mut effects = reload_tasks(model);
    effects.push(Effect::RememberProject(match scope {
        Scope::Project(id) => Some(id),
        _ => None,
    }));
    effects
}

/// Move to the next or previous column layout.
fn switch_layout(model: &mut Model, delta: isize) -> Vec<Effect> {
    if model.layouts.len() < 2 {
        return Vec::new();
    }
    let count = model.layouts.len() as isize;
    model.layout_ix = ((model.layout_ix as isize + delta).rem_euclid(count)) as usize;
    // The layout owns the sort order, so switching layout re-queries rather than
    // re-sorting in place -- the store's idea of "dateless tasks last" is the only one.
    model.query.sort = crate::query::sort_for(model.layout());
    let layout = model.layout();
    let text = match &layout.description {
        Some(description) => format!("{}: {description}", layout.name),
        None => layout.name.clone(),
    };
    model.toast(Toast::info(text));
    reload_tasks(model)
}

/// Ask for the current query again under a fresh id.
fn reload_tasks(model: &mut Model) -> Vec<Effect> {
    model.query_id = model.query_id.next();
    model.data.loading = true;
    vec![Effect::LoadTasks {
        id: model.query_id,
        filter: model.query.filter(),
        sort: model.query.sort,
    }]
}

/// Everything the interface reads, for a first paint or after a sync.
pub fn reload_everything(model: &mut Model) -> Vec<Effect> {
    let mut effects = reload_tasks(model);
    effects.push(Effect::LoadProjects);
    effects.push(Effect::LoadLabels);
    effects.push(Effect::LoadCounts);
    effects
}

/// Why a pane the user just asked for did not appear.
///
/// A toggle that silently does nothing is indistinguishable from a broken keyboard. This
/// is the whole reason `Shown` no longer carries a width rule of its own: the layout is
/// the only thing that knows there is no room, so the layout is what gets asked, and the
/// user gets told.
///
/// And told something they can act on. The sidebar is laid out first, so on a middling
/// terminal it is usually the sidebar — not the width — standing between the user and a
/// preview, and "widen the terminal" would be true but useless advice.
fn no_room(pane: Pane, model: &Model) -> String {
    let (width, height) = model.size;
    let alone = crate::geometry::frames(width, height, false, true);
    if pane == Pane::Preview && alone.preview.is_some() {
        return format!("No room beside the sidebar at {width} columns — hide it with z s");
    }
    format!("No room for the {pane} at {width} columns — widen the terminal")
}

/// Which optional pane a message is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Sidebar,
    Preview,
}

impl std::fmt::Display for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Sidebar => "sidebar",
            Self::Preview => "preview",
        })
    }
}

/// Back out one level.
///
/// Esc is the key every terminal user reaches for to undo the last narrowing, and until
/// this existed it did nothing at all outside a modal — which reads as the interface
/// being stuck. The ladder is: a modal (handled before this is reached), then focus, then
/// the search filter. Nothing below that, because the next rung down would be quitting,
/// and Esc must never be the key that quits.
fn back(model: &mut Model) -> Vec<Effect> {
    if model.focus != Focus::List {
        model.focus = Focus::List;
        return Vec::new();
    }
    if model.query.search.take().is_some() {
        model.toast(Toast::info("Search cleared"));
        return reload_tasks(model);
    }
    Vec::new()
}

/// Move focus to the next visible pane.
fn cycle_focus(model: &mut Model, forward: bool) {
    let mut panes = vec![Focus::List];
    if model.sidebar_showing() {
        panes.insert(0, Focus::Sidebar);
    }
    if model.preview_showing() {
        panes.push(Focus::Preview);
    }
    let at = panes
        .iter()
        .position(|pane| *pane == model.focus)
        .unwrap_or(0);
    let count = panes.len();
    let next = if forward {
        (at + 1) % count
    } else {
        (at + count - 1) % count
    };
    model.focus = panes.get(next).copied().unwrap_or(Focus::List);
}

/// Move focus off a pane that is no longer showing.
fn settle_focus(model: &mut Model) {
    let stranded = match model.focus {
        Focus::Sidebar => !model.sidebar_showing(),
        Focus::Preview => !model.preview_showing(),
        Focus::List => false,
    };
    if stranded {
        model.focus = Focus::List;
    }
}

/// Scroll the list so the selection is on screen.
fn keep_selection_visible(model: &mut Model) {
    let rows = model.frames().list_rows().max(1);
    let Some(index) = model.selected_index() else {
        model.list.offset = 0;
        return;
    };
    if index < model.list.offset {
        model.list.offset = index;
    } else if index >= model.list.offset + rows {
        model.list.offset = index + 1 - rows;
    }
}
