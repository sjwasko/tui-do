//! The keys that moved to match Vikunja's web shortcuts, driven through to the store.
//!
//! `tui-do-ui`'s keymap tests prove `t` resolves to `ToggleDone`; its update tests prove
//! `ToggleDone` emits a mutation. Neither proves that pressing `t` leaves the task done in
//! the store, that `U` brings it back, or that the old keys stopped doing their old
//! thing -- a remap that left `D` bound beside `d` would pass every resolver test. So each
//! test here presses the key a person presses and reads the store afterwards.
//!
//! The server is a small stateful fake rather than a set of fixed answers. A push reads
//! the server's copy and replays the change onto it, then the store takes the server's
//! answer -- so a fixed `GET` would quietly undo whatever the test just did, and an
//! unmocked one would 404 and roll it back.

// A test reports failure by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{NaiveDate, TimeZone, Utc};
use crossterm::event::KeyCode;
use tui_do_core::models::{Project, ProjectId, Task, TaskId};
use tui_do_core::SyncEvent;
use tui_do_smoke::Harness;
use tui_do_ui::modal::{Modal, PickerKind};
use tui_do_ui::query::Scope;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// 2026-08-31 12:00 UTC, so `tomorrow` is 2026-09-01 at offset zero.
fn now() -> chrono::DateTime<chrono::FixedOffset> {
    Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0)
        .unwrap()
        .fixed_offset()
}

/// What the fake server holds: tasks by id, as the JSON it last accepted.
#[derive(Clone, Default)]
struct Tasks(Arc<Mutex<HashMap<i64, serde_json::Value>>>);

/// The task id at the end of `/api/v1/tasks/{id}`.
fn task_id(request: &Request) -> i64 {
    request
        .url
        .path()
        .rsplit('/')
        .next()
        .and_then(|id| id.parse().ok())
        .expect("a task id in the path")
}

struct Get(Tasks);
impl Respond for Get {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        match self.0 .0.lock().unwrap().get(&task_id(request)) {
            Some(task) => ResponseTemplate::new(200).set_body_json(task),
            None => ResponseTemplate::new(404),
        }
    }
}

/// `POST /tasks/{id}`: the body replaces the task, and is echoed back.
struct Update(Tasks);
impl Respond for Update {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let id = task_id(request);
        let mut body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["id"] = id.into();
        self.0 .0.lock().unwrap().insert(id, body.clone());
        ResponseTemplate::new(200).set_body_json(body)
    }
}

struct Delete(Tasks);
impl Respond for Delete {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        match self.0 .0.lock().unwrap().remove(&task_id(request)) {
            Some(_) => ResponseTemplate::new(200).set_body_json(serde_json::json!({})),
            None => ResponseTemplate::new(404),
        }
    }
}

/// `PUT /projects/{id}/tasks`: a create, answered with a server-assigned id.
struct Create(Tasks);
impl Respond for Create {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut tasks = self.0 .0.lock().unwrap();
        let id = 100 + tasks.len() as i64;
        let mut body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["id"] = id.into();
        tasks.insert(id, body.clone());
        ResponseTemplate::new(201).set_body_json(body)
    }
}

async fn mount(server: &MockServer, tasks: &Tasks) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/tasks/\d+$"))
        .respond_with(Get(tasks.clone()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v1/tasks/\d+$"))
        .respond_with(Update(tasks.clone()))
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path_regex(r"^/api/v1/tasks/\d+$"))
        .respond_with(Delete(tasks.clone()))
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/api/v1/projects/\d+/tasks$"))
        .respond_with(Create(tasks.clone()))
        .mount(server)
        .await;
}

fn task(id: i64, project: i64, title: &str, done: bool) -> Task {
    Task {
        id: TaskId(id),
        project_id: ProjectId(project),
        title: title.to_string(),
        done,
        ..Task::default()
    }
}

/// Two projects -- "Work" holding tasks 1 and 2, "Home" holding task 3 -- plus whatever
/// `extra` adds, on the server and in the store alike, with the interface started on
/// "All tasks".
async fn seeded(extra: Vec<Task>) -> (MockServer, Harness) {
    let server = MockServer::start().await;
    let fake = Tasks::default();
    mount(&server, &fake).await;

    let mut harness = Harness::new(&server.uri(), now()).unwrap();
    harness
        .store
        .upsert_projects(vec![
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
        ])
        .await
        .unwrap();
    let mut tasks = vec![
        task(1, 1, "write the report", false),
        task(2, 1, "file the expenses", false),
        task(3, 2, "fix the gate", false),
    ];
    tasks.extend(extra);
    {
        let mut held = fake.0.lock().unwrap();
        for task in &tasks {
            held.insert(task.id.get(), serde_json::to_value(task).unwrap());
        }
    }
    harness.store.upsert_tasks(tasks).await.unwrap();
    harness.start().await;
    assert_eq!(harness.model.query.scope, Scope::All);
    (server, harness)
}

