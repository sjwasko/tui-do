//! `update` driven by message sequences.
//!
//! No terminal is involved, which is the point of keeping the layer pure: every one of
//! these would need a pty and a screen scrape in the architecture criax replaces.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{TimeZone, Utc};
use criax_core::config::columns::ColumnLayout;
use criax_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use criax_core::store::{Mutation, ProjectCounts, TaskCount, TaskOrder};
use criax_core::sync::{Phase, PullReport, PushReport, SyncReport};
use criax_core::{Config, SyncEvent};
use criax_tui::keymap::Key;
use criax_tui::modal::Modal;
use criax_tui::model::{Focus, PaneState, SyncStatus};
use criax_tui::query::Scope;
use criax_tui::update::{reload_everything, update};
use criax_tui::{Effect, Model, Msg};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
}

fn task(id: i64, title: &str) -> Task {
    Task {
        id: TaskId(id),
        project_id: ProjectId(1),
        title: title.to_string(),
        ..Task::default()
    }
}

fn project(id: i64, title: &str, parent: i64) -> Project {
    Project {
        id: ProjectId(id),
        title: title.to_string(),
        parent_project_id: ProjectId(parent),
        ..Project::default()
    }
}

/// A model that has already been answered: three tasks, two projects, counts.
fn loaded() -> Model {
    let mut model = Model::new(&Config::example(), Scope::All, now(), (160, 40));
    let _ = reload_everything(&mut model);
    answer(
        &mut model,
        vec![task(1, "first"), task(2, "second"), task(3, "third")],
    );
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![
            // Named so the tree order is unambiguous: roots sort by title, so Alpha
            // (with its child) comes before Personal.
            project(1, "Alpha", 0),
            project(2, "Alpha child", 1),
            project(3, "Personal", 0),
        ]),
    );
    update(
        &mut model,
        Msg::CountsLoaded(ProjectCounts {
            by_project: [(ProjectId(1), TaskCount { open: 3, done: 0 })]
                .into_iter()
                .collect(),
            favorites: TaskCount::default(),
        }),
    );
    model
}

/// Answer the model's current task query.
fn answer(model: &mut Model, tasks: Vec<Task>) -> Vec<Effect> {
    let id = model.query_id;
    update(model, Msg::TasksLoaded { id, tasks })
}

fn press(model: &mut Model, c: char) -> Vec<Effect> {
    update(
        model,
        Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
    )
}

