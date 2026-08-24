//! Integration tests against a real Vikunja.
//!
//! These are the tests wiremock cannot replace. The `PUT`-versus-`POST` bug in
//! `seed-from-prod.sh` survived to runtime precisely because nothing exercised the real
//! server, and a mock will happily agree with whatever the client believes.
//!
//! # Running them
//!
//! ```sh
//! export CRIAX_TEST_URL=https://dev-box.example.net:8443   # dev, never prod
//! export CRIAX_TEST_TOKEN=tk_...          # Settings -> API tokens, in the web UI
//! # or, instead of a token:
//! export CRIAX_TEST_USERNAME=... CRIAX_TEST_PASSWORD=...
//! cargo test -p criax-api --test live -- --nocapture
//! ```
//!
//! Without `CRIAX_TEST_URL` every test here reports success without doing anything, so
//! `cargo test --workspace` stays green offline and in CI.
//!
//! # Prod is not a test target
//!
//! [`target`] takes an **allow**-list, not a deny-list: the URL must name a known dev
//! host, and the prod hostname is refused outright with no environment variable that can
//! switch the check off. A deny-list would have been one empty `CRIAX_PROD_DENY=` away
//! from running the create/update/delete round-trip against production.
//!
//! `CRIAX_DEV_HOST` names *your* dev instance; it cannot be used to permit prod.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use std::collections::BTreeSet;

use criax_api::models::Login;
use criax_api::{Client, Credentials, TaskQuery};

/// The dev instance these tests are written against.
const DEFAULT_DEV_HOST: &str = "dev-box.example.net";

/// Hosts that are never a test target, whatever else is configured.
///
/// Not overridable, deliberately. Every environment variable here is one an accident can
/// set.
const FORBIDDEN_HOSTS: &[&str] = &["prod-box"];

/// Projects the round-trip test creates, and the title it sweeps up before starting.
const FIXTURE_PROJECT_TITLE: &str = "criax live test";

/// The server under test, or `None` when these tests are not configured to run.
///
/// # Panics
/// If the configured URL is not a known dev host. That is a misconfiguration to fix, not
/// a condition to skip past — skipping would let a typo mean "ran against something
/// else", which is the failure mode worth being loud about.
fn target() -> Option<String> {
    let url = std::env::var("CRIAX_TEST_URL").ok()?;
    let url = url.trim().to_string();
    if url.is_empty() {
        return None;
    }

    for forbidden in FORBIDDEN_HOSTS {
        assert!(
            !url.contains(forbidden),
            "CRIAX_TEST_URL points at {url}, which is production. There is no way to \
             override this; point it at the dev instance."
        );
    }

    let dev_host = std::env::var("CRIAX_DEV_HOST").unwrap_or_else(|_| DEFAULT_DEV_HOST.to_string());
    assert!(
        !dev_host.is_empty() && url.contains(&dev_host),
        "CRIAX_TEST_URL is {url}, which is not the dev host {dev_host:?}. These tests \
         create and delete data; they run against a known dev instance or not at all. \
         Set CRIAX_DEV_HOST if your dev instance lives somewhere else."
    );
    Some(url)
}

/// Delete anything a previous run left behind.
///
/// The round-trip test cleans up after itself on the happy path, but an assertion failure
/// between creating a project and deleting it orphans the fixtures — and that is exactly
/// the case the test exists to produce. Sweeping at the start keeps reruns deterministic
/// and stops a leftover fixture from being picked as "the first real project" by another
/// test.
async fn sweep_fixtures(client: &Client) {
    let Ok(projects) = client.all_projects().await else {
        return;
    };
    for project in projects
        .iter()
        .filter(|p| p.id.get() > 0 && p.title == FIXTURE_PROJECT_TITLE)
    {
        match client.delete_project(project.id).await {
            Ok(()) => println!("swept leftover fixture project {}", project.id),
            Err(err) => println!("could not sweep project {}: {err}", project.id),
        }
    }

    let Ok(labels) = client.all_labels().await else {
        return;
    };
    for label in labels.iter().filter(|l| l.title == "criax-live-test") {
        match client.delete_label(label.id).await {
            Ok(()) => println!("swept leftover fixture label {}", label.id),
            Err(err) => println!("could not sweep label {}: {err}", label.id),
        }
    }
}

