//! `update` driven by message sequences.
//!
//! No terminal is involved, which is the point of keeping the layer pure: every one of
//! these would need a pty and a screen scrape in the architecture tui-do replaces.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_do_core::config::columns::ColumnLayout;
use tui_do_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use tui_do_core::store::{Mutation, ProjectCounts, TaskCount, TaskOrder};
use tui_do_core::sync::{Phase, PullReport, PushReport, SyncReport};
use tui_do_core::{Config, SyncEvent};
use tui_do_ui::keymap::Key;
use tui_do_ui::modal::{Modal, ModalView};
use tui_do_ui::model::{Focus, PaneState, SyncStatus};
use tui_do_ui::query::Scope;
use tui_do_ui::update::{reload_everything, update};
use tui_do_ui::{Effect, Model, Msg};

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
}

/// The same instant, carrying an offset, which is what the model holds.
fn here() -> chrono::DateTime<chrono::FixedOffset> {
    now().fixed_offset()
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

    press(&mut model, 'a');
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

    press(&mut model, 'a');
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
    let mut model = loaded();
    press(&mut model, 'a');
    for c in "Thing *nosuchlabel".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);
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
    press(&mut model, 'a');
    let effects = press_code(&mut model, KeyCode::Enter);
    assert!(applied(&effects).is_none());
    assert!(model.undo.is_empty());
}

#[test]
fn marking_done_shows_before_it_is_stored() {
    let mut model = loaded();
    assert!(!model.selected_task().unwrap().done);

    let first = model.selected_task().unwrap().id;
    let effects = press(&mut model, 'd');

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
        .is_some_and(|toast| toast.text.contains("u to undo")));
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

    let effects = press(&mut model, 'u');
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
    let effects = press(&mut model, 'd');
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
    press(&mut model, 'd');
    assert_eq!(model.undo.len(), 1);

    let effects = press(&mut model, 'u');
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
    press(&mut model, 'd');
    press(&mut model, 'u');
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

    press(&mut model, 'u');
    press(&mut model, 'd');
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
        press(&mut model, 'd');
    }
    assert_eq!(model.undo.len(), 3);
    assert_eq!(model.data.tasks.len(), before - 3);

    // Each undo queues the inverse write. The rows return with the reload the runtime
    // fires after every `Effect::Apply`, not from here.
    for _ in 0..3 {
        let effects = press(&mut model, 'u');
        assert!(applied(&effects).is_some());
    }
    assert!(model.undo.is_empty());
    assert_eq!(model.redo.len(), 3);

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
            provisional,
            assigned: TaskId(3901),
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
            provisional: TaskId(-1),
            assigned: TaskId(3901),
        }),
    );

    let effects = press(&mut model, 'd');
    let applied = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Apply(mutation) => Some(mutation),
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
            provisional: TaskId(-9),
            assigned: TaskId(4000),
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
            subject: TaskId(1),
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
fn a_rejected_change_is_not_left_on_the_undo_stack() {
    // The inverse was pushed when the edit was made. The server then refused the edit and
    // the store rolled it back, so the inverse now describes undoing something that never
    // happened -- and `u` would queue a write against a state the server never held.
    let mut model = loaded();
    let effects = press(&mut model, 'd');
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
            Effect::Apply(mutation) => Some(mutation),
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
        .filter(|effect| matches!(effect, Effect::Apply(Mutation::AttachLabel { .. })))
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
        Effect::Apply(Mutation::UpdateTask { after, .. }) => Some(after.project_id),
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
        Effect::Apply(Mutation::UpdateTask { after, .. }) => Some(after.project_id),
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
    press(&mut model, 'a');
    for c in "Something".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    let filed = effects.iter().find_map(|effect| match effect {
        Effect::Apply(Mutation::CreateTask { task }) => Some(task.project_id),
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
    press(&mut model, 'a');
    for c in "Something +#12".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    let filed = effects.iter().find_map(|effect| match effect {
        Effect::Apply(Mutation::CreateTask { task }) => Some(task.project_id),
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
    press(&mut model, 'a');
    for c in "Something +#9".chars() {
        press(&mut model, c);
    }
    let effects = press_code(&mut model, KeyCode::Enter);

    let filed = effects.iter().find_map(|effect| match effect {
        Effect::Apply(Mutation::CreateTask { task }) => Some(task.project_id),
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

    let effects = press(&mut model, 'd');
    assert!(
        effects.contains(&Effect::LoadPending),
        "queueing a change must re-read the queue"
    );

    update(&mut model, Msg::PendingLoaded(4));
    assert_eq!(model.status.queued, 4);
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
            Effect::Apply(mutation) => Some(mutation),
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

    press_code(&mut model, KeyCode::Char('D'));
    let Some(Modal::Due(state)) = model.modals.last() else {
        panic!("D opens the due prompt: {:?}", model.modals);
    };
    assert_eq!(state.input.value(), "24/12/2026");
}

#[test]
fn a_due_date_goes_through_the_quick_add_parser_not_a_date_format() {
    let mut model = loaded();
    press_code(&mut model, KeyCode::Char('D'));
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

    press_code(&mut model, KeyCode::Char('D'));
    for _ in 0..10 {
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
    press_code(&mut model, KeyCode::Char('D'));
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

    for key in ['p', 'D', 'm', 'l', ' '] {
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

    press_code(&mut model, KeyCode::Char('D'));
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
        press_code(&mut model, KeyCode::Char('D'));
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
    press(&mut model, 'a');
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
    press_code(&mut model, KeyCode::Char('D'));
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
    press_code(&mut model, KeyCode::Char('D'));
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
    press_code(&mut model, KeyCode::Char('D'));
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
    press(&mut model, 'a');
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

    press(&mut model, 'd');
    assert_eq!(
        model.selected_task().map(|task| task.id),
        Some(TaskId(2)),
        "the selection should step down to the next row, not jump to the top"
    );
    press(&mut model, 'd');
    assert_eq!(model.selected_task().map(|task| task.id), Some(TaskId(3)));

    // The last row has nothing below it, so the selection steps up rather than vanishing.
    press(&mut model, 'd');
    assert_eq!(model.data.tasks.len(), 0);
    assert_eq!(model.selected_task().map(|task| task.id), None);
}

#[test]
fn marking_done_leaves_the_row_in_place_when_done_tasks_are_shown() {
    // The row belongs in this list either way, so it stays put and simply gains a tick.
    let mut model = loaded();
    answer(&mut model, vec![task(1, "first"), task(2, "second")]);
    press(&mut model, 't');
    answer(&mut model, vec![task(1, "first"), task(2, "second")]);
    assert!(model.query.include_done);

    press(&mut model, 'd');

    assert_eq!(model.selected_task().map(|task| task.id), Some(TaskId(1)));
    assert!(model.selected_task().expect("still selected").done);
    assert_eq!(
        model.data.tasks.len(),
        2,
        "nothing should have left the list"
    );
}