fn press_code(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    update(model, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn selected_title(model: &Model) -> &str {
    model.selected_task().map_or("", |task| task.title.as_str())
}

fn loaded_query(effects: &[Effect]) -> Option<criax_tui::QueryId> {
    effects.iter().find_map(|effect| match effect {
        Effect::LoadTasks { id, .. } => Some(*id),
        _ => None,
    })
}

#[test]
fn a_first_paint_asks_for_everything_it_shows() {
    let mut model = Model::new(&Config::example(), Scope::All, now(), (160, 40));
    let effects = reload_everything(&mut model);
    assert!(effects.contains(&Effect::LoadProjects));
    assert!(effects.contains(&Effect::LoadLabels));
    assert!(effects.contains(&Effect::LoadCounts));
    assert!(loaded_query(&effects).is_some());
    assert!(model.data.loading, "the list says 'loading', not 'empty'");
}

#[test]
fn an_answer_to_a_superseded_query_is_dropped() {
    let mut model = loaded();
    let stale = model.query_id;

    // The user moves on: a new query is issued and gets a new id.
    let effects = press(&mut model, 't');
    let current = loaded_query(&effects).expect("toggling done re-queries");
    assert_ne!(current, stale);

    // The slow answer to the old query arrives second and must not be rendered.
    update(
        &mut model,
        Msg::TasksLoaded {
            id: stale,
            tasks: vec![task(99, "from the query you cancelled")],
        },
    );
    assert_eq!(model.data.tasks.len(), 3);

    update(
        &mut model,
        Msg::TasksLoaded {
            id: current,
            tasks: vec![task(7, "the one you asked for")],
        },
    );
    assert_eq!(selected_title(&model), "the one you asked for");
}

#[test]
fn the_selection_follows_the_task_not_the_row() {
    let mut model = loaded();
    press(&mut model, 'j');
    assert_eq!(selected_title(&model), "second");

    // A sync reorders the list and drops one row. The cursor must stay on "second".
    answer(&mut model, vec![task(3, "third"), task(2, "second")]);
    assert_eq!(selected_title(&model), "second");
    assert_eq!(model.selected_index(), Some(1));
}

#[test]
fn a_selection_that_no_longer_exists_falls_to_the_top_rather_than_nowhere() {
    let mut model = loaded();
    press(&mut model, 'j');
    answer(&mut model, vec![task(5, "everything else was deleted")]);
    assert_eq!(selected_title(&model), "everything else was deleted");
}

#[test]
fn motion_clamps_at_both_ends_instead_of_wrapping() {
    let mut model = loaded();
    for _ in 0..10 {
        press(&mut model, 'j');
    }
    assert_eq!(selected_title(&model), "third");
    for _ in 0..10 {
        press(&mut model, 'k');
    }
    assert_eq!(selected_title(&model), "first");
}

#[test]
fn gg_and_shift_g_reach_the_ends_and_a_dead_chord_does_nothing() {
    let mut model = loaded();
    press(&mut model, 'G');
    assert_eq!(selected_title(&model), "third");

    press(&mut model, 'g');
    assert_eq!(model.pending.len(), 1, "the chord is in flight");
    press(&mut model, 'g');
    assert_eq!(selected_title(&model), "first");
    assert!(model.pending.is_empty());

    // `g` then something unbound cancels rather than falling through to `j`.
    press(&mut model, 'g');
    press(&mut model, 'j');
    assert_eq!(selected_title(&model), "first");
    assert!(model.pending.is_empty());
}

#[test]
fn a_page_moves_by_the_height_of_the_list() {
    let mut model = Model::new(&Config::example(), Scope::All, now(), (100, 10));
    let _ = reload_everything(&mut model);
    let tasks: Vec<Task> = (1..=50).map(|n| task(n, &format!("task {n}"))).collect();
    answer(&mut model, tasks);
    let rows = model.frames().list_rows();
    assert!(rows > 1);

    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
    );
    assert_eq!(model.selected_index(), Some(rows));
    // And the viewport followed it.
    assert!(model.list.offset > 0);
}

#[test]
fn moving_down_the_sidebar_shows_that_project_and_remembers_it() {
    let mut model = loaded();
    // Tab cycles sidebar -> list -> preview, so the sidebar is one step back.
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);

    let effects = press(&mut model, 'j');
    assert_eq!(model.query.scope, Scope::Project(ProjectId(1)));
    assert!(effects.contains(&Effect::RememberProject(Some(ProjectId(1)))));
    assert!(loaded_query(&effects).is_some());

    // Back to the top: "All tasks" is remembered as no project at all.
    let effects = press(&mut model, 'k');
    assert_eq!(model.query.scope, Scope::All);
    assert!(effects.contains(&Effect::RememberProject(None)));
}

#[test]
fn collapsing_a_project_hides_its_children_and_expanding_restores_them() {
    let mut model = loaded();
    press_code(&mut model, KeyCode::BackTab);
    press(&mut model, 'j'); // Alpha, which has a child
    assert_eq!(model.query.scope, Scope::Project(ProjectId(1)));

    press(&mut model, 'h');
    assert!(model.sidebar.collapsed.contains(&ProjectId(1)));
    // Its child is no longer reachable by moving down.
    press(&mut model, 'j');
    assert_eq!(model.query.scope, Scope::Project(ProjectId(3)));

    press(&mut model, 'k');
    press(&mut model, 'l');
    assert!(!model.sidebar.collapsed.contains(&ProjectId(1)));
    press(&mut model, 'j');
    assert_eq!(model.query.scope, Scope::Project(ProjectId(2)));
}

