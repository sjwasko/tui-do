//! Golden-file rendering tests.
//!
//! The three sizes are the three breakpoint bands: 80×24 is list-only, 120×40 gains the
//! sidebar, 160×50 gains the preview as well. Re-bless them with `CRIAX_BLESS=1 cargo
//! test -p criax-tui --test render` after an intentional change, and read the diff.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use criax_core::models::datetime::Timestamp;
use criax_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use criax_core::store::{ProjectCounts, TaskCount};
use criax_core::Config;
use criax_tui::model::{PaneState, SyncStatus, Toast};
use criax_tui::query::Scope;
use criax_tui::update::{reload_everything, update};
use criax_tui::{view, Model, Msg};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
}

fn at(days: i64) -> Timestamp {
    Timestamp::from(Some(now() + chrono::Duration::days(days)))
}

fn fixture(size: (u16, u16)) -> Model {
    let mut model = Model::new(
        &Config::example(),
        Scope::Project(ProjectId(1)),
        now(),
        size,
    );
    let _ = reload_everything(&mut model);

    let urgent = Label {
        id: LabelId(1),
        title: "urgent".to_string(),
        hex_color: "e05454".to_string(),
        ..Label::default()
    };
    let backend = Label {
        id: LabelId(2),
        title: "backend".to_string(),
        hex_color: "1973ff".to_string(),
        ..Label::default()
    };

    let tasks = vec![
        Task {
            id: TaskId(11),
            project_id: ProjectId(1),
            title: "Fix the token refresh that drops the cookie".to_string(),
            identifier: "WORK-42".to_string(),
            priority: 5,
            due_date: at(-2),
            labels: vec![urgent.clone(), backend.clone()],
            description: "The refresh call needs the cookie set by POST /login.".to_string(),
            ..Task::default()
        },
        Task {
            id: TaskId(12),
            project_id: ProjectId(1),
            title: "Ship the release".to_string(),
            identifier: "WORK-43".to_string(),
            priority: 3,
            due_date: at(1),
            ..Task::default()
        },
        Task {
            id: TaskId(13),
            project_id: ProjectId(2),
            title: "Renew the passport".to_string(),
            identifier: "HOME-7".to_string(),
            due_date: at(30),
            labels: vec![backend],
            ..Task::default()
        },
        Task {
            id: TaskId(14),
            project_id: ProjectId(1),
            title: "A task with no date at all".to_string(),
            identifier: "WORK-44".to_string(),
            ..Task::default()
        },
    ];

    let id = model.query_id;
    update(&mut model, Msg::TasksLoaded { id, tasks });
    update(
        &mut model,
        Msg::ProjectsLoaded(vec![
            Project {
                id: ProjectId(1),
                title: "Work".to_string(),
                ..Project::default()
            },
            Project {
                id: ProjectId(2),
                title: "Home".to_string(),
                ..Project::default()
            },
            Project {
                id: ProjectId(3),
                title: "Errands".to_string(),
                parent_project_id: ProjectId(2),
                ..Project::default()
            },
        ]),
    );
    update(&mut model, Msg::LabelsLoaded(vec![urgent]));
    update(
        &mut model,
        Msg::CountsLoaded(ProjectCounts {
            by_project: [
                (ProjectId(1), TaskCount { open: 3, done: 4 }),
                (ProjectId(2), TaskCount { open: 1, done: 0 }),
            ]
            .into_iter()
            .collect(),
            favorites: TaskCount { open: 2, done: 0 },
        }),
    );
    model.status.last_sync = Some(now() - chrono::Duration::minutes(2));
    model
}

fn draw(model: &Model) -> String {
    let (width, height) = model.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| view(model, frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            let row: String = (0..width)
                .map(|x| {
                    buffer
                        .cell((x, y))
                        .map_or(" ", |cell| cell.symbol())
                        .to_string()
                })
                .collect();
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Compare against the stored screen, or write it when blessing.
fn golden(name: &str, actual: &str) {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "golden", name]
        .iter()
        .collect();
    if std::env::var_os("CRIAX_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} is missing; re-run with CRIAX_BLESS=1", path.display()));
    assert_eq!(
        actual,
        expected,
        "\n{} changed. Read the diff, then re-bless with CRIAX_BLESS=1.\n--- drawn ---\n{actual}",
        path.display()
    );
}

#[test]
fn list_only_at_eighty_columns() {
    let model = fixture((80, 24));
    assert!(model.frames().sidebar.is_none());
    golden("80x24-list.txt", &draw(&model));
}

#[test]
fn sidebar_but_no_preview_in_the_middle_band() {
    let model = fixture((110, 40));
    assert!(model.frames().sidebar.is_some());
    assert!(model.frames().preview.is_none(), "the preview needs 120");
    golden("110x40-sidebar.txt", &draw(&model));
}

#[test]
fn everything_at_a_hundred_and_sixty() {
    let mut model = fixture((160, 50));
    model.panes.preview = PaneState::Shown;
    assert!(model.frames().preview.is_some());
    golden("160x50-full.txt", &draw(&model));
}

#[test]
fn the_help_modal_is_the_keymap() {
    let mut model = fixture((120, 40));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
    );
    golden("120x40-help.txt", &draw(&model));
}

