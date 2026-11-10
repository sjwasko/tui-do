//! `update` driven by message sequences.
//!
//! No terminal is involved, which is the point of keeping the layer pure: every one of
//! these would need a pty and a screen scrape in the architecture tui-do replaces.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_do_core::config::columns::ColumnLayout;
use tui_do_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId, User, UserId};
use tui_do_core::store::{Mutation, ProjectCounts, QueueHealth, Subject, TaskCount, TaskOrder};
use tui_do_core::sync::{Phase, PullReport, PushReport, Stage, SyncReport};
use tui_do_core::{Config, SyncEvent};
use tui_do_ui::keymap::Key;
use tui_do_ui::modal::{Candidate, LabelsState, Modal, ModalView, Pick, PickerKind, PickerState};
use tui_do_ui::model::{Focus, Level, PaneState, SyncStatus};
use tui_do_ui::query::Scope;
use tui_do_ui::sidebar::SidebarTarget;
use tui_do_ui::update::{reload_everything, update};
use tui_do_ui::{Effect, Model, Msg};

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
}

/// The same instant, carrying an offset, which is what the model holds.
fn here() -> tui_do_core::ZonedTime {
    tui_do_core::zone::pinned(now().fixed_offset())
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
    let mut model = Model::new(&Config::example(), Scope::All, here(), (160, 40));
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

fn loaded_query(effects: &[Effect]) -> Option<tui_do_ui::QueryId> {
    effects.iter().find_map(|effect| match effect {
        Effect::LoadTasks { id, .. } => Some(*id),
        _ => None,
    })
}

#[test]
fn a_first_paint_asks_for_everything_it_shows() {
    let mut model = Model::new(&Config::example(), Scope::All, here(), (160, 40));
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
    press(&mut model, 'z');
    let effects = press(&mut model, 'd');
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
    let mut model = Model::new(&Config::example(), Scope::All, here(), (100, 10));
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
    press(&mut model, 'a');
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
    // same toast, as pressing `z d` -- one implementation, not two.
    let mut by_key = loaded();
    press(&mut by_key, 'z');
    let key_effects = press(&mut by_key, 'd');

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
    assert!(titles.contains(&"Sync changes"));
    assert!(titles.contains(&"Sync everything"));
    // The motions by name, not by prefix: "Move to another project" is a command and
    // starts the same way, and a prefix test would have called it a motion.
    assert!(!titles.contains(&"Move down"));
    assert!(!titles.contains(&"Move up"));
    assert!(titles.contains(&"Move to another project"));
    assert!(!titles.contains(&"Run a command by name"));

    let sync = picker
        .candidates
        .iter()
        .find(|candidate| candidate.title == "Sync changes")
        .expect("sync is offered");
    assert_eq!(sync.hint, "r / F5");
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
        Effect::Apply { mutation, .. } => Some(mutation),
        _ => None,
    })
}

#[test]
fn quick_add_builds_the_task_the_syntax_describes() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::LabelsLoaded(vec![Label {
            id: LabelId(4),
            title: "urgent".to_string(),
            ..Label::default()
        }]),
    );

    press(&mut model, 'n');
    for c in "Call the VA *urgent !3 +Personal".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    match applied(&effects).expect("a task was queued") {
        Mutation::CreateTask { task } => {
            assert_eq!(task.title, "Call the VA");
            assert_eq!(task.priority, 3);
            assert_eq!(task.project_id, ProjectId(3), "+Personal named it");
            assert_eq!(task.labels.len(), 1);
            assert_eq!(task.labels[0].id, LabelId(4));
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    // And it is on screen before the store has been told.
    assert_eq!(model.data.tasks.first().unwrap().title, "Call the VA");
    assert!(model.undo.len() == 1, "and it can be taken back");
}

fn user(id: i64, username: &str) -> User {
    User {
        id: UserId(id),
        username: username.to_string(),
        ..User::default()
    }
}

#[test]
fn quick_add_assigns_the_users_it_names() {
    // BUG-34: `@user` was parsed and taken out of the title, then dropped, so every
    // quick-added task was assigned to nobody.
    let mut model = loaded();
    update(
        &mut model,
        Msg::UsersLoaded(vec![user(3, "alice"), user(4, "Ada"), user(5, "ada")]),
    );
    press(&mut model, 'n');
    for c in "Review the PR @alice @Ada @ALICE".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("a task was queued") {
        Mutation::CreateTask { task } => {
            assert_eq!(task.title, "Review the PR");
            let ids: Vec<i64> = task.assignees.iter().map(|u| u.id.get()).collect();
            // Exact case first, so `@Ada` is Ada and not ada; `@ALICE` is alice again,
            // and assigned once.
            assert_eq!(ids, vec![3, 4]);
        }
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn a_task_past_the_servers_limits_is_not_queued() {
    let mut model = loaded();
    press(&mut model, 'n');
    for c in "water the cactus every 1000 years".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(applied(&effects).is_none(), "{effects:?}");
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.starts_with("Not added:"), "{}", toast.text);
}

#[test]
fn a_user_nobody_has_seen_is_reported_rather_than_dropped() {
    let mut model = loaded();
    update(&mut model, Msg::UsersLoaded(vec![user(3, "alice")]));
    press(&mut model, 'n');
    for c in "Thing @alice @nobody".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("the task was still created") {
        Mutation::CreateTask { task } => {
            assert_eq!(task.title, "Thing");
            assert_eq!(task.assignees.len(), 1);
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(
        toast.text.contains("no user called nobody"),
        "{}",
        toast.text
    );
}

#[test]
fn a_task_with_no_project_named_lands_where_you_are_looking() {
    let mut model = loaded();
    press(&mut model, ':');
    for c in "project".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    for c in "personal".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.query.scope, Scope::Project(ProjectId(3)));

    press(&mut model, 'n');
    for c in "Something".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("a task was queued") {
        Mutation::CreateTask { task } => assert_eq!(task.project_id, ProjectId(3)),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn a_label_that_does_not_exist_is_reported_rather_than_dropped() {
    // It is asked about first now — see `an_unknown_label_in_quick_add_asks_before_it_
    // creates` — but the answer "no" still has to land the task and still has to say what
    // was left off it, which is what this test has always been about.
    let mut model = loaded();
    press(&mut model, 'n');
    for c in "Thing *nosuchlabel".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    let effects = press(&mut model, 'n');
    match applied(&effects).expect("the task was still created") {
        Mutation::CreateTask { task } => assert!(task.labels.is_empty()),
        other => panic!("wrong mutation: {other:?}"),
    }
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("nosuchlabel"), "{}", toast.text);
}

#[test]
fn an_empty_quick_add_creates_nothing() {
    let mut model = loaded();
    press(&mut model, 'n');
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(applied(&effects).is_none());
    assert!(model.undo.is_empty());
}

#[test]
fn marking_done_shows_before_it_is_stored() {
    let mut model = loaded();
    assert!(!model.selected_task().unwrap().done);

    let first = model.selected_task().unwrap().id;
    let effects = press(&mut model, 't');

    // The frame after the keystroke already shows it, without waiting for the store: in a
    // list that hides done tasks, "shows it" means the row has left, the same way `x`
    // leaves. The stamped `done_at` rides along in the mutation below.
    assert!(!model.data.tasks.iter().any(|task| task.id == first));

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
fn deleting_takes_the_row_out_and_moves_the_cursor_on() {
    let mut model = loaded();
    press(&mut model, 'j');
    assert_eq!(selected_title(&model), "second");

    let effects = press(&mut model, 'x');
    assert_eq!(model.data.tasks.len(), 2, "gone from the list at once");
    assert_eq!(
        selected_title(&model),
        "third",
        "the cursor took the row that slid up, not nothing"
    );
    match applied(&effects).expect("a delete was queued") {
        Mutation::DeleteTask { before } => assert_eq!(before.title, "second"),
        other => panic!("wrong mutation: {other:?}"),
    }
    assert!(model
        .status
        .toast
        .as_ref()
        .is_some_and(|toast| toast.text.contains("U to undo")));
}

#[test]
fn deleting_the_last_row_falls_back_to_the_one_above() {
    let mut model = loaded();
    press(&mut model, 'G');
    assert_eq!(selected_title(&model), "third");
    press(&mut model, 'x');
    assert_eq!(selected_title(&model), "second");
}

#[test]
fn undoing_a_delete_re_creates_the_task() {
    let mut model = loaded();
    press(&mut model, 'x');
    assert_eq!(model.data.tasks.len(), 2);

    let effects = press(&mut model, 'U');
    assert_eq!(model.data.tasks.len(), 3);
    // Vikunja has no undelete, so the inverse of a delete is a create -- which is why the
    // task comes back with a new id once the server has seen it.
    match applied(&effects).expect("a create was queued") {
        Mutation::CreateTask { task } => assert_eq!(task.title, "first"),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn a_task_key_with_nothing_selected_says_so() {
    let mut model = loaded();
    let id = model.query_id;
    update(&mut model, Msg::TasksLoaded { id, tasks: vec![] });

    for key in ['d', 'x'] {
        model.status.toast = None;
        let effects = press(&mut model, key);
        assert!(applied(&effects).is_none());
        let toast = model.status.toast.as_ref().expect("the user is told");
        assert!(toast.text.contains("No task selected"), "{}", toast.text);
    }
}

#[test]
fn a_task_key_works_from_whichever_pane_has_focus() {
    // `d` was list-only, so it did nothing at all with the sidebar focused -- which
    // reads as a broken key rather than a scoped one.
    let mut model = loaded();
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);

    let before = model.data.tasks.len();
    let effects = press(&mut model, 't');
    assert!(applied(&effects).is_some());
    assert_eq!(
        model.data.tasks.len(),
        before - 1,
        "the key reached the list"
    );
}

#[test]
fn undo_queues_the_inverse_like_any_other_change() {
    let mut model = loaded();
    let first = model.selected_task().unwrap().clone();
    press(&mut model, 't');
    assert_eq!(model.undo.len(), 1);

    let effects = press(&mut model, 'U');
    // The row left the list when it was marked done, so what an undo restores is checked
    // in the mutation it queues. The runtime reloads after every `Effect::Apply`, which
    // is what puts the row back on screen.
    match applied(&effects).expect("undo queued a write") {
        Mutation::UpdateTask { after, .. } => {
            assert_eq!(after.id, first.id);
            assert!(!after.done, "undo un-does the tick");
        }
        other => panic!("wrong mutation: {other:?}"),
    }
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
    press(&mut model, 't');
    press(&mut model, 'U');
    assert_eq!(model.redo.len(), 1);

    let effects = update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
    );
    match applied(&effects).expect("redo queued a write") {
        Mutation::UpdateTask { after, .. } => assert!(after.done),
        other => panic!("wrong mutation: {other:?}"),
    }
    assert!(model.redo.is_empty());

    press(&mut model, 'U');
    press(&mut model, 't');
    assert!(model.redo.is_empty(), "a new edit clears what was undone");
}

#[test]
fn undo_goes_back_as_far_as_the_session_does() {
    let mut model = loaded();
    let before = model.data.tasks.len();
    assert!(before >= 3, "the fixture needs three tasks to tick off");
    // No `j` between them: `d` advances to the next row itself, the way `x` does, so
    // three presses tick off three different tasks.
    for _ in 0..3 {
        press(&mut model, 't');
    }
    assert_eq!(model.undo.len(), 3);
    assert_eq!(model.data.tasks.len(), before - 3);

    // Each undo queues the inverse write. The rows return with the reload the runtime
    // fires after every `Effect::Apply`, not from here.
    for _ in 0..3 {
        let effects = press(&mut model, 'U');
        assert!(applied(&effects).is_some());
    }
    assert!(model.undo.is_empty());
    assert_eq!(model.redo.len(), 3);

    // And the bottom of the stack says so rather than doing something arbitrary.
    let effects = press(&mut model, 'U');
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
    press(&mut model, 't');
    // The runtime sends this once the write has landed.
    let effects = update(&mut model, Msg::Reload);
    assert!(loaded_query(&effects).is_some());
    assert!(effects.contains(&Effect::LoadCounts));
}

#[test]
fn done_tasks_are_hidden_until_asked_for() {
    let mut model = loaded();
    assert_eq!(model.query.filter().done, Some(false));

    press(&mut model, 'z');
    press(&mut model, 'd');
    assert_eq!(model.query.filter().done, None);
    assert!(model.status.toast.is_some());

    press(&mut model, 'z');
    press(&mut model, 'd');
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
fn a_full_pull_counts_its_tasks_beside_the_open_ones() {
    // `R` stores done tasks too, so its running count reaches every task ever made and
    // reads as wrong next to a sidebar showing a tenth of that. The open figure says why.
    let mut model = loaded();
    let mut by_project = std::collections::HashMap::new();
    by_project.insert(
        ProjectId(1),
        TaskCount {
            open: 170,
            done: 3000,
        },
    );
    by_project.insert(ProjectId(2), TaskCount { open: 8, done: 774 });
    update(
        &mut model,
        Msg::CountsLoaded(ProjectCounts {
            by_project,
            ..ProjectCounts::default()
        }),
    );
    update(&mut model, Msg::Sync(SyncEvent::Started(Phase::Pull)));
    update(
        &mut model,
        Msg::Sync(SyncEvent::Progress {
            stage: Stage::Tasks,
            stored: 3952,
            pages: Some(80),
        }),
    );
    assert_eq!(
        model.status.sync,
        SyncStatus::Working {
            detail: "3952 tasks (178 open)".to_string()
        }
    );
}

#[test]
fn a_first_pull_counts_its_tasks_alone() {
    // An empty store has no open figure to offer, and "(0 open)" would be a claim.
    let mut model = loaded();
    update(&mut model, Msg::CountsLoaded(ProjectCounts::default()));
    update(
        &mut model,
        Msg::Sync(SyncEvent::Progress {
            stage: Stage::Tasks,
            stored: 50,
            pages: Some(80),
        }),
    );
    assert_eq!(
        model.status.sync,
        SyncStatus::Working {
            detail: "50 tasks".to_string()
        }
    );
}

#[test]
fn a_failure_after_progress_leaves_the_failure_on_screen() {
    // A pass that errors ends with `Failed` and nothing else, so `Failed` must replace
    // whatever "sending" a progress event put there. The runtime sends it only after the
    // forwarder has drained, so no progress can arrive behind it.
    let mut model = loaded();
    update(&mut model, Msg::Sync(SyncEvent::Started(Phase::Push)));
    update(&mut model, Msg::Sync(SyncEvent::Sending { remaining: 40 }));
    update(
        &mut model,
        Msg::Sync(SyncEvent::Failed {
            phase: Phase::Pull,
            message: "connection refused".to_string(),
        }),
    );
    assert_eq!(
        model.status.sync,
        SyncStatus::Failed {
            message: "connection refused".to_string()
        }
    );
}

#[test]
fn a_long_drain_counts_down_as_it_sends() {
    // BUG-21: the queued count only moved when a pass ended, so a 268-second drain showed
    // 1781 queued the whole way through and read as a hang.
    let mut model = loaded();
    update(
        &mut model,
        Msg::PendingLoaded(QueueHealth {
            queued: 1781,
            failing: 3,
            ..QueueHealth::default()
        }),
    );
    update(&mut model, Msg::Sync(SyncEvent::Started(Phase::Push)));
    assert_eq!(
        model.status.sync,
        SyncStatus::Working {
            detail: "sending, 1781 queued".to_string()
        },
        "the start of a push and its progress say it the same way"
    );

    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Sending { remaining: 1203 }),
    );
    assert!(
        effects.is_empty(),
        "progress is news, not a reason to reload"
    );
    assert_eq!(model.status.queued, 1203);
    assert_eq!(
        model.status.failing, 3,
        "failing still fits inside what is left"
    );
    assert_eq!(
        model.status.sync,
        SyncStatus::Working {
            detail: "sending, 1203 queued".to_string()
        }
    );

    update(&mut model, Msg::Sync(SyncEvent::Sending { remaining: 2 }));
    assert_eq!(model.status.queued, 2);
    assert_eq!(
        model.status.failing, 2,
        "more failing than queued is a state the status line cannot mean"
    );
}

#[test]
fn a_created_task_learns_the_id_the_server_gave_it() {
    // B1, found by driving it: create a task, watch it appear in the web UI, then press
    // `d` on it and get `404 This task does not exist` -- about the task just created.
    //
    // A created task holds a provisional, negative id until the server names it. The
    // store swaps it, but an edit asks for a push-only pass, which ends in `Pushed` and
    // does not reload -- so the interface went on holding -1 and sent `POST /tasks/-1`.
    let mut model = loaded();
    let provisional = TaskId(-1);
    let created = task(-1, "new task");

    // The state `apply_locally` leaves behind after `a`: at the top, selected, with its
    // undo pushed.
    model.data.tasks.insert(0, created.clone());
    model.list.selected = Some(provisional);
    model.undo.push(Mutation::DeleteTask {
        before: Box::new(created),
    });

    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Task(provisional),
            assigned: Subject::Task(TaskId(3901)),
        }),
    );

    assert_eq!(model.data.tasks[0].id, TaskId(3901), "the row on screen");
    assert_eq!(
        model.list.selected,
        Some(TaskId(3901)),
        "the selection, or the cursor jumps off the task just created"
    );
    match model.undo.last().expect("a create leaves an undo") {
        Mutation::DeleteTask { before } => assert_eq!(
            before.id,
            TaskId(3901),
            "`u` after a create must not ask the server to delete an id it never had"
        ),
        other => panic!("the create's inverse is a delete, not {other:?}"),
    }
    assert!(
        loaded_query(&effects).is_some(),
        "the store now holds the server's own row -- identifier, index, created"
    );
}

#[test]
fn an_edit_after_a_create_is_sent_against_the_id_the_server_knows() {
    // The failure B1 actually saw, end to end: adopt, then press `d`.
    let mut model = loaded();
    model.data.tasks.insert(0, task(-1, "new task"));
    model.list.selected = Some(TaskId(-1));

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Task(TaskId(-1)),
            assigned: Subject::Task(TaskId(3901)),
        }),
    );

    let effects = press(&mut model, 't');
    let applied = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Apply { mutation, .. } => Some(mutation),
            _ => None,
        })
        .expect("`d` queues a change");
    match applied {
        Mutation::UpdateTask { before, after } => {
            assert_eq!(after.id, TaskId(3901), "sent against the server's id");
            assert_eq!(before.id, TaskId(3901), "and rolls back to the same row");
            assert!(after.done, "and it is the done toggle");
        }
        other => panic!("`d` is an update, not {other:?}"),
    }
}