#[test]
fn a_resize_moves_an_auto_pane_but_leaves_a_pinned_one_alone() {
    let mut model = loaded();
    assert!(model.sidebar_showing());

    update(&mut model, Msg::Resize(80, 30));
    assert!(!model.sidebar_showing(), "auto follows the width");

    press(&mut model, 'z');
    press(&mut model, 's');
    assert_eq!(model.panes.sidebar, PaneState::Shown);
    assert!(model.sidebar_showing());

    update(&mut model, Msg::Resize(160, 40));
    update(&mut model, Msg::Resize(80, 30));
    assert!(model.sidebar_showing(), "a pin survives a resize");
}

#[test]
fn a_pane_that_cannot_fit_says_so_instead_of_doing_nothing() {
    // The bug this exists for: on a narrow terminal `z p` set the pane to Shown, a second
    // width rule kept it off screen, and nothing on screen explained why. Pressing a key
    // and getting no response at all is indistinguishable from a broken keyboard.
    let mut model = loaded();
    update(&mut model, Msg::Resize(50, 30));
    assert!(!model.preview_showing());

    press(&mut model, 'z');
    press(&mut model, 'p');
    assert_eq!(
        model.panes.preview,
        PaneState::Shown,
        "the intent is recorded"
    );
    assert!(!model.preview_showing(), "but there is genuinely no room");
    let toast = model.status.toast.as_ref().expect("the user is told why");
    assert!(toast.text.contains("No room"), "{}", toast.text);
    assert!(toast.text.contains("50 columns"), "{}", toast.text);

    // And widening honours the pin that was recorded while it could not be shown.
    update(&mut model, Msg::Resize(120, 40));
    assert!(model.preview_showing());
}

#[test]
fn when_the_sidebar_is_what_is_in_the_way_the_message_says_so() {
    // At 80 columns there is room for a preview, but not beside a sidebar -- which is
    // laid out first. "Widen the terminal" would be true and useless.
    let mut model = loaded();
    update(&mut model, Msg::Resize(80, 30));
    press(&mut model, 'z');
    press(&mut model, 's');
    assert!(model.sidebar_showing(), "pinned on at 80");

    press(&mut model, 'z');
    press(&mut model, 'p');
    let toast = model.status.toast.as_ref().expect("the user is told why");
    assert!(toast.text.contains("hide it with z s"), "{}", toast.text);

    // And taking that advice works, without having to ask for the preview again.
    press(&mut model, 'z');
    press(&mut model, 's');
    assert!(model.preview_showing());
}

#[test]
fn a_pin_is_honoured_below_the_auto_breakpoint() {
    // 90 columns is under the preview's auto minimum but has room for one, so asking for
    // it works rather than being silently overruled.
    let mut model = loaded();
    update(&mut model, Msg::Resize(90, 30));
    assert!(!model.preview_showing(), "auto keeps it off at this width");

    press(&mut model, 'z');
    press(&mut model, 'p');
    assert!(model.preview_showing(), "asking for it works");
    assert!(model.status.toast.is_none(), "and says nothing about room");
}

#[test]
fn hiding_the_focused_pane_moves_focus_rather_than_stranding_it() {
    let mut model = loaded();
    // Tab cycles sidebar -> list -> preview, so the sidebar is one step back.
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);

    press(&mut model, 'z');
    press(&mut model, 's');
    assert_eq!(model.focus, Focus::List);

    // And a resize does the same -- even to a pinned sidebar, once the terminal is too
    // narrow to lay one out at all. (50 columns still fits a 20-wide sidebar beside a
    // 30-wide list; 45 does not.)
    press(&mut model, 'z');
    press(&mut model, 's');
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);
    update(&mut model, Msg::Resize(45, 30));
    assert_eq!(model.focus, Focus::List);
}

#[test]
fn escape_backs_out_of_a_pane_rather_than_doing_nothing() {
    // Esc is the key everyone reaches for to undo the last narrowing. Bound only inside
    // modals, it did nothing out here, which reads as the interface being stuck.
    let mut model = loaded();
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.focus, Focus::Preview);
    press_code(&mut model, KeyCode::Esc);
    assert_eq!(model.focus, Focus::List);

    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);
    press_code(&mut model, KeyCode::Esc);
    assert_eq!(model.focus, Focus::List);
}

