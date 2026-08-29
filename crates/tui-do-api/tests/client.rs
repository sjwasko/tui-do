//! Client behaviour against a mock Vikunja.
//!
//! The responses here are shaped from real captures of the dev instance (see
//! `md/2026-08-24-1045-continuity.md`): its 3,876 tasks at a 50-item page cap are exactly
//! 78 pages, and that is the scenario the pagination test reproduces. cria, given the
//! same server, shows 50 tasks and says nothing about the other 3,826.

// A test reports failure by panicking.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;
use tui_do_api::models::{Login, TaskId};
use tui_do_api::{ApiError, Client, Credentials, TaskQuery};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// The dev instance's numbers, so the test fails for the same reason production would.
const DEV_TASK_COUNT: usize = 3876;
const DEV_PAGE_CAP: usize = 50;

/// Serves a paginated task list the way Vikunja does, including the page cap.
struct PaginatedTasks {
    total: usize,
    cap: usize,
    /// Whether to send the `x-pagination-*` headers at all.
    send_headers: bool,
    requests: Arc<AtomicUsize>,
    /// The `per_page` values the client actually asked for.
    requested_per_page: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Respond for PaginatedTasks {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.requests.fetch_add(1, Ordering::SeqCst);

        let query: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
        let page: usize = query
            .get("page")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1)
            .max(1);
        let asked = query
            .get("per_page")
            .map(ToString::to_string)
            .unwrap_or_default();
        self.requested_per_page
            .lock()
            .expect("lock")
            .push(asked.clone());

        // The cap Vikunja applies silently, which is the whole problem.
        let per_page = asked
            .parse::<usize>()
            .unwrap_or(self.cap)
            .min(self.cap)
            .max(1);

        let start = (page - 1) * per_page;
        let items: Vec<_> = (start..self.total.min(start + per_page))
            .map(|i| json!({"id": i + 1, "title": format!("task {}", i + 1), "project_id": 1}))
            .collect();
        let count = items.len();

        let total_pages = self.total.div_ceil(per_page);
        let mut response = ResponseTemplate::new(200).set_body_json(items);
        if self.send_headers {
            response = response
                .insert_header("x-pagination-total-pages", total_pages.to_string().as_str())
                .insert_header("x-pagination-result-count", count.to_string().as_str());
        }
        response
    }
}

/// A client pointed at `server`, authenticated with a static API token.
fn client(server: &MockServer) -> Client {
    Client::builder(server.uri())
        .credentials(Credentials::api_token("tk_test"))
        .build()
        .expect("valid mock server url")
}

#[tokio::test]
async fn a_full_task_fetch_returns_every_page_not_just_the_first() {
    let server = MockServer::start().await;
    let requests = Arc::new(AtomicUsize::new(0));
    let per_page_log = Arc::new(std::sync::Mutex::new(Vec::new()));

    Mock::given(method("GET"))
        .and(path("/api/v1/tasks"))
        .respond_with(PaginatedTasks {
            total: DEV_TASK_COUNT,
            cap: DEV_PAGE_CAP,
            send_headers: true,
            requests: Arc::clone(&requests),
            requested_per_page: Arc::clone(&per_page_log),
        })
        .mount(&server)
        .await;

    let tasks = client(&server)
        .all_tasks(&TaskQuery::new())
        .await
        .expect("fetch should succeed");

    assert_eq!(
        tasks.len(),
        DEV_TASK_COUNT,
        "the fetch stopped early -- this is cria's data-loss bug"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        DEV_TASK_COUNT.div_ceil(DEV_PAGE_CAP),
        "expected one request per page"
    );

    let ids: std::collections::BTreeSet<i64> = tasks.iter().map(|t| t.id.get()).collect();
    assert_eq!(ids.len(), DEV_TASK_COUNT, "pages overlapped or repeated");
    assert_eq!(ids.iter().next_back().copied(), Some(DEV_TASK_COUNT as i64));
}