#[test]
fn an_adoption_leaves_a_task_it_does_not_name_alone() {
    let mut model = loaded();
    model.data.tasks.insert(0, task(-1, "new task"));
    model.list.selected = Some(TaskId(2));

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Task(TaskId(-9)),
            assigned: Subject::Task(TaskId(4000)),
        }),
    );

    assert_eq!(
        model.data.tasks[0].id,
        TaskId(-1),
        "a different create waits"
    );
    assert_eq!(
        model.list.selected,
        Some(TaskId(2)),
        "the selection stays put"
    );
}

#[test]
fn a_created_label_learns_the_id_the_server_gave_it() {
    // The label half of B1. A label created here carries a provisional negative id until
    // the server names it, and the interface holds more copies of a label than it does of
    // a task: two independent snapshots, the filter, and both history stacks.
    let mut model = with_labels(vec![label(-1, "next")], vec![label(-1, "next")]);
    model.query.scope = Scope::Label(LabelId(-1));
    model.undo.push(Mutation::DetachLabel {
        task: TaskId(1),
        label: Box::new(label(-1, "next")),
    });
    model.redo.push(Mutation::AttachLabel {
        task: TaskId(1),
        label: Box::new(label(-1, "next")),
    });

    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );

    assert_eq!(
        model.data.labels[0].id,
        LabelId(41),
        "the labels snapshot, which the picker lists and quick-add resolves against"
    );
    assert_eq!(
        model.data.tasks[0].labels[0].id,
        LabelId(41),
        "the second snapshot: the copy carried on the task, which draws the row chip"
    );
    assert_eq!(
        model.query.scope,
        Scope::Label(LabelId(41)),
        "the filter the list is drawn from, or the next reload asks for an id nothing has"
    );
    match model.undo.last().expect("the undo stack kept its entry") {
        Mutation::DetachLabel { label, .. } => assert_eq!(
            label.id,
            LabelId(41),
            "`u` must not detach an id the server has never seen"
        ),
        other => panic!("the undo stack was rewritten into {other:?}"),
    }
    match model.redo.last().expect("the redo stack kept its entry") {
        Mutation::AttachLabel { label, .. } => {
            assert_eq!(label.id, LabelId(41), "and neither must `ctrl-r`")
        }
        other => panic!("the redo stack was rewritten into {other:?}"),
    }
    assert!(
        loaded_query(&effects).is_some(),
        "the store now holds the server's own row, and the task links moved with it"
    );
    assert!(
        effects.contains(&Effect::LoadLabels),
        "including the label row itself"
    );
}

#[test]
fn adopting_a_label_reaches_the_open_form_that_created_it() {
    // The form is open at exactly the moment this happens -- the user created the label
    // from it -- and it holds cloned labels and a list of ticked ids. Leaving it behind
    // means the next tick sends an id no server has seen.
    let mut model = with_labels(vec![label(-1, "next")], vec![]);
    press(&mut model, 'l');
    press(&mut model, ' ');

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );

    let Some(Modal::Labels(state)) = model.modals.last() else {
        panic!("the form closed: {:?}", model.modals);
    };
    assert_eq!(state.labels[0].id, LabelId(41), "the cloned label");
    assert_eq!(state.chosen, vec![LabelId(41)], "and the ticked ids");
}

#[test]
fn an_attach_from_the_form_that_created_a_label_names_the_id_the_server_gave_it() {
    // The same failure end to end, which is what actually reaches the user: tick the
    // label just created, the server names it while the form is still open, press Enter.
    // A form left on the provisional id resolves nothing and queues nothing at all.
    let mut model = with_labels(vec![label(-1, "next")], vec![]);
    press(&mut model, 'l');
    press(&mut model, ' ');

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );
    let effects = press_code(&mut model, KeyCode::Enter);

    match applied(&effects).expect("ticking a label queues an attach") {
        Mutation::AttachLabel { label, .. } => assert_eq!(
            label.id,
            LabelId(41),
            "`PUT /tasks/1/labels/-1` is a 404 about the label just created"
        ),
        other => panic!("wrong mutation: {other:?}"),
    }
}

/// A Ctrl-chord, which is how the label form's create key arrives.
fn press_ctrl(model: &mut Model, c: char) -> Vec<Effect> {
    update(
        model,
        Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
    )
}

/// The open label form, or a panic naming what is on the stack instead.
fn label_form(model: &Model) -> &tui_do_ui::modal::LabelsState {
    match model.modals.last() {
        Some(Modal::Labels(state)) => state,
        other => panic!("the label form is not open: {other:?}"),
    }
}

#[test]
fn the_label_form_opens_with_nothing_in_it_rather_than_refusing() {
    // It used to toast "No labels exist yet" and not open -- an apology for a form that
    // could not be filled. An empty box is now where the first label gets typed.
    let mut model = with_labels(Vec::new(), Vec::new());
    press(&mut model, 'l');
    assert!(label_form(&model).labels.is_empty());
    assert!(
        model.status.toast.is_none(),
        "nothing to apologise for: {:?}",
        model.status.toast
    );
}

#[test]
fn creating_from_the_label_form_queues_it_and_leaves_the_form_open() {
    let mut model = with_labels(Vec::new(), Vec::new());
    press(&mut model, 'l');
    for c in "next".chars() {
        press(&mut model, c);
    }
    let effects = press_ctrl(&mut model, 'n');

    match applied(&effects).expect("C-n queues a create") {
        Mutation::CreateLabel { label } => {
            assert_eq!(label.title, "next");
            assert_eq!(
                label.id,
                LabelId(0),
                "the store allocates the provisional id, not the interface"
            );
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    assert!(
        effects.contains(&Effect::LoadLabels),
        "without the reload the form never learns the id: {effects:?}"
    );
    assert!(
        matches!(model.modals.last(), Some(Modal::Labels(_))),
        "the user is still deciding this task's labels"
    );
    assert!(
        model.undo.is_empty(),
        "a create has no inverse -- this feature ships no delete"
    );
}

#[test]
fn a_label_created_from_the_form_comes_back_ticked_and_attaches_under_the_server_s_id() {
    // The whole path a human drives: `l` on a task with no labels anywhere, type a name,
    // C-n, the store's reload names it, the server names it again, Enter. Every link in
    // that chain has to carry the id forward or the tick the user is looking at queues
    // nothing at all.
    let mut model = with_labels(Vec::new(), Vec::new());
    press(&mut model, 'l');
    for c in "next".chars() {
        press(&mut model, c);
    }
    press_ctrl(&mut model, 'n');

    // The runtime spawns every effect on its own task, so the reload this queued races
    // the write and can answer from before it. The form waits rather than giving up:
    // `Effect::Apply` sends `Msg::Reload` once the write has landed, and that reload is
    // the one that names it.
    update(&mut model, Msg::LabelsLoaded(Vec::new()));
    assert!(
        label_form(&model).chosen.is_empty(),
        "nothing to tick yet, and nothing invented"
    );

    // The store applied the create as it queued it, so the reload names the provisional.
    update(&mut model, Msg::LabelsLoaded(vec![label(-1, "next")]));
    let form = label_form(&model);
    assert_eq!(form.labels, vec![label(-1, "next")]);
    assert_eq!(form.chosen, vec![LabelId(-1)], "created means ticked");

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );
    // `set_labels` resolves the ticks against `model.data.labels`, which the adoption
    // renumbered too.
    update(&mut model, Msg::LabelsLoaded(vec![label(41, "next")]));
    let effects = press_code(&mut model, KeyCode::Enter);

    match applied(&effects).expect("the tick queues an attach") {
        Mutation::AttachLabel { label, .. } => assert_eq!(label.id, LabelId(41)),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn a_reload_does_not_tick_a_label_the_form_did_not_ask_for() {
    // A background sync reloads labels while the form is open. Absorbing everything
    // would reshuffle the list under the user, and ticking it would put a label somebody
    // created on another box onto this task.
    let mut model = with_labels(vec![label(1, "urgent")], Vec::new());
    press(&mut model, 'l');
    update(
        &mut model,
        Msg::LabelsLoaded(vec![label(1, "urgent"), label(2, "elsewhere")]),
    );
    let form = label_form(&model);
    assert_eq!(form.labels, vec![label(1, "urgent")]);
    assert!(form.chosen.is_empty());
}

#[test]
fn adopting_a_label_reaches_a_picker_and_an_open_edit_form() {
    // Two more holders on the modal stack. The picker's candidate becomes the list's
    // filter, and the edit form's `before` is what `apply_edit` computes the attach and
    // detach sets against -- so a stale one queues a detach of an id the server never had.
    let mut model = with_labels(vec![label(-1, "next")], vec![label(-1, "next")]);
    press(&mut model, 'e');
    model.modals.insert(
        0,
        Modal::Picker(PickerState::new(
            PickerKind::Label,
            vec![Candidate::new(Pick::Label(LabelId(-1)), "next")],
        )),
    );

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );

    let Some(Modal::Picker(picker)) = model.modals.first() else {
        panic!("the picker went missing: {:?}", model.modals);
    };
    assert_eq!(picker.candidates[0].pick, Pick::Label(LabelId(41)));
    let Some(Modal::Edit(edit)) = model.modals.last() else {
        panic!("the edit form went missing: {:?}", model.modals);
    };
    assert_eq!(edit.before.labels[0].id, LabelId(41));
}

#[test]
fn an_adoption_leaves_a_label_it_does_not_name_alone() {
    // Every holder `adopt_label` reaches, including the three on the modal stack: a
    // renumbering that swept up an id it was not asked about would send the *next* write
    // to a label the server never named, and the modal holders are the ones a
    // whole-stack swap most easily overreaches into.
    let mut model = with_labels(vec![label(-1, "next")], vec![label(-1, "next")]);
    model.query.scope = Scope::Label(LabelId(-1));
    // The edit form first, because `e` only reaches `act` with nothing on the stack. Its
    // `before` is what `apply_edit` computes attach and detach sets against.
    press(&mut model, 'e');
    // Then the form the label was made in, opened with the task's own label already
    // ticked -- which is the `chosen` entry this test is here to leave alone.
    model.modals.push(Modal::Labels(LabelsState::new(
        model.data.labels.clone(),
        vec![LabelId(-1)],
    )));
    model.modals.insert(
        0,
        Modal::Picker(PickerState::new(
            PickerKind::Label,
            vec![Candidate::new(Pick::Label(LabelId(-1)), "next")],
        )),
    );

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-9)),
            assigned: Subject::Label(LabelId(50)),
        }),
    );

    assert_eq!(
        model.data.labels[0].id,
        LabelId(-1),
        "a different create waits"
    );
    assert_eq!(model.data.tasks[0].labels[0].id, LabelId(-1));
    assert_eq!(model.query.scope, Scope::Label(LabelId(-1)));
    let Some(Modal::Picker(picker)) = model.modals.first() else {
        panic!("the picker went missing: {:?}", model.modals);
    };
    assert_eq!(picker.candidates[0].pick, Pick::Label(LabelId(-1)));
    let Some(Modal::Edit(edit)) = model.modals.get(1) else {
        panic!("the edit form went missing: {:?}", model.modals);
    };
    assert_eq!(edit.before.labels[0].id, LabelId(-1));
    let form = label_form(&model);
    assert_eq!(form.labels[0].id, LabelId(-1));
    assert_eq!(form.chosen, vec![LabelId(-1)]);
}

/// The open label form, or a panic naming what is on the stack instead.
fn label_edit_form(model: &Model) -> &tui_do_ui::modal::LabelEditState {
    match model.modals.last() {
        Some(Modal::LabelEdit(state)) => state,
        other => panic!("the label edit form is not open: {other:?}"),
    }
}

/// Type `text` into whatever has the keyboard, clearing the field first.
fn retype(model: &mut Model, text: &str) {
    press_ctrl(model, 'u');
    for c in text.chars() {
        press(model, c);
    }
}