/// Move the cursor down until `title` is selected.
async fn select(harness: &mut Harness, title: &str) {
    for _ in 0..=harness.model.data.tasks.len() {
        if harness.model.selected_task().map(|t| t.title.as_str()) == Some(title) {
            return;
        }
        harness.press(KeyCode::Char('j')).await;
    }
    panic!(
        "{title:?} is not in the list: {:?}",
        harness
            .model
            .data
            .tasks
            .iter()
            .map(|t| &t.title)
            .collect::<Vec<_>>()
    );
}

async fn stored(harness: &Harness, id: i64) -> Option<Task> {
    harness.store.task(TaskId(id)).await.unwrap()
}

fn toast(harness: &Harness) -> String {
    harness
        .model
        .status
        .toast
        .as_ref()
        .map_or_else(String::new, |toast| toast.text.clone())
}

/// No push failed or was rolled back: the store reflects what the key asked for, not
/// what a 404 put back.
fn no_failures(harness: &Harness) {
    let bad: Vec<_> = harness
        .events
        .iter()
        .filter(|event| matches!(event, SyncEvent::Failed { .. } | SyncEvent::Rejected { .. }))
        .collect();
    assert!(bad.is_empty(), "a push failed: {bad:?}");
}

#[tokio::test]
async fn t_marks_done_and_capital_u_undoes_and_ctrl_r_redoes() {
    let (_server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "fix the gate").await;

    harness.press(KeyCode::Char('t')).await;
    assert!(
        stored(&harness, 3).await.unwrap().done,
        "`t` did not mark it done"
    );
    assert!(
        harness.model.modals.is_empty(),
        "`t` opened {:?}",
        harness.model.modals
    );

    harness.press(KeyCode::Char('U')).await;
    assert!(
        !stored(&harness, 3).await.unwrap().done,
        "`U` did not undo the done"
    );

    harness.press_ctrl('r').await;
    assert!(
        stored(&harness, 3).await.unwrap().done,
        "`C-r` did not redo the done"
    );
    no_failures(&harness);
}

#[tokio::test]
async fn lower_u_is_no_longer_undo() {
    let (_server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "write the report").await;
    harness.press(KeyCode::Char('t')).await;
    assert!(stored(&harness, 1).await.unwrap().done);

    // `u` now opens the task's project. The task is done and hidden, so there is
    // nothing selected that is task 1 -- whatever `u` does, it must not reopen it.
    harness.press(KeyCode::Char('u')).await;
    assert!(stored(&harness, 1).await.unwrap().done, "`u` still undoes");
}

#[tokio::test]
async fn d_opens_the_due_prompt_and_sets_the_due_date() {
    let (_server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "write the report").await;

    harness.press(KeyCode::Char('d')).await;
    assert!(
        matches!(harness.model.modals.last(), Some(Modal::Due(_))),
        "`d` opened {:?} rather than the due prompt",
        harness.model.modals
    );
    assert!(
        !stored(&harness, 1).await.unwrap().done,
        "`d` still marks the task done"
    );

    harness.type_text("tomorrow").await;
    harness.press(KeyCode::Enter).await;
    assert!(harness.model.modals.is_empty());

    let task = stored(&harness, 1).await.unwrap();
    let due = task.due_date.get().expect("a due date in the store");
    assert_eq!(
        due.date_naive(),
        NaiveDate::from_ymd_opt(2026, 9, 1).unwrap()
    );
    assert!(!task.done);
    no_failures(&harness);
}

#[tokio::test]
async fn n_adds_a_task() {
    let (_server, mut harness) = seeded(Vec::new()).await;

    harness.press(KeyCode::Char('n')).await;
    assert!(
        matches!(harness.model.modals.last(), Some(Modal::Add(_))),
        "`n` opened {:?} rather than the add prompt",
        harness.model.modals
    );
    harness.type_text("buy stamps").await;
    harness.press(KeyCode::Enter).await;

    let all = harness
        .store
        .tasks(Default::default(), Default::default())
        .await
        .unwrap();
    let added: Vec<_> = all.iter().filter(|t| t.title == "buy stamps").collect();
    assert_eq!(added.len(), 1, "store holds {all:?}");
    assert!(added[0].id.get() > 0, "the create was never adopted");
    assert_eq!(
        harness.model.selected_task().map(|t| t.id),
        Some(added[0].id)
    );
    no_failures(&harness);
}