#[tokio::test]
async fn the_page_size_comes_from_the_server_rather_than_a_guess() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/info"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"version": "v2.5.0", "max_items_per_page": 25})),
        )
        .mount(&server)
        .await;

    let requests = Arc::new(AtomicUsize::new(0));
    let per_page_log = Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks"))
        .respond_with(PaginatedTasks {
            total: 60,
            cap: 25,
            send_headers: true,
            requests: Arc::clone(&requests),
            requested_per_page: Arc::clone(&per_page_log),
        })
        .mount(&server)
        .await;

    let client = client(&server);
    assert!(!client.page_size_is_known());

    let info = client.info().await.expect("info should succeed");
    assert_eq!(info.page_cap(), 25);
    assert!(client.page_size_is_known());
    assert_eq!(client.page_size(), 25);

    let tasks = client.all_tasks(&TaskQuery::new()).await.expect("fetch");
    assert_eq!(tasks.len(), 60);
    assert_eq!(
        *per_page_log.lock().expect("lock"),
        vec!["25", "25", "25"],
        "the client should ask for the server's own cap"
    );
}

#[tokio::test]
async fn a_collection_without_pagination_headers_is_still_read_to_the_end() {
    // Not every Vikunja endpoint sends the headers. Falling back to "a full page might
    // have a successor" costs one extra request and loses nothing.
    let server = MockServer::start().await;
    let requests = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks"))
        .respond_with(PaginatedTasks {
            total: 120,
            cap: DEV_PAGE_CAP,
            send_headers: false,
            requests: Arc::clone(&requests),
            requested_per_page: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
        .mount(&server)
        .await;

    let tasks = client(&server)
        .all_tasks(&TaskQuery::new())
        .await
        .expect("fetch");
    assert_eq!(tasks.len(), 120);
    // 50 + 50 + 20. The first page establishes that this server serves 50 at a time, so
    // the short third page is conclusively the last and costs no extra request.
    assert_eq!(requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_pager_reports_progress_page_by_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks"))
        .respond_with(PaginatedTasks {
            total: 130,
            cap: DEV_PAGE_CAP,
            send_headers: true,
            requests: Arc::new(AtomicUsize::new(0)),
            requested_per_page: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
        .mount(&server)
        .await;

    let mut pager = client(&server).tasks(&TaskQuery::new()).expect("pager");
    let mut sizes = Vec::new();
    while let Some(page) = pager.next_page().await.expect("page") {
        sizes.push(page.items.len());
        assert_eq!(pager.total_pages(), Some(3));
    }
    assert_eq!(sizes, vec![50, 50, 30]);
}

#[tokio::test]
async fn query_parameters_reach_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks"))
        .and(query_param("filter", "done = false"))
        .and(query_param("sort_by", "due_date"))
        .and(query_param("order_by", "asc"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let query = TaskQuery::new()
        .filter("done = false")
        .sort("due_date", tui_do_api::Order::Asc);
    client(&server).all_tasks(&query).await.expect("fetch");
}

#[tokio::test]
async fn logging_in_stores_the_jwt_and_sends_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-1"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "Bearer jwt-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": 1, "username": "swasko"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder(server.uri())
        .credentials(Credentials::password("swasko", "hunter2"))
        .build()
        .expect("client");

    // A password is not a credential the server accepts, so nothing works until login.
    assert!(matches!(
        client.current_user().await,
        Err(ApiError::NotAuthenticated { .. })
    ));

    client
        .login(&Login::new("swasko", "hunter2"))
        .await
        .expect("login should succeed");
    assert_eq!(client.auth_kind(), tui_do_api::AuthKind::Jwt);

    let user = client.current_user().await.expect("user");
    assert_eq!(user.username, "swasko");
}

#[tokio::test]
async fn an_expired_jwt_is_refreshed_and_the_request_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-old"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/token/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-new"})))
        .expect(1)
        .mount(&server)
        .await;
    // The expired token is rejected the way the live server rejects it, code 11 and all.
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "Bearer jwt-old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"code": 11, "message": "missing, malformed, expired or otherwise invalid token provided"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "Bearer jwt-new"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": 1, "username": "swasko"})),
        )
        .mount(&server)
        .await;

    let client = Client::builder(server.uri()).build().expect("client");
    client
        .login(&Login::new("swasko", "hunter2"))
        .await
        .expect("login");

    let user = client
        .current_user()
        .await
        .expect("the 401 should have been answered by a refresh, not surfaced");
    assert_eq!(user.username, "swasko");
}