#[test]
fn escape_then_clears_the_search_and_never_quits() {
    let mut model = loaded();
    press(&mut model, '/');
    for c in "bug".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.query.search.as_deref(), Some("bug"));

    let effects = press_code(&mut model, KeyCode::Esc);
    assert_eq!(model.query.search, None, "the second Esc leaves the search");
    assert!(
        loaded_query(&effects).is_some(),
        "and re-queries without it"
    );

    // The bottom of the ladder is nothing at all. Esc must never be the key that quits.
    let effects = press_code(&mut model, KeyCode::Esc);
    assert!(effects.is_empty());
    assert!(model.running);
}

#[test]
fn escape_closes_a_modal_before_it_touches_focus() {
    let mut model = loaded();
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.focus, Focus::Preview);

    press(&mut model, '?');
    press_code(&mut model, KeyCode::Esc);
    assert!(model.modals.is_empty());
    assert_eq!(
        model.focus,
        Focus::Preview,
        "the modal took the key, not the pane"
    );
}

#[test]
fn enter_opens_the_preview_and_motion_scrolls_it() {
    let mut model = loaded();
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.focus, Focus::Preview);

    let before = selected_title(&model).to_string();
    press(&mut model, 'j');
    assert_eq!(model.list.preview_scroll, 1);
    assert_eq!(selected_title(&model), before, "the list did not move");

    press(&mut model, 'k');
    press(&mut model, 'k');
    assert_eq!(model.list.preview_scroll, 0, "scrolling stops at the top");
}

#[test]
fn switching_layout_re_queries_with_the_layouts_own_sort() {
    let mut model = loaded();
    let layouts = ColumnLayout::defaults();
    assert!(layouts.len() > 1);

    let effects = press(&mut model, 'L');
    assert_eq!(model.layout().name, layouts[1].name);
    assert!(loaded_query(&effects).is_some(), "the sort lives in SQL");
    assert!(
        model.status.toast.is_some(),
        "the user is told which layout"
    );

    press(&mut model, 'H');
    assert_eq!(model.layout().name, layouts[0].name);
    assert_eq!(model.query.sort.order, TaskOrder::DueDate);
}

#[test]
fn the_list_narrows_while_the_search_is_still_being_typed() {
    let mut model = loaded();
    press(&mut model, '/');
    assert!(matches!(model.modals.last(), Some(Modal::Search(_))));

    // Each keystroke re-queries, and the prompt stays open.
    let after_b = press(&mut model, 'b');
    assert!(loaded_query(&after_b).is_some());
    assert_eq!(model.query.search.as_deref(), Some("b"));
    assert_eq!(model.modals.len(), 1, "the prompt is still up");

    let after_u = press(&mut model, 'u');
    assert_eq!(model.query.search.as_deref(), Some("bu"));
    assert_eq!(model.query.filter().search.as_deref(), Some("bu"));

    // Every one of those queries has its own id, which is what makes firing them per
    // keystroke safe: the answer to "b" cannot land after the answer to "bu".
    assert_ne!(loaded_query(&after_b), loaded_query(&after_u));

    // `q` is text here, not a quit.
    press(&mut model, 'q');
    assert!(model.running);
    assert_eq!(model.query.search.as_deref(), Some("buq"));

    press_code(&mut model, KeyCode::Enter);
    assert!(model.modals.is_empty(), "Enter puts the prompt away");
    assert_eq!(
        model.query.search.as_deref(),
        Some("buq"),
        "and keeps the filter"
    );
}

#[test]
fn abandoning_a_search_puts_back_the_list_it_started_from() {
    let mut model = loaded();
    press(&mut model, '/');
    for c in "bug".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.query.search.as_deref(), Some("bug"));

    // A second search, abandoned, leaves the first one in place rather than clearing it.
    press(&mut model, '/');
    for c in "zzz".chars() {
        press(&mut model, c);
    }
    assert_eq!(model.query.search.as_deref(), Some("bugzzz"));

    let effects = press_code(&mut model, KeyCode::Esc);
    assert!(model.modals.is_empty());
    assert_eq!(
        model.query.search.as_deref(),
        Some("bug"),
        "restored, not cleared"
    );
    assert!(loaded_query(&effects).is_some());
}

