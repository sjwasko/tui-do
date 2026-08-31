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
use tui_do_core::store::{Mutation, Subject};
use tui_do_core::sync::{Phase, Stage};
use tui_do_core::SyncEvent;

use crate::effect::Effect;
use crate::geometry;
use crate::keymap::{resolve, Action, Key, Resolved, KEYMAP};
use crate::modal::{
    Candidate, ConfirmLabelsState, DueState, EditDraft, EditState, HelpState, LabelEditState,
    LabelsState, Modal, Outcome, Pending, Pick, PickerKind, PickerState, PriorityState,
    QuickActionsState, SearchState, Submission, TextInput,
};
use crate::model::{Focus, Model, SyncStatus, Toast, UrlAction};
use crate::msg::Msg;
use crate::query::{Scope, TASK_LIMIT};
use crate::rows;
use crate::sidebar::{self, SidebarTarget};
use crate::urls;

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
        Msg::LabelsLoaded(labels) => absorb_labels(model, labels),
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
        Msg::EffectFailed(message) => {
            model.toast(Toast::error(message));
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
/// A write the server refused: explain it, forget its undo history, and reload.
///
/// Hoisted out of `on_sync`'s `SyncEvent::Rejected` arm, which was roughly 60 of that
/// function's 134 lines and did four separate jobs while every other arm did one. The body
/// is unchanged; the reasoning in the comments is the record of why each job is here.
fn on_rejected(model: &mut Model, subject: Subject, kind: &str, message: &str) -> Vec<Effect> {
    // Cleared before the release below, which toasts. `Model::toast` is a single
    // slot, so what is standing afterwards can only be the release's own note --
    // where without this an unrelated toast left over from a moment ago would be
    // folded into the rejection as though it were about it. Nothing is lost: the
    // message below is set unconditionally.
    model.status.toast = None;
    let released = release_held_labels(model, subject, kind);
    let aside = model.status.toast.take().map(|toast| toast.text);
    // The one event the user must see: their edit has just vanished from the
    // screen and they are owed an explanation.
    //
    // Combined rather than sequential, because the slot is one deep and the two
    // messages are about one event. The rejection has to win -- it is the half
    // the user cannot work out by looking -- but a released submission that went
    // out without its label would then be written and overwritten inside the same
    // update, leaving the task row itself as the only signal it happened.
    model.toast(Toast::error(match aside {
        Some(note) => format!("{kind} rejected: {message} — {note}"),
        None => format!("{kind} rejected: {message}"),
    }));
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
    //
    // The labels go too, and that became reachable when the `l` form learned to
    // create one. The store rolls a rejected `CreateLabel` back, but
    // `model.data.labels` and any open label form still hold the provisional --
    // ticked, because creating is what ticked it -- so the user's next Enter
    // queues an `AttachLabel` for an id the server has never had, which fails in
    // turn. A rejected `UpdateLabel` is the same story with a rename: the store
    // has the old title and both snapshots show the new one.
    //
    // The released work leads, and the reload follows it. A resumed `CreateTask`
    // and the task reload are two effects the runtime spawns on two tasks of its
    // own, so a reload that read the store first would answer without the row the
    // user has just watched appear and blank it until something reloaded again.
    // Spawn order is a head start rather than a guarantee -- `Effect::Apply`
    // sends `Msg::Reload` once its write lands, which is what actually closes the
    // window -- but the wrong order here loses that race every time and this one
    // wins it nearly always.
    let mut effects = released;
    effects.extend(reload_tasks(model));
    effects.push(Effect::LoadCounts);
    effects.push(Effect::LoadLabels);
    effects.push(Effect::LoadPending);
    effects
}

/// Take a fresh label list, and tell every modal holding a label about it.
///
/// Hoisted out of `update`'s `Msg::LabelsLoaded` arm, which was 78 of that function's 127
/// lines and the only thing stopping it reading as a flat dispatch. The body is unchanged.
fn absorb_labels(model: &mut Model, labels: Vec<Label>) -> Vec<Effect> {
    model.data.labels = labels;
    // The one place a label form ever learns the id of a label it asked for.
    // `Store::queue` allocates the provisional id inside its own transaction,
    // so the reload it triggers is the only route back — and the form is on
    // screen at exactly this moment, because that is where the label was made.
    // A form left without it shows no tick, so the user's next Enter queues
    // nothing at all for the label they just created.
    //
    // And the route back for a title, which a rename made necessary: both lists
    // hold their own clones, `apply_locally` writes the optimistic new title
    // into them, and the store is what decides whether that title survives. A
    // rejected `UpdateLabel` rolls the row back and reloads *this* -- so without
    // the refresh below the pool says `urgent` while the form the user is still
    // looking at says `critical`, which is the rename they were just told failed.
    // Another box's rename arrives the same way.
    for modal in &mut model.modals {
        match modal {
            Modal::Labels(state) => {
                state.absorb_created(&model.data.labels);
                state.refresh(&model.data.labels);
            }
            Modal::Picker(state) => {
                for candidate in &mut state.candidates {
                    let Pick::Label(id) = candidate.pick else {
                        continue;
                    };
                    if let Some(known) = model.data.labels.iter().find(|label| label.id == id) {
                        candidate.title.clone_from(&known.title);
                    }
                }
            }
            // The other half of the same gap, from the other surface that
            // creates a label: this one is holding a whole submission until the
            // labels it named exist, and this snapshot is where it finds out
            // they do. `resume_confirmed` below is what then runs it.
            Modal::ConfirmLabels(state) => state.absorb_created(&model.data.labels),
            // Two that hold a label and are deliberately left as they are:
            // `LabelEdit`'s copy is the `before` half of a three-way merge and
            // its fields are what the user is part-way through typing, so a
            // refresh would overwrite them -- a rename that landed underneath is
            // what `Label::merge_onto` is for, and it toasts. `Edit` holds label
            // *names* the user typed, which are theirs until they save. The rest
            // hold no label at all.
            Modal::LabelEdit(_)
            | Modal::Help(_)
            | Modal::Search(_)
            | Modal::Add(_)
            | Modal::Edit(_)
            | Modal::Priority(_)
            | Modal::Due(_)
            | Modal::QuickActions(_) => {}
        }
    }
    resume_confirmed(model)
}

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
        SyncEvent::Overwrote { subject, fields } => {
            // The write went through -- the user's value is what the server holds now --
            // so there is nothing to roll back and nothing to reload. What is owed is the
            // news, because on a fleet the person who loses the edit is on another box
            // and will never otherwise know why their change evaporated.
            //
            // Named, now that a label can collide too. `(title)` alone reads as being
            // about the task on screen, and a label is a thing shared by every task that
            // carries it -- "you overwrote somebody on a label" is a different piece of
            // news from "you overwrote somebody on this task", and the user cannot act on
            // either without knowing which.
            let what = fields.join(", ");
            model.toast(Toast::warning(format!(
                "saved over a change made elsewhere to {} ({what})",
                names(model, subject)
            )));
            Vec::new()
        }
        SyncEvent::Rejected {
            subject,
            kind,
            message,
        } => on_rejected(model, subject, &kind, &message),
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
        Submission::Add(text) => add_task(model, &text, Unknown::Ask),
        Submission::Edited(draft) => apply_edit(model, *draft, Unknown::Ask),
        Submission::Priority(value) => set_priority(model, value),
        Submission::Due(text) => set_due(model, &text),
        Submission::Labels(chosen) => set_labels(model, &chosen),
        Submission::CreateLabel(title) => create_label(model, title),
        Submission::CreateLabels(titles) => create_labels(model, titles),
        Submission::WithoutLabels(pending) => resume(model, *pending),
        Submission::EditLabel(id) => open_label_form(model, id),
        Submission::EditedLabel {
            before,
            title,
            hex_color,
        } => rename_label(model, *before, title, hex_color),
        Submission::QuickAction(index) => run_quick_action(model, index),
        Submission::Picked(Pick::Project(id)) => show(model, Scope::Project(id)),
        Submission::Picked(Pick::Label(id)) => show(model, Scope::Label(id)),
        Submission::Picked(Pick::MoveTo(id)) => move_task(model, id),
        Submission::Picked(Pick::Url(url)) => follow_link(model, url),
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
        Action::OpenUrl => match model.selected_task() {
            Some(task) => {
                let links = urls::extract(task);
                match links.len() {
                    // Said plainly rather than silently: `o` on a task with no link
                    // would otherwise look like the key was swallowed.
                    0 => {
                        model.toast(Toast::info("No link in this task"));
                        Vec::new()
                    }
                    // 547 of the store's 576 linked tasks carry exactly one. Asking
                    // "which?" about a list of one is ceremony.
                    1 => match links.into_iter().next() {
                        Some(link) => follow_link(model, link.url),
                        None => Vec::new(),
                    },
                    _ => {
                        let candidates = links
                            .into_iter()
                            .map(|link| {
                                // Matched against the URL, because that is what the user
                                // recognises and types at. A Markdown link's text is
                                // worth showing but is not what they aim with.
                                let hint =
                                    link.label.unwrap_or_else(|| link.source.hint().to_string());
                                Candidate::hinted(Pick::Url(link.url.clone()), &link.url, &hint)
                            })
                            .collect();
                        // Deliberately unsorted: the order links appear in the task is
                        // information -- title first, then down the description -- and
                        // alphabetising a list of URLs destroys it.
                        model
                            .modals
                            .push(Modal::Picker(PickerState::new(PickerKind::Url, candidates)));
                        Vec::new()
                    }
                }
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
                // Opened even with nothing in it. It used to refuse, and say "No labels
                // exist yet" -- an apology for a form that could not be filled. It can
                // be now: an empty box is where the user types a name and presses C-n,
                // which is exactly the case that message was written for.
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
                // Everything on the undo stack came from `edit`, which only pushes what
                // `inverse()` answered `Some` to -- but the match still has to be
                // exhaustive, so a mutation that cannot be inverted is skipped rather
                // than unwrapped.
                if let Some(back) = mutation.inverse() {
                    model.redo.push(back);
                }
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
                if let Some(back) = mutation.inverse() {
                    model.undo.push(back);
                }
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
    /// Labels named that do not exist.
    ///
    /// Reported rather than created, because the caller decides what that means: the
    /// interface stops and asks (see [`ConfirmLabelsState`]), since the pool is global to
    /// every project and a typo in a task line would pollute completion everywhere.
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

/// Queue the label the `l` form asked for.
///
/// No id: the store allocates the provisional one inside the same transaction that
/// writes the outbox row, and guessing at it here would be guessing at a counter this
/// crate cannot read. A reload is therefore how both `model.data.labels` and the
/// still-open form find out which id the label was given, and `Msg::LabelsLoaded` is
/// where the form ticks it.
///
/// `Effect::LoadLabels` here is the fast path, not the guarantee. The runtime spawns
/// every effect on a task of its own, so this read races `Effect::Apply`'s write and may
/// answer with a snapshot that does not name the label yet. That is harmless rather than
/// lucky: `LabelsState::absorb_created` is keyed on what the form is still waiting for,
/// so an early answer does nothing and the `Msg::Reload` that `Effect::Apply` sends once
/// the write has landed brings a second, correct one. When the write is quick — it
/// usually is — the tick simply appears a message sooner.
///
/// Not on the undo stack, and that is `Mutation::inverse` returning `None` rather than
/// an omission here: undoing a create means deleting a label, and this feature ships no
/// delete because `u` could not honestly reverse *that* — the label would come back with
/// a new id, detached from everything it had been on.
fn create_label(model: &mut Model, title: String) -> Vec<Effect> {
    let label = Label {
        title: title.clone(),
        ..Label::default()
    };
    let mut effects = edit(
        model,
        Mutation::CreateLabel {
            label: Box::new(label),
        },
    );
    model.toast(Toast::info(format!("Created label {}", truncated(&title))));
    effects.push(Effect::LoadLabels);
    effects
}

/// The [`Mutation::kind`] a queued `CreateLabel` reports.
///
/// A short stable name is what that field is for -- it exists so a policy can select by
/// kind without parsing every row -- and it is the only thing in `SyncEvent::Rejected`
/// that says *which* label operation failed. `Subject::Label(id)` names an id neither
/// waiting modal ever learned, so it cannot answer the question.
///
/// A literal, because the event carries a `String` and this crate has no `Mutation` to
/// ask -- so the coupling is asserted instead, by the test named for it in
/// `tests/update.rs`. Rename the kind in the store without it and the release silently
/// stops firing, and every held submission wedges on a rejection.
const CREATE_LABEL: &str = "create_label";

/// End every wait for a label the server has just refused to make, and take the label
/// itself off the form that is holding it.
///
/// Two modals wait on a create, and both wedge if the wait is never ended.
///
/// Gated on the *kind* and not merely on the subject being a label. A rejected rename or
/// delete of some other label is not evidence about a create that is still in flight: it
/// would abandon a wait the server has said nothing about, leaving the user with a label
/// that did get made and a task that does not carry it -- and, because the combined toast
/// above only names what was actually released, no word of it either.
///
/// The `l` form's *key* dies: `awaiting` is what stops a second `C-n` queueing a
/// duplicate while the first create is in flight, so a title left in there makes
/// `creatable` answer `None` for the rest of the form's life and `C-n` silently does
/// nothing. A user whose account cannot create labels would see the key work once and
/// then die with no explanation.
///
/// The question is worse: it is holding a whole task or edit the user has already
/// submitted, and there is no key at all that would release it -- `y` is refused while a
/// create is outstanding, precisely so a second one is not queued. So it gives up and the
/// submission runs without the label, which is the same thing declining would have done.
///
/// *All* of the waits, not only the one that was rejected: a wait is a *title*, the event
/// names an id, and neither modal ever learned the id of anything it is still waiting for
/// -- that is the whole reason both hold titles. With two creates in flight and one
/// rejected, the survivor loses its automatic tick and the held task goes without it,
/// which is the cost of a bounded answer. It is the smaller failure: the reload the caller
/// queues still brings the survivor into `model.data.labels`, so it is on screen and can
/// be attached in a second keystroke, where a dead key and a task nobody can release are
/// neither visible nor recoverable.
///
/// The *forgetting* below is the opposite and is exact, because a label the form already
/// holds is one it has learned the id of. Only the rejected id goes.
fn release_held_labels(model: &mut Model, subject: Subject, kind: &str) -> Vec<Effect> {
    let Subject::Label(rejected) = subject else {
        return Vec::new();
    };
    if kind != CREATE_LABEL {
        return Vec::new();
    }
    for modal in &mut model.modals {
        match modal {
            Modal::Labels(state) => {
                state.awaiting.clear();
                // The id *is* usable here, unlike the wait above: this form learned it
                // from the reload that named the create, and ticked it. The caller's
                // `Effect::LoadLabels` takes the phantom out of `model.data.labels` and
                // cannot take it out of here -- `LabelsState::refresh` rewrites what the
                // form holds and removes nothing -- so it would sit on the list, still
                // ticked, and the next Enter would queue an `AttachLabel` for an id the
                // server has never had.
                state.forget(rejected);
            }
            Modal::ConfirmLabels(state) => state.give_up(),
            // Nothing else waits on a create. Listed rather than wildcarded so a modal
            // that learns to has to come back here and say what a refusal does to it.
            Modal::Help(_)
            | Modal::Search(_)
            | Modal::Add(_)
            | Modal::Picker(_)
            | Modal::Edit(_)
            | Modal::Priority(_)
            | Modal::Due(_)
            | Modal::LabelEdit(_)
            | Modal::QuickActions(_) => {}
        }
    }
    resume_confirmed(model)
}

/// Queue every label a task line asked for and does not have.
///
/// One `CreateLabel` each, and they go in *before* the submission that names them: the
/// held task or edit is queued only once [`Msg::LabelsLoaded`] has named them all, so
/// the write that carries a label id can never outrun the write that makes the label.
/// That ordering is what lets `adopt_label` retarget what is queued behind it when the
/// server assigns the real id.
///
/// No ids here, for the reason [`create_label`] gives at length: `Store::queue` allocates
/// the provisional one inside its own transaction, so the reload is the only route back.
fn create_labels(model: &mut Model, titles: Vec<String>) -> Vec<Effect> {
    let mut effects = Vec::new();
    for title in &titles {
        effects.extend(edit(
            model,
            Mutation::CreateLabel {
                label: Box::new(Label {
                    title: title.clone(),
                    ..Label::default()
                }),
            },
        ));
    }
    model.toast(Toast::info(format!(
        "Creating label{} {}",
        plural(titles.len()),
        truncated(&titles.join(", "))
    )));
    effects.push(Effect::LoadLabels);
    effects
}

/// Run a submission that was waiting on an answer about its labels.
///
/// [`Unknown::Report`] on both arms: the question has been asked, and asking it a second
/// time over a label the user declined -- or one the server has just rejected -- would be
/// a loop with no way out of it.
fn resume(model: &mut Model, pending: Pending) -> Vec<Effect> {
    match pending {
        Pending::Add(text) => add_task(model, &text, Unknown::Report),
        Pending::Edit(draft) => apply_edit(model, *draft, Unknown::Report),
    }
}

/// Run a held submission whose labels have all arrived, if one has.
///
/// Called from the two places that can end a wait: the reload that names a created label,
/// and the rejection that says one is never coming. By position rather than off the top of
/// the stack, because nothing stops another modal being opened over the question -- and
/// the answer belongs to the submission that asked it, not to whatever is in front.
fn resume_confirmed(model: &mut Model) -> Vec<Effect> {
    let Some(at) = model
        .modals
        .iter()
        .position(|modal| matches!(modal, Modal::ConfirmLabels(state) if state.ready()))
    else {
        return Vec::new();
    };
    let Modal::ConfirmLabels(state) = model.modals.remove(at) else {
        // Unreachable: `position` matched on the variant. Answered rather than
        // `unwrap`ped because the workspace lints refuse one in production code.
        return Vec::new();
    };
    resume(model, state.pending)
}

/// The label names an edit form's Labels field asks for.
///
/// Comma-separated and trimmed, and empty entries dropped so a trailing comma is not a
/// label called "". Shared by the resolution and the question so the two cannot disagree
/// about what was typed.
fn label_names(field: &str) -> Vec<String> {
    field
        .split(',')
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Open the label form over `id`, from whichever list asked.
///
/// Both surfaces that list labels send this, so the form they open is one form: a label
/// renamed from the `l` ticks and a label renamed from `g l` are the same operation
/// against the same `before`.
///
/// The pool first, then the labels the tasks are carrying.
///
/// Two places, because the `l` form shows both: a pull stores labels and tasks in
/// separate passes, so a task can hold a label `model.data.labels` has not caught up
/// with, and the form lists those too. Offering `C-e` on a row and then refusing it would
/// be a key that dead-ends on whichever rows happen to be in that window.
///
/// It is a window and not a hole: the store reads a task's labels by joining the labels
/// table (`store::labels`), so a label on a task in the list has a row in the store, and
/// only this crate's snapshot of it is behind. The write lands on the same row either
/// way.
fn known_label(model: &Model, id: LabelId) -> Option<Label> {
    model
        .data
        .labels
        .iter()
        .chain(model.data.tasks.iter().flat_map(|task| task.labels.iter()))
        .find(|label| label.id == id)
        .cloned()
}

/// Open the label form over `id`, from whichever list asked.
///
/// Both surfaces that list labels send this, so the form they open is one form: a label
/// renamed from the `l` ticks and a label renamed from `g l` are the same operation
/// against the same `before`.
fn open_label_form(model: &mut Model, id: LabelId) -> Vec<Effect> {
    let Some(label) = known_label(model, id) else {
        // Nowhere on this box at all: a pull that dropped it, or another box's delete
        // landing between the list being drawn and the key being pressed.
        model.toast(Toast::info("That label is no longer here"));
        return Vec::new();
    };
    model
        .modals
        .push(Modal::LabelEdit(LabelEditState::new(label)));
    Vec::new()
}

/// Queue the rename and recolour the label form asked for.
///
/// One `UpdateLabel` carrying both fields, because the write carries the whole label
/// either way: a partial body clears what it omits, so two mutations would be two writes
/// that each undo half of the other, and two rows on the undo stack for one keystroke.
///
/// `before` is the form's own copy, carried through the submission rather than read back
/// out of the pool here. It is the half of a three-way merge that says *what the user
/// started from*, and `Label::merge_onto` decides whether a field is theirs by comparing
/// it with `after` -- so re-reading it would hand the merge a `before` the user never
/// saw. `Msg::LabelsLoaded` lands under an open form on every pull, so that is a live
/// difference, not a race: see the two tests named for it.
///
/// Nothing is queued when neither field moved. The form submits on Enter whether or not
/// anything was typed, and an `UpdateLabel` that changes nothing is still a read, a merge
/// and a write against a server that may have a newer copy -- so "no change" would be a
/// way to overwrite somebody else's rename with the value already on screen.
fn rename_label(model: &mut Model, before: Label, title: String, hex_color: String) -> Vec<Effect> {
    if known_label(model, before.id).is_none() {
        // Gone between opening the form and pressing Enter -- a pull that dropped it, or
        // another box's delete. Queuing against it would name an id this box can no
        // longer show the user anything about.
        model.toast(Toast::info("That label is no longer here"));
        return Vec::new();
    }
    let after = Label {
        title,
        hex_color,
        ..before.clone()
    };
    if after == before {
        return Vec::new();
    }
    let mut effects = edit(
        model,
        Mutation::UpdateLabel {
            before: Box::new(before),
            after: Box::new(after.clone()),
        },
    );
    model.toast(Toast::info(format!(
        "Saved label {}",
        truncated(&after.title)
    )));
    // The labels table has changed under every list that reads it. `apply_locally` has
    // already patched both of the model's own snapshots and any open form, so this is
    // the store catching up rather than the screen waiting on it.
    effects.push(Effect::LoadLabels);
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
///
/// The Labels field resolves `*name`s the same way quick-add does, so it asks the same
/// question about one that does not exist: the pool it would be added to is the same
/// global pool, and a typo in this field is as permanent as a typo in a task line.
///
/// Asked *after* the title check and before anything is queued, so an empty title is
/// still refused first — a question about a label the user cannot save anyway is a
/// question that wastes their answer.
fn apply_edit(model: &mut Model, draft: EditDraft, unknown: Unknown) -> Vec<Effect> {
    if draft.title.trim().is_empty() {
        model.toast(Toast::error("A task needs a title"));
        return Vec::new();
    }
    let wanted = label_names(&draft.labels);
    let (_, missing) = resolve_labels(&model.data.labels, &wanted);
    if unknown == Unknown::Ask && !missing.is_empty() {
        model
            .modals
            .push(Modal::ConfirmLabels(ConfirmLabelsState::new(
                missing,
                Pending::Edit(Box::new(draft)),
            )));
        return Vec::new();
    }

    let mut notes: Vec<String> = Vec::new();
    let before = *draft.before;
    let mut after = before.clone();

    after.title = draft.title.trim().to_string();
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

    // The second call over the same `wanted`, and deliberately not a different question:
    // it is the same resolution the check at the top of this function ran, re-run because
    // the answer is needed *here*, after the priority, date and project fields have been
    // parsed, while the question had to be asked *before* any of that so a refused
    // submission does not report three unrelated complaints alongside it. Both calls are
    // pure and read the same pool, so they cannot disagree.
    let (resolved, missing) = resolve_labels(&model.data.labels, &wanted);
    // Reachable only on the resumed run: the first one stopped and asked. What is left
    // here is a label the user declined, or one whose create the server rejected -- in
    // both cases they have already been told, and this says what it cost them.
    if !missing.is_empty() {
        notes.push(format!("No label called {}", missing.join(", ")));
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
    // In the user's zone before asking what day it is, the same as
    // [`crate::rows::relative_date`]. "The day changed" is a claim about the calendar the
    // user is living in: for a reader at UTC+10, moving a due date from the 27th to the
    // 26th local is 2026-08-26T14:00Z against 2026-08-25T14:00Z -- different UTC days by
    // luck, but a shift of a few hours either way makes two different local days share
    // one UTC day, and the warning that the date has passed is then never shown.
    let day = |task: &Task| {
        task.due_date
            .get()
            .map(|due| due.with_timezone(&model.now.timezone()).date_naive())
    };
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

/// What an unknown `*label` means at this point in the flow.
///
/// The same submission is run twice — once when it arrives, and again when the question
/// it raised has been answered — and the difference between the two runs is only this.
/// A second code path that built the write without asking would be a second chance to
/// build it differently from the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unknown {
    /// Stop and ask whether to create it. What a submission does when it first arrives.
    Ask,
    /// Leave it off and say so in the toast. What resuming does, so a question that has
    /// been answered is never asked twice — including when the answer was yes and the
    /// create was then rejected, which is the loop this exists to close.
    Report,
}

/// Turn quick-add text into a task, and queue it.
///
/// The parser is `tui-do-core`'s, the same one `tui-do add` uses, so the syntax cannot mean
/// two things depending on where it was typed.
///
/// An unknown `*label` stops here rather than being dropped with a note. Vikunja's label
/// pool is global across every project, so a typo in a task line becomes a permanent
/// entry that pollutes completion everywhere — see [`ConfirmLabelsState`]. Nothing is
/// queued and nothing is put on screen until the question is answered, because a task
/// created first and labelled second would be half done if the answer were no.
fn add_task(model: &mut Model, text: &str, unknown: Unknown) -> Vec<Effect> {
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

    if unknown == Unknown::Ask && !built.unknown_labels.is_empty() {
        model
            .modals
            .push(Modal::ConfirmLabels(ConfirmLabelsState::new(
                built.unknown_labels,
                Pending::Add(text.to_string()),
            )));
        return Vec::new();
    }

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
///
/// `missing` is deduplicated the same way `known` is matched -- `eq_ignore_ascii_case` on
/// the trimmed title -- so `*waiting *Waiting` is one unresolved name, not two. Without
/// this, two spellings of one typo became two `CreateLabel`s: a title is not unique to
/// Vikunja, so both answer `201` and the pool gets a permanent duplicate, which is exactly
/// the pollution this whole confirmation exists to prevent. First spelling wins, since
/// that is the one already on screen when the question is asked.
fn resolve_labels(known: &[Label], wanted: &[String]) -> (Vec<Label>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for name in wanted {
        let trimmed = name.trim();
        match known
            .iter()
            .find(|label| label.title.eq_ignore_ascii_case(trimmed))
        {
            Some(label) => found.push(label.clone()),
            None => {
                let already_missing = missing
                    .iter()
                    .any(|seen: &String| seen.eq_ignore_ascii_case(trimmed));
                if !already_missing {
                    missing.push(name.clone());
                }
            }
        }
    }
    (found, missing)
}

/// Say why a task key did nothing.
///
/// It does nothing for a good reason -- an empty list, or a filter that matched none --
/// but a key press that produces no response at all reads as a broken binding.
/// Open a link, or copy it when this box has nowhere to open it.
///
/// Which of the two is [`Model::url_action`], set once by the runtime at startup. `update`
/// stays pure: it reads a field rather than sniffing the environment, exactly as it does
/// for the theme.
fn follow_link(model: &mut Model, url: String) -> Vec<Effect> {
    // A real URL in this store runs to 700 characters. The toast says enough to recognise
    // which link was taken and no more.
    let shown = rows::truncate(&url, 60);
    match model.url_action {
        UrlAction::Open => {
            model.toast(Toast::info(format!("Opening {shown}")));
            vec![Effect::OpenUrl(url)]
        }
        // Says *why* it copied rather than opened. Without that this reads as the key
        // doing the wrong thing, on the boxes where it is the only useful thing to do.
        UrlAction::Copy => {
            model.toast(Toast::info(format!(
                "Copied {shown} — no display on this box"
            )));
            vec![Effect::CopyToClipboard(url)]
        }
    }
}

fn nothing_selected(model: &mut Model) -> Vec<Effect> {
    model.toast(Toast::info("No task selected"));
    Vec::new()
}

/// What a [`Subject`] is called, for a message the user has to act on.
///
/// The kind always, the title only when this box can honestly supply one: the list is
/// filtered and the label pool is a snapshot, so a subject the model has never loaded --
/// or has since dropped -- has no title here, and inventing one would be worse than the
/// bare noun. The event carries an id and nothing else, which is why this is a lookup and
/// not a field.
fn names(model: &Model, subject: Subject) -> String {
    let found = match subject {
        Subject::Task(id) => model
            .data
            .tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.title.clone()),
        Subject::Label(id) => model
            .data
            .labels
            .iter()
            .find(|label| label.id == id)
            .map(|label| label.title.clone()),
    };
    let kind = subject.kind();
    match found {
        Some(title) => format!("{kind} \"{}\"", truncated(&title)),
        None => format!("a {kind}"),
    }
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
    // A mutation with no inverse -- creating a label -- is simply not remembered, so `u`
    // reaches past it to the last change that can be taken back.
    if let Some(back) = mutation.inverse() {
        model.undo.push(back);
    }
    model.redo.clear();
    apply(model, mutation)
}

/// Swap a provisional id for the one the server gave it.
///
/// Split by kind because the two id spaces are unrelated: a task id and a label id of the
/// same number name different things, and a single renumbering routine taking both would
/// be one typo away from moving the wrong one. The event carries both sides as a
/// [`Subject`] for exactly that reason, and a mismatched pair — which the engine never
/// emits — is ignored rather than guessed at.
fn adopt(model: &mut Model, provisional: Subject, assigned: Subject) -> Vec<Effect> {
    match (provisional, assigned) {
        (Subject::Task(provisional), Subject::Task(assigned)) => {
            adopt_task(model, provisional, assigned)
        }
        (Subject::Label(provisional), Subject::Label(assigned)) => {
            adopt_label(model, provisional, assigned)
        }
        // A create is answered by a create of the same kind, so a mismatched pair is a
        // bug in the sync engine. Renumbering a label because a task id happened to match
        // would hide it behind whatever it corrupted.
        _ => Vec::new(),
    }
}

/// The task half: the row on screen, the selection, and the undo stack.
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
fn adopt_task(model: &mut Model, provisional: TaskId, assigned: TaskId) -> Vec<Effect> {
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

/// The label half: two snapshots, the filter, both stacks, and the modal stack.
///
/// The store has already renumbered its own rows, the `task_labels` links and anything
/// still queued. What is left is everything the interface holds by id, and *all* of it has
/// to move together or the next write names `/labels/-1` — a `404` about the label the
/// server has just created.
///
/// A label has more holders than a task, and they are worth naming because missing one is
/// silent:
///
/// * `model.data.labels`, which the picker lists, which `resolve_labels` matches a
///   quick-add `*name` against, and where a colour is looked up,
/// * the `labels` carried on every task in the list, which is a *second* and independent
///   copy — the row chips are drawn from it, and it is the lesson `apply_locally`'s
///   `UpdateLabel` arm already had to learn,
/// * the undo and redo stacks, whose mutations name the label they act on,
/// * [`Scope::Label`], when the list is being filtered by the label just created — a
///   reload against the provisional id answers with nothing at all,
/// * the modal stack, which is the one most easily missed. An open [`Modal::Labels`] is on
///   screen at precisely this moment, because that form is where the label was created: it
///   holds cloned labels and a `chosen` list of ticked ids, and `set_labels` resolves
///   those ids against `model.data.labels` when the user presses Enter. Move one and not
///   the other and the tick the user is looking at queues nothing whatsoever.
///
/// The whole stack rather than its top, and every variant that holds an id rather than the
/// label form alone: a label picker's candidates become the list's filter, and the edit
/// form keeps the task whole as `before`, which is what `apply_edit` computes its attach
/// and detach sets against.
fn adopt_label(model: &mut Model, provisional: LabelId, assigned: LabelId) -> Vec<Effect> {
    let swap = |id: &mut LabelId| {
        if *id == provisional {
            *id = assigned;
        }
    };
    for label in &mut model.data.labels {
        swap(&mut label.id);
    }
    for task in &mut model.data.tasks {
        for label in &mut task.labels {
            swap(&mut label.id);
        }
    }
    for mutation in model.undo.iter_mut().chain(model.redo.iter_mut()) {
        mutation.retarget_label(provisional, assigned);
    }
    // Before the reload below, which reads the scope to build its filter.
    if model.query.scope == Scope::Label(provisional) {
        model.query.scope = Scope::Label(assigned);
    }
    for modal in &mut model.modals {
        match modal {
            Modal::Labels(state) => {
                for label in &mut state.labels {
                    swap(&mut label.id);
                }
                for id in &mut state.chosen {
                    swap(id);
                }
            }
            Modal::Picker(state) => {
                for candidate in &mut state.candidates {
                    if let Pick::Label(id) = &mut candidate.pick {
                        swap(id);
                    }
                }
            }
            Modal::Edit(state) => {
                for label in &mut state.before.labels {
                    swap(&mut label.id);
                }
            }
            // Reachable, and the worst one to miss: the label form opens over a label it
            // may have created moments earlier -- `C-n` then `C-e` is two keystrokes --
            // and the id it is holding is what its `EditedLabel` names. Left at the
            // provisional, the rename would queue against `/labels/-1`.
            Modal::LabelEdit(state) => swap(&mut state.label.id),
            // The question itself holds titles and never an id -- that is the whole
            // reason it holds titles -- and the names it is waiting on are re-resolved
            // against `model.data.labels`, renumbered above, at the moment it resumes.
            //
            // What it is *sitting on* is another matter, and the one this arm exists for.
            // A held `EditDraft` carries the task whole as `before`, exactly as
            // `Modal::Edit` does above and for the same reason -- it is what `apply_edit`
            // computes its attach and detach sets against. Left at the provisional, the
            // resumed save detaches the label the server has just named and attaches it
            // again under an id that no longer exists. Held quick-add text carries no id
            // at all.
            Modal::ConfirmLabels(state) => {
                if let Pending::Edit(draft) = &mut state.pending {
                    for label in &mut draft.before.labels {
                        swap(&mut label.id);
                    }
                }
            }
            // Nothing else holds a label id: help and the quick-action menu are drawn
            // from the keymap and the config, search and add are text, and the priority
            // and due fields carry one value each. Listed rather than wildcarded so a
            // modal that grows one has to come back here and say so.
            Modal::Help(_)
            | Modal::Search(_)
            | Modal::Add(_)
            | Modal::Priority(_)
            | Modal::Due(_)
            | Modal::QuickActions(_) => {}
        }
    }
    // The store now holds the server's own copy of the label -- its identifier, and
    // whatever it made of the colour -- and the task links moved with it, so both
    // snapshots are re-read rather than only patched in place. Not `reload_everything`:
    // no project changed, and a label carries no per-project count.
    let mut effects = reload_tasks(model);
    effects.push(Effect::LoadLabels);
    effects
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
        // A rename or a recolour has to reach *two* snapshots, because the model keeps
        // two copies of every label and neither is a view of the other:
        //
        // - the copy carried on each task in the list, which is what the row chips are
        //   drawn from, and
        // - `model.data.labels`, which is what the label picker lists, what
        //   `resolve_labels` matches a quick-add `*name` against, and where a colour is
        //   looked up.
        //
        // Both are written only by a store reload, and `apply` returns `Effect::Apply` and
        // `Effect::LoadPending` -- no labels reload. So an edit that touched only one of
        // them leaves the other showing the old title until something else happens to
        // reload, which for the picker means quick-add still resolving the name the user
        // just renamed away from.
        //
        // Matched on the id, not the title: the title is the thing that just changed.
        Mutation::UpdateLabel { after, .. } => {
            for task in tasks.iter_mut() {
                for held in &mut task.labels {
                    if held.id == after.id {
                        *held = (**after).clone();
                    }
                }
            }
            for known in &mut model.data.labels {
                if known.id == after.id {
                    *known = (**after).clone();
                }
            }
            // And a *third* copy, which became reachable the moment a label could be
            // renamed from a list of labels: the form the user pressed the key in is
            // still open underneath, and it holds its own clones. Neither is a view of
            // `model.data.labels` -- the label form has to keep its own, because it also
            // shows labels a task carries that the pool has not caught up with -- so a
            // rename that stopped at the two snapshots above leaves the user reading the
            // old title on the row they just renamed.
            for modal in &mut model.modals {
                match modal {
                    Modal::Labels(state) => {
                        for held in &mut state.labels {
                            if held.id == after.id {
                                *held = (**after).clone();
                            }
                        }
                    }
                    // The candidate list is rebuilt from `model.data.labels` only when
                    // the picker opens, and `matches` indexes into it, so the title is
                    // rewritten in place rather than the list rebuilt: the highlight
                    // stays on the row the user is looking at, and the next keystroke
                    // refilters against the new title.
                    Modal::Picker(state) => {
                        for candidate in &mut state.candidates {
                            if candidate.pick == Pick::Label(after.id) {
                                candidate.title.clone_from(&after.title);
                            }
                        }
                    }
                    // Two that hold a label and are deliberately left alone, because
                    // modals are exclusive and neither can be on the stack when this
                    // runs:
                    //
                    // * `LabelEdit` is the form that asked, and `Outcome::Submit` pops it
                    //   before `on_submit` is called. Nothing opens a second one over it,
                    //   and `u` is a key the top modal would have eaten.
                    // * `Edit` cannot be underneath a form that renames a label: the two
                    //   surfaces that open one are reached from the task screen, which
                    //   `Edit` covers. Its own `labels` field is *names* the user typed
                    //   anyway, resolved against `model.data.labels` -- rewritten above.
                    // * `ConfirmLabels` is the same story once more removed: the titles
                    //   it is waiting on are matched against the pool, rewritten above,
                    //   and a held `EditDraft` carries a task whose labels `apply_edit`
                    //   reads by *id*, never by title. Its ids do move, which is why
                    //   `adopt_label` has an arm for it and this does not.
                    //
                    // The rest hold no label at all.
                    Modal::ConfirmLabels(_)
                    | Modal::LabelEdit(_)
                    | Modal::Help(_)
                    | Modal::Search(_)
                    | Modal::Add(_)
                    | Modal::Edit(_)
                    | Modal::Priority(_)
                    | Modal::Due(_)
                    | Modal::QuickActions(_) => {}
                }
            }
        }
        // The one mutation with nothing to rewrite. A label the server has never seen
        // cannot be on a task in the list, and it is not in `model.data.labels` either --
        // but that is because the create is queued from a screen that reloads, not because
        // anything here reads the store directly. Nothing in this model does; both label
        // snapshots above are written by `Msg::LabelsLoaded`, and rewritten only by the
        // arm above and by `adopt_label`, which renumbers what the server has just named.
        Mutation::CreateLabel { .. } => {}
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
        // `after` is where the undo left it, which is the label's title *before* the
        // rename. It needs the noun that the two arms above get for free from their verb:
        // a bare `Undone — urgent` says nothing about what happened to `urgent`, and
        // "reverted" rather than "renamed" because an `UpdateLabel` can be a recolour.
        Mutation::UpdateLabel { after, .. } => format!("Undone — reverted label {}", after.title),
        // Unreachable in practice: `inverse()` returns `None` for a create, so `edit`
        // never pushes one onto the undo stack for `Action::Undo` to pop back out here.
        // Still has to type-check against every `Mutation`, the same as every arm above,
        // and still has to read as a sentence if it ever does surface -- a create on the
        // undo stack is the inverse of a delete, which is the `CreateTask` arm's reading.
        Mutation::CreateLabel { label } => format!("Undone — restored label {}", label.title),
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
