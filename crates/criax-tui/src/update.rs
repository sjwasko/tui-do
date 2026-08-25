//! The one function that changes anything.
//!
//! `update` is pure and synchronous: it takes a message, mutates the model, and returns
//! the side effects it would like performed. It cannot await, cannot read a clock and
//! cannot touch a store — every one of those arrives as a [`Msg`] and leaves as an
//! [`Effect`]. That is what keeps the render loop from ever blocking.

use criax_core::models::{Label, Project, ProjectId, Task, TaskId};
use criax_core::quickadd;
use criax_core::store::Mutation;
use criax_core::sync::{Phase, Stage};
use criax_core::SyncEvent;

use crate::effect::Effect;
use crate::keymap::{resolve, Action, Key, Resolved, KEYMAP};
use crate::modal::{
    Candidate, HelpState, Modal, Outcome, Pick, PickerKind, PickerState, SearchState, Submission,
    TextInput,
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
        Msg::Reload => reload_everything(model),
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
        SyncEvent::Adopted {
            provisional,
            assigned,
        } => adopt(model, provisional, assigned),
        SyncEvent::Pushed(report) => {
            // A push can be the whole pass, so this is where "sending" ends. `last_sync`
            // is deliberately not stamped: nothing was fetched, and "synced just now"
            // would be a claim about the server's state that this pass never checked.
            model.status.sync = SyncStatus::Idle;
            model.status.queued = report.deferred;
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
        Submission::Add(text) => add_task(model, &text),
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
        Action::AddTask => {
            model.modals.push(Modal::Add(TextInput::default()));
            Vec::new()
        }
        Action::DeleteTask => match model.selected_task() {
            Some(task) => {
                let before = task.clone();
                // No confirmation, deliberately: a dialog asks the user to predict a
                // mistake, undo lets them recognise one. The caveat is real and goes in
                // the message -- Vikunja has no undelete, so undo re-creates the task
                // and it comes back with a new id.
                model.toast(Toast::warning(format!(
                    "Deleted \"{}\" — u to undo",
                    truncated(&before.title)
                )));
                edit(
                    model,
                    Mutation::DeleteTask {
                        before: Box::new(before),
                    },
                )
            }
            None => nothing_selected(model),
        },
        Action::ToggleDone => match model.selected_task() {
            Some(task) => {
                let mut after = task.clone();
                after.done = !after.done;
                // Vikunja stamps this itself, but the local row has to look right until
                // the pull confirms it. A *repeating* task is the exception: the server
                // advances its due date instead of marking it done, so the reload after
                // the push is what makes that case true rather than this line.
                after.done_at = if after.done {
                    Some(model.now).into()
                } else {
                    None.into()
                };
                let text = if after.done { "Done" } else { "Not done" };
                let mutation = Mutation::UpdateTask {
                    before: Box::new(task.clone()),
                    after: Box::new(after),
                };
                model.toast(Toast::info(text));
                edit(model, mutation)
            }
            None => nothing_selected(model),
        },
        Action::Undo => match model.undo.pop() {
            Some(mutation) => {
                model.redo.push(mutation.inverse());
                model.toast(Toast::info(undo_text(&mutation)));
                apply(model, mutation)
            }
            None => {
                model.toast(Toast::info("Nothing to undo"));
                Vec::new()
            }
        },
        Action::Redo => match model.redo.pop() {
            Some(mutation) => {
                model.undo.push(mutation.inverse());
                apply(model, mutation)
            }
            None => {
                model.toast(Toast::info("Nothing to redo"));
                Vec::new()
            }
        },
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

/// A task built from quick-add text, and what could not be honoured.
#[derive(Debug, Clone, PartialEq)]
pub struct QuickAdd {
    /// The task itself.
    pub task: Task,
    /// Labels named that do not exist. Creating one is its own mutation kind, which
    /// Phase 4 does not have, so they are reported rather than silently dropped.
    pub unknown_labels: Vec<String>,
    /// More than one project answers to the name that was used.
    ///
    /// Not hypothetical: the dev instance has three projects called `Inbox` — a
    /// pseudo-project, an empty one, and the real one — so `+Inbox` genuinely does not
    /// identify a project, and picking one silently puts tasks somewhere the user is not
    /// looking.
    pub ambiguous_project: bool,
}

/// Build a task from parsed quick-add text, if a project can be found for it.
///
/// Shared with `criax add`, so the same syntax means the same thing from the interface
/// and from a shell.
#[must_use]
pub fn quickadd_task(
    parsed: &quickadd::Parsed,
    projects: &[Project],
    labels: &[Label],
    showing: Option<ProjectId>,
) -> Option<QuickAdd> {
    let project = project_for(projects, parsed.project.as_deref(), showing)?;
    let (found, unknown_labels) = resolve_labels(labels, &parsed.labels);
    Some(QuickAdd {
        task: build_task(parsed, project, found),
        unknown_labels,
        ambiguous_project: parsed
            .project
            .as_deref()
            .is_some_and(|name| matches_by_name(projects, name).count() > 1),
    })
}

/// Real projects answering to `name`.
fn matches_by_name<'a>(
    projects: &'a [Project],
    name: &'a str,
) -> impl Iterator<Item = &'a Project> {
    projects.iter().filter(move |project| {
        project.id.get() > 0
            && !project.is_archived
            && project.title.eq_ignore_ascii_case(name.trim())
    })
}

