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

use criax_api::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use criax_api::{Client, Credentials};
use criax_core::store::{Mutation, Store};
use criax_core::sync::{Sync, SyncEvent};
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use wiremock::matchers::{method, path};
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
    let provisional = created.mutation.subject();

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
    let provisional = created.mutation.subject();

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
            assigned: TaskId(4242),
        }),
        "a create that the server named must say so; saw {seen:?}"
    );
}

#[tokio::test]
async fn an_edit_queued_behind_a_create_reaches_the_real_task() {
    // The user types a task and renames it before the connection comes back. The rename
    // has to arrive at the id the server assigned, not the provisional one.
    let server = MockServer::start().await;
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
    let provisional = created.mutation.subject();
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
            assert_eq!(subject, TaskId(1));
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
async fn a_server_failure_keeps_the_queue_and_the_local_change() {
    let server = MockServer::start().await;
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
async fn a_failure_stops_the_queue_rather_than_sending_what_came_after() {
    // Order is the contract. Sending the second change while the first is unsent would
    // leave the server in a state the queue never described.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/1")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"message": "restarting"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/tasks/2")))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json(2, "second")))
        .expect(0)
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

    assert_eq!(report.sent, 0);
    assert_eq!(report.deferred, 2, "both are still waiting");
    assert_eq!(store.pending_count().await.unwrap(), 2);
}

#[tokio::test]
async fn a_pass_pushes_before_it_pulls() {
    // Otherwise the pull returns the server's pre-edit copy, which then has to be
    // skipped -- and the store spends the gap showing something it has to undo.
    let server = MockServer::start().await;
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
        .task(created.mutation.subject())
        .await
        .unwrap()
        .is_none());
    assert!(events(&mut rx)
        .iter()
        .any(|event| matches!(event, SyncEvent::Rejected { .. })));
}
