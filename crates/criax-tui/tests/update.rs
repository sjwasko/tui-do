//! `update` driven by message sequences.
//!
//! No terminal is involved, which is the point of keeping the layer pure: every one of
//! these would need a pty and a screen scrape in the architecture criax replaces.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{TimeZone, Utc};
use criax_core::config::columns::ColumnLayout;
use criax_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use criax_core::store::{ProjectCounts, TaskCount, TaskOrder};
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
    assert!(model.panes.sidebar_visible(model.size.0));

    update(&mut model, Msg::Resize(80, 30));
    assert!(!model.panes.sidebar_visible(80), "auto follows the width");

    press(&mut model, 'z');
    press(&mut model, 's');
    assert_eq!(model.panes.sidebar, PaneState::Shown);
    assert!(model.panes.sidebar_visible(80));

    update(&mut model, Msg::Resize(160, 40));
    update(&mut model, Msg::Resize(80, 30));
    assert!(model.panes.sidebar_visible(80), "a pin survives a resize");
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
    // narrow to honour the pin.
    press(&mut model, 'z');
    press(&mut model, 's');
    press_code(&mut model, KeyCode::BackTab);
    assert_eq!(model.focus, Focus::Sidebar);
    update(&mut model, Msg::Resize(50, 30));
    assert_eq!(model.focus, Focus::List);
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
fn searching_filters_the_query_and_escape_abandons_it() {
    let mut model = loaded();
    press(&mut model, '/');
    assert!(matches!(model.modals.last(), Some(Modal::Search(_))));

    for c in "bug".chars() {
        press(&mut model, c);
    }
    // `q` under a modal is text, not a quit.
    assert!(model.running);

    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(model.modals.is_empty());
    assert_eq!(model.query.search.as_deref(), Some("bug"));
    assert_eq!(
        loaded_query(&effects).map(|_| model.query.filter().search),
        Some(Some("bug".to_string()))
    );

    press(&mut model, '/');
    press(&mut model, 'x');
    press_code(&mut model, KeyCode::Esc);
    assert_eq!(model.query.search.as_deref(), Some("bug"), "unchanged");
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