/// The task itself, once the project and labels are settled.
fn build_task(parsed: &quickadd::Parsed, project: ProjectId, labels: Vec<Label>) -> Task {
    let mut task = Task {
        // Vikunja binds the path first and the body second, so a body that leaves this
        // at 0 makes the server look up project 0 and answer 404 about the project you
        // just named. Writing it here is the fix, and it belongs at the point the task
        // is built rather than in the client.
        project_id: project,
        title: parsed.title.clone(),
        priority: parsed.priority.map_or(0, i64::from),
        due_date: parsed.due_date.into(),
        start_date: parsed.start_date.into(),
        labels,
        ..Task::default()
    };
    if let Some(repeat) = parsed.repeat {
        task.repeat_after = repeat.seconds();
        if repeat.is_monthly() {
            task.repeat_mode = criax_core::models::RepeatMode::Monthly;
        }
    }
    task
}

/// Turn quick-add text into a task, and queue it.
///
/// The parser is `criax-core`'s, the same one `criax add` uses, so the syntax cannot mean
/// two things depending on where it was typed.
fn add_task(model: &mut Model, text: &str) -> Vec<Effect> {
    let parsed = quickadd::parse(text, &model.now);
    if parsed.title.trim().is_empty() {
        model.toast(Toast::info("Nothing to add"));
        return Vec::new();
    }

    let showing = match model.query.scope {
        Scope::Project(id) => Some(id),
        _ => None,
    };
    let Some(built) = quickadd_task(&parsed, &model.data.projects, &model.data.labels, showing)
    else {
        model.toast(Toast::error(match parsed.project.as_deref() {
            Some(name) => format!("No project called \"{name}\""),
            None => "No project to add to".to_string(),
        }));
        return Vec::new();
    };

    let title = built.task.title.clone();
    let project = model
        .project(built.task.project_id)
        .map_or_else(String::new, |project| project.title.clone());
    let mut effects = edit(
        model,
        Mutation::CreateTask {
            task: Box::new(built.task),
        },
    );

    let mut notes = Vec::new();
    if !built.unknown_labels.is_empty() {
        notes.push(format!(
            "no label called {}",
            built.unknown_labels.join(", ")
        ));
    }
    if built.ambiguous_project {
        notes.push(format!("more than one project is called {project}"));
    }
    model.toast(if notes.is_empty() {
        Toast::info(format!("Added \"{title}\""))
    } else {
        Toast::warning(format!("Added \"{title}\" — {}", notes.join("; ")))
    });
    effects.push(Effect::LoadCounts);
    effects
}

/// Which project a new task belongs to.
///
/// A named project wins; otherwise the one being shown; otherwise the Inbox, which is
/// where Vikunja itself puts a task with nowhere else to go.
fn project_for(
    projects: &[Project],
    named: Option<&str>,
    showing: Option<ProjectId>,
) -> Option<ProjectId> {
    let real = || {
        projects
            .iter()
            .filter(|project| project.id.get() > 0 && !project.is_archived)
    };
    if let Some(name) = named {
        return real()
            .find(|project| project.title.eq_ignore_ascii_case(name.trim()))
            .map(|project| project.id);
    }
    if let Some(id) = showing {
        return Some(id);
    }
    real()
        .find(|project| project.title.eq_ignore_ascii_case("inbox"))
        .or_else(|| real().next())
        .map(|project| project.id)
}

/// Match label names against the ones that exist, and report the ones that do not.
fn resolve_labels(known: &[Label], wanted: &[String]) -> (Vec<Label>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for name in wanted {
        match known
            .iter()
            .find(|label| label.title.eq_ignore_ascii_case(name.trim()))
        {
            Some(label) => found.push(label.clone()),
            None => missing.push(name.clone()),
        }
    }
    (found, missing)
}

/// Say why a task key did nothing.
///
/// It does nothing for a good reason -- an empty list, or a filter that matched none --
/// but a key press that produces no response at all reads as a broken binding.
fn nothing_selected(model: &mut Model) -> Vec<Effect> {
    model.toast(Toast::info("No task selected"));
    Vec::new()
}

