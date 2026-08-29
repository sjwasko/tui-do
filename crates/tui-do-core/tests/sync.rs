//! The sync engine against a mock Vikunja.
//!
//! These are the tests that cover rule 5 end to end: a local change lands in the store
//! immediately, reaches the server afterwards, and either settles or is rolled back.
//! The mock exists because the interesting cases -- a 403 on the first of two queued
//! edits, a pull arriving between a write and its push -- are ones a live server cannot
//! be asked for on demand.

// A test reports failure by panicking.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use chrono::TimeZone;
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tui_do_api::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use tui_do_api::{Client, Credentials};
use tui_do_core::store::{LabelFilter, LabelSort, Mutation, Store, Subject, LAST_PULL};
use tui_do_core::sync::{Reach, Sync, SyncEvent};
use wiremock::matchers::{body_partial_json, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/api/v1";

/// A task as the server sends it, zero-time dates and null collections included.
fn task_json(id: i64, title: &str) -> serde_json::Value {
    json!({
        "id": id,
        "project_id": 1,
        "title": title,
        "description": "",
        "done": false,
        "done_at": "0001-01-01T00:00:00Z",
        "due_date": "0001-01-01T00:00:00Z",
        "labels": null,
        "assignees": null,
        "created": "2026-08-01T10:00:00Z",
        "updated": "2026-08-01T10:00:00Z"
    })
}

fn task(id: i64, title: &str) -> Task {
    Task {
        id: TaskId(id),
        project_id: ProjectId(1),
        title: title.to_string(),
        ..Task::default()
    }
}

fn label(id: i64, title: &str) -> Label {
    Label {
        id: LabelId(id),
        title: title.to_string(),
        ..Label::default()
    }
}

/// Mount everything a pull touches, with `tasks` as the (single) page of tasks.
async fn mount_pull(server: &MockServer, tasks: Vec<serde_json::Value>) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/info")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": "v2.5.0",
            "max_items_per_page": 50
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/user")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": 1, "username": "swasko"})),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/projects")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([
                    {"id": 1, "title": "Work", "views": null},
                    {"id": -1, "title": "Favorites", "views": null}
                ]))
                .append_header("x-pagination-total-pages", "1"),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": 7, "title": "errands"}]))
                .append_header("x-pagination-total-pages", "1"),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/tasks")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::Value::Array(tasks))
                .append_header("x-pagination-total-pages", "1"),
        )
        .mount(server)
        .await;
}

/// Mount `GET /tasks/{id}` — the read a push now does before it writes.
///
/// A push replays the local edit onto the server's current copy rather than sending a
/// task read minutes ago, so every test that pushes an `UpdateTask` needs the server to
/// have one. `title` is what the server holds: pass what the edit was based on and the
/// merge is a no-op, pass something else to make it a concurrent change.
async fn mount_task_read(server: &MockServer, id: i64, title: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/tasks/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json(id, title)))
        .mount(server)
        .await;
}

fn engine(server: &MockServer, store: &Store) -> (Sync, UnboundedReceiver<SyncEvent>) {
    let client = Client::builder(server.uri())
        .credentials(Credentials::api_token("tk_test"))
        .build()
        .expect("client");
    let (tx, rx) = unbounded_channel();
    (Sync::new(client, store.clone()).with_events(tx), rx)
}

fn events(rx: &mut UnboundedReceiver<SyncEvent>) -> Vec<SyncEvent> {
    let mut all = Vec::new();
    while let Ok(event) = rx.try_recv() {
        all.push(event);
    }
    all
}

#[tokio::test]
async fn a_pull_fills_the_store_and_records_what_it_learned() {
    let server = MockServer::start().await;
    mount_pull(
        &server,
        vec![
            task_json(1, "write the sync engine"),
            task_json(2, "test it"),
        ],
    )
    .await;
    let store = Store::in_memory().unwrap();
    let (sync, mut rx) = engine(&server, &store);

    let report = sync.pull().await.unwrap();

    assert_eq!(report.tasks, 2);
    assert_eq!(report.labels, 1);
    assert_eq!(report.projects, 2, "the pseudo-project is stored too");
    assert_eq!(store.task_counts().await.unwrap(), (2, 2));
    // Stored so an offline start knows the page size rather than guessing one.
    assert_eq!(store.page_cap().await.unwrap(), Some(50));
    assert!(store.last_pull().await.unwrap().is_some());
    // The sidebar gets Favorites; a write-target picker does not.
    assert_eq!(store.project_counts().await.unwrap(), (1, 1));

    let seen = events(&mut rx);
    assert!(matches!(seen.first(), Some(SyncEvent::Started(_))));
    assert!(seen.iter().any(|e| matches!(e, SyncEvent::Progress { .. })));
}