#[test]
fn renaming_a_label_from_the_task_form_carries_both_fields_and_keeps_the_ticks() {
    // The whole path a human drives, through the keys they actually press: `l`, `C-e`,
    // retype the title, Tab, retype the colour, Enter. One request and one undo step,
    // because a partial body clears what it omits -- and the form underneath is still
    // open, still holding the ticks, now showing the new name.
    let mut model = with_labels(
        vec![
            Label {
                id: LabelId(1),
                title: "urgent".to_string(),
                hex_color: "4287f5".to_string(),
                ..Label::default()
            },
            label(2, "next"),
        ],
        vec![label(1, "urgent")],
    );
    press(&mut model, 'l');
    assert_eq!(label_form(&model).chosen, vec![LabelId(1)]);

    press_ctrl(&mut model, 'e');
    assert_eq!(label_edit_form(&model).label.id, LabelId(1));
    assert_eq!(
        model.modals.len(),
        2,
        "the ticks are still underneath: {:?}",
        model.modals
    );

    retype(&mut model, "critical");
    press_code(&mut model, KeyCode::Tab);
    retype(&mut model, "e8384f");
    let effects = press_code(&mut model, KeyCode::Enter);

    match applied(&effects).expect("the rename is queued") {
        Mutation::UpdateLabel { before, after } => {
            assert_eq!(before.title, "urgent");
            assert_eq!(before.hex_color, "4287f5");
            assert_eq!(after.title, "critical");
            assert_eq!(after.hex_color, "e8384f");
            assert_eq!(
                after.id,
                LabelId(1),
                "and it names the label it opened over"
            );
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    // Undoable, unlike a create: the inverse is the label as it was.
    assert_eq!(model.undo.len(), 1);

    // The form the user was in is back on top, showing what they just renamed -- it
    // holds its own clones, which no reload reaches.
    let form = label_form(&model);
    assert_eq!(form.labels[0].title, "critical");
    assert_eq!(
        form.chosen,
        vec![LabelId(1)],
        "the tick survived the detour"
    );
    // And the row the list draws its chips from moved with it.
    assert_eq!(model.data.tasks[0].labels[0].title, "critical");
    assert_eq!(model.data.labels[0].title, "critical");
}

/// The same label, recoloured -- what another box's edit looks like when it arrives.
fn coloured(id: i64, title: &str, hex: &str) -> Label {
    Label {
        id: LabelId(id),
        title: title.to_string(),
        hex_color: hex.to_string(),
        ..Label::default()
    }
}

#[test]
fn a_field_the_user_did_not_touch_is_not_claimed_as_their_edit() {
    // `Label::merge_onto` decides whether a field is the user's by comparing `before`
    // with `after`, so `before` has to be what they started from. Re-reading it out of
    // the pool at Enter reads a colour another box changed under the open form as *this*
    // user's edit: the merge would then overwrite the other box's colour with a value
    // this user never typed, and toast `Overwrote` blaming them for it.
    let mut model = with_labels(vec![coloured(1, "urgent", "4287f5")], Vec::new());
    press(&mut model, 'l');
    press_ctrl(&mut model, 'e');

    // The other box's recolour lands while the form is open. `LabelEdit` is deliberately
    // not refreshed from it -- the fields are mid-typing -- so the two now disagree.
    update(
        &mut model,
        Msg::LabelsLoaded(vec![coloured(1, "urgent", "e8384f")]),
    );
    retype(&mut model, "critical");
    let effects = press_code(&mut model, KeyCode::Enter);

    match applied(&effects).expect("the rename is queued") {
        Mutation::UpdateLabel { before, after } => {
            assert_eq!(before.hex_color, "4287f5", "what the user started from");
            assert_eq!(
                after.hex_color, before.hex_color,
                "the colour is untouched, so the merge keeps the server's"
            );
            assert_eq!(after.title, "critical");
        }
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn enter_over_a_rename_that_landed_underneath_queues_nothing() {
    // The other half of the same fault. With `before` re-read from the pool, an Enter
    // over a form nobody typed in has `after.title` (the form's, old) differing from
    // `before.title` (the pool's, new), so the no-change return does not fire and the
    // write reverts the other box's rename.
    let mut model = with_labels(vec![label(1, "urgent")], Vec::new());
    press(&mut model, 'l');
    press_ctrl(&mut model, 'e');
    update(&mut model, Msg::LabelsLoaded(vec![label(1, "next steps")]));

    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(applied(&effects).is_none(), "nothing was typed here");
    assert!(model.undo.is_empty());
    assert_eq!(model.data.labels[0].title, "next steps", "and it stands");
}

#[test]
fn a_reload_carries_a_rename_into_an_open_picker() {
    // The picker's counterpart to `LabelsState::refresh`. Its candidates are a copy of
    // the title made when it opened, and nothing else rewrites them -- so a rename from
    // another box, or the store rolling this box's own rename back, would leave `g l`
    // offering a name that no longer exists.
    let mut model = with_labels(vec![label(1, "urgent"), label(2, "next")], Vec::new());
    press(&mut model, 'g');
    press(&mut model, 'a');

    update(
        &mut model,
        Msg::LabelsLoaded(vec![label(1, "critical"), label(2, "next")]),
    );

    let Some(Modal::Picker(picker)) = model.modals.last() else {
        panic!("the picker went missing: {:?}", model.modals);
    };
    // Sorted by title when it opened, so `next` is first and `urgent` second. The order
    // is deliberately left as it was: re-sorting would move the highlight under someone
    // mid-keystroke.
    assert_eq!(picker.candidates[1].title, "critical");
    assert_eq!(picker.candidates[1].pick, Pick::Label(LabelId(1)));
    assert_eq!(picker.candidates[0].title, "next");
}

#[test]
fn a_label_only_a_task_carries_can_still_be_renamed() {
    // The `l` form lists labels the task holds that the pool has not caught up with --
    // a pull stores labels and tasks in separate passes -- and offers `C-e` on them like
    // any other row. The store reads a task's labels by joining the labels table, so the
    // row is there; only this crate's snapshot is behind, and the write lands on the same
    // row either way.
    let mut model = with_labels(Vec::new(), vec![coloured(2, "backend", "1973ff")]);
    press(&mut model, 'l');
    press_ctrl(&mut model, 'e');

    assert_eq!(label_edit_form(&model).label.id, LabelId(2));
    retype(&mut model, "infra");
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("the rename is queued") {
        Mutation::UpdateLabel { before, after } => {
            assert_eq!(before.title, "backend");
            assert_eq!(before.hex_color, "1973ff", "carried, not defaulted");
            assert_eq!(after.title, "infra");
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    assert_eq!(model.data.tasks[0].labels[0].title, "infra");
}

#[test]
fn a_label_that_is_nowhere_on_this_box_says_so_rather_than_opening_a_form() {
    // A pull that dropped it, or another box's delete, landing between the list being
    // drawn and the key being pressed.
    let mut model = with_labels(vec![label(1, "urgent")], Vec::new());
    press(&mut model, 'g');
    press(&mut model, 'a');
    update(&mut model, Msg::LabelsLoaded(Vec::new()));
    press_ctrl(&mut model, 'e');

    assert!(
        matches!(model.modals.last(), Some(Modal::Picker(_))),
        "no form opened: {:?}",
        model.modals
    );
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("no longer here"), "{}", toast.text);
}

#[test]
fn a_rejected_rename_puts_the_old_title_back_in_the_list_that_asked_for_it() {
    // The list the user renamed from is still open, and it holds its own clones --
    // `apply_locally` wrote the optimistic title into them. The store rolls a rejected
    // `UpdateLabel` back and the rejection reloads the pool, so the reload is the only
    // route back: without it the toast says the rename failed while the row underneath
    // still reads `critical`.
    let mut model = with_labels(vec![label(1, "urgent")], vec![label(1, "urgent")]);
    press(&mut model, 'l');
    press_ctrl(&mut model, 'e');
    retype(&mut model, "critical");
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(label_form(&model).labels[0].title, "critical");

    update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(1)),
            kind: "label update".to_string(),
            message: "This label does not exist.".to_string(),
        }),
    );
    // What the store answers with once it has rolled the row back.
    update(&mut model, Msg::LabelsLoaded(vec![label(1, "urgent")]));

    let form = label_form(&model);
    assert_eq!(form.labels[0].title, "urgent");
    assert_eq!(form.chosen, vec![LabelId(1)], "and the tick is left alone");
    assert!(
        model.undo.is_empty(),
        "the inverse of a change that never happened is not an undo"
    );
}

#[test]
fn the_picker_opens_the_same_form_over_the_same_label() {
    // A label edited from `g l` must not be a different thing from a label edited from
    // the task form, so both surfaces send the same submission and open the same modal.
    let mut model = with_labels(vec![label(1, "urgent"), label(2, "next")], Vec::new());
    press(&mut model, 'g');
    press(&mut model, 'a');
    // The candidates are sorted by title, so the second row is `urgent`.
    press_code(&mut model, KeyCode::Down);
    press_ctrl(&mut model, 'e');

    assert_eq!(label_edit_form(&model).label.id, LabelId(1));
    retype(&mut model, "critical");
    let effects = press_code(&mut model, KeyCode::Enter);

    match applied(&effects).expect("the rename is queued") {
        Mutation::UpdateLabel { before, after } => {
            assert_eq!(before.title, "urgent");
            assert_eq!(after.title, "critical");
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    // The picker is still open -- it was `Outcome::Update`, not a submit -- and its
    // candidates are a third copy of the title, which nothing else rewrites.
    let Some(Modal::Picker(picker)) = model.modals.last() else {
        panic!("the picker closed: {:?}", model.modals);
    };
    assert_eq!(picker.candidates[1].title, "critical");
    assert_eq!(
        picker.current().map(|c| c.pick.clone()),
        Some(Pick::Label(LabelId(1)))
    );
}

#[test]
fn a_colour_the_server_would_reject_never_reaches_the_queue() {
    // A server rejection rolls the *whole* mutation back, taking the rename with it --
    // minutes later, over whatever the user has moved on to.
    let mut model = with_labels(vec![label(1, "urgent")], Vec::new());
    press(&mut model, 'l');
    press_ctrl(&mut model, 'e');
    press_code(&mut model, KeyCode::Tab);
    retype(&mut model, "nope");
    let effects = press_code(&mut model, KeyCode::Enter);

    assert!(applied(&effects).is_none(), "nothing was queued");
    assert!(model.undo.is_empty());
    let form = label_edit_form(&model);
    assert!(form.error.is_some(), "and the form says why, still open");

    // Fix it in place and the same Enter goes through.
    retype(&mut model, "e8384f");
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(matches!(
        applied(&effects),
        Some(Mutation::UpdateLabel { .. })
    ));
}

#[test]
fn a_form_closed_without_a_change_queues_nothing() {
    // Enter submits whether or not anything was typed. An `UpdateLabel` that changes
    // nothing is still a read, a merge and a write, so it is a way to overwrite another
    // box's rename with the value already on screen.
    let mut model = with_labels(vec![label(1, "urgent")], Vec::new());
    press(&mut model, 'l');
    press_ctrl(&mut model, 'e');
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(applied(&effects).is_none());
    assert!(model.undo.is_empty());
}

#[test]
fn a_label_named_by_the_server_while_the_form_is_open_is_renamed_under_its_new_id() {
    // `C-n` then `C-e` is two keystrokes, and the id the form is holding is what its
    // submission names. Left at the provisional, the rename queues against `/labels/-1`
    // -- a 404 about the label the server has just created.
    let mut model = with_labels(vec![label(-1, "next")], Vec::new());
    press(&mut model, 'l');
    press_ctrl(&mut model, 'e');
    assert_eq!(label_edit_form(&model).label.id, LabelId(-1));

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );
    update(&mut model, Msg::LabelsLoaded(vec![label(41, "next")]));
    assert_eq!(label_edit_form(&model).label.id, LabelId(41));

    retype(&mut model, "next up");
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("the rename is queued") {
        Mutation::UpdateLabel { after, .. } => assert_eq!(after.id, LabelId(41)),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn a_collision_says_which_thing_was_written_over() {
    // `SyncEvent::Overwrote` has carried a `Subject` since the label mutations landed and
    // nothing read it, so a label collision toasted word for word like a task one --
    // `(title)`, which reads as being about the task on screen. Overwriting somebody on a
    // label is different news: it is worn by every task that carries it.
    let mut model = with_labels(vec![label(1, "urgent")], Vec::new());
    update(
        &mut model,
        Msg::Sync(SyncEvent::Overwrote {
            subject: Subject::Label(LabelId(1)),
            fields: vec!["title".to_string()],
        }),
    );
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("label \"urgent\""), "{}", toast.text);
    assert!(toast.text.contains("(title)"), "{}", toast.text);

    update(
        &mut model,
        Msg::Sync(SyncEvent::Overwrote {
            subject: Subject::Task(TaskId(1)),
            fields: vec!["title".to_string()],
        }),
    );
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("task \"first\""), "{}", toast.text);

    // And a subject this box has never loaded is named by its kind rather than invented.
    update(
        &mut model,
        Msg::Sync(SyncEvent::Overwrote {
            subject: Subject::Label(LabelId(404)),
            fields: vec!["hex_color".to_string()],
        }),
    );
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("a label"), "{}", toast.text);
}

#[test]
fn a_mismatched_adoption_renumbers_nothing() {
    // A create is answered by a create of the same kind, so this pair is a bug in the
    // sync engine. Renumbering a label because a task id matched would hide it.
    let mut model = with_labels(vec![label(-1, "next")], vec![label(-1, "next")]);
    model.data.tasks[0].id = TaskId(-1);

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Task(TaskId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );

    assert_eq!(model.data.labels[0].id, LabelId(-1));
    assert_eq!(model.data.tasks[0].id, TaskId(-1));
}

#[test]
fn asking_to_sync_reads_the_store_before_it_waits_on_the_server() {
    // `tui-do add` writes to the same store, so the task is local before the pull starts.
    // Waiting seventy-eight pages to show a row that is already on disk is the lag this
    // project exists to remove.
    let mut model = loaded();
    let effects = press(&mut model, 'r');

    assert!(
        loaded_query(&effects).is_some(),
        "r must re-read the store on the spot"
    );
    assert!(
        effects.contains(&Effect::LoadCounts),
        "including the counts, which is where a task added elsewhere shows first"
    );
    assert!(
        effects.contains(&Effect::SyncNow),
        "and still ask the server"
    );
}

#[test]
fn the_two_sync_keys_ask_for_different_amounts_of_server() {
    // `r` is a page and `R` is seventy-eight. They are separate keys because only the
    // full one can notice a task deleted in another client, and a user watching for one
    // to disappear has no way to learn that from the outside.
    let mut model = loaded();
    assert!(press(&mut model, 'r').contains(&Effect::SyncNow));
    assert!(press(&mut model, 'R').contains(&Effect::SyncFull));

    // Which one `r` is has to be said somewhere the user will read it, and the toast at
    // the keystroke is the only place they are looking.
    let mut model = loaded();
    press(&mut model, 'r');
    let said = model
        .status
        .toast
        .as_ref()
        .is_some_and(|toast| toast.text.contains('R'));
    assert!(said, "r must name the key that fetches everything");
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
    assert_eq!(model.status.last_sync, Some(here()));
    assert_eq!(model.status.queued, 2);
    assert!(effects.contains(&Effect::LoadCounts));
    assert!(loaded_query(&effects).is_some());
}

#[test]
fn a_rejected_change_is_the_one_sync_event_the_user_is_shown() {
    let mut model = loaded();
    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Task(TaskId(1)),
            kind: "update_task".to_string(),
            message: "This project does not exist.".to_string(),
        }),
    );
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("This project does not exist."));
    // The store rolled the row back before emitting this. Without a reload the toast
    // says "rejected" over a row still showing the change, and nothing else reloads:
    // an edit's push ends at `Pushed`, which returns only `LoadPending`.
    assert!(
        loaded_query(&effects).is_some(),
        "the rolled-back row is still on screen"
    );
    assert!(effects.contains(&Effect::LoadCounts));
}

#[test]
fn a_rejected_label_create_is_taken_off_the_screen_it_is_still_ticked_on() {
    // Reachable only since the `l` form learned to create. The store rolled the
    // provisional label back, but both label snapshots still hold it -- ticked, because
    // creating is what ticked it -- so the user's next Enter queues an `AttachLabel` for
    // an id the server never had, and it fails in turn.
    let mut model = with_labels(Vec::new(), Vec::new());
    press(&mut model, 'l');
    for c in "next".chars() {
        press(&mut model, c);
    }
    press_ctrl(&mut model, 'n');
    update(&mut model, Msg::LabelsLoaded(vec![label(-1, "next")]));
    assert_eq!(label_form(&model).chosen, vec![LabelId(-1)]);

    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(-1)),
            kind: "create_label".to_string(),
            message: "You do not have the right to do this.".to_string(),
        }),
    );

    assert!(
        effects.contains(&Effect::LoadLabels),
        "the label the store rolled back is still in both snapshots: {effects:?}"
    );
    // And the reload has to actually take it away. Asserting only that the effect was
    // queued asserts that the right message was sent, not that the wrong state cannot
    // arise -- and the wrong state is the whole point: a phantom left ticked is queued as
    // an `AttachLabel` by the user's next Enter, for an id the server has never had.
    // `LabelsState::refresh` rewrites the labels the form holds and removes none, so the
    // store's answer is delivered here and the form is asked what it now holds.
    update(&mut model, Msg::LabelsLoaded(Vec::new()));

    assert!(
        model.data.labels.is_empty(),
        "the pool still holds a label the store rolled back"
    );
    assert!(
        label_form(&model).chosen.is_empty(),
        "the phantom is still ticked, so the next Enter attaches an id no server issued: {:?}",
        label_form(&model).chosen
    );
    assert!(
        label_form(&model)
            .labels
            .iter()
            .all(|held| held.id != LabelId(-1)),
        "the phantom is still listed, so the user can tick it again"
    );
}

#[test]
fn a_rejected_label_create_does_not_leave_the_create_key_dead() {
    // `awaiting` is what stops a second C-n queueing a duplicate while the first create
    // is in flight. A rejection means that create is never coming back, so a title left
    // in there makes `creatable` answer `None` for the rest of the form's life -- and a
    // user whose account cannot create labels sees the key work once and then die
    // silently. This is the rejection that lands before any reload named it.
    let mut model = with_labels(Vec::new(), Vec::new());
    press(&mut model, 'l');
    for c in "next".chars() {
        press(&mut model, c);
    }
    press_ctrl(&mut model, 'n');
    assert_eq!(label_form(&model).creatable(), None, "one is in flight");

    update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(-1)),
            kind: "create_label".to_string(),
            message: "You do not have the right to do this.".to_string(),
        }),
    );

    assert_eq!(
        label_form(&model).creatable(),
        Some("next"),
        "the key is dead for the rest of the form's life"
    );
    assert!(
        applied(&press_ctrl(&mut model, 'n')).is_some(),
        "and pressing it does nothing at all"
    );
}

#[test]
fn a_rejected_task_leaves_a_label_form_waiting() {
    // The clear is keyed on the subject's *kind*. A task rejection has nothing to say
    // about a label create still in flight, and dropping the wait would reopen the
    // duplicate window `awaiting` exists to close.
    let mut model = with_labels(Vec::new(), Vec::new());
    press(&mut model, 'l');
    for c in "next".chars() {
        press(&mut model, c);
    }
    press_ctrl(&mut model, 'n');

    update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Task(TaskId(1)),
            kind: "update_task".to_string(),
            message: "This project does not exist.".to_string(),
        }),
    );

    assert_eq!(label_form(&model).creatable(), None);
}

#[test]
fn a_rejected_change_is_not_left_on_the_undo_stack() {
    // The inverse was pushed when the edit was made. The server then refused the edit and
    // the store rolled it back, so the inverse now describes undoing something that never
    // happened -- and `u` would queue a write against a state the server never held.
    let mut model = loaded();
    let effects = press(&mut model, 't');
    assert!(!model.undo.is_empty(), "the edit left an inverse behind");
    let subject = applied(&effects).expect("the toggle was applied").subject();

    update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject,
            kind: "update_task".to_string(),
            message: "This project does not exist.".to_string(),
        }),
    );

    assert!(
        model.undo.iter().all(|m| m.subject() != subject),
        "undo still holds an inverse of a change the server refused"
    );
}

/// Every label id a mutation would hand the store, on any path: the one it sends, and
/// the ones a task it carries holds -- `decompose` turns those into attaches.
fn label_ids(mutation: &Mutation) -> Vec<LabelId> {
    match mutation {
        Mutation::AttachLabel { label, .. }
        | Mutation::DetachLabel { label, .. }
        | Mutation::CreateLabel { label } => vec![label.id],
        Mutation::UpdateLabel { before, after } => vec![before.id, after.id],
        Mutation::CreateTask { task } => task.labels.iter().map(|l| l.id).collect(),
        Mutation::DeleteTask { before } => before.labels.iter().map(|l| l.id).collect(),
        Mutation::UpdateTask { before, after } => before
            .labels
            .iter()
            .chain(after.labels.iter())
            .map(|l| l.id)
            .collect(),
    }
}

/// Task 1 given priority 3, then label `-1` created and attached to it through the form.
fn attached_to_a_provisional() -> Model {
    let mut model = with_labels(Vec::new(), Vec::new());
    press(&mut model, 'p');
    press(&mut model, '3');
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.selected_task().unwrap().priority, 3);

    press(&mut model, 'l');
    for c in "next".chars() {
        press(&mut model, c);
    }
    press_ctrl(&mut model, 'n');
    update(&mut model, Msg::LabelsLoaded(vec![label(-1, "next")]));
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("the tick queues an attach") {
        Mutation::AttachLabel { task, label } => {
            assert_eq!((*task, label.id), (TaskId(1), LabelId(-1)));
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    model
}

fn reject_the_create(model: &mut Model) {
    update(
        model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(-1)),
            kind: "create_label".to_string(),
            message: "You do not have the right to do this.".to_string(),
        }),
    );
}

