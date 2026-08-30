//! Golden-file rendering tests.
//!
//! The three sizes are the three breakpoint bands: 80×24 is list-only, 120×40 gains the
//! sidebar, 160×50 gains the preview as well. Re-bless them with `TUI_DO_BLESS=1 cargo
//! test -p tui-do-ui --test render` after an intentional change, and read the diff.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use ratatui::Terminal;
use tui_do_core::config::columns::{Column, ColumnLayout, ColumnSpec};
use tui_do_core::models::datetime::Timestamp;
use tui_do_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use tui_do_core::store::{ProjectCounts, TaskCount};
use tui_do_core::Config;
use tui_do_ui::model::{Focus, PaneState, SyncStatus, Toast};
use tui_do_ui::query::Scope;
use tui_do_ui::update::{reload_everything, update};
use tui_do_ui::{view, Model, Msg};

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
}

/// The same instant, carrying an offset, which is what the model holds.
fn here() -> chrono::DateTime<chrono::FixedOffset> {
    now().fixed_offset()
}

fn at(days: i64) -> Timestamp {
    Timestamp::from(Some(now() + chrono::Duration::days(days)))
}

fn fixture(size: (u16, u16)) -> Model {
    let mut model = Model::new(
        &Config::example(),
        Scope::Project(ProjectId(1)),
        here(),
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
    model.status.last_sync = Some(here() - chrono::Duration::minutes(2));
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
    if std::env::var_os("TUI_DO_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} is missing; re-run with TUI_DO_BLESS=1", path.display()));
    assert_eq!(
        actual,
        expected,
        "\n{} changed. Read the diff, then re-bless with TUI_DO_BLESS=1.\n--- drawn ---\n{actual}",
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
fn the_help_modal_folds_rather_than_scrolling_where_there_is_room() {
    // Thirty-five bindings and four headings need forty-four rows, so a forty-row
    // terminal used to open help already scrolled -- with `q  Quit` below the fold, on
    // the one screen whose whole job is telling you which key does what.
    let mut model = fixture((120, 40));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    assert!(
        drawn.contains("q / C-c     Quit"),
        "the last binding is off screen:\n{drawn}"
    );
    assert!(
        !drawn.contains("j/k scrolls"),
        "it folded and should not still be advertising a scroll:\n{drawn}"
    );

    // Too narrow to fold, so it scrolls and says so rather than drawing two columns of
    // truncated descriptions.
    let mut narrow = fixture((80, 24));
    update(
        &mut narrow,
        Msg::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
    );
    let drawn = draw(&narrow);
    assert!(drawn.contains("j/k scrolls"), "{drawn}");
    assert!(drawn.contains("Navigation"), "{drawn}");
}

#[test]
fn the_question_before_a_new_label() {
    // The reason is on screen, not just the choice: `y` is the reflex, and what makes it
    // the wrong one -- a pool shared by every project -- is nowhere in the task line the
    // user just typed.
    let mut model = fixture((120, 40));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
    );
    for c in "Ship it *nosuchlabel".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    assert!(drawn.contains("nosuchlabel"), "{drawn}");
    assert!(drawn.contains("y creates"), "{drawn}");
    assert!(
        drawn.contains("every project"),
        "the reason to say no:\n{drawn}"
    );
    golden("120x40-confirm-labels.txt", &drawn);
}

#[test]
fn the_question_grows_a_row_per_name_it_cannot_have() {
    // The box is sized from the list, so a second name must not push the note that says
    // why it is asking off the bottom -- which is the one thing on screen the user cannot
    // work out from the task line they just typed.
    let mut model = fixture((120, 40));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
    );
    for c in "Ship it *nope *alsonope".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    assert!(drawn.contains("nope"), "{drawn}");
    assert!(drawn.contains("alsonope"), "{drawn}");
    assert!(drawn.contains("No such labels"), "plural:\n{drawn}");
    assert!(
        drawn.contains("every project"),
        "the reason survives:\n{drawn}"
    );
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
fn the_label_form_and_its_offer_to_create() {
    // `120x40-labels.txt` was blessed by a test that no longer calls `golden`, so the
    // form's only screen-level coverage was a file nothing read. It is read again here,
    // because this is the modal that grew a footer.
    let mut model = fixture((120, 40));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    assert!(
        !drawn.contains("C-n"),
        "an empty box has nothing to offer:\n{drawn}"
    );
    golden("120x40-labels.txt", &drawn);

    // A name that no label has. `C-n` is advertised nowhere else -- the help modal is
    // rendered from `KEYMAP`, and this is a key the modal handles itself -- so the offer
    // is the whole of its discoverability.
    for c in "next".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    let drawn = draw(&model);
    assert!(drawn.contains("nothing matches"), "{drawn}");
    assert!(drawn.contains("C-n creates \"next\""), "{drawn}");

    // And a name one already has: the pool is global, so a duplicate is never offered.
    for _ in 0.."next".len() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        );
    }
    for c in "urgent".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    let drawn = draw(&model);
    assert!(!drawn.contains("C-n"), "urgent already exists:\n{drawn}");
}

