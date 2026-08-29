//! The one function that changes anything.
//!
//! `update` is pure and synchronous: it takes a message, mutates the model, and returns
//! the side effects it would like performed. It cannot await, cannot read a clock and
//! cannot touch a store — every one of those arrives as a [`Msg`] and leaves as an
//! [`Effect`]. That is what keeps the render loop from ever blocking.

use chrono::{DateTime, FixedOffset, Utc};
use tui_do_core::config::{QuickAction, QuickActionKind};
use tui_do_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use tui_do_core::quickadd;
use tui_do_core::store::Mutation;
use tui_do_core::sync::{Phase, Stage};
use tui_do_core::SyncEvent;

use crate::effect::Effect;
use crate::geometry;
use crate::keymap::{resolve, Action, Key, Resolved, KEYMAP};
use crate::modal::{
    Candidate, DueState, EditDraft, EditState, HelpState, LabelsState, Modal, Outcome, Pick,
    PickerKind, PickerState, PriorityState, QuickActionsState, SearchState, Submission, TextInput,
};
use crate::model::{Focus, Model, SyncStatus, Toast};
use crate::msg::Msg;
use crate::query::{Scope, TASK_LIMIT};
use crate::rows;
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
            keep_sidebar_visible(model);
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
            keep_sidebar_visible(model);
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
        Msg::PendingLoaded(health) => {
            model.status.queued = health.queued;
            model.status.failing = health.failing;
            model.status.queue_error = health.last_error;
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
        SyncEvent::Overwrote { fields, .. } => {
            // The write went through -- the user's value is what the server holds now --
            // so there is nothing to roll back and nothing to reload. What is owed is the
            // news, because on a fleet the person who loses the edit is on another box
            // and will never otherwise know why their change evaporated.
            let what = fields.join(", ");
            model.toast(Toast::warning(format!(
                "saved over a change made elsewhere ({what})"
            )));
            Vec::new()
        }
        SyncEvent::Rejected {
            subject,
            kind,
            message,
        } => {
            // The one event the user must see: their edit has just vanished from the
            // screen and they are owed an explanation.
            model.toast(Toast::error(format!("{kind} rejected: {message}")));
            // An inverse of a change that never happened is not an undo of anything. The
            // store has rolled the row back, so replaying the inverse would queue a write
            // derived from a state the server never held -- `u` after a rejected create
            // asks it to delete an id it has never seen. Drop what named this task from
            // both stacks and leave the rest of the session's history intact.
            model.undo.retain(|mutation| mutation.subject() != subject);
            model.redo.retain(|mutation| mutation.subject() != subject);
            // And it has to actually vanish. The store rolled the row back before this
            // event was emitted, but the screen is drawn from the last query's answer, so
            // without a reload the toast says "rejected" over a row still showing the
            // change. Nothing else reloads in time either: an edit reaches the server via
            // a standalone push, which ends at `Pushed` -- and that returns only
            // `LoadPending`. The next full pass is five minutes away by default, so the
            // contradiction sits on screen until the user presses `r`.
            //
            // The counts go too: a rejected done-toggle changes how many tasks a project
            // is showing as open.
            let mut effects = reload_tasks(model);
            effects.push(Effect::LoadCounts);
            effects.push(Effect::LoadPending);
            effects
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
            // The report is what this pass deferred; the store is what is actually
            // queued. They differ whenever something else shares the outbox -- `tui-do
            // add` in another terminal, or a second interface -- so the report paints
            // immediately and the store corrects it.
            model.status.queued = report.deferred;
            vec![Effect::LoadPending]
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
        Submission::Edited(draft) => apply_edit(model, *draft),
        Submission::Priority(value) => set_priority(model, value),
        Submission::Due(text) => set_due(model, &text),
        Submission::Labels(chosen) => set_labels(model, &chosen),
        Submission::QuickAction(index) => run_quick_action(model, index),
        Submission::Picked(Pick::Project(id)) => show(model, Scope::Project(id)),
        Submission::Picked(Pick::Label(id)) => show(model, Scope::Label(id)),
        Submission::Picked(Pick::MoveTo(id)) => move_task(model, id),
        // A command chosen by name does exactly what its key does. One implementation,
        // so the two can never disagree about what "toggle the sidebar" means.
        Submission::Picked(Pick::Command(action)) => act(model, action),
    }
}