#[test]
fn an_undo_after_a_rejected_label_create_never_names_its_id() {
    // BUG-27. The attach's inverse is a `DetachLabel` whose subject is the *task*, so a
    // prune by `Subject::Label(-1)` kept it. `u` then sent `DELETE /tasks/1/labels/-1`,
    // and `C-r` a `PUT` with `label_id: -1` -- a 4xx the drain treats as permanent, which
    // discards every other entry queued for task 1.
    let mut model = attached_to_a_provisional();
    reject_the_create(&mut model);

    let mut effects = press(&mut model, 'U');
    effects.extend(press_ctrl(&mut model, 'r'));
    effects.extend(press(&mut model, 'U'));
    effects.extend(press(&mut model, 'U'));

    let queued = all_applied(&effects);
    assert!(!queued.is_empty(), "nothing was undone at all");
    for mutation in queued {
        assert!(
            !label_ids(mutation).contains(&LabelId(-1)),
            "queued a write naming a label whose create was rejected: {mutation:?}"
        );
    }
}

#[test]
fn an_edit_made_before_the_attach_still_undoes_after_the_prune() {
    let mut model = attached_to_a_provisional();
    reject_the_create(&mut model);

    let effects = press(&mut model, 'U');
    match applied(&effects).expect("the priority edit is still undoable") {
        Mutation::UpdateTask { after, .. } => assert_eq!(after.priority, 0),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn an_edit_made_after_the_attach_undoes_without_the_dead_label() {
    // Here the task already carried `-1` when it was edited, so both halves of the
    // inverse hold it. The edit itself is still the user's, so the label is stripped
    // rather than the undo thrown away.
    let mut model = attached_to_a_provisional();
    press(&mut model, 'p');
    press(&mut model, '5');
    press_code(&mut model, KeyCode::Enter);
    reject_the_create(&mut model);

    let effects = press(&mut model, 'U');
    let mutation = applied(&effects).expect("the later edit is still undoable");
    match mutation {
        Mutation::UpdateTask { after, .. } => assert_eq!(after.priority, 3),
        other => panic!("wrong mutation: {other:?}"),
    }
    assert!(
        !label_ids(mutation).contains(&LabelId(-1)),
        "the undo still carries the dead label: {mutation:?}"
    );
}

#[test]
fn undoing_a_delete_made_after_a_rejected_label_create_drops_the_label() {
    // The re-created task is split into a `CreateTask` and one `AttachLabel` per label it
    // carries, so a `-1` left on it reaches the same permanent rejection. The reload the
    // rejection asked for has not answered yet, which is the window a fast `x` lands in.
    let mut model = attached_to_a_provisional();
    reject_the_create(&mut model);
    press(&mut model, 'x');

    let effects = press(&mut model, 'U');
    match applied(&effects).expect("the delete is undoable") {
        Mutation::CreateTask { task } => {
            assert_eq!(task.title, "first");
            assert!(
                task.labels.iter().all(|held| held.id != LabelId(-1)),
                "the re-created task carries the rejected label: {:?}",
                task.labels
            );
        }
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn undoing_a_delete_made_before_a_rejected_label_create_drops_the_label() {
    let mut model = attached_to_a_provisional();
    press(&mut model, 'x');
    reject_the_create(&mut model);

    let effects = press(&mut model, 'U');
    match applied(&effects).expect("the delete is undoable") {
        Mutation::CreateTask { task } => assert!(
            task.labels.iter().all(|held| held.id != LabelId(-1)),
            "the re-created task carries the rejected label: {:?}",
            task.labels
        ),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn saving_an_edit_form_open_across_the_rejection_names_no_dead_label() {
    // The form opened on a task carrying `-1`, so its labels field says `next`, and the
    // pool still resolves that name to `-1` until the reload answers. Stripping only the
    // form's `before` turned the save into an `AttachLabel -1`, which the store refuses
    // with a toast that says nothing the user can act on.
    let mut model = attached_to_a_provisional();
    press(&mut model, 'e');
    reject_the_create(&mut model);
    for c in " again".chars() {
        press(&mut model, c);
    }
    let effects = press_ctrl(&mut model, 's');

    let queued = all_applied(&effects);
    match queued.as_slice() {
        [Mutation::UpdateTask { after, .. }] => assert_eq!(after.title, "first again"),
        other => panic!("wrong mutations: {other:?}"),
    }
    for mutation in queued {
        assert!(
            !label_ids(mutation).contains(&LabelId(-1)),
            "the save named a label whose create was rejected: {mutation:?}"
        );
    }
    assert!(
        model.modals.is_empty(),
        "the save asked about a label instead of saving: {:?}",
        model.modals
    );
}

#[test]
fn a_label_picker_open_across_the_rejection_stops_offering_it() {
    let mut model = attached_to_a_provisional();
    while !model.modals.is_empty() {
        press_code(&mut model, KeyCode::Esc);
    }
    press(&mut model, 'g');
    press(&mut model, 'a');
    reject_the_create(&mut model);

    match model.modals.last() {
        Some(Modal::Picker(state)) => assert!(
            state
                .candidates
                .iter()
                .all(|candidate| candidate.pick != Pick::Label(LabelId(-1))),
            "the picker still offers the dead label: {:?}",
            state.candidates
        ),
        other => panic!("the label picker is not open: {other:?}"),
    }
}

#[test]
fn a_rejected_rename_of_a_real_label_leaves_its_attach_undoable() {
    // The widened prune is for a label that never existed. A real label whose rename was
    // refused is still on the server, and undoing its attach is still a real undo.
    let mut model = with_labels(vec![label(7, "work")], Vec::new());
    press(&mut model, 'l');
    press(&mut model, ' ');
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(matches!(
        applied(&effects),
        Some(Mutation::AttachLabel { .. })
    ));
    update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(7)),
            kind: "update_label".to_string(),
            message: "nope".to_string(),
        }),
    );

    let effects = press(&mut model, 'U');
    assert!(
        matches!(applied(&effects), Some(Mutation::DetachLabel { label, .. }) if label.id == LabelId(7)),
        "the attach of a label that exists was forgotten: {effects:?}"
    );
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

/// What the token error actually says, in full.
const AUTH_FAILURE: &str =
    "not authorized: missing, malformed, expired or otherwise invalid token provided";

#[test]
fn a_sync_failure_says_the_whole_thing_somewhere() {
    // BUG-16, the half the filed bug missed. The header indicator cuts the message to a
    // hardcoded 30 columns, and nothing else showed it *at all* -- so the message above
    // reached the user as "not authorized: missing, malf…". The cause was a production
    // token on a dev box, and nothing in the visible third could have led anybody there.
    // Two misdiagnoses in two days. The status line is the surface with room for it.
    let mut model = loaded();
    update(
        &mut model,
        Msg::Sync(SyncEvent::Failed {
            phase: Phase::Pull,
            message: AUTH_FAILURE.to_string(),
        }),
    );
    let toast = model
        .status
        .toast
        .as_ref()
        .expect("a failure the user has to act on is worth a toast");
    assert_eq!(toast.text, AUTH_FAILURE);
    assert_eq!(toast.level, Level::Error);
}

#[test]
fn the_same_failure_twice_does_not_reopen_the_toast() {
    // The timer keeps trying while a box is offline, so every pass fails with the same
    // words. Re-raising on each one would park the toast over the list and the key hints
    // for as long as the network was down -- and the toast now takes up to three rows.
    // The header indicator is the surface for "still offline"; the toast is for news.
    let mut model = loaded();
    let failure = || {
        Msg::Sync(SyncEvent::Failed {
            phase: Phase::Pull,
            message: AUTH_FAILURE.to_string(),
        })
    };
    update(&mut model, failure());
    model.status.toast = None;
    update(&mut model, failure());
    assert!(
        model.status.toast.is_none(),
        "the same failure said twice is not twice the news"
    );

    // A *different* failure is news, and says so.
    update(
        &mut model,
        Msg::Sync(SyncEvent::Failed {
            phase: Phase::Pull,
            message: "connection refused".to_string(),
        }),
    );
    assert_eq!(
        model.status.toast.as_ref().map(|t| t.text.as_str()),
        Some("connection refused")
    );
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
    press(&mut model, 'z');
    press(&mut model, 'd');
    assert!(model.status.toast.is_some());

    for _ in 0..tui_do_ui::model::Toast::LIFETIME {
        update(&mut model, Msg::Tick(here()));
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

/// Open the edit form over the selected task and return it.
fn open_edit(model: &mut Model) -> &mut tui_do_ui::modal::EditState {
    press(model, 'e');
    match model.modals.last_mut() {
        Some(Modal::Edit(state)) => state.as_mut(),
        other => panic!("`e` did not open the form: {other:?}"),
    }
}

fn type_into(state: &mut tui_do_ui::modal::EditState, text: &str) {
    for c in text.chars() {
        state.handle(Key::char(c));
    }
}

fn save(model: &mut Model) -> Vec<Effect> {
    update(
        model,
        Msg::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
    )
}

#[test]
fn the_edit_form_opens_filled_in_from_the_task() {
    let mut model = loaded();
    let selected = model
        .selected_task()
        .expect("something is selected")
        .clone();
    let state = open_edit(&mut model);

    assert_eq!(state.title.value(), selected.title);
    assert_eq!(
        state.priority.value(),
        selected.priority.to_string(),
        "a form that opened empty would clear whatever it did not show"
    );
}

#[test]
fn the_edit_form_sends_one_update_for_the_task_body() {
    let mut model = loaded();
    let before = model.selected_task().expect("selected").clone();
    let state = open_edit(&mut model);
    // Clear the title and type a new one.
    for _ in 0..before.title.chars().count() {
        state.handle(Key::plain(KeyCode::Backspace));
    }
    type_into(state, "a different title");

    let effects = save(&mut model);
    assert!(model.modals.is_empty(), "saving closes the form");

    let applied: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Apply { mutation, .. } => Some(mutation),
            _ => None,
        })
        .collect();
    assert_eq!(
        applied.len(),
        1,
        "one write for the body, not one per field"
    );
    match applied[0] {
        Mutation::UpdateTask { before: was, after } => {
            assert_eq!(after.title, "a different title");
            assert_eq!(was.id, before.id);
            assert_eq!(
                after.id, before.id,
                "the write has to name the task it edits"
            );
        }
        other => panic!("expected an update, got {other:?}"),
    }
    // The screen shows it before the store has answered.
    assert_eq!(selected_title(&model), "a different title");
}

#[test]
fn labels_leave_the_form_as_their_own_mutations() {
    // A task write ignores the body's `labels`, so a form that folded them into the
    // update would silently drop them.
    let mut model = loaded();
    update(
        &mut model,
        Msg::LabelsLoaded(vec![Label {
            id: LabelId(1),
            title: "urgent".to_string(),
            ..Label::default()
        }]),
    );
    let state = open_edit(&mut model);
    state.focus = tui_do_ui::modal::EditField::Labels;
    type_into(state, "urgent");

    let effects = save(&mut model);
    let attached: Vec<_> = effects
        .iter()
        .filter(|effect| {
            matches!(
                effect,
                Effect::Apply {
                    mutation: Mutation::AttachLabel { .. },
                    ..
                }
            )
        })
        .collect();
    assert_eq!(attached.len(), 1, "the label was not sent on its own");
}

#[test]
fn the_edit_form_refuses_to_save_an_empty_title() {
    let mut model = loaded();
    let before = model.selected_task().expect("selected").clone();
    let state = open_edit(&mut model);
    for _ in 0..before.title.chars().count() {
        state.handle(Key::plain(KeyCode::Backspace));
    }

    let effects = save(&mut model);
    assert!(
        effects.is_empty(),
        "an untitled task is not a task; nothing should be sent"
    );
    let toast = model.status.toast.as_ref().expect("the user is told why");
    assert!(toast.text.contains("title"));
}

#[test]
fn a_form_saved_untouched_sends_nothing() {
    let mut model = loaded();
    open_edit(&mut model);
    let effects = save(&mut model);
    assert!(
        effects.is_empty(),
        "opening and closing a form is not an edit"
    );
}

#[test]
fn enter_moves_between_fields_but_writes_a_newline_in_the_description() {
    let mut model = loaded();
    let state = open_edit(&mut model);
    assert_eq!(state.focus, tui_do_ui::modal::EditField::Title);

    state.handle(Key::plain(KeyCode::Enter));
    assert_eq!(
        state.focus,
        tui_do_ui::modal::EditField::Description,
        "Enter should move on from a one-line field"
    );

    let before = state.description.value().len();
    state.handle(Key::plain(KeyCode::Enter));
    assert_eq!(
        state.focus,
        tui_do_ui::modal::EditField::Description,
        "Enter belongs to the description, which is genuinely several lines"
    );
    assert_eq!(state.description.value().len(), before + 1);
}

#[test]
fn escaping_the_form_changes_nothing() {
    let mut model = loaded();
    let before = model.selected_task().expect("selected").clone();
    let state = open_edit(&mut model);
    type_into(state, " and more");

    let effects = update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    );
    assert!(model.modals.is_empty());
    assert!(effects.is_empty());
    assert_eq!(selected_title(&model), before.title);
}

#[test]
fn editing_a_task_does_not_move_it_to_a_project_that_merely_shares_a_name() {
    // Dev really does carry two projects called "Inbox" -- #1, which Vikunja creates for
    // the account, and #12, seeded from prod. The form shows a project by *name*, so a
    // task in #12 whose project field was never touched used to resolve back to #1 and
    // get silently moved: gone from the list the user was looking at, in tui-do and in
    // the web UI both. The name is a label, not a key; the id is what was opened.
    let mut model = loaded();
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![
            project(1, "Inbox", 0),
            project(12, "Inbox", 0),
            project(3, "Personal", 0),
        ]),
    );
    answer(
        &mut model,
        vec![Task {
            project_id: ProjectId(12),
            ..task(1, "first")
        }],
    );

    let state = open_edit(&mut model);
    assert_eq!(state.project.value(), "Inbox");
    state.focus = tui_do_ui::modal::EditField::Title;
    type_into(state, "!");
    let effects = save(&mut model);

    let moved = effects.iter().find_map(|effect| match effect {
        Effect::Apply {
            mutation: Mutation::UpdateTask { after, .. },
            ..
        } => Some(after.project_id),
        _ => None,
    });
    assert_eq!(
        moved,
        Some(ProjectId(12)),
        "the title changed; the project did not"
    );
}

#[test]
fn retyping_the_project_field_still_moves_the_task() {
    // The other half of the pair above: leaving the field alone must not move the task,
    // but changing it must, or the fix has quietly turned the field read-only.
    let mut model = loaded();
    let state = open_edit(&mut model);
    state.focus = tui_do_ui::modal::EditField::Project;
    for _ in 0..state.project.value().chars().count() {
        state.handle(Key::plain(KeyCode::Backspace));
    }
    type_into(state, "Personal");
    let effects = save(&mut model);

    let moved = effects.iter().find_map(|effect| match effect {
        Effect::Apply {
            mutation: Mutation::UpdateTask { after, .. },
            ..
        } => Some(after.project_id),
        _ => None,
    });
    assert_eq!(
        moved,
        Some(ProjectId(3)),
        "the field was retyped, so it moves"
    );
}

#[test]
fn a_configured_default_project_takes_the_tasks_that_name_none() {
    // The fallback without one is to look for a project called "Inbox", which is a guess
    // and picks the first of two. Naming it by id is how an account with duplicate titles
    // says which one it means.
    let mut model = loaded();
    model.default_project = Some("#3".to_string());
    press(&mut model, 'n');
    for c in "Something".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    let filed = effects.iter().find_map(|effect| match effect {
        Effect::Apply {
            mutation: Mutation::CreateTask { task },
            ..
        } => Some(task.project_id),
        _ => None,
    });
    assert_eq!(filed, Some(ProjectId(3)));
}

#[test]
fn a_hash_id_names_the_project_that_a_shared_title_cannot() {
    // The lever for an account with two projects called "Inbox": `+#12` says which.
    let mut model = loaded();
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![project(1, "Inbox", 0), project(12, "Inbox", 0)]),
    );
    press(&mut model, 'n');
    for c in "Something +#12".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    let filed = effects.iter().find_map(|effect| match effect {
        Effect::Apply {
            mutation: Mutation::CreateTask { task },
            ..
        } => Some(task.project_id),
        _ => None,
    });
    assert_eq!(
        filed,
        Some(ProjectId(12)),
        "a bare `+Inbox` would have picked whichever comes first"
    );
}

#[test]
fn a_hash_id_that_names_nothing_falls_back_to_the_title() {
    // So a project genuinely called "#9" stays reachable.
    let mut model = loaded();
    update(&mut model, Msg::ProjectsLoaded(vec![project(3, "#9", 0)]));
    press(&mut model, 'n');
    for c in "Something +#9".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    let filed = effects.iter().find_map(|effect| match effect {
        Effect::Apply {
            mutation: Mutation::CreateTask { task },
            ..
        } => Some(task.project_id),
        _ => None,
    });
    assert_eq!(filed, Some(ProjectId(3)));
}

#[test]
fn the_queued_count_is_read_from_the_store_not_carried_forward() {
    // The outbox is shared. `tui-do add` in another terminal queues against the same one,
    // so a count left over from the last sync report drifts: the terminal says 4 and the
    // interface says 3. Every refresh and every local write asks the store instead.
    let mut model = loaded();
    assert!(
        reload_everything(&mut model).contains(&Effect::LoadPending),
        "a refresh must re-read the queue"
    );

    let effects = press(&mut model, 't');
    assert!(
        effects.contains(&Effect::LoadPending),
        "queueing a change must re-read the queue"
    );

    update(
        &mut model,
        Msg::PendingLoaded(QueueHealth {
            queued: 4,
            failing: 2,
            last_error: Some("connection refused".to_string()),
        }),
    );
    assert_eq!(model.status.queued, 4);
    // A count that will not go down reads as progress unless the trouble is named.
    assert_eq!(model.status.failing, 2);
    assert_eq!(
        model.status.queue_error.as_deref(),
        Some("connection refused")
    );
}