#[test]
fn the_project_picker_switches_scope() {
    let mut model = loaded();
    press(&mut model, 'g');
    press(&mut model, 'p');
    assert!(matches!(model.modals.last(), Some(Modal::Picker(_))));

    for c in "personal".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(model.modals.is_empty());
    assert_eq!(model.query.scope, Scope::Project(ProjectId(3)));
    assert!(effects.contains(&Effect::RememberProject(Some(ProjectId(3)))));
}

#[test]
fn the_label_picker_filters_without_moving_the_sidebar_highlight() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::LabelsLoaded(vec![Label {
            id: LabelId(4),
            title: "urgent".to_string(),
            ..Label::default()
        }]),
    );
    let before = model.sidebar.selected;

    press(&mut model, 'g');
    press(&mut model, 'l');
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.query.scope, Scope::Label(LabelId(4)));
    assert_eq!(model.query.filter().label, Some(LabelId(4)));
    assert_eq!(model.sidebar.selected, before);
}

#[test]
fn the_command_palette_runs_an_action_by_name() {
    let mut model = loaded();
    assert!(model.sidebar_showing());

    press(&mut model, ':');
    assert!(matches!(model.modals.last(), Some(Modal::Picker(_))));

    for c in "sidebar".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    assert!(model.modals.is_empty());
    assert_eq!(model.panes.sidebar, PaneState::Hidden);
    assert_eq!(model.focus, Focus::List, "focus left the pane it hid");
}

#[test]
fn a_command_run_by_name_is_the_same_code_path_as_its_key() {
    // Toggling done tasks through the palette must produce the same effects, and the
    // same toast, as pressing `t` -- one implementation, not two.
    let mut by_key = loaded();
    let key_effects = press(&mut by_key, 't');

    let mut by_name = loaded();
    press(&mut by_name, ':');
    for c in "completed".chars() {
        press(&mut by_name, c);
    }
    let name_effects = press_code(&mut by_name, KeyCode::Enter);

    assert_eq!(by_name.query.include_done, by_key.query.include_done);
    assert_eq!(by_name.status.toast, by_key.status.toast);
    assert_eq!(name_effects.len(), key_effects.len());
}

#[test]
fn the_palette_offers_no_motions_and_shows_the_key_beside_each_command() {
    let mut model = loaded();
    press(&mut model, ':');
    let Some(Modal::Picker(picker)) = model.modals.last() else {
        panic!("the palette did not open");
    };
    let titles: Vec<&str> = picker
        .candidates
        .iter()
        .map(|candidate| candidate.title.as_str())
        .collect();
    assert!(titles.contains(&"Sync now"));
    assert!(!titles.iter().any(|title| title.starts_with("Move ")));
    assert!(!titles.contains(&"Run a command by name"));

    let sync = picker
        .candidates
        .iter()
        .find(|candidate| candidate.title == "Sync now")
        .expect("sync is offered");
    assert_eq!(sync.hint, "r");
}

#[test]
fn the_palette_offers_the_focused_panes_commands() {
    let mut model = loaded();
    press(&mut model, ':');
    let Some(Modal::Picker(from_list)) = model.modals.last() else {
        panic!("the palette did not open");
    };
    let list_titles: Vec<String> = from_list
        .candidates
        .iter()
        .map(|candidate| candidate.title.clone())
        .collect();
    assert!(list_titles.contains(&"Open the selected task".to_string()));
    assert!(!list_titles.contains(&"Collapse, or move to the parent".to_string()));

    press_code(&mut model, KeyCode::Esc);
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);
    press(&mut model, ':');
    let Some(Modal::Picker(from_sidebar)) = model.modals.last() else {
        panic!("the palette did not open");
    };
    assert!(from_sidebar
        .candidates
        .iter()
        .any(|candidate| candidate.title == "Collapse, or move to the parent"));
}