#[tokio::test]
async fn u_opens_the_tasks_project_and_keeps_it_selected() {
    let (_server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "fix the gate").await;

    harness.press(KeyCode::Char('u')).await;

    assert_eq!(harness.model.query.scope, Scope::Project(ProjectId(2)));
    let listed: Vec<_> = harness
        .model
        .data
        .tasks
        .iter()
        .map(|t| (t.id, t.project_id))
        .collect();
    assert_eq!(listed, vec![(TaskId(3), ProjectId(2))]);
    assert_eq!(harness.model.list.selected, Some(TaskId(3)));
    assert_eq!(toast(&harness), "Showing Home");

    harness.press(KeyCode::Char('u')).await;
    assert_eq!(harness.model.query.scope, Scope::Project(ProjectId(2)));
    assert_eq!(toast(&harness), "Already showing Home");
}

#[tokio::test]
async fn u_keeps_the_selection_when_it_is_not_the_first_row() {
    // In "Work" the task sits second, so a selection lost in the reload would fall back
    // to the first row and name a different task.
    let (_server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "file the expenses").await;
    harness.press(KeyCode::Char('u')).await;

    assert_eq!(harness.model.query.scope, Scope::Project(ProjectId(1)));
    assert_eq!(harness.model.data.tasks.len(), 2);
    assert_eq!(
        harness.model.selected_task().map(|t| t.title.as_str()),
        Some("file the expenses")
    );
}

#[tokio::test]
async fn z_d_shows_and_hides_completed_tasks() {
    let (_server, mut harness) = seeded(vec![task(4, 1, "already finished", true)]).await;
    let showing = |harness: &Harness| {
        harness
            .model
            .data
            .tasks
            .iter()
            .any(|t| t.title == "already finished")
    };
    assert!(!harness.model.query.include_done);
    assert!(!showing(&harness));

    harness.type_text("zd").await;
    assert!(harness.model.query.include_done);
    assert!(showing(&harness), "`z d` did not show the done task");

    harness.type_text("zd").await;
    assert!(!harness.model.query.include_done);
    assert!(!showing(&harness), "`z d` did not hide it again");
}

#[tokio::test]
async fn delete_removes_the_selected_task() {
    let (server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "file the expenses").await;

    // No confirmation: `x` and `Delete` delete at once and lean on undo.
    harness.press(KeyCode::Delete).await;
    assert!(
        harness.model.modals.is_empty(),
        "Delete opened {:?}",
        harness.model.modals
    );
    assert!(
        stored(&harness, 2).await.is_none(),
        "task 2 is still stored"
    );
    assert!(stored(&harness, 1).await.is_some());
    assert!(stored(&harness, 3).await.is_some());
    assert!(!harness.model.data.tasks.iter().any(|t| t.id == TaskId(2)));

    let deletes = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE" && r.url.path() == "/api/v1/tasks/2")
        .count();
    assert_eq!(deletes, 1);
    no_failures(&harness);
}

#[tokio::test]
async fn x_also_deletes() {
    let (_server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "write the report").await;
    harness.press(KeyCode::Char('x')).await;
    assert!(stored(&harness, 1).await.is_none());
    no_failures(&harness);
}

#[tokio::test]
async fn the_old_keys_no_longer_do_their_old_thing() {
    let (server, mut harness) = seeded(Vec::new()).await;
    select(&mut harness, "write the report").await;
    let before = stored(&harness, 1).await.unwrap();

    // `D` was "set due date".
    harness.press(KeyCode::Char('D')).await;
    assert!(
        !harness
            .model
            .modals
            .iter()
            .any(|m| matches!(m, Modal::Due(_))),
        "`D` still opens the due prompt"
    );
    harness.press(KeyCode::Esc).await;

    // `a` was "add a task".
    harness.press(KeyCode::Char('a')).await;
    assert!(
        !harness
            .model
            .modals
            .iter()
            .any(|m| matches!(m, Modal::Add(_))),
        "`a` still opens the add prompt"
    );
    harness.press(KeyCode::Esc).await;

    // `g l` was "go to a label".
    harness.type_text("gl").await;
    assert!(
        !harness
            .model
            .modals
            .iter()
            .any(|m| matches!(m, Modal::Picker(p) if p.kind == PickerKind::Label)),
        "`g l` still opens the label picker"
    );
    harness.model.modals.clear();

    // And `g a` now does.
    harness.type_text("ga").await;
    assert!(
        matches!(
            harness.model.modals.last(),
            Some(Modal::Picker(p)) if p.kind == PickerKind::Label
        ),
        "`g a` opened {:?}",
        harness.model.modals
    );
    harness.model.modals.clear();

    // None of it wrote anything.
    assert_eq!(stored(&harness, 1).await.unwrap(), before);
    assert!(server.received_requests().await.unwrap().is_empty());
}