/// A title short enough to sit in a message beside other words.
fn truncated(title: &str) -> String {
    crate::rows::truncate(title, 40)
}

/// Make a change: apply it, and remember how to take it back.
///
/// Every edit goes through here, so the undo stack cannot fall out of step with what was
/// done — and a new edit clears the redo stack, as everywhere else.
fn edit(model: &mut Model, mutation: Mutation) -> Vec<Effect> {
    model.undo.push(mutation.inverse());
    model.redo.clear();
    apply(model, mutation)
}

/// Swap a provisional task id for the one the server gave it.
///
/// The store has already done this to its own rows and to anything still queued. What is
/// left is everything the interface holds by id, and *all* of it has to move together:
///
/// * the row on screen, so the frame before the reload is not stale,
/// * the selection, which is tracked by id precisely so it survives a reload,
/// * the undo and redo stacks, whose mutations name the task they act on.
///
/// Missing any one of them sends the next edit to `/tasks/-14`, which the server answers
/// `404 This task does not exist` — the task it just created. The undo stack is the one
/// most easily forgotten: a create pushes a delete of the provisional id, so `u` right
/// after creating a task would ask the server to delete something it never had.
fn adopt(model: &mut Model, provisional: TaskId, assigned: TaskId) -> Vec<Effect> {
    if let Some(task) = find(&mut model.data.tasks, provisional) {
        task.id = assigned;
    }
    if model.list.selected == Some(provisional) {
        model.list.selected = Some(assigned);
    }
    for mutation in model.undo.iter_mut().chain(model.redo.iter_mut()) {
        mutation.retarget(provisional, assigned);
    }
    // The store now holds the server's own copy of the row -- its identifier, its index,
    // its created stamp -- which the optimistic one never had.
    reload_tasks(model)
}

/// Apply a mutation to the model's own copy, and ask for it to be stored and queued.
///
/// The snapshot is changed here rather than waited for: the next frame already shows the
/// tick. `Store::queue` does the durable half in one transaction, and the reload that
/// follows is confirmation, not the mechanism.
fn apply(model: &mut Model, mutation: Mutation) -> Vec<Effect> {
    apply_locally(model, &mutation);
    vec![Effect::Apply(mutation)]
}

/// The optimistic edit, against the list the user is looking at.
fn apply_locally(model: &mut Model, mutation: &Mutation) {
    let tasks = &mut model.data.tasks;
    match mutation {
        Mutation::CreateTask { task } => {
            // At the top, and selected, so it is visible the instant it exists. The
            // reload puts it in its sorted place and the selection follows it there,
            // which is what tracking the selection by id is for.
            tasks.insert(0, (**task).clone());
            model.list.selected = Some(task.id);
            model.list.offset = 0;
        }
        Mutation::UpdateTask { after, .. } => {
            if let Some(existing) = tasks.iter_mut().find(|task| task.id == after.id) {
                *existing = (**after).clone();
            }
        }
        Mutation::DeleteTask { before } => {
            if let Some(at) = tasks.iter().position(|task| task.id == before.id) {
                tasks.remove(at);
                // The row that slid up into the gap, or the one above if it was last.
                let next = tasks.get(at).or_else(|| tasks.get(at.saturating_sub(1)));
                model.list.selected = next.map(|task| task.id);
            }
        }
        Mutation::AttachLabel { task, label } => {
            if let Some(existing) = find(tasks, *task) {
                if !existing.labels.iter().any(|held| held.id == label.id) {
                    existing.labels.push((**label).clone());
                }
            }
        }
        Mutation::DetachLabel { task, label } => {
            if let Some(existing) = find(tasks, *task) {
                existing.labels.retain(|held| held.id != label.id);
            }
        }
    }
    keep_selection_visible(model);
}

fn find(tasks: &mut [Task], id: criax_core::models::TaskId) -> Option<&mut Task> {
    tasks.iter_mut().find(|task| task.id == id)
}

/// What an undo just did, in words the user can check against the screen.
fn undo_text(mutation: &Mutation) -> String {
    match mutation {
        Mutation::CreateTask { task } => format!("Undone — restored \"{}\"", task.title),
        Mutation::DeleteTask { before } => format!("Undone — removed \"{}\"", before.title),
        Mutation::UpdateTask { after, .. } => format!("Undone — \"{}\"", after.title),
        Mutation::AttachLabel { label, .. } => format!("Undone — added {}", label.title),
        Mutation::DetachLabel { label, .. } => format!("Undone — removed {}", label.title),
    }
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