#[tokio::test]
async fn a_failed_refresh_surfaces_the_original_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-old"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/token/refresh"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"message": "No refresh token provided."})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"code": 11, "message": "missing, malformed, expired or otherwise invalid token provided"}),
        ))
        .mount(&server)
        .await;

    let client = Client::builder(server.uri()).build().expect("client");
    client
        .login(&Login::new("swasko", "hunter2"))
        .await
        .expect("login");

    match client.current_user().await {
        Err(ApiError::Unauthorized { code, .. }) => assert_eq!(
            code,
            Some(11),
            "the user should see why their session died, not why the refresh did"
        ),
        other => panic!("expected Unauthorized, got {other:?}"),
    }
}

#[tokio::test]
async fn an_api_token_session_does_not_attempt_a_refresh() {
    // Refreshing an API token would resend the same rejected credential. The mock has no
    // refresh route at all, so an attempt would show up as an unmatched request.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"code": 11, "message": "nope"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let result = client(&server).current_user().await;
    assert!(matches!(result, Err(ApiError::Unauthorized { .. })));
}

#[tokio::test]
async fn error_bodies_become_typed_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/404"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"code": 4004, "message": "The task does not exist."})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/429"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "17")
                .set_body_json(json!({"message": "Too many requests."})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/500"))
        .respond_with(ResponseTemplate::new(500).set_body_string("<html>oh no</html>"))
        .mount(&server)
        .await;

    let client = client(&server);

    match client.task(TaskId(404)).await {
        Err(err @ ApiError::Rejected { .. }) => {
            assert_eq!(err.code(), Some(4004));
            assert!(!err.is_retryable());
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    match client.task(TaskId(429)).await {
        Err(err @ ApiError::RateLimited { .. }) => {
            assert!(err.is_retryable());
            assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(17)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }

    match client.task(TaskId(500)).await {
        Err(err @ ApiError::Server { .. }) => {
            assert!(err.is_retryable());
            // Non-JSON bodies are surfaced as text rather than a serde complaint.
            assert!(err.to_string().contains("oh no"));
        }
        other => panic!("expected Server, got {other:?}"),
    }
}

#[tokio::test]
async fn a_response_of_the_wrong_shape_is_reported_as_spec_drift() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "not a number"})))
        .mount(&server)
        .await;

    match client(&server).task(TaskId(1)).await {
        Err(err @ ApiError::Deserialize { .. }) => {
            assert!(!err.is_retryable(), "retrying will not change the shape");
        }
        other => panic!("expected Deserialize, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unreachable_server_is_a_retryable_transport_error() {
    // Port 1 is never a Vikunja; the connection is refused before any HTTP happens.
    let client = Client::builder("http://127.0.0.1:1")
        .credentials(Credentials::api_token("tk_test"))
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .expect("client");

    match client.task(TaskId(1)).await {
        Err(err @ ApiError::Transport { .. }) => assert!(err.is_retryable()),
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[tokio::test]
async fn projects_and_labels_paginate_too() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "2")
                .set_body_json(
                    (1..=50)
                        .map(|i| json!({"id": i, "title": "p"}))
                        .collect::<Vec<_>>(),
                ),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects"))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "2")
                .set_body_json(vec![json!({"id": 51, "title": "p"})]),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/labels"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "1")
                .set_body_json(vec![json!({"id": 1, "title": "urgent"})]),
        )
        .mount(&server)
        .await;

    let client = client(&server);
    assert_eq!(client.all_projects().await.expect("projects").len(), 51);
    assert_eq!(client.all_labels().await.expect("labels").len(), 1);
}

#[tokio::test]
async fn the_projects_listing_asks_for_archived_ones_too() {
    // `is_archived=true` reads like a filter and is the opposite: the spec words it "if
    // true, *also* returns all archived projects". The sync engine hands this listing to
    // `retain_projects`, which deletes every project it does not name and cascades to
    // their tasks -- so without this parameter, every pull deletes the user's archived
    // projects and everything in them. The mock answers only when the parameter is
    // present, so dropping it fails this test rather than quietly losing data.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects"))
        .and(query_param("is_archived", "true"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "1")
                .set_body_json(vec![
                    json!({"id": 1, "title": "live"}),
                    json!({"id": 2, "title": "archived", "is_archived": true}),
                ]),
        )
        .expect(1)
        .mount(&server)
        .await;

    let projects = client(&server)
        .all_projects()
        .await
        .expect("the archived project is in the listing");
    assert_eq!(projects.len(), 2);
    assert!(projects.iter().any(|project| project.is_archived));
}