#[test]
fn quitting_from_the_palette_quits() {
    let mut model = loaded();
    press(&mut model, ':');
    for c in "quit".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(!model.running);
    assert!(effects.contains(&Effect::Quit));
}

fn applied(effects: &[Effect]) -> Option<&Mutation> {
    effects.iter().find_map(|effect| match effect {
        Effect::Apply(mutation) => Some(mutation),
        _ => None,
    })
}

#[test]
fn marking_done_shows_before_it_is_stored() {
    let mut model = loaded();
    assert!(!model.selected_task().unwrap().done);

    let effects = press(&mut model, 'd');

    // The frame after the keystroke already has it, without waiting for the store.
    assert!(model.selected_task().unwrap().done);
    assert!(model.selected_task().unwrap().done_at.get().is_some());

    // And the durable half was asked for, carrying what it looked like before.
    match applied(&effects).expect("a write was queued") {
        Mutation::UpdateTask { before, after } => {
            assert!(!before.done, "the rollback still has the old row");
            assert!(after.done);
        }
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn undo_queues_the_inverse_like_any_other_change() {
    let mut model = loaded();
    press(&mut model, 'd');
    assert!(model.selected_task().unwrap().done);
    assert_eq!(model.undo.len(), 1);

    let effects = press(&mut model, 'u');
    assert!(!model.selected_task().unwrap().done, "the list went back");
    assert!(
        applied(&effects).is_some(),
        "undo is a queued mutation, not a parallel mechanism"
    );
    assert!(model.undo.is_empty());
    assert_eq!(model.redo.len(), 1);
}

#[test]
fn redo_puts_it_back_and_a_new_edit_clears_the_stack() {
    let mut model = loaded();
    press(&mut model, 'd');
    press(&mut model, 'u');
    assert_eq!(model.redo.len(), 1);

    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
    );
    assert!(model.selected_task().unwrap().done);
    assert!(model.redo.is_empty());

    press(&mut model, 'u');
    press(&mut model, 'd');
    assert!(model.redo.is_empty(), "a new edit clears what was undone");
}

#[test]
fn undo_goes_back_as_far_as_the_session_does() {
    let mut model = loaded();
    for _ in 0..3 {
        press(&mut model, 'd');
        press(&mut model, 'j');
    }
    assert_eq!(model.undo.len(), 3);
    assert_eq!(model.data.tasks.iter().filter(|task| task.done).count(), 3);

    for _ in 0..3 {
        press(&mut model, 'u');
    }
    assert_eq!(model.data.tasks.iter().filter(|task| task.done).count(), 0);
    assert!(model.undo.is_empty());

    // And the bottom of the stack says so rather than doing something arbitrary.
    let effects = press(&mut model, 'u');
    assert!(applied(&effects).is_none());
    assert!(model
        .status
        .toast
        .as_ref()
        .is_some_and(|toast| toast.text.contains("Nothing to undo")));
}

#[test]
fn a_write_is_reloaded_from_the_store_rather_than_trusted() {
    let mut model = loaded();
    press(&mut model, 'd');
    // The runtime sends this once the write has landed.
    let effects = update(&mut model, Msg::Reload);
    assert!(loaded_query(&effects).is_some());
    assert!(effects.contains(&Effect::LoadCounts));
}

#[test]
fn done_tasks_are_hidden_until_asked_for() {
    let mut model = loaded();
    assert_eq!(model.query.filter().done, Some(false));

    press(&mut model, 't');
    assert_eq!(model.query.filter().done, None);
    assert!(model.status.toast.is_some());

    press(&mut model, 't');
    assert_eq!(model.query.filter().done, Some(false));
}

#[test]
fn help_opens_from_the_table_and_any_key_closes_it() {
    let mut model = loaded();
    press(&mut model, '?');
    assert!(matches!(model.modals.last(), Some(Modal::Help(_))));

    // Movement scrolls it rather than closing it.
    press(&mut model, 'j');
    assert_eq!(model.modals.len(), 1);

    press_code(&mut model, KeyCode::Esc);
    assert!(model.modals.is_empty());
}

#[test]
fn a_modal_swallows_every_key_except_quit() {
    let mut model = loaded();
    press(&mut model, '/');
    // A binding that would otherwise fire does not reach the screen.
    press(&mut model, 'j');
    assert_eq!(selected_title(&model), "first");
    assert_eq!(model.modals.len(), 1);

    let effects = update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert!(!model.running);
    assert!(effects.contains(&Effect::Quit));
}

#[test]
fn a_push_on_its_own_ends_the_sending_it_started() {
    // An edit asks for a push, not a full pass. Without its own event the header would
    // sit on "sending" for ever and the queued count would never move.
    let mut model = loaded();
    update(&mut model, Msg::Sync(SyncEvent::Started(Phase::Push)));
    assert!(matches!(model.status.sync, SyncStatus::Working { .. }));

    update(
        &mut model,
        Msg::Sync(SyncEvent::Pushed(PushReport {
            sent: 1,
            rejected: 0,
            deferred: 2,
        })),
    );
    assert_eq!(model.status.sync, SyncStatus::Idle);
    assert_eq!(model.status.queued, 2, "what is still waiting is visible");
    assert_eq!(
        model.status.last_sync, None,
        "a push fetched nothing, so it cannot claim the list is current"
    );
}

#[test]
fn a_finished_sync_reloads_what_the_screen_is_showing() {
    let mut model = loaded();
    update(&mut model, Msg::Sync(SyncEvent::Started(Phase::Pull)));
    assert!(matches!(model.status.sync, SyncStatus::Working { .. }));

    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Finished(SyncReport {
            push: PushReport {
                sent: 1,
                rejected: 0,
                deferred: 2,
            },
            pull: PullReport::default(),
        })),
    );
    assert_eq!(model.status.sync, SyncStatus::Idle);
    assert_eq!(model.status.last_sync, Some(now()));
    assert_eq!(model.status.queued, 2);
    assert!(effects.contains(&Effect::LoadCounts));
    assert!(loaded_query(&effects).is_some());
}