#[test]
fn shift_enter_writes_a_newline_in_the_description() {
    // Terminals that speak the enhanced keyboard protocol report Shift-Enter as Enter
    // with a modifier; the rest send a plain carriage return. Both have to reach the
    // description as a line break, or the chord the user reaches for does nothing.
    let mut model = loaded();
    let state = open_edit(&mut model);
    state.focus = tui_do_ui::modal::EditField::Description;
    type_into(state, "one");
    state.handle(Key {
        code: KeyCode::Enter,
        mods: KeyModifiers::SHIFT,
    });
    type_into(state, "two");
    assert_eq!(state.description.value(), "one\ntwo");
    assert_eq!(
        state.focus,
        tui_do_ui::modal::EditField::Description,
        "Shift-Enter must not move to the next field"
    );
}

/// Every mutation a keystroke queued, in order.
fn all_applied(effects: &[Effect]) -> Vec<&Mutation> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Apply { mutation, .. } => Some(mutation),
            _ => None,
        })
        .collect()
}

/// A model whose selected task carries `labels`, with `known` on the server.
fn with_labels(known: Vec<Label>, held: Vec<Label>) -> Model {
    let mut model = loaded();
    update(&mut model, Msg::LabelsLoaded(known));
    let mut first = task(1, "first");
    first.labels = held;
    answer(&mut model, vec![first, task(2, "second"), task(3, "third")]);
    model
}

fn label(id: i64, title: &str) -> Label {
    Label {
        id: LabelId(id),
        title: title.to_string(),
        ..Label::default()
    }
}

/// The priority field, or a panic naming what opened instead.
fn priority_field(model: &Model) -> &tui_do_ui::modal::PriorityState {
    match model.modals.last() {
        Some(Modal::Priority(state)) => state,
        other => panic!("p opens the priority field: {other:?}"),
    }
}

#[test]
fn the_priority_field_opens_on_what_the_task_holds() {
    let mut model = loaded();
    press(&mut model, 'p');
    let state = priority_field(&model);
    assert_eq!(state.current, 0);
    assert_eq!(
        state.selected, 0,
        "the highlight starts where the task stands"
    );
    assert_eq!(state.typed, "", "and nothing has been typed yet");
}

#[test]
fn the_priority_field_takes_only_digits_that_leave_a_valid_priority() {
    let mut model = loaded();
    press(&mut model, 'p');

    // *This was broken:* the picker fuzzy-matched `0005` against "0  Unset" and friends,
    // matched nothing, and Enter then did nothing at all -- so the key read as broken
    // rather than as refusing the input.
    for c in "0005".chars() {
        press(&mut model, c);
    }
    let state = priority_field(&model);
    assert_eq!(state.typed, "00", "the third digit had nowhere valid to go");
    assert_eq!(state.selected, 0);

    // Every other character is refused outright, printable or not.
    for c in ['9', 'p', '-', '.', ' '] {
        press(&mut model, c);
    }
    assert_eq!(priority_field(&model).typed, "00");
}

#[test]
fn a_leading_zero_is_the_one_two_character_priority() {
    let mut model = loaded();
    press(&mut model, 'p');
    press(&mut model, '0');
    press(&mut model, '5');
    let state = priority_field(&model);
    assert_eq!(state.typed, "05");
    assert_eq!(state.selected, 5, "`05` is five, not fifty-five");

    let effects = press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.selected_task().unwrap().priority, 5);
    assert!(applied(&effects).is_some());
}

#[test]
fn a_priority_digit_and_the_arrows_agree_about_the_highlight() {
    let mut model = loaded();
    press(&mut model, 'p');
    press(&mut model, '3');
    assert_eq!(priority_field(&model).selected, 3);

    press_code(&mut model, KeyCode::Down);
    let state = priority_field(&model);
    assert_eq!(state.selected, 4);
    assert_eq!(state.typed, "4", "the field follows the highlight");

    // Rubbing the field out puts the highlight back where the task stands, so Enter
    // after a full erase changes nothing rather than setting zero.
    press_code(&mut model, KeyCode::Backspace);
    let state = priority_field(&model);
    assert_eq!(state.typed, "");
    assert_eq!(state.selected, 0);
}

#[test]
fn picking_a_priority_sets_it_optimistically() {
    let mut model = loaded();
    press(&mut model, 'p');
    press(&mut model, '4');
    let effects = press_code(&mut model, KeyCode::Enter);

    assert_eq!(model.selected_task().unwrap().priority, 4, "on screen now");
    match applied(&effects).expect("a write was queued") {
        Mutation::UpdateTask { before, after } => {
            assert_eq!(before.priority, 0);
            assert_eq!(after.priority, 4);
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("Urgent"), "{}", toast.text);
}

#[test]
fn setting_the_priority_it_already_has_queues_nothing() {
    let mut model = loaded();
    press(&mut model, 'p');
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(
        applied(&effects).is_none(),
        "a write the server would answer with the same row is not worth sending"
    );
    assert!(model.undo.is_empty(), "and it is not worth undoing either");
}

#[test]
fn the_due_prompt_opens_on_what_the_task_already_says() {
    let mut model = loaded();
    let mut first = task(1, "first");
    first.due_date = Some(Utc.with_ymd_and_hms(2026, 12, 24, 9, 0, 0).unwrap()).into();
    answer(&mut model, vec![first]);

    press(&mut model, 'd');
    let Some(Modal::Due(state)) = model.modals.last() else {
        panic!("D opens the due prompt: {:?}", model.modals);
    };
    // 09:00 is not the due default, so the time is shown and survives a retyped day.
    assert_eq!(state.input.value(), "2026-12-24 at 09:00");
}

#[test]
fn a_due_date_goes_through_the_quick_add_parser_not_a_date_format() {
    let mut model = loaded();
    press(&mut model, 'd');
    for c in "tomorrow".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    let due = model
        .selected_task()
        .unwrap()
        .due_date
        .get()
        .expect("a date was understood");
    assert_eq!(due.date_naive(), now().date_naive().succ_opt().unwrap());
    assert!(applied(&effects).is_some());
}

#[test]
fn an_empty_due_field_clears_the_date_rather_than_cancelling() {
    let mut model = loaded();
    let mut first = task(1, "first");
    first.due_date = Some(Utc.with_ymd_and_hms(2026, 12, 24, 9, 0, 0).unwrap()).into();
    answer(&mut model, vec![first]);

    press(&mut model, 'd');
    for _ in 0..30 {
        press_code(&mut model, KeyCode::Backspace);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    assert!(model.selected_task().unwrap().due_date.get().is_none());
    // Esc is how you back out without changing anything; an empty field is a request.
    match applied(&effects).expect("a write was queued") {
        Mutation::UpdateTask { before, after } => {
            assert!(before.due_date.get().is_some());
            assert!(after.due_date.get().is_none());
        }
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn a_due_date_nobody_can_parse_is_reported_rather_than_silently_dropped() {
    let mut model = loaded();
    press(&mut model, 'd');
    for c in "zzzz".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    assert!(applied(&effects).is_none());
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("not a date"), "{}", toast.text);
}

#[test]
fn moving_offers_only_projects_that_accept_writes() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![
            project(1, "Alpha", 0),
            // The server invents these -- Favorites, My Open Tasks, Inbox -- and every
            // one of them rejects a write.
            project(-1, "Favorites", 0),
            Project {
                is_archived: true,
                ..project(4, "Archived", 0)
            },
        ]),
    );
    press(&mut model, 'm');

    let Some(Modal::Picker(picker)) = model.modals.last() else {
        panic!("m opens a picker: {:?}", model.modals);
    };
    let titles: Vec<&str> = picker
        .candidates
        .iter()
        .map(|candidate| candidate.title.as_str())
        .collect();
    assert_eq!(titles, ["Alpha"]);
}

#[test]
fn moving_a_task_writes_the_new_project_and_says_which() {
    let mut model = loaded();
    press(&mut model, 'm');
    for c in "Personal".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    assert_eq!(model.selected_task().unwrap().project_id, ProjectId(3));
    match applied(&effects).expect("a write was queued") {
        Mutation::UpdateTask { before, after } => {
            assert_eq!(before.project_id, ProjectId(1));
            assert_eq!(after.project_id, ProjectId(3));
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("Personal"), "{}", toast.text);
}

#[test]
fn the_label_form_opens_with_the_tasks_own_labels_ticked() {
    let mut model = with_labels(
        vec![label(1, "urgent"), label(2, "backend")],
        vec![label(2, "backend")],
    );
    press(&mut model, 'l');

    let Some(Modal::Labels(state)) = model.modals.last() else {
        panic!("l opens the label form: {:?}", model.modals);
    };
    assert!(state.is_chosen(LabelId(2)));
    assert!(!state.is_chosen(LabelId(1)));
}

#[test]
fn ticking_labels_attaches_and_detaches_rather_than_writing_the_task() {
    let mut model = with_labels(
        vec![label(1, "urgent"), label(2, "backend")],
        vec![label(2, "backend")],
    );
    press(&mut model, 'l');
    // Space toggles `urgent` on, then move down and toggle `backend` off.
    press(&mut model, ' ');
    press_code(&mut model, KeyCode::Down);
    press(&mut model, ' ');
    let effects = press_code(&mut model, KeyCode::Enter);

    let mutations = all_applied(&effects);
    assert_eq!(mutations.len(), 2, "one per label, {mutations:?}");
    // Labels do not travel in the task body, so an UpdateTask carrying a new list would
    // be a write the server ignores.
    assert!(
        !mutations
            .iter()
            .any(|mutation| matches!(mutation, Mutation::UpdateTask { .. })),
        "{mutations:?}"
    );
    match mutations[0] {
        Mutation::AttachLabel { label, .. } => assert_eq!(label.title, "urgent"),
        other => panic!("wrong mutation: {other:?}"),
    }
    match mutations[1] {
        Mutation::DetachLabel { label, .. } => assert_eq!(label.title, "backend"),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn applying_the_labels_a_task_already_has_queues_nothing() {
    let mut model = with_labels(vec![label(1, "urgent")], vec![label(1, "urgent")]);
    press(&mut model, 'l');
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(applied(&effects).is_none());
}

#[test]
fn the_label_key_leaves_the_sidebars_own_l_alone() {
    // `l` expands a project in the tree, which is the vim meaning nobody should have to
    // unlearn -- so the label form is the one that gives way.
    let mut model = with_labels(vec![label(1, "urgent")], vec![]);
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);

    press(&mut model, 'l');
    assert!(
        !matches!(model.modals.last(), Some(Modal::Labels(_))),
        "the sidebar's l stays the sidebar's"
    );
}

#[test]
fn space_lists_what_was_configured_and_the_key_runs_it() {
    // `Config::example` binds `u` to priority 5 and `w` to the Work project.
    let mut model = loaded();
    press(&mut model, ' ');

    let Some(Modal::QuickActions(state)) = model.modals.last() else {
        panic!("Space opens the quick-action menu: {:?}", model.modals);
    };
    assert_eq!(state.rows.len(), 2);
    assert_eq!(state.rows[0].0, 'u');
    assert!(state.rows[0].1.contains("DO NOW"), "{}", state.rows[0].1);

    let effects = press(&mut model, 'u');
    assert_eq!(model.selected_task().unwrap().priority, 5);
    assert!(applied(&effects).is_some());
    assert!(model.modals.is_empty(), "the menu closes once it has run");
}

#[test]
fn a_quick_action_naming_something_that_does_not_exist_says_so_when_pressed() {
    // `w` moves to "Work", and this model has no such project. Reporting it at startup
    // would be a line nobody reads; reporting it here is the moment it matters.
    let mut model = loaded();
    press(&mut model, ' ');
    let effects = press(&mut model, 'w');

    assert!(applied(&effects).is_none());
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("No project called"), "{}", toast.text);
}

#[test]
fn an_unconfigured_quick_action_key_leaves_the_menu_up() {
    let mut model = loaded();
    press(&mut model, ' ');
    press(&mut model, 'z');
    assert!(
        matches!(model.modals.last(), Some(Modal::QuickActions(_))),
        "the list of keys that do work is still on screen"
    );
    // And the key that opened it closes it.
    press(&mut model, ' ');
    assert!(model.modals.is_empty());
}

#[test]
fn a_label_quick_action_toggles_rather_than_only_adding() {
    let mut model = with_labels(vec![label(1, "urgent")], vec![]);
    model.quick_actions = vec![tui_do_core::config::QuickAction {
        key: 'i',
        kind: tui_do_core::config::QuickActionKind::Label("urgent".to_string()),
    }];

    press(&mut model, ' ');
    let effects = press(&mut model, 'i');
    match applied(&effects).expect("a write was queued") {
        Mutation::AttachLabel { label, .. } => assert_eq!(label.title, "urgent"),
        other => panic!("wrong mutation: {other:?}"),
    }

    press(&mut model, ' ');
    let effects = press(&mut model, 'i');
    match applied(&effects).expect("a write was queued") {
        Mutation::DetachLabel { label, .. } => assert_eq!(label.title, "urgent"),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn space_with_nothing_configured_says_so_instead_of_opening_an_empty_box() {
    let mut model = loaded();
    model.quick_actions.clear();
    press(&mut model, ' ');
    assert!(model.modals.is_empty());
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("quick_actions"), "{}", toast.text);
}

#[test]
fn every_quick_key_with_nothing_selected_says_so() {
    let mut model = loaded();
    let id = model.query_id;
    update(&mut model, Msg::TasksLoaded { id, tasks: vec![] });
    update(&mut model, Msg::LabelsLoaded(vec![label(1, "urgent")]));

    for key in ['t', 'p', 'd', 'u', 'm', 'l', ' '] {
        model.status.toast = None;
        let effects = update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
        );
        assert!(applied(&effects).is_none(), "{key} queued a write");
        assert!(model.modals.is_empty(), "{key} opened a modal anyway");
        let toast = model.status.toast.as_ref().expect("the user is told");
        assert!(
            toast.text.contains("No task selected"),
            "{key}: {}",
            toast.text
        );
    }
}

#[test]
fn the_label_form_can_take_off_a_label_the_labels_table_has_not_caught_up_with() {
    // A pull stores tasks and labels in separate passes, so a task can carry one the
    // list has never seen. A form that could not show it could not take it off either.
    let mut model = with_labels(vec![label(1, "urgent")], vec![label(9, "from a pull")]);
    press(&mut model, 'l');

    let Some(Modal::Labels(state)) = model.modals.last() else {
        panic!("l opens the label form: {:?}", model.modals);
    };
    assert_eq!(state.labels.len(), 2);
    assert!(state.is_chosen(LabelId(9)));

    // Move to it and untick it.
    press_code(&mut model, KeyCode::Down);
    press(&mut model, ' ');
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("a detach was queued") {
        Mutation::DetachLabel { label, .. } => assert_eq!(label.title, "from a pull"),
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn ctrl_u_clears_a_prefilled_field_rather_than_making_the_user_backspace_it() {
    let mut model = loaded();
    let mut first = task(1, "first");
    first.due_date = Some(Utc.with_ymd_and_hms(2026, 12, 24, 9, 0, 0).unwrap()).into();
    answer(&mut model, vec![first]);

    press(&mut model, 'd');
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
    );
    let Some(Modal::Due(state)) = model.modals.last() else {
        panic!("the due prompt is still open: {:?}", model.modals);
    };
    assert_eq!(state.input.value(), "");

    // And the field then takes a fresh date rather than appending to the old one.
    for c in "tomorrow".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    let due = model.selected_task().unwrap().due_date.get().unwrap();
    assert_eq!(due.date_naive(), now().date_naive().succ_opt().unwrap());
}

#[test]
fn the_edit_forms_priority_field_has_the_same_hard_limit() {
    // It used to take any text and report "not a priority between 0 and 5" on save,
    // which is a slower and less honest way of saying the field cannot hold it.
    let mut model = loaded();
    press(&mut model, 'e');
    // Tab to Priority: Title, Description, Priority.
    press_code(&mut model, KeyCode::Tab);
    press_code(&mut model, KeyCode::Tab);

    for c in "0005".chars() {
        press(&mut model, c);
    }
    for c in ['9', 'p'] {
        press(&mut model, c);
    }
    let Some(Modal::Edit(state)) = model.modals.last() else {
        panic!("the form is still open: {:?}", model.modals);
    };
    assert_eq!(state.priority.value(), "00");
}

#[test]
fn a_date_written_any_of_the_ways_people_write_them_reaches_the_task() {
    // The `D` prompt hands its text to the same parser quick-add uses, so one table
    // covers both. Fixed `now` is 2026-08-24 in these tests.
    for (typed, expected) in [
        ("27/08/26", "2026-08-27"),
        ("8/27/26", "2026-08-27"),
        ("2026-08-27", "2026-08-27"),
        ("27aug26", "2026-08-27"),
        ("27-Aug-2026", "2026-08-27"),
        ("27 August 2026", "2026-08-27"),
        ("tomorrow", "2026-08-25"),
    ] {
        let mut model = loaded();
        press(&mut model, 'd');
        for c in typed.chars() {
            press(&mut model, c);
        }
        press_code(&mut model, KeyCode::Enter);
        let due = model
            .selected_task()
            .unwrap()
            .due_date
            .get()
            .unwrap_or_else(|| panic!("{typed:?} was not understood"));
        assert_eq!(due.date_naive().to_string(), expected, "{typed:?}");
    }
}

#[test]
fn quick_add_leaves_a_key_name_in_the_title_rather_than_acting_on_it() {
    // `tui-do add 'Go to the shop p3 D 26Aug27 l Scooby'` landed with the whole tail in
    // the title, because `p`, `D` and `l` are *interface keys* and quick-add has its own
    // syntax. The date is the one part of that line the parser should now take.
    let mut model = loaded();
    press(&mut model, 'n');
    for c in "Buy milk p3 l Scooby".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    match applied(&effects).expect("a create was queued") {
        Mutation::CreateTask { task } => {
            assert_eq!(task.title, "Buy milk p3 l Scooby");
            assert_eq!(task.priority, 0, "`p3` is not priority syntax; `!3` is");
            assert!(task.labels.is_empty(), "`l x` is not label syntax; `*x` is");
        }
        other => panic!("wrong mutation: {other:?}"),
    }
}

/// A model with a deep enough tree that the sidebar cannot show all of it.
fn tall_tree(height: u16) -> Model {
    let mut model = Model::new(&Config::example(), Scope::All, here(), (160, height));
    let _ = reload_everything(&mut model);
    answer(&mut model, vec![task(1, "first")]);
    let projects = (1..=20)
        .map(|id| project(id, &format!("Project {id:02}"), 0))
        .collect();
    update(&mut model, Msg::ProjectsLoaded(projects));
    model
}

#[test]
fn the_sidebar_scrolls_to_follow_the_selection() {
    // *This was broken:* the sidebar carried an offset from the first day and nothing
    // ever wrote to it, so a tree taller than the pane stopped at the bottom edge and
    // the selection walked on into rows nobody could see.
    let mut model = tall_tree(12);
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);
    assert_eq!(model.sidebar.offset, 0);

    for _ in 0..20 {
        press(&mut model, 'j');
    }
    assert!(
        model.sidebar.offset > 0,
        "twenty rows down a twelve-row pane and it never scrolled"
    );

    let height = usize::from(
        model
            .frames()
            .sidebar
            .expect("the sidebar is showing")
            .height,
    );
    let rows = tui_do_ui::sidebar::rows(&model.data.projects, &model.data.counts, &model.sidebar);
    let index = rows
        .iter()
        .position(|row| row.target() == Some(model.sidebar.selected))
        .expect("the selection is a row");
    assert!(
        (model.sidebar.offset..model.sidebar.offset + height).contains(&index),
        "row {index} is outside the drawn window {}..{}",
        model.sidebar.offset,
        model.sidebar.offset + height
    );

    // And back up again.
    for _ in 0..20 {
        press(&mut model, 'k');
    }
    assert_eq!(model.sidebar.offset, 0, "the top is reachable again");
}

#[test]
fn a_shrinking_tree_does_not_leave_the_sidebar_scrolled_past_its_end() {
    let mut model = tall_tree(12);
    press_code(&mut model, KeyCode::BackTab);
    for _ in 0..20 {
        press(&mut model, 'j');
    }
    assert!(model.sidebar.offset > 0);

    // Every project vanishes -- an archive, a filter, a pull that dropped them.
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![project(1, "Alpha", 0)]),
    );
    let rows = tui_do_ui::sidebar::rows(&model.data.projects, &model.data.counts, &model.sidebar);
    assert!(
        model.sidebar.offset < rows.len(),
        "the pane would have drawn empty over a list that is still there"
    );
}