#[tokio::test]
async fn an_archived_project_and_its_tasks_survive_a_pull() {
    // `GET /projects` omits archived projects unless asked for them, and `retain_projects`
    // deletes every project the listing did not name *and cascades to its tasks*. So the
    // one missing query parameter is a data-loss bug, and it runs on every pull -- both
    // reaches call `pull_lists` unconditionally, so `Reach` does not contain it.
    //
    // The mock answers the two shapes differently, exactly as the server does: ask
    // without the parameter and the archived project is simply not there. That is what
    // makes this a regression test rather than a restatement of the client's behaviour.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/projects")))
        .and(query_param("is_archived", "true"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([
                    {"id": 1, "title": "Work", "views": null},
                    {"id": 2, "title": "Last year", "is_archived": true, "views": null}
                ]))
                .append_header("x-pagination-total-pages", "1"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/projects")))
        .and(query_param_is_missing("is_archived"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": 1, "title": "Work", "views": null}]))
                .append_header("x-pagination-total-pages", "1"),
        )
        .mount(&server)
        .await;
    for (path_suffix, body) in [
        (
            "info",
            json!({"version": "v2.5.0", "max_items_per_page": 50}),
        ),
        ("user", json!({"id": 1, "username": "swasko"})),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/{path_suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    for (path_suffix, body) in [
        ("labels", json!([])),
        (
            "tasks",
            json!([{
                "id": 9, "project_id": 2, "title": "filed away",
                "done": false, "labels": null, "assignees": null,
                "created": "2026-08-01T10:00:00Z", "updated": "2026-08-01T10:00:00Z"
            }]),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/{path_suffix}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(body)
                    .append_header("x-pagination-total-pages", "1"),
            )
            .mount(&server)
            .await;
    }

    let store = Store::in_memory().unwrap();
    let (sync, _rx) = engine(&server, &store);
    sync.pull().await.unwrap();

    let projects = store
        .projects(Default::default(), Default::default())
        .await
        .unwrap();
    assert!(
        projects
            .iter()
            .any(|p| p.id == ProjectId(2) && p.is_archived),
        "the archived project was deleted by the pull that was supposed to fetch it"
    );
    assert_eq!(
        store.task_counts().await.unwrap().0,
        1,
        "the archived project's tasks were cascaded away with it"
    );
}

#[tokio::test]
async fn a_task_a_pull_skipped_stays_inside_the_next_pulls_window() {
    // A pull leaves a task alone when a local change for it is still queued. It had been
    // advancing the watermark past it anyway, so the server's version of that task was
    // dropped and never asked for again -- `updated` only moves when someone edits it
    // *again*, and by then the watermark was long past.
    //
    // On one machine that is a stale row. Across a fleet sharing one server it is worse:
    // the stale row is what the next full-body write sends back, so it silently reverts
    // whatever another box changed on that task.
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "the server's newer title")]).await;
    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "local")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "local")),
            after: Box::new(task(1, "what the user typed")),
        })
        .await
        .unwrap();
    let (sync, _rx) = engine(&server, &store);

    let report = sync.pull().await.unwrap();
    assert_eq!(
        report.skipped, 1,
        "the queued edit should have been protected"
    );

    // The task's own `updated` is 2026-08-01T10:00:00Z. The watermark must not have moved
    // past it, or the next incremental pull asks a window this task is not in.
    let watermark = store.last_pull().await.unwrap().expect("a watermark");
    let task_updated = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 10, 0, 0).unwrap();
    assert!(
        watermark <= task_updated,
        "the watermark moved to {watermark}, past the {task_updated} of a task the pull \
         skipped -- no incremental pull will ever ask for it again"
    );
}

#[tokio::test]
async fn a_pull_does_not_overwrite_an_edit_that_has_not_been_sent() {
    // The window this closes: the user edits, a scheduled pull fires before the push,
    // and the server's older copy lands on top of what they are looking at.
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "the server's older title")]).await;
    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "original")),
            after: Box::new(task(1, "what the user typed")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.pull().await.unwrap();

    assert_eq!(report.skipped, 1);
    assert_eq!(report.tasks, 0);
    assert_eq!(
        store.task(TaskId(1)).await.unwrap().unwrap().title,
        "what the user typed"
    );
}

#[tokio::test]
async fn a_pull_does_not_delete_a_task_created_offline() {
    // Its id is provisional, so it cannot possibly be in the server's list. Retaining
    // naively would delete the task the moment the connection came back.
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "a task the server has")]).await;
    let store = Store::in_memory().unwrap();
    let created = store
        .queue(Mutation::CreateTask {
            task: Box::new(task(0, "typed on a train")),
        })
        .await
        .unwrap();
    let provisional = created.mutation.subject().task().unwrap();

    let (sync, _rx) = engine(&server, &store);
    sync.pull().await.unwrap();

    assert!(
        store.task(provisional).await.unwrap().is_some(),
        "the offline task was deleted by the first pull"
    );
    assert_eq!(store.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_pull_removes_what_the_server_no_longer_has() {
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "still there")]).await;
    let store = Store::in_memory().unwrap();
    store
        .upsert_tasks(vec![task(1, "still there"), task(2, "deleted elsewhere")])
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    sync.pull().await.unwrap();

    assert!(store.task(TaskId(1)).await.unwrap().is_some());
    assert!(store.task(TaskId(2)).await.unwrap().is_none());
}