/// A client authenticated the way the environment says, or `None` to skip.
async fn connect(test: &str) -> Option<Client> {
    let Some(url) = target() else {
        println!("skipping {test}: set CRIAX_TEST_URL to the dev instance to run it");
        return None;
    };

    let token = std::env::var("CRIAX_TEST_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let username = std::env::var("CRIAX_TEST_USERNAME")
        .ok()
        .filter(|u| !u.is_empty());

    let mut builder = Client::builder(&url);
    if let Some(token) = token {
        builder = builder.credentials(Credentials::api_token(token));
    }
    let client = builder
        .build()
        .expect("CRIAX_TEST_URL should be a valid URL");

    // Reading /info first is what a real startup does, and it is what tells the paginators
    // the server's page cap instead of leaving them on the built-in default.
    let info = client.info().await.expect("GET /info should succeed");
    println!(
        "{test}: {url} is Vikunja {} (cap {})",
        info.version,
        info.page_cap()
    );

    if let Some(username) = username {
        let password = std::env::var("CRIAX_TEST_PASSWORD")
            .expect("CRIAX_TEST_USERNAME needs CRIAX_TEST_PASSWORD");
        client
            .login(&Login::new(username, password))
            .await
            .expect("POST /login should succeed");
    }

    assert_ne!(
        client.auth_kind(),
        criax_api::AuthKind::Anonymous,
        "set CRIAX_TEST_TOKEN, or CRIAX_TEST_USERNAME and CRIAX_TEST_PASSWORD"
    );

    sweep_fixtures(&client).await;
    Some(client)
}

#[tokio::test]
async fn info_reports_the_version_the_spec_was_captured_from() {
    let Some(url) = target() else {
        println!("skipping: CRIAX_TEST_URL is not set");
        return;
    };
    let client = Client::builder(url).build().expect("valid url");
    let info = client.info().await.expect("GET /info should succeed");

    assert_eq!(
        info.version, "v2.5.0",
        "the server was upgraded; refresh the spec with `cargo xtask fetch-spec` and \
         re-run the conformance tests before trusting anything else"
    );
    assert!(info.page_cap() > 0);
    assert!(info.supports_vikunja_file_migration());
}

#[tokio::test]
async fn a_full_task_fetch_returns_every_page() {
    let Some(client) = connect("a_full_task_fetch_returns_every_page").await else {
        return;
    };

    let cap = client.page_size();
    let mut pager = client.tasks(&TaskQuery::new()).expect("pager");
    let mut tasks = Vec::new();
    let mut pages = 0;
    while let Some(page) = pager.next_page().await.expect("each page should load") {
        pages += 1;
        tasks.extend(page.items);
    }
    let total_pages = pager
        .total_pages()
        .expect("the server should report a page count");

    println!("fetched {} tasks across {pages} pages", tasks.len());
    assert_eq!(pages, total_pages, "stopped before the server's last page");
    assert!(
        tasks.len() > cap as usize,
        "the dev instance holds thousands of tasks; getting {} back means pagination is \
         broken -- this is exactly cria's silent truncation",
        tasks.len()
    );

    let ids: BTreeSet<i64> = tasks.iter().map(|t| t.id.get()).collect();
    assert_eq!(ids.len(), tasks.len(), "the same task arrived on two pages");

    // Does a list endpoint populate assignees, or leave them nil? It decides how
    // dangerous read-mutate-write is: `update_task` sends the whole body and an empty
    // `assignees` clears them, so if list results omit assignees then every optimistic
    // edit unassigns everyone. Reported rather than asserted -- seeded data may
    // legitimately have none, and a zero here means "inconclusive", not "broken".
    let with_assignees = tasks.iter().filter(|t| !t.assignees.is_empty()).count();
    let with_labels = tasks.iter().filter(|t| !t.labels.is_empty()).count();
    println!(
        "of {} tasks from the list endpoint, {with_assignees} carry assignees and \
         {with_labels} carry labels",
        tasks.len()
    );

    // The lower bound implied by the page count: every page but the last was full.
    assert!(tasks.len() >= ((total_pages - 1) * cap + 1) as usize);

    if let Ok(expected) = std::env::var("CRIAX_TEST_EXPECTED_TASKS") {
        let expected: usize = expected
            .parse()
            .expect("CRIAX_TEST_EXPECTED_TASKS is a number");
        assert_eq!(tasks.len(), expected);
    }
}

#[tokio::test]
async fn dateless_tasks_do_not_read_as_two_thousand_years_overdue() {
    // The zero-time bug, checked against real data rather than a fixture: Vikunja sends
    // "0001-01-01T00:00:00Z" for every unset date, and a task with no due date must come
    // back with `None`, not a year-1 timestamp.
    let Some(client) = connect("dateless_tasks_do_not_read_as_two_thousand_years_overdue").await
    else {
        return;
    };

    let tasks = client.all_tasks(&TaskQuery::new()).await.expect("fetch");
    let now = chrono::Utc::now();
    let ancient: Vec<_> = tasks
        .iter()
        .filter_map(|t| t.due_date.get().map(|d| (t.id, d)))
        .filter(|(_, due)| due.timestamp() < 0)
        .collect();
    assert!(
        ancient.is_empty(),
        "these tasks parsed their unset due date as a real one: {ancient:?}"
    );

    let dateless = tasks.iter().filter(|t| t.due_date.get().is_none()).count();
    let overdue = tasks.iter().filter(|t| t.is_overdue(now)).count();
    println!(
        "{dateless} of {} tasks have no due date; {overdue} overdue",
        tasks.len()
    );
    assert!(dateless > 0, "seeded data should contain dateless tasks");
}

#[tokio::test]
async fn projects_labels_and_views_load() {
    let Some(client) = connect("projects_labels_and_views_load").await else {
        return;
    };

    let projects = client.all_projects().await.expect("projects should load");
    // Vikunja returns pseudo-projects in this list: -1 is Favorites, and saved filters
    // appear with negative ids too. They reject writes, so a project picker has to
    // exclude them -- this is where the count of them gets noticed.
    let (pseudo, real): (Vec<_>, Vec<_>) = projects.iter().partition(|p| p.id.get() < 0);
    println!(
        "{} projects: {} real, {} pseudo ({:?})",
        projects.len(),
        real.len(),
        pseudo.len(),
        pseudo.iter().map(|p| (p.id, &p.title)).collect::<Vec<_>>()
    );
    assert!(!projects.is_empty());

    // Labels may legitimately be empty on a sparse instance; loading them must still work.
    let labels = client.all_labels().await.expect("labels should load");
    println!("{} labels", labels.len());

    let first = real.first().copied().expect("at least one real project");
    let views = client
        .project_views(first.id)
        .await
        .expect("project views should load");
    assert!(
        !views.is_empty(),
        "every Vikunja project has at least a List view"
    );

    // The view endpoint is how the web frontend loads a project, and the only source of
    // each task's per-view position.
    let list = views.first().expect("at least one view");
    let tasks = client
        .view_tasks(first.id, list.id, &TaskQuery::new())
        .expect("pager")
        .collect_all()
        .await
        .expect("view tasks should load");
    println!(
        "project {} view {} holds {} tasks",
        first.title,
        list.id,
        tasks.len()
    );
}

#[tokio::test]
async fn the_current_user_is_who_we_authenticated_as() {
    let Some(client) = connect("the_current_user_is_who_we_authenticated_as").await else {
        return;
    };
    let user = client
        .current_user()
        .await
        .expect("GET /user should succeed");
    assert!(!user.username.is_empty());
    println!("authenticated as {}", user.display_name());
}

#[tokio::test]
async fn a_task_round_trips_through_create_read_update_delete() {
    let Some(client) = connect("a_task_round_trips_through_create_read_update_delete").await else {
        return;
    };

    // Everything is created inside a throwaway project and deleted again, so a run leaves
    // the dev instance as it found it even without `deploy/reset-dev.sh`.
    let project = client
        .create_project(&criax_api::models::Project {
            title: FIXTURE_PROJECT_TITLE.into(),
            description: "created by cargo test -p criax-api --test live".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /projects should create a project");
    assert!(project.id.get() > 0);
    println!("created project {}", project.id);

    let due = chrono::Utc::now() + chrono::Duration::days(3);
    let created = client
        .create_task(
            project.id,
            &criax_api::models::Task {
                title: "round trip".into(),
                description: "written by the live test".into(),
                priority: 3,
                due_date: Some(due).into(),
                ..Default::default()
            },
        )
        .await
        .expect("PUT /projects/{id}/tasks should create a task");
    assert!(created.id.get() > 0);
    assert_eq!(created.title, "round trip");
    assert_eq!(created.project_id, project.id);
    assert!(
        created.due_date.get().is_some(),
        "the due date did not survive the round trip"
    );

    let read = client.task(created.id).await.expect("GET /tasks/{id}");
    assert_eq!(read.id, created.id);
    assert_eq!(read.title, "round trip");

    // Update the whole task, the way an optimistic write does: read, mutate, send back.
    let mut edited = read.clone();
    edited.title = "round trip, edited".into();
    edited.done = true;
    edited.due_date = None.into();
    let updated = client
        .update_task(&edited)
        .await
        .expect("POST /tasks/{id} should update");
    assert_eq!(updated.title, "round trip, edited");
    assert!(updated.done);
    assert_eq!(
        updated.due_date.get(),
        None,
        "sending the zero time should have cleared the due date, not left it set"
    );
    assert!(
        updated.done_at.get().is_some(),
        "completing a task should stamp done_at"
    );

    // Labels attach through their own endpoint, not through the task body.
    let label = client
        .create_label(&criax_api::models::Label {
            title: "criax-live-test".into(),
            hex_color: "4287f5".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /labels should create a label");
    client
        .add_label_to_task(created.id, label.id)
        .await
        .expect("PUT /tasks/{task}/labels should attach");
    let attached = client.task_labels(created.id).await.expect("task labels");
    assert!(attached.iter().any(|l| l.id == label.id));
    client
        .remove_label_from_task(created.id, label.id)
        .await
        .expect("DELETE /tasks/{task}/labels/{label} should detach");
    client.delete_label(label.id).await.expect("delete label");

    // The comment endpoints, including the update the spec declares with no request body.
    let comment = client
        .create_comment(created.id, "posted by the live test")
        .await
        .expect("PUT /tasks/{taskID}/comments should post");
    assert!(comment.comment.contains("posted by the live test"));

    let mut edited_comment = comment.clone();
    edited_comment.comment = "edited by the live test".into();
    let updated_comment = client
        .update_comment(created.id, &edited_comment)
        .await
        .expect("POST /tasks/{taskID}/comments/{commentID} should update");
    assert!(
        updated_comment.comment.contains("edited"),
        "the spec declares no body for a comment update; the server ignored ours, so the \
         model or the endpoint needs revisiting"
    );

    client
        .delete_comment(created.id, comment.id)
        .await
        .expect("delete comment");
    client.delete_task(created.id).await.expect("delete task");
    client
        .delete_project(project.id)
        .await
        .expect("delete project");

    // The task is gone, and asking for it says so specifically rather than vaguely.
    match client.task(created.id).await {
        Err(err) => println!("deleted task now reports: {err}"),
        Ok(task) => panic!("the deleted task is still readable: {task:?}"),
    }
}