#[test]
fn a_taller_terminal_scrolls_the_sidebar_back_to_the_top() {
    let mut model = tall_tree(12);
    press_code(&mut model, KeyCode::BackTab);
    for _ in 0..20 {
        press(&mut model, 'j');
    }
    assert!(model.sidebar.offset > 0);

    update(&mut model, Msg::Resize(160, 60));
    assert_eq!(
        model.sidebar.offset, 0,
        "the whole tree fits now, so there is nothing to scroll past"
    );
}

#[test]
fn a_due_date_in_the_past_is_allowed_but_said_loudly() {
    // Overdue is a real state and backdating is a real thing to want, so it is not
    // refused. It is not confirmed as though it were ordinary either: `2024` where
    // `2026` was meant is a typo, and a quiet "Due ..." would bless it.
    let mut model = loaded();
    press(&mut model, 'd');
    for c in "27/08/2024".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    assert!(applied(&effects).is_some(), "the write still happens");
    let due = model.selected_task().unwrap().due_date.get().unwrap();
    assert_eq!(due.date_naive().to_string(), "2024-08-27");

    let toast = model.status.toast.as_ref().expect("the user is told");
    assert_eq!(toast.level, tui_do_ui::model::Level::Warning);
    assert!(toast.text.contains("has passed"), "{}", toast.text);
}

#[test]
fn a_due_date_in_the_future_is_confirmed_quietly() {
    let mut model = loaded();
    press(&mut model, 'd');
    for c in "27/08/2027".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert_eq!(toast.level, tui_do_ui::model::Level::Info);
}

#[test]
fn every_way_of_setting_a_past_due_date_says_so() {
    // Allowed -- overdue is a real state and backdating something you have been carrying
    // is a real thing to want -- but never confirmed quietly, because `2024` typed where
    // `2026` was meant is indistinguishable from a deliberate backdate once it is stored.
    // There are three ways in, and for a long time only `D` said anything: the edit form
    // answered "Saved" and quick-add answered "Added", which is precisely the quiet
    // confirmation this rule exists to prevent.

    // `D`, where the date is the subject and leads the message.
    let mut model = loaded();
    press(&mut model, 'd');
    for _ in 0..20 {
        press_code(&mut model, KeyCode::Backspace);
    }
    for c in "27/08/2024".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    let toast = model.status.toast.clone().expect("D said nothing");
    assert_eq!(toast.level, tui_do_ui::model::Level::Warning, "{toast:?}");
    assert!(toast.text.contains("that date has passed"), "{toast:?}");

    // The edit form.
    let mut model = loaded();
    let state = open_edit(&mut model);
    state.focus = tui_do_ui::modal::EditField::Due;
    for _ in 0..20 {
        state.handle(
            Key::from_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)).unwrap(),
        );
    }
    type_into(state, "27/08/2024");
    save(&mut model);
    let toast = model.status.toast.clone().expect("the form said nothing");
    assert_eq!(toast.level, tui_do_ui::model::Level::Warning, "{toast:?}");
    assert!(toast.text.contains("that date has passed"), "{toast:?}");

    // Quick-add, where the date is incidental -- nothing was aimed at a date field, the
    // text simply had a date in it, which makes this the easiest one to do by accident.
    let mut model = loaded();
    press(&mut model, 'n');
    for c in "buy stamps 27/08/2024".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    let toast = model.status.toast.clone().expect("quick-add said nothing");
    assert_eq!(toast.level, tui_do_ui::model::Level::Warning, "{toast:?}");
    assert!(toast.text.contains("that date has passed"), "{toast:?}");
}

#[test]
fn saving_an_already_overdue_task_does_not_warn_about_a_date_it_did_not_move() {
    // Otherwise every edit to anything overdue carries the warning, and a warning that
    // fires when nothing happened is one the user learns to read past.
    let mut model = loaded();
    let mut first = task(1, "first");
    first.due_date = Some(Utc.with_ymd_and_hms(2024, 8, 27, 9, 0, 0).unwrap()).into();
    answer(&mut model, vec![first]);

    let state = open_edit(&mut model);
    type_into(state, "!");
    save(&mut model);

    let toast = model.status.toast.clone().expect("a toast");
    assert_eq!(toast.text, "Saved", "{toast:?}");
}

#[test]
fn marking_done_holds_its_place_in_the_list_the_way_deleting_does() {
    // `x` moves the selection to the row that slid into the gap. `d` used to leave it on
    // a task that the reload then dropped from the list -- and `TasksLoaded` cannot tell
    // "the selected row left this list" from "this is a different list", so it fell back
    // to the first row. Holding `d` down a list sent the cursor to the top every press.
    let mut model = loaded();
    answer(
        &mut model,
        vec![task(1, "first"), task(2, "second"), task(3, "third")],
    );
    assert!(
        !model.query.include_done,
        "done tasks are hidden by default"
    );

    press(&mut model, 't');
    assert_eq!(
        model.selected_task().map(|task| task.id),
        Some(TaskId(2)),
        "the selection should step down to the next row, not jump to the top"
    );
    press(&mut model, 't');
    assert_eq!(model.selected_task().map(|task| task.id), Some(TaskId(3)));

    // The last row has nothing below it, so the selection steps up rather than vanishing.
    press(&mut model, 't');
    assert_eq!(model.data.tasks.len(), 0);
    assert_eq!(model.selected_task().map(|task| task.id), None);
}

#[test]
fn marking_done_leaves_the_row_in_place_when_done_tasks_are_shown() {
    // The row belongs in this list either way, so it stays put and simply gains a tick.
    let mut model = loaded();
    answer(&mut model, vec![task(1, "first"), task(2, "second")]);
    press(&mut model, 'z');
    press(&mut model, 'd');
    answer(&mut model, vec![task(1, "first"), task(2, "second")]);
    assert!(model.query.include_done);

    press(&mut model, 't');

    assert_eq!(model.selected_task().map(|task| task.id), Some(TaskId(1)));
    assert!(model.selected_task().expect("still selected").done);
    assert_eq!(
        model.data.tasks.len(),
        2,
        "nothing should have left the list"
    );
}

#[test]
fn undoing_a_label_rename_renames_both_copies_of_the_label() {
    // `apply_locally` is what makes an edit visible before the store answers, and the
    // model keeps *two* snapshots of every label, neither a view of the other: the copy
    // carried on each task in the list, which the row chips are drawn from, and
    // `model.data.labels`, which the picker lists and `resolve_labels` matches a quick-add
    // `*name` against. Both are written only by `Msg::LabelsLoaded`, and an edit returns
    // no labels reload -- so rewriting one and not the other leaves the picker offering a
    // name the user has just renamed away from.
    //
    // Driven through `u` because that is the only path the interface has to an
    // `UpdateLabel` today -- the label editor lands later in the phase -- and it exercises
    // `undo_text` at the same time.
    let mut model = with_labels(vec![label(7, "next up"), label(9, "unrelated")], vec![]);
    model.data.tasks[0].labels = vec![label(7, "next up")];
    model.data.tasks[2].labels = vec![label(9, "unrelated")];
    model.undo.push(Mutation::UpdateLabel {
        before: Box::new(label(7, "next up")),
        after: Box::new(label(7, "next")),
    });

    let effects = press(&mut model, 'U');

    assert_eq!(
        model.data.tasks[0].labels[0].title, "next",
        "the chip on the row still shows the old title"
    );
    assert_eq!(
        model.data.tasks[2].labels[0].title, "unrelated",
        "a label the rename did not name was rewritten"
    );
    // The picker's copy, which nothing else reloads.
    let known: Vec<&str> = model
        .data
        .labels
        .iter()
        .map(|label| label.title.as_str())
        .collect();
    assert_eq!(
        known,
        vec!["next", "unrelated"],
        "the label list the picker and quick-add read still holds the old title"
    );
    // And it is queued like any other change, rather than being a screen-only edit.
    match applied(&effects).expect("the undo queues the inverse rename") {
        Mutation::UpdateLabel { before, after } => {
            assert_eq!(before.title, "next up");
            assert_eq!(after.title, "next");
        }
        other => panic!("wrong mutation: {other:?}"),
    }
    // Exact, not `contains`: the plausible mistake in `undo_text` is reading `before`
    // instead of `after`, which yields "reverted label next up" -- and that contains
    // "next". The noun is part of the assertion because a bare "Undone — next" is the
    // wording this arm used to have, and it reads as a fragment beside its neighbours.
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert_eq!(toast.text, "Undone — reverted label next");
}

/// The question a quick-add asks about a label nobody has made yet, or a panic naming
/// what is on the stack instead.
fn confirmation(model: &Model) -> &tui_do_ui::modal::ConfirmLabelsState {
    match model.modals.last() {
        Some(Modal::ConfirmLabels(state)) => state,
        other => panic!("nothing is being asked: {other:?}"),
    }
}

/// A model holding one label, with `a` typed into the quick-add prompt and submitted.
fn asked_about_waiting() -> (Model, Vec<Effect>) {
    let mut model = loaded();
    update(&mut model, Msg::LabelsLoaded(vec![label(4, "urgent")]));
    press(&mut model, 'n');
    for c in "Call the VA *urgent *waiting".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    (model, effects)
}

#[test]
fn an_unknown_label_in_quick_add_asks_before_it_creates() {
    // The whole point of the modal: the label pool is global to every project, so a typo
    // in a task line would pollute completion everywhere, forever. The web UI creates it
    // silently and that is deliberately not what happens here.
    let (model, effects) = asked_about_waiting();
    assert!(
        all_applied(&effects).is_empty(),
        "the task waits behind the answer -- creating it and then attaching a label the \
         user rejected would leave it half done: {effects:?}"
    );
    assert_eq!(confirmation(&model).unknown, vec!["waiting".to_string()]);
    assert!(
        model
            .data
            .tasks
            .iter()
            .all(|task| task.title != "Call the VA"),
        "nothing optimistic either, or Esc would leave a row nobody queued"
    );
}

#[test]
fn declining_adds_the_task_without_the_label() {
    let (mut model, _) = asked_about_waiting();
    let effects = press(&mut model, 'n');

    match all_applied(&effects).as_slice() {
        [Mutation::CreateTask { task }] => {
            assert_eq!(task.title, "Call the VA");
            assert_eq!(
                task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
                vec![LabelId(4)],
                "the label that does exist still lands"
            );
        }
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty(), "the question is answered");
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("waiting"), "{}", toast.text);
}

#[test]
fn accepting_creates_the_label_first_and_the_task_behind_it() {
    // Order is the contract: the create is queued first so the task naming the label
    // cannot outrun it, and so adoption can retarget what is queued behind it.
    let (mut model, _) = asked_about_waiting();
    let effects = press(&mut model, 'y');

    match all_applied(&effects).as_slice() {
        [Mutation::CreateLabel { label }] => {
            assert_eq!(label.title, "waiting");
            assert_eq!(
                label.id,
                LabelId(0),
                "the store allocates the provisional id, not the interface"
            );
        }
        other => panic!("the create is queued alone, and first: {other:?}"),
    }
    assert!(
        effects.contains(&Effect::LoadLabels),
        "without the reload the task never learns the id: {effects:?}"
    );
    assert!(
        matches!(model.modals.last(), Some(Modal::ConfirmLabels(_))),
        "the box stays up while the label is being made"
    );

    // The store applied the create as it queued it, so the reload names the provisional.
    let effects = update(
        &mut model,
        Msg::LabelsLoaded(vec![label(4, "urgent"), label(-1, "waiting")]),
    );
    match all_applied(&effects).as_slice() {
        [Mutation::CreateTask { task }] => assert_eq!(
            task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![LabelId(4), LabelId(-1)],
            "the task carries the label the user just agreed to"
        ),
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty(), "and the box is gone");
}

#[test]
fn the_task_waits_for_the_label_it_asked_for_rather_than_a_stale_snapshot() {
    // The runtime spawns every effect on its own task, so the reload the create asked for
    // races the write and can answer from before it. Resuming on that answer would queue
    // the task without the label the user had just said yes to.
    let (mut model, _) = asked_about_waiting();
    press(&mut model, 'y');
    let effects = update(&mut model, Msg::LabelsLoaded(vec![label(4, "urgent")]));
    assert!(
        all_applied(&effects).is_empty(),
        "nothing to resume on yet: {effects:?}"
    );
    assert!(matches!(model.modals.last(), Some(Modal::ConfirmLabels(_))));
}

#[test]
fn a_second_yes_does_not_queue_the_label_twice() {
    // A label title is not unique, so two creates are two labels and nothing in the
    // response can tell them apart afterwards.
    let (mut model, _) = asked_about_waiting();
    press(&mut model, 'y');
    let effects = press(&mut model, 'y');
    assert!(all_applied(&effects).is_empty(), "{effects:?}");
}

#[test]
fn a_rejected_label_create_does_not_wedge_the_task_that_asked_for_it() {
    // `awaiting` is what stops a second yes queueing a duplicate. A rejection means the
    // label is never coming back, so a title left in there holds the task the user was
    // adding on screen forever, with no key that would ever release it.
    let (mut model, _) = asked_about_waiting();
    press(&mut model, 'y');
    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(-1)),
            kind: "create_label".to_string(),
            message: "nope".to_string(),
        }),
    );

    match all_applied(&effects).as_slice() {
        [Mutation::CreateTask { task }] => assert_eq!(
            task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![LabelId(4)],
            "the label that failed is left off; the rest of the task is not lost"
        ),
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty(), "and nothing is left waiting");
    // One message, not two written over each other: the slot is one deep, the rejection
    // is the half the user cannot work out by looking, and the note about the task going
    // out unlabelled would otherwise leave the task row as its only signal.
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("rejected"), "{}", toast.text);
    assert!(
        toast.text.contains("waiting"),
        "and what it cost them: {}",
        toast.text
    );
}

#[test]
fn a_rejected_rename_leaves_a_create_that_is_still_in_flight_alone() {
    // The release is gated on the mutation's *kind*, not merely on the subject being a
    // label. A rejected rename of some other label says nothing about a create the server
    // has not answered yet -- abandoning that wait leaves the user with a label that does
    // get made and a task that does not carry it, and the combined toast above would not
    // even mention it, because nothing was released to mention.
    let (mut model, _) = asked_about_waiting();
    press(&mut model, 'y');
    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(9)),
            kind: "update_label".to_string(),
            message: "This label does not exist.".to_string(),
        }),
    );
    assert!(
        all_applied(&effects).is_empty(),
        "the held task is still waiting for the label it asked for: {effects:?}"
    );
    assert!(matches!(model.modals.last(), Some(Modal::ConfirmLabels(_))));

    // And when the create it *is* waiting for lands, it still resumes.
    let effects = update(
        &mut model,
        Msg::LabelsLoaded(vec![label(4, "urgent"), label(-1, "waiting")]),
    );
    assert!(matches!(
        all_applied(&effects).as_slice(),
        [Mutation::CreateTask { .. }]
    ));
}