#[tokio::test]
async fn a_pushed_create_adopts_the_server_id_and_keeps_its_labels() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/projects/1/tasks")))
        .respond_with(ResponseTemplate::new(201).set_body_json(task_json(4242, "buy milk")))
        .expect(1)
        .mount(&server)
        .await;
    // Labels do not travel in the task body, so the create has to follow up.
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/4242/labels")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"label_id": 7})))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut new_task = task(0, "buy milk");
    new_task.labels = vec![label(7, "errands")];
    let created = store
        .queue(Mutation::CreateTask {
            task: Box::new(new_task),
        })
        .await
        .unwrap();
    let provisional = created.mutation.subject().task().unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    // Two entries: the create, and the label attach queued behind it. A task write
    // cannot carry labels, so one entry could not have done both in one request.
    assert_eq!(report.sent, 2);
    assert!(store.task(provisional).await.unwrap().is_none());
    let stored = store.task(TaskId(4242)).await.unwrap().expect("the task");
    assert_eq!(stored.title, "buy milk");
    assert_eq!(
        stored.labels.len(),
        1,
        "the label was lost between the create and the store"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_pushed_create_announces_the_id_it_was_given() {
    // The store swaps the provisional id in its own rows and in anything still queued.
    // What it cannot reach is the interface, which holds that id in its list, its
    // selection and its undo stack. An edit asks for a push-only pass, which ends in
    // `Pushed` and never reloads -- so without this event the next edit is sent as
    // `POST /tasks/-1` and answered `404 This task does not exist`.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/projects/1/tasks")))
        .respond_with(ResponseTemplate::new(201).set_body_json(task_json(4242, "buy milk")))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let created = store
        .queue(Mutation::CreateTask {
            task: Box::new(task(0, "buy milk")),
        })
        .await
        .unwrap();
    let provisional = created.mutation.subject();

    let (sync, mut rx) = engine(&server, &store);
    sync.push().await.unwrap();

    let seen = events(&mut rx);
    assert!(
        seen.contains(&SyncEvent::Adopted {
            provisional,
            assigned: Subject::Task(TaskId(4242)),
        }),
        "a create that the server named must say so; saw {seen:?}"
    );
}

#[tokio::test]
async fn a_label_create_is_sent_and_takes_the_id_the_server_gave_it() {
    // The first attempt writes without reading: a create that has never been sent has no
    // earlier attempt of its own to find, and adopting a label another box legitimately
    // created would silently merge two users' intentions.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/labels")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 41, "title": "next"})))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let created = store
        .queue(Mutation::CreateLabel {
            label: Box::new(label(0, "next")),
        })
        .await
        .unwrap();
    let provisional = created.mutation.subject();

    let (sync, mut rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.sent, 1);
    assert_eq!(store.pending_count().await.unwrap(), 0);
    let labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap();
    assert_eq!(
        labels.len(),
        1,
        "the provisional row is gone; saw {labels:?}"
    );
    assert_eq!(labels[0].id, LabelId(41));

    let seen = events(&mut rx);
    assert!(
        seen.contains(&SyncEvent::Adopted {
            provisional,
            assigned: Subject::Label(LabelId(41)),
        }),
        "a label the server named must say so; saw {seen:?}"
    );
}

#[tokio::test]
async fn a_retried_label_create_adopts_the_one_the_lost_response_made() {
    // Measured on dev 2026-08-29: creating the same title twice answers 201 twice with
    // two different ids, and nothing in the response says which. So a create that has
    // already failed once looks before it writes -- and finding its own earlier attempt
    // is the whole point. `is_already_done` cannot help here and deliberately has no arm
    // for a label create.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels")))
        .and(query_param("s", "next"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "1")
                .set_body_json(vec![json!({"id": 41, "title": "next"})]),
        )
        .expect(1)
        .mount(&server)
        .await;
    // No PUT mock at all: a second create is a test failure, not a fallback.

    let store = Store::in_memory().unwrap();
    let created = store
        .queue(Mutation::CreateLabel {
            label: Box::new(label(0, "next")),
        })
        .await
        .unwrap();
    // One failed attempt, which is what arms the read. `Retry-After: 0` is the server's
    // own way of saying "try again now", and it is how the entry is put past its backoff
    // without the suite sleeping through the five-second first step.
    store
        .defer(
            created.id,
            "connection reset".into(),
            Some(std::time::Duration::ZERO),
        )
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.sent, 1);
    let labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap();
    assert_eq!(labels.len(), 1, "one label, not two; saw {labels:?}");
    assert_eq!(labels[0].id, LabelId(41));
}

#[tokio::test]
async fn a_retried_label_create_whose_read_finds_nothing_still_creates_it() {
    // The other half of the reason the read exists. When the first attempt never reached
    // the server -- a connect timeout, a DNS failure -- there is no earlier attempt to
    // find, and the retry must go on and create the label. An empty answer is a fact
    // about the server, not a failure and not a reason to give up: treating it as either
    // would drop the user's label on the floor every time their connection dropped
    // before the request went out.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels")))
        .and(query_param("s", "next"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "1")
                .set_body_json(Vec::<serde_json::Value>::new()),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/labels")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 41, "title": "next"})))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let created = store
        .queue(Mutation::CreateLabel {
            label: Box::new(label(0, "next")),
        })
        .await
        .unwrap();
    store
        .defer(
            created.id,
            "connection reset".into(),
            Some(std::time::Duration::ZERO),
        )
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.sent, 1);
    let labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap();
    assert_eq!(labels.len(), 1, "the label is created, not lost");
    assert_eq!(labels[0].id, LabelId(41));
}