#[tokio::test]
async fn writes_use_the_verbs_the_spec_declares() {
    // Vikunja creates with PUT, updates with POST -- except a label, which updates with
    // PUT. Each mock matches one exact method and path and expects exactly one hit, so
    // sending the conventional-but-wrong verb fails the test rather than surfacing as a
    // 405 at runtime, which is how the same mistake reached production in
    // `deploy/seed-from-prod.sh`.
    let server = MockServer::start().await;

    let task = json!({"id": 7, "title": "written", "project_id": 3});
    let expectations: Vec<(&str, &str, serde_json::Value)> = vec![
        ("PUT", "/api/v1/projects/3/tasks", task.clone()),
        ("POST", "/api/v1/tasks/7", task.clone()),
        ("DELETE", "/api/v1/tasks/7", json!({"message": "ok"})),
        ("PUT", "/api/v1/projects", json!({"id": 3, "title": "p"})),
        ("POST", "/api/v1/projects/3", json!({"id": 3, "title": "p"})),
        ("DELETE", "/api/v1/projects/3", json!({"message": "ok"})),
        ("PUT", "/api/v1/labels", json!({"id": 5, "title": "l"})),
        ("PUT", "/api/v1/labels/5", json!({"id": 5, "title": "l"})),
        ("DELETE", "/api/v1/labels/5", json!({"id": 5, "title": "l"})),
        ("PUT", "/api/v1/tasks/7/labels", json!({"label_id": 5})),
        (
            "DELETE",
            "/api/v1/tasks/7/labels/5",
            json!({"message": "ok"}),
        ),
        ("PUT", "/api/v1/tasks/7/assignees", json!({"user_id": 1})),
        (
            "DELETE",
            "/api/v1/tasks/7/assignees/1",
            json!({"message": "ok"}),
        ),
        (
            "PUT",
            "/api/v1/tasks/7/comments",
            json!({"id": 9, "comment": "hi"}),
        ),
        (
            "POST",
            "/api/v1/tasks/7/comments/9",
            json!({"id": 9, "comment": "edited"}),
        ),
        (
            "DELETE",
            "/api/v1/tasks/7/comments/9",
            json!({"message": "ok"}),
        ),
    ];
    for (verb, route, body) in expectations {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
    }

    let client = client(&server);
    let mut task = tui_do_api::models::Task {
        id: TaskId(7),
        title: "written".into(),
        ..Default::default()
    };
    let project = tui_do_api::models::ProjectId(3);
    let label = tui_do_api::models::LabelId(5);

    task = client
        .create_task(project, &task)
        .await
        .expect("create task");
    assert_eq!(task.id, TaskId(7));
    client.update_task(&task).await.expect("update task");
    client.delete_task(task.id).await.expect("delete task");

    let project_body = tui_do_api::models::Project {
        id: project,
        title: "p".into(),
        ..Default::default()
    };
    client
        .create_project(&project_body)
        .await
        .expect("create project");
    client
        .update_project(&project_body)
        .await
        .expect("update project");
    client
        .delete_project(project)
        .await
        .expect("delete project");

    let label_body = tui_do_api::models::Label {
        id: label,
        title: "l".into(),
        ..Default::default()
    };
    client
        .create_label(&label_body)
        .await
        .expect("create label");
    client
        .update_label(&label_body)
        .await
        .expect("update label");
    client.delete_label(label).await.expect("delete label");

    client
        .add_label_to_task(task.id, label)
        .await
        .expect("attach label");
    client
        .remove_label_from_task(task.id, label)
        .await
        .expect("detach label");

    let user = tui_do_api::models::UserId(1);
    client.assign_user(task.id, user).await.expect("assign");
    client.unassign_user(task.id, user).await.expect("unassign");

    let comment = client.create_comment(task.id, "hi").await.expect("comment");
    assert_eq!(comment.id, tui_do_api::models::CommentId(9));
    client
        .update_comment(task.id, &comment)
        .await
        .expect("edit comment");
    client
        .delete_comment(task.id, comment.id)
        .await
        .expect("delete comment");
}

#[tokio::test]
async fn an_update_sends_the_whole_task_including_cleared_dates() {
    // Vikunja replaces the task from the body, and encodes "no date" as Go's zero time
    // rather than null. Sending null is rejected; omitting the field leaves the old date
    // in place. Clearing a due date therefore means sending 0001-01-01T00:00:00Z.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/7"))
        .and(wiremock::matchers::body_partial_json(
            json!({"id": 7, "title": "kept", "due_date": "0001-01-01T00:00:00Z"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 7, "title": "kept"})))
        .expect(1)
        .mount(&server)
        .await;

    let task = tui_do_api::models::Task {
        id: TaskId(7),
        title: "kept".into(),
        due_date: None.into(),
        ..Default::default()
    };
    client(&server).update_task(&task).await.expect("update");
}