#[test]
fn the_label_edit_form_shows_the_colour_it_is_about_to_save() {
    // Six hex digits are not a colour anybody can read, and the label is about to be
    // worn by every task that carries it -- so the chip is drawn in the colour being
    // typed, in the style the list rows use.
    let mut model = fixture((120, 40));
    for key in ['g', 'l'] {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
        );
    }
    // The picker is where the key is advertised: it has no footer, so its title says so.
    let drawn = draw(&model);
    assert!(drawn.contains("C-e edits"), "{drawn}");

    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)),
    );
    let drawn = draw(&model);
    assert!(drawn.contains("Title"), "{drawn}");
    assert!(drawn.contains("e05454"), "the colour it has now:\n{drawn}");
    assert!(
        drawn.contains("six hex digits"),
        "the rule, before it is broken:\n{drawn}"
    );
    golden("120x40-label-edit.txt", &drawn);

    // And a colour it cannot save says so on the same row, rather than letting the
    // server answer minutes later and roll the rename back with it.
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
    );
    for c in "zz".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    assert!(drawn.contains("A colour is six hex digits"), "{drawn}");
}

#[test]
fn a_label_form_over_an_empty_pool_invites_a_name_rather_than_blaming_the_filter() {
    // What the deleted "No labels exist yet" toast used to say, now said inside the form
    // that can actually do something about it.
    let mut model = fixture((120, 40));
    update(&mut model, Msg::LabelsLoaded(Vec::new()));
    let id = model.query_id;
    update(
        &mut model,
        Msg::TasksLoaded {
            id,
            tasks: vec![Task {
                id: TaskId(12),
                project_id: ProjectId(1),
                title: "Ship the release".to_string(),
                ..Task::default()
            }],
        },
    );
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    assert!(drawn.contains("no labels yet — type a name"), "{drawn}");
    assert!(!drawn.contains("nothing matches"), "{drawn}");
}

/// A model whose label pool is longer than the form's box, filtered to `typed`.
fn overfull_label_form(size: (u16, u16), typed: &str) -> Model {
    let mut model = fixture(size);
    let many: Vec<Label> = (1..=20)
        .map(|n| Label {
            id: LabelId(100 + n),
            title: format!("alpha {n:02}"),
            ..Label::default()
        })
        .collect();
    update(&mut model, Msg::LabelsLoaded(many));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
    );
    for c in typed.chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    model
}

#[test]
fn a_full_label_list_does_not_push_the_offer_off_the_bottom() {
    // *This was nearly broken:* the offer was appended after `lines.truncate(height)`,
    // which cuts from the end, so a list long enough to fill the box would have eaten the
    // only place `C-n` is ever advertised. The row is reserved before the list is laid
    // out instead.
    //
    // `a` matches all twenty fuzzily and is nobody's title, so the list overflows and the
    // offer stands.
    let model = overfull_label_form((120, 40), "a");
    let drawn = draw(&model);
    assert!(drawn.contains("alpha 01"), "the list is drawn:\n{drawn}");
    assert!(drawn.contains("C-n creates \"a\""), "{drawn}");
    // One row fewer of list than without the offer, not one row more of box. Counted by
    // the tick box rather than the title, because the footer names a label too -- `C-e`
    // is offered beside `C-n` whenever there is a row under the cursor to edit.
    assert!(drawn.contains("C-e edits \"alpha 01\""), "{drawn}");
    assert_eq!(
        drawn.matches("[ ] alpha ").count(),
        12,
        "the offer took its row off the list:\n{drawn}"
    );
}