#[tokio::test]
async fn an_edit_queued_behind_a_create_reaches_the_real_task() {
    // The user types a task and renames it before the connection comes back. The rename
    // has to arrive at the id the server assigned, not the provisional one.
    let server = MockServer::start().await;
    // The rename is renumbered onto the id the server assigned, so that is the copy
    // the push reads before writing.
    mount_task_read(&server, 4242, "buy milk").await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/projects/1/tasks")))
        .respond_with(ResponseTemplate::new(201).set_body_json(task_json(4242, "buy milk")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/4242")))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json(4242, "buy oat milk")))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let created = store
        .queue(Mutation::CreateTask {
            task: Box::new(task(0, "buy milk")),
        })
        .await
        .unwrap();
    let provisional = created.mutation.subject().task().unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(provisional.get(), "buy milk")),
            after: Box::new(task(provisional.get(), "buy oat milk")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.sent, 2);
    assert_eq!(
        store.task(TaskId(4242)).await.unwrap().unwrap().title,
        "buy oat milk"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn an_edit_that_changes_labels_sends_them_through_their_own_endpoints() {
    // `POST /tasks/{id}` ignores the body's labels. Without this the change would show
    // locally, never reach the server, and vanish at the next pull.
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "original").await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json(1, "chores")))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/1/labels")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"label_id": 7})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/tasks/1/labels/9")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut before = task(1, "chores");
    before.labels = vec![label(9, "old")];
    store.upsert_tasks(vec![before.clone()]).await.unwrap();
    let mut after = task(1, "chores");
    after.labels = vec![label(7, "errands")];
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(before),
            after: Box::new(after),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    // The edit, the attach and the detach: three entries, three requests.
    assert_eq!(sync.push().await.unwrap().sent, 3);

    let stored = store.task(TaskId(1)).await.unwrap().unwrap();
    assert_eq!(stored.labels.len(), 1);
    assert_eq!(stored.labels[0].id, LabelId(7));
}

#[tokio::test]
async fn a_rejected_change_is_rolled_back_and_the_user_is_told() {
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "original").await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "code": 4001,
            "message": "The task title cannot be empty."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "original")),
            after: Box::new(task(1, "")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 1);
    assert_eq!(report.sent, 0);
    assert_eq!(
        store.task(TaskId(1)).await.unwrap().unwrap().title,
        "original",
        "a refused change was left on screen"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);

    let rejection = events(&mut rx)
        .into_iter()
        .find(|event| matches!(event, SyncEvent::Rejected { .. }))
        .expect("the user has to be told their edit was undone");
    match rejection {
        SyncEvent::Rejected {
            subject, message, ..
        } => {
            assert_eq!(subject, Subject::Task(TaskId(1)));
            assert!(message.contains("title cannot be empty"), "{message}");
        }
        other => panic!("wrong event: {other:?}"),
    }
}

#[tokio::test]
async fn a_rejection_takes_the_edits_queued_behind_it_for_that_task() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "code": 4003, "message": "You do not have write access to this project."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "original")),
            after: Box::new(task(1, "first edit")),
        })
        .await
        .unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "first edit")),
            after: Box::new(task(1, "second edit")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 2, "the second edit was built on the first");
    assert_eq!(
        store.task(TaskId(1)).await.unwrap().unwrap().title,
        "original"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_push_does_not_revert_what_another_box_changed() {
    // The fleet case. Box A pulled task 1 while it was called "original", the user set a
    // priority on it, and in between box B renamed it on the server. Vikunja replaces the
    // task from the request body and has no conditional write, so sending box A's stale
    // copy would put the old title back and box B's rename would vanish with nothing to
    // show it ever existed.
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "renamed by another box").await;
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let recorder = std::sync::Arc::clone(&sent);
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(move |request: &wiremock::Request| {
            recorder
                .lock()
                .expect("the recorder is not poisoned")
                .push(request.body_json().expect("a JSON body"));
            ResponseTemplate::new(200).set_body_json(task_json(1, "renamed by another box"))
        })
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut before = task(1, "original");
    let mut after = task(1, "original");
    after.priority = 4;
    store.upsert_tasks(vec![before.clone()]).await.unwrap();
    before.updated = Some(chrono::Utc.with_ymd_and_hms(2026, 8, 1, 10, 0, 0).unwrap()).into();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(before),
            after: Box::new(after),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();
    assert_eq!(report.sent, 1);

    let body = sent.lock().expect("the recorder is not poisoned").clone();
    let body = body.first().expect("one write was sent").clone();
    assert_eq!(
        body["title"], "renamed by another box",
        "the push sent a stale title and reverted another box's rename"
    );
    assert_eq!(body["priority"], 4, "the user's own change did not survive");
}

#[tokio::test]
async fn a_genuine_collision_is_written_and_the_user_is_told() {
    // Both boxes changed the *same* field. The user's value wins -- they are the one
    // sitting there, and refusing it would lose what they just typed -- but overwriting
    // someone in silence is how a fleet loses work nobody can account for.
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "renamed by another box").await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json(1, "renamed here")))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "original")),
            after: Box::new(task(1, "renamed here")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    assert_eq!(sync.push().await.unwrap().sent, 1);

    let overwrote = events(&mut rx)
        .into_iter()
        .find_map(|event| match event {
            SyncEvent::Overwrote { fields, .. } => Some(fields),
            _ => None,
        })
        .expect("a collision on the title should have been reported");
    assert_eq!(overwrote, vec!["title".to_string()]);
}

#[tokio::test]
async fn a_server_failure_keeps_the_queue_and_the_local_change() {
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "original").await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"message": "boom"})))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "original")),
            after: Box::new(task(1, "edited")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.sent, 0);
    assert_eq!(report.rejected, 0);
    assert_eq!(report.deferred, 1);
    assert!(!report.is_complete());
    // The edit is still on screen and still queued: a 500 is not an answer.
    assert_eq!(
        store.task(TaskId(1)).await.unwrap().unwrap().title,
        "edited"
    );
    let pending = store.pending(None).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 1);
    assert!(pending[0].last_error.is_some());
}

