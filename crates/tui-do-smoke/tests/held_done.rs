//! Holding `t` down a list sends each task once.
//!
//! BUG-22: each write is stored by an effect of its own, and the reload one write asks for
//! can read the store before a later write has landed. Its answer has that later task
//! open again, and when it was taken wholesale the row came back under a held key and was
//! pressed a second time -- 17 duplicate `POST /tasks/{id}` in 1,767, measured
//! 2026-09-13.
//!
//! The harness runs effects in order, which never produces that interleaving, so this
//! holds the writes back and lets them reach the store one at a time.

// A test reports failure by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{TimeZone, Utc};
use crossterm::event::KeyCode;
use tui_do_core::models::{Project, ProjectId, Task, TaskId};
use tui_do_smoke::Harness;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn now() -> chrono::DateTime<chrono::FixedOffset> {
    Utc.with_ymd_and_hms(2026, 9, 13, 23, 15, 0)
        .unwrap()
        .fixed_offset()
}

const TITLES: [&str; 2] = ["first", "second"];

/// The read and the write a pushed done-toggle makes, for each task.
async fn mount_tasks(server: &MockServer) {
    for (at, title) in TITLES.iter().enumerate() {
        let id = at + 1;
        let body = |done: bool| serde_json::json!({ "id": id, "project_id": 1, "title": title, "done": done });
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/tasks/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body(false)))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/tasks/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body(true)))
            .mount(server)
            .await;
    }
}

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
    let tasks = TITLES
        .iter()
        .enumerate()
        .map(|(at, title)| Task {
            id: TaskId(at as i64 + 1),
            project_id: ProjectId(1),
            title: (*title).to_string(),
            ..Task::default()
        })
        .collect();
    harness.store.upsert_tasks(tasks).await.unwrap();
    harness.start().await;
    harness
}

#[tokio::test]
async fn a_reload_that_overtakes_a_write_does_not_offer_its_task_again() {
    let server = MockServer::start().await;
    mount_tasks(&server).await;
    let mut harness = seeded(&server).await;
    assert_eq!(harness.model.data.tasks.len(), 2);

    harness.hold_writes();
    harness.press(KeyCode::Char('t')).await;
    harness.press(KeyCode::Char('t')).await;
    assert_eq!(harness.held_writes(), 2);
    assert!(harness.model.data.tasks.is_empty());

    // The first write lands, and its reload reads a store the second has not reached:
    // second is still open there.
    harness.release_write().await;
    let showing: Vec<_> = harness
        .model
        .data
        .tasks
        .iter()
        .map(|task| task.title.clone())
        .collect();
    assert!(
        showing.is_empty(),
        "the reload put {showing:?} back on screen, and the next `t` presses it again"
    );

    // The key is still held. With nothing left, nothing is written.
    harness.press(KeyCode::Char('t')).await;
    assert_eq!(harness.held_writes(), 1, "a third write was made");

    harness.release_write().await;
    assert!(harness.model.data.tasks.is_empty());
    assert!(
        harness.model.unconfirmed.is_empty(),
        "every write is in what the store last said"
    );

    let requests = server.received_requests().await.unwrap();
    for id in 1..=TITLES.len() {
        let posts = requests
            .iter()
            .filter(|request| {
                request.method.as_str() == "POST"
                    && request.url.path() == format!("/api/v1/tasks/{id}")
            })
            .count();
        assert_eq!(posts, 1, "task {id} was sent {posts} times");
    }
}