#[tokio::test]
async fn creating_a_task_puts_the_project_in_the_body_as_well_as_the_path() {
    // Vikunja binds path parameters before the JSON body, so a body carrying
    // "project_id": 0 silently overwrites the id from the URL. The server then reports
    // 404 / 3001 "This project does not exist." about a project that does. Found against
    // the live dev instance; this is the guard.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/31/tasks"))
        .and(wiremock::matchers::body_partial_json(
            json!({"project_id": 31}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1, "project_id": 31})))
        .expect(1)
        .mount(&server)
        .await;

    let created = client(&server)
        .create_task(
            tui_do_api::models::ProjectId(31),
            // Deliberately left at the default 0, the way a caller building a new task
            // naturally would.
            &tui_do_api::models::Task {
                title: "new".into(),
                ..Default::default()
            },
        )
        .await
        .expect("create");
    assert_eq!(created.project_id, tui_do_api::models::ProjectId(31));
}

#[tokio::test]
async fn a_task_page_survives_vikunjas_null_collections() {
    // Go marshals nil slices as null, and the spec calls all of these arrays. Before the
    // fix this failed the whole page with "invalid type: null, expected a sequence".
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pagination-total-pages", "1")
                .insert_header("x-pagination-result-count", "1")
                .set_body_json(json!([{
                    "id": 1,
                    "title": "real task",
                    "project_id": 3,
                    "reminders": null,
                    "labels": null,
                    "assignees": null,
                    "attachments": null,
                    "related_tasks": null,
                    "due_date": "0001-01-01T00:00:00Z"
                }])),
        )
        .mount(&server)
        .await;

    let tasks = client(&server)
        .all_tasks(&TaskQuery::new())
        .await
        .expect("null collections should read as empty, not fail the page");
    let task = tasks.first().expect("one task");
    assert_eq!(task.title, "real task");
    assert!(task.labels.is_empty());
    assert!(task.assignees.is_empty());
    assert!(task.reminders.is_empty());
    assert!(task.related_tasks.is_empty());
    assert_eq!(task.due_date.get(), None);
}

#[tokio::test]
async fn a_refresh_is_refused_after_logout() {
    // The refresh cookie survives `logout`'s local session reset, so without a guard a
    // caller could mint a new JWT from a session the user ended. The mock has no refresh
    // route, so an attempted request would show up as an unmatched one.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-1"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/logout"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder(server.uri()).build().expect("client");
    client
        .login(&Login::new("swasko", "hunter2"))
        .await
        .expect("login");
    client.logout().await.expect("logout");

    assert_eq!(client.auth_kind(), tui_do_api::AuthKind::Anonymous);
    assert!(matches!(
        client.refresh_token().await,
        Err(ApiError::NotAuthenticated { .. })
    ));
}

#[tokio::test]
async fn an_oversized_response_is_refused_rather_than_buffered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(4096)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 2})))
        .mount(&server)
        .await;

    // The ceiling is lowered rather than the body inflated, so the guard itself is under
    // test: at 64 MiB an assertion would mean allocating that much to prove nothing.
    let capped = Client::builder(server.uri())
        .credentials(Credentials::api_token("tk_test"))
        .max_response_bytes(1024)
        .build()
        .expect("client");

    match capped.task(TaskId(1)).await {
        Err(ApiError::ResponseTooLarge { limit, .. }) => assert_eq!(limit, 1024),
        other => panic!("expected ResponseTooLarge, got {other:?}"),
    }

    // And a body under the ceiling still goes through, so the guard is not simply
    // rejecting everything.
    capped.task(TaskId(2)).await.expect("a small body is fine");
}

#[tokio::test]
async fn a_server_that_ignores_the_page_parameter_fails_loudly() {
    // Returning the same full page forever is how a broken server turns pagination into
    // an infinite loop. Stopping quietly would be worse than the loop: it would hand
    // back a collection that looks complete. It has to be an error.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tasks"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                (1..=50)
                    .map(|i| json!({"id": i, "title": "t"}))
                    .collect::<Vec<_>>(),
            ),
        )
        .mount(&server)
        .await;

    match client(&server).all_tasks(&TaskQuery::new()).await {
        Err(ApiError::TooManyPages { pages, .. }) => assert!(pages >= 1_000),
        other => panic!("expected TooManyPages, got {:?}", other.map(|t| t.len())),
    }
}