#[test]
fn the_kind_the_release_gates_on_is_the_one_the_store_reports() {
    // `release_held_labels` compares against a literal, because the event carries a
    // `String` and the interface has no `Mutation` to ask. This is the coupling: if the
    // store ever renames the kind, the release silently stops firing and every held
    // submission wedges on a rejection.
    assert_eq!(
        Mutation::CreateLabel {
            label: Box::new(Label::default())
        }
        .kind(),
        "create_label"
    );
}

#[test]
fn escaping_the_question_still_adds_the_task() {
    // Esc means "back out" everywhere else, and here there is nothing to back out to:
    // the prompt that held the text has already closed. So it answers the question the
    // safe way rather than throwing away what the user typed.
    let (mut model, _) = asked_about_waiting();
    let effects = press_code(&mut model, KeyCode::Esc);
    match all_applied(&effects).as_slice() {
        [Mutation::CreateTask { task }] => assert_eq!(task.title, "Call the VA"),
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty());
}

#[test]
fn the_edit_form_asks_about_a_new_label_too() {
    // The same question from the other surface that resolves `*label` names, because a
    // typo in the Labels field pollutes the same global pool.
    let mut model = with_labels(vec![label(4, "urgent")], Vec::new());
    press(&mut model, 'e');
    for _ in 0..5 {
        press_code(&mut model, KeyCode::Tab);
    }
    for c in "waiting".chars() {
        press(&mut model, c);
    }
    let effects = press_ctrl(&mut model, 's');
    assert!(all_applied(&effects).is_empty(), "{effects:?}");
    assert_eq!(confirmation(&model).unknown, vec!["waiting".to_string()]);

    press(&mut model, 'y');
    let effects = update(
        &mut model,
        Msg::LabelsLoaded(vec![label(4, "urgent"), label(-1, "waiting")]),
    );
    // The task write carries the label only so the row on screen looks right; the attach
    // behind it is what actually puts the label on, because labels do not travel in the
    // task body.
    match all_applied(&effects).as_slice() {
        [Mutation::UpdateTask { .. }, Mutation::AttachLabel { task, label }] => {
            assert_eq!(*task, TaskId(1));
            assert_eq!(
                label.id,
                LabelId(-1),
                "the id the store gave the label it just made"
            );
        }
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty());
}

#[test]
fn an_adoption_reaches_the_edit_the_question_is_holding() {
    // The question is sitting on an `EditDraft`, and a draft carries the task whole as
    // `before` -- which is what `apply_edit` computes its attach and detach sets against.
    // A label id left at the provisional in there makes the resumed save detach the label
    // the server has just named and attach it again under the id it no longer has.
    let mut model = with_labels(vec![label(-1, "next")], vec![label(-1, "next")]);
    press(&mut model, 'e');
    for _ in 0..5 {
        press_code(&mut model, KeyCode::Tab);
    }
    for c in ", waiting".chars() {
        press(&mut model, c);
    }
    press_ctrl(&mut model, 's');
    assert_eq!(confirmation(&model).unknown, vec!["waiting".to_string()]);

    update(
        &mut model,
        Msg::Sync(SyncEvent::Adopted {
            provisional: Subject::Label(LabelId(-1)),
            assigned: Subject::Label(LabelId(41)),
        }),
    );
    press(&mut model, 'y');
    let effects = update(
        &mut model,
        Msg::LabelsLoaded(vec![label(41, "next"), label(-2, "waiting")]),
    );

    let queued: Vec<&Mutation> = all_applied(&effects);
    assert!(
        !queued
            .iter()
            .any(|mutation| matches!(mutation, Mutation::DetachLabel { .. })),
        "the label the task already carries is not being taken off it: {queued:?}"
    );
    match queued.as_slice() {
        [Mutation::UpdateTask { .. }, Mutation::AttachLabel { label, .. }] => {
            assert_eq!(label.id, LabelId(-2), "only the new one is attached");
        }
        other => panic!("wrong mutations: {other:?}"),
    }
}

#[test]
fn enter_does_not_create_a_label_by_reflex() {
    // Enter is the key that submitted the quick-add a frame ago, so this box appears
    // under a finger already on it. A second press must not add to a pool shared by every
    // project -- the same rule the `l` form's `C-n` follows.
    let (mut model, _) = asked_about_waiting();
    let effects = press_code(&mut model, KeyCode::Enter);
    match all_applied(&effects).as_slice() {
        [Mutation::CreateTask { task }] => assert_eq!(
            task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![LabelId(4)],
            "the safe answer, not the irreversible one"
        ),
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty());
}

#[test]
fn two_unknown_labels_are_one_question_and_the_task_waits_for_both() {
    // One question, not one per typo: the box names both, `y` queues both creates, and
    // `Waiting::Creating` is only empty -- and the task only queued -- once both have
    // come back. A resume on the first would land the task carrying half of what the user
    // agreed to.
    let mut model = loaded();
    update(&mut model, Msg::LabelsLoaded(vec![label(4, "urgent")]));
    press(&mut model, 'n');
    for c in "Call the VA *urgent *waiting *blocked".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(
        confirmation(&model).unknown,
        vec!["waiting".to_string(), "blocked".to_string()],
        "in the order they were typed"
    );
    assert!(
        confirmation(&model).title().contains("No such labels"),
        "plural: {}",
        confirmation(&model).title()
    );

    let effects = press(&mut model, 'y');
    let titles: Vec<&str> = all_applied(&effects)
        .iter()
        .filter_map(|mutation| match mutation {
            Mutation::CreateLabel { label } => Some(label.title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec!["waiting", "blocked"], "both, and in order");
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("labels"), "plural: {}", toast.text);

    // One of the two lands. The other is still outstanding, so nothing resumes.
    let effects = update(
        &mut model,
        Msg::LabelsLoaded(vec![label(4, "urgent"), label(-1, "waiting")]),
    );
    assert!(
        all_applied(&effects).is_empty(),
        "half the answer is not the answer: {effects:?}"
    );

    let effects = update(
        &mut model,
        Msg::LabelsLoaded(vec![
            label(4, "urgent"),
            label(-1, "waiting"),
            label(-2, "blocked"),
        ]),
    );
    match all_applied(&effects).as_slice() {
        [Mutation::CreateTask { task }] => assert_eq!(
            task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![LabelId(4), LabelId(-1), LabelId(-2)]
        ),
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty());
}

#[test]
fn a_label_named_twice_with_different_case_is_asked_about_and_created_once() {
    // A title is not unique to Vikunja -- creating the same one twice answers `201`
    // twice, per CLAUDE.md's replay table -- so two spellings of one typo must not become
    // two creates. `resolve_labels` already folds ASCII case to *match* a known label;
    // this is the same fold applied to what it reports as missing.
    let mut model = loaded();
    update(&mut model, Msg::LabelsLoaded(vec![label(4, "urgent")]));
    press(&mut model, 'n');
    for c in "Call the VA *waiting *Waiting *WAITING".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(
        confirmation(&model).unknown,
        vec!["waiting".to_string()],
        "one name once, first spelling wins"
    );

    let effects = press(&mut model, 'y');
    let titles: Vec<&str> = all_applied(&effects)
        .iter()
        .filter_map(|mutation| match mutation {
            Mutation::CreateLabel { label } => Some(label.title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec!["waiting"], "one create, not three");
}

#[test]
fn one_rejected_create_of_two_still_lands_the_task_with_the_other() {
    // The bounded answer, written down: the event names an id neither waiting modal ever
    // learned, so a rejection ends the wait for *all* the titles rather than the one that
    // failed. The survivor is really made -- the reload brings it into the pool -- and the
    // task simply goes out without it, which is recoverable in one keystroke, where a box
    // nobody can close is not.
    let mut model = loaded();
    update(&mut model, Msg::LabelsLoaded(vec![label(4, "urgent")]));
    press(&mut model, 'n');
    for c in "Call the VA *urgent *waiting *blocked".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    press(&mut model, 'y');

    let effects = update(
        &mut model,
        Msg::Sync(SyncEvent::Rejected {
            subject: Subject::Label(LabelId(-2)),
            kind: "create_label".to_string(),
            message: "You do not have the right to do this.".to_string(),
        }),
    );
    match all_applied(&effects).as_slice() {
        [Mutation::CreateTask { task }] => assert_eq!(
            task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![LabelId(4)],
            "neither new label is on it: the pool has not named either one yet"
        ),
        other => panic!("wrong mutations: {other:?}"),
    }
    assert!(model.modals.is_empty(), "and nothing is left waiting");
    let toast = model.status.toast.as_ref().expect("the user is told");
    assert!(toast.text.contains("rejected"), "{}", toast.text);
    assert!(
        toast.text.contains("waiting") && toast.text.contains("blocked"),
        "and both names it went out without: {}",
        toast.text
    );
}

/// A model whose selected task carries these links in title and description.
fn with_links(title: &str, description: &str) -> Model {
    let mut model = loaded();
    answer(
        &mut model,
        vec![Task {
            id: TaskId(1),
            project_id: ProjectId(1),
            title: title.to_string(),
            description: description.to_string(),
            ..Task::default()
        }],
    );
    model
}

#[test]
fn o_on_a_task_with_no_link_says_so_rather_than_doing_nothing() {
    let mut model = with_links("Call the VA", "about the case number");
    let effects = press(&mut model, 'o');
    assert!(effects.is_empty());
    assert!(model.modals.is_empty(), "a picker opened for no links");
    assert_eq!(
        model.status.toast.as_ref().map(|t| t.text.as_str()),
        Some("No link in this task")
    );
}

#[test]
fn o_on_a_task_with_one_link_opens_it_without_asking() {
    // 547 of the store's 576 linked tasks are this case; a picker over one item is
    // ceremony, and the keystroke it costs is paid on nearly every use.
    let mut model = with_links("Read this https://example.com/a", "");
    let effects = press(&mut model, 'o');
    assert!(model.modals.is_empty(), "a picker opened for a single link");
    assert_eq!(
        effects,
        vec![Effect::OpenUrl("https://example.com/a".into())]
    );
}

#[test]
fn o_on_a_task_with_several_links_asks_which() {
    let mut model = with_links(
        "https://one.test",
        "and https://two.test and https://three.test",
    );
    let effects = press(&mut model, 'o');
    assert!(effects.is_empty(), "a link was opened before being chosen");
    let Some(Modal::Picker(picker)) = model.modals.last() else {
        panic!("no picker: {:?}", model.modals);
    };
    assert_eq!(picker.kind, PickerKind::Url);
    // Order is information -- title first, then down the description -- so the picker
    // does not sort, unlike the project and label ones.
    let titles: Vec<&str> = picker.candidates.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(
        titles,
        vec!["https://one.test", "https://two.test", "https://three.test"]
    );
}

#[test]
fn choosing_a_link_from_the_picker_opens_that_one() {
    let mut model = with_links("https://one.test", "and https://two.test");
    press(&mut model, 'o');
    press_code(&mut model, KeyCode::Down);
    let effects = press_code(&mut model, KeyCode::Enter);
    assert_eq!(effects, vec![Effect::OpenUrl("https://two.test".into())]);
    assert!(model.modals.is_empty(), "the picker stayed open");
}

#[test]
fn a_box_with_no_display_copies_the_link_and_says_why() {
    // The fleet case: xdg-open over SSH either fails or opens a browser at the far end of
    // the connection, which is not where the person is sitting.
    let mut model = with_links("https://example.com/a", "");
    model.url_action = tui_do_ui::model::UrlAction::Copy;
    let effects = press(&mut model, 'o');
    assert_eq!(
        effects,
        vec![Effect::CopyToClipboard("https://example.com/a".into())]
    );
    let toast = model.status.toast.as_ref().map(|t| t.text.as_str());
    assert!(
        toast.is_some_and(|t| t.contains("Copied") && t.contains("no display")),
        "the toast does not say why it copied: {toast:?}"
    );
}

#[test]
fn o_with_nothing_selected_does_not_panic() {
    let mut model = loaded();
    answer(&mut model, Vec::new());
    let effects = press(&mut model, 'o');
    assert!(effects.is_empty());
}

/// Archiving elsewhere the project you are looking at must not strand the cursor.
///
/// Driven on 2026-08-31: five tasks added to `Retirement` in the web UI, `R` in tui-do,
/// navigate into it, archive it in the web UI, `R` again. The project left the sidebar --
/// `sidebar::rows` filters archived ones out -- while the breadcrumb still said
/// `Retirement › List` and its five tasks were still listed, with no way back to them once
/// you left. And the sidebar's arrows died: the selection named a row that was no longer
/// drawn, so `position()` found nothing and every keypress returned without moving.
#[test]
fn a_project_archived_elsewhere_does_not_strand_the_sidebar() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![project(1, "Alpha", 0), project(3, "Retirement", 0)]),
    );
    // Looking at it, the way `g p` then choosing it leaves you.
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
    );
    press(&mut model, 'p');
    for c in "Retirement".chars() {
        press(&mut model, c);
    }
    press_code(&mut model, KeyCode::Enter);
    assert_eq!(model.sidebar.selected, SidebarTarget::Project(ProjectId(3)));

    // Somebody archives it in the web UI, and the next pull says so.
    let mut archived = project(3, "Retirement", 0);
    archived.is_archived = true;
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![project(1, "Alpha", 0), archived]),
    );

    assert_eq!(
        model.sidebar.selected,
        SidebarTarget::AllTasks,
        "the cursor stayed on a project that is no longer in the tree"
    );
    let toast = model.status.toast.as_ref().map(|t| t.text.as_str());
    assert!(
        toast.is_some_and(|t| t.contains("Retirement") && t.contains("archived")),
        "nothing said why the view moved: {toast:?}"
    );
}

/// And the arrows must work even if a selection does somehow point at a missing row.
#[test]
fn the_sidebar_arrows_never_dead_end() {
    let mut model = loaded();
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![project(1, "Alpha", 0)]),
    );
    model.focus = Focus::Sidebar;
    // A target that is not in the tree at all -- the wedged state, forced directly.
    model.sidebar.selected = SidebarTarget::Project(ProjectId(999));

    press(&mut model, 'j');
    assert_ne!(
        model.sidebar.selected,
        SidebarTarget::Project(ProjectId(999)),
        "the sidebar is wedged: j did nothing and there is no way out but a restart"
    );
}

/// What being offline actually says, in full.
const OFFLINE: &str = "could not reach the Vikunja server at https://10.255.255.1:8443";

#[test]
fn a_failure_with_work_queued_says_the_work_is_safe() {
    // BUG-18. `Toast::error` is the register a *rejection* uses, and a rejection has
    // thrown the user's edit away. A push that could not reach the server has thrown
    // nothing away: the entries are in the outbox and go when the network comes back.
    // Said in the same voice, the two are indistinguishable -- and on 2026-09-05 someone
    // driving the offline check read this toast, saw no change in a list whose layout has
    // no priority column, and reported that priority cannot be set offline. It could. All
    // four presses were queued, written to the store, and landed on the server intact.
    let mut model = loaded();
    model.status.queued = 3;
    update(
        &mut model,
        Msg::Sync(SyncEvent::Failed {
            phase: Phase::Push,
            message: OFFLINE.to_string(),
        }),
    );
    let toast = model
        .status
        .toast
        .as_ref()
        .expect("a failed pass is still worth saying");

    assert_ne!(
        toast.level,
        Level::Error,
        "nothing was lost, so the loudest register is the wrong one"
    );
    assert!(
        toast.text.contains("3") && toast.text.contains("queued"),
        "must say how much work is safe: {:?}",
        toast.text
    );
    // BUG-16's lesson, which this must not undo: the diagnosis stays readable in full.
    assert!(
        toast.text.contains(OFFLINE),
        "the whole diagnosis must survive: {:?}",
        toast.text
    );
}

#[test]
fn a_failure_with_nothing_queued_is_still_an_error() {
    // The other half, and what stops the fix above from muffling everything. With no
    // queued work there is nothing to reassure anyone about -- an expired token or an
    // unreachable server is simply bad news, and BUG-16 is the record of what it costs
    // when that news is hard to read.
    let mut model = loaded();
    model.status.queued = 0;
    update(
        &mut model,
        Msg::Sync(SyncEvent::Failed {
            phase: Phase::Pull,
            message: AUTH_FAILURE.to_string(),
        }),
    );
    let toast = model.status.toast.as_ref().expect("bad news is still news");
    assert_eq!(toast.level, Level::Error);
    assert_eq!(toast.text, AUTH_FAILURE);
}

/// The write an effect list asks to store, and its number.
fn write_of(effects: &[Effect]) -> (tui_do_ui::WriteSeq, Mutation) {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Apply { write, mutation } => Some((*write, mutation.clone())),
            _ => None,
        })
        .expect("a write was asked for")
}

/// What the runtime sends once the store holds `write`: the acknowledgement, then the
/// reload. Answers with the reload's task query.
fn store_answers(model: &mut Model, write: tui_do_ui::WriteSeq) -> tui_do_ui::QueryId {
    update(model, Msg::Written(write));
    loaded_query(&update(model, Msg::Reload)).expect("the reload asks for tasks")
}

fn titles(model: &Model) -> Vec<&str> {
    model
        .data
        .tasks
        .iter()
        .map(|task| task.title.as_str())
        .collect()
}