#[tokio::test]
async fn a_failure_stops_that_tasks_own_queue() {
    // Order is the contract *within* a task. Sending the second change while the first is
    // unsent would leave the server in a state the queue never described.
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "original").await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"message": "restarting"})))
        // Once, not twice: the second edit to this task must not be tried after the first
        // failed.
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "first")]).await.unwrap();
    for (before, after) in [("first", "first edited"), ("first edited", "twice edited")] {
        store
            .queue(Mutation::UpdateTask {
                before: Box::new(task(1, before)),
                after: Box::new(task(1, after)),
            })
            .await
            .unwrap();
    }

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.sent, 0);
    assert_eq!(report.deferred, 2, "both are still waiting");
    assert_eq!(store.pending_count().await.unwrap(), 2);
}

#[tokio::test]
async fn a_failed_entry_waits_before_it_is_tried_again() {
    // `attempts` was counted from the first commit and never read, so a failing entry was
    // retried at full speed on every pass. The only thing keeping that from being a hot
    // loop against a struggling server was the thirty-second floor on the sync timer.
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "original").await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"message": "restarting"})))
        // Once across two passes: the second pass must find it still inside its backoff.
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "first")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "first")),
            after: Box::new(task(1, "edited")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    assert_eq!(sync.push().await.unwrap().deferred, 1);

    let health = store.queue_health().await.unwrap();
    assert_eq!(health.queued, 1);
    assert_eq!(health.failing, 1, "the user is owed more than a count");
    assert!(health.last_error.is_some());

    // A second pass right away must not touch the server again.
    assert_eq!(sync.push().await.unwrap().deferred, 1);
    assert_eq!(store.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_failure_does_not_hold_back_another_tasks_changes() {
    // Ordering across *different* tasks is not a contract -- two edits to two tasks have
    // no causal relationship -- and treating it as one meant a single unreachable task
    // held back every other change the user had made. On a fleet, where a box can carry a
    // long backlog, that is the difference between one stuck task and a box that has
    // quietly stopped syncing.
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "original").await;
    mount_task_read(&server, 2, "original").await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"message": "restarting"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/2")))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json(2, "second edited")))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store
        .upsert_tasks(vec![task(1, "first"), task(2, "second")])
        .await
        .unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "first")),
            after: Box::new(task(1, "first edited")),
        })
        .await
        .unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(2, "second")),
            after: Box::new(task(2, "second edited")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.sent, 1, "the reachable task's edit should have gone");
    assert_eq!(report.deferred, 1, "only the failing one is still queued");
    assert_eq!(store.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_pass_pushes_before_it_pulls() {
    // Otherwise the pull returns the server's pre-edit copy, which then has to be
    // skipped -- and the store spends the gap showing something it has to undo.
    let server = MockServer::start().await;
    mount_task_read(&server, 1, "original").await;
    mount_pull(&server, vec![task_json(1, "edited")]).await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json(1, "edited")))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "original")]).await.unwrap();
    store
        .queue(Mutation::UpdateTask {
            before: Box::new(task(1, "original")),
            after: Box::new(task(1, "edited")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    let report = sync.once().await.unwrap();

    assert_eq!(report.push.sent, 1);
    assert_eq!(report.pull.skipped, 0, "the change had already been sent");
    assert_eq!(report.pull.tasks, 1);
    assert!(events(&mut rx)
        .iter()
        .any(|event| matches!(event, SyncEvent::Finished(_))));
}

#[tokio::test]
async fn a_delete_is_sent_and_clears_the_queue() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "gone")]).await.unwrap();
    store
        .queue(Mutation::DeleteTask {
            before: Box::new(task(1, "gone")),
        })
        .await
        .unwrap();
    // Gone from the screen before the request is even made.
    assert!(store.task(TaskId(1)).await.unwrap().is_none());

    let (sync, _rx) = engine(&server, &store);
    assert_eq!(sync.push().await.unwrap().sent, 1);
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn project_views_are_fetched_on_demand_rather_than_in_every_pull() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/projects/1/views")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([
                    {"id": 11, "project_id": 1, "title": "List", "view_kind": "list"},
                    {"id": 12, "project_id": 1, "title": "Kanban", "view_kind": "kanban"}
                ]))
                .append_header("x-pagination-total-pages", "1"),
        )
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store
        .upsert_projects(vec![Project {
            id: ProjectId(1),
            title: "Work".into(),
            ..Project::default()
        }])
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    assert_eq!(sync.pull_project_views(ProjectId(1)).await.unwrap(), 2);

    let stored = store.project(ProjectId(1)).await.unwrap().unwrap();
    assert_eq!(stored.views.len(), 2);
}