#[tokio::test]
async fn a_401_on_a_delete_is_refreshed_and_retried_like_any_other_request() {
    // Deletes and label attachments go through a different send path than reads. It
    // skipped the refresh entirely, so an expired JWT let you edit a task but not delete
    // it -- and under optimistic writes that surfaces as a rollback and a toast.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-old"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/token/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-new"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tasks/7"))
        .and(header("authorization", "Bearer jwt-old"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"code": 11, "message": "expired"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tasks/7"))
        .and(header("authorization", "Bearer jwt-new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder(server.uri()).build().expect("client");
    client
        .login(&Login::new("swasko", "hunter2"))
        .await
        .expect("login");
    client
        .delete_task(TaskId(7))
        .await
        .expect("the 401 should have been answered by a refresh, not surfaced");
}

#[tokio::test]
async fn a_permission_denial_does_not_spend_a_token_refresh() {
    // 403 means the credential is fine and the user is not allowed. Refreshing on it
    // would burn the rate-limited refresh endpoint -- ten requests per window -- on
    // every read-only project a sync pass touches.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt-1"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects/9"))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            json!({"code": 3004, "message": "You don't have the right to see this project."}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder(server.uri()).build().expect("client");
    client
        .login(&Login::new("swasko", "hunter2"))
        .await
        .expect("login");

    // No refresh route is mounted, so an attempt would be an unmatched request.
    match client.project(tui_do_api::models::ProjectId(9)).await {
        Err(err @ ApiError::Forbidden { .. }) => {
            assert_eq!(err.code(), Some(3004));
            assert!(!err.is_retryable());
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

#[tokio::test]
async fn pages_fetched_at_once_still_arrive_in_order() {
    // The fan-out is only safe if the results are reassembled by page. Later pages are
    // made to answer sooner, so a naive "whatever finishes first" would scramble them.
    let server = MockServer::start().await;
    for page in 1..=6u32 {
        // Page 1 must answer first for the walk to start, so it is not delayed; the
        // rest answer in reverse order of their number.
        let delay = if page == 1 {
            0
        } else {
            u64::from(7 - page) * 40
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/tasks"))
            .and(query_param("page", page.to_string().as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(delay))
                    .set_body_json(vec![json!({
                        "id": page,
                        "title": format!("page {page}"),
                        "project_id": 1
                    })])
                    .insert_header("x-pagination-total-pages", "6")
                    .insert_header("x-pagination-result-count", "1"),
            )
            .mount(&server)
            .await;
    }

    let tasks = client(&server)
        .all_tasks(&TaskQuery::default())
        .await
        .expect("every page");

    let ids: Vec<i64> = tasks.iter().map(|task| task.id.get()).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 6], "pages came back out of order");
}

#[tokio::test]
async fn a_collection_that_grows_while_it_is_read_is_still_read_to_the_end() {
    // The page count comes from page one. If tasks arrive while the rest are in flight,
    // the last page says so -- and stopping at the original count would hand back a
    // short list, which the sync engine would treat as "these tasks are gone".
    let server = MockServer::start().await;
    let body = |id: i64, total: &str, count: &str| {
        ResponseTemplate::new(200)
            .set_body_json(vec![json!({"id": id, "title": "t", "project_id": 1})])
            .insert_header("x-pagination-total-pages", total)
            .insert_header("x-pagination-result-count", count)
    };
    // Two pages announced, but page two reports three, and page three is real.
    for (page, id, total) in [(1u32, 1i64, "2"), (2, 2, "3"), (3, 3, "3")] {
        Mock::given(method("GET"))
            .and(path("/api/v1/tasks"))
            .and(query_param("page", page.to_string().as_str()))
            .respond_with(body(id, total, "1"))
            .mount(&server)
            .await;
    }

    let tasks = client(&server)
        .all_tasks(&TaskQuery::default())
        .await
        .expect("every page");

    let ids: Vec<i64> = tasks.iter().map(|task| task.id.get()).collect();
    assert_eq!(
        ids,
        vec![1, 2, 3],
        "the page that appeared mid-walk was dropped"
    );
}