#[test]
fn a_rejected_change_is_the_one_sync_event_the_user_is_shown() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: TaskId(1),
            kind: "update_task".to_string(),
            message: "This project does not exist.".to_string(),
        }),
    );
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("This project does not exist."));
}

#[test]
fn a_failed_sync_says_so_without_taking_the_interface_away() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::Sync(SyncEvent::Failed {
            phase: Phase::Pull,
            message: "connection refused".to_string(),
        }),
    );
    assert!(matches!(model.status.sync, SyncStatus::Failed { .. }));
    assert!(model.running);
    assert!(model.modals.is_empty(), "not a modal; the app still works");

    press(&mut model, 'j');
    assert_eq!(selected_title(&model), "second");
}

#[test]
fn a_store_failure_is_a_banner_rather_than_a_dead_screen() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::StoreFailed("database is locked".to_string()),
    );
    assert_eq!(model.data.error.as_deref(), Some("database is locked"));
    assert!(!model.data.loading);
    assert!(model.running);
}

#[test]
fn a_toast_expires_on_ticks_because_update_has_no_clock() {
    let mut model = loaded();
    press(&mut model, 't');
    assert!(model.status.toast.is_some());

    for _ in 0..criax_tui::model::Toast::LIFETIME {
        update(&mut model, Msg::Tick(now()));
    }
    assert!(model.status.toast.is_none());
}

#[test]
fn quitting_stops_the_runtime_and_says_so_once() {
    let mut model = loaded();
    let effects = press(&mut model, 'q');
    assert!(!model.running);
    assert_eq!(effects, vec![Effect::Quit]);
}

#[test]
fn a_key_release_does_not_move_the_cursor_twice() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::Key(KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Release,
        )),
    );
    assert_eq!(selected_title(&model), "first");
    assert_eq!(
        Key::from_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
        Some(Key::char('j'))
    );
}