#[test]
fn a_label_form_clipped_to_one_row_keeps_what_is_being_typed() {
    // `centered` clips the box to whatever the terminal has, so the inner area can come
    // out at a single row. What the user is typing outranks the offer to create it --
    // without the `.max(1)` the input line was the line that went.
    let model = overfull_label_form((120, 3), "zz");
    let drawn = draw(&model);
    assert!(drawn.contains("> zz"), "the field survived:\n{drawn}");
    assert!(
        !drawn.contains("C-n"),
        "and the offer is what gave way, not the field:\n{drawn}"
    );
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
fn the_add_prompt_teaches_the_syntax_and_then_shows_the_parse() {
    let mut model = fixture((110, 40));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
    );
    // Nothing typed: the legend, because there is no order to remember and the sigils
    // are the whole of what there is to learn.
    let empty = draw(&model);
    let status = empty.lines().last().unwrap_or_default();
    assert!(status.contains("*label"), "{status}");
    assert!(status.contains("!1-5"), "{status}");

    for c in "Call the VA *urgent !3 tomorrow".chars() {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    let typed = draw(&model);
    let status = typed.lines().last().unwrap_or_default();
    assert!(status.starts_with("+ Call the VA"), "{status}");
    assert!(status.contains("*urgent"), "{status}");
    assert!(status.contains("P3"), "{status}");
    assert!(status.contains("due Tomorrow"), "{status}");
    golden("110x40-add.txt", &draw(&model));
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
        "Not syncing: config error in /home/swasko/.config/tui-do/token: could not read \
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

#[test]
fn the_selection_stays_on_screen_when_rows_wrap() {
    // Reported from a tiled Hyprland window: eighteen tasks in the inbox, ten drawn, and
    // holding Down moved the preview through items 11-18 while the list never scrolled.
    //
    // A row wraps to as many as `rows::MAX_ROW_LINES` lines, so a body with room for
    // eighteen *lines* holds far fewer *tasks*. The scroll maths counted lines, decided
    // everything already fitted, and never moved the offset.
    let mut model = fixture((80, 24));
    // The shipped layouts truncate now, so a layout that wraps has to be asked for --
    // `wrap` is still the user's to set, and the scroll maths still has to survive it.
    model.layouts = vec![ColumnLayout {
        name: "wrapping".to_string(),
        description: None,
        columns: vec![ColumnSpec {
            min_width: Some(20),
            wrap: true,
            ..ColumnSpec::new(Column::Title)
        }],
    }];
    model.layout_ix = 0;
    model.data.tasks = (1..=18)
        .map(|n| Task {
            id: TaskId(n),
            project_id: ProjectId(1),
            title: format!(
                "Task {n} - a title long enough that it has to wrap across more than \
                 one line when the column is narrow"
            ),
            ..Task::default()
        })
        .collect();
    model.list.selected = Some(TaskId(1));
    model.list.offset = 0;

    for _ in 0..17 {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
    }
    assert_eq!(
        model.list.selected,
        Some(TaskId(18)),
        "the selection itself moved"
    );

    let screen = draw(&model);
    assert!(
        screen.contains("Task 18"),
        "the selected task is not on the screen the user is looking at:\n{screen}"
    );
}

/// The style of the row containing `needle`, searched inside one pane only.
///
/// Scoped to a pane on purpose: the preview draws the selected task's title too, so a
/// whole-screen search finds that copy first and reads the style of the wrong thing.
fn style_in_pane(
    model: &Model,
    area: ratatui::layout::Rect,
    needle: &str,
) -> ratatui::style::Style {
    let (width, height) = model.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| view(model, frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    for y in area.y..area.y.saturating_add(area.height) {
        let row: String = (area.x..area.x.saturating_add(area.width))
            .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
            .collect();
        if let Some(at_byte) = row.find(needle) {
            // Byte offset is not a column: rows carry multibyte box-drawing characters.
            let at = area.x + row[..at_byte].chars().count() as u16;
            return buffer.cell((at, y)).unwrap().style();
        }
    }
    panic!("no row containing {needle:?} in {area:?}");
}

#[test]
fn the_pane_being_driven_wears_the_brighter_selection() {
    // Reported as "the colours are backwards": the unfocused row used REVERSED, which
    // swaps each span's own colour into its background. A row carrying a due-soon date
    // therefore became a yellow bar -- on the pane the user was *not* driving, while the
    // focused pane got a quiet dark blue.
    let mut model = fixture((160, 50));
    let title = "Fix the token refresh";
    let theme = model.theme;
    let list = model.frames().list;
    let sidebar = model
        .frames()
        .sidebar
        .expect("the sidebar shows at 160 wide");

    assert_eq!(model.focus, Focus::List, "the list starts focused");
    let driven = style_in_pane(&model, list, title);
    assert_eq!(
        driven.bg,
        theme.selected(true).bg,
        "the focused list should wear the focused selection"
    );
    assert_eq!(
        style_in_pane(&model, sidebar, "Work").bg,
        theme.selected(false).bg,
        "and the sidebar, which is not being driven, the quiet one"
    );

    // Tab off the list. With every pane showing, focus leaves the list.
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
    );
    assert_ne!(model.focus, Focus::List);

    let idle = style_in_pane(&model, list, title);
    assert_eq!(
        idle.bg,
        theme.selected(false).bg,
        "the list should step back once it is not the pane being driven"
    );
    assert_ne!(
        driven.bg, idle.bg,
        "the two states have to be told apart at a glance"
    );
}

#[test]
fn a_selection_never_borrows_the_rows_own_colours() {
    // REVERSED is the specific thing that made an unfocused row loud: it turns every
    // coloured span into a block of that colour.
    let theme = tui_do_ui::theme::Theme::new(tui_do_ui::theme::ColorDepth::TrueColor);
    for focused in [true, false] {
        let style = theme.selected(focused);
        assert!(
            !style.add_modifier.contains(Modifier::REVERSED),
            "selected({focused}) still reverses the row's own spans"
        );
        assert!(
            style.bg.is_some(),
            "selected({focused}) must be findable on the screen"
        );
    }
    assert_ne!(theme.pane(true).fg, theme.pane(false).fg);
}

#[test]
fn the_edit_form_draws_a_description_on_the_lines_the_user_typed() {
    // `wrap` used `split_whitespace`, which ate the newline: two paragraphs came out as
    // one run-on line, so pressing Enter in the form appeared to do nothing at all.
    let mut model = fixture((120, 40));
    let task = tui_do_core::models::Task {
        id: tui_do_core::models::TaskId(1),
        project_id: tui_do_core::models::ProjectId(1),
        title: "with a description".to_string(),
        description: "first paragraph\nsecond paragraph".to_string(),
        ..Default::default()
    };
    model.modals.push(tui_do_ui::modal::Modal::Edit(Box::new(
        tui_do_ui::modal::EditState::new(&task, "Alpha"),
    )));

    let screen = draw(&model);
    assert!(
        screen.contains("first paragraph"),
        "no first line:\n{screen}"
    );
    let first = screen
        .lines()
        .position(|line| line.contains("first paragraph"))
        .expect("first line is on screen");
    let second = screen
        .lines()
        .position(|line| line.contains("second paragraph"))
        .expect("second line is on screen");
    assert_eq!(
        second,
        first + 1,
        "the two paragraphs must be on consecutive rows, not joined:\n{screen}"
    );
}

#[test]
fn a_clipped_modal_scrolls_to_its_highlight_rather_than_truncating() {
    // *This was broken:* every list modal is sized to its content and then clipped by
    // `centered` to whatever the terminal has. On a short window the bottom rows went
    // away with nothing to scroll them back, so the highlight moved off the edge and the
    // arrow keys read as doing nothing at all.
    let mut model = fixture((80, 8));
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
    );
    // The task is priority 5, the last row, and the box has room for five of six.
    let drawn = draw(&model);
    assert!(
        drawn.contains("5  DO NOW"),
        "the highlight is on screen:\n{drawn}"
    );
    assert!(
        !drawn.contains("0  Unset"),
        "and the row it scrolled past is not"
    );

    // Wrapping back to 0 brings the top of the list with it.
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
    );
    let drawn = draw(&model);
    assert!(
        drawn.contains("0  Unset"),
        "the highlight is on screen:\n{drawn}"
    );
    assert!(!drawn.contains("5  DO NOW"));
}

#[test]
fn a_short_sidebar_draws_the_window_the_selection_is_in() {
    let mut model = fixture((120, 12));
    // More projects than the pane has rows, which is the only case that scrolls.
    update(
        &mut model,
        Msg::ProjectsLoaded(
            (1..=15)
                .map(|id| Project {
                    id: ProjectId(id),
                    title: format!("Project {id:02}"),
                    ..Project::default()
                })
                .collect(),
        ),
    );
    update(
        &mut model,
        Msg::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
    );
    for _ in 0..14 {
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
        );
    }
    // The last project, not the first: the pane holds nine rows and the tree is
    // seventeen, so the window has to have moved for this to be drawable at all.
    golden("120x12-sidebar-scrolled.txt", &draw(&model));
}