/// Perform an action.
fn act(model: &mut Model, action: Action) -> Vec<Effect> {
    // How many tasks are actually on screen, not how many lines there are. With wrapped
    // rows those differ by a factor of three, and a page motion that used lines jumped
    // over two screens of tasks for every one it showed.
    let page = rows::fit(
        &model.data.tasks,
        &rows::measure(model.layout(), model.frames().list.width),
        row_context(model),
        model.list.offset,
        body_height(model),
    ) as isize;
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
        Action::EditTask => match model.selected_task() {
            Some(task) => {
                // The project's name, not its id: a form showing `12` is a form nobody
                // can edit, and the modal cannot reach the model to look it up itself.
                let project = model
                    .project(task.project_id)
                    .map_or_else(String::new, |project| project.title.clone());
                model
                    .modals
                    .push(Modal::Edit(Box::new(EditState::new(task, &project))));
                Vec::new()
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
                    Some(model.now.with_timezone(&Utc)).into()
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
        Action::SetPriority => match model.selected_task() {
            Some(task) => {
                model
                    .modals
                    .push(Modal::Priority(PriorityState::new(task.priority)));
                Vec::new()
            }
            None => nothing_selected(model),
        },
        Action::SetDue => match model.selected_task() {
            Some(task) => {
                let current = task
                    .due_date
                    .get()
                    .map(|due| due.format("%d/%m/%Y").to_string())
                    .unwrap_or_default();
                model.modals.push(Modal::Due(DueState::new(&current)));
                Vec::new()
            }
            None => nothing_selected(model),
        },
        Action::MoveTask => match model.selected_task() {
            Some(task) => {
                // Pseudo-projects reject writes, so the ones the server invents -- `-1`
                // Favorites and the rest -- must not be offered as somewhere to move to.
                let current = task.project_id;
                let candidates = model
                    .data
                    .projects
                    .iter()
                    .filter(|project| project.id.get() > 0 && !project.is_archived)
                    .map(|project| {
                        Candidate::hinted(
                            Pick::MoveTo(project.id),
                            &project.title,
                            if project.id == current { "current" } else { "" },
                        )
                    })
                    .collect();
                model.modals.push(Modal::Picker(PickerState::new(
                    PickerKind::MoveProject,
                    sorted(candidates),
                )));
                Vec::new()
            }
            None => nothing_selected(model),
        },
        Action::SetLabels => match model.selected_task() {
            Some(task) => {
                let held: Vec<LabelId> = task.labels.iter().map(|label| label.id).collect();
                // Every label that exists, *plus* any the task carries that the labels
                // table has not caught up with. A pull stores tasks and labels in
                // separate passes, so a task can hold one this list has never seen --
                // and a form that could not show it could not take it off either.
                let mut labels = model.data.labels.clone();
                for label in &task.labels {
                    if !labels.iter().any(|known| known.id == label.id) {
                        labels.push(label.clone());
                    }
                }
                if labels.is_empty() {
                    // tui-do cannot create labels yet, so an empty form would be a box
                    // with nothing in it and no way to fill it.
                    model.toast(Toast::info("No labels exist yet"));
                    return Vec::new();
                }
                model
                    .modals
                    .push(Modal::Labels(LabelsState::new(labels, held)));
                Vec::new()
            }
            None => nothing_selected(model),
        },
        Action::QuickAction => {
            if model.selected_task().is_none() {
                return nothing_selected(model);
            }
            if model.quick_actions.is_empty() {
                // Named as configuration rather than as a missing feature: the key works,
                // there is simply nothing bound under it yet.
                model.toast(Toast::info(
                    "No quick actions configured — see quick_actions in the config",
                ));
                return Vec::new();
            }
            let rows = model
                .quick_actions
                .iter()
                .map(|action| (action.key, quick_action_doc(&action.kind)))
                .collect();
            model
                .modals
                .push(Modal::QuickActions(QuickActionsState::new(rows)));
            Vec::new()
        }
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
            // Read the store first, and only then ask the server. Everything `tui-do add`
            // wrote is already local, so it appears on this frame rather than after the
            // pull — which is seventy-eight sequential pages, better than half a minute,
            // and no part of it is needed to show a task the store already has.
            //
            // The pull still runs, and its own reload lands when it finishes. This is
            // the local-first claim applied to the refresh key itself: the cache answers
            // now, the server confirms later.
            // Named at the keystroke, because the difference between the two keys is
            // invisible from the outside until it bites: `r` cannot see a deletion made
            // in another client, and someone watching for one to disappear has no other
            // way to learn that `R` is the key that would.
            model.toast(Toast::info("Syncing changes — R for everything"));
            let mut effects = reload_everything(model);
            effects.push(Effect::SyncNow);
            effects
        }
        Action::SyncFull => {
            model.status.sync = SyncStatus::Working {
                detail: "starting".to_string(),
            };
            // The slow one, asked for deliberately, so it says so: `r` is a second and
            // `R` is fifteen, and a key that looks identical to the fast one while
            // taking fifteen times as long reads as a hang.
            model.toast(Toast::info("Fetching everything"));
            let mut effects = reload_everything(model);
            effects.push(Effect::SyncFull);
            effects
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
            // Expanding lengthens the tree under the cursor; collapsing shortens it. Both
            // move every row below, so both have to settle the scroll.
            keep_sidebar_visible(model);
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
        keep_sidebar_visible(model);
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
    keep_sidebar_visible(model);
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
    effects.push(Effect::LoadPending);
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
/// Shared with `tui-do add`, so the same syntax means the same thing from the interface
/// and from a shell.
#[must_use]
pub fn quickadd_task(
    parsed: &quickadd::Parsed,
    projects: &[Project],
    labels: &[Label],
    showing: Option<ProjectId>,
    configured: Option<&str>,
) -> Option<QuickAdd> {
    let project = project_for(projects, parsed.project.as_deref(), showing, configured)?;
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
            task.repeat_mode = tui_do_core::models::RepeatMode::Monthly;
        }
    }
    task
}

/// Turn a filled-in edit form into mutations.
///
/// The form hands over text and nothing else. What a project name, a label list or a date
/// means depends on what the model knows, so it is decided here -- against the same
/// helpers the quick-add prompt uses, so `+Legal` in a new task and `Legal` in this form
/// cannot come to different conclusions.
///
/// The task write and the label changes are separate mutations because the server treats
/// them separately: labels are attached and detached through their own endpoints and a
/// task write ignores the body's `labels`. That means a form that changed both is two or
/// three entries in the undo stack rather than one, which is honest about what was sent.
/// Change the selected task through `change`, and toast `told` when it changed anything.
///
/// Every quick key ends here rather than building its own mutation: the `before`/`after`
/// pair, the "nothing selected" answer and the "that is what it already said" answer are
/// the same three cases each time, and writing them five times is five chances to write
/// one of them differently.
fn change_selected(
    model: &mut Model,
    told: impl FnOnce(&Task) -> Toast,
    change: impl FnOnce(&mut Task),
) -> Vec<Effect> {
    let Some(before) = model.selected_task().cloned() else {
        return nothing_selected(model);
    };
    let mut after = before.clone();
    change(&mut after);
    if after == before {
        model.toast(Toast::info("Nothing changed"));
        return Vec::new();
    }
    model.toast(told(&after));
    edit(
        model,
        Mutation::UpdateTask {
            before: Box::new(before),
            after: Box::new(after),
        },
    )
}

/// Set the selected task's priority.
fn set_priority(model: &mut Model, value: i64) -> Vec<Effect> {
    change_selected(
        model,
        |task| {
            Toast::info(if task.priority == 0 {
                "Priority cleared".to_string()
            } else {
                format!(
                    "Priority {} — {}",
                    task.priority,
                    rows::priority_name(task.priority)
                )
            })
        },
        |task| task.priority = value,
    )
}

/// Move the selected task into `project`.
fn move_task(model: &mut Model, project: ProjectId) -> Vec<Effect> {
    let name = model.project(project).map_or_else(
        || format!("#{}", project.get()),
        |project| project.title.clone(),
    );
    change_selected(
        model,
        |_| Toast::info(format!("Moved to {name}")),
        |task| task.project_id = project,
    )
}

/// Set the selected task's due date from `text`, which is empty to clear it.
fn set_due(model: &mut Model, text: &str) -> Vec<Effect> {
    let text = text.trim();
    // Through the quick-add parser, not a date format, so the field understands
    // `tomorrow` and `next friday` -- exactly what the edit form's Due field does.
    let parsed = if text.is_empty() {
        None
    } else {
        match quickadd::parse(text, &model.now).due_date {
            Some(due) => Some(due),
            None => {
                model.toast(Toast::error(format!(
                    "{text:?} is not a date tui-do understands"
                )));
                return Vec::new();
            }
        }
    };
    let now = model.now;
    change_selected(
        model,
        move |task| match task.due_date.get() {
            // A date in the past is allowed -- overdue is a real state, and backdating a
            // task you have been carrying is a real thing to want. It is *loud*, though:
            // `2024` where `2026` was meant is a typo the toast would otherwise confirm
            // as though it were what the user asked for.
            Some(due) if due < now => Toast::warning(format!(
                "Due {} — that date has passed",
                rows::relative_date(Some(due), now)
            )),
            Some(due) => Toast::info(format!("Due {}", rows::relative_date(Some(due), now))),
            None => Toast::info("Due date cleared".to_string()),
        },
        |task| task.due_date = parsed.into(),
    )
}

/// Make `chosen`, exactly `chosen`, the selected task's labels.
fn set_labels(model: &mut Model, chosen: &[LabelId]) -> Vec<Effect> {
    let Some(task) = model.selected_task().cloned() else {
        return nothing_selected(model);
    };
    // Labels do not travel in the task body -- they are attached and detached through
    // their own endpoints -- so this is a set difference against what the task holds,
    // never an `UpdateTask` carrying a new list.
    let attach: Vec<Label> = model
        .data
        .labels
        .iter()
        .filter(|label| {
            chosen.contains(&label.id) && !task.labels.iter().any(|held| held.id == label.id)
        })
        .cloned()
        .collect();
    let detach: Vec<Label> = task
        .labels
        .iter()
        .filter(|held| !chosen.contains(&held.id))
        .cloned()
        .collect();

    if attach.is_empty() && detach.is_empty() {
        model.toast(Toast::info("Nothing changed"));
        return Vec::new();
    }
    let told = match (attach.len(), detach.len()) {
        (added, 0) => format!("{added} label{} added", plural(added)),
        (0, removed) => format!("{removed} label{} removed", plural(removed)),
        (added, removed) => format!("{added} added, {removed} removed"),
    };
    model.toast(Toast::info(told));

    let mut effects = Vec::new();
    for label in attach {
        effects.extend(edit(
            model,
            Mutation::AttachLabel {
                task: task.id,
                label: Box::new(label),
            },
        ));
    }
    for label in detach {
        effects.extend(edit(
            model,
            Mutation::DetachLabel {
                task: task.id,
                label: Box::new(label),
            },
        ));
    }
    effects
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

/// Attach `label` to the selected task, or detach it if it is already on.
fn toggle_label(model: &mut Model, label: &Label) -> Vec<Effect> {
    let Some(task) = model.selected_task().cloned() else {
        return nothing_selected(model);
    };
    let held = task.labels.iter().any(|on| on.id == label.id);
    let mutation = if held {
        model.toast(Toast::info(format!("Removed *{}", label.title)));
        Mutation::DetachLabel {
            task: task.id,
            label: Box::new(label.clone()),
        }
    } else {
        model.toast(Toast::info(format!("Added *{}", label.title)));
        Mutation::AttachLabel {
            task: task.id,
            label: Box::new(label.clone()),
        }
    };
    edit(model, mutation)
}

/// What the quick-action menu prints beside a key.
fn quick_action_doc(kind: &QuickActionKind) -> String {
    match kind {
        QuickActionKind::Project(name) => format!("Move to {name}"),
        QuickActionKind::Priority(value) => {
            format!(
                "Priority {value} — {}",
                rows::priority_name(i64::from(*value))
            )
        }
        QuickActionKind::Label(name) => format!("Toggle *{name}"),
    }
}

/// Run the configured quick action at `index`.
///
/// Resolving happens here rather than when the config is read, because a project or a
/// label named in the config may not exist -- and the honest moment to say so is when the
/// key is pressed, not at startup where it would be one more line nobody reads.
fn run_quick_action(model: &mut Model, index: usize) -> Vec<Effect> {
    let Some(QuickAction { kind, .. }) = model.quick_actions.get(index).cloned() else {
        return Vec::new();
    };
    match kind {
        QuickActionKind::Priority(value) => set_priority(model, i64::from(value)),
        QuickActionKind::Project(name) => match resolve_project(&model.data.projects, &name) {
            Some(id) => move_task(model, id),
            None => {
                model.toast(Toast::error(format!("No project called {name:?}")));
                Vec::new()
            }
        },
        QuickActionKind::Label(name) => {
            match model
                .data
                .labels
                .iter()
                .find(|label| label.title.eq_ignore_ascii_case(name.trim()))
                .cloned()
            {
                Some(label) => toggle_label(model, &label),
                None => {
                    model.toast(Toast::error(format!("No label called {name:?}")));
                    Vec::new()
                }
            }
        }
    }
}

fn apply_edit(model: &mut Model, draft: EditDraft) -> Vec<Effect> {
    let mut notes: Vec<String> = Vec::new();
    let before = *draft.before;
    let mut after = before.clone();

    after.title = draft.title.trim().to_string();
    if after.title.is_empty() {
        model.toast(Toast::error("A task needs a title"));
        return Vec::new();
    }
    after.description = draft.description;

    match draft.priority.as_str() {
        "" => after.priority = 0,
        text => match text.parse::<i64>() {
            Ok(value) if (0..=5).contains(&value) => after.priority = value,
            _ => notes.push(format!("{text:?} is not a priority between 0 and 5")),
        },
    }

    // An empty field clears the date. Anything else goes through the quick-add parser, so
    // the field understands `tomorrow` and `next friday` and not just `24/12/2026`.
    if draft.due.is_empty() {
        after.due_date = None.into();
    } else {
        let parsed = quickadd::parse(&draft.due, &model.now);
        match parsed.due_date {
            Some(due) => after.due_date = Some(due).into(),
            None => notes.push(format!("{:?} is not a date tui-do understands", draft.due)),
        }
    }

    // A project name is a label, not a key -- two of them can read "Inbox". Resolving an
    // untouched field by name would answer with whichever one sorts first and move the
    // task there, out of the list the user was looking at, without saying so. So the
    // field only resolves when it was actually retyped; otherwise the id stands.
    if draft.project.eq_ignore_ascii_case(&draft.project_was) {
        // Untouched, so there is nothing to resolve and nothing to say.
    } else if draft.project.is_empty() {
        notes.push("A task needs a project; kept the one it was in".to_string());
    } else {
        match project_for(&model.data.projects, Some(&draft.project), None, None) {
            Some(id) => after.project_id = id,
            None => notes.push(format!("No project called {:?}", draft.project)),
        }
    }

    let wanted: Vec<String> = draft
        .labels
        .split(',')
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    let (resolved, unknown) = resolve_labels(&model.data.labels, &wanted);
    if !unknown.is_empty() {
        notes.push(format!(
            "No label called {} -- tui-do cannot create labels yet",
            unknown.join(", ")
        ));
    }

    let mut effects = Vec::new();
    // Labels do not travel in the task body, so they are compared against `before` and
    // sent on their own. `after` keeps them only so the row on screen looks right.
    let attach: Vec<_> = resolved
        .iter()
        .filter(|label| !before.labels.iter().any(|held| held.id == label.id))
        .cloned()
        .collect();
    let detach: Vec<_> = before
        .labels
        .iter()
        .filter(|held| !resolved.iter().any(|label| label.id == held.id))
        .cloned()
        .collect();
    after.labels = resolved;

    // Read before `after` is moved into the mutation. Only when the form *moved* the
    // date: a task that was already overdue and had some other field edited is not
    // something the user just backdated, and warning on every save would train them to
    // ignore the warning that matters.
    // By day, not by instant: the field shows a date and the parser gives it a time, so
    // re-saving an untouched form moves 09:00 to 23:59 and an instant comparison would
    // call that a backdate on every save of anything overdue.
    let day = |task: &Task| task.due_date.get().map(|due| due.date_naive());
    let backdated = (day(&after) != day(&before))
        .then(|| past_due_note(after.due_date.get(), model.now))
        .flatten();

    if after != before {
        effects.extend(edit(
            model,
            Mutation::UpdateTask {
                before: Box::new(before.clone()),
                after: Box::new(after),
            },
        ));
    }
    for label in attach {
        effects.extend(edit(
            model,
            Mutation::AttachLabel {
                task: before.id,
                label: Box::new(label),
            },
        ));
    }
    for label in detach {
        effects.extend(edit(
            model,
            Mutation::DetachLabel {
                task: before.id,
                label: Box::new(label),
            },
        ));
    }

    if effects.is_empty() {
        model.toast(Toast::info("Nothing changed"));
    } else if notes.is_empty() {
        model.toast(match backdated {
            Some(note) => Toast::warning(format!("Saved — {note}")),
            None => Toast::info("Saved"),
        });
    }
    if !notes.is_empty() {
        model.toast(Toast::error(notes.join(" · ")));
    }
    effects
}

/// Turn quick-add text into a task, and queue it.
///
/// The parser is `tui-do-core`'s, the same one `tui-do add` uses, so the syntax cannot mean
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
    let Some(built) = quickadd_task(
        &parsed,
        &model.data.projects,
        &model.data.labels,
        showing,
        model.default_project.as_deref(),
    ) else {
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
    // Read before the task is moved into the mutation. A bare date in quick-add is the
    // easiest of the three ways to set one by accident, because nothing was aimed at a
    // date field -- the text simply had a date in it.
    let backdated = past_due_note(built.task.due_date.get(), model.now);
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
    notes.extend(backdated);
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
    configured: Option<&str>,
) -> Option<ProjectId> {
    if let Some(name) = named {
        return resolve_project(projects, name);
    }
    if let Some(id) = showing {
        return Some(id);
    }
    // Only once nothing else has said where: what the user is looking at outranks what
    // they configured, or `a` inside a project would file the task somewhere else.
    if let Some(id) = configured.and_then(|spec| resolve_project(projects, spec)) {
        return Some(id);
    }
    let real = || {
        projects
            .iter()
            .filter(|project| project.id.get() > 0 && !project.is_archived)
    };
    real()
        .find(|project| project.title.eq_ignore_ascii_case("inbox"))
        .or_else(|| real().next())
        .map(|project| project.id)
}

/// Find the project a written reference means.
///
/// `#12` names one by id and cannot be ambiguous; anything else is a title, and a title
/// can be worn by more than one project -- Vikunja does not require them to be unique,
/// and this account has two called `Inbox`. `#12` is the way to say which, and it works
/// wherever a project can be written: `tui-do add +#12`, the edit form's project field,
/// and `view.default_project` in the config.
///
/// A reference that looks like an id but names no project falls through to the title
/// match rather than failing, so a project genuinely called `#12` is still reachable.
fn resolve_project(projects: &[Project], spec: &str) -> Option<ProjectId> {
    let spec = spec.trim();
    let real = || {
        projects
            .iter()
            .filter(|project| project.id.get() > 0 && !project.is_archived)
    };
    if let Some(id) = spec.strip_prefix('#').and_then(|n| n.parse::<i64>().ok()) {
        if let Some(project) = real().find(|project| project.id.get() == id) {
            return Some(project.id);
        }
    }
    real()
        .find(|project| project.title.eq_ignore_ascii_case(spec))
        .map(|project| project.id)
}

/// What a date already in the past earns saying, if it is one.
///
/// A past due date is allowed — overdue is a real state, and backdating something you
/// have been carrying is a real thing to want — but it is never confirmed quietly,
/// because `2024` typed where `2026` was meant is indistinguishable from a deliberate
/// backdate the moment it is stored.
///
/// Here rather than at each call site because there are three ways to set a due date —
/// `D`, the edit form and quick-add — and only `D` said it. The other two answered
/// "Saved" and "Added", which is the quiet confirmation this rule exists to prevent.
pub fn past_due_note(due: Option<DateTime<Utc>>, now: DateTime<FixedOffset>) -> Option<String> {
    let due = due.filter(|due| *due < now)?;
    // The date in parentheses rather than leading, because this is appended to a message
    // that already has a subject: "Saved — that date has passed (due Aug 27, 24)". `D`
    // keeps its own phrasing, where the date *is* the subject and leads.
    Some(format!(
        "that date has passed (due {})",
        rows::relative_date(Some(due), now)
    ))
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
    // Asked for rather than incremented: `Effect::Apply` is what writes the outbox row,
    // and counting ahead of it would show a number the store does not agree with if the
    // write fails.
    vec![Effect::Apply(mutation), Effect::LoadPending]
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
            if let Some(at) = tasks.iter().position(|task| task.id == after.id) {
                // A task marked done leaves a list that is not showing done tasks, and it
                // leaves it *here* rather than on the reload that follows. The reload
                // cannot tell "the selected row left this list" from "this is a different
                // list", so it falls back to the first row -- which sent the cursor to the
                // top of the list on every `d`, where `x` holds its place. Removing it
                // here means the selection moves down one, exactly as a delete does.
                if after.done && !model.query.include_done {
                    tasks.remove(at);
                    let next = tasks.get(at).or_else(|| tasks.get(at.saturating_sub(1)));
                    model.list.selected = next.map(|task| task.id);
                } else {
                    tasks[at] = (**after).clone();
                }
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

fn find(tasks: &mut [Task], id: tui_do_core::models::TaskId) -> Option<&mut Task> {
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
/// Scroll the sidebar so the selected row is on screen.
///
/// *This was broken:* the sidebar carried an `offset` from the first day and nothing ever
/// wrote to it, so a tree taller than the pane stopped dead at the bottom edge and the
/// selection walked on into rows nobody could see. The task list had the same fault and
/// was fixed; this is the same fix, and it is simpler because sidebar rows are one line
/// each.
fn keep_sidebar_visible(model: &mut Model) {
    let Some(area) = model.frames().sidebar else {
        return;
    };
    let rows = sidebar::rows(&model.data.projects, &model.data.counts, &model.sidebar);
    // One line per row, unlike the task list, where a row wraps to as many as three and
    // the offset has to be measured rather than counted.
    let height = usize::from(area.height).max(1);
    if let Some(index) = rows
        .iter()
        .position(|row| row.target() == Some(model.sidebar.selected))
    {
        if index < model.sidebar.offset {
            model.sidebar.offset = index;
        } else if index >= model.sidebar.offset + height {
            model.sidebar.offset = index + 1 - height;
        }
    }
    // Unconditionally, and not as an `else`: a tree that shrank -- a collapse, an
    // archive, a pull that dropped a project -- leaves the offset past the end, and a
    // selection that vanished along with it is precisely the case where the branch above
    // has nothing to correct it by. Skipping this was drawing an empty pane over a list
    // that was still there.
    model.sidebar.offset = model.sidebar.offset.min(rows.len().saturating_sub(height));
}

fn keep_selection_visible(model: &mut Model) {
    let Some(index) = model.selected_index() else {
        model.list.offset = 0;
        return;
    };
    if index < model.list.offset {
        model.list.offset = index;
        return;
    }
    // Counting rows as one line each is what broke this: a body with room for eighteen
    // lines holds seven wrapped tasks, so the selection walked off the bottom of a list
    // that had decided it was already showing everything.
    let first = rows::first_visible(
        &model.data.tasks,
        &rows::measure(model.layout(), model.frames().list.width),
        row_context(model),
        index,
        body_height(model),
    );
    if model.list.offset < first {
        model.list.offset = first;
    }
}

/// The lines available to task rows, once the column headings have taken theirs.
fn body_height(model: &Model) -> u16 {
    model
        .frames()
        .list
        .height
        .saturating_sub(geometry::LIST_HEADING_HEIGHT)
}

/// What the renderer would measure rows with.
fn row_context(model: &Model) -> rows::RowContext<'_> {
    rows::RowContext {
        projects: &model.data.projects,
        theme: model.theme,
        now: model.now,
    }
}