#[test]
fn a_reload_that_read_the_store_before_a_later_write_does_not_bring_its_row_back() {
    // BUG-22. `d` on first and then on second makes two writes, each stored by an effect
    // of its own. The first lands and its reload is issued -- after the acknowledgement,
    // so it can be trusted with the first -- but it reads the store before the second has
    // landed and still finds second open. Taken wholesale, that put second back on screen
    // and a held `d` pressed it again: 17 duplicate writes in 1,767.
    let mut model = loaded();
    let (first, _) = write_of(&press(&mut model, 't'));
    let (second, _) = write_of(&press(&mut model, 't'));
    assert_ne!(first, second, "each write has its own number");
    assert_eq!(selected_title(&model), "third");

    let reload = store_answers(&mut model, first);
    update(
        &mut model,
        Msg::TasksLoaded {
            id: reload,
            tasks: vec![task(2, "second"), task(3, "third")],
        },
    );

    assert_eq!(
        titles(&model),
        ["third"],
        "second was marked done on screen and the store has not answered for it yet"
    );
    assert_eq!(selected_title(&model), "third");
    match write_of(&press(&mut model, 't')).1 {
        Mutation::UpdateTask { after, .. } => {
            assert_eq!(after.id, TaskId(3), "the next press is for the next task");
        }
        other => panic!("wrong mutation: {other:?}"),
    }
}

#[test]
fn a_stale_reload_at_the_end_of_the_list_leaves_nothing_to_press_twice() {
    // The shape that sends a task twice: the last row is pressed, the list is empty, and
    // a stale answer would put the row back as the only one -- selected, because an
    // empty selection falls to the first row.
    let mut model = loaded();
    answer(&mut model, vec![task(1, "first"), task(2, "second")]);
    let (first, _) = write_of(&press(&mut model, 't'));
    press(&mut model, 't');
    assert!(model.data.tasks.is_empty());

    let reload = store_answers(&mut model, first);
    update(
        &mut model,
        Msg::TasksLoaded {
            id: reload,
            tasks: vec![task(2, "second")],
        },
    );

    assert!(model.data.tasks.is_empty(), "{:?}", titles(&model));
    assert!(
        applied(&press(&mut model, 't')).is_none(),
        "nothing is selected, so a further press writes nothing"
    );
}

#[test]
fn once_its_write_is_stored_a_later_load_is_trusted_with_it() {
    // The replay ends. After second's acknowledgement, a load issued afterwards read a
    // store that held the write, so what it says wins -- here another client has reopened
    // second since, and that has to show.
    let mut model = loaded();
    let (first, _) = write_of(&press(&mut model, 't'));
    let (second, _) = write_of(&press(&mut model, 't'));
    store_answers(&mut model, first);
    let reload = store_answers(&mut model, second);
    assert_eq!(
        model.unconfirmed.len(),
        2,
        "nothing is trusted until a later load answers"
    );

    update(
        &mut model,
        Msg::TasksLoaded {
            id: reload,
            tasks: vec![task(2, "second, reopened elsewhere"), task(3, "third")],
        },
    );

    assert!(
        model.unconfirmed.is_empty(),
        "both writes are in what the store said"
    );
    assert_eq!(titles(&model), ["second, reopened elsewhere", "third"]);
    assert_eq!(
        selected_title(&model),
        "third",
        "the cursor stays where it was"
    );
}

#[test]
fn a_load_issued_before_the_acknowledgement_is_still_not_trusted() {
    // The acknowledgement arrives while a load is out. That load may have read the store
    // first, so it is laid over like any other until the reload after it answers.
    let mut model = loaded();
    let (write, _) = write_of(&press(&mut model, 't'));
    let out = model.query_id;
    update(&mut model, Msg::Written(write));
    update(
        &mut model,
        Msg::TasksLoaded {
            id: out,
            tasks: vec![task(1, "first"), task(2, "second"), task(3, "third")],
        },
    );
    assert_eq!(titles(&model), ["second", "third"]);
    assert_eq!(model.unconfirmed.len(), 1);
}

#[test]
fn a_write_the_store_refused_is_no_longer_drawn() {
    // Nothing will ever hold it, so the reload after the failure is the truth: first is
    // back, and the user is told.
    let mut model = loaded();
    let (write, _) = write_of(&press(&mut model, 't'));
    update(
        &mut model,
        Msg::WriteFailed {
            write,
            message: "disk full".to_string(),
        },
    );
    assert!(model.unconfirmed.is_empty());
    assert_eq!(
        model.status.toast.as_ref().map(|toast| toast.level),
        Some(Level::Error)
    );

    let reload = loaded_query(&update(&mut model, Msg::Reload)).expect("a reload");
    update(
        &mut model,
        Msg::TasksLoaded {
            id: reload,
            tasks: vec![task(1, "first"), task(2, "second"), task(3, "third")],
        },
    );
    assert_eq!(titles(&model), ["first", "second", "third"]);
}

#[test]
fn a_replayed_create_is_not_shown_twice() {
    let mut model = loaded();
    press(&mut model, 'n');
    for c in "water the plants".chars() {
        press(&mut model, c);
    }
    let (write, _) = write_of(&press_code(&mut model, KeyCode::Enter));
    let count = |model: &Model| {
        titles(model)
            .iter()
            .filter(|title| **title == "water the plants")
            .count()
    };
    let out = model.query_id;
    let mut stored = task(-1, "water the plants");
    stored.project_id = model.data.tasks[0].project_id;

    // A load that read the store after the create landed but before the model was told
    // its id: the row is there under a provisional id the model does not know yet.
    let found = vec![stored.clone(), task(1, "first")];
    update(
        &mut model,
        Msg::TasksLoaded {
            id: out,
            tasks: found.clone(),
        },
    );
    assert_eq!(count(&model), 1, "{:?}", titles(&model));

    // The same, once it has been told.
    update(
        &mut model,
        Msg::TaskCreated {
            write,
            id: TaskId(-1),
        },
    );
    update(
        &mut model,
        Msg::TasksLoaded {
            id: out,
            tasks: found,
        },
    );
    assert_eq!(count(&model), 1, "{:?}", titles(&model));
    assert_eq!(
        selected_title(&model),
        "water the plants",
        "and it is selected"
    );

    // A load that read the store before the create still shows it, at the top.
    update(
        &mut model,
        Msg::TasksLoaded {
            id: out,
            tasks: vec![task(1, "first")],
        },
    );
    assert_eq!(titles(&model), ["water the plants", "first"]);
    assert_eq!(selected_title(&model), "water the plants");

    let reload = store_answers(&mut model, write);
    update(
        &mut model,
        Msg::TasksLoaded {
            id: reload,
            tasks: vec![task(1, "first"), stored],
        },
    );
    assert!(model.unconfirmed.is_empty());
    assert_eq!(count(&model), 1);
}

#[test]
fn a_replay_does_not_put_back_a_row_the_list_is_not_showing() {
    // `d` and then `u`: the undo is an update to a row the list no longer has, so as it
    // is made it changes nothing, and the reload shows the row again. A load that has the
    // done but not the undo must not have the done laid over it and then find nothing --
    // and must not invent a row the filter hides, either. Only the newest edit to a task
    // is laid on.
    let mut model = loaded();
    let (done, _) = write_of(&press(&mut model, 't'));
    let (undone, _) = write_of(&press(&mut model, 'U'));
    let reload = store_answers(&mut model, done);
    update(
        &mut model,
        Msg::TasksLoaded {
            id: reload,
            tasks: vec![task(2, "second"), task(3, "third")],
        },
    );
    assert_eq!(titles(&model), ["second", "third"]);

    // The load that read the undo shows first again, and is trusted with both.
    let reload = store_answers(&mut model, undone);
    update(
        &mut model,
        Msg::TasksLoaded {
            id: reload,
            tasks: vec![task(1, "first"), task(2, "second"), task(3, "third")],
        },
    );
    assert_eq!(titles(&model), ["first", "second", "third"]);
    assert!(model.unconfirmed.is_empty());
}

#[test]
fn an_undo_laid_over_a_load_that_missed_it_shows_the_row_reopened() {
    // The other order: the load read the store before either write, so it still has
    // first open. The newest edit to it is the undo, which puts it back as it was rather
    // than the done taking it away.
    let mut model = loaded();
    write_of(&press(&mut model, 't'));
    write_of(&press(&mut model, 'U'));
    let out = model.query_id;
    update(
        &mut model,
        Msg::TasksLoaded {
            id: out,
            tasks: vec![task(1, "first"), task(2, "second"), task(3, "third")],
        },
    );
    assert_eq!(titles(&model), ["first", "second", "third"]);
    assert!(!model.data.tasks[0].done);
}

#[test]
fn a_create_the_list_would_not_show_is_not_replayed_onto_it() {
    // Quick-add into another project while looking at this one. The row is drawn at the
    // top as it is made and the first load takes it away, as the reload always did; the
    // replay must not put it back, though its id is known and the load does not have it.
    let mut model = loaded();
    model.query.scope = Scope::Project(ProjectId(1));
    press(&mut model, 'n');
    for c in "for later +Personal".chars() {
        press(&mut model, c);
    }
    let (write, _) = write_of(&press_code(&mut model, KeyCode::Enter));
    assert_eq!(titles(&model)[0], "for later", "drawn as it was made");
    update(
        &mut model,
        Msg::TaskCreated {
            write,
            id: TaskId(-1),
        },
    );

    let out = model.query_id;
    update(
        &mut model,
        Msg::TasksLoaded {
            id: out,
            tasks: vec![task(1, "first"), task(2, "second"), task(3, "third")],
        },
    );
    assert_eq!(titles(&model), ["first", "second", "third"]);
}

#[test]
fn a_query_admits_what_the_store_would_return_for_it() {
    let mut query = tui_do_ui::Query::default();
    let mut open = task(1, "Water the Plants");
    assert!(query.admits(&open));

    open.done = true;
    assert!(!query.admits(&open), "done tasks are hidden by default");
    query.include_done = true;
    assert!(query.admits(&open));

    query.scope = Scope::Project(ProjectId(2));
    assert!(!query.admits(&open), "another project");
    query.scope = Scope::Project(ProjectId(1));
    assert!(query.admits(&open));

    query.search = Some("the plants".to_string());
    assert!(query.admits(&open), "the search folds case, as LIKE does");
    query.search = Some("cactus".to_string());
    assert!(!query.admits(&open));
    query.search = None;

    query.scope = Scope::Favorites;
    assert!(!query.admits(&open));
    open.is_favorite = true;
    assert!(query.admits(&open));

    query.scope = Scope::Label(LabelId(4));
    assert!(!query.admits(&open));
    open.labels.push(Label {
        id: LabelId(4),
        title: "home".to_string(),
        ..Label::default()
    });
    assert!(query.admits(&open));
}

/// BUG-30: a reader at -05:00 on 2026-11-08, the day the bug was found.
fn in_new_york() -> tui_do_core::ZonedTime {
    tui_do_core::zone::pinned(
        chrono::FixedOffset::west_opt(5 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 8, 12, 0, 0)
            .unwrap(),
    )
}

/// A model at -05:00 whose first task is due at `due`.
fn due_in_new_york(due: chrono::DateTime<Utc>) -> Model {
    let mut model = loaded();
    model.now = in_new_york();
    let mut first = task(1, "first");
    first.due_date = Some(due).into();
    answer(&mut model, vec![first]);
    model
}

/// The `before` and `after` of the one `UpdateTask` in `effects`.
fn the_update(effects: &[Effect]) -> (Task, Task) {
    match applied(effects).expect("an update was queued") {
        Mutation::UpdateTask { before, after } => ((**before).clone(), (**after).clone()),
        other => panic!("wrong mutation: {other:?}"),
    }
}

/// Open the edit form, add `suffix` to the title, and save.
fn retitle(model: &mut Model, suffix: &str) -> (Task, Task) {
    let state = open_edit(model);
    type_into(state, suffix);
    let effects = save(model);
    the_update(&effects)
}

/// Open the edit form, replace the Due field with `typed`, and save.
fn retype_due(model: &mut Model, typed: &str) -> (Task, Task) {
    let state = open_edit(model);
    state.focus = tui_do_ui::modal::EditField::Due;
    for _ in 0..40 {
        state.handle(Key::plain(KeyCode::Backspace));
    }
    type_into(state, typed);
    let effects = save(model);
    the_update(&effects)
}

#[test]
fn a_title_only_save_keeps_the_due_date_to_the_instant() {
    // The user's task: due 10 March 2027 at 23:59 EST. Saving a new title stored
    // 2027-03-12T04:59Z, which is 11 March local -- a day lost per save.
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);

    let (before, after) = retitle(&mut model, " renamed");
    assert_eq!(after.title, "first renamed");
    assert_eq!(before.due_date.get(), Some(due));
    assert_eq!(after.due_date.get(), Some(due), "the due date moved");
}

#[test]
fn saving_twice_never_drifts_the_due_date() {
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);

    let (_, once) = retitle(&mut model, " a");
    answer(&mut model, vec![once.clone()]);
    let (_, twice) = retitle(&mut model, " b");

    assert_eq!(once.due_date.get(), Some(due));
    assert_eq!(
        twice.due_date.get(),
        Some(due),
        "a second save moved it again"
    );
    assert_eq!(twice.title, "first a b");
}

#[test]
fn a_due_time_that_is_not_the_default_survives_a_title_only_save() {
    // 14:00 at -05:00. Re-reading the field reset this to 23:59 on every save.
    let due = Utc.with_ymd_and_hms(2027, 3, 10, 19, 0, 0).unwrap();
    let mut model = due_in_new_york(due);

    let (_, after) = retitle(&mut model, "!");
    assert_eq!(after.due_date.get(), Some(due));
}

#[test]
fn an_edited_due_field_is_read_as_a_local_date() {
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);

    let (_, after) = retype_due(&mut model, "2027-03-15");
    // 15 March 23:59 at -05:00.
    assert_eq!(
        after.due_date.get(),
        Some(Utc.with_ymd_and_hms(2027, 3, 16, 4, 59, 0).unwrap())
    );
}

#[test]
fn an_emptied_due_field_still_clears_the_date() {
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);

    let (_, after) = retype_due(&mut model, "");
    assert_eq!(after.due_date.get(), None);
}

#[test]
fn enter_on_the_unchanged_due_prompt_queues_nothing() {
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);

    press(&mut model, 'd');
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(
        applied(&effects).is_none(),
        "the prefill was re-read as a new date: {effects:?}"
    );
    assert_eq!(model.selected_task().unwrap().due_date.get(), Some(due));
    assert!(model.undo.is_empty());
}

#[test]
fn a_date_typed_into_the_due_prompt_still_applies() {
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);

    press(&mut model, 'd');
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
    );
    for c in "2027-03-15".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
    let (_, after) = the_update(&effects);
    assert_eq!(
        after.due_date.get(),
        Some(Utc.with_ymd_and_hms(2027, 3, 16, 4, 59, 0).unwrap())
    );
}

#[test]
fn the_edit_form_shows_the_due_date_on_the_users_own_day() {
    // 2027-03-11T04:59Z is 10 March at -05:00. The form showed `11/03/2027`.
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);
    let state = open_edit(&mut model);
    assert_eq!(state.due.value(), "2027-03-10");
}

#[test]
fn the_due_prompt_shows_the_due_date_on_the_users_own_day() {
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    let mut model = due_in_new_york(due);
    press(&mut model, 'd');
    let Some(Modal::Due(state)) = model.modals.last() else {
        panic!("D opens the due prompt: {:?}", model.modals);
    };
    assert_eq!(state.input.value(), "2027-03-10");
}

#[test]
fn a_due_time_that_is_not_the_default_is_shown_in_the_form() {
    let due = Utc.with_ymd_and_hms(2027, 3, 10, 19, 0, 0).unwrap();
    let mut model = due_in_new_york(due);
    let state = open_edit(&mut model);
    assert_eq!(state.due.value(), "2027-03-10 at 14:00");
}

/// `hour:minute` on `day` November 2026, at `hours` behind UTC.
fn pinned_at(hours: i32, day: u32, hour: u32, minute: u32) -> tui_do_core::ZonedTime {
    tui_do_core::zone::pinned(
        chrono::FixedOffset::west_opt(hours * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, day, hour, minute, 0)
            .unwrap(),
    )
}

#[test]
fn a_tick_while_the_edit_form_is_open_leaves_the_due_date_alone() {
    // The prefill is text made from the clock at opening; the save must not re-read it
    // against a later clock. Across midnight, and across an offset change, both.
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    for later in [pinned_at(5, 9, 0, 10), pinned_at(4, 9, 1, 10)] {
        let mut model = due_in_new_york(due);
        model.now = pinned_at(5, 8, 23, 50);
        let state = open_edit(&mut model);
        type_into(state, " renamed");
        update(&mut model, Msg::Tick(later));
        let (_, after) = the_update(&save(&mut model));
        assert_eq!(after.due_date.get(), Some(due), "after a tick to {later}");
    }
}

#[test]
fn a_tick_while_the_due_prompt_is_open_leaves_the_due_date_alone() {
    let due = Utc.with_ymd_and_hms(2027, 3, 11, 4, 59, 0).unwrap();
    for later in [pinned_at(5, 9, 0, 10), pinned_at(4, 9, 1, 10)] {
        let mut model = due_in_new_york(due);
        model.now = pinned_at(5, 8, 23, 50);
        press_code(&mut model, KeyCode::Char('D'));
        update(&mut model, Msg::Tick(later));
        let effects = press_code(&mut model, KeyCode::Enter);
        assert!(applied(&effects).is_none(), "after a tick to {later}");
        assert_eq!(model.selected_task().unwrap().due_date.get(), Some(due));
    }
}