#[test]
fn the_project_picker() {
    let mut model = fixture((120, 40));
    for key in ['g', 'p'] {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
        );
    }
    golden("120x40-picker.txt", &draw(&model));
}

#[test]
fn the_command_palette() {
    let mut model = fixture((120, 40));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE)),
    );
    for c in "la".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    golden("120x40-palette.txt", &draw(&model));
}

#[test]
fn the_search_prompt_sits_on_the_status_line_and_counts_as_it_goes() {
    // A panel in the middle of the screen would cover the list it is filtering.
    let mut model = fixture((80, 24));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
    );
    for c in "renew".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    // The store answers the narrowed query.
    let id = model.query_id;
    let matching: Vec<_> = model
        .data
        .tasks
        .iter()
        .filter(|task| task.title.to_lowercase().contains("renew"))
        .cloned()
        .collect();
    update(
        &mut model,
        Msg::TasksLoaded {
            id,
            tasks: matching,
        },
    );

    let drawn = draw(&model);
    let status = drawn.lines().last().unwrap_or_default();
    assert!(status.starts_with("/renew"), "{status}");
    assert!(status.contains("1 match"), "{status}");
    assert!(status.contains("Esc:cancel"), "{status}");
    // The list is still visible, which is the whole point.
    assert!(drawn.contains("Renew the passport"));
    golden("80x24-search.txt", &draw(&model));
}

#[test]
fn a_filtered_view_says_how_to_leave_it() {
    // Finding your way out of a narrowed list should not require opening the help modal.
    let mut model = fixture((80, 24));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
    );
    for c in "the".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    );
    assert_eq!(model.query.search.as_deref(), Some("the"));

    let drawn = draw(&model);
    let status = drawn.lines().last().unwrap_or_default();
    assert!(status.contains("Esc:clear filter"), "{status}");
    assert!(status.contains("filtered"), "{status}");
    golden("80x24-filtered.txt", &draw(&model));
}

#[test]
fn a_phone_terminal_still_answers_the_two_questions_the_header_is_for() {
    // Termux on a phone, portrait. The tabs and the brand give way; which list this is
    // and whether it is talking to the server do not.
    let model = fixture((45, 20));
    let drawn = draw(&model);
    let header = drawn.lines().next().unwrap_or_default();

    assert!(header.contains("Work"), "the breadcrumb survives: {header}");
    assert!(header.contains('⟳'), "and the sync state: {header}");
    assert!(!header.contains("Kanban"), "the tabs gave way: {header}");
    assert!(
        header.chars().count() <= 45,
        "and nothing overflowed: {header}"
    );
    // The two halves never abut.
    assert!(header.contains("  "), "{header}");

    assert!(model.frames().sidebar.is_none());
    assert!(model.frames().preview.is_none());
    golden("45x20-phone.txt", &draw(&model));
}

#[test]
fn a_phone_terminal_can_still_read_the_help() {
    let mut model = fixture((45, 20));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    // Too short to hold the whole keymap, so the title says how to see the rest.
    assert!(drawn.contains("j/k scrolls"), "{drawn}");
    assert!(drawn.lines().all(|line| line.chars().count() <= 45));
    golden("45x20-help.txt", &draw(&model));
}

#[test]
fn an_empty_list_says_which_kind_of_empty_it_is() {
    let mut model = fixture((80, 24));
    let id = model.query_id;
    update(&mut model, Msg::TasksLoaded { id, tasks: vec![] });
    golden("80x24-empty.txt", &draw(&model));
}

#[test]
fn an_unreachable_server_is_a_status_line_not_a_dead_screen() {
    let mut model = fixture((80, 24));
    model.status.sync = SyncStatus::Failed {
        message: "connection refused".to_string(),
    };
    model.status.queued = 3;
    golden("80x24-offline.txt", &draw(&model));
}

#[test]
fn a_long_message_is_cut_to_the_line_rather_than_corrupting_it() {
    let mut model = fixture((80, 24));
    model.status.toast = Some(Toast::error(
        "Not syncing: config error in /home/swasko/.config/criax/token: could not read \
         the API token file: No such file or directory (os error 2)",
    ));
    let drawn = draw(&model);
    let status = drawn.lines().last().unwrap_or_default();
    assert!(status.chars().count() <= 80, "{status}");
    assert!(status.ends_with('…'), "{status}");
    golden("80x24-long-message.txt", &draw(&model));
}

#[test]
fn a_toast_takes_the_whole_status_line() {
    // The hints are always true and one `?` away. A message telling the user what to do
    // about something is neither, and sharing the line cut it off mid-advice -- which is
    // exactly how "hide it with z s" disappeared on a 70-column terminal.
    let mut model = fixture((80, 24));
    model.status.toast = Some(Toast::info(
        "No room beside the sidebar at 70 columns — hide it with z s",
    ));
    let drawn = draw(&model);
    let status = drawn.lines().last().unwrap_or_default();
    assert!(status.contains("hide it with z s"), "{status}");
    assert!(
        !status.contains("?:help"),
        "the hints stood aside: {status}"
    );
    golden("80x24-toast.txt", &draw(&model));
}

#[test]
fn a_chord_in_flight_is_visible() {
    let mut model = fixture((80, 24));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
    );
    assert_eq!(model.pending.len(), 1);
    golden("80x24-chord.txt", &draw(&model));
}
