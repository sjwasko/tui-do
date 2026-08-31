//! Where the cursor is after `a`.
//!
//! Adding a task selects it, so the next keystroke acts on the thing just created — press
//! `a`, then `p`, and the priority belongs to the new task. That is only true if the
//! selection survives the reload that follows, and the selection is tracked by id.
//!
//! This lives in the smoke crate because neither side can see the bug alone. `tui-do-ui`'s
//! tests answer their own queries with whatever id they choose; the store's tests never
//! press a key. The id the interface selects and the id the store assigns are agreed
//! nowhere except here.

// A test reports failure by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{TimeZone, Utc};
use crossterm::event::KeyCode;
use tui_do_core::models::{Project, ProjectId, Task, TaskId};
use tui_do_smoke::Harness;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn now() -> chrono::DateTime<chrono::FixedOffset> {
    Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0)
        .unwrap()
        .fixed_offset()
}

/// The id the server hands out, deliberately nothing like the provisional one.
const ASSIGNED: i64 = 77;

/// `PUT /projects/1/tasks` accepts the create, as a reachable server would.
///
/// Without this the mock server 404s, `is_permanent` calls that the server's final answer,
/// and the task is rolled back -- correct behaviour that would mask the question being
/// asked here.
async fn mount_create(server: &MockServer, title: &str) {
    // The echoed title has to match what was typed. A hardcoded one is not a harmless
    // fixture: the push replaces the local row with the server's answer, so a mock that
    // echoes the wrong title renames the task under the cursor and an assertion about
    // *which task is selected* then fails for a reason that has nothing to do with the
    // selection.
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/1/tasks"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": ASSIGNED,
            "project_id": 1,
            "title": title,
            "done": false,
        })))
        .mount(server)
        .await;
}

/// A store holding one project and two tasks that sort ahead of anything added later.
///
/// Two, not one, so a wrong answer is distinguishable: falling back to "the first row"
/// picks a *named* task, and the assertion can say which.
async fn seeded(server: &MockServer) -> Harness {
    let mut harness = Harness::new(&server.uri(), now()).unwrap();
    harness
        .store
        .upsert_projects(vec![Project {
            id: ProjectId(1),
            title: "Work".to_string(),
            ..Project::default()
        }])
        .await
        .unwrap();
    harness
        .store
        .upsert_tasks(vec![
            Task {
                id: TaskId(1),
                project_id: ProjectId(1),
                title: "already here, first".to_string(),
                ..Task::default()
            },
            Task {
                id: TaskId(2),
                project_id: ProjectId(1),
                title: "already here, second".to_string(),
                ..Task::default()
            },
        ])
        .await
        .unwrap();
    harness.start().await;
    harness
}

#[tokio::test]
async fn the_task_just_added_is_the_one_selected() {
    let server = MockServer::start().await;
    mount_create(&server, "the one I just typed").await;
    let mut harness = seeded(&server).await;

    // `a`, the title, Enter -- the way a person adds a task.
    harness.press(KeyCode::Char('a')).await;
    harness.type_text("the one I just typed").await;
    harness.press(KeyCode::Enter).await;

    let selected = harness
        .model
        .selected_task()
        .map(|task| task.title.clone())
        .unwrap_or_else(|| "<nothing selected>".to_string());

    assert_eq!(
        selected, "the one I just typed",
        "after adding a task the cursor is on {selected:?} instead of the new task. \
         The next keystroke -- `d`, `x`, `p` -- would act on the wrong task."
    );
}

#[tokio::test]
async fn the_selected_id_is_the_one_the_store_actually_assigned() {
    // The sharper form of the same question. The interface selects an id at the moment it
    // draws the new row optimistically; the store assigns the provisional id a moment
    // later. If those two are not the same value, the selection is dangling and only
    // *looks* right when the new task happens to sort first.
    let server = MockServer::start().await;
    mount_create(&server, "the one I just typed").await;
    let mut harness = seeded(&server).await;

    harness.press(KeyCode::Char('a')).await;
    harness.type_text("the one I just typed").await;
    harness.press(KeyCode::Enter).await;

    let selected_id = harness.model.list.selected;
    let stored_id = harness
        .model
        .data
        .tasks
        .iter()
        .find(|task| task.title == "the one I just typed")
        .map(|task| task.id);

    assert!(
        stored_id.is_some(),
        "the new task is not in the list at all"
    );
    assert_eq!(
        selected_id, stored_id,
        "the selection names {selected_id:?} but the new task is {stored_id:?}"
    );
    assert_ne!(
        selected_id,
        Some(TaskId(0)),
        "the selection is the default id, which no stored task ever has"
    );
}

// This was BUG-1, and it is the test that proved it: before the fix the cursor landed on
// whichever task sorted first rather than the one just created, so the next `x` deleted a
// task the user never chose. Reproduced by hand on 2026-08-31 (the top row, id 2071, took
// the highlight) and now a regression test.
//
// It is the *third* shape of this test and the only one that could fail. With no mock the
// create 404s and rolls back correctly; with the new task sorting first, the dangling
// selection and the "take the first row" fallback land on the same row and the bug is
// invisible. Only an unfavourable sort exposes it -- which is why the seeded tasks carry
// due dates and this one does not.
#[tokio::test]
async fn the_new_task_is_still_selected_when_it_does_not_sort_first() {
    // The previous two tests would pass even if the selection were dangling, so long as
    // the new task happened to land at the top of the sorted list -- the same place the
    // "no selection, take the first row" fallback lands. Giving the existing tasks due
    // dates puts them ahead of a dateless new one under the default layout, so the
    // fallback and the correct answer are now different rows.
    let server = MockServer::start().await;
    mount_create(&server, "dateless, sorts last").await;
    let mut harness = Harness::new(&server.uri(), now()).unwrap();
    harness
        .store
        .upsert_projects(vec![Project {
            id: ProjectId(1),
            title: "Work".to_string(),
            ..Project::default()
        }])
        .await
        .unwrap();
    let due = Utc.with_ymd_and_hms(2026, 8, 1, 9, 0, 0).unwrap();
    harness
        .store
        .upsert_tasks(vec![
            Task {
                id: TaskId(1),
                project_id: ProjectId(1),
                title: "overdue, sorts first".to_string(),
                due_date: Some(due).into(),
                ..Task::default()
            },
            Task {
                id: TaskId(2),
                project_id: ProjectId(1),
                title: "overdue, sorts second".to_string(),
                due_date: Some(due).into(),
                ..Task::default()
            },
        ])
        .await
        .unwrap();
    harness.start().await;

    harness.press(KeyCode::Char('a')).await;
    harness.type_text("dateless, sorts last").await;
    harness.press(KeyCode::Enter).await;

    let selected = harness
        .model
        .selected_task()
        .map(|task| task.title.clone())
        .unwrap_or_else(|| "<nothing selected>".to_string());
    assert_eq!(
        selected, "dateless, sorts last",
        "the cursor landed on {selected:?}. The next `d`, `x` or `p` acts on that task."
    );
}
