//! F3, automated: a label made in the `l` form reaches the task under the server's id.
//!
//! The manual check this replaces is `md/MANUAL-CHECKS2.md` F3, and the reason it was
//! written by hand first is that its failure is silence — no error, no toast, no missing
//! row. The label is listed and ticked, the tick names an id the store renumbered out
//! from under it, and Enter resolves nothing. The only way to see it is to look at the
//! task afterwards, which is what these assert.

// A test reports failure by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{TimeZone, Utc};
use crossterm::event::KeyCode;
use serde_json::json;
use tui_do_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use tui_do_smoke::Harness;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/api/v1";

/// The id the server hands out, which is deliberately nothing like the provisional `-1`.
const ASSIGNED: i64 = 41;

fn now() -> chrono::DateTime<chrono::FixedOffset> {
    Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, 0)
        .unwrap()
        .fixed_offset()
}

/// A harness whose store already holds one project and one task, as a pull would leave it.
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
        .upsert_tasks(vec![Task {
            id: TaskId(1),
            project_id: ProjectId(1),
            title: "fix the token refresh".to_string(),
            ..Task::default()
        }])
        .await
        .unwrap();
    harness.start().await;
    harness
}

/// `PUT /labels` answers with an id of its own choosing, as Vikunja does.
async fn mount_create(server: &MockServer) {
    Mock::given(method("PUT"))
        .and(path(format!("{API}/labels")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": ASSIGNED,
            "title": "next",
            "hex_color": "",
            "created": "2026-08-30T12:00:00Z",
            "updated": "2026-08-30T12:00:00Z"
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_label_made_in_the_form_reaches_the_task_under_the_id_the_server_gave_it() {
    let server = MockServer::start().await;
    mount_create(&server).await;
    // The whole test is this matcher. Only a body naming the *assigned* id is answered,
    // so an attach still carrying the provisional one finds no mock, is answered 404, and
    // fails the assertions below rather than passing quietly.
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/1/labels")))
        .and(body_partial_json(json!({ "label_id": ASSIGNED })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"label_id": ASSIGNED})))
        .expect(1)
        .mount(&server)
        .await;

    let mut harness = seeded(&server).await;

    // `l` on the task, a name nothing matches, and the key that makes it.
    harness.press(KeyCode::Char('l')).await;
    harness.type_text("next").await;
    harness.press_ctrl('n').await;

    // The push has already happened -- `Effect::Apply` ends in one -- so by here the
    // server has named the label and the form is holding whatever it was told.
    let form = label_form(&harness);
    assert_eq!(
        form.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![LabelId(ASSIGNED)],
        "the form's own cloned list still holds the provisional id"
    );
    assert_eq!(
        form.chosen,
        vec![LabelId(ASSIGNED)],
        "the tick was left pointing at an id nothing will match"
    );

    // Enter applies the ticks. This is where the silence would be.
    harness.press(KeyCode::Enter).await;

    let task = harness
        .store
        .task(TaskId(1))
        .await
        .unwrap()
        .expect("the task");
    assert_eq!(
        task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![LabelId(ASSIGNED)],
        "the label never reached the task: {:?}",
        harness.model.status.toast
    );
    assert_eq!(
        harness.store.pending_count().await.unwrap(),
        0,
        "and nothing was left queued"
    );
    // Both mocks are `expect`ed, and wiremock verifies on drop: a request that named the
    // provisional id matched neither.
}

#[tokio::test]
async fn an_attach_the_server_refuses_leaves_the_label_off_the_task() {
    // The other half of the same seam, and the reason the test above cannot simply assert
    // "a request was made": a request that is *refused* must not leave the interface
    // showing a label the task does not carry. 403 is what a detach answers when it has
    // already happened; on an attach it is a genuine refusal, and rolls back.
    let server = MockServer::start().await;
    mount_create(&server).await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/1/labels")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "no"})))
        .expect(1)
        .mount(&server)
        .await;

    let mut harness = seeded(&server).await;
    harness.press(KeyCode::Char('l')).await;
    harness.type_text("next").await;
    harness.press_ctrl('n').await;
    harness.press(KeyCode::Enter).await;

    let task = harness
        .store
        .task(TaskId(1))
        .await
        .unwrap()
        .expect("the task");
    assert!(
        task.labels.is_empty(),
        "a refused attach was rolled back off the task: {:?}",
        task.labels
    );
    // The label itself was created, and is not rolled back with the attach: two
    // mutations, two answers, and the server did make the label.
    let pool = harness.store.label(LabelId(ASSIGNED)).await.unwrap();
    assert!(pool.is_some(), "the create succeeded and stands on its own");
}

#[tokio::test]
async fn a_label_that_already_existed_is_attached_under_its_own_id() {
    // The control. Nothing is created, so no adoption happens, and the same keystrokes
    // must still put the label on the task -- otherwise the test above could pass on a
    // harness that attaches the wrong thing consistently.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/tasks/1/labels")))
        .and(body_partial_json(json!({ "label_id": 7 })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"label_id": 7})))
        .expect(1)
        .mount(&server)
        .await;

    let mut harness = seeded(&server).await;
    harness
        .store
        .upsert_labels(vec![Label {
            id: LabelId(7),
            title: "errands".to_string(),
            ..Label::default()
        }])
        .await
        .unwrap();
    harness.send(tui_do_ui::Msg::Reload).await;

    harness.press(KeyCode::Char('l')).await;
    harness.press(KeyCode::Char(' ')).await;
    harness.press(KeyCode::Enter).await;

    let task = harness
        .store
        .task(TaskId(1))
        .await
        .unwrap()
        .expect("the task");
    assert_eq!(
        task.labels.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![LabelId(7)]
    );
}

/// The open label form, or a panic naming what is on the modal stack instead.
fn label_form(harness: &Harness) -> &tui_do_ui::modal::LabelsState {
    match harness.model.modals.last() {
        Some(tui_do_ui::modal::Modal::Labels(state)) => state,
        other => panic!("the label form is not open: {other:?}"),
    }
}