#[tokio::test]
async fn a_create_whose_label_attach_fails_is_not_sent_twice() {
    // The reason one entry may only ever be one request. If a create attached its own
    // labels, a transient failure on the attach would leave the entry queued with the
    // task already on the server, and the retry would create it a second time --
    // `PUT /projects/{id}/tasks` has no idempotency key to save us.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/projects/1/tasks")))
        .respond_with(ResponseTemplate::new(201).set_body_json(task_json(4242, "buy milk")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/4242/labels")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"message": "restarting"})))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut new_task = task(0, "buy milk");
    new_task.labels = vec![label(7, "errands")];
    store
        .queue(Mutation::CreateTask {
            task: Box::new(new_task),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let first = sync.push().await.unwrap();
    assert_eq!(first.sent, 1, "the create landed");
    assert_eq!(first.deferred, 1, "the attach did not");

    // The retry must resume at the attach. `.expect(1)` on the create is the assertion:
    // wiremock fails the test on drop if it was called twice.
    let second = sync.push().await.unwrap();
    assert_eq!(second.sent, 0);
    assert_eq!(second.deferred, 1);

    let stored = store.task(TaskId(4242)).await.unwrap().expect("the task");
    assert_eq!(
        stored.labels.len(),
        1,
        "settling the create cascaded away a label attached locally"
    );
    assert_eq!(store.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_delete_the_server_has_already_done_is_not_undone() {
    // Another device deleted the same task first, so the DELETE answers 404. That is
    // the outcome the user asked for. Rolling it back would resurrect the task and toast
    // "this task does not exist" while it reappeared in front of them.
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "code": 4002, "message": "This task does not exist."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_tasks(vec![task(1, "gone")]).await.unwrap();
    store
        .queue(Mutation::DeleteTask {
            before: Box::new(task(1, "gone")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 0);
    assert_eq!(report.sent, 1);
    assert!(
        store.task(TaskId(1)).await.unwrap().is_none(),
        "a deleted task came back"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
    assert!(
        !events(&mut rx)
            .iter()
            .any(|event| matches!(event, SyncEvent::Rejected { .. })),
        "nothing went wrong, so nothing should be reported as having gone wrong"
    );
}

#[tokio::test]
async fn detaching_a_label_that_is_already_gone_is_not_undone() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/tasks/1/labels/9")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "code": 4004, "message": "This label does not exist."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut tagged = task(1, "chores");
    tagged.labels = vec![label(9, "old")];
    store.upsert_tasks(vec![tagged]).await.unwrap();
    store
        .queue(Mutation::DetachLabel {
            task: TaskId(1),
            label: Box::new(label(9, "old")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 0);
    assert_eq!(
        store.task(TaskId(1)).await.unwrap().unwrap().labels.len(),
        0,
        "a label the user removed was put back"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn detaching_a_label_the_server_answers_403_for_is_not_undone() {
    // Measured against dev, and the reason this arm exists: detaching a label that is
    // already detached answers `403 Forbidden` with no code and no message beyond the
    // word. The obvious reading is 404, and a client that assumes it undoes the detach
    // the user asked for every time a lost response makes the push replay.
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/tasks/1/labels/9")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "code": 0, "message": "Forbidden"
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut tagged = task(1, "chores");
    tagged.labels = vec![label(9, "old")];
    store.upsert_tasks(vec![tagged]).await.unwrap();
    store
        .queue(Mutation::DetachLabel {
            task: TaskId(1),
            label: Box::new(label(9, "old")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 0);
    assert_eq!(
        store.task(TaskId(1)).await.unwrap().unwrap().labels.len(),
        0,
        "a label the user removed was put back"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn attaching_a_label_that_is_already_attached_is_not_undone() {
    // Also measured: `400` with Vikunja's code 8001. A 4xx is otherwise final, so
    // without this arm a replayed attach rolls the label back off the task.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/1/labels")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "code": 8001, "message": "This label already exists on the task."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut tagged = task(1, "chores");
    tagged.labels = vec![label(9, "urgent")];
    store.upsert_tasks(vec![tagged]).await.unwrap();
    store
        .queue(Mutation::AttachLabel {
            task: TaskId(1),
            label: Box::new(label(9, "urgent")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 0);
    assert_eq!(
        store.task(TaskId(1)).await.unwrap().unwrap().labels.len(),
        1,
        "a label the user added was taken back off"
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_different_400_on_an_attach_is_still_a_rejection() {
    // The arm is keyed on Vikunja's code, not on the status: a 400 that means something
    // else must still roll back and tell the user.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/1/labels")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "code": 4001, "message": "The label title cannot be empty."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let mut tagged = task(1, "chores");
    tagged.labels = vec![label(9, "urgent")];
    store.upsert_tasks(vec![tagged]).await.unwrap();
    store
        .queue(Mutation::AttachLabel {
            task: TaskId(1),
            label: Box::new(label(9, "urgent")),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 1);
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_create_that_names_a_missing_project_is_still_a_rejection() {
    // The narrow reading of 404: it means the *project* is gone, not that the task is
    // already created, so this one does roll back.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/projects/1/tasks")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "code": 3001, "message": "This project does not exist."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let created = store
        .queue(Mutation::CreateTask {
            task: Box::new(task(0, "orphan")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 1);
    assert!(store
        .task(created.mutation.subject().task().unwrap())
        .await
        .unwrap()
        .is_none());
    assert!(events(&mut rx)
        .iter()
        .any(|event| matches!(event, SyncEvent::Rejected { .. })));
}

/// What the mock was asked for, as a query string.
fn filters_asked_for(requests: &[wiremock::Request]) -> Vec<String> {
    requests
        .iter()
        .filter(|request| request.url.path().ends_with("/tasks"))
        .filter_map(|request| {
            request
                .url
                .query_pairs()
                .find(|(key, _)| key == "filter")
                .map(|(_, value)| value.into_owned())
        })
        .collect()
}

#[tokio::test]
async fn an_incremental_pull_with_no_watermark_fetches_everything() {
    // Every first run. There is nothing to ask "since", and "since the epoch" is the
    // whole listing anyway -- so it is a full pull, and it says so rather than
    // reporting an incremental one that quietly did not skip anything.
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "still there")]).await;
    let store = Store::in_memory().unwrap();
    store
        .upsert_tasks(vec![task(1, "still there"), task(2, "deleted elsewhere")])
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.pull_with(Reach::Incremental).await.unwrap();

    assert_eq!(report.reach, Reach::Full);
    assert!(
        filters_asked_for(&server.received_requests().await.unwrap()).is_empty(),
        "asked the server to filter with no watermark to filter by"
    );
    assert!(
        store.task(TaskId(2)).await.unwrap().is_none(),
        "it retained"
    );
}

#[tokio::test]
async fn an_incremental_pull_asks_only_for_what_changed_since_the_watermark() {
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "edited since")]).await;
    let store = Store::in_memory().unwrap();
    let watermark = chrono::Utc.with_ymd_and_hms(2026, 8, 25, 12, 0, 0).unwrap();
    store
        .set_state(LAST_PULL, watermark.to_rfc3339())
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.pull_with(Reach::Incremental).await.unwrap();

    assert_eq!(report.reach, Reach::Incremental);
    // Two minutes before the watermark, not at it: the watermark comes from this
    // machine's clock and `updated` from the server's, and the two disagreeing by a few
    // seconds would drop a task in the gap permanently -- the next pull asks from an
    // even later point. Re-fetching a handful of already-stored tasks costs one page.
    assert_eq!(
        filters_asked_for(&server.received_requests().await.unwrap()),
        vec!["updated > '2026-08-25T11:58:00Z'".to_string()],
    );
}

#[tokio::test]
async fn an_incremental_pull_does_not_delete_what_it_did_not_mention() {
    // The whole reason `Reach` exists. A filtered listing names what changed, and every
    // unchanged task is absent from it -- so retaining against that list would erase all
    // but the last few days of the user's tasks in one pass.
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "edited since")]).await;
    let store = Store::in_memory().unwrap();
    store
        .upsert_tasks(vec![
            task(1, "edited since"),
            task(2, "untouched for months"),
            task(3, "deleted elsewhere"),
        ])
        .await
        .unwrap();
    store
        .set_state(LAST_PULL, chrono::Utc::now().to_rfc3339())
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    let report = sync.pull_with(Reach::Incremental).await.unwrap();

    assert_eq!(report.removed, 0);
    assert!(store.task(TaskId(2)).await.unwrap().is_some());
    // And the documented cost, asserted so that it is a decision rather than a surprise:
    // a task deleted in another client survives an incremental pull. `R` is what removes
    // it.
    assert!(store.task(TaskId(3)).await.unwrap().is_some());
}

#[tokio::test]
async fn only_a_full_pull_says_that_deletions_have_been_reconciled() {
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "still there")]).await;
    let store = Store::in_memory().unwrap();
    let (sync, _rx) = engine(&server, &store);

    sync.pull().await.unwrap();
    let reconciled = store.last_reconcile().await.unwrap().expect("a full pull");

    // The incremental pull that follows advances the watermark -- everything up to now
    // has been seen -- without advancing the reconcile stamp, because it could not have
    // noticed a deletion.
    sync.pull_with(Reach::Incremental).await.unwrap();

    assert_eq!(store.last_reconcile().await.unwrap(), Some(reconciled));
    assert!(
        store.last_pull().await.unwrap().unwrap() >= reconciled,
        "the watermark did not move"
    );
}

#[tokio::test]
async fn the_watermark_is_stamped_from_before_the_requests_not_after() {
    // A task edited while the pages were being fetched may or may not have landed in one
    // of them. A watermark taken at the end would put it in the past of a pull that
    // never saw it, so the next incremental pull would skip it and it would stay wrong
    // until a full one ran.
    let server = MockServer::start().await;
    mount_pull(&server, vec![task_json(1, "a task")]).await;
    let store = Store::in_memory().unwrap();
    let (sync, _rx) = engine(&server, &store);

    let before = chrono::Utc::now();
    sync.pull().await.unwrap();
    let after = chrono::Utc::now();

    let stamped = store.last_pull().await.unwrap().expect("a watermark");
    assert!(stamped >= before && stamped <= after);
}

#[tokio::test]
async fn a_label_update_replays_the_edit_onto_the_servers_copy() {
    // Read, merge, write -- the same as an `UpdateTask` and for the same reasons. A
    // partial body clears what it omits (measured on dev 2026-08-29: a `POST /labels/12`
    // carrying only `title` cleared `hex_color` to ""), and Vikunja has no conditional
    // write, so a body assembled from what this box last saw reverts whatever another box
    // changed since.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels/41")))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id": 41, "title": "next", "hex_color": "4287f5", "description": ""}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/labels/41")))
        // The assertion that matters: the local `before` and `after` both say `aaaaaa`,
        // and the request has to carry the server's `4287f5` -- the colour another box
        // set, which a rename must not revert.
        .and(body_partial_json(
            json!({"id": 41, "title": "next up", "hex_color": "4287f5"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id": 41, "title": "next up", "hex_color": "4287f5", "description": ""}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    let was = Label {
        id: LabelId(41),
        title: "next".into(),
        hex_color: "aaaaaa".into(),
        ..Label::default()
    };
    store.upsert_labels(vec![was.clone()]).await.unwrap();
    store
        .queue(Mutation::UpdateLabel {
            before: Box::new(was.clone()),
            after: Box::new(Label {
                title: "next up".into(),
                ..was
            }),
        })
        .await
        .unwrap();

    let (sync, _rx) = engine(&server, &store);
    assert_eq!(sync.push().await.unwrap().sent, 1);
    assert_eq!(store.pending_count().await.unwrap(), 0);

    // The server's answer is stored, so the colour the merge preserved is what the label
    // picker shows -- without this the local row still reads `aaaaaa` until the next pull.
    let stored = store.label(LabelId(41)).await.unwrap().expect("the label");
    assert_eq!(stored.title, "next up");
    assert_eq!(stored.hex_color, "4287f5");
}

#[tokio::test]
async fn a_genuine_label_collision_is_written_and_the_user_is_told() {
    // Both boxes renamed the same label. The user's value wins -- refusing would lose what
    // they just typed -- but the field is named, because overwriting someone in silence is
    // how a fleet loses work nobody can account for. The subject is a `Subject`, not a
    // `TaskId`: a label is a thing that can be overwritten too.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels/41")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": 41, "title": "renamed by another box"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/labels/41")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": 41, "title": "renamed here"})),
        )
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_labels(vec![label(41, "next")]).await.unwrap();
    store
        .queue(Mutation::UpdateLabel {
            before: Box::new(label(41, "next")),
            after: Box::new(label(41, "renamed here")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    assert_eq!(sync.push().await.unwrap().sent, 1);

    let overwrote = events(&mut rx)
        .into_iter()
        .find_map(|event| match event {
            SyncEvent::Overwrote { subject, fields } => Some((subject, fields)),
            _ => None,
        })
        .expect("a collision on the title should have been reported");
    assert_eq!(
        overwrote,
        (Subject::Label(LabelId(41)), vec!["title".to_string()])
    );
}

#[tokio::test]
async fn renaming_a_label_that_is_already_gone_is_not_undone() {
    // Measured on dev 2026-08-29: renaming a label another box deleted answers `404` with
    // Vikunja code `8002`, "This label does not exist." A 4xx is otherwise final, so
    // without the arm a replayed rename rolls the local row back to a title the user
    // deliberately changed -- and toasts an error for a label that no longer exists to
    // have a title at all.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels/41")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "code": 8002, "message": "This label does not exist."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_labels(vec![label(41, "next")]).await.unwrap();
    store
        .queue(Mutation::UpdateLabel {
            before: Box::new(label(41, "next")),
            after: Box::new(label(41, "next up")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 0);
    assert_eq!(report.sent, 1);
    assert_eq!(store.pending_count().await.unwrap(), 0);
    assert_eq!(
        store
            .label(LabelId(41))
            .await
            .unwrap()
            .expect("the local row")
            .title,
        "next up",
        "a rename the user asked for was rolled back"
    );
    assert!(
        !events(&mut rx)
            .iter()
            .any(|event| matches!(event, SyncEvent::Rejected { .. })),
        "nothing went wrong, so nothing should be reported as having gone wrong"
    );
}

#[tokio::test]
async fn a_different_404_on_a_label_rename_is_still_a_rejection() {
    // The arm is keyed on Vikunja's code, not on the status alone: a 404 that means
    // something else must still roll back and tell the user, exactly as the attach arm
    // does.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels/41")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "code": 4001, "message": "Something else is missing."
        })))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_labels(vec![label(41, "next")]).await.unwrap();
    store
        .queue(Mutation::UpdateLabel {
            before: Box::new(label(41, "next")),
            after: Box::new(label(41, "next up")),
        })
        .await
        .unwrap();

    let (sync, mut rx) = engine(&server, &store);
    let report = sync.push().await.unwrap();

    assert_eq!(report.rejected, 1);
    assert_eq!(store.pending_count().await.unwrap(), 0);
    assert_eq!(
        store
            .label(LabelId(41))
            .await
            .unwrap()
            .expect("the local row")
            .title,
        "next",
        "a rejected rename must put the old title back"
    );
    assert!(events(&mut rx)
        .iter()
        .any(|event| matches!(event, SyncEvent::Rejected { .. })));
}

#[tokio::test]
async fn a_label_rename_does_not_store_the_answer_while_another_edit_is_queued() {
    // The same guard `Sent::Updated` has. Two renames queued back to back: the first
    // one's answer describes a label the second has already moved past, so storing it
    // would briefly undo an edit the user can see on screen.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels/41")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 41, "title": "next"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/labels/41")))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"message": "boom"})))
        .mount(&server)
        .await;

    let store = Store::in_memory().unwrap();
    store.upsert_labels(vec![label(41, "next")]).await.unwrap();
    store
        .queue(Mutation::UpdateLabel {
            before: Box::new(label(41, "next")),
            after: Box::new(label(41, "once")),
        })
        .await
        .unwrap();
    store
        .queue(Mutation::UpdateLabel {
            before: Box::new(label(41, "once")),
            after: Box::new(label(41, "twice")),
        })
        .await
        .unwrap();

    assert!(
        store.is_pending_label(LabelId(41)).await.unwrap(),
        "a queued rename has to be findable by its subject, or the guard cannot fire"
    );
    // And a label nothing is queued for is not pending, or the guard would never let an
    // answer be stored at all.
    assert!(!store.is_pending_label(LabelId(9)).await.unwrap());

    let (sync, _rx) = engine(&server, &store);
    sync.push().await.unwrap();
    assert_eq!(
        store
            .label(LabelId(41))
            .await
            .unwrap()
            .expect("the local row")
            .title,
        "twice",
        "the second edit was undone by the first's bookkeeping"
    );
}
